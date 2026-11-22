//! Minimal HTTP/1.0 GET over a unix socket: used for Docker and by the UI to read the agent's API.
//! HTTP/1.0 means the server sends a plain body and closes the connection, so we read to the end.

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
