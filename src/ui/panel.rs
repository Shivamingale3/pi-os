//! `pios ui --fb …`: Home on the real panel.
//!
//! The loop sleeps in poll() on the touch device until something is due (a touch, the clock,
//! the next fetch, the end of a press highlight), so it costs nothing while nothing happens.
//! It redraws only when the picture changes, and sends only the changed rows (fb.rs).
//! Backlight: off after `idle` without a touch; the first touch on a dark screen only wakes it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::canvas::{Canvas, Fonts, rounded};
use super::fb::Framebuffer;
use super::home::{Data, Home};
use super::layout::{self, Paint, Rect, Tree};
use super::theme;
use super::touch::{Calibration, Event, Taps, Touch};
use crate::error::Result;
use crate::signals;

pub const FETCH_EVERY: Duration = Duration::from_secs(5);
/// While the screen is dark, read the agent rarely (nothing is shown).
pub const FETCH_EVERY_DARK: Duration = Duration::from_secs(60);
pub const PRESS_SHOWN: Duration = Duration::from_millis(180);

/// The touch target under (x, y): the innermost one (added last).
pub fn hit(tree: &Tree, x: f32, y: f32) -> Option<usize> {
    tree.items.iter().enumerate().rev().find_map(|(i, it)| {
        let r = it.rect;
        (it.touch && x >= r.x && x < r.right() && y >= r.y && y < r.bottom()).then_some(i)
    })
}

/// Draws a screen; `pressed` gets a 2 px ring inside its edge (feedback for a tap).
pub fn frame<'f>(tree: &Tree, fonts: &'f Fonts, pressed: Option<usize>) -> Canvas<'f> {
    let mut c = Canvas::new(fonts);
    layout::render(tree, &mut c);
    if let Some(it) = pressed.and_then(|i| tree.items.get(i)) {
        let radius = match it.paint {
            Paint::Panel { radius, .. } => radius,
            _ => theme::RADIUS_TILE,
        };
        let r = it.rect;
        c.stroke(
            &rounded(
                Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, r.h - 2.0),
                radius - 1.0,
            ),
            theme::INK2,
            2.0,
        );
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

    let mut data = Data::default();
    let mut fetched: Option<Instant> = None;
    let mut taps = Taps::default();
    let mut last_touch = Instant::now();
    let mut dark = false;
    let mut swallow = false; // the touch that woke the screen does nothing else
    let mut pressed: Option<(usize, Instant)> = None;
    let mut shown: Option<(Home, Option<usize>)> = None;

    while !signals::stopping() {
        let now = Instant::now();
        let every = if dark { FETCH_EVERY_DARK } else { FETCH_EVERY };
        if fetched.is_none_or(|t| now - t >= every) {
            data = super::fetch(&o.socket);
            fetched = Some(now);
        }

        for ev in touch.as_mut().map(|t| t.read()).unwrap_or_default() {
            last_touch = now;
            if dark {
                if let Event::Down(..) = ev {
                    dark = false;
                    swallow = true;
                    light.set(true);
                    fb.invalidate();
                }
                continue;
            }
            if swallow {
                if ev == Event::Up {
                    swallow = false;
                }
                continue;
            }
            if let Some((x, y)) = taps.feed(ev, now) {
                let (time, date) = super::local_now();
                let tree = Home::build(&data, &time, &date).tree(fonts);
                if let Some(i) = hit(&tree, x, y) {
                    eprintln!("pios ui: tap {} at {x:.0},{y:.0}", tree.items[i].name);
                    pressed = Some((i, now + PRESS_SHOWN));
                }
            }
        }

        if !dark && now - last_touch >= o.idle {
            dark = true;
            light.set(false);
        }

        if pressed.is_some_and(|(_, until)| now >= until) {
            pressed = None;
        }
        if !dark {
            let (time, date) = super::local_now();
            let home = Home::build(&data, &time, &date);
            let p = pressed.map(|(i, _)| i);
            if shown.as_ref() != Some(&(home.clone(), p)) {
                let tree = home.tree(fonts);
                let c = frame(&tree, fonts, p);
                fb.present(c.pm.data())?;
                shown = Some((home, p));
            }
        }

        // sleep until something is due: a touch, the press highlight ending, the next fetch, or
        // (lit) the next second for the clock
        let mut wait = if dark {
            FETCH_EVERY_DARK
        } else {
            Duration::from_secs(1)
        };
        if let Some((_, until)) = pressed {
            wait = wait.min(until.saturating_duration_since(Instant::now()));
        }
        if let Some(t) = fetched {
            wait = wait.min((t + every).saturating_duration_since(Instant::now()));
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

    #[test]
    fn taps_hit_the_right_target() {
        let fonts = Fonts::load();
        let tree = Home::build(
            &super::super::sample("good").unwrap(),
            "12:04",
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
            "12:04",
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
            &Home::build(&d, "12:04", "Wed 18 Nov").tree(&fonts),
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
