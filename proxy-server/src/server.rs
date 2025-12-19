use anyhow::Result;
use std::time::Duration;
use tracing::{error, info};
use wtransport::Endpoint;
use wtransport::Identity;
use wtransport::ServerConfig;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

mod proxy;
mod transport; 
mod rtsp; 

use proxy::RTSPProxy;
use transport::Transport;

type WsStream = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

enum SessionState {
    WaitingForData(WsStream, String), // Control socket waiting, holds RTSP URL
    WaitingForControl(WsStream),      // Data socket waiting
}

type SessionRegistry = Arc<Mutex<HashMap<String, SessionState>>>;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    let cert_pemfile = "./DO_NOT_USE_CERT.pem";
    let private_key_pemfile = "./DO_NOT_USE_KEY.pem";
    
    // Check if certs exist, otherwise generate self-signed (for dev)
    let identity = if std::path::Path::new(cert_pemfile).exists() {
        Identity::load_pemfiles(cert_pemfile, private_key_pemfile)
            .await
            .unwrap()
    } else {
        info!("Certificates not found, using self-signed identity");
        Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap()
    };

    let config = ServerConfig::builder()
        .with_bind_default(4433)
        .with_identity(identity)
        .keep_alive_interval(Some(Duration::from_secs(3)))
        .build();

    let wt_server = Endpoint::server(config)?;
    info!("WebTransport Server ready on port 4433");
    
    // WebSocket Server
    let ws_listener = TcpListener::bind("0.0.0.0:8080").await?;
    info!("WebSocket Server ready on port 8080");

    let session_registry: SessionRegistry = Arc::new(Mutex::new(HashMap::new()));

    loop {
        tokio::select! {
             // WebTransport
            incoming_session = wt_server.accept() => {
                tokio::spawn(async move {
                    if let Err(e) = handle_wt_connection(incoming_session).await {
                         error!("WebTransport connection error: {:?}", e);
                    }
                });
            }
            // WebSocket
            Ok((stream, _addr)) = ws_listener.accept() => {
                let registry = session_registry.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_ws_connection(stream, registry).await {
                         error!("WebSocket connection error: {:?}", e);
                    }
                });
            }
        }
    }
}

async fn handle_wt_connection(incoming_session: wtransport::endpoint::IncomingSession) -> Result<()> {
    info!("Waiting for WebTransport session request...");
    let session_request = incoming_session.await?;

    let path = session_request.path();
    let url = url::Url::parse(&format!("https://localhost{}", path)).unwrap_or_else(|_| url::Url::parse("https://localhost/").unwrap());
    
    let rtsp_url = extract_rtsp_url(&url)?;
    info!("Client requested RTSP URL: {}", rtsp_url);

    let connection = session_request.accept().await?;
    
    // Accept the bi-stream for control immediately to form the Transport
    let (send, recv) = connection.accept_bi().await?;
    
    let transport = Transport::new_wt(std::sync::Arc::new(connection), send, recv);
    let proxy = RTSPProxy::new(rtsp_url);
    
    proxy.handle_connection(transport).await?;
    
    Ok(())
}

async fn handle_ws_connection(stream: tokio::net::TcpStream, registry: SessionRegistry) -> Result<()> {
    // Shared state to extract query parameters from the handshake callback
    let query_params = Arc::new(Mutex::new(None));
    let query_params_clone = query_params.clone();

    let ws_stream = accept_hdr_async(stream, move |req: &Request, response: Response| {
        let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
        if let Ok(url) = url::Url::parse(&format!("http://localhost{}", path)) {
            let mut params = HashMap::new();
            for (key, value) in url.query_pairs() {
                params.insert(key.into_owned(), value.into_owned());
            }
            *query_params_clone.lock().unwrap() = Some(params);
        }
        Ok::<_, tokio_tungstenite::tungstenite::handshake::server::ErrorResponse>(response)
    }).await?;
    
    let params = {
        let locked = query_params.lock().unwrap();
        locked.clone().ok_or_else(|| anyhow::anyhow!("Missing query parameters"))?
    };

    let session_id = params.get("session_id").cloned().ok_or_else(|| anyhow::anyhow!("Missing 'session_id'"))?;
    let conn_type = params.get("type").map(|s| s.as_str()).unwrap_or("control"); // default to control for backward compat?
    
    info!("WebSocket connection: type={}, session_id={}", conn_type, session_id);

    let maybe_pair = {
        let mut reg = registry.lock().unwrap();
        
        if conn_type == "data" {
            // I am Data. Check if Control is waiting.
            match reg.remove(&session_id) {
                Some(SessionState::WaitingForData(control_socket, rtsp_url)) => {
                    info!("Paired with waiting Control connection for session {}", session_id);
                    Some((control_socket, ws_stream, rtsp_url))
                }
                Some(SessionState::WaitingForControl(_)) => {
                    return Err(anyhow::anyhow!("Duplicate Data connection for session {}", session_id));
                }
                None => {
                    info!("Data connection waiting for Control for session {}", session_id);
                    reg.insert(session_id, SessionState::WaitingForControl(ws_stream));
                    None
                }
            }
        } else {
            // I am Control. Check if Data is waiting.
            // Control connection MUST have 'rtsp' param
            let rtsp_url = params.get("rtsp").cloned().ok_or_else(|| anyhow::anyhow!("Missing 'rtsp' query parameter for control connection"))?;
            
            match reg.remove(&session_id) {
                Some(SessionState::WaitingForControl(data_socket)) => {
                    info!("Paired with waiting Data connection for session {}", session_id);
                    Some((ws_stream, data_socket, rtsp_url))
                }
                Some(SessionState::WaitingForData(_, _)) => {
                    return Err(anyhow::anyhow!("Duplicate Control connection for session {}", session_id));
                }
                None => {
                    info!("Control connection waiting for Data for session {}", session_id);
                    reg.insert(session_id, SessionState::WaitingForData(ws_stream, rtsp_url));
                    None
                }
            }
        }
    };

    if let Some((control_sock, data_sock, rtsp_url)) = maybe_pair {
        let transport = Transport::new_ws(control_sock, data_sock);
        let proxy = RTSPProxy::new(rtsp_url);
        
        proxy.handle_connection(transport).await?;
    }
    Ok(())
}

fn extract_rtsp_url(url: &url::Url) -> Result<String> {
    for (key, value) in url.query_pairs() {
        if key == "rtsp" {
            return Ok(value.to_string());
        }
    }
    Err(anyhow::anyhow!("Missing 'rtsp' query parameter"))
}
