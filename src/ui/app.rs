//! The app: which screen is open, what it shows, and what to fetch for it (only that).
//! The panel loop and the laptop preview both drive this; they only add input and output.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

use super::canvas::Fonts;
use super::home::{Data, Home};
use super::layout::{Nav, Tree};
use super::screens::{self, Incident, IncidentFull};
use crate::details::{CpuDetails, DiskDetails, MemDetails, TempDetails};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    Cpu,
    Temps,
    Memory,
    Disk,
    History,
    Incident(i64),
}

impl Screen {
    pub fn parse(name: &str) -> Option<Screen> {
        Some(match name {
            "home" => Screen::Home,
            "cpu" => Screen::Cpu,
            "temps" => Screen::Temps,
            "memory" => Screen::Memory,
            "disk" => Screen::Disk,
            "history" => Screen::History,
            "incident" => Screen::Incident(1),
            _ => return None,
        })
    }
}

pub const SCREENS: [&str; 7] = [
    "home", "cpu", "temps", "memory", "disk", "history", "incident",
];

/// I/O sparkline length (one point per Disk refresh).
const IO_POINTS: usize = 12;
/// 24 h graphs are drawn from this many averaged buckets (15 min each).
const TREND_BUCKETS: usize = 96;

#[derive(Default)]
pub struct App {
    pub screen: Option<Screen>,
    pub page: usize,
    pub home: Data,
    pub cpu: Option<CpuDetails>,
    pub temps: Option<TempDetails>,
    pub mem: Option<MemDetails>,
    pub disk: Option<DiskDetails>,
    pub cpu_trend: Vec<f32>,
    pub temp_trend: Vec<f32>,
    pub reads: VecDeque<f32>,
    pub writes: VecDeque<f32>,
    pub incidents: Option<Vec<Incident>>,
    pub incident: Option<IncidentFull>,
    last: HashMap<&'static str, Instant>,
    /// false for built-in samples: never fetch
    pub live: bool,
}

/// Averages the points into at most `n` buckets (keeps the shape, drops the detail).
pub fn downsample(points: &[f32], n: usize) -> Vec<f32> {
    if points.len() <= n {
        return points.to_vec();
    }
    let size = points.len() as f32 / n as f32;
    (0..n)
        .map(|i| {
            let a = (i as f32 * size) as usize;
            let b = (((i + 1) as f32 * size) as usize).clamp(a + 1, points.len());
            points[a..b].iter().sum::<f32>() / (b - a) as f32
        })
        .collect()
}

impl App {
    pub fn live() -> App {
        App {
            screen: Some(Screen::Home),
            live: true,
            ..App::default()
        }
    }

    pub fn screen(&self) -> Screen {
        self.screen.unwrap_or(Screen::Home)
    }

    pub fn go(&mut self, nav: Nav) {
        let next = match nav {
            Nav::Home => Screen::Home,
            Nav::Cpu => Screen::Cpu,
            Nav::Temps => Screen::Temps,
            Nav::Memory => Screen::Memory,
            Nav::Disk => Screen::Disk,
            Nav::History => {
                self.page = 0;
                Screen::History
            }
            Nav::Page(p) => {
                self.page = p;
                Screen::History
            }
            Nav::Incident(id) => {
                if self.incident.as_ref().is_none_or(|i| i.incident.id != id) {
                    self.incident = None;
                }
                self.last.remove("incident");
                Screen::Incident(id)
            }
            Nav::Back => match self.screen() {
                Screen::Incident(_) => Screen::History,
                _ => Screen::Home,
            },
        };
        if next == Screen::Disk && self.screen() != Screen::Disk {
            self.reads.clear();
            self.writes.clear();
        }
        self.screen = Some(next);
    }

    fn due(&mut self, key: &'static str, every: Duration, now: Instant) -> bool {
        if self
            .last
            .get(key)
            .is_some_and(|t| now.duration_since(*t) < every)
        {
            return false;
        }
        self.last.insert(key, now);
        true
    }

    /// What this screen needs, and how often.
    fn needs(&self) -> Vec<(&'static str, Duration)> {
        let s = Duration::from_secs;
        let mut v = vec![("home", s(5))];
        match self.screen() {
            Screen::Home => {}
            Screen::Cpu => v.extend([("cpu", s(2)), ("cpu_trend", s(60))]),
            Screen::Temps => v.extend([("temps", s(5)), ("temp_trend", s(60))]),
            Screen::Memory => v.push(("mem", s(5))),
            Screen::Disk => v.push(("disk", s(3))),
            Screen::History => v.push(("incidents", s(10))),
            Screen::Incident(_) => v.push(("incident", s(10))),
        }
        v
    }

    /// Fetches what's due for the open screen. Returns true if anything was fetched.
    pub fn update(&mut self, socket: &Path, now: Instant) -> bool {
        if !self.live {
            return false;
        }
        let mut any = false;
        for (key, every) in self.needs() {
            if !self.due(key, every, now) {
                continue;
            }
            any = true;
            match key {
                "home" => self.home = super::fetch(socket),
                "cpu" => self.cpu = super::get_json(socket, "/details/cpu").ok(),
                "temps" => self.temps = super::get_json(socket, "/details/temps").ok(),
                "mem" => self.mem = super::get_json(socket, "/details/memory").ok(),
                "disk" => {
                    self.disk = super::get_json::<DiskDetails>(socket, "/details/disk").ok();
                    if let Some(d) = &self.disk {
                        for (q, v) in [
                            (&mut self.reads, d.read_bps),
                            (&mut self.writes, d.write_bps),
                        ] {
                            q.push_back(v as f32);
                            while q.len() > IO_POINTS {
                                q.pop_front();
                            }
                        }
                    }
                }
                "cpu_trend" => {
                    self.cpu_trend =
                        downsample(&super::trend_24h(socket, "cpu.percent"), TREND_BUCKETS)
                }
                "temp_trend" => {
                    self.temp_trend =
                        downsample(&super::trend_24h(socket, "cpu.tempC"), TREND_BUCKETS)
                }
                "incidents" => {
                    self.incidents = super::get_json::<super::IncidentsOut>(socket, "/incidents")
                        .ok()
                        .map(|l| l.incidents)
                }
                "incident" => {
                    if let Screen::Incident(id) = self.screen() {
                        self.incident = super::get_json(socket, &format!("/incidents/{id}")).ok();
                    }
                }
                _ => {}
            }
        }
        any
    }

    /// How long until the next fetch is due.
    pub fn next_due(&self, now: Instant) -> Duration {
        self.needs()
            .into_iter()
            .map(|(k, every)| {
                self.last.get(k).map_or(Duration::ZERO, |t| {
                    (*t + every).saturating_duration_since(now)
                })
            })
            .min()
            .unwrap_or(Duration::from_secs(5))
    }

    /// Lays out the open screen. `at` turns a timestamp into (time, date) text.
    pub fn tree(
        &self,
        f: &Fonts,
        now_ms: i64,
        time: &str,
        date: &str,
        at: &dyn Fn(i64) -> (String, String),
    ) -> Tree {
        let reads: Vec<f32> = self.reads.iter().copied().collect();
        let writes: Vec<f32> = self.writes.iter().copied().collect();
        match self.screen() {
            Screen::Home => Home::build(&self.home, time, date).tree(f),
            Screen::Cpu => screens::cpu(f, self.cpu.as_ref(), &self.cpu_trend),
            Screen::Temps => screens::temps(f, self.temps.as_ref(), &self.temp_trend),
            Screen::Memory => screens::memory(f, self.mem.as_ref()),
            Screen::Disk => screens::disk(f, self.disk.as_ref(), &reads, &writes),
            Screen::History => {
                screens::history(f, self.incidents.as_deref(), self.page, now_ms, at)
            }
            Screen::Incident(_) => screens::incident(f, self.incident.as_ref(), now_ms, at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_and_back() {
        let mut a = App::live();
        a.go(Nav::Cpu);
        assert_eq!(a.screen(), Screen::Cpu);
        a.go(Nav::Back);
        assert_eq!(a.screen(), Screen::Home);
        a.go(Nav::History);
        a.go(Nav::Page(2));
        assert_eq!((a.screen(), a.page), (Screen::History, 2));
        a.go(Nav::Incident(7));
        assert_eq!(a.screen(), Screen::Incident(7));
        a.go(Nav::Back);
        assert_eq!((a.screen(), a.page), (Screen::History, 2));
        a.go(Nav::Back);
        assert_eq!(a.screen(), Screen::Home);
    }

    #[test]
    fn only_the_open_screen_is_fetched() {
        let mut a = App::live();
        assert_eq!(a.needs().len(), 1);
        a.go(Nav::Cpu);
        let keys: Vec<_> = a.needs().into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["home", "cpu", "cpu_trend"]);
    }

    #[test]
    fn downsampling() {
        let pts: Vec<f32> = (0..1440).map(|i| i as f32).collect();
        let d = downsample(&pts, 96);
        assert_eq!(d.len(), 96);
        assert_eq!(d[0], 7.0);
        assert_eq!(downsample(&[1.0, 2.0], 96), [1.0, 2.0]);
    }
}
