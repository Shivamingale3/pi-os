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
//!   pios deploy-ssh                  forced command of the Jenkins deploy key
//!   pios version
//!
//! Environment (all optional):
//!   PIOS_DATA_DIR       where pios.db lives        (default ./data)
//!   PIOS_SOCKET         API socket                  (default $PIOS_DATA_DIR/pios.sock)
//!   PIOS_DOCKER_SOCKET  Docker socket               (default /var/run/docker.sock)

mod api;
mod collect;
mod details;
mod error;
mod helper;
mod http;
mod parse;
mod rules;
mod services;
mod setup;
mod signals;
mod store;
mod types;
mod ui;
mod update;

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
}

impl Incidents {
    fn apply(&mut self, store: &Store, action: Action) -> Result<()> {
        match action {
            Action::Open(o) => {
                let id = store.insert_incident(&o)?;
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
            }
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
    let mut engine = Engine::new();
    let mut incidents = Incidents {
        open: HashMap::new(),
        peaks: HashMap::new(),
    };
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

        let snap = match collector.collect() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("pios: collect: {e}");
                continue;
            }
        };
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
        if last_prune.elapsed() >= PRUNE_INCIDENTS_EVERY {
            last_prune = Instant::now();
            log_err("incident prune", store.prune_incidents(snap.ts));
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
                "usage: pios <agent|snapshot|ui|version|setup|update|uninstall|helper|deploy-ssh>"
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
