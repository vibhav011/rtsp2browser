use anyhow::Result;
use std::time::Duration;
use tracing::{error, info};
use wtransport::Endpoint;
use wtransport::Identity;
use wtransport::ServerConfig;

mod proxy;
mod rtsp;
use proxy::RTSPProxy;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    let cert_pemfile = "./cert.pem";
    let private_key_pemfile = "./key.pem";
    
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

    let server = Endpoint::server(config)?;

    info!("WebTransport Server ready on port 4433");

    for id in 0.. {
        let incoming_session = server.accept().await;
        tokio::spawn(async move {
            info!("Connection #{} accepted", id);
            let result = handle_connection(incoming_session).await;
            if let Err(e) = result {
                error!("Connection #{} error: {:?}", id, e);
            }
        });
    }

    Ok(())
}

async fn handle_connection(incoming_session: wtransport::endpoint::IncomingSession) -> Result<()> {
    info!("Waiting for session request...");
    let session_request = incoming_session.await?;

    info!(
        "New session: Authority: '{}', Path: '{}'",
        session_request.authority(),
        session_request.path()
    );

    let path = session_request.path();
    let url = url::Url::parse(&format!("https://localhost{}", path)).unwrap_or_else(|_| url::Url::parse("https://localhost/").unwrap());
    
    let mut rtsp_url = None;
    
    for (key, value) in url.query_pairs() {
        if key == "rtsp" {
            rtsp_url = Some(value.to_string());
            info!("Client requested RTSP URL: {}", value);
        }
    }

    let rtsp_url = rtsp_url.ok_or_else(|| anyhow::anyhow!("Missing 'rtsp' query parameter"))?;

    let connection = session_request.accept().await?;
    
    // Create a new proxy instance for this connection
    let proxy = RTSPProxy::new(rtsp_url);
    
    proxy.handle_connection(connection).await?;
    
    Ok(())
}
