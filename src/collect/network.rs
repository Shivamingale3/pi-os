//! rx/tx bytes per second ← /proc/net/dev (counters since boot), rate between snapshots.
//! Allowlist of real interfaces so Docker's veth*/br-*/docker0 never flood the list.

use std::collections::BTreeMap;

use crate::error::{Result, err, read_file};
use crate::parse::parse_u64;
use crate::types::{NetIface, Network};

const ALLOW: [&str; 4] = ["eth", "en", "wl", "tailscale"];

/// name → (rx bytes, tx bytes) since boot
pub type Counters = BTreeMap<String, (u64, u64)>;

pub fn parse_net_dev(text: &str) -> Result<Counters> {
    let mut out = Counters::new();
    for line in text.lines().skip(2) {
        // "  eth0: 123 …" — the name can be glued to the number ("eth0:123"), so split on ':' first.
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if !ALLOW.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let mut fields = rest.split_whitespace();
        let rx = fields
            .next()
            .ok_or_else(|| err(format!("/proc/net/dev: {name} has no rx")))?;
        let tx = fields
            .nth(7)
            .ok_or_else(|| err(format!("/proc/net/dev: {name} has no tx")))?;
        out.insert(
            name.to_string(),
            (
                parse_u64(rx, &format!("{name} rx"))?,
                parse_u64(tx, &format!("{name} tx"))?,
            ),
        );
    }
    Ok(out)
}

pub fn read_counters() -> Result<Counters> {
    parse_net_dev(&read_file("/proc/net/dev")?)
}

/// Interfaces present in both reads. A counter that went down (interface re-created) → 0.
pub fn rates(prev: &Counters, cur: &Counters, secs: f64) -> Network {
    let interfaces = cur
        .iter()
        .filter_map(|(name, &(rx, tx))| {
            let &(prx, ptx) = prev.get(name)?;
            let rate = |now: u64, before: u64| {
                if secs <= 0.0 {
                    0.0
                } else {
                    now.saturating_sub(before) as f64 / secs
                }
            };
            Some(NetIface {
                name: name.clone(),
                rx_bytes_per_sec: rate(rx, prx),
                tx_bytes_per_sec: rate(tx, ptx),
            })
        })
        .collect();
    Network { interfaces }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Laptop format (copied from the real file).
    const LAPTOP: &str = "Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 76126111  114044    0    0    0     0          0         0 76126111  114044    0    0    0     0       0          0
wlp1s0: 720834765  648824    0    0    0     0          0         0 119273553  251566    0    2    0     0       0          0
tailscale0:     580       4    0    0    0     0          0         0      801      14    0    0    0     0       0          0
docker0: 24013790   39740    0    0    0     0          0         0 24055144   44128    0    2    0     0       0          0
veth42fe4ed:     126       3    0    0    0     0          0         0    31134     454    0    0    0     0       0          0
";

    // Pi-like: eth0, a Docker bridge, and a name glued to a big counter.
    const PI: &str = "h1\nh2\n  eth0:4294967296000 1 0 0 0 0 0 0 5000 2 0 0 0 0 0 0\nbr-1a2b3c: 1 1 0 0 0 0 0 0 1 1 0 0 0 0 0 0\n wlan0: 10 1 0 0 0 0 0 0 20 1 0 0 0 0 0 0\n";

    #[test]
    fn keeps_only_real_interfaces() {
        let c = parse_net_dev(LAPTOP).unwrap();
        assert_eq!(c.keys().collect::<Vec<_>>(), ["tailscale0", "wlp1s0"]);
        assert_eq!(c["wlp1s0"], (720834765, 119273553));
    }

    #[test]
    fn pi_format_and_glued_names() {
        let c = parse_net_dev(PI).unwrap();
        assert_eq!(c.keys().collect::<Vec<_>>(), ["eth0", "wlan0"]);
        assert_eq!(c["eth0"], (4294967296000, 5000));
    }

    #[test]
    fn rates_per_second() {
        let mut a = Counters::new();
        a.insert("eth0".into(), (1000, 0));
        a.insert("wlan0".into(), (500, 500));
        let mut b = Counters::new();
        b.insert("eth0".into(), (6000, 2500));
        b.insert("wlan0".into(), (100, 600)); // rx went down: interface re-created
        b.insert("tailscale0".into(), (1, 1)); // new: skipped this round
        let n = rates(&a, &b, 5.0);
        assert_eq!(n.interfaces.len(), 2);
        assert_eq!(n.interfaces[0].name, "eth0");
        assert_eq!(n.interfaces[0].rx_bytes_per_sec, 1000.0);
        assert_eq!(n.interfaces[0].tx_bytes_per_sec, 500.0);
        assert_eq!(n.interfaces[1].rx_bytes_per_sec, 0.0);
        assert_eq!(n.interfaces[1].tx_bytes_per_sec, 20.0);
    }

    #[test]
    fn broken_line_is_an_error() {
        assert!(parse_net_dev("h1\nh2\neth0: 12\n").is_err());
        assert!(parse_net_dev("h1\nh2\neth0: x 0 0 0 0 0 0 0 1\n").is_err());
    }
}
