//! `pios setup` / `pios uninstall` (root). The units and the sudoers line ship inside the binary,
//! so a release always installs the files that match it. Everything created here is listed by
//! `uninstall`, which removes it again.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use crate::error::{Result, err};

pub const UNITS: [(&str, &str); 3] = [
    (
        "pios-agent.service",
        include_str!("../deploy/pios-agent.service"),
    ),
    (
        "pios-helper.socket",
        include_str!("../deploy/pios-helper.socket"),
    ),
    (
        "pios-helper.service",
        include_str!("../deploy/pios-helper.service"),
    ),
];
const SUDOERS: &str = include_str!("../deploy/sudoers-pios-deploy");

const UNIT_DIR: &str = "/etc/systemd/system";
const SUDOERS_PATH: &str = "/etc/sudoers.d/pios-deploy";
const DEPLOY_HOME: &str = "/var/lib/pios-deploy";
/// The deploy key can run this and nothing else (no shell, no forwarding, no pty).
const KEY_OPTIONS: &str = r#"restrict,command="/usr/local/bin/pios deploy-ssh""#;

pub fn require_root() -> Result<()> {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return Err(err("this command must run as root (sudo)"));
    }
    Ok(())
}

fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .map_err(|e| err(format!("{cmd}: {e}")))?;
    if !status.success() {
        return Err(err(format!("{cmd} {} failed ({status})", args.join(" "))));
    }
    Ok(())
}

fn succeeds(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn user_exists(name: &str) -> bool {
    succeeds("getent", &["passwd", name])
}

fn id_of(flag: &str, user: &str) -> Result<u32> {
    let out = Command::new("id").args([flag, user]).output()?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|_| err(format!("id {flag} {user}: no such user")))
}

/// Writes `body` to `path` (via a temp file + rename) only when it differs. Returns true if written.
fn write_if_changed(path: &str, body: &str, mode: u32) -> Result<bool> {
    if fs::read_to_string(path).is_ok_and(|old| old == body) {
        return Ok(false);
    }
    let tmp = format!("{path}.pios-tmp");
    fs::write(&tmp, body)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    fs::rename(&tmp, path)?;
    println!("  wrote {path}");
    Ok(true)
}

/// "ssh-ed25519 AAAA… comment" → the key without options, or an error.
pub fn validate_public_key(text: &str) -> Result<String> {
    let line = text.trim();
    let ok_type = [
        "ssh-ed25519 ",
        "ecdsa-sha2-nistp256 ",
        "ecdsa-sha2-nistp384 ",
        "ecdsa-sha2-nistp521 ",
        "ssh-rsa ",
    ]
    .iter()
    .any(|p| line.starts_with(p));
    if !ok_type || line.lines().count() != 1 || line.contains('"') {
        return Err(err(
            "the deploy key must be one public key line (ssh-ed25519 …)",
        ));
    }
    Ok(line.to_string())
}

pub fn setup(deploy_key: Option<&Path>) -> Result<()> {
    require_root()?;
    println!("pios setup {}", env!("CARGO_PKG_VERSION"));

    if !user_exists("pios") {
        run(
            "useradd",
            &[
                "--system",
                "--user-group",
                "--no-create-home",
                "--home-dir",
                "/nonexistent",
                "--shell",
                "/usr/sbin/nologin",
                "pios",
            ],
        )?;
        println!("  created user pios");
    }
    if !user_exists("pios-deploy") {
        run(
            "useradd",
            &[
                "--system",
                "--user-group",
                "--create-home",
                "--home-dir",
                DEPLOY_HOME,
                "--shell",
                "/bin/sh",
                "pios-deploy",
            ],
        )?;
        println!("  created user pios-deploy");
    }

    let mut units_changed = false;
    for (name, body) in UNITS {
        units_changed |= write_if_changed(&format!("{UNIT_DIR}/{name}"), body, 0o644)?;
    }

    // sudoers: validate before it goes live (a broken sudoers file would lock out sudo)
    if fs::read_to_string(SUDOERS_PATH).map_or(true, |old| old != SUDOERS) {
        let tmp = "/etc/sudoers.d/.pios-deploy.tmp"; // a '.' in the name: sudo ignores it
        fs::write(tmp, SUDOERS)?;
        fs::set_permissions(tmp, fs::Permissions::from_mode(0o440))?;
        if let Err(e) = run("visudo", &["-cq", "-f", tmp]) {
            let _ = fs::remove_file(tmp);
            return Err(err(format!("sudoers check failed, nothing installed: {e}")));
        }
        fs::rename(tmp, SUDOERS_PATH)?;
        println!("  wrote {SUDOERS_PATH}");
    }

    if let Some(key_path) = deploy_key {
        let key = validate_public_key(&fs::read_to_string(key_path)?)?;
        let (uid, gid) = (id_of("-u", "pios-deploy")?, id_of("-g", "pios-deploy")?);
        let ssh = format!("{DEPLOY_HOME}/.ssh");
        fs::create_dir_all(&ssh)?;
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700))?;
        std::os::unix::fs::chown(&ssh, Some(uid), Some(gid))?;
        let keys = format!("{ssh}/authorized_keys");
        write_if_changed(&keys, &format!("{KEY_OPTIONS} {key}\n"), 0o600)?;
        std::os::unix::fs::chown(&keys, Some(uid), Some(gid))?;
    }

    if units_changed {
        run("systemctl", &["daemon-reload"])?;
    }
    run(
        "systemctl",
        &["enable", "--quiet", "--now", "pios-helper.socket"],
    )?;
    run("systemctl", &["enable", "--quiet", "pios-agent.service"])?;
    run("systemctl", &["try-restart", "pios-helper.service"])?;
    run("systemctl", &["restart", "pios-agent.service"])?;
    println!("  pios-agent restarted");
    Ok(())
}

pub fn uninstall(keep_data: bool) -> Result<()> {
    require_root()?;
    let _ = run(
        "systemctl",
        &[
            "disable",
            "--quiet",
            "--now",
            "pios-agent.service",
            "pios-helper.socket",
        ],
    );
    let _ = run("systemctl", &["stop", "pios-helper.service"]);
    for (name, _) in UNITS {
        remove(&format!("{UNIT_DIR}/{name}"));
    }
    let _ = run("systemctl", &["daemon-reload"]);
    remove(SUDOERS_PATH);
    if user_exists("pios-deploy") {
        let _ = run("userdel", &["--remove", "pios-deploy"]); // also removes its home and key
        println!("  removed user pios-deploy");
    }
    remove_dir(crate::update::CACHE);
    if !keep_data {
        remove_dir("/var/lib/pios");
        if user_exists("pios") {
            let _ = run("userdel", &["pios"]);
            println!("  removed user pios");
        }
    }
    remove(crate::update::PREV);
    remove(crate::update::BIN);
    println!(
        "PiOS removed{}",
        if keep_data {
            " (data in /var/lib/pios and user pios kept)"
        } else {
            ""
        }
    );
    Ok(())
}

fn remove(path: &str) {
    if fs::remove_file(path).is_ok() {
        println!("  removed {path}");
    }
}

fn remove_dir(path: &str) {
    if fs::remove_dir_all(path).is_ok() {
        println!("  removed {path}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deploy_key_validation() {
        assert!(validate_public_key("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIabc jenkins\n").is_ok());
        assert!(validate_public_key("command=\"sh\" ssh-ed25519 AAAA").is_err());
        assert!(validate_public_key("ssh-ed25519 AAAA\nssh-ed25519 BBBB").is_err());
        assert!(validate_public_key("-----BEGIN OPENSSH PRIVATE KEY-----").is_err());
        assert!(validate_public_key("").is_err());
    }

    #[test]
    fn units_are_sandboxed_and_capped() {
        let agent = UNITS[0].1;
        for line in [
            "User=pios",
            "NoNewPrivileges=yes",
            "ProtectSystem=strict",
            "MemoryMax=80M",
            "CPUQuota=15%",
            "RestrictAddressFamilies=AF_UNIX",
            "CapabilityBoundingSet=",
        ] {
            assert!(agent.lines().any(|l| l == line), "agent unit lacks {line}");
        }
        let helper = UNITS[2].1;
        for line in [
            "PrivateNetwork=yes",
            "MemoryMax=16M",
            "CapabilityBoundingSet=",
            "ProtectSystem=strict",
        ] {
            assert!(
                helper.lines().any(|l| l == line),
                "helper unit lacks {line}"
            );
        }
        assert!(UNITS[1].1.lines().any(|l| l == "SocketMode=0660"));
        assert!(
            SUDOERS
                .lines()
                .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
                .eq(["pios-deploy ALL=(root) NOPASSWD: /usr/local/bin/pios update *"])
        );
    }
}
