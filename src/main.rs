//! PiOS: monitoring for the Pebble Raspberry Pi 5.
//!
//!   pios agent      run forever: collect every 5 s, 24 h trend + incidents in SQLite, local API
//!   pios snapshot   collect one snapshot and print it as JSON
//!   pios ui ...     draw the screens (see `src/ui/mod.rs`; for now a PNG or a laptop preview)
//!
//! On the Pi (root):
//!   pios setup [--deploy-key FILE]   install/refresh users, systemd units, sudoers line, deploy key
//!   pios update vX.Y.Z | --rollback  install a GitHub release (checksum, health check, auto-rollback)
//!   pios uninstall [--keep-data]     remove everything `setup` created
//!   pios helper                      filtered Docker access (started by pios-helper.socket)
//!   pios web                         public hostname checks every 60 s (pios-web.service)
//!   pios deploy-ssh                  forced command of the Jenkins deploy key
//!   pios version
//!
//! Environment (all optional):
//!   PIOS_DATA_DIR       where pios.db lives        (default ./data)
//!   PIOS_SOCKET         API socket                  (default $PIOS_DATA_DIR/pios.sock)
//!   PIOS_DOCKER_SOCKET  Docker socket               (default /var/run/docker.sock)
//!   PIOS_CHECKS         host services + hostnames   (default /etc/pios/checks, see checks.rs)
//!   PIOS_WEB_OUT        `pios web` results file      (default /run/pios-web/web.json)

mod alerts;
mod api;
mod checks;
mod collect;
mod details;
mod error;
mod events;
mod fans;
mod helper;
mod http;
mod parse;
mod rules;
mod services;
mod setup;
mod signals;
mod store;
mod telegram;
mod types;
mod ui;
mod update;
mod watch;
mod web;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use error::Result;
use rules::{Action, Engine};
use store::Store;
use types::{Snapshot, now_ms};

const INTERVAL: Duration = Duration::from_secs(5);
const TREND_EVERY: Duration = Duration::from_secs(60);
const PRUNE_INCIDENTS_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const EVENTS_EVERY: Duration = Duration::from_secs(60);
const BOOTS_EVERY: Duration = Duration::from_secs(60 * 60);

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn docker_socket() -> PathBuf {
    env_path("PIOS_DOCKER_SOCKET").unwrap_or_else(|| PathBuf::from("/var/run/docker.sock"))
}

fn fmt_value(v: Option<f64>) -> String {
    v.map(|v| format!(" (peak {v:.1})")).unwrap_or_default()
}

/// Turns rule actions into DB writes. Peaks are kept in memory and written once a minute
/// (or when the incident closes), so a climbing value doesn't write every 5 s.
struct Incidents {
    open: HashMap<String, i64>,
    peaks: HashMap<i64, (i64, f64, Box<Snapshot>)>,
    /// problems already alerted (key → title, severity, started), kept across agent restarts so a
    /// deploy doesn't re-send "down" for a problem that's still the same one
    alerted: HashMap<String, (String, String, i64)>,
}

const ALERTED: &str = "alerted";

impl Incidents {
    fn apply(&mut self, store: &Store, action: Action) -> Result<()> {
        match action {
            Action::Open(o) => {
                let id = store.insert_incident(&o)?;
                if !self.alerted.contains_key(&o.key) {
                    let sev = o.severity.as_str();
                    let value = if o.kind == rules::Kind::Metric {
                        o.peak_value
                    } else {
                        None
                    };
                    store.insert_alert(
                        o.started_at,
                        alerts::PROBLEM,
                        sev,
                        &alerts::problem_text(&o.title, sev, value),
                    )?;
                    self.alerted.insert(
                        o.key.clone(),
                        (o.title.clone(), sev.to_string(), o.started_at),
                    );
                    self.save_alerted(store);
                }
                eprintln!(
                    "pios: incident #{id} opened: {} [{}]{}",
                    o.title,
                    o.severity.as_str(),
                    fmt_value(o.peak_value)
                );
                self.open.insert(o.key, id);
            }
            Action::Escalate { key, severity } => {
                if let Some(&id) = self.open.get(&key) {
                    store.update_severity(id, severity)?;
                    eprintln!("pios: incident #{id} is now {}", severity.as_str());
                    if severity.as_str() == "critical"
                        && let Some(a) = self.alerted.get_mut(&key)
                        && a.1 != "critical"
                    {
                        a.1 = "critical".into();
                        store.insert_alert(
                            now_ms(),
                            alerts::PROBLEM,
                            "critical",
                            &alerts::escalated_text(&a.0),
                        )?;
                        self.save_alerted(store);
                    }
                }
            }
            Action::Peak {
                key,
                value,
                at,
                snapshot,
            } => {
                if let Some(&id) = self.open.get(&key) {
                    self.peaks.insert(id, (at, value, snapshot));
                }
            }
            Action::Close {
                key,
                ended_at,
                snapshot,
                reason,
            } => {
                if let Some(id) = self.open.remove(&key) {
                    self.flush_peak(store, id)?;
                    store.close_incident(id, ended_at, reason, &snapshot)?;
                    eprintln!("pios: incident #{id} closed: {reason}");
                }
                // the agent stopping isn't a recovery: keep it alerted, so it isn't sent twice
                if reason != "agent stopped" {
                    self.recovered(store, &key, ended_at, reason == "recovered")?;
                }
            }
        }
        Ok(())
    }

    fn save_alerted(&self, store: &Store) {
        if let Ok(j) = serde_json::to_string(&self.alerted) {
            log_err("alerts", store.set_kv(ALERTED, &j));
        }
    }

    /// The problem is over: "✅ Recovered" (unless it was only removed, or no longer checked).
    fn recovered(&mut self, store: &Store, key: &str, at: i64, notify: bool) -> Result<()> {
        if let Some((title, sev, started)) = self.alerted.remove(key) {
            if notify {
                store.insert_alert(
                    at,
                    alerts::RECOVERED,
                    &sev,
                    &alerts::recovered_text(&title, at - started),
                )?;
            }
            self.save_alerted(store);
        }
        Ok(())
    }

    /// A while after start: problems alerted before a restart that didn't come back are over.
    fn settle(&mut self, store: &Store, now: i64) -> Result<()> {
        let still: Vec<String> = self.open.keys().cloned().collect();
        let gone: Vec<String> = self
            .alerted
            .keys()
            .filter(|k| !still.contains(k))
            .cloned()
            .collect();
        for k in gone {
            self.recovered(store, &k, now, true)?;
        }
        Ok(())
    }

    fn flush_peak(&mut self, store: &Store, id: i64) -> Result<()> {
        if let Some((at, value, snap)) = self.peaks.remove(&id) {
            store.update_peak(id, at, value, &snap)?;
        }
        Ok(())
    }

    fn flush_peaks(&mut self, store: &Store) -> Result<()> {
        let ids: Vec<i64> = self.peaks.keys().copied().collect();
        for id in ids {
            self.flush_peak(store, id)?;
        }
        Ok(())
    }
}

fn log_err<T>(what: &str, r: Result<T>) {
    if let Err(e) = r {
        eprintln!("pios: {what}: {e}");
    }
}

fn data_dir() -> PathBuf {
    env_path("PIOS_DATA_DIR").unwrap_or_else(|| PathBuf::from("data"))
}

fn api_socket() -> PathBuf {
    env_path("PIOS_SOCKET").unwrap_or_else(|| data_dir().join("pios.sock"))
}

/// New SSH / sudo lines since the saved cursor (and, when due, the boot list) into the store.
/// `clean`: which past boots ended with a clean shutdown (asked once each; they never change).
fn poll_events(
    store: &Store,
    boots: bool,
    clean: &mut HashMap<String, Option<bool>>,
) -> Result<()> {
    const CURSOR: &str = "journal_cursor";
    let cursor = store.get_kv(CURSOR);
    let (list, next) = events::read_journal(cursor.as_deref())?;
    let mut new = store.insert_events(&list)?;
    if let Some(c) = next {
        store.set_kv(CURSOR, &c)?;
    }
    if boots {
        let mut list = events::read_boots()?;
        // the newest boot is still running: only the ones before it have ended
        for (id, _) in list.iter().rev().skip(1) {
            if !clean.contains_key(id) {
                clean.insert(id.clone(), events::ended_cleanly(id));
            }
        }
        events::mark_power_lost(&mut list, &|id| clean.get(id).copied().flatten());
        for (_, e) in list.iter().filter(|(_, e)| e.kind == "boot") {
            store.unmark_power_lost(e.ts)?;
        }
        let list: Vec<events::Event> = list.into_iter().map(|(_, e)| e).collect();
        new.extend(store.insert_events(&list)?);
    }
    // phone alerts for what just happened (not for history read back on a first run)
    let recent = now_ms() - 10 * 60_000;
    for e in new.iter().filter(|e| e.ts >= recent) {
        if let Some((kind, text)) = alerts::from_event(e) {
            store.insert_alert(e.ts, kind, "", &text)?;
        }
    }
    Ok(())
}

fn agent() -> Result<()> {
    let data_dir = data_dir();
    std::fs::create_dir_all(&data_dir)?;
    let db_path = data_dir.join("pios.db");
    let socket_path = api_socket();

    signals::install();

    let mut store = Store::open(&db_path)?;
    let now = now_ms();
    let dangling = store.close_dangling(now)?;
    if dangling > 0 {
        eprintln!("pios: closed {dangling} incident(s) left open by the last run");
    }
    store.prune_trend(now)?;
    store.prune_incidents(now)?;

    let live: api::Live = Arc::new(RwLock::new(None));
    let listener = api::bind(&socket_path)?;
    {
        let (live, db_path) = (live.clone(), db_path.clone());
        std::thread::Builder::new()
            .name("api".into())
            .spawn(move || api::serve(listener, live, db_path))?;
    }

    let mut collector = collect::Collector::new(docker_socket())?;
    let mut watch = watch::Watch::new();
    let mut engine = Engine::new();
    let mut incidents = Incidents {
        open: HashMap::new(),
        peaks: HashMap::new(),
        alerted: store
            .get_kv(ALERTED)
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
    };
    let started = Instant::now();
    let mut settled = false;
    eprintln!(
        "pios: agent {} running · db {} · api {}",
        env!("CARGO_PKG_VERSION"),
        db_path.display(),
        socket_path.display()
    );

    // first snapshot 1 s after the baseline, then every 5 s
    let mut next = Instant::now() + Duration::from_secs(1);
    let mut last_trend: Option<Instant> = None;
    let mut last_prune = Instant::now();
    let mut last_events: Option<Instant> = None;
    let mut last_boots = Instant::now();
    let mut events_err: Option<String> = None;
    let mut clean_boots: HashMap<String, Option<bool>> = HashMap::new();
    let mut last_snapshot: Option<Snapshot> = None;

    while !signals::stopping() {
        signals::sleep_until(next);
        if signals::stopping() {
            break;
        }
        next += INTERVAL;
        if next < Instant::now() {
            next = Instant::now() + INTERVAL; // fell behind (suspend, stall): don't burst
        }

        let mut snap = match collector.collect() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("pios: collect: {e}");
                continue;
            }
        };
        watch.tick(&mut snap, collector.mounts(), &store, &docker_socket());
        if let Ok(mut g) = live.write() {
            *g = Some(snap.clone());
        }
        for action in engine.step(&snap) {
            log_err("incident", incidents.apply(&store, action));
        }

        if last_trend.is_none_or(|t| t.elapsed() >= TREND_EVERY) {
            last_trend = Some(Instant::now());
            log_err("trend", store.write_trend(&snap));
            log_err("trend prune", store.prune_trend(snap.ts));
            log_err("peaks", incidents.flush_peaks(&store));
        }
        // 90 s after start (incidents reopen after 30 s bad): alerted problems that didn't
        // come back recovered while the agent was down
        if !settled && started.elapsed() >= Duration::from_secs(90) {
            settled = true;
            log_err("alerts", incidents.settle(&store, snap.ts));
        }
        if last_events.is_none_or(|t| t.elapsed() >= EVENTS_EVERY) {
            let boots_due = last_events.is_none() || last_boots.elapsed() >= BOOTS_EVERY;
            last_events = Some(Instant::now());
            if boots_due {
                last_boots = Instant::now();
            }
            if let Err(e) = poll_events(&store, boots_due, &mut clean_boots) {
                // once per distinct error, not every minute
                let msg = e.to_string();
                if events_err.as_deref() != Some(msg.as_str()) {
                    eprintln!("pios: events: {msg}");
                    events_err = Some(msg);
                }
            } else {
                events_err = None;
            }
        }
        if last_prune.elapsed() >= PRUNE_INCIDENTS_EVERY {
            last_prune = Instant::now();
            log_err("incident prune", store.prune_incidents(snap.ts));
            log_err("event prune", store.prune_events(snap.ts));
            log_err("alert prune", store.prune_alerts(snap.ts).map(|_| ()));
        }
        last_snapshot = Some(snap);
    }

    eprintln!("pios: stopping");
    if let Some(s) = &last_snapshot {
        for action in engine.close_all(s, "agent stopped") {
            log_err("incident", incidents.apply(&store, action));
        }
    }
    log_err("peaks", incidents.flush_peaks(&store));
    log_err("checkpoint", store.checkpoint());
    log_err(
        "socket",
        std::fs::remove_file(&socket_path).map_err(Into::into),
    );
    Ok(())
}

fn snapshot() -> Result<()> {
    let mut c = collect::Collector::new(docker_socket())?;
    std::thread::sleep(Duration::from_secs(1));
    let s = c.collect()?;
    println!("{}", serde_json::to_string_pretty(&s)?);
    Ok(())
}

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1);
    let rest: Vec<String> = std::env::args().skip(2).collect();
    let result = match cmd.as_deref() {
        Some("agent") => agent(),
        Some("snapshot") => snapshot(),
        Some("ui") => ui::run(&rest, &api_socket()),
        Some("helper") => helper::run(docker_socket()),
        Some("web") => web::run(),
        Some("setup") => match rest.as_slice() {
            [] => setup::setup(None),
            [flag, file] if flag == "--deploy-key" => {
                setup::setup(Some(std::path::Path::new(file)))
            }
            _ => Err(error::err("usage: pios setup [--deploy-key FILE]")),
        },
        Some("uninstall") => match rest.as_slice() {
            [] => setup::uninstall(false),
            [flag] if flag == "--keep-data" => setup::uninstall(true),
            _ => Err(error::err("usage: pios uninstall [--keep-data]")),
        },
        Some("update") => match rest.as_slice() {
            [arg] => update::update(arg),
            _ => Err(error::err("usage: pios update vX.Y.Z | --rollback")),
        },
        Some("deploy-ssh") => update::deploy_ssh(),
        Some("version" | "--version") => {
            println!("pios {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!(
                "usage: pios <agent|snapshot|ui|version|setup|update|uninstall|helper|web|deploy-ssh>"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pios: {e}");
            ExitCode::FAILURE
        }
    }
}
