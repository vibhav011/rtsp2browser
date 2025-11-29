use anyhow::{anyhow, Result};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct RtspRequest {
    pub method: String,
    pub path: String,
    pub version: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct RtspResponse {
    pub version: String,
    pub status_code: u16,
    pub reason: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl RtspRequest {
    pub fn parse(data: &[u8]) -> Result<Option<(Self, usize)>> {
        let text = String::from_utf8_lossy(data);
        
        // Find double CRLF which marks end of headers
        let header_end = match text.find("\r\n\r\n") {
            Some(idx) => idx,
            None => return Ok(None), // Incomplete
        };

        let header_bytes = header_end + 4;
        let header_str = &text[..header_end];
        
        let mut lines = header_str.lines();
        let request_line = lines.next().ok_or_else(|| anyhow!("Empty request"))?;
        
        let parts: Vec<&str> = request_line.split_whitespace().collect();
        if parts.len() < 3 {
            return Err(anyhow!("Invalid request line"));
        }
        
        let method = parts[0].to_string();
        let path = parts[1].to_string();
        let version = parts[2].to_string();
        
        let mut headers = HashMap::new();
        for line in lines {
            if let Some((key, value)) = line.split_once(':') {
                headers.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
        
        // Check Content-Length
        let content_length: usize = headers
            .get("Content-Length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
            
        if data.len() < header_bytes + content_length {
            return Ok(None); // Incomplete body
        }
        
        let body = data[header_bytes..header_bytes + content_length].to_vec();
        
        Ok(Some((
            RtspRequest {
                method,
                path,
                version,
                headers,
                body,
            },
            header_bytes + content_length,
        )))
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("{} {} {}\r\n", self.method, self.path, self.version).as_bytes());
        
        for (k, v) in &self.headers {
            out.extend_from_slice(format!("{}: {}\r\n", k, v).as_bytes());
        }
        
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

impl RtspResponse {
    pub fn parse(data: &[u8]) -> Result<Option<(Self, usize)>> {
        let text = String::from_utf8_lossy(data);
        
        let header_end = match text.find("\r\n\r\n") {
            Some(idx) => idx,
            None => return Ok(None),
        };

        let header_bytes = header_end + 4;
        let header_str = &text[..header_end];
        
        let mut lines = header_str.lines();
        let status_line = lines.next().ok_or_else(|| anyhow!("Empty response"))?;
        
        let parts: Vec<&str> = status_line.split_whitespace().collect();
        if parts.len() < 3 {
            return Err(anyhow!("Invalid status line"));
        }
        
        let version = parts[0].to_string();
        let status_code: u16 = parts[1].parse().map_err(|_| anyhow!("Invalid status code"))?;
        let reason = parts[2..].join(" ");
        
        let mut headers = HashMap::new();
        for line in lines {
            if let Some((key, value)) = line.split_once(':') {
                headers.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
        
        let content_length: usize = headers
            .get("Content-Length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
            
        if data.len() < header_bytes + content_length {
            return Ok(None);
        }
        
        let body = data[header_bytes..header_bytes + content_length].to_vec();
        
        Ok(Some((
            RtspResponse {
                version,
                status_code,
                reason,
                headers,
                body,
            },
            header_bytes + content_length,
        )))
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("{} {} {}\r\n", self.version, self.status_code, self.reason).as_bytes());
        
        for (k, v) in &self.headers {
            out.extend_from_slice(format!("{}: {}\r\n", k, v).as_bytes());
        }
        
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}
