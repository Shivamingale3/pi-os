//! The screen's settings besides the LED ring's (ring.rs): display timeouts, the clock, the night
//! face. Saved as `key=value` lines in the screen's state directory; a bad line keeps its default.
//!
//! Every setting is a list of steps the screen walks with − / + or picks from, so nothing typed
//! can be out of range.

use std::path::Path;

/// "Screen off after" steps, minutes (None = never).
pub const OFF_AFTER: [Option<u32>; 8] = [
    Some(1),
    Some(2),
    Some(5),
    Some(10),
    Some(15),
    Some(30),
    Some(60),
    None,
];
/// "Night face after" steps, minutes without a touch.
pub const NIGHT_AFTER: [u32; 6] = [1, 2, 5, 10, 15, 30];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NightRing {
    Faint,
    Off,
    AsDay,
}

impl NightRing {
    pub const ALL: [NightRing; 3] = [NightRing::Faint, NightRing::Off, NightRing::AsDay];
    pub fn label(self) -> &'static str {
        match self {
            NightRing::Faint => "Faint",
            NightRing::Off => "Off",
            NightRing::AsDay => "As by day",
        }
    }
    fn key(self) -> &'static str {
        match self {
            NightRing::Faint => "faint",
            NightRing::Off => "off",
            NightRing::AsDay => "day",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prefs {
    /// index into OFF_AFTER
    pub off_after: usize,
    /// index into NIGHT_AFTER
    pub night_after: usize,
    pub clock24: bool,
    pub night: bool,
    /// minutes after midnight
    pub night_start: u32,
    pub night_end: u32,
    pub night_ring: NightRing,
    /// phone alerts: problems (and recovery), quiet at night, failed SSH, deploys and boots,
    /// SSH logins, sudo (the agent applies them; Settings → Alerts)
    pub alerts: [bool; 6],
}

/// Names of `Prefs::alerts`, in order.
pub const ALERT_KEYS: [&str; 6] = ["problems", "quiet", "failed_ssh", "deploys", "ssh", "sudo"];

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            off_after: 5,   // 30 min
            night_after: 1, // 2 min
            clock24: false,
            night: true,
            night_start: 23 * 60,
            night_end: 7 * 60,
            // the ring looks the same day and night unless asked otherwise
            night_ring: NightRing::AsDay,
            alerts: [true, true, true, true, false, false],
        }
    }
}

/// One change from a tap: which setting, and a choice (index) or a step (−1 / +1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    OffAfter(i8),
    NightAfter(i8),
    Clock24(bool),
    Night(bool),
    NightStart(i8),
    NightEnd(i8),
    NightRing(u8),
    /// one of ALERT_KEYS on or off
    Alert(u8, bool),
}

fn step(i: usize, d: i8, len: usize) -> usize {
    (i as i64 + d as i64).clamp(0, len as i64 - 1) as usize
}

fn step_time(m: u32, d: i8) -> u32 {
    ((m as i64 + d as i64 * 30).rem_euclid(24 * 60)) as u32
}

/// "11:30 PM" / "23:30"
pub fn clock(minutes: u32, clock24: bool) -> String {
    let (h, m) = (minutes / 60 % 24, minutes % 60);
    if clock24 {
        format!("{h:02}:{m:02}")
    } else {
        let (h12, half) = match h {
            0 => (12, "AM"),
            1..=11 => (h, "AM"),
            12 => (12, "PM"),
            _ => (h - 12, "PM"),
        };
        format!("{h12}:{m:02} {half}")
    }
}

impl Prefs {
    pub fn apply(&mut self, c: Change) {
        match c {
            Change::OffAfter(d) => self.off_after = step(self.off_after, d, OFF_AFTER.len()),
            Change::NightAfter(d) => {
                self.night_after = step(self.night_after, d, NIGHT_AFTER.len())
            }
            Change::Clock24(on) => self.clock24 = on,
            Change::Night(on) => self.night = on,
            Change::NightStart(d) => self.night_start = step_time(self.night_start, d),
            Change::NightEnd(d) => self.night_end = step_time(self.night_end, d),
            Change::NightRing(i) => {
                self.night_ring = NightRing::ALL[(i as usize).min(NightRing::ALL.len() - 1)]
            }
            Change::Alert(i, on) => {
                if let Some(a) = self.alerts.get_mut(i as usize) {
                    *a = on;
                }
            }
        }
    }

    /// Screen off after this long without a touch (None: never).
    pub fn off_after(&self) -> Option<std::time::Duration> {
        OFF_AFTER[self.off_after.min(OFF_AFTER.len() - 1)]
            .map(|m| std::time::Duration::from_secs(m as u64 * 60))
    }

    pub fn night_after(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            NIGHT_AFTER[self.night_after.min(NIGHT_AFTER.len() - 1)] as u64 * 60,
        )
    }

    pub fn off_label(&self) -> String {
        match OFF_AFTER[self.off_after.min(OFF_AFTER.len() - 1)] {
            Some(60) => "1 hour".into(),
            Some(m) => format!("{m} min"),
            None => "Never".into(),
        }
    }

    pub fn night_after_label(&self) -> String {
        format!(
            "{} min",
            NIGHT_AFTER[self.night_after.min(NIGHT_AFTER.len() - 1)]
        )
    }

    /// Is `minutes` (after local midnight) inside the night window? (It may cross midnight.)
    pub fn is_night(&self, minutes: u32) -> bool {
        if !self.night || self.night_start == self.night_end {
            return false;
        }
        let (s, e) = (self.night_start, self.night_end);
        if s < e {
            (s..e).contains(&minutes)
        } else {
            minutes >= s || minutes < e
        }
    }

    /// The agent's form of the alert settings (`POST /alerts/prefs`).
    pub fn alert_query(&self) -> String {
        let a = self.alerts;
        crate::alerts::Prefs {
            problems: a[0],
            quiet: a[1],
            failed_ssh: a[2],
            deploys: a[3],
            ssh: a[4],
            sudo: a[5],
            night_start: if self.night { self.night_start } else { 0 },
            night_end: if self.night { self.night_end } else { 0 },
        }
        .to_query()
    }

    pub fn to_text(self) -> String {
        let alerts: String = ALERT_KEYS
            .iter()
            .zip(self.alerts)
            .map(|(k, on)| format!("alert_{k}={}\n", if on { "on" } else { "off" }))
            .collect();
        alerts
            + &format!(
                "off_after={}\nnight_after={}\nclock={}\nnight={}\nnight_start={}\nnight_end={}\nnight_ring={}\n",
                self.off_after,
                self.night_after,
                if self.clock24 { "24" } else { "12" },
                if self.night { "on" } else { "off" },
                self.night_start,
                self.night_end,
                self.night_ring.key()
            )
    }

    pub fn parse(text: &str) -> Prefs {
        let mut p = Prefs::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            let n = v.parse::<u32>().ok();
            match (k, n) {
                ("off_after", Some(i)) if (i as usize) < OFF_AFTER.len() => {
                    p.off_after = i as usize
                }
                ("night_after", Some(i)) if (i as usize) < NIGHT_AFTER.len() => {
                    p.night_after = i as usize
                }
                ("clock", _) if v == "24" || v == "12" => p.clock24 = v == "24",
                ("night", _) if v == "on" || v == "off" => p.night = v == "on",
                ("night_start", Some(m)) if m < 24 * 60 && m % 30 == 0 => p.night_start = m,
                ("night_end", Some(m)) if m < 24 * 60 && m % 30 == 0 => p.night_end = m,
                (k, _) if k.starts_with("alert_") && (v == "on" || v == "off") => {
                    if let Some(i) = ALERT_KEYS
                        .iter()
                        .position(|a| Some(*a) == k.strip_prefix("alert_"))
                    {
                        p.alerts[i] = v == "on";
                    }
                }
                ("night_ring", _) => {
                    if let Some(r) = NightRing::ALL.into_iter().find(|r| r.key() == v) {
                        p.night_ring = r;
                    }
                }
                _ => {}
            }
        }
        p
    }

    pub fn load(path: Option<&Path>) -> Prefs {
        path.and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| Prefs::parse(&t))
            .unwrap_or_default()
    }

    pub fn save(&self, path: Option<&Path>) {
        if let Some(p) = path
            && let Err(e) = std::fs::write(p, self.to_text())
        {
            eprintln!("pios ui: {}: {e}", p.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn night_window_crosses_midnight() {
        let p = Prefs::default(); // 11 PM – 7 AM
        assert!(p.is_night(23 * 60));
        assert!(p.is_night(2 * 60));
        assert!(!p.is_night(7 * 60));
        assert!(!p.is_night(12 * 60));
        let day = Prefs {
            night_start: 13 * 60,
            night_end: 15 * 60,
            ..p
        };
        assert!(day.is_night(14 * 60) && !day.is_night(16 * 60));
        assert!(!Prefs { night: false, ..p }.is_night(2 * 60));
    }

    #[test]
    fn steps_stay_in_range() {
        let mut p = Prefs::default();
        for _ in 0..20 {
            p.apply(Change::OffAfter(1));
        }
        assert_eq!(p.off_after(), None, "the last step is never");
        assert_eq!(p.off_label(), "Never");
        for _ in 0..20 {
            p.apply(Change::OffAfter(-1));
        }
        assert_eq!(p.off_label(), "1 min");
        p.apply(Change::NightStart(2)); // 11 PM + 1 h wraps to midnight
        assert_eq!(p.night_start, 0);
        p.apply(Change::NightEnd(-15)); // 7 AM − 7.5 h wraps to the evening before
        assert_eq!(p.night_end, 23 * 60 + 30);
        assert_eq!(clock(p.night_end, false), "11:30 PM");
        assert_eq!(clock(p.night_end, true), "23:30");
    }

    #[test]
    fn file_round_trip_and_bad_lines() {
        let p = Prefs {
            off_after: 7,
            night_after: 3,
            clock24: true,
            night: false,
            night_start: 22 * 60 + 30,
            night_end: 6 * 60,
            night_ring: NightRing::AsDay,
            alerts: [false, true, false, true, true, false],
        };
        assert_eq!(Prefs::parse(&p.to_text()), p);
        let bad = Prefs::parse("off_after=99\nnight_start=61\nclock=13\nnight=maybe\n");
        assert_eq!(bad, Prefs::default());
    }
}
