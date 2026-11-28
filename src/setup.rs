//! `pios setup` / `pios uninstall` (root). The units and the sudoers line ship inside the binary,
//! so a release always installs the files that match it. Everything created here is listed by
//! `uninstall`, which removes it again.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use crate::error::{Result, err};

/// Installed and enabled at boot.
pub const UNITS: [(&str, &str); 4] = [
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
    (
        "pios-web.service",
        include_str!("../deploy/pios-web.service"),
    ),
];
/// Host services and hostnames to watch: the real file is the user's, never shipped (the repo
/// is public); setup only refreshes the commented example next to it.
const CHECKS_DIR: &str = "/etc/pios";
const CHECKS_EXAMPLE: &str = include_str!("../deploy/checks.example");
/// Installed but never enabled or disabled by setup: the user decides when PiOS takes the panel.
pub const UI_UNIT: (&str, &str) = ("pios-ui.service", include_str!("../deploy/pios-ui.service"));
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
    if !user_exists("pios-ui") {
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
                "pios-ui",
            ],
        )?;
        println!("  created user pios-ui");
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
    for (name, body) in UNITS.iter().chain([&UI_UNIT]) {
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

    fs::create_dir_all(CHECKS_DIR)?;
    fs::set_permissions(CHECKS_DIR, fs::Permissions::from_mode(0o755))?;
    // the Telegram bot token (pios-web's credential): an empty, root-only file until you fill it
    let tg = format!("{CHECKS_DIR}/telegram");
    if !Path::new(&tg).exists() {
        fs::write(
            &tg,
            "# Telegram bot token on line 1 (from @BotFather); optional chat ID on line 2\n",
        )?;
        fs::set_permissions(&tg, fs::Permissions::from_mode(0o600))?;
        println!("  created {tg} (empty: phone alerts are off until you add a bot token)");
    }
    write_if_changed(
        &format!("{CHECKS_DIR}/checks.example"),
        CHECKS_EXAMPLE,
        0o644,
    )?;

    if units_changed {
        run("systemctl", &["daemon-reload"])?;
    }
    run(
        "systemctl",
        &["enable", "--quiet", "--now", "pios-helper.socket"],
    )?;
    run("systemctl", &["enable", "--quiet", "pios-agent.service"])?;
    run("systemctl", &["enable", "--quiet", "pios-web.service"])?;
    run("systemctl", &["restart", "pios-web.service"])?;
    run("systemctl", &["try-restart", "pios-helper.service"])?;
    run("systemctl", &["restart", "pios-agent.service"])?;
    println!("  pios-agent restarted");
    // the screen only restarts if it's already running (it's never started from here)
    run("systemctl", &["try-restart", "pios-ui.service"])?;
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
            "pios-ui.service",
            "pios-web.service",
            "pios-agent.service",
            "pios-helper.socket",
        ],
    );
    let _ = run("systemctl", &["stop", "pios-helper.service"]);
    for (name, _) in UNITS.iter().chain([&UI_UNIT]) {
        remove(&format!("{UNIT_DIR}/{name}"));
    }
    let _ = run("systemctl", &["daemon-reload"]);
    remove(SUDOERS_PATH);
    remove(&format!("{CHECKS_DIR}/checks.example"));
    if !keep_data {
        remove(&format!("{CHECKS_DIR}/checks"));
        remove(&format!("{CHECKS_DIR}/telegram"));
    }
    let _ = fs::remove_dir(CHECKS_DIR); // only if empty
    if user_exists("pios-deploy") {
        let _ = run("userdel", &["--remove", "pios-deploy"]); // also removes its home and key
        println!("  removed user pios-deploy");
    }
    if user_exists("pios-ui") {
        let _ = run("userdel", &["pios-ui"]);
        println!("  removed user pios-ui");
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
    remove(&format!("{}.failed", crate::update::BIN));
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
            "SupplementaryGroups=systemd-journal",
            "NoNewPrivileges=yes",
            "ProtectSystem=strict",
            "MemoryHigh=24M",
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
            "MemoryMax=24M",
            // one capability (smartctl), and only sd disks, read-only
            "CapabilityBoundingSet=CAP_SYS_RAWIO",
            "DevicePolicy=closed",
            "DeviceAllow=block-sd r",
            "ProtectSystem=strict",
        ] {
            assert!(
                helper.lines().any(|l| l == line),
                "helper unit lacks {line}"
            );
        }
        assert!(UNITS[1].1.lines().any(|l| l == "SocketMode=0660"));
        // the only unit with network: its own throwaway user, nothing else
        let web = UNITS[3].1;
        for line in [
            "DynamicUser=yes",
            "NoNewPrivileges=yes",
            "CapabilityBoundingSet=",
            "ProtectSystem=strict",
            "MemoryMax=48M",
            "CPUQuota=10%",
            "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK",
            "LoadCredential=telegram:/etc/pios/telegram",
            "SupplementaryGroups=pios",
        ] {
            assert!(web.lines().any(|l| l == line), "web unit lacks {line}");
        }
        assert!(
            !agent.contains("AF_INET"),
            "the agent stays off the network"
        );
        // the example only: real hostnames never ship
        assert!(
            CHECKS_EXAMPLE
                .lines()
                .all(|l| l.starts_with('#') || l.trim().is_empty())
        );
        let ui = UI_UNIT.1;
        for line in [
            "User=pios-ui",
            "Environment=PIOS_FB_SWAP_RB=1",
            "Environment=PIOS_RING_DEVICE=1f00098000.pwm",
            "Conflicts=tft-btop.service case-ring-breathe.service",
            "MemoryMax=60M",
            "CPUQuota=10%",
            "DevicePolicy=closed",
            "DeviceAllow=/dev/fb0 rw",
            "PrivateNetwork=yes",
            "CapabilityBoundingSet=",
        ] {
            assert!(ui.lines().any(|l| l == line), "ui unit lacks {line}");
        }
        assert!(ui.lines().any(|l| l == "WantedBy=multi-user.target"));
        // setup leaves the screen's boot state to the user
        let src = include_str!("setup.rs");
        let setup = &src[src.find("pub fn setup").unwrap()..src.find("pub fn uninstall").unwrap()];
        assert!(!setup.contains("\"enable\", \"--quiet\", \"pios-ui"));
        assert!(setup.contains("\"try-restart\", \"pios-ui.service\""));
        assert!(
            SUDOERS
                .lines()
                .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
                .eq(["pios-deploy ALL=(root) NOPASSWD: /usr/local/bin/pios update *"])
        );
    }
}
