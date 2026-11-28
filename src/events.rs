//! System events for History: SSH logins (and failures), sudo commands, Jenkins deploys, boots
//! and manual fan changes (fan_ctl's own log lines), read from the systemd journal once a minute
//! and kept 7 days.
//!
//! The agent is in the `systemd-journal` group (read-only) and runs `journalctl` for just the
//! sshd and sudo lines after the last cursor it saw; boots come from `journalctl --list-boots`.
//! Nothing here alerts (other tools already do). Read-only, no network.

use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Result, err};

/// SSH logins by the same user from the same address within this window show as one row (×N).
pub const MERGE_MS: i64 = 60 * 60 * 1000;
/// The first read (no cursor yet) goes back this far.
const FIRST_SINCE: &str = "-7d";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// ms since the Unix epoch
    pub ts: i64,
    /// ssh · ssh_failed · sudo · deploy · boot · fan
    pub kind: String,
    pub user: String,
    /// where from (ssh), or "" (sudo, boot)
    pub from: String,
    /// sudo: the command · deploy: the version · boot: ms the Pi was off (as text)
    /// fan: what changed ("80 % for 1 h", "auto", "overruled: too hot"); `user` is case / cooler
    pub detail: String,
    /// merged repeats (×N); 1 when alone
    #[serde(default = "one")]
    pub count: u32,
}

fn one() -> u32 {
    1
}

fn ev(ts: i64, kind: &str, user: &str, from: &str, detail: &str) -> Event {
    Event {
        ts,
        kind: kind.into(),
        user: user.into(),
        from: from.into(),
        detail: detail.into(),
        count: 1,
    }
}

/// "for USER from ADDR port" → (USER, ADDR)
fn user_from(rest: &str) -> Option<(&str, &str)> {
    let rest = rest.strip_prefix("invalid user ").unwrap_or(rest);
    let (user, rest) = rest.split_once(" from ")?;
    let addr = rest.split_whitespace().next()?;
    Some((user, addr))
}

/// "for 3599 s" → "for 1 h"; "until fan_ctl stops" → "until restart".
fn fan_for(rest: &str) -> String {
    let Some(secs) = rest
        .strip_prefix("for ")
        .and_then(|r| r.strip_suffix(" s"))
        .and_then(|n| n.parse::<u64>().ok())
    else {
        return "until restart".into();
    };
    let min = (secs + 30) / 60;
    if min >= 60 && min % 60 == 0 {
        format!("for {} h", min / 60)
    } else if min >= 60 {
        format!("for {} h {} min", min / 60, min % 60)
    } else {
        format!("for {} min", min.max(1))
    }
}

/// fan_ctl's mode-change lines → (which fan, what).
fn fan_line(msg: &str) -> Option<(&'static str, String)> {
    if let Some(rest) = msg.strip_prefix("cooler manual level ") {
        // "cooler manual level 2/4 for 3599 s"
        let (level, rest) = rest.split_once(' ')?;
        let level = level.split('/').next()?;
        let what = if level == "0" {
            "off".to_string()
        } else {
            format!("level {level}")
        };
        return Some(("cooler", format!("{what} {}", fan_for(rest))));
    }
    if let Some(rest) = msg.strip_prefix("cooler manual overruled: ") {
        let why = match rest.split_once(';') {
            Some((cpu, _)) => cpu.replace("CPU at ", "CPU ").to_string(),
            None => rest.to_string(),
        };
        return Some(("cooler", format!("overruled: {why}")));
    }
    if msg == "cooler back to the kernel" {
        return Some(("cooler", "auto".into()));
    }
    if msg.starts_with("manual overruled:") {
        return Some(("case", "overruled: too hot, 100 %".into()));
    }
    if let Some(rest) = msg.strip_prefix("manual ") {
        // "manual 80% for 3600 s"
        let (pct, rest) = rest.split_once(' ')?;
        let pct: u32 = pct.strip_suffix('%')?.parse().ok()?;
        let what = if pct == 0 {
            "off".to_string()
        } else {
            format!("{pct} %")
        };
        return Some(("case", format!("{what} {}", fan_for(rest))));
    }
    if msg == "back to auto" {
        return Some(("case", "auto".into()));
    }
    None
}

/// One journal line → an event, if it is one we show.
pub fn parse(ident: &str, msg: &str, ts: i64) -> Option<Event> {
    match ident {
        "fan_ctl" => {
            let (which, what) = fan_line(msg)?;
            Some(ev(ts, "fan", which, "", &what))
        }
        "sshd" | "sshd-session" => {
            if let Some(rest) = msg.strip_prefix("Accepted ") {
                // "Accepted publickey for admin from 1.2.3.4 port 5 ssh2: …"
                let (_, rest) = rest.split_once(" for ")?;
                let (user, from) = user_from(rest)?;
                // a deploy is shown once, by its sudo line
                (user != "pios-deploy").then(|| ev(ts, "ssh", user, from, ""))
            } else if let Some(rest) = msg.strip_prefix("Failed ") {
                // "Failed password for (invalid user )root from 1.2.3.4 port 5 ssh2"
                let (_, rest) = rest.split_once(" for ")?;
                let (user, from) = user_from(rest)?;
                Some(ev(ts, "ssh_failed", user, from, ""))
            } else if let Some(rest) = msg.strip_prefix("Invalid user ") {
                // "Invalid user admin from 1.2.3.4 port 5"
                let (user, from) = user_from(rest)?;
                Some(ev(ts, "ssh_failed", user, from, ""))
            } else {
                None
            }
        }
        "sudo" => {
            // "    admin : PWD=/home/admin ; USER=root ; COMMAND=/usr/bin/systemctl status nginx"
            let (user, rest) = msg.trim_start().split_once(" : ")?;
            let cmd = rest.split(" ; ").find_map(|f| f.strip_prefix("COMMAND="))?;
            let mut words = cmd.split_whitespace();
            let prog = words.next()?.rsplit('/').next()?;
            let args: Vec<&str> = words.collect();
            if user == "pios-deploy" && prog == "pios" && args.first() == Some(&"update") {
                return Some(ev(
                    ts,
                    "deploy",
                    user,
                    "",
                    args.get(1).copied().unwrap_or(""),
                ));
            }
            let shown = std::iter::once(prog)
                .chain(args)
                .collect::<Vec<_>>()
                .join(" ");
            Some(ev(ts, "sudo", user.trim(), "", &shown))
        }
        _ => None,
    }
}

/// MESSAGE is a string, or an array of bytes when it isn't valid UTF-8.
fn message(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => {
            let bytes: Vec<u8> = a
                .iter()
                .filter_map(|b| b.as_u64().map(|b| b as u8))
                .collect();
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
        _ => None,
    }
}

/// `journalctl -o json` output → (events, last cursor).
pub fn parse_journal(text: &str) -> (Vec<Event>, Option<String>) {
    let mut out = Vec::new();
    let mut cursor = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(c) = v.get("__CURSOR").and_then(Value::as_str) {
            cursor = Some(c.to_string());
        }
        let ts = v
            .get("__REALTIME_TIMESTAMP")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<i64>().ok())
            .map(|us| us / 1000);
        let ident = v.get("SYSLOG_IDENTIFIER").and_then(Value::as_str);
        let msg = v.get("MESSAGE").and_then(message);
        if let (Some(ts), Some(ident), Some(msg)) = (ts, ident, msg)
            && let Some(e) = parse(ident, &msg, ts)
        {
            out.push(e);
        }
    }
    (out, cursor)
}

/// New sshd, sudo and fan_ctl lines since `cursor` (or the last 7 days the first time).
pub fn read_journal(cursor: Option<&str>) -> Result<(Vec<Event>, Option<String>)> {
    let mut cmd = Command::new("journalctl");
    cmd.args([
        "--no-pager",
        "--quiet",
        "--output=json",
        "--output-fields=MESSAGE,SYSLOG_IDENTIFIER",
        "--identifier=sshd-session",
        "--identifier=sshd",
        "--identifier=sudo",
        "--identifier=fan_ctl",
    ]);
    match cursor {
        Some(c) => cmd.arg(format!("--after-cursor={c}")),
        None => cmd.arg(format!("--since={FIRST_SINCE}")),
    };
    let out = cmd.output().map_err(|e| err(format!("journalctl: {e}")))?;
    if !out.status.success() && out.stdout.is_empty() {
        return Err(err(format!(
            "journalctl: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(parse_journal(&String::from_utf8_lossy(&out.stdout)))
}

/// `journalctl --list-boots -o json` → one event per boot (with how long the Pi was off
/// before it), as (boot id, event).
pub fn parse_boots(text: &str) -> Vec<(String, Event)> {
    let Ok(Value::Array(list)) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_i64);
    let mut out = Vec::new();
    let mut prev_last: Option<i64> = None;
    for b in &list {
        let (Some(first), Some(id)) = (
            num(b, "first_entry"),
            b.get("boot_id").and_then(Value::as_str),
        ) else {
            continue;
        };
        let off = prev_last.map(|l| ((first - l) / 1000).max(0));
        out.push((
            id.to_string(),
            ev(
                first / 1000,
                "boot",
                "",
                "",
                &off.map(|ms| ms.to_string()).unwrap_or_default(),
            ),
        ));
        prev_last = num(b, "last_entry");
    }
    out
}

/// Did this boot end with a clean shutdown? systemd says "Shutting down." at the very end of one
/// (a reboot adds a line about the hardware watchdog after it); after a power cut or a hard
/// reset the journal just stops. None: can't tell (no journal).
pub fn ended_cleanly(boot_id: &str) -> Option<bool> {
    if boot_id.len() != 32 || !boot_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let out = Command::new("journalctl")
        .args([
            "--no-pager",
            "-b",
            boot_id,
            "_PID=1",
            "-n",
            "5",
            "-o",
            "cat",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        return None;
    }
    Some(text.lines().any(|l| clean_end(l.trim())))
}

/// systemd's last line on a clean shutdown, reboot, halt or kexec.
pub fn clean_end(last: &str) -> bool {
    last.starts_with("Shutting down")
}

/// The Pi has no clock battery: after a power cut it starts from the last saved time, so the
/// time between two boots isn't how long it was off. Under a minute is shown as unknown.
pub fn known_off(detail: &str) -> Option<i64> {
    detail.parse::<i64>().ok().filter(|ms| *ms >= 60_000)
}

/// Boots after one that didn't end cleanly become "power_lost" (same time and off-time).
pub fn mark_power_lost(boots: &mut [(String, Event)], clean: &dyn Fn(&str) -> Option<bool>) {
    for i in 1..boots.len() {
        if clean(&boots[i - 1].0) == Some(false) {
            boots[i].1.kind = "power_lost".into();
        }
    }
}

pub fn read_boots() -> Result<Vec<(String, Event)>> {
    let out = Command::new("journalctl")
        .args(["--no-pager", "--list-boots", "--output=json"])
        .output()
        .map_err(|e| err(format!("journalctl --list-boots: {e}")))?;
    Ok(parse_boots(&String::from_utf8_lossy(&out.stdout)))
}

/// Newest first, SSH logins (and failures) by the same user from the same address within
/// MERGE_MS of the newest one merged into one row. A boot also stored as "power_lost" (found
/// after it was first recorded) shows once, as the power loss.
pub fn merge(mut list: Vec<Event>) -> Vec<Event> {
    let lost: Vec<i64> = list
        .iter()
        .filter(|e| e.kind == "power_lost")
        .map(|e| e.ts)
        .collect();
    list.retain(|e| !(e.kind == "boot" && lost.contains(&e.ts)));
    list.sort_by_key(|e| std::cmp::Reverse(e.ts));
    let mut out: Vec<Event> = Vec::new();
    for e in list {
        if matches!(e.kind.as_str(), "ssh" | "ssh_failed")
            && let Some(m) = out.iter_mut().rev().take(8).find(|m| {
                m.kind == e.kind && m.user == e.user && m.from == e.from && m.ts - e.ts <= MERGE_MS
            })
        {
            m.count += e.count;
            continue;
        }
        out.push(e);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fan_ctl_lines() {
        let f = |m| parse("fan_ctl", m, 1).map(|e| (e.user, e.detail));
        let s = |a: &str, b: &str| Some((a.to_string(), b.to_string()));
        assert_eq!(f("manual 80% for 3600 s"), s("case", "80 % for 1 h"));
        assert_eq!(f("manual 0% for 1799 s"), s("case", "off for 30 min"));
        assert_eq!(
            f("manual 100% until fan_ctl stops"),
            s("case", "100 % until restart")
        );
        assert_eq!(f("manual 65% for 5400 s"), s("case", "65 % for 1 h 30 min"));
        assert_eq!(f("manual 65% for 8 s"), s("case", "65 % for 1 min"));
        assert_eq!(f("back to auto"), s("case", "auto"));
        assert_eq!(
            f("manual overruled: a sensor reached full temperature; fan 100%"),
            s("case", "overruled: too hot, 100 %")
        );
        assert_eq!(
            f("cooler manual level 2/4 for 3599 s"),
            s("cooler", "level 2 for 1 h")
        );
        assert_eq!(
            f("cooler manual level 0/4 until fan_ctl stops"),
            s("cooler", "off until restart")
        );
        assert_eq!(f("cooler back to the kernel"), s("cooler", "auto"));
        assert_eq!(
            f("cooler manual overruled: CPU at 75 °C; kernel in control"),
            s("cooler", "overruled: CPU 75 °C")
        );
        assert_eq!(
            f("cooler manual overruled: sensor error"),
            s("cooler", "overruled: sensor error")
        );
        for other in [
            "fan 60% auto (cpu 42.5 °C, nvme 51.8 °C)",
            "driving /sys/class/pwm/pwmchip2/pwm0",
            "fan_ctl 0.2.0 starting",
        ] {
            assert_eq!(f(other), None, "{other}");
        }
        assert_eq!(
            parse("fan_ctl", "manual 80% for 3600 s", 7).unwrap().kind,
            "fan"
        );
    }

    #[test]
    fn ssh_lines() {
        let e = parse(
            "sshd-session",
            "Accepted publickey for admin from 100.64.0.7 port 60942 ssh2: RSA SHA256:abc",
            5,
        )
        .unwrap();
        assert_eq!(
            (e.kind.as_str(), e.user.as_str(), e.from.as_str()),
            ("ssh", "admin", "100.64.0.7")
        );
        assert!(
            parse(
                "sshd-session",
                "Accepted publickey for pios-deploy from 172.17.0.2 port 1 ssh2",
                5
            )
            .is_none()
        );
        let f = parse(
            "sshd",
            "Failed password for invalid user root from 203.0.113.9 port 22 ssh2",
            5,
        )
        .unwrap();
        assert_eq!((f.kind.as_str(), f.user.as_str()), ("ssh_failed", "root"));
        let i = parse(
            "sshd-session",
            "Invalid user admin from 203.0.113.9 port 4444",
            5,
        )
        .unwrap();
        assert_eq!(
            (i.kind.as_str(), i.user.as_str(), i.from.as_str()),
            ("ssh_failed", "admin", "203.0.113.9")
        );
        assert!(
            parse(
                "sshd-session",
                "pam_unix(sshd:session): session opened for user admin(uid=1000)",
                5
            )
            .is_none()
        );
    }

    #[test]
    fn sudo_lines() {
        let s = parse(
            "sudo",
            "    admin : PWD=/home/admin ; USER=root ; COMMAND=/usr/bin/systemctl status nginx",
            5,
        )
        .unwrap();
        assert_eq!(
            (s.kind.as_str(), s.user.as_str(), s.detail.as_str()),
            ("sudo", "admin", "systemctl status nginx")
        );
        let d = parse("sudo", "pios-deploy : PWD=/var/lib/pios-deploy ; USER=root ; COMMAND=/usr/local/bin/pios update v0.7.3", 5).unwrap();
        assert_eq!((d.kind.as_str(), d.detail.as_str()), ("deploy", "v0.7.3"));
        assert!(
            parse(
                "sudo",
                "pam_unix(sudo:session): session opened for user root(uid=0) by admin(uid=1000)",
                5
            )
            .is_none()
        );
    }

    #[test]
    fn journal_json() {
        let t = r#"{"__CURSOR":"c1","__REALTIME_TIMESTAMP":"1795341600000000","SYSLOG_IDENTIFIER":"sudo","MESSAGE":"admin : PWD=/ ; USER=root ; COMMAND=/usr/bin/apt upgrade"}
{"__CURSOR":"c2","__REALTIME_TIMESTAMP":"1795341601000000","SYSLOG_IDENTIFIER":"sshd-session","MESSAGE":[65,99,99,101,112,116,101,100,32,112,117,98,108,105,99,107,101,121,32,102,111,114,32,120,32,102,114,111,109,32,49,46,50,46,51,46,52,32,112,111,114,116,32,49]}
not json"#;
        let (ev, cur) = parse_journal(t);
        assert_eq!(cur.as_deref(), Some("c2"));
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].ts, 1_795_341_600_000);
        assert_eq!((ev[1].user.as_str(), ev[1].from.as_str()), ("x", "1.2.3.4"));
    }

    #[test]
    fn boots_with_downtime() {
        let t = r#"[{"index":-1,"boot_id":"a","first_entry":1000000000,"last_entry":2000000000},
                    {"index":0,"boot_id":"b","first_entry":2013000000,"last_entry":3000000000}]"#;
        let b = parse_boots(t);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].1.detail, "");
        assert_eq!(
            (b[1].0.as_str(), b[1].1.ts, b[1].1.detail.as_str()),
            ("b", 2_013_000, "13000")
        );
        assert!(parse_boots("garbage").is_empty());
    }

    #[test]
    fn power_lost_after_an_unclean_end() {
        let t = r#"[{"boot_id":"a","first_entry":1000000000,"last_entry":2000000000},
                    {"boot_id":"b","first_entry":2013000000,"last_entry":3000000000},
                    {"boot_id":"c","first_entry":3120000000,"last_entry":4000000000}]"#;
        let mut b = parse_boots(t);
        // a ended cleanly, b just stopped (power cut), c is running
        mark_power_lost(&mut b, &|id| match id {
            "a" => Some(true),
            "b" => Some(false),
            _ => None,
        });
        let kinds: Vec<&str> = b.iter().map(|(_, e)| e.kind.as_str()).collect();
        assert_eq!(kinds, ["boot", "boot", "power_lost"]);
        assert_eq!(b[2].1.detail, "120000", "keeps how long it was off");
        assert!(clean_end("Shutting down."));
        assert_eq!(known_off("120000"), Some(120_000));
        assert_eq!(known_off("12000"), None, "the clock can't tell");
        assert!(!clean_end(
            "Finished apt-daily.service - Daily apt download activities."
        ));
        assert_eq!(ended_cleanly("../x"), None);
        // stored twice (boot first, power_lost found later): shown once, as the power loss
        let shown = merge(vec![b[1].1.clone(), b[2].1.clone(), {
            let mut e = b[2].1.clone();
            e.kind = "boot".into();
            e
        }]);
        let kinds: Vec<&str> = shown.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, ["power_lost", "boot"]);
    }

    #[test]
    fn repeats_merge() {
        let s = |ts, user: &str| ev(ts, "ssh", user, "1.2.3.4", "");
        let m = merge(vec![
            s(1_000, "admin"),
            s(2_000, "admin"),
            ev(2_500, "sudo", "admin", "", "ls"),
            s(3_000, "admin"),
            s(3_000 + MERGE_MS + 1, "admin"),
            s(4_000, "other"),
        ]);
        let ssh: Vec<(i64, u32)> = m
            .iter()
            .filter(|e| e.kind == "ssh" && e.user == "admin")
            .map(|e| (e.ts, e.count))
            .collect();
        assert_eq!(ssh, [(3_000 + MERGE_MS + 1, 1), (3_000, 3)]);
        assert_eq!(m.len(), 4);
    }
}
