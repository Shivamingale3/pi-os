//! Host services and public hostnames to watch, from a file kept on the Pi only
//! (`/etc/pios/checks`, or PIOS_CHECKS), because the repo is public:
//!
//! ```text
//! # host NAME           a systemd service (NAME or NAME.service) that should be running
//! # web HOST[/PATH] [CODE]   https://HOST/PATH should answer CODE (default 200)
//! # mount PATH          a data disk that should be mounted there (the first one is watched)
//! host nginx
//! web status.example.com 200
//! web s3.example.com 403
//! web cloud.example.com/status.php     (an app's own health page)
//! ```
//!
//! Host services are read from their cgroup (populated = running; memory, uptime): no D-Bus,
//! no `systemctl`, no extra permissions. Web checks run in `pios web` (web.rs), the only PiOS
//! process with network access; the agent reads its results file.

use std::path::{Path, PathBuf};

use crate::types::{Unit, Units, Web};

pub const DEFAULT_PATH: &str = "/etc/pios/checks";
pub const DEFAULT_WEB_OUT: &str = "/run/pios-web/web.json";
/// The agent ignores web results older than this (`pios web` stopped).
pub const WEB_STALE_MS: i64 = 180_000;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Config {
    pub host: Vec<String>,
    /// (hostname, expected code)
    pub web: Vec<(String, u16)>,
    /// mount points of data disks (`mount` lines)
    pub mounts: Vec<String>,
}

pub fn path() -> PathBuf {
    std::env::var_os("PIOS_CHECKS")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PATH))
}

pub fn web_out() -> PathBuf {
    std::env::var_os("PIOS_WEB_OUT")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_WEB_OUT))
}

/// A unit name we accept: letters, digits, `-_.@` (no paths).
fn is_unit(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.@".contains(&b))
}

/// A hostname we accept: letters, digits, `-.` (no scheme or port).
fn is_hostname(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.contains('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-.".contains(&b))
}

/// An absolute mount point: `/` then plain path characters, no `..`, not `/` itself.
fn is_mount(s: &str) -> bool {
    s.len() > 1
        && s.len() <= 200
        && s.starts_with('/')
        && !s.ends_with('/')
        && !s.contains("..")
        && !s.contains("//")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))
}

/// `HOST` or `HOST/PATH`; the path is plain URL characters only (it goes to curl as one argument,
/// never through a shell, but stays boring anyway).
fn is_target(s: &str) -> bool {
    let (host, path) = s.split_once('/').unwrap_or((s, ""));
    is_hostname(host)
        && path.len() <= 200
        && !path.contains("..")
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~/%?=&+".contains(&b))
}

/// Unknown or malformed lines are skipped with a warning (on stderr), never fatal.
pub fn parse(text: &str) -> (Config, Vec<String>) {
    let mut c = Config::default();
    let mut warnings = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let w: Vec<&str> = line.split_whitespace().collect();
        match w.as_slice() {
            ["host", name] if is_unit(name) => {
                let name = name.trim_end_matches(".service").to_string();
                if !c.host.contains(&name) {
                    c.host.push(name);
                }
            }
            ["mount", path] if is_mount(path) => {
                if !c.mounts.iter().any(|m| m == path) {
                    c.mounts.push(path.to_string());
                }
            }
            ["web", host, rest @ ..] if is_target(host) && rest.len() <= 1 => {
                let code = match rest.first() {
                    None => Some(200),
                    Some(s) => s.parse::<u16>().ok().filter(|c| (100..600).contains(c)),
                };
                match code {
                    Some(code) if !c.web.iter().any(|(h, _)| h == host) => {
                        // the hostname is case-insensitive, the path isn't
                        let t = match host.split_once('/') {
                            Some((h, p)) => format!("{}/{p}", h.to_ascii_lowercase()),
                            None => host.to_ascii_lowercase(),
                        };
                        c.web.push((t, code))
                    }
                    Some(_) => {}
                    None => warnings.push(format!("line {}: bad HTTP code: {line}", n + 1)),
                }
            }
            _ => warnings.push(format!("line {}: not understood: {line}", n + 1)),
        }
    }
    (c, warnings)
}

/// The file, or empty if it isn't there (nothing set up yet).
pub fn load(path: &Path) -> Config {
    match std::fs::read_to_string(path) {
        Ok(t) => {
            let (c, warnings) = parse(&t);
            for w in warnings {
                eprintln!("pios: {}: {w}", path.display());
            }
            c
        }
        Err(_) => Config::default(),
    }
}

pub fn unit_dir(name: &str) -> PathBuf {
    PathBuf::from(format!("/sys/fs/cgroup/system.slice/{name}.service"))
}

/// Running = its cgroup has processes ("populated 1").
pub fn populated(events: &str) -> bool {
    events.lines().any(|l| l.trim() == "populated 1")
}

pub fn units(cfg: &Config) -> Option<Units> {
    if cfg.host.is_empty() {
        return None;
    }
    let units: Vec<Unit> = cfg
        .host
        .iter()
        .map(|name| Unit {
            name: name.clone(),
            running: std::fs::read_to_string(unit_dir(name).join("cgroup.events"))
                .map(|e| populated(&e))
                .unwrap_or(false),
        })
        .collect();
    Some(Units {
        running: units.iter().filter(|u| u.running).count() as u32,
        total: units.len() as u32,
        units,
    })
}

/// `pios web`'s latest results, if fresh and still matching the file's hosts.
pub fn web(cfg: &Config, out: &Path, now_ms: i64) -> Option<Web> {
    if cfg.web.is_empty() {
        return None;
    }
    let w: Web = serde_json::from_str(&std::fs::read_to_string(out).ok()?).ok()?;
    (now_ms - w.checked_at <= WEB_STALE_MS).then_some(w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_lines() {
        let (c, w) = parse(
            "# comment\n\nhost nginx\nhost ssh.service  # trailing\nhost nginx\n\
             web A.example.com\nweb s3.example.com 403\nweb bad 200\nweb x.example.com 99\n\
             host ../etc\nweb x.example.com/../etc\nweb y.example.com/a\"b\nfrobnicate\n\
             web Cloud.example.com/status.php\nweb q.example.com/x;rm 200\n",
        );
        assert_eq!(c.host, ["nginx", "ssh"]);
        assert_eq!(
            c.web,
            [
                ("a.example.com".to_string(), 200),
                ("s3.example.com".into(), 403),
                ("cloud.example.com/status.php".into(), 200)
            ]
        );
        assert_eq!(w.len(), 7, "{w:?}");
    }

    #[test]
    fn mount_lines() {
        let (c, w) = parse(
            "mount /mnt/data\nmount /mnt/data\nmount /\nmount mnt/x\nmount /mnt/../etc\n\
             mount /mnt/a b\nmount /mnt/x;rm\nmount /media/backup-1\nmount /mnt/data/\n",
        );
        assert_eq!(c.mounts, ["/mnt/data", "/media/backup-1"]);
        assert_eq!(w.len(), 6, "{w:?}");
    }

    #[test]
    fn cgroup_events() {
        assert!(populated("populated 1\nfrozen 0\n"));
        assert!(!populated("populated 0\nfrozen 0\n"));
        assert!(!populated(""));
    }

    #[test]
    fn stale_web_results_are_dropped() {
        let dir = std::env::temp_dir().join(format!("pios-web-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("web.json");
        let w = Web {
            ok: 1,
            total: 1,
            checked_at: 1_000_000,
            every_sec: 60,
            checks: vec![],
        };
        std::fs::write(&out, serde_json::to_string(&w).unwrap()).unwrap();
        let cfg = Config {
            host: vec![],
            web: vec![("a.example.com".into(), 200)],
            mounts: vec![],
        };
        assert!(web(&cfg, &out, 1_000_000 + WEB_STALE_MS).is_some());
        assert!(web(&cfg, &out, 1_000_001 + WEB_STALE_MS).is_none());
        assert!(web(&Config::default(), &out, 1_000_000).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
