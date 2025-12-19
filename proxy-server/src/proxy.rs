use anyhow::{Context, Result};
use bytes::{BytesMut, Buf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tracing::{error, info, instrument};
use crate::rtsp::{RtspRequest, RtspResponse};
use std::collections::VecDeque;
use tokio_util::sync::CancellationToken;
use crate::transport::Transport;

pub struct RTSPProxy {
    rtsp_url: String,
}

struct PendingSetup {
    rtp_channel_id: u8,
    rtcp_channel_id: u8,
    rtp_socket: Arc<UdpSocket>,
    rtcp_socket: Arc<UdpSocket>,
}

impl RTSPProxy {
    pub fn new(rtsp_url: String) -> Self {
        Self { rtsp_url }
    }


    #[instrument(skip(self, transport))]
    pub async fn handle_connection(&self, mut transport: Transport) -> Result<()> {
        info!("Handling new connection via Transport abstraction");

        // 1. Reading/Writing control is now done via transport
        // We don't accept_bi here anymore, we expect transport to be ready for control

        // 2. Connect to the RTSP server
        let url = url::Url::parse(&self.rtsp_url).context("Invalid RTSP URL")?;
        let host = url.host_str().context("Missing host in RTSP URL")?;
        let port = url.port().unwrap_or(8554);
        let addr = format!("{}:{}", host, port);

        info!("Connecting to RTSP server at {}", addr);
        let mut tcp_stream = TcpStream::connect(&addr)
            .await
            .context("Failed to connect to RTSP server")?;
        
        info!("Connected to RTSP server");

        let (mut tcp_read, mut tcp_write) = tcp_stream.split();

        // For detecting connection loss
        // let closed_fut = transport.closed(); // This borrows transport.
        // tokio::pin!(closed_fut);
        
        // State management
        let mut next_channel_id = 0;
        let mut pending_setups: VecDeque<PendingSetup> = VecDeque::new();
        let mut session_id: Option<String> = None;
        
        // Cancellation token for background tasks
        let cancel_token = CancellationToken::new();

        // Buffers
        let mut wt_buf = BytesMut::with_capacity(4096);
        let mut tcp_buf = BytesMut::with_capacity(4096);

        loop {
            tokio::select! {
                // Read from Transport (Browser) -> Forward to TCP (RTSP Server)
                res = transport.read_control(&mut wt_buf) => {
                    let n = match res {
                        Ok(n) => n,
                        Err(e) => {
                            error!("Transport read error: {}", e);
                            break;
                        }
                    };
                    
                    if n == 0 {
                        info!("Transport stream closed by client");
                        break;
                    }

                    // Process all complete requests in buffer
                    while let Some((mut req, consumed)) = RtspRequest::parse(&wt_buf)? {
                        wt_buf.advance(consumed);
                        
                        if req.method == "SETUP" {
                            info!("Intercepted SETUP request");
                            
                            // 1. Allocate UDP ports
                            let rtp_socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
                            let rtcp_socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
                            let rtp_port = rtp_socket.local_addr()?.port();
                            let rtcp_port = rtcp_socket.local_addr()?.port();
                            
                            info!("Allocated UDP ports: RTP={}, RTCP={}", rtp_port, rtcp_port);

                            // 2. Rewrite Transport header
                            if let Some(transport) = req.headers.get_mut("Transport") {
                                *transport = format!("RTP/AVP;unicast;client_port={}-{}", rtp_port, rtcp_port);
                            }

                            // 3. Store pending state
                            let rtp_id = next_channel_id;
                            let rtcp_id = next_channel_id + 1;
                            next_channel_id += 2;

                            pending_setups.push_back(PendingSetup {
                                rtp_channel_id: rtp_id,
                                rtcp_channel_id: rtcp_id,
                                rtp_socket,
                                rtcp_socket,
                            });
                        }

                        // Forward to RTSP Server
                        if let Err(e) = tcp_write.write_all(&req.to_bytes()).await {
                            error!("Failed to write to RTSP server: {}", e);
                            break;
                        }
                    }
                }
                
                // Read from TCP (RTSP Server) -> Forward to Transport (Browser)
                res = tcp_read.read_buf(&mut tcp_buf) => {
                    let n = match res {
                        Ok(n) => n,
                        Err(e) => {
                            error!("RTSP server read error: {}", e);
                            break;
                        }
                    };
                    
                    if n == 0 {
                        info!("RTSP server closed connection");
                        break;
                    }
                    
                    // Process all complete responses in buffer
                    while let Some((mut resp, consumed)) = RtspResponse::parse(&tcp_buf)? {
                        tcp_buf.advance(consumed);
                        
                        // Capture Session ID if present
                        if let Some(sid) = resp.headers.get("Session") {
                            // Session ID might have ;timeout=...
                            let clean_sid = sid.split(';').next().unwrap_or(sid).to_string();
                            if session_id.is_none() {
                                info!("Captured Session ID: {}", clean_sid);
                                session_id = Some(clean_sid);
                            }
                        }

                        if resp.status_code == 200 {
                            if resp.headers.contains_key("Transport") {
                                if let Some(setup) = pending_setups.pop_front() {
                                    info!("Intercepted SETUP response, injecting channel IDs {}-{}", setup.rtp_channel_id, setup.rtcp_channel_id);
                                    
                                    // Inject Channel IDs into Transport header
                                    if let Some(transport) = resp.headers.get_mut("Transport") {
                                        *transport = format!("{};x-wt-channel-id={}-{}", transport, setup.rtp_channel_id, setup.rtcp_channel_id);
                                    }
                                    
                                    // Spawn UDP forwarders
                                    // We need to clone the transport sender part
                                    // Assuming transport.clone_sender() exists and returns a DatagramSender
                                    let sender = transport.clone_sender(); 
                                    let rtp_socket = setup.rtp_socket.clone();
                                    let rtp_id = setup.rtp_channel_id;
                                    let token = cancel_token.clone();
                                    
                                    tokio::spawn(async move {
                                        if let Err(e) = forward_udp(rtp_socket, sender, rtp_id, token).await {
                                            // Only log error if not cancelled
                                            error!("RTP forwarder error: {}", e);
                                        }
                                    });
                                    
                                    let sender = transport.clone_sender(); 
                                    let rtcp_socket = setup.rtcp_socket.clone();
                                    let rtcp_id = setup.rtcp_channel_id;
                                    let token = cancel_token.clone();
                                    
                                    tokio::spawn(async move {
                                        if let Err(e) = forward_udp(rtcp_socket, sender, rtcp_id, token).await {
                                            error!("RTCP forwarder error: {}", e);
                                        }
                                    });
                                }
                            }
                        }
                        
                        // Forward to Browser
                        if let Err(e) = transport.write_control(&resp.to_bytes()).await {
                            error!("Failed to write to Transport: {}", e);
                            break;
                        }
                    }
                }

                // _ = closed_fut => {
                //      error!("Connection closed");
                //      break;
                // }
            }
        }
        
        // Cleanup
        info!("Cleaning up connection...");
        cancel_token.cancel(); // Stop UDP forwarders
        
        // Send TEARDOWN if we have a session ID
        if let Some(sid) = session_id {
            info!("Sending TEARDOWN for session {}", sid);
            let teardown = format!(
                "TEARDOWN {} RTSP/1.0\r\nCSeq: 99\r\nSession: {}\r\n\r\n",
                self.rtsp_url, sid
            );
            
            // We ignore errors here as the connection might be broken
            let _ = tcp_write.write_all(teardown.as_bytes()).await;
        }

        Ok(())
    }
}

async fn forward_udp(
    socket: Arc<UdpSocket>, 
    sender: crate::transport::TransportSender, 
    channel_id: u8,
    token: CancellationToken
) -> Result<()> {
    let mut buf = [0u8; 2048];
    loop {
        tokio::select! {
            _ = token.cancelled() => {
                // info!("UDP forwarder cancelled");
                return Ok(());
            }
            res = socket.recv_from(&mut buf) => {
                match res {
                    Ok((n, _)) => {
                        let mut payload = bytes::BytesMut::with_capacity(n + 1);
                        payload.extend_from_slice(&[channel_id]);
                        payload.extend_from_slice(&buf[..n]);
                        
                        if let Err(e) = sender.send_datagram(payload.freeze()).await {
                            // If connection is closed, we should stop
                            return Err(anyhow::anyhow!("Failed to send datagram: {}", e));
                        }
                    }
                    Err(e) => {
                        return Err(anyhow::anyhow!("UDP recv error: {}", e));
                    }
                }
            }
        }
    }
}
