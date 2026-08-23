use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use serde_json::json;
use sha2::{Digest, Sha256};

pub(super) const MAX_RESPONSE_BODY_BYTES: usize = 32 * 1024 * 1024;
// Reasoning models can legitimately take several minutes to complete a
// non-streaming turn. Keep this bounded, but longer than the three-minute
// retry window that can otherwise prevent a valid response from ever landing.
pub(super) const UPSTREAM_INFERENCE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;

static BRIDGE_NONCE: AtomicU64 = AtomicU64::new(1);

pub(super) fn route_token(port: u16) -> String {
    let nonce = BRIDGE_NONCE.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let digest = Sha256::digest(format!("{}:{port}:{nonce}:{now}", std::process::id()));
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug)]
pub(super) struct BridgeRequest {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct RequestReadError {
    message: &'static str,
    should_respond: bool,
}

impl RequestReadError {
    fn malformed(message: &'static str) -> Self {
        Self {
            message,
            should_respond: true,
        }
    }

    fn idle_connection(message: &'static str) -> Self {
        Self {
            message,
            should_respond: false,
        }
    }

    pub(super) fn message(&self) -> &'static str {
        self.message
    }

    pub(super) fn should_respond(&self) -> bool {
        self.should_respond
    }
}

pub(super) fn read_request(stream: &mut TcpStream) -> Result<BridgeRequest, RequestReadError> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if bytes.len() >= MAX_HEADER_BYTES {
            return Err(RequestReadError::malformed("request headers are too large"));
        }
        let mut chunk = [0_u8; 4096];
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(_) if bytes.is_empty() => {
                return Err(RequestReadError::idle_connection(
                    "request could not be read",
                ));
            }
            Err(_) => {
                return Err(RequestReadError::malformed("request could not be read"));
            }
        };
        if read == 0 {
            let error = if bytes.is_empty() {
                RequestReadError::idle_connection("connection closed before a request")
            } else {
                RequestReadError::malformed("request ended before its headers")
            };
            return Err(error);
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let header_text = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| RequestReadError::malformed("request headers are invalid"))?;
    let mut lines = header_text.split("\r\n");
    let mut request_line = lines
        .next()
        .ok_or_else(|| RequestReadError::malformed("request line is missing"))?
        .split_whitespace();
    let method = request_line
        .next()
        .ok_or_else(|| RequestReadError::malformed("request method is missing"))?
        .to_string();
    let path = request_line
        .next()
        .ok_or_else(|| RequestReadError::malformed("request path is missing"))?
        .to_string();
    let version = request_line
        .next()
        .ok_or_else(|| RequestReadError::malformed("request version is missing"))?;
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") || request_line.next().is_some() {
        return Err(RequestReadError::malformed("request line is invalid"));
    }

    let mut headers = Vec::new();
    let mut content_length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| RequestReadError::malformed("request header is invalid"))?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        if name == "transfer-encoding" {
            return Err(RequestReadError::malformed(
                "chunked requests are not supported",
            ));
        }
        if name == "content-length" {
            if content_length.is_some() {
                return Err(RequestReadError::malformed(
                    "request has multiple content lengths",
                ));
            }
            content_length =
                Some(value.parse::<usize>().map_err(|_| {
                    RequestReadError::malformed("request content length is invalid")
                })?);
        }
        headers.push((name, value));
    }
    let content_length = content_length
        .ok_or_else(|| RequestReadError::malformed("request body length is missing"))?;
    if content_length > MAX_REQUEST_BODY_BYTES {
        return Err(RequestReadError::malformed("request body is too large"));
    }

    let mut body = bytes[header_end..].to_vec();
    if body.len() > content_length {
        body.truncate(content_length);
    }
    while body.len() < content_length {
        let remaining = content_length - body.len();
        let mut chunk = vec![0_u8; remaining.min(8192)];
        let read = stream
            .read(&mut chunk)
            .map_err(|_| RequestReadError::malformed("request body could not be read"))?;
        if read == 0 {
            return Err(RequestReadError::malformed("request body ended early"));
        }
        body.extend_from_slice(&chunk[..read]);
    }

    Ok(BridgeRequest {
        method,
        path,
        headers,
        body,
    })
}

pub(super) fn write_error(
    stream: &mut TcpStream,
    status: StatusCode,
    message: &str,
) -> io::Result<()> {
    let body = serde_json::to_vec(&json!({"error": {"message": message}}))
        .unwrap_or_else(|_| b"{\"error\":{\"message\":\"bridge error\"}}".to_vec());
    write_response(stream, status, "application/json", &body)
}

pub(super) fn write_response(
    stream: &mut TcpStream,
    status: StatusCode,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("Response"),
        content_type,
        body.len(),
    )?;
    stream.write_all(body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_from_client(write: impl FnOnce(&mut TcpStream) + Send + 'static) -> RequestReadError {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            write(&mut stream);
        });
        let (mut server, _) = listener.accept().unwrap();
        let error = read_request(&mut server).unwrap_err();
        client.join().unwrap();
        error
    }

    #[test]
    fn upstream_timeout_allows_long_reasoning_without_becoming_unbounded() {
        assert!(UPSTREAM_INFERENCE_TIMEOUT > Duration::from_secs(3 * 60));
        assert!(UPSTREAM_INFERENCE_TIMEOUT <= Duration::from_secs(30 * 60));
    }

    #[test]
    fn idle_preconnection_closes_without_an_http_error() {
        let error = read_from_client(|_| {});
        assert!(!error.should_respond());
    }

    #[test]
    fn partial_request_still_gets_an_http_error() {
        let error = read_from_client(|stream| stream.write_all(b"POST").unwrap());
        assert!(error.should_respond());
    }
}
