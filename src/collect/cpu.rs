//! CPU % and temperature.
//!
//! `/proc/stat` line 1 holds tick counters since boot. The % is the busy share between two
//! reads. The loop already runs every 5 s, so we compare with the previous snapshot instead of
//! sleeping: no extra wait, and the value covers the whole 5 s instead of a 0.5 s sample.

use std::path::{Path, PathBuf};

use crate::error::{Result, err, read_file};
use crate::parse::{parse_f64, parse_u64};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticks {
    pub idle: u64,
    pub total: u64,
}

/// "cpu  user nice system idle iowait irq softirq steal guest guest_nice"
/// total = first 8 fields (guest and guest_nice are already inside user and nice).
pub fn parse_stat(text: &str) -> Result<Ticks> {
    let line = text
        .lines()
        .next()
        .ok_or_else(|| err("/proc/stat is empty"))?;
    let mut fields = line.split_whitespace();
    if fields.next() != Some("cpu") {
        return Err(err(format!("/proc/stat: unexpected first line {line:?}")));
    }
    let mut v = [0u64; 8];
    for (i, slot) in v.iter_mut().enumerate() {
        let piece = fields
            .next()
            .ok_or_else(|| err(format!("/proc/stat: missing field {i}")))?;
        *slot = parse_u64(piece, &format!("/proc/stat field {i}"))?;
    }
    Ok(Ticks {
        idle: v[3] + v[4],
        total: v.iter().sum(),
    })
}

pub fn read_ticks() -> Result<Ticks> {
    parse_stat(&read_file("/proc/stat")?)
}

pub fn percent(prev: Ticks, cur: Ticks) -> f64 {
    let total = cur.total.saturating_sub(prev.total);
    if total == 0 {
        return 0.0;
    }
    let idle = cur.idle.saturating_sub(prev.idle).min(total);
    100.0 * (1.0 - idle as f64 / total as f64)
}

/// "51000\n" (millidegrees) → 51.0
pub fn parse_temp(text: &str) -> Result<f64> {
    Ok(parse_f64(text, "temperature")? / 1000.0)
}

/// Picks the CPU sensor once at start: `cpu_thermal` (Pi) or `x86_pkg_temp` (Intel),
/// else thermal_zone0, else none.
pub fn find_temp_sensor() -> Option<PathBuf> {
    let base = Path::new("/sys/class/thermal");
    let zones: Vec<PathBuf> = std::fs::read_dir(base)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("thermal_zone"))
        })
        .collect();
    for wanted in ["cpu_thermal", "x86_pkg_temp"] {
        for zone in &zones {
            let kind = std::fs::read_to_string(zone.join("type")).unwrap_or_default();
            if kind.trim() == wanted {
                return Some(zone.join("temp"));
            }
        }
    }
    let zone0 = base.join("thermal_zone0/temp");
    zone0.exists().then_some(zone0)
}

/// None on any failure: a missing sensor is "unavailable", not an error.
pub fn read_temp(sensor: Option<&Path>) -> Option<f64> {
    let text = std::fs::read_to_string(sensor?).ok()?;
    parse_temp(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT_A: &str =
        "cpu  591074 1502 211416 32461072 15788 0 12121 0 0 0\ncpu0 1 2 3 4 5 6 7 8 9 10\n";
    const STAT_B: &str = "cpu  591085 1502 211423 32461864 15788 0 12121 0 0 0\n";

    #[test]
    fn ticks_from_stat() {
        let t = parse_stat(STAT_A).unwrap();
        assert_eq!(t.idle, 32461072 + 15788);
        assert_eq!(t.total, 591074 + 1502 + 211416 + 32461072 + 15788 + 12121);
    }

    #[test]
    fn percent_between_two_reads() {
        let p = percent(parse_stat(STAT_A).unwrap(), parse_stat(STAT_B).unwrap());
        // Δtotal = 810, Δidle = 792 → 2.22 %
        assert!((p - 2.2222).abs() < 0.001, "{p}");
    }

    #[test]
    fn no_ticks_means_zero_not_nan() {
        let t = parse_stat(STAT_A).unwrap();
        assert_eq!(percent(t, t), 0.0);
    }

    #[test]
    fn guest_fields_not_counted_twice() {
        let a = parse_stat("cpu 100 0 0 100 0 0 0 0 0 0").unwrap();
        let b = parse_stat("cpu 150 0 0 150 0 0 0 0 50 0").unwrap();
        assert_eq!(percent(a, b), 50.0);
    }

    #[test]
    fn broken_stat_is_an_error() {
        assert!(parse_stat("").is_err());
        assert!(parse_stat("intr 1 2 3").is_err());
        assert!(parse_stat("cpu 1 2 3").is_err());
    }

    #[test]
    fn temperature() {
        assert_eq!(parse_temp("51000\n").unwrap(), 51.0);
        assert_eq!(parse_temp("37450").unwrap(), 37.45);
        assert!(parse_temp("").is_err());
    }
}
