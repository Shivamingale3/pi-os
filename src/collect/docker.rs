//! Containers ← Docker's HTTP API on its unix socket, one GET, written by hand (no HTTP crate).
//! HTTP/1.0 so Docker sends a plain body and closes the connection: read to end, done.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Result, err};
use crate::types::{Container, Docker};

const REQUEST: &[u8] = b"GET /containers/json?all=true HTTP/1.0\r\nHost: docker\r\n\r\n";
const MAX_RESPONSE: u64 = 4 * 1024 * 1024;

#[derive(Deserialize)]
struct ApiContainer {
    #[serde(rename = "Names", default)]
    names: Vec<String>,
    #[serde(rename = "State")]
    state: String,
}

pub fn parse_response(raw: &[u8]) -> Result<Docker> {
    let text = std::str::from_utf8(raw).map_err(|_| err("docker: response is not UTF-8"))?;
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| err("docker: no end of headers"))?;
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("");
    if status != "200" {
        return Err(err(format!("docker: HTTP {status}")));
    }
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        return Err(err("docker: chunked response not supported"));
    }
    let list: Vec<ApiContainer> = serde_json::from_str(body)?;
    let mut containers: Vec<Container> = list
        .into_iter()
        .map(|c| Container {
            name: c
                .names
                .first()
                .map(|n| n.trim_start_matches('/').to_string())
                .unwrap_or_default(),
            state: c.state,
        })
        .collect();
    containers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Docker {
        running: containers.iter().filter(|c| c.state == "running").count() as u32,
        total: containers.len() as u32,
        containers,
    })
}

/// Any failure (no Docker, no permission, timeout, bad answer) → Err; the caller stores None.
pub fn read(socket: &Path, timeout: Duration) -> Result<Docker> {
    let mut stream = UnixStream::connect(socket)
        .map_err(|e| err(format!("docker: {}: {e}", socket.display())))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(REQUEST)?;
    let mut raw = Vec::new();
    stream.take(MAX_RESPONSE).read_to_end(&mut raw)?;
    parse_response(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_running() {
        let raw = b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n[{\"Names\":[\"/redis\"],\"State\":\"running\",\"Id\":\"x\"},{\"Names\":[\"/minio\"],\"State\":\"exited\"}]";
        let d = parse_response(raw).unwrap();
        assert_eq!((d.running, d.total), (1, 2));
        assert_eq!(d.containers[0].name, "minio");
        assert_eq!(d.containers[1].state, "running");
    }

    #[test]
    fn errors() {
        assert!(parse_response(b"HTTP/1.0 500 Internal\r\n\r\n{}").is_err());
        assert!(parse_response(b"HTTP/1.0 200 OK\r\n\r\n{\"not\":\"a list\"}").is_err());
        assert!(parse_response(b"garbage").is_err());
        assert!(
            parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n[]\r\n")
                .is_err()
        );
    }

    #[test]
    fn missing_socket_is_an_error() {
        assert!(
            read(
                Path::new("/nonexistent/docker.sock"),
                Duration::from_millis(100)
            )
            .is_err()
        );
    }
}
