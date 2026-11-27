//! Local API: HTTP/1.1 on a unix socket, one thread, one request per connection.
//! Hand-written: 4 routes don't need an HTTP crate (no extra threads, no async runtime).
//!
//!   GET /now                  latest snapshot (503 until the first one)
//!   GET /trend?metric=NAME    last 24 h: {"metric", "points": [[ts, value], …]}
//!   GET /incidents            last 7 days, newest first, without snapshots
//!   GET /incidents/ID         one incident with its start / peak / end snapshots
//!   GET /events               last 7 days of SSH logins, sudo, deploys, boots (newest first)
//!   GET /alerts?after=ID      phone alerts to send (pios web); GET /alerts/status: is it working
//!   POST /alerts/prefs?… · POST /alerts/test   the screen's Alerts settings and test button
//!   GET /details/TILE         cpu · temps · memory · disk · services, collected on request (screen open)
//!   POST /containers/ID/restart   the screen's restart (202 at once; progress in /details/services)
//!   POST /fans/case?duty=N&for=S · /fans/cooler?level=N&for=S · /fans/{case,cooler}/auto
//!                             manual fan control through fan_ctl (fans.rs)
//!
//! Try: curl --unix-socket data/pios.sock http://pios/now

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::error::{Result, err};
use crate::http;
use crate::store::{self, INCIDENT_KEEP_MS, TREND_METRICS, TREND_WINDOW_MS};
use crate::types::{Snapshot, now_ms};

pub type Live = Arc<RwLock<Option<Snapshot>>>;

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
        if let Err(e) = handle(&mut stream, &live, db.as_ref(), &db_path)
            && !http::is_client_gone(&e)
        {
            eprintln!("pios: api: {e}");
        }
    }
}

fn handle(
    stream: &mut UnixStream,
    live: &Live,
    db: Option<&Connection>,
    db_path: &Path,
) -> std::io::Result<()> {
    let Some(req) = http::read_request(stream)? else {
        return Ok(());
    };
    let (status, body) = match req {
        http::Request::TooLarge => (431, json!({"error": "request too large"})),
        http::Request::Line { method, target } if method == "GET" => route(&target, live, db),
        http::Request::Line { method, target } if method == "POST" => {
            if target.starts_with("/alerts/") {
                alert_action(&target, db_path)
            } else if target.starts_with("/fans/") {
                let (status, msg) = crate::fans::ask(&helper_socket(), &target);
                let key = if status < 300 { "status" } else { "error" };
                (status, json!({ key: msg }))
            } else {
                action(&target, live)
            }
        }
        http::Request::Line { .. } => (405, json!({"error": "only GET or POST"})),
    };
    http::respond(stream, status, &body.to_string())
}

/// POST /alerts/prefs?… (the screen's Alerts settings) · POST /alerts/test (its test button).
/// Rare writes: a short-lived writer connection (the API thread otherwise only reads).
pub fn alert_action(target: &str, db_path: &Path) -> (u16, Value) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let store = match store::Store::open(db_path) {
        Ok(s) => s,
        Err(e) => return (503, json!({"error": e.to_string()})),
    };
    let r = match path {
        "/alerts/prefs" => {
            let p = crate::alerts::Prefs::from_query(query);
            store
                .set_kv("alert_prefs", &p.to_query())
                .map(|_| json!({"status": "saved"}))
        }
        "/alerts/test" => store
            .insert_alert(
                now_ms(),
                crate::alerts::TEST,
                "",
                "🔔 Test from PiOS: alerts work",
            )
            .map(|id| json!({"status": "queued", "id": id})),
        _ => return (404, json!({"error": "POST /alerts/prefs or /alerts/test"})),
    };
    r.map(|v| (200, v))
        .unwrap_or_else(|e| (500, json!({"error": e.to_string()})))
}

/// The one action: POST /containers/ID/restart (from the screen's 2 s hold).
pub fn action(target: &str, live: &Live) -> (u16, Value) {
    let Some(id) = target
        .strip_prefix("/containers/")
        .and_then(|t| t.strip_suffix("/restart"))
    else {
        return (404, json!({"error": "POST /containers/ID/restart only"}));
    };
    let known: std::collections::HashMap<String, String> = live
        .read()
        .ok()
        .and_then(|g| g.as_ref()?.docker.clone())
        .map(|d| d.containers.into_iter().map(|c| (c.id, c.name)).collect())
        .unwrap_or_default();
    let (status, msg) = crate::services::ask_restart(&helper_socket(), id, &known);
    let key = if status < 300 { "status" } else { "error" };
    (status, json!({ key: msg }))
}

/// The helper's socket (on the Pi; Docker's own socket on a laptop).
fn helper_socket() -> PathBuf {
    std::env::var_os("PIOS_DOCKER_SOCKET")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/run/docker.sock"))
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

    if let Some(tile) = path.strip_prefix("/details/") {
        let snap = live.read().ok().and_then(|g| g.clone());
        let docker = std::env::var_os("PIOS_DOCKER_SOCKET")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/run/docker.sock"));
        let r = match tile {
            "cpu" => crate::details::cpu().map(|d| serde_json::to_value(d).unwrap_or(Value::Null)),
            "temps" => {
                crate::details::temps().map(|d| serde_json::to_value(d).unwrap_or(Value::Null))
            }
            "memory" => crate::details::memory(snap.as_ref())
                .map(|d| serde_json::to_value(d).unwrap_or(Value::Null)),
            "disk" => crate::details::disk(&docker)
                .map(|d| serde_json::to_value(d).unwrap_or(Value::Null)),
            "services" => crate::services::collect(&docker)
                .map(|d| serde_json::to_value(d).unwrap_or(Value::Null)),
            _ => {
                return (
                    404,
                    json!({"error": "details: cpu, temps, memory, disk or services"}),
                );
            }
        };
        return r
            .map(|v| (200, v))
            .unwrap_or_else(|e| (500, json!({"error": e.to_string()})));
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
        "/events" => store::list_events(db, now_ms() - INCIDENT_KEEP_MS)
            .map(|list| (200, json!({"events": list}))),
        // pios web: what to send after its cursor (the settings already applied), and the
        // newest id it has seen (filtered-out alerts move the cursor too)
        "/alerts" => {
            let after = query_param(query, "after")
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(-1);
            let prefs = store::kv(db, "alert_prefs")
                .map(|q| crate::alerts::Prefs::from_query(&q))
                .unwrap_or_default();
            store::alerts_after(db, after.max(0)).map(|list| {
                let last = list.last().map_or(after.max(0), |a| a.id);
                // the first ask (after=-1) only learns where "now" is: no flood of old alerts
                let send: Vec<_> = if after < 0 {
                    vec![]
                } else {
                    list.into_iter()
                        .filter(|a| prefs.wants(a, crate::alerts::local_minutes(a.ts)))
                        .collect()
                };
                (200, json!({"alerts": send, "last": last}))
            })
        }
        // the screen: is sending set up and working? (pios web's status file)
        "/alerts/status" => {
            let v = std::fs::read_to_string(crate::alerts::STATUS_PATH)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .unwrap_or(json!({"configured": false}));
            Ok((200, v))
        }
        _ => match path.strip_prefix("/incidents/").map(str::parse::<i64>) {
            Some(Ok(id)) => store::get_incident(db, id).map(|found| match found {
                Some(v) => (200, v),
                None => (404, json!({"error": "no such incident"})),
            }),
            _ => {
                return (
                    404,
                    json!({"error": "not found", "routes": ["/now", "/trend?metric=", "/incidents", "/incidents/ID", "/events"]}),
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

    #[test]
    fn alerts_outbox() {
        use crate::alerts::{BOOT, PROBLEM, SSH, TEST};
        let st = Store::memory().unwrap();
        let l = live(None);
        let noon = {
            // a timestamp at local noon today, so quiet hours don't apply
            let t = now_ms();
            t - (crate::alerts::local_minutes(t) as i64 - 12 * 60) * 60_000
        };
        st.insert_alert(noon, PROBLEM, "warn", "🟠 CPU over 80%")
            .unwrap();
        st.insert_alert(noon, SSH, "", "🔑 SSH: admin").unwrap();
        st.insert_alert(noon, BOOT, "", "🔄 booted").unwrap();
        // the first ask only learns where "now" is
        let (code, v) = route("/alerts?after=-1", &l, Some(st.conn()));
        assert_eq!(code, 200);
        assert_eq!(v["alerts"], json!([]));
        assert_eq!(v["last"], 3);
        // then: the default settings let problems and boots through, not SSH logins
        let v = route("/alerts?after=0", &l, Some(st.conn())).1;
        let kinds: Vec<&str> = v["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["problem", "boot"]);
        assert_eq!(v["last"], 3, "the cursor moves past filtered ones too");
        // settings from the screen: SSH on, problems off
        st.set_kv("alert_prefs", "problems=0&ssh=1&deploys=1&quiet=0")
            .unwrap();
        let v = route("/alerts?after=0", &l, Some(st.conn())).1;
        let kinds: Vec<&str> = v["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["ssh", "boot"]);
        st.insert_alert(noon, TEST, "", "🔔 test").unwrap();
        assert_eq!(
            route("/alerts?after=3", &l, Some(st.conn())).1["alerts"][0]["kind"],
            "test"
        );
        assert_eq!(
            route("/alerts/status", &l, Some(st.conn())).1["configured"],
            json!(false)
        );
    }
}
