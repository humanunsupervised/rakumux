//! A minimal blocking `GET /v1/health`, so `--detach` can wait for the API
//! without an HTTP client stack.

use crate::dto::Health;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// Ask the API at `url` (`http://127.0.0.1:<port>`) for its health.
pub fn health(url: &str, timeout: Duration) -> Result<Health, String> {
    let authority = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("{url} is not an http:// URL"))?
        .trim_end_matches('/');
    let addr: SocketAddr = authority
        .parse()
        .map_err(|e| format!("{url} has no IP address and port: {e}"))?;
    let mut stream =
        TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("connect {url}: {e}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|_| stream.set_write_timeout(Some(timeout)))
        .map_err(|e| format!("socket: {e}"))?;
    let request = format!(
        "GET /v1/health HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\n\
         Connection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("send to {url}: {e}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("read from {url}: {e}"))?;
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| format!("{url} sent no HTTP response"))?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let status = head.split_whitespace().nth(1).unwrap_or("");
    if status != "200" {
        return Err(format!("{url} answered {status} to /v1/health"));
    }
    serde_json::from_slice(&raw[split + 4..])
        .map_err(|e| format!("{url}: unexpected health body: {e}"))
}
