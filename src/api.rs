//! Local API: HTTP/1.1 GET on a unix socket, one thread, one request per connection.
//! Hand-written: 4 routes don't need an HTTP crate (no extra threads, no async runtime).
//!
//!   GET /now                  latest snapshot (503 until the first one)
//!   GET /trend?metric=NAME    last 24 h: {"metric", "points": [[ts, value], …]}
//!   GET /incidents            last 7 days, newest first, without snapshots
//!   GET /incidents/ID         one incident with its start / peak / end snapshots
//!
//! Try: curl --unix-socket data/pios.sock http://pios/now

use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::error::{Result, err};
use crate::store::{self, INCIDENT_KEEP_MS, TREND_METRICS, TREND_WINDOW_MS};
use crate::types::{Snapshot, now_ms};

pub type Live = Arc<RwLock<Option<Snapshot>>>;

const MAX_REQUEST: usize = 8 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Binds the socket. Refuses if another agent already answers on it; removes a stale one.
pub fn bind(path: &Path) -> Result<UnixListener> {
    if path.as_os_str().len() >= 108 {
        return Err(err(format!(
            "socket path is too long for a unix socket (max 107 bytes): {} (set PIOS_SOCKET)",
            path.display()
        )));
    }
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(err(format!(
                "{} is in use: is another pios agent running?",
                path.display()
            )));
        }
        let meta = std::fs::symlink_metadata(path)?;
        if !std::os::unix::fs::FileTypeExt::is_socket(&meta.file_type()) {
            return Err(err(format!(
                "{} exists and is not a socket",
                path.display()
            )));
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    // owner + group only (the UI will be in the group on the Pi)
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    Ok(listener)
}

pub fn serve(listener: UnixListener, live: Live, db_path: PathBuf) {
    let mut db: Option<Connection> = None;
    for conn in listener.incoming() {
        let Ok(mut stream) = conn else { continue };
        if db.is_none() {
            db = store::open_reader(&db_path)
                .map_err(|e| eprintln!("pios: api: {e}"))
                .ok();
        }
        if let Err(e) = handle(&mut stream, &live, db.as_ref()) {
            // a client that hangs up or times out is not worth a log line
            if !matches!(
                e.kind(),
                ErrorKind::BrokenPipe
                    | ErrorKind::WouldBlock
                    | ErrorKind::TimedOut
                    | ErrorKind::ConnectionReset
            ) {
                eprintln!("pios: api: {e}");
            }
        }
    }
}

fn handle(stream: &mut UnixStream, live: &Live, db: Option<&Connection>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        if buf.len() > MAX_REQUEST {
            return respond(stream, 431, &json!({"error": "request too large"}));
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    }

    let head = String::from_utf8_lossy(&buf);
    let mut parts = head.lines().next().unwrap_or("").split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let (status, body) = if method == "GET" {
        route(target, live, db)
    } else {
        (405, json!({"error": "only GET"}))
    };
    respond(stream, status, &body)
}

fn respond(stream: &mut UnixStream, status: u16, body: &Value) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let body = body.to_string();
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())
}

fn query_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| {
        kv.split_once('=')
            .filter(|(k, _)| *k == name)
            .map(|(_, v)| v)
    })
}

pub fn route(target: &str, live: &Live, db: Option<&Connection>) -> (u16, Value) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let path = path.trim_end_matches('/');

    if path == "/now" {
        return match live.read().ok().and_then(|g| g.clone()) {
            Some(s) => (200, serde_json::to_value(s).unwrap_or(Value::Null)),
            None => (503, json!({"error": "no snapshot yet"})),
        };
    }

    let Some(db) = db else {
        return (503, json!({"error": "database not available"}));
    };
    let result = match path {
        "/trend" => {
            let metric = query_param(query, "metric").unwrap_or("");
            if !store::is_trend_metric(metric) {
                let known: Vec<&str> = TREND_METRICS.iter().map(|(m, _)| *m).collect();
                return (
                    400,
                    json!({"error": "unknown or missing ?metric=", "metrics": known}),
                );
            }
            store::trend(db, metric, now_ms() - TREND_WINDOW_MS).map(|pts| {
                (
                    200,
                    json!({"metric": metric, "windowMs": TREND_WINDOW_MS, "points": pts}),
                )
            })
        }
        "/incidents" => store::list_incidents(db, now_ms() - INCIDENT_KEEP_MS)
            .map(|list| (200, json!({"incidents": list}))),
        _ => match path.strip_prefix("/incidents/").map(str::parse::<i64>) {
            Some(Ok(id)) => store::get_incident(db, id).map(|found| match found {
                Some(v) => (200, v),
                None => (404, json!({"error": "no such incident"})),
            }),
            _ => {
                return (
                    404,
                    json!({"error": "not found", "routes": ["/now", "/trend?metric=", "/incidents", "/incidents/ID"]}),
                );
            }
        },
    };
    result.unwrap_or_else(|e| (500, json!({"error": e.to_string()})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::tests::snap;
    use crate::store::Store;

    fn live(s: Option<Snapshot>) -> Live {
        Arc::new(RwLock::new(s))
    }

    #[test]
    fn now() {
        assert_eq!(route("/now", &live(None), None).0, 503);
        let (code, v) = route("/now", &live(Some(snap(5, 12.0))), None);
        assert_eq!(code, 200);
        assert_eq!(v["cpu"]["percent"], 12.0);
    }

    #[test]
    fn trend_and_errors() {
        let mut st = Store::memory().unwrap();
        st.write_trend(&snap(now_ms(), 3.0)).unwrap();
        let l = live(None);
        let (code, v) = route("/trend?metric=cpu.percent", &l, Some(st.conn()));
        assert_eq!(code, 200);
        assert_eq!(v["points"][0][1], 3.0);
        assert_eq!(route("/trend", &l, Some(st.conn())).0, 400);
        assert_eq!(route("/trend?metric=x", &l, Some(st.conn())).0, 400);
        assert_eq!(route("/nope", &l, Some(st.conn())).0, 404);
        assert_eq!(route("/incidents/abc", &l, Some(st.conn())).0, 404);
        assert_eq!(route("/incidents/7", &l, Some(st.conn())).0, 404);
        assert_eq!(
            route("/incidents", &l, Some(st.conn())).1["incidents"],
            json!([])
        );
    }
}
