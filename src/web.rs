//! `pios web`: checks the public hostnames in /etc/pios/checks every 60 s, from outside in
//! (DNS → Cloudflare → the tunnel → the app), and writes the results for the agent.
//!
//! It is the only PiOS process with network access (its own unit, `pios-web.service`); the
//! agent stays on unix sockets only. One `curl --parallel` per round fetches every host at
//! once (curl is already on the Pi; no TLS code in PiOS), redirects not followed, 10 s each.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::checks::{self, Config};
use crate::error::{Result, err};
use crate::signals;
use crate::types::{Web, WebCheck, now_ms};

pub const EVERY: Duration = Duration::from_secs(60);
const TIMEOUT_SECS: &str = "10";

/// One curl `--write-out` line per transfer: url, code, seconds, error (tab-separated).
const WRITE_OUT: &str = "%{url}\\t%{response_code}\\t%{time_total}\\t%{errormsg}\\n";

/// One host's answer: (host, HTTP code, ms, curl's error).
pub type Got = (String, Option<u16>, Option<u32>, Option<String>);

/// "host" → https://host/ · "host/status.php" → https://host/status.php
fn url(target: &str) -> String {
    if target.contains('/') {
        format!("https://{target}")
    } else {
        format!("https://{target}/")
    }
}

/// curl's output → (host, code, ms, error), in any order.
pub fn parse_write_out(text: &str) -> Vec<Got> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.splitn(4, '\t');
            let host = f
                .next()?
                .strip_prefix("https://")?
                .trim_end_matches('/')
                .to_string();
            let code = f.next()?.trim().parse::<u16>().ok().filter(|c| *c > 0);
            let secs: Option<f64> = f.next().and_then(|s| s.trim().parse().ok());
            let error = f
                .next()
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(String::from);
            let ms = code.and(secs).map(|s| (s * 1000.0).round() as u32);
            Some((host, code, ms, error))
        })
        .collect()
}

/// Merges one round into the previous results (the failure streaks carry over).
pub fn merge(cfg: &Config, got: &[Got], prev: Option<&Web>, at: i64) -> Web {
    let checks: Vec<WebCheck> = cfg
        .web
        .iter()
        .map(|(host, expect)| {
            let (code, ms, error) = got
                .iter()
                .find(|g| &g.0 == host)
                .map(|g| (g.1, g.2, g.3.clone()))
                .unwrap_or((None, None, Some("not checked".into())));
            let ok = code == Some(*expect);
            let before = prev
                .and_then(|p| p.checks.iter().find(|c| &c.host == host))
                .map_or(0, |c| c.fails);
            WebCheck {
                host: host.clone(),
                expect: *expect,
                code,
                ms,
                error: if ok { None } else { error },
                ok,
                fails: if ok { 0 } else { before + 1 },
            }
        })
        .collect();
    Web {
        ok: checks.iter().filter(|c| c.ok).count() as u32,
        total: checks.len() as u32,
        checked_at: at,
        every_sec: EVERY.as_secs() as u32,
        checks,
    }
}

fn round(cfg: &Config) -> Result<Vec<Got>> {
    let mut cmd = Command::new("curl");
    cmd.args([
        "--silent",
        "--parallel",
        "--parallel-max",
        "16",
        "--max-time",
        TIMEOUT_SECS,
        "--user-agent",
        "pios-check",
        "--write-out",
        WRITE_OUT,
    ]);
    for (host, _) in &cfg.web {
        cmd.args(["--output", "/dev/null", &url(host)]);
    }
    let out = cmd.output().map_err(|e| err(format!("curl: {e}")))?;
    // curl exits non-zero when any transfer fails; the write-out still has every line
    Ok(parse_write_out(&String::from_utf8_lossy(&out.stdout)))
}

/// Writes atomically (temp file + rename), so the agent never reads half a file.
fn save(out: &Path, w: &Web) -> Result<()> {
    let tmp = out.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(w)?)?;
    std::fs::rename(&tmp, out)?;
    Ok(())
}

pub fn run() -> Result<()> {
    signals::install();
    let (cfg_path, out) = (checks::path(), checks::web_out());
    eprintln!(
        "pios web: checking {} every {} s → {}",
        cfg_path.display(),
        EVERY.as_secs(),
        out.display()
    );
    let mut prev: Option<Web> = None;
    let mut next = Instant::now();
    while !signals::stopping() {
        signals::sleep_until(next);
        if signals::stopping() {
            break;
        }
        next += EVERY;
        let cfg = checks::load(&cfg_path);
        if cfg.web.is_empty() {
            prev = None;
            let _ = std::fs::remove_file(&out);
            continue;
        }
        let got = match round(&cfg) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("pios web: {e}");
                vec![]
            }
        };
        let w = merge(&cfg, &got, prev.as_ref(), now_ms());
        for c in w.checks.iter().filter(|c| !c.ok && c.fails == 2) {
            eprintln!(
                "pios web: {} failing twice: {}",
                c.host,
                c.code
                    .map(|x| format!("HTTP {x}, want {}", c.expect))
                    .or(c.error.clone())
                    .unwrap_or_default()
            );
        }
        if let Err(e) = save(&out, &w) {
            eprintln!("pios web: {}: {e}", out.display());
        }
        prev = Some(w);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_round_trip() {
        for t in ["a.example.com", "a.example.com/status.php"] {
            let line = format!("{}\t200\t0.1\t\n", url(t));
            assert_eq!(parse_write_out(&line)[0].0, t);
        }
    }

    #[test]
    fn write_out_lines() {
        let t = "https://a.example.com/\t200\t0.123456\t\n\
                 https://b.example.com/\t000\t10.001\tOperation timed out after 10001 ms\n\
                 https://c.example.com/\t403\t0.5\t\n\
                 garbage\n";
        let v = parse_write_out(t);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], ("a.example.com".into(), Some(200), Some(123), None));
        assert_eq!(v[1].1, None);
        assert_eq!(v[1].2, None);
        assert!(v[1].3.as_deref().unwrap().contains("timed out"));
    }

    #[test]
    fn streaks_and_expected_codes() {
        let cfg = Config {
            host: vec![],
            web: vec![
                ("a.example.com".into(), 200),
                ("s3.example.com".into(), 403),
            ],
        };
        let bad = vec![
            (
                "a.example.com".to_string(),
                None,
                None,
                Some("timeout".to_string()),
            ),
            ("s3.example.com".to_string(), Some(403), Some(80), None),
        ];
        let w1 = merge(&cfg, &bad, None, 1);
        assert_eq!((w1.ok, w1.total), (1, 2));
        assert_eq!(w1.checks[0].fails, 1);
        assert!(w1.checks[1].ok, "403 is what s3 should answer");
        let w2 = merge(&cfg, &bad, Some(&w1), 2);
        assert_eq!(w2.checks[0].fails, 2);
        let good = vec![("a.example.com".to_string(), Some(200), Some(90), None)];
        let w3 = merge(&cfg, &good, Some(&w2), 3);
        assert_eq!(w3.checks[0].fails, 0);
        assert_eq!(w3.checks[1].error.as_deref(), Some("not checked"));
    }
}
