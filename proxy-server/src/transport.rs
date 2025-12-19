use anyhow::Result;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use wtransport::Connection;

#[derive(Debug)]
pub enum TransportType {
    WebTransport(
        Arc<Connection>,
        wtransport::SendStream,
        wtransport::RecvStream,
    ),
    WebSocket {
        control: Arc<Mutex<tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>>>,
        data: Arc<Mutex<tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>>>,
    },
}

/// Abstract transport for RTSP/RTP
pub struct Transport {
    inner: TransportType,
}

/// Clone-able sender for datagrams
#[derive(Clone, Debug)]
pub enum TransportSender {
    WebTransport(Arc<Connection>),
    WebSocket(Arc<Mutex<tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>>>),
}

impl TransportSender {
    pub async fn send_datagram(&self, payload: Bytes) -> Result<()> {
        match self {
            TransportSender::WebTransport(conn) => {
                conn.send_datagram(payload)?;
                Ok(())
            }
            TransportSender::WebSocket(ws) => {
                let mut ws = ws.lock().await;
                if let Err(e) = ws.send(Message::Binary(payload.into())).await {
                    tracing::error!("Failed to send WS datagram: {}", e);
                }
                Ok(())
            }
        }
    }
}

impl Transport {
    pub fn new_wt(
        conn: Arc<Connection>,
        send: wtransport::SendStream,
        recv: wtransport::RecvStream,
    ) -> Self {
        Self {
            inner: TransportType::WebTransport(conn, send, recv),
        }
    }

    pub fn new_ws(
        control: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        data: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) -> Self {
        Self {
            inner: TransportType::WebSocket {
                control: Arc::new(Mutex::new(control)),
                data: Arc::new(Mutex::new(data)),
            },
        }
    }

    pub fn clone_sender(&self) -> TransportSender {
        match &self.inner {
            TransportType::WebTransport(conn, _, _) => TransportSender::WebTransport(conn.clone()),
            TransportType::WebSocket { data, .. } => TransportSender::WebSocket(data.clone()),
        }
    }

    /// Read next control message (RTSP text)
    pub async fn read_control(&mut self, buf: &mut bytes::BytesMut) -> Result<usize> {
        match &mut self.inner {
            TransportType::WebTransport(_, _, recv) => {
                // Read from WT stream
                let n = recv.read_buf(buf).await?;
                Ok(n) // 0 means EOF
            }
            TransportType::WebSocket { control, .. } => {
                let mut ws = control.lock().await;
                match ws.next().await {
                    Some(Ok(msg)) => {
                        match msg {
                            Message::Text(text) => {
                                buf.extend_from_slice(text.as_bytes());
                                Ok(text.len())
                            }
                            Message::Close(_) => Ok(0),
                            _ => Ok(0), // Ignore other types for control
                        }
                    }
                    Some(Err(e)) => Err(anyhow::anyhow!("WebSocket error: {}", e)),
                    None => Ok(0), // EOF
                }
            }
        }
    }

    /// Write control message (RTSP text)
    pub async fn write_control(&mut self, data: &[u8]) -> Result<()> {
        match &mut self.inner {
            TransportType::WebTransport(_, send, _) => {
                send.write_all(data).await?;
                Ok(())
            }
            TransportType::WebSocket { control, .. } => {
                // Ideally we should check if data is valid UTF-8, but RTSP is generally ASCII/UTF-8
                let text = String::from_utf8_lossy(data).to_string();
                let mut ws = control.lock().await;
                ws.send(Message::Text(text.into())).await?;
                Ok(())
            }
        }
    }

    pub async fn closed(&self) {
        match &self.inner {
            TransportType::WebTransport(conn, _, _) => {
                conn.closed().await;
            }
            TransportType::WebSocket { .. } => {
                // Monitor WS close?
                // Currently just wait forever or until read returns 0
                futures_util::future::pending::<()>().await;
            }
        }
    }
}
