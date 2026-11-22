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
use crate::services::{Service, Services, key};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    Cpu,
    Temps,
    Memory,
    Disk,
    History,
    Incident(i64),
    Services,
    Container(u64),
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
            "services" | "host" | "web" => Screen::Services,
            "events" => Screen::History,
            "container" => Screen::Container(0),
            _ => return None,
        })
    }
}

/// `--screen host` / `--screen web`: the Services screen on that tab.
pub fn tab_for(name: &str) -> screens::Tab {
    match name {
        "host" => screens::Tab::Host,
        "web" => screens::Tab::Web,
        _ => screens::Tab::Apps,
    }
}

/// `--screen events`: History on its Events tab.
pub fn hist_tab_for(name: &str) -> screens::HistTab {
    if name == "events" {
        screens::HistTab::Events
    } else {
        screens::HistTab::Incidents
    }
}

pub const SCREENS: [&str; 12] = [
    "home",
    "cpu",
    "temps",
    "memory",
    "disk",
    "history",
    "events",
    "incident",
    "services",
    "host",
    "web",
    "container",
];

/// The refresh icon spins at least this long, so a fast fetch is still seen to happen.
pub const REFRESH_SHOWN: Duration = Duration::from_millis(800);

/// Sparkline length (one point per refresh: Disk I/O, a container's CPU and memory).
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
    pub services: Option<Services>,
    /// the open container's CPU and memory, one point per refresh
    pub svc_cpu: VecDeque<f32>,
    pub svc_mem: VecDeque<f32>,
    /// the Restart button is being held: 0–1 of the 2 s (set by the panel or preview)
    pub holding: Option<f32>,
    /// the agent refused the last restart request (container key, why)
    pub refused: Option<(u64, String)>,
    /// the open Services tab
    pub tab: screens::Tab,
    /// History's open tab, and the Events tab's data
    pub hist_tab: screens::HistTab,
    pub events: Option<Vec<crate::events::Event>>,
    /// when Refresh was tapped: its icon spins for at least REFRESH_SHOWN
    pub refreshing: Option<Instant>,
    last: HashMap<&'static str, Instant>,
    /// false for built-in samples: never fetch
    pub live: bool,
}

#[derive(serde::Deserialize)]
struct EventsOut {
    events: Vec<crate::events::Event>,
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
                self.hist_tab = screens::HistTab::Incidents;
                Screen::History
            }
            Nav::Services => {
                self.page = 0;
                self.tab = self.problem_tab();
                Screen::Services
            }
            Nav::Tab(i) if self.screen() == Screen::History => {
                let t = if i == 0 {
                    screens::HistTab::Incidents
                } else {
                    screens::HistTab::Events
                };
                if t != self.hist_tab {
                    self.page = 0;
                }
                self.hist_tab = t;
                Screen::History
            }
            Nav::Tab(i) => {
                let t = screens::Tab::ALL[(i as usize).min(2)];
                if t != self.tab {
                    self.page = 0;
                }
                self.tab = t;
                Screen::Services
            }
            Nav::Page(p) => {
                self.page = p;
                match self.screen() {
                    Screen::Services => Screen::Services,
                    _ => Screen::History,
                }
            }
            Nav::Container(k) => {
                if self.screen() != Screen::Container(k) {
                    self.svc_cpu.clear();
                    self.svc_mem.clear();
                    self.refused = None;
                }
                Screen::Container(k)
            }
            // an action, not a place (see `act`)
            Nav::Restart(_) => self.screen(),
            // everything the open screen shows becomes due now (the caller fetches right after)
            Nav::Refresh => {
                for (k, _) in self.needs() {
                    self.last.remove(k);
                }
                self.refreshing = Some(Instant::now());
                self.screen()
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
                Screen::Container(_) => Screen::Services,
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
    pub(crate) fn needs(&self) -> Vec<(&'static str, Duration)> {
        let s = Duration::from_secs;
        let mut v = vec![("home", s(5))];
        match self.screen() {
            Screen::Home => {}
            Screen::Cpu => v.extend([("cpu", s(2)), ("cpu_trend", s(60))]),
            Screen::Temps => v.extend([("temps", s(5)), ("temp_trend", s(60))]),
            Screen::Memory => v.push(("mem", s(5))),
            Screen::Disk => v.push(("disk", s(3))),
            Screen::History if self.hist_tab == screens::HistTab::Events => {
                v.push(("events", s(30)))
            }
            Screen::History => v.push(("incidents", s(10))),
            Screen::Incident(_) => v.push(("incident", s(10))),
            Screen::Services => v.push(("services", s(5))),
            Screen::Container(_) => v.push(("services", s(3))),
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
                "services" => {
                    self.services = super::get_json(socket, "/details/services").ok();
                    if let Screen::Container(_) = self.screen()
                        && let Some(c) = self.open_container()
                    {
                        let (cpu, mem) = (c.cpu, c.mem);
                        for (q, v) in [
                            (&mut self.svc_cpu, cpu),
                            (&mut self.svc_mem, mem.map(|m| m as f64)),
                        ] {
                            if let Some(v) = v {
                                q.push_back(v as f32);
                                while q.len() > IO_POINTS {
                                    q.pop_front();
                                }
                            }
                        }
                    }
                }
                "events" => {
                    self.events = super::get_json::<EventsOut>(socket, "/events")
                        .ok()
                        .map(|l| l.events)
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

    /// A hold target fired: asks the agent to restart the open container, then refreshes so the
    /// button shows "Please wait" at once. Samples only log it.
    pub fn act(&mut self, nav: Nav, socket: &Path, now: Instant) {
        let Nav::Restart(k) = nav else {
            return self.go(nav);
        };
        let Some(id) = self
            .open_container()
            .filter(|s| key(&s.id) == k)
            .map(|s| s.id.clone())
        else {
            return;
        };
        self.holding = None;
        if !self.live {
            eprintln!(
                "pios ui: restart {} (sample: nothing sent)",
                &id[..12.min(id.len())]
            );
            return;
        }
        let path = format!("/containers/{id}/restart");
        self.refused = match crate::http::post(socket, &path, Duration::from_secs(3)) {
            Ok((202, _)) => None,
            Ok((_, body)) => Some((
                k,
                serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("error")?.as_str().map(String::from))
                    .unwrap_or_else(|| "refused".into()),
            )),
            Err(_) => Some((k, "agent not answering".into())),
        };
        self.last.remove("services");
        self.update(socket, now);
    }

    /// Services opens on the first tab with a problem (from Home's data), else Apps.
    fn problem_tab(&self) -> screens::Tab {
        let Some(s) = &self.home.snapshot else {
            return screens::Tab::Apps;
        };
        if s.docker.as_ref().is_none_or(|d| d.running < d.total) {
            screens::Tab::Apps
        } else if s.units.as_ref().is_some_and(|u| u.running < u.total) {
            screens::Tab::Host
        } else if s.web.as_ref().is_some_and(|w| w.ok < w.total) {
            screens::Tab::Web
        } else {
            screens::Tab::Apps
        }
    }

    /// The container the open screen shows (None: not loaded yet, or it was removed).
    pub fn open_container(&self) -> Option<&Service> {
        let Screen::Container(k) = self.screen() else {
            return None;
        };
        let d = self.services.as_ref()?;
        d.services.iter().chain(&d.host).find(|s| key(&s.id) == k)
    }

    /// The refresh icon is spinning (the panel redraws often meanwhile).
    pub fn spinning(&self, now: Instant) -> bool {
        self.refreshing
            .is_some_and(|t| now.saturating_duration_since(t) < REFRESH_SHOWN)
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
        let mut t = self.screen_tree(f, now_ms, time, date, at);
        if let Some(start) = self.refreshing.filter(|_| self.spinning(Instant::now())) {
            let angle = start.elapsed().as_secs_f32() * 450.0 % 360.0;
            for it in t.items.iter_mut().filter(|i| i.name == "refresh icon") {
                it.paint = super::layout::Paint::Spinner {
                    angle,
                    color: super::theme::INK,
                };
            }
        }
        t
    }

    fn screen_tree(
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
            Screen::History => screens::history(
                f,
                self.incidents.as_deref(),
                self.events.as_deref(),
                self.hist_tab,
                self.page,
                now_ms,
                at,
            ),
            Screen::Incident(_) => screens::incident(f, self.incident.as_ref(), now_ms, at),
            Screen::Services => {
                screens::services(f, self.services.as_ref(), self.tab, self.page, at)
            }
            Screen::Container(_) => {
                let cpu: Vec<f32> = self.svc_cpu.iter().copied().collect();
                let mem: Vec<f32> = self.svc_mem.iter().copied().collect();
                let k = match self.screen() {
                    Screen::Container(k) => k,
                    _ => 0,
                };
                screens::container(
                    f,
                    self.open_container(),
                    &cpu,
                    &mem,
                    self.services.is_some(),
                    screens::Hold {
                        progress: self.holding,
                        refused: self
                            .refused
                            .as_ref()
                            .filter(|(rk, _)| *rk == k)
                            .map(|(_, w)| w.as_str()),
                    },
                )
            }
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
    fn refresh_makes_the_open_screen_due_now() {
        let mut a = App::live();
        a.go(Nav::Cpu);
        let now = Instant::now();
        for (k, _) in a.needs() {
            a.last.insert(k, now);
        }
        assert!(a.next_due(now) > Duration::ZERO);
        a.go(Nav::Refresh);
        assert_eq!(a.screen(), Screen::Cpu, "refresh stays on the screen");
        assert_eq!(a.next_due(now), Duration::ZERO);
        // its icon spins (and the screen still passes the spacing check)
        assert!(a.spinning(Instant::now()));
        let f = Fonts::load();
        let t = a.tree(&f, 0, "9:00 AM", "Sun 22 Nov", &crate::ui::utc_at);
        let icon = t.items.iter().find(|i| i.name == "refresh icon").unwrap();
        assert!(matches!(
            icon.paint,
            crate::ui::layout::Paint::Spinner { .. }
        ));
        assert!(crate::ui::layout::check(&t).is_empty());
        a.refreshing = Some(Instant::now() - REFRESH_SHOWN);
        assert!(!a.spinning(Instant::now()));
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
