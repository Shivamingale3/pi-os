//! Resistive touch (ADS7846) through evdev, read without libinput: raw ABS_X / ABS_Y + BTN_TOUCH,
//! mapped to screen pixels with the same calibration matrix libinput uses
//! (LIBINPUT_CALIBRATION_MATRIX in the udev rule), then turned into taps.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::error::{Result, err};

/// libinput's 2×3 matrix, applied to coordinates normalised to 0–1:
/// x' = a·x + b·y + c, y' = d·x + e·y + f.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration(pub [f32; 6]);

impl Calibration {
    pub const IDENTITY: Calibration = Calibration([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);

    pub fn parse(s: &str) -> Result<Calibration> {
        let v: Vec<f32> = s
            .split_whitespace()
            .map(|p| p.parse::<f32>())
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| err(format!("bad calibration {s:?}")))?;
        let arr: [f32; 6] = v
            .try_into()
            .map_err(|_| err(format!("calibration needs 6 numbers: {s:?}")))?;
        Ok(Calibration(arr))
    }

    /// Normalised (0–1) device position → screen pixels, clamped to the screen.
    pub fn to_screen(self, x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.0;
        let sx = (a * x + b * y + c).clamp(0.0, 1.0) * w;
        let sy = (d * x + e * y + f).clamp(0.0, 1.0) * h;
        (sx.min(w - 1.0), sy.min(h - 1.0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    Down(f32, f32),
    Move(f32, f32),
    Up,
}

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const BTN_TOUCH: u16 = 330;
const EVENT_SIZE: usize = 24; // struct input_event on 64-bit: timeval(16) + type + code + value

/// Turns a stream of raw evdev records into Down / Move / Up in screen pixels. Pure (tested).
#[derive(Debug)]
pub struct Decoder {
    pub cal: Calibration,
    pub range_x: (i32, i32),
    pub range_y: (i32, i32),
    pub size: (f32, f32),
    x: i32,
    y: i32,
    down: bool,
    was_down: bool,
}

impl Decoder {
    pub fn new(
        cal: Calibration,
        range_x: (i32, i32),
        range_y: (i32, i32),
        size: (f32, f32),
    ) -> Self {
        Decoder {
            cal,
            range_x,
            range_y,
            size,
            x: 0,
            y: 0,
            down: false,
            was_down: false,
        }
    }

    fn screen(&self) -> (f32, f32) {
        let n = |v: i32, (lo, hi): (i32, i32)| (v - lo) as f32 / (hi - lo).max(1) as f32;
        self.cal.to_screen(
            n(self.x, self.range_x),
            n(self.y, self.range_y),
            self.size.0,
            self.size.1,
        )
    }

    /// One evdev record (type, code, value) → at most one event, emitted at SYN_REPORT.
    pub fn feed(&mut self, kind: u16, code: u16, value: i32) -> Option<Event> {
        match (kind, code) {
            (EV_ABS, ABS_X) => self.x = value,
            (EV_ABS, ABS_Y) => self.y = value,
            (EV_KEY, BTN_TOUCH) => self.down = value != 0,
            (EV_SYN, 0) => {
                let ev = match (self.was_down, self.down) {
                    (false, true) => Some(Event::Down(self.screen().0, self.screen().1)),
                    (true, true) => Some(Event::Move(self.screen().0, self.screen().1)),
                    (true, false) => Some(Event::Up),
                    (false, false) => None,
                };
                self.was_down = self.down;
                return ev;
            }
            _ => {}
        }
        None
    }
}

pub struct Touch {
    file: File,
    pub decoder: Decoder,
}

fn abs_range(file: &File, code: u16) -> Option<(i32, i32)> {
    let mut b = [0i32; 6];
    // EVIOCGABS(code) = _IOR('E', 0x40 + code, struct input_absinfo) (24 bytes)
    let req = (2u64 << 30) | (24u64 << 16) | (('E' as u64) << 8) | (0x40 + code as u64);
    // SAFETY: the ioctl fills the 24-byte struct we pass.
    let r = unsafe { libc::ioctl(file.as_raw_fd(), req as libc::c_ulong, b.as_mut_ptr()) };
    (r == 0 && b[2] > b[1]).then_some((b[1], b[2]))
}

impl Touch {
    pub fn open(path: &Path, cal: Calibration, size: (f32, f32)) -> Result<Touch> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| err(format!("{}: {e}", path.display())))?;
        let rx = abs_range(&file, ABS_X).unwrap_or((0, 4095));
        let ry = abs_range(&file, ABS_Y).unwrap_or((0, 4095));
        Ok(Touch {
            file,
            decoder: Decoder::new(cal, rx, ry, size),
        })
    }

    pub fn raw_fd(&self) -> i32 {
        self.file.as_raw_fd()
    }

    /// Everything that's waiting, without blocking.
    pub fn read(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        let mut buf = [0u8; EVENT_SIZE * 64];
        while let Ok(n) = self.file.read(&mut buf) {
            if n == 0 {
                break;
            }
            for rec in buf[..n].as_chunks::<EVENT_SIZE>().0 {
                let kind = u16::from_ne_bytes([rec[16], rec[17]]);
                let code = u16::from_ne_bytes([rec[18], rec[19]]);
                let value = i32::from_ne_bytes([rec[20], rec[21], rec[22], rec[23]]);
                out.extend(self.decoder.feed(kind, code, value));
            }
        }
        out
    }
}

/// A tap: press and release within 1.5 s, moving less than 20 px (resistive touch wobbles).
#[derive(Debug, Default)]
pub struct Taps {
    start: Option<(Instant, (f32, f32))>,
    last: (f32, f32),
}

pub const TAP_MAX_MOVE: f32 = 20.0;
pub const TAP_MAX_TIME: Duration = Duration::from_millis(1500);

impl Taps {
    pub fn feed(&mut self, ev: Event, now: Instant) -> Option<(f32, f32)> {
        match ev {
            Event::Down(x, y) => {
                self.start = Some((now, (x, y)));
                self.last = (x, y);
                None
            }
            Event::Move(x, y) => {
                self.last = (x, y);
                None
            }
            Event::Up => {
                let (t0, p0) = self.start.take()?;
                let moved = ((self.last.0 - p0.0).powi(2) + (self.last.1 - p0.1).powi(2)).sqrt();
                (now.duration_since(t0) <= TAP_MAX_TIME && moved < TAP_MAX_MOVE).then_some(p0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEBBLE: &str = "0 1.1052 -0.0649 1.1297 0 -0.0755";

    #[test]
    fn pebble_calibration_swaps_axes() {
        let c = Calibration::parse(PEBBLE).unwrap();
        // raw y drives screen x, raw x drives screen y
        let (x, y) = c.to_screen(0.5, 0.5, 480.0, 320.0);
        assert!(
            (x - 234.1).abs() < 0.5 && (y - 156.6).abs() < 0.5,
            "{x} {y}"
        );
        assert_eq!(c.to_screen(0.0, 0.0, 480.0, 320.0), (0.0, 0.0)); // clamped
        assert_eq!(c.to_screen(1.0, 1.0, 480.0, 320.0), (479.0, 319.0)); // clamped
        assert!(Calibration::parse("1 2 3").is_err());
        assert!(Calibration::parse("a b c d e f").is_err());
    }

    #[test]
    fn decoder_emits_down_move_up_at_syn() {
        let mut d = Decoder::new(Calibration::IDENTITY, (0, 4095), (0, 4095), (480.0, 320.0));
        assert_eq!(d.feed(EV_ABS, ABS_X, 2048), None);
        assert_eq!(d.feed(EV_ABS, ABS_Y, 1024), None);
        assert_eq!(d.feed(EV_KEY, BTN_TOUCH, 1), None);
        let Some(Event::Down(x, y)) = d.feed(EV_SYN, 0, 0) else {
            panic!()
        };
        assert!((x - 240.0).abs() < 0.2 && (y - 80.0).abs() < 0.2);
        d.feed(EV_ABS, ABS_X, 2100);
        assert!(matches!(d.feed(EV_SYN, 0, 0), Some(Event::Move(..))));
        d.feed(EV_KEY, BTN_TOUCH, 0);
        assert_eq!(d.feed(EV_SYN, 0, 0), Some(Event::Up));
        assert_eq!(d.feed(EV_SYN, 0, 0), None);
    }

    #[test]
    fn taps() {
        let t0 = Instant::now();
        let mut t = Taps::default();
        t.feed(Event::Down(100.0, 100.0), t0);
        t.feed(Event::Move(108.0, 104.0), t0);
        assert_eq!(
            t.feed(Event::Up, t0 + Duration::from_millis(200)),
            Some((100.0, 100.0))
        );
        // a drag is not a tap
        t.feed(Event::Down(100.0, 100.0), t0);
        t.feed(Event::Move(160.0, 100.0), t0);
        assert_eq!(t.feed(Event::Up, t0 + Duration::from_millis(200)), None);
        // a long press is not a tap
        t.feed(Event::Down(100.0, 100.0), t0);
        assert_eq!(t.feed(Event::Up, t0 + Duration::from_secs(3)), None);
        // Up without Down
        assert_eq!(t.feed(Event::Up, t0), None);
    }
}
