//! Manual fan control from the screen, through fan_ctl ≥ 0.2 (github.com/Shivamingale3/fan_ctl).
//!
//! fan_ctl reads `/run/fan_ctl/override` (root-only) about once a second and keeps its own
//! safety rules over it (too hot or a sensor error: case fan 100 %, cooler back to the kernel).
//! PiOS only ever edits that file:
//!
//! ```text
//!  screen ──POST /fans/case?duty=75&for=3600──► agent (checks, ≤ 1 change / 300 ms per fan)
//!         ──POST /fan_ctl/case?duty=75&until=T──► helper (root): read, edit one fan, rename into place
//! ```
//!
//! Paths: `/fans/case?duty=0..100[&for=S]`, `/fans/cooler?level=0..10[&for=S]`,
//! `/fans/case/auto`, `/fans/cooler/auto`. Without `for` it lasts until fan_ctl stops.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// fan_ctl's runtime directory (override + status).
pub const FAN_CTL_DIR: &str = "/run/fan_ctl";
/// Longest manual period accepted from the screen.
pub const MAX_FOR_S: u64 = 7 * 86_400;
/// Highest cooler level fan_ctl accepts (it clamps to the cooler's own max).
pub const MAX_LEVEL: u32 = 10;
/// Fast taps on − / + are debounced by the screen; this is the agent's own floor.
const MIN_EVERY: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Case,
    Cooler,
}

impl Which {
    fn name(self) -> &'static str {
        match self {
            Which::Case => "case",
            Which::Cooler => "cooler",
        }
    }
    /// (value key, end-time key) in fan_ctl's override file
    fn keys(self) -> (&'static str, &'static str) {
        match self {
            Which::Case => ("duty", "duty_until"),
            Which::Cooler => ("cooler", "cooler_until"),
        }
    }
    fn max(self) -> u32 {
        match self {
            Which::Case => 100,
            Which::Cooler => MAX_LEVEL,
        }
    }
}

/// One change for one fan: a manual value (until a unix time, or None = no end), or auto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    pub which: Which,
    pub set: Option<(u32, Option<u64>)>,
}

fn query<'a>(q: &'a str, name: &str) -> Option<&'a str> {
    q.split('&').find_map(|kv| {
        kv.split_once('=')
            .filter(|(k, _)| *k == name)
            .map(|(_, v)| v)
    })
}

/// A strict whole number: digits only (no sign, no spaces), at most 10 of them.
fn number(s: &str) -> Option<u64> {
    (!s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// A parsed path: which fan, and a manual (value, end) or None for auto.
type Parsed = (Which, Option<(u32, Option<u64>)>);

/// Parses `{prefix}/case?…`, `{prefix}/cooler?…`, `{prefix}/case/auto`, `{prefix}/cooler/auto`.
/// `time` is the name of the end parameter (`for` on the agent, `until` on the helper).
fn parse(target: &str, prefix: &str, time: &str) -> Option<Parsed> {
    let rest = target.strip_prefix(prefix)?.strip_prefix('/')?;
    let (path, q) = rest.split_once('?').unwrap_or((rest, ""));
    let (which, auto) = match path {
        "case" => (Which::Case, false),
        "cooler" => (Which::Cooler, false),
        "case/auto" => (Which::Case, true),
        "cooler/auto" => (Which::Cooler, true),
        _ => return None,
    };
    if auto {
        return q.is_empty().then_some((which, None));
    }
    let key = if which == Which::Case {
        "duty"
    } else {
        "level"
    };
    // only the two known parameters, each once
    let allowed = q.split('&').all(|kv| {
        kv.split_once('=').is_some_and(|(k, _)| {
            (k == key || k == time) && q.matches(&format!("{k}=")).count() == 1
        })
    });
    if !allowed {
        return None;
    }
    let value = number(query(q, key)?).filter(|v| *v <= u64::from(which.max()))? as u32;
    let end = match query(q, time) {
        Some(t) => Some(number(t)?),
        None => None,
    };
    Some((which, Some((value, end))))
}

/// Agent side: `/fans/…?duty=N&for=S` → a change ending `for` seconds after `now`.
pub fn parse_request(target: &str, now: u64) -> Option<Change> {
    let (which, set) = parse(target, "/fans", "for")?;
    let set = match set {
        Some((v, Some(secs))) if (60..=MAX_FOR_S).contains(&secs) => Some((v, Some(now + secs))),
        Some((_, Some(_))) => return None,
        other => other,
    };
    Some(Change { which, set })
}

/// The helper's path for a change (rebuilt by the agent, never forwarded from the screen).
pub fn helper_path(c: &Change) -> String {
    let w = c.which.name();
    match c.set {
        None => format!("/fan_ctl/{w}/auto"),
        Some((v, until)) => {
            let key = if c.which == Which::Case {
                "duty"
            } else {
                "level"
            };
            format!("/fan_ctl/{w}?{key}={v}&until={}", until.unwrap_or(0))
        }
    }
}

/// Helper side: `/fan_ctl/…?duty=N&until=T` (T = unix s, 0 = no end).
pub fn parse_helper(target: &str) -> Option<Change> {
    let (which, set) = parse(target, "/fan_ctl", "until")?;
    let set = match set {
        Some((v, Some(until))) => Some((v, (until > 0).then_some(until))),
        Some((_, None)) => return None, // the agent always sends `until`
        None => None,
    };
    Some(Change { which, set })
}

/// Applies a change to fan_ctl's override text, keeping the other fan's lines.
/// Unknown or malformed lines are dropped (fan_ctl would reject the whole file for them).
pub fn edit(current: &str, c: &Change) -> String {
    const KEYS: [&str; 4] = ["duty", "duty_until", "cooler", "cooler_until"];
    let mut kv: BTreeMap<&str, u64> = BTreeMap::new();
    for line in current.lines() {
        let line = line.split('#').next().unwrap_or("");
        if let Some((k, v)) = line.split_once('=')
            && let (k, Some(v)) = (k.trim(), number(v.trim()))
            && let Some(k) = KEYS.iter().find(|x| **x == k)
        {
            kv.insert(k, v);
        }
    }
    let (vk, uk) = c.which.keys();
    kv.remove(vk);
    kv.remove(uk);
    if let Some((v, until)) = c.set {
        kv.insert(vk, u64::from(v));
        kv.insert(uk, until.unwrap_or(0));
    }
    // a lone end time is an error for fan_ctl
    for (v, u) in [("duty", "duty_until"), ("cooler", "cooler_until")] {
        if !kv.contains_key(v) {
            kv.remove(u);
        }
    }
    KEYS.iter()
        .filter_map(|k| kv.get(k).map(|v| format!("{k} = {v}\n")))
        .collect()
}

/// Helper side: writes the change into `dir/override` (atomically, 0644), or removes the
/// file when no fan is manual any more.
pub fn write(dir: &Path, c: &Change) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if !dir.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "fan_ctl isn't running (no /run/fan_ctl)",
        ));
    }
    let path = dir.join("override");
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    let next = edit(&current, c);
    if next.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let tmp = dir.join(".override.pios.tmp");
    std::fs::write(&tmp, next)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
    std::fs::rename(&tmp, path)
}

pub fn dir() -> PathBuf {
    std::env::var_os("PIOS_FAN_CTL_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(FAN_CTL_DIR))
}

static LAST: Mutex<[Option<Instant>; 2]> = Mutex::new([None, None]);

/// Agent side: checks and rate-limits a screen request, then asks the helper.
pub fn ask(helper: &Path, target: &str) -> (u16, String) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let Some(c) = parse_request(target, now) else {
        return (400, "bad fan request".into());
    };
    {
        let Ok(mut last) = LAST.lock() else {
            return (500, "busy".into());
        };
        let slot = &mut last[c.which as usize];
        if slot.is_some_and(|t| t.elapsed() < MIN_EVERY) {
            return (429, "too fast".into());
        }
        *slot = Some(Instant::now());
    }
    let what = match c.set {
        None => "auto".to_string(),
        Some((v, until)) => format!(
            "{v}{} {}",
            if c.which == Which::Case { " %" } else { "" },
            until.map_or("until fan_ctl stops".into(), |u| format!(
                "for {} s",
                u - now
            ))
        ),
    };
    eprintln!("pios: {} fan {what} asked from the screen", c.which.name());
    match crate::http::post(helper, &helper_path(&c), Duration::from_secs(3)) {
        Ok((200, _)) => (200, "ok".into()),
        Ok((status, body)) => (
            status,
            serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("error")?.as_str().map(String::from))
                .unwrap_or_else(|| format!("helper: HTTP {status}")),
        ),
        Err(e) => (502, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_795_711_297;

    #[test]
    fn screen_requests() {
        let set = |which, v, until| {
            Some(Change {
                which,
                set: Some((v, until)),
            })
        };
        assert_eq!(
            parse_request("/fans/case?duty=75&for=3600", NOW),
            set(Which::Case, 75, Some(NOW + 3600))
        );
        assert_eq!(
            parse_request("/fans/case?for=1800&duty=0", NOW),
            set(Which::Case, 0, Some(NOW + 1800))
        );
        assert_eq!(
            parse_request("/fans/cooler?level=2", NOW),
            set(Which::Cooler, 2, None)
        );
        assert_eq!(
            parse_request("/fans/cooler/auto", NOW),
            Some(Change {
                which: Which::Cooler,
                set: None
            })
        );
        for bad in [
            "/fans/case?duty=101&for=3600",
            "/fans/case?duty=-1",
            "/fans/case?duty=+5",
            "/fans/case?duty=",
            "/fans/case?duty=5x",
            "/fans/case",
            "/fans/case?level=2",
            "/fans/cooler?level=11",
            "/fans/cooler?duty=2",
            "/fans/case?duty=50&for=59",
            "/fans/case?duty=50&for=604801",
            "/fans/case?duty=50&duty=60",
            "/fans/case?duty=50&x=1",
            "/fans/case/auto?duty=5",
            "/fans/both?duty=5",
            "/fans/case/../cooler?level=1",
            "/fanscase?duty=5",
            "/fan_ctl/case?duty=5&until=0",
        ] {
            assert_eq!(parse_request(bad, NOW), None, "{bad}");
        }
    }

    #[test]
    fn agent_and_helper_agree() {
        for c in [
            Change {
                which: Which::Case,
                set: Some((75, Some(NOW + 60))),
            },
            Change {
                which: Which::Case,
                set: Some((0, None)),
            },
            Change {
                which: Which::Cooler,
                set: Some((4, Some(NOW))),
            },
            Change {
                which: Which::Cooler,
                set: None,
            },
            Change {
                which: Which::Case,
                set: None,
            },
        ] {
            let p = helper_path(&c);
            assert_eq!(parse_helper(&p), Some(c), "{p}");
        }
        assert_eq!(
            helper_path(&Change {
                which: Which::Case,
                set: Some((80, None))
            }),
            "/fan_ctl/case?duty=80&until=0"
        );
        assert_eq!(
            parse_helper("/fan_ctl/case?duty=80"),
            None,
            "until is required"
        );
        assert_eq!(parse_helper("/fans/case?duty=80&until=0"), None);
    }

    #[test]
    fn edit_keeps_the_other_fan() {
        let case = |v, u| Change {
            which: Which::Case,
            set: Some((v, u)),
        };
        let s = edit("", &case(75, Some(9)));
        assert_eq!(s, "duty = 75\nduty_until = 9\n");
        let s = edit(
            &s,
            &Change {
                which: Which::Cooler,
                set: Some((2, None)),
            },
        );
        assert_eq!(
            s,
            "duty = 75\nduty_until = 9\ncooler = 2\ncooler_until = 0\n"
        );
        let s = edit(&s, &case(60, None));
        assert_eq!(
            s,
            "duty = 60\nduty_until = 0\ncooler = 2\ncooler_until = 0\n"
        );
        let s = edit(
            &s,
            &Change {
                which: Which::Case,
                set: None,
            },
        );
        assert_eq!(s, "cooler = 2\ncooler_until = 0\n");
        assert_eq!(
            edit(
                &s,
                &Change {
                    which: Which::Cooler,
                    set: None
                }
            ),
            ""
        );
        // junk and lone end times are dropped
        assert_eq!(
            edit(
                "bogus\nduty = lots\nduty_until = 5\nspeed = 3 # x\n",
                &Change {
                    which: Which::Cooler,
                    set: Some((1, None))
                }
            ),
            "cooler = 1\ncooler_until = 0\n"
        );
    }

    #[test]
    fn write_and_remove() {
        let dir = std::env::temp_dir().join(format!("pios-fans-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            write(
                &dir,
                &Change {
                    which: Which::Case,
                    set: None
                }
            )
            .is_err(),
            "no fan_ctl"
        );
        std::fs::create_dir_all(&dir).unwrap();
        write(
            &dir,
            &Change {
                which: Which::Cooler,
                set: Some((3, Some(NOW))),
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("override")).unwrap(),
            format!("cooler = 3\ncooler_until = {NOW}\n")
        );
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("override"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o644);
        write(
            &dir,
            &Change {
                which: Which::Cooler,
                set: None,
            },
        )
        .unwrap();
        assert!(!dir.join("override").exists());
        write(
            &dir,
            &Change {
                which: Which::Cooler,
                set: None,
            },
        )
        .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
