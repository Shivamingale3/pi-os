//! hostname · uptime · load  ← /proc/sys/kernel/hostname, /proc/uptime, /proc/loadavg

use crate::error::{Result, err, read_file};
use crate::parse::parse_f64;
use crate::types::Host;

pub fn parse_uptime(text: &str) -> Result<f64> {
    let first = text
        .split_whitespace()
        .next()
        .ok_or_else(|| err("/proc/uptime is empty"))?;
    parse_f64(first, "uptime")
}

pub fn parse_loadavg(text: &str) -> Result<[f64; 3]> {
    let mut fields = text.split_whitespace();
    let mut out = [0.0; 3];
    for (slot, label) in out.iter_mut().zip(["load 1m", "load 5m", "load 15m"]) {
        let piece = fields
            .next()
            .ok_or_else(|| err(format!("/proc/loadavg: missing {label}")))?;
        *slot = parse_f64(piece, label)?;
    }
    Ok(out)
}

pub fn read() -> Result<Host> {
    let hostname = read_file("/proc/sys/kernel/hostname")?.trim().to_string();
    if hostname.is_empty() {
        return Err(err("hostname is empty"));
    }
    Ok(Host {
        hostname,
        uptime_sec: parse_uptime(&read_file("/proc/uptime")?)?,
        load: parse_loadavg(&read_file("/proc/loadavg")?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_and_load() {
        assert_eq!(parse_uptime("56550.23 84067.75\n").unwrap(), 56550.23);
        assert_eq!(
            parse_loadavg("0.19 0.37 0.52 1/3019 39388\n").unwrap(),
            [0.19, 0.37, 0.52]
        );
    }

    #[test]
    fn broken_input_is_an_error() {
        assert!(parse_uptime("").is_err());
        assert!(parse_loadavg("0.19 0.37").is_err());
    }
}
