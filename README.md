# RTSP to Browser (WebTransport + WebCodecs)

This project enables **low-latency, real-time viewing of RTSP video streams directly in a web browser**.

## Why this is useful

Viewing RTSP streams (common in security cameras and drones) in a browser is historically difficult because browsers do not natively support RTSP or raw TCP/UDP sockets.

Traditional solutions often involve:
*   **Transcoding to HLS/DASH**: Introduces significant latency (seconds to tens of seconds).
*   **WebSocket Proxies**: Wrap RTP packets in TCP. While better, TCP's reliable delivery mechanism can cause **Head-of-Line Blocking**, where one lost packet delays all subsequent packets, causing video stutters and increasing latency on unstable networks.

## How it works

This project solves these issues by using **WebTransport**, a modern web API that allows sending data over **HTTP/3 (QUIC)**. Crucially, WebTransport supports **unreliable datagrams** (like UDP), which are perfect for live video where timeliness is more important than perfect reliability.

### Architecture

1.  **RTSP Proxy Server (Rust)**:
    *   Acts as a bridge between the browser and the RTSP source.
    *   Connects to the RTSP source using standard TCP/UDP.
    *   Accepts incoming WebTransport connections from the browser.
    *   Forwards RTSP control messages (SETUP, PLAY, TEARDOWN) over a reliable stream.
    *   **Crucially**, forwards RTP video packets as **unreliable datagrams** over WebTransport. This ensures minimal latency.

2.  **Web Client (JavaScript)**:
    *   Connects to the proxy via WebTransport.
    *   Handles the RTSP handshake logic.
    *   **Depacketizes** the incoming RTP datagrams to extract H.264 NAL units.
    *   Uses **WebCodecs (`VideoDecoder`)** for decoding.
    *   Renders the decoded frames efficiently using **WebGL**.
    *   Runs the heavy lifting (networking, decoding, rendering) in a **Web Worker** to keep the UI responsive.

## Usage

### Prerequisites
*   Rust (cargo)
*   A modern browser with WebTransport and WebCodecs support (Chrome 94+, Edge, Firefox 114+).

### 1. Run the Proxy Server
The proxy server handles the WebTransport connection and forwards traffic to the RTSP stream.

```bash
cd proxy-server
# Run the proxy (defaults to listening on port 4433)
cargo run --bin server
```

*(Optional) To simulate an RTSP stream if you don't have a camera:*
```bash
# In a separate terminal
cd proxy-server
cargo run --bin rtsp-server
```

### 2. Run the Web Client
Serve the static files for the browser.

```bash
cd client
python3 -m http.server 8000
```

### 3. View the Stream
1.  Open Chrome and navigate to `http://localhost:8000`.
2.  Enter the WebTransport Proxy URL (default: `https://127.0.0.1:4433/`).
3.  Enter your RTSP Stream URL (e.g., `rtsp://127.0.0.1:8554/test` or your camera's IP).
4.  Click **Connect**.

> **Note**: Since the proxy uses a self-signed certificate for development, you may need to launch your browser with specific flags to accept the hash.
