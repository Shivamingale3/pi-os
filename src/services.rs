//! The Services screen's data, collected only on request (`GET /details/services`, while the
//! screen is open): Docker's container list (the same allow-listed GET the agent already makes)
//! plus each container's CPU, memory, limit and uptime from its cgroup. Read-only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

pub fn collect(socket: &Path) -> Result<Services> {
    let got = crate::http::get(socket, crate::helper::ALLOWED, Duration::from_secs(5));
    let mut list = match got {
        Ok((200, body)) => parse_list(&body)?,
        _ => return Ok(unreachable()),
    };
    let dirs: Vec<Option<PathBuf>> = list
        .iter()
        .map(|s| (s.state == "running").then(|| cgroup_dir(&s.id)).flatten())
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
    for (i, s) in list.iter_mut().enumerate() {
        let Some(dir) = &dirs[i] else { continue };
        if let (Some(a), Some(b)) = (a[i], b[i]) {
            s.cpu = Some(b.saturating_sub(a) as f64 / el * 100.0);
        }
        s.mem = read_u64(&dir.join("memory.current"));
        s.mem_limit = read_u64(&dir.join("memory.max")); // "max" (no limit) → None
        s.up_sec = uptime_of(dir);
    }
    Ok(Services {
        reachable: true,
        services: list,
    })
}

fn unreachable() -> Services {
    Services {
        reachable: false,
        services: vec![],
    }
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
