//! The button's LED ring (white, one PWM channel): it says what the screen says, by rhythm.
//!
//! `level` is pure (time and state in, brightness out) so every pattern is tested; a small
//! thread (`spawn`) turns that into PWM duty, writing only when the value changes. The panel
//! loop hands it the current state each turn (`Shared`).
//!
//! Patterns (the choices are on the LED ring screen, saved in the screen's state file):
//!   All good          breathe 4 s, 5 → 100 % (as case-ring-breathe did) · steady · off
//!   Needs attention   double pulse every 6 s, 15 → 100 % · blink · steady
//!   Something down    1 s blink for a minute, then the double pulse · pulse · steady
//!   Starting          1.5 s rising ramps until data arrives
//!   Holding Restart   fills with the hold; two flashes when it fires

use std::f32::consts::TAU;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::theme::Status;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    Breathe,
    Pulse,
    Blink,
    Steady,
    Off,
}

impl Pattern {
    pub fn label(self) -> &'static str {
        match self {
            Pattern::Breathe => "Breathe",
            Pattern::Pulse => "Pulse",
            Pattern::Blink => "Blink",
            Pattern::Steady => "Steady",
            Pattern::Off => "Off",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Pattern::Breathe => "breathe",
            Pattern::Pulse => "pulse",
            Pattern::Blink => "blink",
            Pattern::Steady => "steady",
            Pattern::Off => "off",
        }
    }
    fn parse(s: &str) -> Option<Pattern> {
        [
            Pattern::Breathe,
            Pattern::Pulse,
            Pattern::Blink,
            Pattern::Steady,
            Pattern::Off,
        ]
        .into_iter()
        .find(|p| p.key() == s)
    }
}

/// The choices on each row of the LED ring screen, in order.
pub const GOOD: [Pattern; 3] = [Pattern::Breathe, Pattern::Steady, Pattern::Off];
pub const ATTENTION: [Pattern; 3] = [Pattern::Pulse, Pattern::Blink, Pattern::Steady];
pub const DOWN: [Pattern; 3] = [Pattern::Blink, Pattern::Pulse, Pattern::Steady];
pub const BRIGHTNESS: [(&str, f32); 3] = [("Low", 0.45), ("Medium", 0.7), ("High", 1.0)];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub good: Pattern,
    pub attention: Pattern,
    pub down: Pattern,
    /// index into BRIGHTNESS
    pub brightness: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            good: Pattern::Breathe,
            attention: Pattern::Pulse,
            down: Pattern::Blink,
            brightness: 2,
        }
    }
}

impl Settings {
    pub fn to_text(self) -> String {
        format!(
            "good={}\nattention={}\ndown={}\nbrightness={}\n",
            self.good.key(),
            self.attention.key(),
            self.down.key(),
            BRIGHTNESS[self.brightness.min(2)].0.to_lowercase()
        )
    }

    /// Unknown or bad lines keep the default (a hand-edited file can't break the ring).
    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            let p = Pattern::parse(v);
            match k {
                "good" => s.good = p.filter(|p| GOOD.contains(p)).unwrap_or(s.good),
                "attention" => {
                    s.attention = p.filter(|p| ATTENTION.contains(p)).unwrap_or(s.attention)
                }
                "down" => s.down = p.filter(|p| DOWN.contains(p)).unwrap_or(s.down),
                "brightness" => {
                    if let Some(i) = BRIGHTNESS
                        .iter()
                        .position(|(n, _)| n.eq_ignore_ascii_case(v))
                    {
                        s.brightness = i;
                    }
                }
                _ => {}
            }
        }
        s
    }

    pub fn load(path: Option<&Path>) -> Settings {
        path.and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| Settings::parse(&t))
            .unwrap_or_default()
    }

    pub fn save(&self, path: Option<&Path>) {
        if let Some(p) = path
            && let Err(e) = std::fs::write(p, self.to_text())
        {
            eprintln!("pios ui: {}: {e}", p.display());
        }
    }
}

/// Which status a pattern belongs to (it sets that pattern's low/high levels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Good,
    Attention,
    Down,
}

/// Everything the ring needs to know, set by the panel loop each turn.
#[derive(Debug, Clone, Copy)]
pub struct State {
    pub status: Status,
    /// when `status` last changed (Down blinks for its first minute)
    pub since: Instant,
    pub settings: Settings,
    /// a Restart hold in progress: 0–1
    pub hold: Option<f32>,
    /// the hold fired at
    pub fired: Option<Instant>,
    /// a pattern chosen on the LED ring screen plays for PREVIEW
    pub preview: Option<(Role, Pattern, Instant)>,
    /// inside the night window: how All good shows (problems still show, at low brightness)
    pub night: Option<super::prefs::NightRing>,
    /// shutting down: the share of containers still running (the ring fades with it)
    pub shutdown: Option<f32>,
}

pub const PREVIEW: Duration = Duration::from_secs(6);
/// "Faint" at night: a steady glow you only see in the dark (about 1.5 % duty)
pub const NIGHT_FAINT: f32 = 0.15;
const DOWN_BLINK_FOR: f32 = 60.0;

fn bump(x: f32, width: f32) -> f32 {
    if (0.0..width).contains(&x) {
        (x / width * TAU / 2.0).sin().powi(2)
    } else {
        0.0
    }
}

/// One pattern's brightness (0–1, before the brightness setting) `t` s into it.
pub fn pattern_level(role: Role, p: Pattern, t: f32) -> f32 {
    match p {
        Pattern::Off => 0.0,
        Pattern::Steady => match role {
            Role::Good => 0.6,
            Role::Attention => 0.8,
            Role::Down => 1.0,
        },
        // the old case-ring-breathe: 4 s, 5 → 100 % (perceptual), never fully off
        Pattern::Breathe => 0.05 + 0.95 * (0.5 - 0.5 * (t / 4.0 * TAU).cos()),
        Pattern::Pulse => {
            let x = t % 6.0;
            0.15 + 0.85 * (bump(x, 0.35) + bump(x - 0.5, 0.35))
        }
        Pattern::Blink => {
            if t % 1.0 < 0.5 {
                1.0
            } else {
                0.0
            }
        }
    }
}

/// The ring's brightness (0–1, perceptual) at `now`.
pub fn level(s: &State, now: Instant) -> f32 {
    use super::prefs::NightRing;
    if let Some(left) = s.shutdown {
        return left.clamp(0.0, 1.0);
    }
    let night = s.night.filter(|n| *n != NightRing::AsDay);
    let max = if night.is_some() {
        BRIGHTNESS[0].1
    } else {
        BRIGHTNESS[s.settings.brightness.min(2)].1
    };
    let secs = |t: Instant| now.saturating_duration_since(t).as_secs_f32();
    // two flashes when a hold fires (0.6 s)
    if let Some(f) = s.fired
        && secs(f) < 0.6
    {
        let x = secs(f);
        return if (x % 0.3) < 0.15 { max } else { 0.0 };
    }
    if let Some(p) = s.hold {
        return p.clamp(0.0, 1.0) * max;
    }
    if let Some((role, p, at)) = s.preview
        && secs(at) < PREVIEW.as_secs_f32()
    {
        return pattern_level(role, p, secs(at)) * max;
    }
    let t = secs(s.since);
    let v = match s.status {
        Status::Good if night == Some(NightRing::Faint) => return NIGHT_FAINT,
        Status::Good if night == Some(NightRing::Off) => return 0.0,
        Status::Good => pattern_level(Role::Good, s.settings.good, t),
        Status::Attention => pattern_level(Role::Attention, s.settings.attention, t),
        Status::Down => match s.settings.down {
            // blink for the first minute, then nudge
            Pattern::Blink if t >= DOWN_BLINK_FOR => pattern_level(Role::Down, Pattern::Pulse, t),
            p => pattern_level(Role::Down, p, t),
        },
        // starting, or the agent isn't answering: rising ramps
        Status::Unknown => (t % 1.5) / 1.5,
    };
    v * max
}

/// Changes over time right now (else the thread can sleep longer).
fn animating(s: &State, now: Instant) -> bool {
    let p = match s.status {
        Status::Good => s.settings.good,
        Status::Attention => s.settings.attention,
        Status::Down => s.settings.down,
        Status::Unknown => Pattern::Blink,
    };
    let resting = s.status == Status::Good
        && matches!(
            s.night,
            Some(super::prefs::NightRing::Faint | super::prefs::NightRing::Off)
        );
    s.shutdown.is_some()
        || s.hold.is_some()
        || s.fired
            .is_some_and(|f| now.saturating_duration_since(f) < Duration::from_secs(1))
        || s.preview
            .is_some_and(|(_, _, at)| now.saturating_duration_since(at) < PREVIEW)
        || (!resting && !matches!(p, Pattern::Steady | Pattern::Off))
}

pub type Shared = Arc<Mutex<State>>;

/// The PWM channel (sysfs) of the ring: the pwmchip whose `device` is `device`.
pub struct Pwm {
    duty: PathBuf,
    period: u64,
}

impl Pwm {
    pub fn find(device: &str, channel: u32) -> Option<Pwm> {
        let dir = std::fs::read_dir("/sys/class/pwm").ok()?;
        for e in dir.flatten() {
            let chip = e.path();
            let Ok(dev) = std::fs::canonicalize(chip.join("device")) else {
                continue;
            };
            if dev.file_name().and_then(|n| n.to_str()) != Some(device) {
                continue;
            }
            let ch = chip.join(format!("pwm{channel}"));
            let period = std::fs::read_to_string(ch.join("period"))
                .ok()?
                .trim()
                .parse()
                .ok()?;
            return Some(Pwm {
                duty: ch.join("duty_cycle"),
                period,
            });
        }
        None
    }

    /// Perceptual 0–1 → duty (gamma 2.2, like case-ring-breathe), in steps of 1/10 000 of the
    /// period: fine enough that the dim end of a fade doesn't visibly step.
    pub fn duty_for(&self, level: f32) -> u64 {
        let lin = level.clamp(0.0, 1.0).powf(2.2) as f64;
        (lin * 10_000.0).round() as u64 * self.period / 10_000
    }

    fn write(&self, duty: u64) -> std::io::Result<()> {
        std::fs::write(&self.duty, duty.to_string())
    }
}

/// Starts the ring thread. Without a PWM (laptop, or setup missing) it does nothing.
pub fn spawn(device: Option<String>, channel: u32, state: Shared) {
    let Some(device) = device else { return };
    let Some(pwm) = Pwm::find(&device, channel) else {
        eprintln!("pios ui: LED ring: no pwm{channel} on {device}; the ring stays as it is");
        return;
    };
    let _ = std::thread::Builder::new()
        .name("ring".into())
        .spawn(move || {
            let mut last: Option<u64> = None;
            loop {
                let now = Instant::now();
                let s = *state.lock().unwrap_or_else(|e| e.into_inner());
                let duty = pwm.duty_for(level(&s, now));
                if last != Some(duty) {
                    if let Err(e) = pwm.write(duty) {
                        eprintln!("pios ui: LED ring: {e}; stopping the ring");
                        return;
                    }
                    last = Some(duty);
                }
                // ~33 writes a second at most while it moves; a lazy check when it's still
                std::thread::sleep(if animating(&s, now) {
                    Duration::from_millis(30)
                } else {
                    Duration::from_millis(250)
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(status: Status) -> State {
        State {
            status,
            since: Instant::now(),
            settings: Settings::default(),
            hold: None,
            fired: None,
            preview: None,
            night: None,
            shutdown: None,
        }
    }

    fn at(s: &State, secs: f32) -> f32 {
        level(s, s.since + Duration::from_secs_f32(secs))
    }

    #[test]
    fn good_breathes_like_the_old_script() {
        let s = st(Status::Good); // default brightness: High
        assert!((at(&s, 0.0) - 0.05).abs() < 1e-3);
        assert!((at(&s, 2.0) - 1.0).abs() < 1e-3);
        assert!((at(&s, 4.0) - 0.05).abs() < 1e-3);
        let p = Pwm {
            duty: PathBuf::new(),
            period: 1_000_000,
        };
        // the old script's numbers: 0.05^2.2 and full
        assert_eq!(p.duty_for(0.05), 1_400);
        assert_eq!(p.duty_for(1.0), 1_000_000);
    }

    #[test]
    fn down_blinks_for_a_minute_then_nudges() {
        let s = st(Status::Down);
        let max = BRIGHTNESS[2].1;
        assert_eq!(at(&s, 0.2), max);
        assert_eq!(at(&s, 0.7), 0.0);
        // after a minute: the double pulse's floor (15 %) between bumps
        assert!((at(&s, 62.0 + 3.0) - 0.15 * max).abs() < 1e-3);
    }

    #[test]
    fn hold_and_flashes_win() {
        let mut s = st(Status::Good);
        s.hold = Some(0.5);
        assert!((at(&s, 1.0) - 0.5 * BRIGHTNESS[2].1).abs() < 1e-3);
        s.hold = None;
        s.fired = Some(s.since);
        assert_eq!(at(&s, 0.05), BRIGHTNESS[2].1);
        assert_eq!(at(&s, 0.2), 0.0);
    }

    #[test]
    fn preview_plays_then_stops() {
        let mut s = st(Status::Good);
        s.settings.good = Pattern::Off;
        s.preview = Some((Role::Down, Pattern::Steady, s.since));
        assert_eq!(at(&s, 1.0), BRIGHTNESS[2].1);
        assert_eq!(at(&s, 7.0), 0.0);
    }

    #[test]
    fn off_and_steady_rest() {
        let mut s = st(Status::Good);
        s.settings.good = Pattern::Off;
        assert_eq!(at(&s, 3.0), 0.0);
        assert!(!animating(&s, s.since));
        s.settings.good = Pattern::Steady;
        assert!(!animating(&s, s.since));
        assert!(animating(&st(Status::Unknown), Instant::now()));
    }

    #[test]
    fn night_and_shutdown() {
        use crate::ui::prefs::NightRing;
        let mut s = st(Status::Good);
        s.night = Some(NightRing::Faint);
        assert_eq!(at(&s, 3.0), NIGHT_FAINT);
        assert!(!animating(&s, s.since), "a steady glow needs no redraws");
        s.night = Some(NightRing::Off);
        assert_eq!(at(&s, 3.0), 0.0);
        // problems still show at night, at low brightness
        s.status = Status::Down;
        assert_eq!(at(&s, 0.2), BRIGHTNESS[0].1);
        s.night = Some(NightRing::AsDay);
        assert_eq!(at(&s, 0.2), BRIGHTNESS[2].1);
        // shutting down: the ring is the share of containers still running
        s.shutdown = Some(0.25);
        assert_eq!(at(&s, 0.2), 0.25);
    }

    #[test]
    fn settings_round_trip_and_bad_lines() {
        let s = Settings {
            good: Pattern::Steady,
            attention: Pattern::Blink,
            down: Pattern::Pulse,
            brightness: 2,
        };
        assert_eq!(Settings::parse(&s.to_text()), s);
        let bad = Settings::parse("good=blink\nattention=nope\nbrightness=extreme\nnoise\n");
        assert_eq!(
            bad,
            Settings::default(),
            "invalid choices keep the defaults"
        );
    }

    #[test]
    fn gamma_and_steps() {
        let p = Pwm {
            duty: PathBuf::new(),
            period: 1_000_000,
        };
        assert_eq!(p.duty_for(0.0), 0);
        assert_eq!(p.duty_for(1.0), 1_000_000);
        assert_eq!(p.duty_for(0.5), 217_600); // 0.5^2.2 ≈ 0.2176
    }
}
