//! Minimal HTTP over unix sockets, both sides, no HTTP crate.
//!   client: one GET (HTTP/1.0: the server sends a plain body and closes, so we read to the end)
//!   server: read one request line, write one response, close (the agent's API, the Docker helper)

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::error::{Result, err};

const MAX_RESPONSE: u64 = 4 * 1024 * 1024;

/// Returns (status code, body).
pub fn get(socket: &Path, path: &str, timeout: Duration) -> Result<(u16, String)> {
    let mut stream =
        UnixStream::connect(socket).map_err(|e| err(format!("{}: {e}", socket.display())))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(format!("GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").as_bytes())?;
    let mut raw = Vec::new();
    stream.take(MAX_RESPONSE).read_to_end(&mut raw)?;
    parse_response(&raw)
}

pub fn parse_response(raw: &[u8]) -> Result<(u16, String)> {
    let text = std::str::from_utf8(raw).map_err(|_| err("http: response is not UTF-8"))?;
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| err("http: no end of headers"))?;
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| err("http: bad status line"))?;
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        return Err(err("http: chunked response not supported"));
    }
    Ok((status, body.to_string()))
}

// ---------- server side ----------

const MAX_REQUEST: usize = 8 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

pub enum Request {
    Line { method: String, target: String },
    TooLarge,
}

/// Reads one request head. `None` = the client closed without sending one.
pub fn read_request(stream: &mut UnixStream) -> std::io::Result<Option<Request>> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        if buf.len() > MAX_REQUEST {
            return Ok(Some(Request::TooLarge));
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf);
    let mut parts = head.lines().next().unwrap_or("").split_whitespace();
    Ok(Some(Request::Line {
        method: parts.next().unwrap_or("").to_string(),
        target: parts.next().unwrap_or("").to_string(),
    }))
}

/// Writes a complete JSON response and lets the caller close the connection.
pub fn respond(stream: &mut UnixStream, status: u16, json_body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        json_body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(json_body.as_bytes())
}

/// Errors from a client that hung up or timed out aren't worth a log line.
pub fn is_client_gone(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        e.kind(),
        BrokenPipe | WouldBlock | TimedOut | ConnectionReset
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_and_body() {
        let (code, body) =
            parse_response(b"HTTP/1.1 503 Service Unavailable\r\nA: b\r\n\r\n{}").unwrap();
        assert_eq!((code, body.as_str()), (503, "{}"));
    }

    #[test]
    fn errors() {
        assert!(parse_response(b"garbage").is_err());
        assert!(parse_response(b"HTTP/1.1 abc\r\n\r\n").is_err());
        assert!(
            parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n").is_err()
        );
    }
}
