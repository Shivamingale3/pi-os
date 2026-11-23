//! `pios ui --fb …`: the screens on the real panel.
//!
//! The loop sleeps in poll() on the touch device until something is due (a touch, the clock,
//! the next fetch for the open screen, the end of a press highlight), so it costs nothing while
//! nothing happens. It redraws only when the layout changes, and sends only changed rows (fb.rs).
//! A tap brightens its target for a moment, then opens where it points. A hold target (Restart)
//! ignores taps: it fires after HOLD of pressing still, and lifting or sliding away cancels it.
//! Backlight: off after `idle` without a touch (and back to Home); the first touch on a dark
//! screen only wakes it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::app::{App, Screen};
use super::canvas::{Canvas, Fonts, rounded};
use super::fb::Framebuffer;
use super::layout::{self, Nav, Paint, Tree};
use super::theme;
use super::touch::{Calibration, Event, Taps, Touch};
use crate::error::Result;
use crate::signals;
use crate::types::now_ms;

/// While the screen is dark, read the agent rarely (nothing is shown).
/// (10 s, not more: the LED ring still shows the status while the screen is off)
pub const FETCH_EVERY_DARK: Duration = Duration::from_secs(10);
pub const PRESS_SHOWN: Duration = Duration::from_millis(150);
pub const HOLD: Duration = Duration::from_secs(2);
/// While a hold is in progress, redraw its bar this often.
const HOLD_FRAME: Duration = Duration::from_millis(50);

/// A press on a hold target, until it fires or is cancelled.
#[derive(Debug, Clone, Copy)]
pub struct Holding {
    pub at: Instant,
    pub from: (f32, f32),
    pub nav: Nav,
}

impl Holding {
    /// A press at (x, y) starts one if it lands on a hold target.
    pub fn start(tree: &Tree, x: f32, y: f32, now: Instant) -> Option<Holding> {
        let i = hit(tree, x, y)?;
        let it = &tree.items[i];
        (it.hold && it.nav.is_some()).then(|| Holding {
            at: now,
            from: (x, y),
            nav: it.nav.unwrap(),
        })
    }
    pub fn progress(&self, now: Instant) -> f32 {
        now.duration_since(self.at).as_secs_f32() / HOLD.as_secs_f32()
    }
    /// Slid too far to still count as holding the same spot.
    pub fn moved_away(&self, x: f32, y: f32) -> bool {
        ((x - self.from.0).powi(2) + (y - self.from.1).powi(2)).sqrt() >= super::touch::TAP_MAX_MOVE
    }
}

/// The touch target under (x, y): the innermost one (added last).
pub fn hit(tree: &Tree, x: f32, y: f32) -> Option<usize> {
    tree.items.iter().enumerate().rev().find_map(|(i, it)| {
        let r = it.rect;
        (it.touch && x >= r.x && x < r.right() && y >= r.y && y < r.bottom()).then_some(i)
    })
}

/// Draws a screen; `pressed` is brightened (feedback for a tap).
pub fn frame<'f>(tree: &Tree, fonts: &'f Fonts, pressed: Option<usize>) -> Canvas<'f> {
    let mut c = Canvas::new(fonts);
    layout::render(tree, &mut c);
    if let Some(it) = pressed.and_then(|i| tree.items.get(i)) {
        let radius = match it.paint {
            Paint::Panel { radius, .. } => radius,
            _ => theme::RADIUS_TILE,
        };
        c.fill_alpha(&rounded(it.rect, radius), 0xFFFFFF, 34);
    }
    c
}

pub struct Backlight(Option<PathBuf>);

impl Backlight {
    pub fn set(&self, on: bool) {
        if let Some(p) = &self.0
            && let Err(e) = std::fs::write(p, if on { "1" } else { "0" })
        {
            eprintln!("pios ui: backlight {}: {e}", p.display());
        }
    }
}

pub struct Options {
    pub fb: PathBuf,
    pub touch: Option<PathBuf>,
    pub calibration: Calibration,
    pub backlight: Option<PathBuf>,
    pub socket: PathBuf,
    /// the LED ring's pwmchip (by its `device`) and channel; None = no ring
    pub ring_device: Option<String>,
    pub ring_channel: u32,
    /// where the screen keeps its settings (the LED ring's choices)
    pub state_dir: Option<PathBuf>,
}

fn poll_touch(fd: Option<i32>, timeout: Duration) {
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    match fd {
        Some(fd) => {
            let mut p = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd for the duration of the call.
            unsafe { libc::poll(&mut p, 1, ms) };
        }
        None => std::thread::sleep(timeout),
    }
}

/// The current screen's layout, for the clock right now.
pub fn current_tree(app: &App, fonts: &Fonts) -> Tree {
    let (time, date) = super::local_now();
    app.tree(fonts, now_ms(), &time, &date, &super::local_at)
}

pub fn run(o: Options, fonts: &Fonts) -> Result<()> {
    signals::install();
    let mut fb = Framebuffer::open(&o.fb, theme::W as usize, theme::H as usize)?;
    let mut touch = match &o.touch {
        Some(p) => Some(Touch::open(p, o.calibration, (theme::W, theme::H))?),
        None => None,
    };
    let light = Backlight(o.backlight.clone());
    light.set(true);
    eprintln!(
        "pios ui: on {}{}",
        o.fb.display(),
        o.touch
            .as_deref()
            .map(|t| format!(", touch {}", t.display()))
            .unwrap_or_default()
    );

    let mut app = App::live();
    app.ring_path = o.state_dir.as_ref().map(|d| d.join("ring.conf"));
    app.ring = super::ring::Settings::load(app.ring_path.as_deref());
    app.prefs_path = o.state_dir.as_ref().map(|d| d.join("settings.conf"));
    app.prefs = super::prefs::Prefs::load(app.prefs_path.as_deref());
    super::set_clock24(app.prefs.clock24);
    app.alert_sync = true; // the agent gets the alert settings at every start
    let ring_state: super::ring::Shared =
        std::sync::Arc::new(std::sync::Mutex::new(super::ring::State {
            status: theme::Status::Unknown,
            since: Instant::now(),
            settings: app.ring,
            hold: None,
            fired: None,
            preview: None,
            night: None,
            shutdown: None,
        }));
    super::ring::spawn(o.ring_device.clone(), o.ring_channel, ring_state.clone());
    let mut fired: Option<Instant> = None;
    let mut taps = Taps::default();
    let mut last_touch = Instant::now();
    let mut dark = false;
    let mut dark_fetch: Option<Instant> = None;
    let mut swallow = false; // the touch that woke the screen does nothing else
    let mut pressed: Option<(usize, Instant, Option<Nav>)> = None;
    let mut holding: Option<Holding> = None;
    let mut shown: Option<(Tree, Option<usize>)> = None;
    // right after the Pi boots: the boot screen until the system and the agent are up
    let mut booting = uptime_secs() < BOOT_WINDOW;
    let mut system_ready = !booting;
    let mut boot_checked: Option<Instant> = None;

    while !signals::stopping() {
        let now = Instant::now();
        if !dark {
            app.update(&o.socket, now);
        } else if dark_fetch.is_none_or(|t| now - t >= FETCH_EVERY_DARK) {
            app.update(&o.socket, now);
            dark_fetch = Some(now);
        }

        if booting {
            if boot_checked.is_none_or(|t| now - t >= Duration::from_secs(1)) {
                boot_checked = Some(now);
                system_ready = system_started();
            }
            if (system_ready && app.home.snapshot.is_some()) || uptime_secs() >= BOOT_WINDOW {
                booting = false;
                fb.invalidate();
                shown = None;
            }
        }

        for ev in touch.as_mut().map(|t| t.read()).unwrap_or_default() {
            last_touch = now;
            if booting {
                continue; // nothing to tap on the boot screen
            }
            if dark {
                if let Event::Down(..) = ev {
                    dark = false;
                    swallow = true;
                    light.set(true);
                    fb.invalidate();
                    shown = None;
                }
                continue;
            }
            if swallow {
                if ev == Event::Up {
                    swallow = false;
                }
                continue;
            }
            match ev {
                Event::Down(x, y) => {
                    holding = Holding::start(&current_tree(&app, fonts), x, y, now);
                }
                Event::Move(x, y) if holding.is_some_and(|h| h.moved_away(x, y)) => {
                    holding = None;
                }
                Event::Up => holding = None,
                _ => {}
            }
            if let Some((x, y)) = taps.feed(ev, now) {
                let tree = current_tree(&app, fonts);
                if let Some(i) = hit(&tree, x, y).filter(|i| !tree.items[*i].hold) {
                    eprintln!("pios ui: tap {} at {x:.0},{y:.0}", tree.items[i].name);
                    pressed = Some((i, now + PRESS_SHOWN, tree.items[i].nav));
                }
            }
        }
        app.holding = holding.map(|h| h.progress(now).min(1.0));
        if let Some(h) = holding
            && now.duration_since(h.at) >= HOLD
        {
            holding = None;
            eprintln!("pios ui: hold fired: {:?}", h.nav);
            fired = Some(now);
            app.act(h.nav, &o.socket, now);
        }

        let night = app.prefs.is_night(super::local_minutes());
        // the ring says what Home says (also while the screen is dark)
        {
            let status = super::home::Home::build(&app.home, "", "").status;
            let mut r = ring_state.lock().unwrap_or_else(|e| e.into_inner());
            if r.status != status {
                r.status = status;
                r.since = now;
            }
            r.settings = app.ring;
            r.hold = app.holding;
            r.fired = fired;
            r.preview = app.ring_preview;
            r.night = night.then_some(app.prefs.night_ring);
        }

        // at night, after a while without a touch, the dim clock; a tap brings Home back
        let screen = app.screen();
        if !dark
            && night
            && screen != Screen::NightFace
            && now - last_touch >= app.prefs.night_after()
        {
            app.go(Nav::Home);
            app.screen = Some(Screen::NightFace);
            pressed = None;
            holding = None;
        } else if screen == Screen::NightFace && !night {
            app.go(Nav::Home);
        }

        if !dark && app.prefs.off_after().is_some_and(|d| now - last_touch >= d) {
            dark = true;
            light.set(false);
            app.go(Nav::Home);
            pressed = None;
            holding = None;
            app.holding = None;
        }

        if let Some((_, until, nav)) = pressed
            && now >= until
        {
            pressed = None;
            if let Some(nav) = nav {
                app.go(nav);
                // Refresh: draw the spinner first; the fetch runs at the top of the next turn
                if nav != Nav::Refresh {
                    app.update(&o.socket, Instant::now());
                }
            }
        }

        if !dark {
            let tree = if booting {
                super::screens::boot(
                    fonts,
                    uptime_secs(),
                    system_ready,
                    app.home.snapshot.is_some(),
                    running_containers(),
                )
            } else {
                current_tree(&app, fonts)
            };
            let p = pressed.map(|(i, _, _)| i);
            if shown.as_ref().is_none_or(|(t, sp)| *t != tree || *sp != p) {
                let c = frame(&tree, fonts, p);
                fb.present(c.pm.data())?;
                shown = Some((tree, p));
            }
        }

        // sleep until something is due: a touch, the highlight ending, the open screen's next
        // fetch, or (lit) the next second for the clock
        let now = Instant::now();
        let mut wait = if dark {
            FETCH_EVERY_DARK
        } else {
            Duration::from_secs(1).min(app.next_due(now))
        };
        if let Some((_, until, _)) = pressed {
            wait = wait.min(until.saturating_duration_since(now));
        }
        if holding.is_some() || app.spinning(now) {
            wait = wait.min(HOLD_FRAME);
        }
        poll_touch(
            touch.as_ref().map(|t| t.raw_fd()),
            wait.max(Duration::from_millis(10)),
        );
    }
    // stopped because the whole system is going down: say so until the containers are gone
    if let Some(reboot) = going_down() {
        eprintln!(
            "pios ui: the system is {}",
            if reboot { "rebooting" } else { "shutting down" }
        );
        light.set(true);
        fb.invalidate();
        let started = super::local_now().0;
        let total = running_containers();
        let until = Instant::now() + SHUTDOWN_SHOWN;
        loop {
            let left = running_containers().min(total);
            ring_state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .shutdown = Some(if total == 0 {
                0.0
            } else {
                left as f32 / total as f32
            });
            let tree = super::screens::shutdown(fonts, reboot, &started, left, total);
            fb.present(frame(&tree, fonts, None).pm.data())?;
            if left == 0 || Instant::now() >= until {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        // the last frame stays on the panel; ExecStopPost turns the ring off
        return Ok(());
    }
    eprintln!("pios ui: stopping");
    light.set(true);
    Ok(())
}

/// The boot screen shows only this soon after boot (later, a silent agent is a fault, not a boot).
const BOOT_WINDOW: u64 = 300;

fn uptime_secs() -> u64 {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split('.').next()?.trim().parse().ok())
        .unwrap_or(u64::MAX)
}

/// systemd has finished starting (running, or degraded with a failed unit).
fn system_started() -> bool {
    std::process::Command::new("systemctl")
        .arg("is-system-running")
        .output()
        .map(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            !matches!(s.trim(), "initializing" | "starting" | "")
        })
        .unwrap_or(true)
}

/// How long the shutdown screen waits for containers (the unit allows 60 s to stop).
const SHUTDOWN_SHOWN: Duration = Duration::from_secs(45);

/// Some(true) rebooting, Some(false) powering off, None: only this service is stopping.
fn going_down() -> Option<bool> {
    let out = |args: &[&str]| {
        std::process::Command::new("systemctl")
            .args(args)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    if out(&["is-system-running"])?.trim() != "stopping" {
        return None;
    }
    let jobs = out(&["list-jobs", "--no-legend", "--no-pager"]).unwrap_or_default();
    Some(jobs.contains("reboot.target") || jobs.contains("kexec.target"))
}

/// Containers still running: their cgroups (docker-ID.scope) that still hold processes.
fn running_containers() -> usize {
    std::fs::read_dir("/sys/fs/cgroup/system.slice")
        .map(|d| {
            d.flatten()
                .filter(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    n.starts_with("docker-") && n.ends_with(".scope")
                })
                .filter(|e| {
                    std::fs::read_to_string(e.path().join("cgroup.events"))
                        .is_ok_and(|t| crate::checks::populated(&t))
                })
                .count()
        })
        .unwrap_or(0)
}

/// For tests and `--check`: where a tap on each touch target lands.
#[cfg(test)]
pub fn centre(tree: &Tree, name: &str) -> (f32, f32) {
    let r = tree.items.iter().find(|i| i.name == name).unwrap().rect;
    (r.x + r.w / 2.0, r.cy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::home::Home;

    #[test]
    fn taps_hit_the_right_target() {
        let fonts = Fonts::load();
        let tree = Home::build(
            &super::super::sample("good").unwrap(),
            "12:04 PM",
            "Wed 18 Nov",
        )
        .tree(&fonts);
        for name in [
            "history button",
            "hero card",
            "CPU",
            "TEMP",
            "MEMORY",
            "DISK",
        ] {
            let (x, y) = centre(&tree, name);
            assert_eq!(
                hit(&tree, x, y).map(|i| tree.items[i].name.as_str()),
                Some(name)
            );
        }
        // the 8 px gaps between targets hit nothing
        let cpu = tree.items.iter().find(|i| i.name == "CPU").unwrap().rect;
        assert_eq!(hit(&tree, cpu.right() + 4.0, cpu.cy()), None);
        assert_eq!(hit(&tree, 5.0, 5.0), None); // screen margin
    }

    #[test]
    fn pressed_frame_differs_only_around_the_target() {
        let fonts = Fonts::load();
        let tree = Home::build(
            &super::super::sample("good").unwrap(),
            "12:04 PM",
            "Wed 18 Nov",
        )
        .tree(&fonts);
        let i = tree.items.iter().position(|i| i.name == "DISK").unwrap();
        let (a, b) = (frame(&tree, &fonts, None), frame(&tree, &fonts, Some(i)));
        let r = tree.items[i].rect;
        let w = theme::W as usize;
        for (k, (pa, pb)) in a.pm.pixels().iter().zip(b.pm.pixels()).enumerate() {
            if pa != pb {
                let (x, y) = ((k % w) as f32, (k / w) as f32);
                assert!(
                    x >= r.x - 1.0
                        && x <= r.right() + 1.0
                        && y >= r.y - 1.0
                        && y <= r.bottom() + 1.0,
                    "pixel {x},{y} changed outside DISK"
                );
            }
        }
    }

    #[test]
    fn fb_gets_only_changed_rows() {
        let fonts = Fonts::load();
        let d = super::super::sample("good").unwrap();
        let a = frame(
            &Home::build(&d, "12:04 PM", "Wed 18 Nov").tree(&fonts),
            &fonts,
            None,
        );
        let b = frame(
            &Home::build(&d, "12:05", "Wed 18 Nov").tree(&fonts),
            &fonts,
            None,
        );
        let bands =
            super::super::fb::changed_bands(Some(a.pm.data()), b.pm.data(), theme::W as usize * 4);
        let rows: usize = bands.iter().map(|b| b.1).sum();
        assert!(
            rows > 0 && rows <= 24,
            "clock tick sent {rows} rows: {bands:?}"
        );
    }
}
