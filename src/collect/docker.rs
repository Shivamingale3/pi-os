//! Containers ← Docker's HTTP API on its unix socket: one GET through `crate::http`.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Result, err};
use crate::types::{Container, Docker};

#[derive(Deserialize)]
struct ApiContainer {
    #[serde(rename = "Id", default)]
    id: String,
    #[serde(rename = "Names", default)]
    names: Vec<String>,
    #[serde(rename = "State")]
    state: String,
}

/// A full raw HTTP response (used by tests).
#[cfg(test)]
pub fn parse_response(raw: &[u8]) -> Result<Docker> {
    let (status, body) = crate::http::parse_response(raw)?;
    parse(status, &body)
}

fn parse(status: u16, body: &str) -> Result<Docker> {
    if status != 200 {
        return Err(err(format!("docker: HTTP {status}")));
    }
    let list: Vec<ApiContainer> = serde_json::from_str(body)?;
    let mut containers: Vec<Container> = list
        .into_iter()
        .map(|c| Container {
            id: c.id,
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
    let (status, body) = crate::http::get(socket, "/containers/json?all=true", timeout)
        .map_err(|e| err(format!("docker: {e}")))?;
    parse(status, &body)
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
