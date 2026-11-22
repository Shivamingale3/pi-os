//! `pios update vX.Y.Z` / `pios update --rollback` (root), and `pios deploy-ssh`, the forced
//! command of the Jenkins deploy key.
//!
//! update: download the release with curl → check SHA256SUMS → check `version` → keep the current
//! binary as pios.prev → swap atomically → `pios setup` (units may change) → health check
//! (/now answers within 15 s) → otherwise roll back on its own.
//! Downloads go to a root-only directory, so the unprivileged agent can't swap a file between
//! the checksum check and the install.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::error::{Result, err};
use crate::setup::require_root;

const REPO: &str = "Shivamingale3/pi-os";
const ASSET: &str = "pios-aarch64-linux-musl";
pub const BIN: &str = "/usr/local/bin/pios";
pub const PREV: &str = "/usr/local/bin/pios.prev";
pub const CACHE: &str = "/var/cache/pios-update";
const API_SOCKET: &str = "/run/pios/pios.sock";
const HEALTH_TIMEOUT: Duration = Duration::from_secs(15);

/// Exactly `v<digits>.<digits>.<digits>`, nothing else (it ends up in a URL and a command line).
pub fn is_tag(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<&str> = rest.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The hash for `name` in a SHA256SUMS file ("<hash>  <name>" or "<hash> *<name>").
pub fn expected_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hash, file) = l.split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

fn version_of(bin: &str) -> Option<String> {
    let out = Command::new(bin).arg("version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim().strip_prefix("pios ").map(str::to_string)
}

fn download(url: &str, out: &Path) -> Result<()> {
    // GitHub may still be attaching assets right after "published": retry for about a minute.
    let status = Command::new("curl")
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--retry",
            "6",
            "--retry-delay",
            "10",
            "--retry-all-errors",
            "--max-time",
            "120",
            "-o",
        ])
        .arg(out)
        .arg(url)
        .status()
        .map_err(|e| err(format!("curl: {e}")))?;
    if !status.success() {
        return Err(err(format!("download failed: {url}")));
    }
    Ok(())
}

fn sha256(path: &Path) -> Result<String> {
    let out = Command::new("sha256sum").arg(path).output()?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| err("sha256sum gave no output"))
}

/// Waits for the agent's first snapshot; returns a one-line summary of it, or None on timeout.
fn healthy() -> Option<String> {
    let start = Instant::now();
    while start.elapsed() < HEALTH_TIMEOUT {
        if let Ok((200, body)) =
            crate::http::get(Path::new(API_SOCKET), "/now", Duration::from_secs(2))
        {
            return Some(summary(&body));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    None
}

/// "cpu 2% · 41 °C · ram 19% · disk 11% · docker 9/9" from a /now body (unknown parts left out).
pub fn summary(now_json: &str) -> String {
    let Ok(s) = serde_json::from_str::<crate::types::Snapshot>(now_json) else {
        return "agent answered".into();
    };
    let mut parts = vec![format!("cpu {:.0}%", s.cpu.percent)];
    if let Some(t) = s.cpu.temp_c {
        parts.push(format!("{t:.0} °C"));
    }
    parts.push(format!("ram {:.0}%", s.memory.used_percent));
    parts.push(format!("disk {:.0}%", s.disk.used_percent));
    parts.push(match &s.docker {
        Some(d) => format!("docker {}/{}", d.running, d.total),
        None => "docker unreachable".into(),
    });
    parts.join(" · ")
}

/// Puts `src` at BIN atomically (copy beside it, then rename over it).
fn place(src: &str) -> Result<()> {
    let tmp = format!("{BIN}.new");
    fs::copy(src, &tmp)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
    fs::rename(&tmp, BIN)?;
    Ok(())
}

fn setup_with(bin: &str) -> Result<()> {
    let status = Command::new(bin).arg("setup").status()?;
    if !status.success() {
        return Err(err(format!("{bin} setup failed")));
    }
    Ok(())
}

pub fn update(arg: &str) -> Result<()> {
    require_root()?;
    if arg == "--rollback" {
        return rollback();
    }
    if !is_tag(arg) {
        return Err(err("usage: pios update vX.Y.Z | --rollback"));
    }
    let want = &arg[1..];
    if version_of(BIN).as_deref() == Some(want) {
        println!("already at {arg}");
        return Ok(());
    }

    fs::create_dir_all(CACHE)?;
    fs::set_permissions(CACHE, fs::Permissions::from_mode(0o700))?;
    let dir = Path::new(CACHE).join(arg);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir(&dir)?;
    let base = format!("https://github.com/{REPO}/releases/download/{arg}");
    let (bin, sums) = (dir.join(ASSET), dir.join("SHA256SUMS"));
    println!("downloading {arg}");
    download(&format!("{base}/{ASSET}"), &bin)?;
    download(&format!("{base}/SHA256SUMS"), &sums)?;

    let want_hash = expected_hash(&fs::read_to_string(&sums)?, ASSET)
        .ok_or_else(|| err("SHA256SUMS has no entry for the binary"))?;
    let got_hash = sha256(&bin)?;
    if got_hash != want_hash {
        return Err(err(format!(
            "checksum mismatch: expected {want_hash}, got {got_hash}; nothing installed"
        )));
    }
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755))?;
    let bin_s = bin.to_string_lossy().to_string();
    match version_of(&bin_s) {
        Some(v) if v == want => {}
        other => {
            return Err(err(format!(
                "the downloaded binary reports {other:?}, expected {want}; nothing installed"
            )));
        }
    }
    println!("checksum and version OK");

    if Path::new(BIN).exists() {
        fs::copy(BIN, PREV)?;
    }
    place(&bin_s)?;
    println!("installed {arg}; previous kept as {PREV}");

    let check = setup_with(BIN)
        .and_then(|_| healthy().ok_or_else(|| err("the agent didn't answer /now within 15 s")));
    let health = match check {
        Ok(h) => h,
        Err(e) => {
            eprintln!("update failed: {e}; rolling back");
            rollback()?;
            return Err(err(format!(
                "update to {arg} failed and was rolled back: {e}"
            )));
        }
    };
    let _ = fs::remove_dir_all(&dir);
    println!("updated to {arg}, agent healthy: {health}");
    Ok(())
}

fn rollback() -> Result<()> {
    if !Path::new(PREV).exists() {
        return Err(err(format!(
            "nothing to roll back to ({PREV} doesn't exist)"
        )));
    }
    let failed = format!("{BIN}.failed");
    let _ = fs::copy(BIN, &failed);
    place(PREV)?;
    setup_with(BIN)?;
    let v = version_of(BIN).unwrap_or_else(|| "?".into());
    let Some(health) = healthy() else {
        return Err(err(format!(
            "rolled back to {v}, but the agent still doesn't answer"
        )));
    };
    println!("rolled back to {v}, agent healthy: {health} (the replaced binary is at {failed})");
    Ok(())
}

/// Forced command for the deploy key: accepts "update vX.Y.Z" or "rollback" in
/// SSH_ORIGINAL_COMMAND and runs the matching `sudo pios update …`. Anything else is refused.
pub fn deploy_ssh() -> Result<()> {
    let cmd = std::env::var("SSH_ORIGINAL_COMMAND").unwrap_or_default();
    let words: Vec<&str> = cmd.split_whitespace().collect();
    let arg = match words.as_slice() {
        ["update", tag] if is_tag(tag) => *tag,
        ["rollback"] => "--rollback",
        _ => {
            return Err(err(
                "refused: allowed commands are `update vX.Y.Z` and `rollback`",
            ));
        }
    };
    use std::os::unix::process::CommandExt;
    let e = Command::new("sudo").args(["-n", BIN, "update", arg]).exec();
    Err(err(format!("sudo: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags() {
        for ok in ["v0.2.0", "v10.0.123", "v1.2.3"] {
            assert!(is_tag(ok), "{ok}");
        }
        for bad in [
            "0.2.0",
            "v0.2",
            "v0.2.0-rc1",
            "v0.2.0;rm",
            "v0..1",
            "v1.2.3 ",
            "latest",
            "v1.2.3.4",
            "v1234567.0.0",
            "",
        ] {
            assert!(!is_tag(bad), "{bad}");
        }
    }

    #[test]
    fn health_summary() {
        let s = crate::rules::tests::snap(0, 2.4);
        assert_eq!(
            summary(&serde_json::to_string(&s).unwrap()),
            "cpu 2% · 40 °C · ram 10% · disk 10% · docker unreachable"
        );
        assert_eq!(summary("not json"), "agent answered");
    }

    #[test]
    fn sums() {
        let h = "a".repeat(64);
        let text = format!("{h}  pios-aarch64-linux-musl\n{}  other\n", "b".repeat(64));
        assert_eq!(expected_hash(&text, ASSET), Some(h.clone()));
        assert_eq!(
            expected_hash(&format!("{h} *pios-aarch64-linux-musl"), ASSET),
            Some(h)
        );
        assert_eq!(expected_hash("short  pios-aarch64-linux-musl", ASSET), None);
        assert_eq!(expected_hash(&text, "missing"), None);
    }
}
