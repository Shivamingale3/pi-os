//! RAM ← /proc/meminfo. used = MemTotal − MemAvailable (MemFree ignores reclaimable cache).

use crate::error::{Result, err, read_file};
use crate::parse::parse_u64;
use crate::types::Memory;

pub fn parse_meminfo(text: &str) -> Result<Memory> {
    let mut total_kb = None;
    let mut avail_kb = None;
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let slot = match key.trim() {
            "MemTotal" => &mut total_kb,
            "MemAvailable" => &mut avail_kb,
            _ => continue,
        };
        let num = rest
            .split_whitespace()
            .next()
            .ok_or_else(|| err(format!("/proc/meminfo: bad line {line:?}")))?;
        *slot = Some(parse_u64(num, key.trim())?);
    }
    let total = total_kb.ok_or_else(|| err("/proc/meminfo: MemTotal missing"))? * 1024;
    let available = avail_kb.ok_or_else(|| err("/proc/meminfo: MemAvailable missing"))? * 1024;
    if total == 0 {
        return Err(err("/proc/meminfo: MemTotal is 0"));
    }
    let used = total.saturating_sub(available);
    Ok(Memory {
        total_bytes: total,
        used_bytes: used,
        available_bytes: available,
        used_percent: used as f64 * 100.0 / total as f64,
    })
}

pub fn read() -> Result<Memory> {
    parse_meminfo(&read_file("/proc/meminfo")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO: &str = "MemTotal:       15750520 kB\nMemFree:          311736 kB\nMemAvailable:    3140048 kB\nBuffers:          123 kB\nHugePages_Total:       0\n";

    #[test]
    fn used_is_total_minus_available() {
        let m = parse_meminfo(MEMINFO).unwrap();
        assert_eq!(m.total_bytes, 15750520 * 1024);
        assert_eq!(m.available_bytes, 3140048 * 1024);
        assert_eq!(m.used_bytes, (15750520 - 3140048) * 1024);
        assert!((m.used_percent - 80.064).abs() < 0.01, "{}", m.used_percent);
    }

    #[test]
    fn zero_available_is_valid() {
        let m = parse_meminfo("MemTotal: 100 kB\nMemAvailable: 0 kB\n").unwrap();
        assert_eq!(m.used_percent, 100.0);
    }

    #[test]
    fn missing_keys_are_errors() {
        assert!(parse_meminfo("MemTotal: 100 kB\n").is_err());
        assert!(parse_meminfo("MemAvailable: 100 kB\n").is_err());
        assert!(parse_meminfo("MemTotal: 0 kB\nMemAvailable: 0 kB\n").is_err());
    }
}
