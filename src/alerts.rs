//! Phone alerts (Telegram). The agent decides *what* to send; `pios web` (the only PiOS process
//! with network) sends it.
//!
//!   incident opened / escalated / recovered, and events (failed SSH, deploys, boots, SSH, sudo)
//!     → the agent's outbox (SQLite `alerts`, kept 7 days)
//!   GET /alerts?after=ID → the ones the alert settings let through (quiet at night drops all but
//!     red ones and their recovery) → pios web batches them into one message and sends it.
//!
//! The settings come from the screen (Settings → Alerts, `POST /alerts/prefs`) and live in the
//! agent's kv table, so pios web never needs to read the screen's files.

use serde::{Deserialize, Serialize};

/// `pios web` writes how sending goes here; the agent serves it to the screen.
pub const STATUS_PATH: &str = "/run/pios-web/alerts.json";

/// Minutes after local midnight at `ts_ms` (quiet hours).
pub fn local_minutes(ts_ms: i64) -> u32 {
    let t = (ts_ms / 1000) as libc::time_t;
    // SAFETY: localtime_r only writes into the struct we pass.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    (tm.tm_hour * 60 + tm.tm_min).max(0) as u32
}

/// What `pios web` reports about sending (the screen shows it under Settings → Alerts).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// a bot token is set
    pub configured: bool,
    /// the chat to send to is known (set, or learned from the first message to the bot)
    pub chat: bool,
    /// ms since epoch of the last message delivered
    pub last_sent: Option<i64>,
    /// the last failure, until the next success
    pub error: Option<String>,
}

/// Kinds in the outbox.
pub const PROBLEM: &str = "problem";
pub const RECOVERED: &str = "recovered";
pub const FAILED_SSH: &str = "ssh_failed";
pub const DEPLOY: &str = "deploy";
pub const BOOT: &str = "boot";
pub const SSH: &str = "ssh";
pub const SUDO: &str = "sudo";
pub const TEST: &str = "test";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    pub id: i64,
    pub ts: i64,
    pub kind: String,
    /// "critical" / "warn" for problems (and their recovery), "" otherwise
    pub severity: String,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prefs {
    pub problems: bool,
    pub failed_ssh: bool,
    pub deploys: bool,
    pub ssh: bool,
    pub sudo: bool,
    pub quiet: bool,
    /// the screen's night window (minutes after local midnight); used by `quiet`
    pub night_start: u32,
    pub night_end: u32,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            problems: true,
            failed_ssh: true,
            deploys: true,
            ssh: false,
            sudo: false,
            quiet: true,
            night_start: 23 * 60,
            night_end: 7 * 60,
        }
    }
}

fn in_window(start: u32, end: u32, m: u32) -> bool {
    if start == end {
        false
    } else if start < end {
        (start..end).contains(&m)
    } else {
        m >= start || m < end
    }
}

impl Prefs {
    /// `problems=1&failed_ssh=1&…&night=1380-420` (the screen's POST); unknown keys ignored.
    pub fn from_query(q: &str) -> Prefs {
        let mut p = Prefs::default();
        for kv in q.split('&') {
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            let on = v == "1";
            match k {
                "problems" => p.problems = on,
                "failed_ssh" => p.failed_ssh = on,
                "deploys" => p.deploys = on,
                "ssh" => p.ssh = on,
                "sudo" => p.sudo = on,
                "quiet" => p.quiet = on,
                "night" => {
                    if let Some((a, b)) = v.split_once('-')
                        && let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>())
                        && a < 1440
                        && b < 1440
                    {
                        (p.night_start, p.night_end) = (a, b);
                    }
                }
                _ => {}
            }
        }
        p
    }

    pub fn to_query(self) -> String {
        let b = |x: bool| if x { "1" } else { "0" };
        format!(
            "problems={}&failed_ssh={}&deploys={}&ssh={}&sudo={}&quiet={}&night={}-{}",
            b(self.problems),
            b(self.failed_ssh),
            b(self.deploys),
            b(self.ssh),
            b(self.sudo),
            b(self.quiet),
            self.night_start,
            self.night_end
        )
    }

    /// Does this alert go out, `minutes` after local midnight when it happened?
    pub fn wants(&self, a: &Alert, minutes: u32) -> bool {
        let kind_on = match a.kind.as_str() {
            PROBLEM | RECOVERED => self.problems,
            FAILED_SSH => self.failed_ssh,
            DEPLOY | BOOT => self.deploys,
            SSH => self.ssh,
            SUDO => self.sudo,
            TEST => return true,
            _ => false,
        };
        if !kind_on {
            return false;
        }
        // quiet at night: only red problems (and their recovery) wake you
        !(self.quiet && in_window(self.night_start, self.night_end, minutes))
            || (matches!(a.kind.as_str(), PROBLEM | RECOVERED) && a.severity == "critical")
    }
}

/// "3 m", "2 h 05 m"
fn took(ms: i64) -> String {
    let m = ms.max(0) / 60_000;
    if m < 60 {
        format!("{} m", m.max(1))
    } else {
        format!("{} h {:02} m", m / 60, m % 60)
    }
}

pub fn problem_text(title: &str, severity: &str, value: Option<f64>) -> String {
    let icon = if severity == "critical" {
        "🔴"
    } else {
        "🟠"
    };
    match value {
        Some(v) => format!("{icon} {title} ({v:.0})"),
        None => format!("{icon} {title}"),
    }
}

pub fn escalated_text(title: &str) -> String {
    format!("🔴 {title}: now critical")
}

pub fn recovered_text(title: &str, lasted_ms: i64) -> String {
    format!("✅ Recovered: {title} (lasted {})", took(lasted_ms))
}

/// An event row (events.rs) as an alert, or None for kinds that never alert.
pub fn from_event(e: &crate::events::Event) -> Option<(&'static str, String)> {
    Some(match e.kind.as_str() {
        "ssh_failed" => (
            FAILED_SSH,
            format!("⚠️ Failed SSH: {} from {}", e.user, e.from),
        ),
        "deploy" if e.detail == "--rollback" => (DEPLOY, "↩️ Rolled back PiOS".to_string()),
        "deploy" => (DEPLOY, format!("🚀 Deployed PiOS {}", e.detail)),
        "boot" => (
            BOOT,
            match e.detail.parse::<i64>() {
                Ok(off) => format!("🔄 The Pi booted (it was off {})", took(off)),
                Err(_) => "🔄 The Pi booted".to_string(),
            },
        ),
        // a problem, not a routine reboot: it goes out with the Problems setting
        "power_lost" => (
            PROBLEM,
            match e.detail.parse::<i64>() {
                Ok(off) => format!(
                    "⚡ The Pi lost power: no clean shutdown, back after {}",
                    took(off)
                ),
                Err(_) => "⚡ The Pi lost power: no clean shutdown".to_string(),
            },
        ),
        "ssh" => (SSH, format!("🔑 SSH: {} from {}", e.user, e.from)),
        "sudo" => (SUDO, format!("🛡️ sudo: {}: {}", e.user, e.detail)),
        _ => return None,
    })
}

/// Several alerts in one message (a burst shouldn't buzz ten times): host line, then one per line.
pub fn message(host: &str, list: &[Alert]) -> String {
    const MAX: usize = 20;
    let mut s = format!("PiOS · {host}\n");
    for a in list.iter().take(MAX) {
        s.push_str(&a.text);
        s.push('\n');
    }
    if list.len() > MAX {
        s.push_str(&format!("… and {} more\n", list.len() - MAX));
    }
    s.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(kind: &str, severity: &str) -> Alert {
        Alert {
            id: 1,
            ts: 0,
            kind: kind.into(),
            severity: severity.into(),
            text: "x".into(),
        }
    }

    #[test]
    fn power_lost_is_a_problem_alert() {
        let e = crate::events::Event {
            ts: 0,
            kind: "power_lost".into(),
            user: String::new(),
            from: String::new(),
            detail: "120000".into(),
            count: 1,
        };
        let (kind, text) = from_event(&e).unwrap();
        assert_eq!(kind, PROBLEM);
        assert_eq!(
            text,
            "⚡ The Pi lost power: no clean shutdown, back after 2 m"
        );
    }

    #[test]
    fn defaults_and_quiet_hours() {
        let p = Prefs::default();
        let noon = 12 * 60;
        let night = 2 * 60;
        assert!(p.wants(&a(PROBLEM, "warn"), noon));
        assert!(p.wants(&a(FAILED_SSH, ""), noon));
        assert!(p.wants(&a(BOOT, ""), noon));
        assert!(!p.wants(&a(SSH, ""), noon), "SSH logins are off by default");
        assert!(!p.wants(&a(SUDO, ""), noon));
        // at night only red problems and their recovery
        assert!(p.wants(&a(PROBLEM, "critical"), night));
        assert!(p.wants(&a(RECOVERED, "critical"), night));
        assert!(!p.wants(&a(PROBLEM, "warn"), night));
        assert!(!p.wants(&a(DEPLOY, ""), night));
        assert!(p.wants(&a(TEST, ""), night), "a test always goes");
        let loud = Prefs { quiet: false, ..p };
        assert!(loud.wants(&a(DEPLOY, ""), night));
    }

    #[test]
    fn query_round_trip() {
        let p = Prefs {
            problems: false,
            ssh: true,
            night_start: 1320,
            night_end: 360,
            ..Prefs::default()
        };
        assert_eq!(Prefs::from_query(&p.to_query()), p);
        assert_eq!(Prefs::from_query("night=9999-1&bogus=1"), Prefs::default());
    }

    #[test]
    fn texts() {
        assert_eq!(
            problem_text("nginx stopped", "critical", None),
            "🔴 nginx stopped"
        );
        assert_eq!(
            problem_text("CPU over 80%", "warn", Some(86.4)),
            "🟠 CPU over 80% (86)"
        );
        assert_eq!(
            recovered_text("nginx stopped", 3 * 60_000),
            "✅ Recovered: nginx stopped (lasted 3 m)"
        );
        assert_eq!(
            recovered_text("x", 125 * 60_000),
            "✅ Recovered: x (lasted 2 h 05 m)"
        );
        let list: Vec<Alert> = (0..25)
            .map(|i| Alert {
                id: i,
                ts: 0,
                kind: "x".into(),
                severity: "".into(),
                text: format!("line {i}"),
            })
            .collect();
        let m = message("home", &list);
        assert!(m.starts_with("PiOS · home\nline 0\n"));
        assert!(m.ends_with("… and 5 more"));
    }
}
