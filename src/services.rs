//! The Services screen's data, collected only on request (`GET /details/services`, while the
//! screen is open): Docker's container list (the same allow-listed GET the agent already makes)
//! plus each container's CPU, memory, limit and uptime from its cgroup.
//!
//! The one action: `POST /containers/ID/restart` (the screen's 2 s hold). The agent answers at
//! once and asks the helper on a background thread; the list then reports how it went.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::Result;

const SAMPLE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Services {
    /// false: Docker (or the helper) didn't answer
    pub reachable: bool,
    pub services: Vec<Service>,
    /// host services from /etc/pios/checks (`host` lines)
    #[serde(default)]
    pub host: Vec<Service>,
    /// `pios web`'s latest results (None: no `web` lines, or no fresh results)
    #[serde(default)]
    pub web: Option<crate::types::Web>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    pub id: String,
    pub name: String,
    pub image: String,
    /// Docker's State: running, exited, restarting, paused, created, dead
    pub state: String,
    /// healthy, unhealthy or starting (only containers with a health check)
    pub health: Option<String>,
    /// last exit code (exited / restarting)
    pub exit_code: Option<i32>,
    /// how long ago it stopped, short: "12 m ago" (stopped containers)
    pub ago: Option<String>,
    /// Compose project and service, if started by Compose
    pub project: Option<String>,
    pub service: Option<String>,
    /// "127.0.0.1:8080 → 80", "8080 → 80" (all interfaces), "80 inside"
    pub ports: Vec<String>,
    /// % of one core (like `docker stats`), over 300 ms
    pub cpu: Option<f64>,
    pub mem: Option<u64>,
    pub mem_limit: Option<u64>,
    pub up_sec: Option<u64>,
    /// a restart asked for from the screen in the last minute
    #[serde(default)]
    pub restart: Option<Restart>,
    /// a host service (systemd), not a container: no Restart; `image` holds its main process
    #[serde(default)]
    pub host: bool,
    /// processes and threads in it (host services)
    #[serde(default)]
    pub tasks: Option<u64>,
    /// something more to say about it ("login ends in 162 d" for tailscaled)
    #[serde(default)]
    pub note: Option<Note>,
}

/// Tailscale's login on its host service row ("login ends in 162 d"), if it's watched.
pub fn note_tailscale(host: &mut [Service], t: &crate::types::Tailscale, now: i64) {
    if let Some(s) = host.iter_mut().find(|s| s.name == "tailscaled") {
        let (text, level) = crate::watch::tailscale_note(t, now);
        s.note = Some(Note { text, level });
    }
}

/// A note on a service; `level` 1 or 2 makes the service amber or red even while it runs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub text: String,
    /// 0 information, 1 attention, 2 down
    #[serde(default)]
    pub level: u8,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Restart {
    pub ago_sec: u64,
    pub done: bool,
    /// why it failed (Docker's message); None = it worked (or is still running)
    pub error: Option<String>,
}

/// One restart per container per minute; the screen shows the countdown.
pub const RESTART_EVERY: Duration = Duration::from_secs(60);

struct Asked {
    at: Instant,
    done: bool,
    error: Option<String>,
}

static ASKED: Mutex<Option<HashMap<String, Asked>>> = Mutex::new(None);

fn recent(id: &str) -> Option<Restart> {
    let g = ASKED.lock().ok()?;
    let a = g.as_ref()?.get(id)?;
    let ago = a.at.elapsed();
    // keep showing a failure until the next try; a success only for the minute
    (ago < RESTART_EVERY || a.error.is_some() || !a.done).then(|| Restart {
        ago_sec: ago.as_secs(),
        done: a.done,
        error: a.error.clone(),
    })
}

/// Docker's error body {"message": "..."} → the message.
fn docker_message(status: u16, body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("message")
                .or_else(|| v.get("error"))?
                .as_str()
                .map(String::from)
        })
        .unwrap_or_else(|| format!("HTTP {status}"))
}

/// The agent's `POST /containers/ID/restart`: checks, records, and returns at once
/// (202); the helper does the restart on a background thread. `known`: the ID → name of every
/// container in the latest snapshot (only those can be restarted).
pub fn ask_restart(helper: &Path, id: &str, known: &HashMap<String, String>) -> (u16, String) {
    if !crate::helper::is_id(id) {
        return (400, "not a container ID".into());
    }
    let Some(name) = known.get(id).cloned() else {
        return (404, "no such container".into());
    };
    {
        let Ok(mut g) = ASKED.lock() else {
            return (500, "busy".into());
        };
        let map = g.get_or_insert_with(HashMap::new);
        if let Some(a) = map.get(id) {
            if !a.done {
                return (409, "a restart is already running".into());
            }
            if a.at.elapsed() < RESTART_EVERY && a.error.is_none() {
                return (429, "restarted less than a minute ago".into());
            }
        }
        map.insert(
            id.to_string(),
            Asked {
                at: Instant::now(),
                done: false,
                error: None,
            },
        );
    }
    eprintln!("pios: restart {name} ({}) asked from the screen", &id[..12]);
    let (helper, id) = (helper.to_path_buf(), id.to_string());
    std::thread::spawn(move || {
        let r = crate::http::post(
            &helper,
            &crate::helper::restart_path(&id),
            Duration::from_secs(45),
        );
        let error = match r {
            Ok((204 | 200, _)) => None,
            Ok((status, body)) => Some(docker_message(status, &body)),
            Err(e) => Some(e.to_string()),
        };
        match &error {
            None => eprintln!("pios: restart {name}: done"),
            Some(e) => eprintln!("pios: restart {name}: failed: {e}"),
        }
        if let Ok(mut g) = ASKED.lock()
            && let Some(a) = g.get_or_insert_with(HashMap::new).get_mut(&id)
        {
            a.done = true;
            a.error = error;
        }
    });
    (202, "restarting".into())
}

#[derive(Deserialize)]
struct ApiPort {
    #[serde(rename = "IP", default)]
    ip: String,
    #[serde(rename = "PrivatePort", default)]
    private: u16,
    #[serde(rename = "PublicPort", default)]
    public: u16,
    #[serde(rename = "Type", default)]
    kind: String,
}

#[derive(Deserialize)]
struct ApiContainer {
    #[serde(rename = "Id", default)]
    id: String,
    #[serde(rename = "Names", default)]
    names: Vec<String>,
    #[serde(rename = "Image", default)]
    image: String,
    #[serde(rename = "State", default)]
    state: String,
    #[serde(rename = "Status", default)]
    status: String,
    #[serde(rename = "Ports", default)]
    ports: Vec<ApiPort>,
    #[serde(rename = "Labels", default)]
    labels: Option<HashMap<String, String>>,
}

/// Docker's Status text → (health, exit code, "12 m ago").
/// "Up 4 hours (healthy)", "Up 2 seconds (health: starting)", "Exited (137) 12 minutes ago",
/// "Restarting (1) 5 seconds ago", "Created".
pub fn parse_status(s: &str) -> (Option<String>, Option<i32>, Option<String>) {
    let health = if s.contains("(unhealthy)") {
        Some("unhealthy")
    } else if s.contains("(healthy)") {
        Some("healthy")
    } else if s.contains("(health: starting)") {
        Some("starting")
    } else {
        None
    };
    let mut code = None;
    let mut ago = None;
    for word in ["Exited (", "Restarting ("] {
        if let Some(rest) = s.strip_prefix(word)
            && let Some((n, tail)) = rest.split_once(')')
        {
            code = n.trim().parse().ok();
            ago = short_ago(tail.trim());
        }
    }
    (health.map(String::from), code, ago)
}

/// "12 minutes ago" → "12 m ago", "About an hour ago" → "1 h ago", "Less than a second ago" → "just now".
pub fn short_ago(s: &str) -> Option<String> {
    let s = s.strip_suffix(" ago")?;
    if s.starts_with("Less than") {
        return Some("just now".into());
    }
    let mut w = s.split_whitespace();
    let n = match w.next()? {
        "About" => {
            w.next();
            1
        }
        "a" | "an" => 1,
        n => n.parse::<u64>().ok()?,
    };
    let unit = match w.next()?.trim_end_matches('s') {
        "second" => "s",
        "minute" => "m",
        "hour" => "h",
        "day" => "d",
        "week" => "w",
        "month" => "mo",
        "year" => "y",
        _ => return None,
    };
    Some(format!("{n} {unit} ago"))
}

fn ports(list: &[ApiPort]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in list {
        let proto = if p.kind == "udp" { "/udp" } else { "" };
        let s = if p.public == 0 {
            format!("{}{proto} inside", p.private)
        } else if p.ip.is_empty() || p.ip == "0.0.0.0" || p.ip == "::" {
            format!("{} → {}{proto}", p.public, p.private)
        } else if p.ip.contains(':') {
            format!("[{}]:{} → {}{proto}", p.ip, p.public, p.private)
        } else {
            format!("{}:{} → {}{proto}", p.ip, p.public, p.private)
        };
        if !out.contains(&s) {
            out.push(s); // Docker lists IPv4 and IPv6 bindings separately
        }
    }
    out
}

/// The container list (no cgroup numbers yet), by name.
pub fn parse_list(body: &str) -> Result<Vec<Service>> {
    let list: Vec<ApiContainer> = serde_json::from_str(body)?;
    let mut v: Vec<Service> = list
        .into_iter()
        .map(|c| {
            let (health, exit_code, ago) = parse_status(&c.status);
            let labels = c.labels.unwrap_or_default();
            Service {
                name: c
                    .names
                    .first()
                    .map(|n| n.trim_start_matches('/').to_string())
                    .unwrap_or_default(),
                ports: ports(&c.ports),
                id: c.id,
                image: c.image,
                health,
                exit_code: if c.state == "running" {
                    None
                } else {
                    exit_code
                },
                ago,
                state: c.state,
                project: labels.get("com.docker.compose.project").cloned(),
                service: labels.get("com.docker.compose.service").cloned(),
                ..Service::default()
            }
        })
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(v)
}

/// A container's short handle for navigation: its ID's first 16 hex digits.
pub fn key(id: &str) -> u64 {
    u64::from_str_radix(id.get(..16).unwrap_or(id), 16).unwrap_or(0)
}

/// The container's cgroup directory (systemd or cgroupfs driver).
pub fn cgroup_dir(id: &str) -> Option<PathBuf> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    [
        format!("/sys/fs/cgroup/system.slice/docker-{id}.scope"),
        format!("/sys/fs/cgroup/docker/{id}"),
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_dir())
}

fn read_u64(p: &Path) -> Option<u64> {
    std::fs::read_to_string(p).ok()?.trim().parse().ok()
}

/// "usage_usec 123\n…" → 123
pub fn usage_usec(cpu_stat: &str) -> Option<u64> {
    cpu_stat
        .lines()
        .find_map(|l| l.strip_prefix("usage_usec "))
        .and_then(|v| v.trim().parse().ok())
}

/// Field 22 of /proc/PID/stat (start time in clock ticks after boot); comm may hold spaces.
pub fn start_ticks(stat: &str) -> Option<u64> {
    let after = &stat[stat.rfind(')')? + 1..];
    after.split_whitespace().nth(19)?.parse().ok()
}

fn uptime_of(dir: &Path) -> Option<u64> {
    let pid = std::fs::read_to_string(dir.join("cgroup.procs"))
        .ok()?
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .min()?; // the container's first process
    let start = start_ticks(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)?;
    // SAFETY: sysconf has no side effects
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let up: f64 = std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some((up as u64).saturating_sub(start / hz))
}

/// Containers (through the helper), host services (from /etc/pios/checks, by their cgroups)
/// and `pios web`'s latest results, with CPU sampled over 300 ms for everything running.
pub fn collect(socket: &Path) -> Result<Services> {
    let got = crate::http::get(socket, crate::helper::ALLOWED, Duration::from_secs(5));
    let (reachable, mut list) = match got {
        Ok((200, body)) => (true, parse_list(&body)?),
        _ => (false, vec![]),
    };
    let cfg = crate::checks::load(&crate::checks::path());
    let mut host = host_services(&cfg);
    let dirs: Vec<Option<PathBuf>> = list
        .iter()
        .map(|s| (s.state == "running").then(|| cgroup_dir(&s.id)).flatten())
        .chain(
            host.iter()
                .map(|s| (s.state == "running").then(|| crate::checks::unit_dir(&s.name))),
        )
        .collect();
    let cpu = |d: &Option<PathBuf>| {
        d.as_ref()
            .and_then(|d| std::fs::read_to_string(d.join("cpu.stat")).ok())
            .and_then(|t| usage_usec(&t))
    };
    let t0 = Instant::now();
    let a: Vec<Option<u64>> = dirs.iter().map(cpu).collect();
    std::thread::sleep(SAMPLE);
    let b: Vec<Option<u64>> = dirs.iter().map(cpu).collect();
    let el = t0.elapsed().as_micros().max(1) as f64;
    for (i, s) in list.iter_mut().chain(host.iter_mut()).enumerate() {
        let Some(dir) = &dirs[i] else { continue };
        if let (Some(a), Some(b)) = (a[i], b[i]) {
            s.cpu = Some(b.saturating_sub(a) as f64 / el * 100.0);
        }
        s.mem = read_u64(&dir.join("memory.current"));
        s.mem_limit = read_u64(&dir.join("memory.max")); // "max" (no limit) → None
        s.up_sec = uptime_of(dir);
        if s.host {
            s.tasks = read_u64(&dir.join("pids.current"));
            s.image = first_process(dir).unwrap_or_default();
        }
    }
    for s in list.iter_mut() {
        s.restart = recent(&s.id);
    }
    Ok(Services {
        reachable,
        services: list,
        host,
        web: crate::checks::web(&cfg, &crate::checks::web_out(), crate::types::now_ms()),
    })
}

/// A host service's handle: a stable hex ID from its name (FNV-1a), so `key` works for it.
pub fn host_id(name: &str) -> String {
    let h = name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{h:016x}")
}

fn host_services(cfg: &crate::checks::Config) -> Vec<Service> {
    crate::checks::units(cfg)
        .map(|u| u.units)
        .unwrap_or_default()
        .into_iter()
        .map(|u| Service {
            id: host_id(&u.name),
            state: if u.running { "running" } else { "stopped" }.into(),
            host: true,
            name: u.name,
            ..Service::default()
        })
        .collect()
}

/// The command name of the service's main (first) process: "nginx", "sshd".
fn first_process(dir: &Path) -> Option<String> {
    let pid = std::fs::read_to_string(dir.join("cgroup.procs"))
        .ok()?
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .min()?;
    Some(
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()?
            .trim()
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_text() {
        let s = |t| parse_status(t);
        assert_eq!(s("Up 4 hours (healthy)").0.as_deref(), Some("healthy"));
        assert_eq!(s("Up 1 second (unhealthy)").0.as_deref(), Some("unhealthy"));
        assert_eq!(
            s("Up 2 seconds (health: starting)").0.as_deref(),
            Some("starting")
        );
        assert_eq!(s("Up 3 days"), (None, None, None));
        assert_eq!(
            s("Exited (137) 12 minutes ago"),
            (None, Some(137), Some("12 m ago".into()))
        );
        assert_eq!(
            s("Restarting (1) Less than a second ago"),
            (None, Some(1), Some("just now".into()))
        );
        assert_eq!(
            s("Exited (0) About an hour ago").2.as_deref(),
            Some("1 h ago")
        );
        assert_eq!(s("Exited (2) 3 weeks ago").2.as_deref(), Some("3 w ago"));
        assert_eq!(s("Created"), (None, None, None));
        assert_eq!(short_ago("soon"), None);
    }

    #[test]
    fn container_list() {
        let body = r#"[
          {"Id":"bb","Names":["/web"],"Image":"nginx:1","State":"running",
           "Status":"Up 4 hours (healthy)",
           "Ports":[{"IP":"0.0.0.0","PrivatePort":80,"PublicPort":8080,"Type":"tcp"},
                    {"IP":"::","PrivatePort":80,"PublicPort":8080,"Type":"tcp"},
                    {"IP":"127.0.0.1","PrivatePort":53,"PublicPort":53,"Type":"udp"},
                    {"PrivatePort":9000,"Type":"tcp"}],
           "Labels":{"com.docker.compose.project":"apps","com.docker.compose.service":"web"}},
          {"Id":"aa","Names":["/api"],"Image":"api:2","State":"exited",
           "Status":"Exited (137) 12 minutes ago","Ports":[],"Labels":null}
        ]"#;
        let v = parse_list(body).unwrap();
        assert_eq!(v[0].name, "api");
        assert_eq!(v[0].exit_code, Some(137));
        assert_eq!(v[0].ago.as_deref(), Some("12 m ago"));
        assert_eq!(v[0].project, None);
        let w = &v[1];
        assert_eq!(w.health.as_deref(), Some("healthy"));
        assert_eq!(
            w.ports,
            ["8080 → 80", "127.0.0.1:53 → 53/udp", "9000 inside"]
        );
        assert_eq!(
            (w.project.as_deref(), w.service.as_deref()),
            (Some("apps"), Some("web"))
        );
        assert!(parse_list("not json").is_err());
    }

    #[test]
    fn restart_checks_and_limits() {
        let id = "c".repeat(64);
        let known: HashMap<String, String> = [(id.clone(), "web".to_string())].into();
        let helper = Path::new("/nonexistent/helper.sock");
        assert_eq!(ask_restart(helper, "abc", &known).0, 400);
        assert_eq!(ask_restart(helper, &"d".repeat(64), &known).0, 404);
        assert_eq!(ask_restart(helper, &id, &known).0, 202);
        // the helper isn't there: it fails quickly, and a failure may be retried at once
        for _ in 0..100 {
            if recent(&id).is_some_and(|r| r.done) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let r = recent(&id).unwrap();
        assert!(r.done && r.error.is_some(), "{r:?}");
        assert_eq!(ask_restart(helper, &id, &known).0, 202);
        assert_eq!(
            docker_message(500, r#"{"message":"no such container"}"#),
            "no such container"
        );
        assert_eq!(docker_message(500, "<html>"), "HTTP 500");
    }

    #[test]
    fn cgroup_files() {
        assert_eq!(usage_usec("usage_usec 1234\nuser_usec 1000\n"), Some(1234));
        assert_eq!(usage_usec("nothing"), None);
        let stat =
            "42 (my (odd) name) S 1 42 42 0 -1 4194560 100 0 0 0 5 6 0 0 20 0 1 0 98765 1000 50";
        assert_eq!(start_ticks(stat), Some(98765));
        assert_eq!(cgroup_dir("../etc"), None);
        assert_eq!(cgroup_dir(""), None);
    }
}
