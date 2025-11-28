
const LOG_LEVEL = 'info';

function log(msg, level = 'info') {
    postMessage({ type: 'log', msg, level });
}

class H264Depacketizer {
    constructor(onFrame) {
        this.onFrame = onFrame;
        this.fragmentBuffer = null;
        this.fragmentType = null;
        this.fragmentTimestamp = null;
        this.lastSequenceNumber = null;
        this.packetStats = { total: 0, lost: 0, outOfOrder: 0 };
    }

    process(packet) {
        if (packet.length < 12) {
            log(`Packet too short: ${packet.length} bytes`);
            return;
        }

        // Parse RTP Header
        const v_p_x_cc = packet[0];
        const x_bit = (v_p_x_cc & 0x10) >> 4;
        const cc = (v_p_x_cc & 0x0F);

        // Extract sequence number (bytes 2-3)
        const sequenceNumber = (packet[2] << 8) | packet[3];

        // Extract timestamp (bytes 4-7)
        const timestamp = ((packet[4] << 24) | (packet[5] << 16) | (packet[6] << 8) | packet[7]) >>> 0;

        // Track sequence numbers for debugging
        this.packetStats.total++;
        if (this.lastSequenceNumber !== null) {
            const expectedSeq = (this.lastSequenceNumber + 1) & 0xFFFF;  // 16-bit wraparound
            if (sequenceNumber !== expectedSeq) {
                if (sequenceNumber < this.lastSequenceNumber && (this.lastSequenceNumber - sequenceNumber) < 100) {
                    this.packetStats.outOfOrder++;
                    log(`WARNING: Out-of-order packet! Expected seq ${expectedSeq}, got ${sequenceNumber}`);
                } else {
                    const lost = (sequenceNumber - expectedSeq) & 0xFFFF;
                    this.packetStats.lost += lost;
                    log(`WARNING: Packet loss detected! Expected seq ${expectedSeq}, got ${sequenceNumber}. Lost: ${lost}`);
                }
            }
        }
        this.lastSequenceNumber = sequenceNumber;

        // Log stats periodically
        if (this.packetStats.total % 1000 === 0) {
            log(`RTP Stats: Total=${this.packetStats.total}, Lost=${this.packetStats.lost}, OutOfOrder=${this.packetStats.outOfOrder}`);
        }

        let payloadOffset = 12 + (cc * 4);

        // Handle Extension (X) bit
        if (x_bit) {
            if (packet.length < payloadOffset + 4) {
                log('Packet too short for extension header');
                return;
            }
            // Extension header: 2 bytes profile + 2 bytes length
            const extLen = (packet[payloadOffset + 2] << 8) | packet[payloadOffset + 3];
            payloadOffset += 4 + (extLen * 4);
        }

        if (packet.length < payloadOffset) {
            log('Packet too short after header parsing');
            return;
        }

        let payload = packet.subarray(payloadOffset);

        if (payload.length === 0) return;

        // NAL Unit Header
        const nalHeader = payload[0];
        const forbidden_zero_bit = (nalHeader & 0x80) >> 7;
        if (forbidden_zero_bit !== 0) {
            log(`Forbidden zero bit set in NAL header: ${nalHeader}`);
            return;
        }
        const nal_ref_idc = (nalHeader & 0x60) >> 5;
        const nal_unit_type = nalHeader & 0x1F;

        if (nal_unit_type >= 1 && nal_unit_type <= 23) {
            // Single NAL Unit Packet

            // log(`Single NAL type ${nal_unit_type}, payload size ${payload.length}`);
            const data = new Uint8Array(4 + payload.length);
            data.set([0, 0, 0, 1], 0);
            data.set(payload, 4);
            this.onFrame(data, timestamp);
        } else if (nal_unit_type === 28 || nal_unit_type === 29) {
            // FU-A or FU-B (Fragmented Unit)
            const fuHeader = payload[1];
            const s_bit = (fuHeader & 0x80) >> 7;
            const e_bit = (fuHeader & 0x40) >> 6;
            const r_bit = (fuHeader & 0x20) >> 5;
            const fuType = fuHeader & 0x1F;
            let nal_payload_idx = 2;
            if (nal_unit_type === 29) {
                nal_payload_idx = 4;
            }

            if (s_bit) {
                // Start of fragment
                // log(`FU-A/FU-B Start: type ${fuType}, payload size ${payload.length - nal_payload_idx}`);
                const reconstructedNalHeader = (nal_ref_idc << 5) | fuType;
                this.fragmentBuffer = [new Uint8Array([reconstructedNalHeader]), payload.subarray(nal_payload_idx)];
                this.fragmentType = fuType;
                this.fragmentTimestamp = timestamp; // Store timestamp for this fragment
            } else if (this.fragmentBuffer && this.fragmentType === fuType) {
                // Middle or End
                this.fragmentBuffer.push(payload.subarray(nal_payload_idx));
                if (e_bit) {
                    // End of fragment
                    // Concatenate
                    let totalLen = 4; // Start code
                    for (const chunk of this.fragmentBuffer) totalLen += chunk.length;

                    // Sanity check: ensure fragment is reasonable size
                    if (totalLen < 20) {
                        log(`WARNING: Suspiciously small fragmented NAL: ${totalLen} bytes. Discarding.`);
                        this.fragmentBuffer = null;
                        return;
                    }

                    // log(`FU-A/FU-B End: total size ${totalLen}`);
                    const data = new Uint8Array(totalLen);
                    data.set([0, 0, 0, 1], 0);
                    let offset = 4;
                    for (const chunk of this.fragmentBuffer) {
                        data.set(chunk, offset);
                        offset += chunk.length;
                    }
                    this.onFrame(data, this.fragmentTimestamp || timestamp);
                    this.fragmentBuffer = null;
                }
            } else {
                // Received middle/end fragment without start - packet loss
                log(`WARNING: FU-A/FU-B fragment without start bit. Packet loss detected.`);
            }
        } else {
            log(`Unsupported NAL Type: ${nal_unit_type}`, 'warn');
        }
    }
}

class RTSPClient {
    constructor(url, rtspUrl, canvas) {
        this.url = url;
        this.rtspUrl = rtspUrl;
        this.canvas = canvas;
        // this.ctx = this.canvas.getContext('2d');
        this.gl = this.canvas.getContext('webgl2') || this.canvas.getContext('webgl');
        if (!this.gl) {
            log('WebGL not supported', 'error');
        } else {
            this.initWebGL();
        }

        this.transport = null;
        this.controlStream = null;
        this.writer = null;
        this.reader = null;
        this.cseq = 1;
        this.decoder = null;
        this.depacketizer = new H264Depacketizer(this.onNalUnit.bind(this));

        this.spsPps = null; // Will store SPS/PPS from SDP
        this.streamSPS = null; // Buffer SPS from stream
        this.streamPPS = null; // Buffer PPS from stream
        this.hasSeenKeyFrame = false; // Track if we've seen a keyframe
        this.videoChannelId = null; // Dynamically assigned by server
        this.profileLevelId = '42001E'; // Default fallback
        this.sentSPSPPS = false; // Track if we've sent SPS/PPS to decoder
    }

    async connect() {
        log(`Connecting to ${this.url}...`);

        // Append RTSP URL as query param
        const connectionUrl = `${this.url}?rtsp=${encodeURIComponent(this.rtspUrl)}`;

        try {
            const HASH = new Uint8Array([80, 60, 8, 29, 216, 215, 88, 188, 104, 47, 68, 80, 64, 58, 157, 239, 233, 248, 79, 248, 120, 100, 241, 127, 140, 240, 12, 254, 124, 69, 89, 67]);
            this.transport = new WebTransport(connectionUrl, { serverCertificateHashes: [{ algorithm: "sha-256", value: HASH.buffer }] });
            await this.transport.ready;
            log('WebTransport connected');
        } catch (e) {
            log(`Connection failed: ${e}`, 'error');
            return;
        }

        // Setup VideoDecoder (will configure later after getting SPS/PPS from SDP)
        this.decoder = new VideoDecoder({
            output: (frame) => {
                // this.ctx.drawImage(frame, 0, 0, this.canvas.width, this.canvas.height);
                if (this.gl) {
                    this.renderFrame(frame);
                }
                frame.close();
            },
            error: (e) => log(`Decoder error: ${e}`, 'error')
        });

        // Open Control Stream
        this.controlStream = await this.transport.createBidirectionalStream();
        this.writer = this.controlStream.writable.getWriter();
        this.reader = this.controlStream.readable.getReader();

        // Start reading control responses
        this.readControl();

        // Start reading datagrams
        this.readDatagrams();

        // Start RTSP Handshake
        await this.sendRTSP('OPTIONS', this.rtspUrl);
        await this.sendRTSP('DESCRIBE', this.rtspUrl);
    }

    initWebGL() {
        const gl = this.gl;

        // Vertex Shader
        const vsSource = `
            attribute vec2 a_position;
            attribute vec2 a_texCoord;
            varying vec2 v_texCoord;
            void main() {
                gl_Position = vec4(a_position, 0.0, 1.0);
                v_texCoord = a_texCoord;
            }
        `;

        // Fragment Shader
        const fsSource = `
            precision mediump float;
            varying vec2 v_texCoord;
            uniform sampler2D u_image;
            void main() {
                gl_FragColor = texture2D(u_image, v_texCoord);
            }
        `;

        // Compile shaders
        const vs = this.compileShader(gl, gl.VERTEX_SHADER, vsSource);
        const fs = this.compileShader(gl, gl.FRAGMENT_SHADER, fsSource);

        // Link program
        this.program = gl.createProgram();
        gl.attachShader(this.program, vs);
        gl.attachShader(this.program, fs);
        gl.linkProgram(this.program);

        if (!gl.getProgramParameter(this.program, gl.LINK_STATUS)) {
            log('WebGL program link failed: ' + gl.getProgramInfoLog(this.program), 'error');
            return;
        }

        gl.useProgram(this.program);

        // Look up locations
        this.positionLocation = gl.getAttribLocation(this.program, "a_position");
        this.texCoordLocation = gl.getAttribLocation(this.program, "a_texCoord");
        this.imageLocation = gl.getUniformLocation(this.program, "u_image");

        // Provide texture coordinates for the rectangle.
        this.texCoordBuffer = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, this.texCoordBuffer);
        // Flip Y for WebGL texture coordinates
        gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([
            0.0, 1.0,
            1.0, 1.0,
            0.0, 0.0,
            1.0, 0.0,
        ]), gl.STATIC_DRAW);

        // Create a buffer to put the vertices in
        this.positionBuffer = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, this.positionBuffer);
        gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([
            -1.0, -1.0,
            1.0, -1.0,
            -1.0, 1.0,
            1.0, 1.0,
        ]), gl.STATIC_DRAW);

        // Create a texture.
        this.texture = gl.createTexture();
        gl.bindTexture(gl.TEXTURE_2D, this.texture);

        // Set the parameters so we can render any size image.
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    }

    compileShader(gl, type, source) {
        const shader = gl.createShader(type);
        gl.shaderSource(shader, source);
        gl.compileShader(shader);
        if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
            log('Shader compile failed: ' + gl.getShaderInfoLog(shader), 'error');
            gl.deleteShader(shader);
            return null;
        }
        return shader;
    }

    renderFrame(frame) {
        const gl = this.gl;

        gl.viewport(0, 0, gl.drawingBufferWidth, gl.drawingBufferHeight);
        gl.clearColor(0, 0, 0, 1);
        gl.clear(gl.COLOR_BUFFER_BIT);

        gl.useProgram(this.program);

        // Turn on the position attribute
        gl.enableVertexAttribArray(this.positionLocation);
        gl.bindBuffer(gl.ARRAY_BUFFER, this.positionBuffer);
        gl.vertexAttribPointer(this.positionLocation, 2, gl.FLOAT, false, 0, 0);

        // Turn on the texcoord attribute
        gl.enableVertexAttribArray(this.texCoordLocation);
        gl.bindBuffer(gl.ARRAY_BUFFER, this.texCoordBuffer);
        gl.vertexAttribPointer(this.texCoordLocation, 2, gl.FLOAT, false, 0, 0);

        // Bind the texture
        gl.activeTexture(gl.TEXTURE0);
        gl.bindTexture(gl.TEXTURE_2D, this.texture);

        // Upload the video frame to the texture
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, frame);

        // Draw the rectangle.
        gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    }

    async sendRTSP(method, url, headers = {}) {
        let msg = `${method} ${url} RTSP/1.0\r\n`;
        msg += `CSeq: ${this.cseq++}\r\n`;
        msg += `User-Agent: WebTransportClient\r\n`;
        for (const [k, v] of Object.entries(headers)) {
            msg += `${k}: ${v}\r\n`;
        }
        msg += `\r\n`;

        log(`Sending ${method}`);
        await this.writer.write(new TextEncoder().encode(msg));
    }

    async readControl() {
        const decoder = new TextDecoder();
        try {
            while (true) {
                const { value, done } = await this.reader.read();
                if (done) break;
                const text = decoder.decode(value);
                log(`RTSP Response: ${text}`);

                // Simple state machine
                if (text.includes('RTSP/1.0 200 OK')) {
                    if (text.includes('Public:')) {
                        // Response to OPTIONS
                        // Next: DESCRIBE
                    } else if (text.includes('Content-Type: application/sdp')) {
                        // Response to DESCRIBE
                        // Parse SDP (for potential future use)
                        this.parseSDP(text);

                        // Configure decoder for Annex B format
                        // Since we're sending NAL units with start codes (Annex B),
                        this.decoder.configure({
                            codec: `avc1.${this.profileLevelId}`,
                            hardwareAcceleration: 'prefer-software',
                            optimizeForLatency: false,
                            colorSpace: {
                                matrix: 'smpte170m',
                                primaries: 'smpte170m',
                                transfer: 'smpte170m'
                            }
                        });
                        log('VideoDecoder configured for Annex B format');

                        // Next: SETUP
                        await this.sendRTSP('SETUP', this.rtspUrl + '/stream=0', {
                            'Transport': 'RTP/AVP;unicast;client_port=0-0'
                        });
                    } else if (text.includes('Transport:')) {
                        // Response to SETUP
                        // Check for Session ID
                        const match = text.match(/Session:\s*(\S+)/);
                        if (match) {
                            this.sessionId = match[1].split(';')[0];
                        }

                        // Check for Channel ID injection
                        // Format: Transport: ...;x-wt-channel-id=0-1
                        const channelMatch = text.match(/x-wt-channel-id=(\d+)-(\d+)/);
                        if (channelMatch) {
                            this.videoChannelId = parseInt(channelMatch[1], 10);
                            const rtcpChannelId = parseInt(channelMatch[2], 10);
                            log(`Assigned Channel IDs: Video=${this.videoChannelId}, RTCP=${rtcpChannelId}`);
                        } else {
                            log('WARNING: No x-wt-channel-id found in Transport header. Defaulting to 0.', 'warn');
                            this.videoChannelId = 0;
                        }

                        // Next: PLAY
                        if (this.sessionId) {
                            await this.sendRTSP('PLAY', this.rtspUrl, { Session: this.sessionId });
                        }
                    }
                }
            }
        } catch (e) {
            log(`Control stream error: ${e}`, 'error');
        }
    }

    async readDatagrams() {
        const reader = this.transport.datagrams.readable.getReader();
        try {
            while (true) {
                const { value, done } = await reader.read();
                if (done) break;

                // value is Uint8Array
                // First byte is Channel ID
                const channelId = value[0];
                const payload = value.subarray(1);

                if (this.videoChannelId !== null && channelId === this.videoChannelId) {
                    this.depacketizer.process(payload);
                }
            }
        } catch (e) {
            log(`Datagram error: ${e}`, 'error');
        }
    }

    parseSDP(sdpText) {
        // Extract profile-level-id
        const profileMatch = sdpText.match(/profile-level-id=([0-9a-fA-F]+)/);
        if (profileMatch) {
            this.profileLevelId = profileMatch[1].toUpperCase();
            log(`Parsed profile-level-id: ${this.profileLevelId}`);
        }

        // Extract sprop-parameter-sets from SDP
        // Format: a=fmtp:96 ... sprop-parameter-sets=<SPS base64>,<PPS base64>
        const match = sdpText.match(/sprop-parameter-sets=([^;\s]+)/);
        if (!match) {
            log('No sprop-parameter-sets found in SDP');
            return;
        }

        const paramSets = match[1].split(',');
        if (paramSets.length < 2) {
            log('Invalid sprop-parameter-sets format');
            return;
        }

        try {
            // Base64 decode SPS and PPS
            const sps = Uint8Array.from(atob(paramSets[0]), c => c.charCodeAt(0));
            const pps = Uint8Array.from(atob(paramSets[1]), c => c.charCodeAt(0));

            // Combine into Annex B format: [start code][SPS][start code][PPS]
            const description = new Uint8Array(4 + sps.length + 4 + pps.length);
            description.set([0, 0, 0, 1], 0);
            description.set(sps, 4);
            description.set([0, 0, 0, 1], 4 + sps.length);
            description.set(pps, 4 + sps.length + 4);

            // this.spsPps = description;
            log('Parsed SPS/PPS from SDP');
        } catch (e) {
            log(`Failed to parse SPS/PPS: ${e}`, 'error');
        }
    }

    onNalUnit(data, timestamp) {
        // data is Annex B NAL Unit (00 00 00 01 <NAL>)
        // timestamp is RTP timestamp

        // Check if decoder is ready
        if (!this.decoder || this.decoder.state !== 'configured') {
            return; // Skip if decoder not configured yet
        }

        // Extract NAL type from the data (after start code)
        // Start code is 4 bytes (00 00 00 01)
        if (data.length < 5) return;

        const nalHeader = data[4];
        const nalType = nalHeader & 0x1F;

        // NAL type 5 = IDR (keyframe), 7 = SPS, 8 = PPS, 6 = SEI

        // Buffer SPS and PPS when we receive them from the stream
        if (nalType === 7) {
            // SPS
            this.streamSPS = data;
            // log(`Buffered SPS from stream (${data.length} bytes)`);
            return; // Don't send SPS alone to decoder yet
        } else if (nalType === 8) {
            // PPS
            this.streamPPS = data;
            // log(`Buffered PPS from stream (${data.length} bytes)`);
            return; // Don't send PPS alone to decoder yet
        }

        // For IDR frames, prepend buffered SPS/PPS
        if (nalType === 5) {
            if (!this.hasSeenKeyFrame) {
                log(`First keyframe received! NAL size: ${data.length}`);
                this.hasSeenKeyFrame = true;
            }

            // Prepend SPS/PPS to EVERY IDR frame
            // Use stream SPS/PPS if available, otherwise fallback to SDP
            // Note: this.spsPps from SDP is [start code][SPS][start code][PPS]
            // We need to extract just the SPS and PPS NAL units from it.
            let spsToPrepend = this.streamSPS;
            let ppsToPrepend = this.streamPPS;

            if (!spsToPrepend && this.spsPps) {
                // Extract SPS from this.spsPps (assuming it's [0001][SPS][0001][PPS])
                let spsStart = 4;
                let spsEnd = -1;
                for (let i = spsStart; i < this.spsPps.length - 3; i++) {
                    if (this.spsPps[i] === 0 && this.spsPps[i + 1] === 0 && this.spsPps[i + 2] === 0 && this.spsPps[i + 3] === 1) {
                        spsEnd = i;
                        break;
                    }
                }
                if (spsEnd !== -1) {
                    spsToPrepend = this.spsPps.subarray(spsStart, spsEnd);
                }
            }

            if (!ppsToPrepend && this.spsPps) {
                // Extract PPS from this.spsPps
                let ppsStart = -1;
                for (let i = 0; i < this.spsPps.length - 3; i++) {
                    if (this.spsPps[i] === 0 && this.spsPps[i + 1] === 0 && this.spsPps[i + 2] === 0 && this.spsPps[i + 3] === 1) {
                        if (ppsStart === -1) { // Found first start code (SPS)
                            ppsStart = i + 4; // PPS starts after this start code
                        } else { // Found second start code (PPS)
                            ppsStart = i + 4; // PPS starts after this start code
                            break;
                        }
                    }
                }
                if (ppsStart !== -1) {
                    ppsToPrepend = this.spsPps.subarray(ppsStart);
                }
            }

            if (spsToPrepend && ppsToPrepend) {
                const combinedData = new Uint8Array(spsToPrepend.length + ppsToPrepend.length + data.length);
                combinedData.set(spsToPrepend, 0);
                combinedData.set(ppsToPrepend, spsToPrepend.length);
                combinedData.set(data, spsToPrepend.length + ppsToPrepend.length);
                data = combinedData;
                // log(`Prepended SPS+PPS to IDR frame. Total size: ${data.length}`);
            } else {
                log('WARNING: No SPS/PPS available for IDR frame. Skipping this frame.', 'error');
                return; // Skip IDR if we don't have SPS/PPS
            }
        }

        // Skip all frames until we see the first keyframe
        if (!this.hasSeenKeyFrame) {
            log(`Skipping NAL type ${nalType} - waiting for keyframe`);
            return;
        }

        // Mark IDR frames as keyframes, everything else as delta
        const frameType = (nalType === 5) ? 'key' : 'delta';

        // We need to convert RTP timestamp to microseconds for EncodedVideoChunk
        // H.264 usually 90000 Hz clock.
        const timestampUs = (timestamp / 90000) * 1_000_000;

        const chunk = new EncodedVideoChunk({
            type: frameType,
            timestamp: timestampUs,
            data: data
        });

        try {
            this.decoder.decode(chunk);
        } catch (e) {
            log(`Decode error: ${e}`, 'error');
        }
    }
}

self.onmessage = (e) => {
    const { type, url, rtspUrl, canvas } = e.data;
    if (type === 'init') {
        const client = new RTSPClient(url, rtspUrl, canvas);
        client.connect();
    }
};
