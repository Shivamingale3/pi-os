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

use super::app::App;
use super::canvas::{Canvas, Fonts, rounded};
use super::fb::Framebuffer;
use super::layout::{self, Nav, Paint, Tree};
use super::theme;
use super::touch::{Calibration, Event, Taps, Touch};
use crate::error::Result;
use crate::signals;
use crate::types::now_ms;

/// While the screen is dark, read the agent rarely (nothing is shown).
pub const FETCH_EVERY_DARK: Duration = Duration::from_secs(60);
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
    pub idle: Duration,
    pub socket: PathBuf,
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
    let mut taps = Taps::default();
    let mut last_touch = Instant::now();
    let mut dark = false;
    let mut dark_fetch: Option<Instant> = None;
    let mut swallow = false; // the touch that woke the screen does nothing else
    let mut pressed: Option<(usize, Instant, Option<Nav>)> = None;
    let mut holding: Option<Holding> = None;
    let mut shown: Option<(Tree, Option<usize>)> = None;

    while !signals::stopping() {
        let now = Instant::now();
        if !dark {
            app.update(&o.socket, now);
        } else if dark_fetch.is_none_or(|t| now - t >= FETCH_EVERY_DARK) {
            app.update(&o.socket, now);
            dark_fetch = Some(now);
        }

        for ev in touch.as_mut().map(|t| t.read()).unwrap_or_default() {
            last_touch = now;
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
            app.act(h.nav, &o.socket, now);
        }

        if !dark && now - last_touch >= o.idle {
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
                app.update(&o.socket, Instant::now());
            }
        }

        if !dark {
            let tree = current_tree(&app, fonts);
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
        if holding.is_some() {
            wait = wait.min(HOLD_FRAME);
        }
        poll_touch(
            touch.as_ref().map(|t| t.raw_fd()),
            wait.max(Duration::from_millis(10)),
        );
    }
    eprintln!("pios ui: stopping");
    light.set(true);
    Ok(())
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
