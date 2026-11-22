//! `pios helper`: the only PiOS process that touches Docker (it runs as root; the agent doesn't).
//! It forwards exactly two read-only requests and answers 403 to anything else, so access to its
//! socket is not access to Docker.
//!
//! systemd starts it through `pios-helper.socket` and hands over the listening socket as fd 3
//! (socket activation). Without systemd, set PIOS_HELPER_SOCKET to listen on a path (tests).

use std::os::fd::FromRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Result, err};
use crate::http;

/// The whole allowlist: the container list (every 5 s) and disk usage (Disk screen, cached 10 min).
pub const ALLOWED: &str = "/containers/json?all=true";
pub const DOCKER_DF: &str = "/system/df";
const ALLOWLIST: [&str; 2] = [ALLOWED, DOCKER_DF];
const DOCKER_TIMEOUT: Duration = Duration::from_secs(2);

/// What to answer for one request line: (status, JSON body). Pure, so it's tested directly.
pub fn decide(method: &str, target: &str) -> Option<(u16, String)> {
    if method != "GET" {
        return Some((405, r#"{"error":"only GET"}"#.into()));
    }
    if !ALLOWLIST.contains(&target) {
        return Some((403, r#"{"error":"not allowed"}"#.into()));
    }
    None // allowed: forward to Docker
}

fn handle(stream: &mut UnixStream, docker: &Path) -> std::io::Result<()> {
    let Some(req) = http::read_request(stream)? else {
        return Ok(());
    };
    let (method, target) = match req {
        http::Request::TooLarge => {
            return http::respond(stream, 431, r#"{"error":"request too large"}"#);
        }
        http::Request::Line { method, target } => (method, target),
    };
    if let Some((status, body)) = decide(&method, &target) {
        return http::respond(stream, status, &body);
    }
    // forward the matched constant, never the client's string
    let Some(path) = ALLOWLIST.iter().find(|p| **p == target) else {
        return http::respond(stream, 403, r#"{"error":"not allowed"}"#);
    };
    let timeout = if *path == DOCKER_DF { Duration::from_secs(10) } else { DOCKER_TIMEOUT };
    match http::get(docker, path, timeout) {
        Ok((status, body)) => http::respond(stream, status, &body),
        Err(e) => http::respond(
            stream,
            502,
            &serde_json::json!({ "error": e.to_string() }).to_string(),
        ),
    }
}

/// The socket systemd passed us, if any (LISTEN_PID must be us, LISTEN_FDS ≥ 1, first fd is 3).
fn activated_listener() -> Option<UnixListener> {
    let pid: u32 = std::env::var("LISTEN_PID").ok()?.parse().ok()?;
    let fds: u32 = std::env::var("LISTEN_FDS").ok()?.parse().ok()?;
    if pid != std::process::id() || fds < 1 {
        return None;
    }
    // SAFETY: systemd guarantees fd 3 is our listening socket when LISTEN_PID/LISTEN_FDS match.
    Some(unsafe { UnixListener::from_raw_fd(3) })
}

pub fn run(docker: PathBuf) -> Result<()> {
    let listener = match activated_listener() {
        Some(l) => l,
        None => {
            let path = std::env::var_os("PIOS_HELPER_SOCKET")
                .map(PathBuf::from)
                .ok_or_else(|| {
                    err("pios helper: not started by systemd and PIOS_HELPER_SOCKET isn't set")
                })?;
            crate::api::bind(&path)?
        }
    };
    for conn in listener.incoming() {
        let Ok(mut stream) = conn else { continue };
        if let Err(e) = handle(&mut stream, &docker)
            && !http::is_client_gone(&e)
        {
            eprintln!("pios helper: {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_container_list_passes() {
        assert_eq!(decide("GET", "/containers/json?all=true"), None);
        assert_eq!(decide("GET", "/system/df"), None);
        for bad in [
            "/containers/json",
            "/containers/redis/stop",
            "/containers/json?all=true&x=1",
            "/images/json",
            "/system/df?verbose=1",
            "/system/prune",
            "/v1.45/containers/json?all=true",
            "/containers/json?all=true/../create",
            "",
        ] {
            assert_eq!(decide("GET", bad).map(|d| d.0), Some(403), "{bad}");
        }
        assert_eq!(decide("POST", ALLOWED).map(|d| d.0), Some(405));
        assert_eq!(decide("DELETE", "/containers/x").map(|d| d.0), Some(405));
    }
}
