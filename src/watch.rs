//! The agent's checks beyond /proc, run on its 5 s loop: the data disk (mount, space, I/O, and
//! its own health report through the helper every 30 min), Tailscale's login (its local API every
//! 10 min) and the Pi's under-voltage alarm. They fill the snapshot's `data_disk`, `tailscale`
//! and `checks`; rules.rs turns a bad check into an incident like a stopped service.
//!
//! All reads: sysfs, /proc/self/mountinfo, udev's database, two unix sockets. Nothing here
//! writes outside the agent's own store (the cable-error baseline, a voltage event).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::details::{DataDisk, Smart};
use crate::rules::THRESHOLDS;
use crate::store::Store;
use crate::types::{Flag, Snapshot, Tailscale};

/// The disk's health report: this often, and never waking it (smartctl -n standby).
const SMART_EVERY: Duration = Duration::from_secs(30 * 60);
/// … first one a minute after start (not in the agent's first busy seconds)
const SMART_FIRST: Duration = Duration::from_secs(60);
const SMART_TIMEOUT: Duration = Duration::from_secs(30);
const TAILSCALE_EVERY: Duration = Duration::from_secs(10 * 60);
pub const TAILSCALE_SOCKET: &str = "/var/run/tailscale/tailscaled.sock";
/// Amber this many days before Tailscale's login expires.
pub const TAILSCALE_WARN_DAYS: i64 = 14;
/// Cable-error and bad-sector rises count as "new" for a day.
const BASE_KEEP_MS: i64 = 24 * 3600 * 1000;
const BASE_KEY: &str = "data_disk_base";

pub struct Watch {
    started: Instant,
    io: Option<Io>,
    smart: Option<(Instant, Smart)>,
    smart_at: Option<Instant>,
    tailscale: Option<(Instant, Option<Tailscale>)>,
    volt: Option<Option<PathBuf>>,
    volt_alarm: bool,
}

/// The data disk's last I/O counters (sectors read, written) and when they last moved.
struct Io {
    dev: String,
    read: u64,
    written: u64,
    at: Instant,
    moved: Instant,
}

impl Default for Watch {
    fn default() -> Self {
        Self::new()
    }
}

impl Watch {
    pub fn new() -> Self {
        Watch {
            started: Instant::now(),
            io: None,
            smart: None,
            smart_at: None,
            tailscale: None,
            volt: None,
            volt_alarm: false,
        }
    }

    pub fn tick(&mut self, s: &mut Snapshot, mounts: &[String], store: &Store, helper: &Path) {
        if let Some(m) = mounts.first() {
            let d = self.data_disk(m, store, helper);
            s.checks.extend(disk_flags(&d));
            s.data_disk = Some(d);
        } else {
            self.io = None;
        }
        if self
            .tailscale
            .as_ref()
            .is_none_or(|(t, _)| t.elapsed() >= TAILSCALE_EVERY)
        {
            self.tailscale = Some((Instant::now(), read_tailscale(Path::new(TAILSCALE_SOCKET))));
        }
        if let Some((_, Some(t))) = &self.tailscale {
            s.checks.push(tailscale_flag(t, s.ts));
            s.tailscale = Some(t.clone());
        }
        if let Some(alarm) = self.voltage() {
            if alarm && !self.volt_alarm {
                let e = crate::events::Event {
                    ts: s.ts,
                    kind: "power".into(),
                    user: String::new(),
                    from: String::new(),
                    detail: "supply voltage too low".into(),
                    count: 1,
                };
                if let Err(e) = store.insert_events(&[e]) {
                    eprintln!("pios: voltage event: {e}");
                }
            }
            self.volt_alarm = alarm;
            s.checks.push(Flag {
                key: "check:voltage".into(),
                title: "Low supply voltage".into(),
                bad: alarm,
                critical: false,
            });
        }
    }

    /// The under-voltage alarm of the `rpi_volt` sensor (Pi 4/5), if there is one.
    fn voltage(&mut self) -> Option<bool> {
        let path = self
            .volt
            .get_or_insert_with(|| {
                std::fs::read_dir("/sys/class/hwmon")
                    .ok()?
                    .flatten()
                    .find_map(|e| {
                        let name = std::fs::read_to_string(e.path().join("name")).ok()?;
                        (name.trim() == "rpi_volt").then(|| e.path().join("in0_lcrit_alarm"))
                    })
            })
            .clone()?;
        std::fs::read_to_string(path).ok().map(|v| v.trim() == "1")
    }

    fn data_disk(&mut self, mount: &str, store: &Store, helper: &Path) -> DataDisk {
        let mut d = DataDisk {
            mount: mount.to_string(),
            ..DataDisk::default()
        };
        let info = std::fs::read_to_string("/proc/self/mountinfo")
            .ok()
            .and_then(|t| find_mount(&t, mount));
        let Some(info) = info else {
            self.io = None;
            return d;
        };
        d.mounted = true;
        d.read_only = info.read_only;
        d.fstype = info.fstype.clone();
        if let Ok(fs) = crate::collect::disk::read(mount) {
            d.used = fs.used_bytes;
            d.size = fs.total_bytes;
            d.percent = fs.used_percent;
        }
        d.label = udev_label(&info.dev).unwrap_or_default();
        let Some(dev) = disk_of(&info.dev) else {
            return d;
        };
        let sys = PathBuf::from("/sys/block").join(&dev);
        d.model = read_trim(&sys.join("device/model")).unwrap_or_default();
        d.dev_size = read_trim(&sys.join("size"))
            .and_then(|s| s.parse::<u64>().ok())
            .map_or(0, |n| n * 512);
        d.link = link(&sys).unwrap_or_default();

        // I/O rates and idle time from the block counters
        let now = Instant::now();
        if let Some((read, written)) = read_trim(&sys.join("stat")).and_then(|t| sectors(&t)) {
            match &mut self.io {
                Some(io) if io.dev == dev => {
                    let secs = now.duration_since(io.at).as_secs_f64().max(0.001);
                    d.read_bps = read.saturating_sub(io.read) as f64 * 512.0 / secs;
                    d.write_bps = written.saturating_sub(io.written) as f64 * 512.0 / secs;
                    if read != io.read || written != io.written {
                        io.moved = now;
                    }
                    io.read = read;
                    io.written = written;
                    io.at = now;
                }
                _ => {
                    self.io = Some(Io {
                        dev: dev.clone(),
                        read,
                        written,
                        at: now,
                        moved: now,
                    })
                }
            }
        }
        let moved = self.io.as_ref().map(|io| io.moved);
        d.idle_sec = moved.map(|m| now.duration_since(m).as_secs());

        // its health report: every 30 min, through the helper (root, smartctl)
        let due = match self.smart_at {
            None => self.started.elapsed() >= SMART_FIRST,
            Some(t) => t.elapsed() >= SMART_EVERY,
        };
        if due {
            self.smart_at = Some(now);
            match ask_smart(helper, &dev) {
                Ok(Answer::Asleep) => {
                    if let Some((_, s)) = &mut self.smart {
                        s.asleep = true;
                    }
                }
                Ok(Answer::Report(mut s)) => {
                    baselines(&mut s, store, crate::types::now_ms());
                    self.smart = Some((now, s));
                }
                Err(e) => eprintln!("pios: disk health: {e}"),
            }
        }
        if let Some((at, s)) = &mut self.smart {
            // any I/O since it was found asleep means it's awake again
            if s.asleep && moved.is_some_and(|m| m > *at) {
                s.asleep = false;
            }
            let mut s = s.clone();
            s.age_sec = now.duration_since(*at).as_secs();
            d.smart = Some(s);
        }
        d
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MountInfo {
    /// "8:1"
    pub dev: String,
    pub fstype: String,
    /// the filesystem itself is read-only (not just this view of it)
    pub read_only: bool,
}

/// The last /proc/self/mountinfo entry for `path` (the one on top).
pub fn find_mount(text: &str, path: &str) -> Option<MountInfo> {
    text.lines().rev().find_map(|l| {
        let (left, right) = l.split_once(" - ")?;
        let f: Vec<&str> = left.split(' ').collect();
        let r: Vec<&str> = right.split(' ').collect();
        if f.get(4) != Some(&path) {
            return None;
        }
        Some(MountInfo {
            dev: f.get(2)?.to_string(),
            fstype: r.first()?.to_string(),
            read_only: r.get(2).is_some_and(|o| o.split(',').any(|x| x == "ro")),
        })
    })
}

/// "8:1" → "sda": the whole disk a partition (or disk) belongs to.
fn disk_of(dev: &str) -> Option<String> {
    let p = std::fs::canonicalize(format!("/sys/dev/block/{dev}")).ok()?;
    let disk = if p.join("partition").exists() {
        p.parent()?.to_path_buf()
    } else {
        p
    };
    let name = disk.file_name()?.to_str()?.to_string();
    is_disk_name(&name).then_some(name)
}

/// A SCSI/SATA/USB disk name: sd + 1–2 letters. Anything else gets no health report.
pub fn is_disk_name(s: &str) -> bool {
    s.strip_prefix("sd")
        .is_some_and(|r| (1..=2).contains(&r.len()) && r.bytes().all(|b| b.is_ascii_lowercase()))
}

/// Its filesystem label, from udev's database (no device access needed).
fn udev_label(dev: &str) -> Option<String> {
    let t = std::fs::read_to_string(format!("/run/udev/data/b{dev}")).ok()?;
    t.lines()
        .find_map(|l| l.strip_prefix("E:ID_FS_LABEL="))
        .map(String::from)
}

fn read_trim(p: &Path) -> Option<String> {
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
}

/// /sys/block/X/stat → (sectors read, sectors written)
pub fn sectors(stat: &str) -> Option<(u64, u64)> {
    let f: Vec<u64> = stat
        .split_whitespace()
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    Some((*f.get(2)?, *f.get(6)?))
}

/// "USB 3 · 5 Gb/s" for a disk on USB (its USB device's `speed`, Mb/s), else None.
fn link(sys: &Path) -> Option<String> {
    let p = std::fs::canonicalize(sys).ok()?;
    let speed = p
        .ancestors()
        .find(|a| a.join("speed").exists() && a.join("idVendor").exists())
        .and_then(|a| read_trim(&a.join("speed")))?;
    Some(usb_link(&speed))
}

pub fn usb_link(mbps: &str) -> String {
    match mbps {
        "20000" => "USB 3 · 20 Gb/s".into(),
        "10000" => "USB 3 · 10 Gb/s".into(),
        "5000" => "USB 3 · 5 Gb/s".into(),
        "480" => "USB 2 · 480 Mb/s".into(),
        other => format!("USB · {other} Mb/s"),
    }
}

/// What the helper's smartctl said.
#[derive(Debug, PartialEq)]
pub enum Answer {
    Asleep,
    Report(Smart),
}

/// The helper's answer: {"exit": N, "out": <smartctl's JSON>}.
#[derive(Deserialize, Serialize)]
pub struct SmartReply {
    pub exit: i32,
    pub out: Value,
}

fn ask_smart(helper: &Path, dev: &str) -> crate::error::Result<Answer> {
    let (status, body) = crate::http::get(helper, &format!("/smart/{dev}"), SMART_TIMEOUT)?;
    if status != 200 {
        return Err(crate::error::err(format!("helper: HTTP {status}: {body}")));
    }
    let r: SmartReply = serde_json::from_str(&body)?;
    parse_smart(&r).ok_or_else(|| crate::error::err(format!("smartctl exit {}", r.exit)))
}

/// smartctl -j output → its report, or Asleep (with -n standby it stops at "STANDBY", exit 2).
pub fn parse_smart(r: &SmartReply) -> Option<Answer> {
    let o = &r.out;
    let standby = o["smartctl"]["messages"].as_array().is_some_and(|m| {
        m.iter()
            .any(|x| x["string"].as_str().is_some_and(|s| s.contains("STANDBY")))
    });
    if standby {
        return Some(Answer::Asleep);
    }
    let passed = o["smart_status"]["passed"].as_bool()?;
    let attr = |id: u64| -> u64 {
        o["ata_smart_attributes"]["table"]
            .as_array()
            .and_then(|t| t.iter().find(|a| a["id"].as_u64() == Some(id)))
            .and_then(|a| a["raw"]["value"].as_u64())
            .unwrap_or(0)
    };
    let hours = o["power_on_time"]["hours"].as_u64().unwrap_or(0);
    // the running test (status value 0xF_) or the newest logged one
    let status = &o["ata_smart_data"]["self_test"]["status"];
    let (test, test_ago_sec) = if status["value"].as_u64().is_some_and(|v| v >> 4 == 0xF) {
        let left = status["remaining_percent"].as_u64().unwrap_or(0);
        (format!("running {} %", 100 - left.min(100)), None)
    } else {
        match o["ata_smart_self_test_log"]["standard"]["table"]
            .as_array()
            .and_then(|t| t.first())
        {
            Some(t) => {
                let passed = t["status"]["passed"].as_bool().unwrap_or(false);
                let at = t["lifetime_hours"].as_u64().unwrap_or(hours);
                (
                    if passed { "passed" } else { "failed" }.to_string(),
                    Some(hours.saturating_sub(at) * 3600),
                )
            }
            None => (String::new(), None),
        }
    };
    Some(Answer::Report(Smart {
        passed,
        reallocated: attr(5),
        pending: attr(197),
        uncorrectable: attr(198),
        crc: attr(199),
        crc_new: 0,
        reallocated_new: 0,
        temp_c: o["temperature"]["current"].as_f64(),
        hours,
        test,
        test_ago_sec,
        asleep: false,
        age_sec: 0,
    }))
}

/// Cable-error and bad-sector counts a day ago, kept in the store, so a rise shows as "new".
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Base {
    pub at: i64,
    pub crc: u64,
    pub reallocated: u64,
}

pub fn rises(s: &mut Smart, base: Option<Base>, now: i64) -> Base {
    let b = base.unwrap_or(Base {
        at: now,
        crc: s.crc,
        reallocated: s.reallocated,
    });
    s.crc_new = s.crc.saturating_sub(b.crc);
    s.reallocated_new = s.reallocated.saturating_sub(b.reallocated);
    // a day on, today's counts become the baseline (a counter going down = another disk)
    if now - b.at >= BASE_KEEP_MS || s.crc < b.crc || s.reallocated < b.reallocated {
        Base {
            at: now,
            crc: s.crc,
            reallocated: s.reallocated,
        }
    } else {
        b
    }
}

fn baselines(s: &mut Smart, store: &Store, now: i64) {
    let base = store
        .get_kv(BASE_KEY)
        .and_then(|j| serde_json::from_str::<Base>(&j).ok());
    let next = rises(s, base, now);
    if Some(next) != base
        && let Ok(j) = serde_json::to_string(&next)
        && let Err(e) = store.set_kv(BASE_KEY, &j)
    {
        eprintln!("pios: disk baseline: {e}");
    }
}

/// The data disk's checks: mounted, writable, space, health.
pub fn disk_flags(d: &DataDisk) -> Vec<Flag> {
    let mut out = vec![
        Flag {
            key: "check:data-mounted".into(),
            title: format!("Data disk not mounted ({})", d.mount),
            bad: !d.mounted,
            critical: true,
        },
        Flag {
            key: "check:data-writable".into(),
            title: "Data disk switched to read-only".into(),
            bad: d.mounted && d.read_only,
            critical: true,
        },
    ];
    if d.mounted
        && let Some(t) = THRESHOLDS.iter().find(|t| t.metric == "disk.usedPercent")
    {
        out.push(Flag {
            key: "check:data-space".into(),
            title: format!("Data disk {:.0}% full", d.percent),
            bad: d.percent >= t.warn,
            critical: d.percent >= t.critical,
        });
    }
    if let Some(s) = &d.smart {
        let (level, why) = s.level();
        out.push(Flag {
            key: "check:data-health".into(),
            title: if why.is_empty() {
                "Data disk health".into()
            } else {
                format!("Data disk {why}")
            },
            bad: level > 0,
            critical: level == 2,
        });
    }
    out
}

/// tailscaled's local API: its state and when the login expires.
fn read_tailscale(socket: &Path) -> Option<Tailscale> {
    if !socket.exists() {
        return None;
    }
    let (status, body) = crate::http::get_host(
        socket,
        "local-tailscaled.sock",
        "/localapi/v0/status?peers=false",
        Duration::from_secs(3),
    )
    .ok()?;
    (status == 200).then(|| parse_tailscale(&body)).flatten()
}

pub fn parse_tailscale(body: &str) -> Option<Tailscale> {
    let v: Value = serde_json::from_str(body).ok()?;
    Some(Tailscale {
        state: v["BackendState"].as_str()?.to_string(),
        key_expiry: v["Self"]["KeyExpiry"].as_str().and_then(rfc3339_ms),
    })
}

/// "2027-05-09T11:01:10Z" (UTC, optional fraction) → ms since the epoch.
pub fn rfc3339_ms(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let d: Vec<i64> = date
        .split('-')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    let time = time.split('.').next()?;
    let t: Vec<i64> = time
        .split(':')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    let (&[y, m, day], &[hh, mm, ss]) = (d.as_slice(), t.as_slice()) else {
        return None;
    };
    // days from 1970-01-01 (civil-from-days, inverted; proleptic Gregorian)
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400) + hh * 3600 + mm * 60 + ss) * 1000)
}

/// Tailscale's login as a check and as the Host tab's note: (text, level 0–2).
pub fn tailscale_note(t: &Tailscale, now: i64) -> (String, u8) {
    if t.state != "Running" {
        return match t.state.as_str() {
            "NeedsLogin" => ("logged out".into(), 2),
            other => (other.to_lowercase(), 2),
        };
    }
    match t.key_expiry {
        None => ("login never expires".into(), 0),
        Some(e) if e <= now => {
            let ago = (now - e) / 86_400_000;
            (
                if ago == 0 {
                    "login expired today".into()
                } else {
                    format!("login expired {ago} d ago")
                },
                2,
            )
        }
        Some(e) => {
            let left = (e - now) / 86_400_000;
            (
                if left == 0 {
                    "login ends today".into()
                } else {
                    format!("login ends in {left} d")
                },
                u8::from(left < TAILSCALE_WARN_DAYS),
            )
        }
    }
}

fn tailscale_flag(t: &Tailscale, now: i64) -> Flag {
    let (text, level) = tailscale_note(t, now);
    Flag {
        key: "check:tailscale".into(),
        title: format!("Tailscale {text}"),
        bad: level > 0,
        critical: level == 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo() {
        let t = "\
22 1 259:2 / / ro,relatime shared:1 - ext4 /dev/nvme0n1p2 rw\n\
91 22 8:1 / /mnt/data ro,noatime shared:40 - ext4 /dev/sda1 rw\n\
92 22 8:17 / /mnt/old rw - ext4 /dev/sdb1 ro,errors=remount-ro\n";
        // the agent's view is read-only (ProtectSystem); the filesystem itself isn't
        assert_eq!(
            find_mount(t, "/mnt/data"),
            Some(MountInfo {
                dev: "8:1".into(),
                fstype: "ext4".into(),
                read_only: false
            })
        );
        assert!(find_mount(t, "/mnt/old").unwrap().read_only);
        assert_eq!(find_mount(t, "/mnt"), None);
    }

    #[test]
    fn disk_names_and_stat() {
        assert!(is_disk_name("sda") && is_disk_name("sdab"));
        assert!(!is_disk_name("sda1") && !is_disk_name("nvme0n1") && !is_disk_name("sd"));
        assert!(!is_disk_name("sd/a") && !is_disk_name("sdabc"));
        let stat = "  1146   200  95270   4000   35  10  2048  70  0  1000  4070  0 0 0 0 0 0";
        assert_eq!(sectors(stat), Some((95270, 2048)));
        assert_eq!(usb_link("5000"), "USB 3 · 5 Gb/s");
        assert_eq!(usb_link("480"), "USB 2 · 480 Mb/s");
    }

    fn reply(v: Value, exit: i32) -> SmartReply {
        SmartReply { exit, out: v }
    }

    #[test]
    fn smart_report() {
        // trimmed from smartctl 7.4 -j on the real disk (-d sat, through a USB bridge)
        let v = serde_json::json!({
            "smart_status": {"passed": true},
            "temperature": {"current": 47},
            "power_on_time": {"hours": 7217},
            "ata_smart_data": {"self_test": {"status": {"value": 0, "passed": true}}},
            "ata_smart_attributes": {"table": [
                {"id": 5, "raw": {"value": 0}},
                {"id": 194, "raw": {"value": 77309411375u64}},
                {"id": 197, "raw": {"value": 0}},
                {"id": 199, "raw": {"value": 3}}
            ]},
            "ata_smart_self_test_log": {"standard": {"table": [
                {"status": {"value": 0, "passed": true}, "lifetime_hours": 7169}
            ]}}
        });
        let Some(Answer::Report(s)) = parse_smart(&reply(v, 0)) else {
            panic!()
        };
        assert!(s.passed);
        assert_eq!((s.crc, s.reallocated, s.pending), (3, 0, 0));
        assert_eq!(s.temp_c, Some(47.0));
        assert_eq!(s.test, "passed");
        assert_eq!(s.test_ago_sec, Some(48 * 3600));
        assert_eq!(s.level().0, 0);

        let running = serde_json::json!({
            "smart_status": {"passed": true},
            "ata_smart_data": {"self_test": {"status": {"value": 249, "remaining_percent": 60}}}
        });
        let Some(Answer::Report(s)) = parse_smart(&reply(running, 0)) else {
            panic!()
        };
        assert_eq!(s.test, "running 40 %");

        let asleep = serde_json::json!({
            "smartctl": {"messages": [{"string": "Device is in STANDBY mode, exit(2)"}]}
        });
        assert_eq!(parse_smart(&reply(asleep, 2)), Some(Answer::Asleep));
        assert_eq!(parse_smart(&reply(serde_json::json!({}), 1)), None);
    }

    #[test]
    fn health_levels() {
        let ok = Smart {
            passed: true,
            temp_c: Some(40.0),
            ..Smart::default()
        };
        assert_eq!(ok.level().0, 0);
        assert_eq!(
            Smart {
                passed: false,
                ..ok.clone()
            }
            .level()
            .0,
            2
        );
        assert_eq!(
            Smart {
                temp_c: Some(61.0),
                ..ok.clone()
            }
            .level()
            .0,
            2
        );
        assert_eq!(
            Smart {
                temp_c: Some(56.0),
                ..ok.clone()
            }
            .level()
            .0,
            1
        );
        assert_eq!(
            Smart {
                pending: 8,
                ..ok.clone()
            }
            .level()
            .1,
            "8 unreadable sectors"
        );
        assert_eq!(
            Smart {
                crc_new: 1,
                ..ok.clone()
            }
            .level()
            .0,
            1
        );
        // old reallocated sectors alone don't keep it amber; new ones do
        assert_eq!(
            Smart {
                reallocated: 16,
                ..ok.clone()
            }
            .level()
            .0,
            0
        );
        assert_eq!(
            Smart {
                reallocated_new: 2,
                ..ok.clone()
            }
            .level()
            .0,
            1
        );
        assert_eq!(
            Smart {
                test: "failed".into(),
                ..ok
            }
            .level()
            .0,
            1
        );
    }

    #[test]
    fn cable_error_rises_last_a_day() {
        let day = BASE_KEEP_MS;
        let mut s = Smart {
            crc: 3,
            ..Smart::default()
        };
        let b = rises(&mut s, None, 0);
        assert_eq!((s.crc_new, b.crc), (0, 3));
        s.crc = 5;
        let b = rises(&mut s, Some(b), day / 2);
        assert_eq!((s.crc_new, b.crc, b.at), (2, 3, 0), "kept during the day");
        let b = rises(&mut s, Some(b), day);
        assert_eq!(
            (s.crc_new, b.crc),
            (2, 5),
            "a day on: shown once more, then the new base"
        );
        let _ = rises(&mut s, Some(b), day + 1);
        assert_eq!(s.crc_new, 0);
    }

    #[test]
    fn flags_for_the_disk() {
        let mut d = DataDisk {
            mount: "/mnt/data".into(),
            ..DataDisk::default()
        };
        let f = disk_flags(&d);
        assert!(f[0].bad && f[0].critical, "not mounted is red");
        assert_eq!(f.len(), 2, "no space or health check while unmounted");
        d.mounted = true;
        d.percent = 91.0;
        d.smart = Some(Smart {
            passed: true,
            pending: 2,
            ..Smart::default()
        });
        let f = disk_flags(&d);
        assert!(!f[0].bad && !f[1].bad);
        assert!(f[2].bad, "{:?}", f[2]);
        assert_eq!(f[3].title, "Data disk 2 unreadable sectors");
        assert!(f[3].bad && !f[3].critical);
    }

    #[test]
    fn tailscale_login() {
        let body = r#"{"BackendState":"Running","Self":{"KeyExpiry":"2027-05-09T11:01:10Z"}}"#;
        let t = parse_tailscale(body).unwrap();
        // 2027-05-09T11:01:10Z
        assert_eq!(t.key_expiry, Some(1_809_860_470_000));
        let never = parse_tailscale(r#"{"BackendState":"Running","Self":{}}"#).unwrap();
        assert_eq!(tailscale_note(&never, 0), ("login never expires".into(), 0));
        let day = 86_400_000;
        let e = t.key_expiry.unwrap();
        assert_eq!(
            tailscale_note(&t, e - 162 * day),
            ("login ends in 162 d".into(), 0)
        );
        assert_eq!(
            tailscale_note(&t, e - 6 * day),
            ("login ends in 6 d".into(), 1)
        );
        assert_eq!(
            tailscale_note(&t, e + 2 * day),
            ("login expired 2 d ago".into(), 2)
        );
        let out = Tailscale {
            state: "NeedsLogin".into(),
            key_expiry: None,
        };
        assert_eq!(tailscale_note(&out, 0), ("logged out".into(), 2));
        assert_eq!(rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339_ms("2000-03-01T00:00:00.5Z"), Some(951_868_800_000));
        assert_eq!(rfc3339_ms("garbage"), None);
    }
}
