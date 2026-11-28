//! `pios helper`: the only PiOS process that touches Docker (it runs as root; the agent doesn't).
//! It forwards exactly two read-only requests and one action, restarting one container by its
//! full ID, and answers 403 to anything else, so access to its socket is not access to Docker.
//! It also does two things outside Docker: edits fan_ctl's manual override (`fans.rs`), one fan
//! at a time, with values checked again here; and reads a data disk's health report
//! (`GET /smart/sdX`: smartctl -n standby, so a sleeping disk is never woken).
//! The unit gives it CAP_SYS_RAWIO and read access to sd* block devices for that, nothing more.
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
/// Docker gives the container 10 s to stop before killing it, then starts it again.
const RESTART_TIMEOUT: Duration = Duration::from_secs(40);
/// At most this many restarts at once (each runs on its own thread; the unit allows 4 tasks).
const MAX_RESTARTS: usize = 2;
static RESTARTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A full container ID: exactly 64 lowercase hex digits.
pub fn is_id(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The one action's path. The helper rebuilds it from the ID, never forwards the client's text.
pub fn restart_path(id: &str) -> String {
    format!("/containers/{id}/restart?t=10")
}

/// "/containers/ID/restart?t=10" → ID
fn restart_id(target: &str) -> Option<&str> {
    let id = target
        .strip_prefix("/containers/")?
        .strip_suffix("/restart?t=10")?;
    is_id(id).then_some(id)
}

/// "/smart/sda" → "sda" (an sd disk name only, never a path).
pub fn smart_dev(target: &str) -> Option<&str> {
    target
        .strip_prefix("/smart/")
        .filter(|d| crate::watch::is_disk_name(d))
}

const SMARTCTL: &str = "/usr/sbin/smartctl";
const SMART_TIMEOUT: Duration = Duration::from_secs(25);

/// smartctl's arguments for one disk: JSON, health, attributes, self-test log; never wake it.
/// A disk behind a USB bridge needs `-d sat` (smartctl 7.4 doesn't know every bridge).
pub fn smart_args(dev: &str, usb: bool) -> Vec<String> {
    let mut a: Vec<String> = ["-j", "-n", "standby", "-H", "-A", "-c", "-l", "selftest"]
        .map(String::from)
        .to_vec();
    if usb {
        a.extend(["-d".to_string(), "sat".to_string()]);
    }
    a.push(format!("/dev/{dev}"));
    a
}

/// Runs smartctl (killed after 25 s): {"exit": N, "out": its JSON}.
fn smart(dev: &str) -> (u16, String) {
    let usb = std::fs::canonicalize(format!("/sys/block/{dev}"))
        .is_ok_and(|p| p.to_string_lossy().contains("/usb"));
    let child = std::process::Command::new(SMARTCTL)
        .args(smart_args(dev, usb))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            return (
                503,
                serde_json::json!({ "error": format!("smartctl: {e}") }).to_string(),
            );
        }
    };
    // read while it runs, so a big report can't fill the pipe and stall it
    let reader = child.stdout.take().map(|so| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut out = String::new();
            let _ = so.take(1 << 20).read_to_string(&mut out);
            out
        })
    });
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() < SMART_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(100))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let Some(status) = status else {
        return (504, r#"{"error":"smartctl took too long"}"#.into());
    };
    let out = reader.and_then(|r| r.join().ok()).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or(serde_json::Value::Null);
    (
        200,
        serde_json::json!({ "exit": status.code().unwrap_or(-1), "out": v }).to_string(),
    )
}

/// What to answer for one request line: (status, JSON body). Pure, so it's tested directly.
pub fn decide(method: &str, target: &str) -> Option<(u16, String)> {
    let ok = match method {
        "GET" => ALLOWLIST.contains(&target) || smart_dev(target).is_some(),
        "POST" => restart_id(target).is_some() || crate::fans::parse_helper(target).is_some(),
        _ => return Some((405, r#"{"error":"only GET, or POST to restart"}"#.into())),
    };
    if !ok {
        return Some((403, r#"{"error":"not allowed"}"#.into()));
    }
    None // allowed: forward to Docker
}

fn restart(mut stream: UnixStream, docker: PathBuf, id: String) {
    let r = http::post(&docker, &restart_path(&id), RESTART_TIMEOUT);
    let short = &id[..12];
    let sent = match r {
        Ok((status, body)) => {
            eprintln!("pios helper: restart {short}: HTTP {status}");
            http::respond(
                &mut stream,
                status,
                if body.is_empty() { "{}" } else { &body },
            )
        }
        Err(e) => {
            eprintln!("pios helper: restart {short}: {e}");
            http::respond(
                &mut stream,
                502,
                &serde_json::json!({ "error": e.to_string() }).to_string(),
            )
        }
    };
    if let Err(e) = sent
        && !http::is_client_gone(&e)
    {
        eprintln!("pios helper: {e}");
    }
    RESTARTS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
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
    if let Some(dev) = smart_dev(&target) {
        let (status, body) = smart(dev);
        return http::respond(stream, status, &body);
    }
    if let Some(c) = crate::fans::parse_helper(&target) {
        return match crate::fans::write(&crate::fans::dir(), &c) {
            Ok(()) => http::respond(stream, 200, r#"{"status":"ok"}"#),
            Err(e) => {
                eprintln!("pios helper: fan override: {e}");
                http::respond(
                    stream,
                    503,
                    &serde_json::json!({ "error": e.to_string() }).to_string(),
                )
            }
        };
    }
    if let Some(id) = restart_id(&target) {
        // on its own thread, so the 5 s container list keeps answering meanwhile
        if RESTARTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= MAX_RESTARTS {
            RESTARTS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            return http::respond(stream, 429, r#"{"error":"another restart is running"}"#);
        }
        let (s, d, id) = (stream.try_clone()?, docker.to_path_buf(), id.to_string());
        std::thread::spawn(move || restart(s, d, id));
        return Ok(());
    }
    // forward the matched constant, never the client's string
    let Some(path) = ALLOWLIST.iter().find(|p| **p == target) else {
        return http::respond(stream, 403, r#"{"error":"not allowed"}"#);
    };
    let timeout = if *path == DOCKER_DF {
        Duration::from_secs(10)
    } else {
        DOCKER_TIMEOUT
    };
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
        assert_eq!(decide("POST", ALLOWED).map(|d| d.0), Some(403));
        assert_eq!(decide("DELETE", "/containers/x").map(|d| d.0), Some(405));
        assert_eq!(decide("PUT", ALLOWED).map(|d| d.0), Some(405));
    }

    #[test]
    fn smart_only_for_sd_disks() {
        assert_eq!(decide("GET", "/smart/sda"), None);
        assert_eq!(smart_dev("/smart/sdab"), Some("sdab"));
        for bad in [
            "/smart/sda1",
            "/smart/nvme0n1",
            "/smart/../sda",
            "/smart/sda/x",
            "/smart/sda?x=1",
            "/smart/",
            "/smart/sdA",
        ] {
            assert_eq!(decide("GET", bad).map(|d| d.0), Some(403), "{bad}");
        }
        assert_eq!(decide("POST", "/smart/sda").map(|d| d.0), Some(403));
        assert_eq!(
            smart_args("sda", true).join(" "),
            "-j -n standby -H -A -c -l selftest -d sat /dev/sda"
        );
        assert!(!smart_args("sdb", false).contains(&"sat".to_string()));
    }

    #[test]
    fn fan_overrides_pass_only_in_the_helper_form() {
        assert_eq!(decide("POST", "/fan_ctl/case?duty=75&until=0"), None);
        assert_eq!(decide("POST", "/fan_ctl/cooler/auto"), None);
        for bad in [
            "/fans/case?duty=75&for=3600",
            "/fan_ctl/case?duty=75",
            "/fan_ctl/case?duty=750&until=0",
            "/fan_ctl/override",
        ] {
            assert_eq!(decide("POST", bad).map(|d| d.0), Some(403), "{bad}");
        }
        assert_eq!(
            decide("GET", "/fan_ctl/cooler/auto").map(|d| d.0),
            Some(403)
        );
    }

    #[test]
    fn only_restart_of_one_full_id_passes() {
        let id = "a".repeat(64);
        let ok = restart_path(&id);
        assert_eq!(decide("POST", &ok), None);
        assert_eq!(restart_id(&ok), Some(id.as_str()));
        assert_eq!(
            decide("GET", &ok).map(|d| d.0),
            Some(403),
            "restart is POST only"
        );
        for bad in [
            format!("/containers/{}/restart?t=10", "a".repeat(63)),
            format!("/containers/{}/restart?t=10", "A".repeat(64)),
            format!("/containers/{}/restart?t=10", "g".repeat(64)),
            format!("/containers/{id}/restart"),
            format!("/containers/{id}/restart?t=0"),
            format!("/containers/{id}/restart?t=10&signal=KILL"),
            format!("/containers/{id}/stop?t=10"),
            format!("/containers/{id}/kill"),
            format!("/containers/{id}/../{id}/restart?t=10"),
            "/containers/redis/restart?t=10".into(),
            "/containers//restart?t=10".into(),
            format!("/v1.45/containers/{id}/restart?t=10"),
        ] {
            assert_eq!(decide("POST", &bad).map(|d| d.0), Some(403), "{bad}");
        }
    }
}
