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
    Fans,
    Settings,
    Ring,
    Display,
    Night,
    /// the dim clock shown at night when nobody touches the screen
    NightFace,
    Alerts,
    AlertTypes,
}

impl Screen {
    pub fn parse(name: &str) -> Option<Screen> {
        Some(match name {
            "home" => Screen::Home,
            "cpu" => Screen::Cpu,
            "temps" => Screen::Temps,
            "memory" => Screen::Memory,
            "disk" | "data" => Screen::Disk,
            "history" => Screen::History,
            "incident" => Screen::Incident(1),
            "services" | "host" | "web" => Screen::Services,
            "events" => Screen::History,
            "fans" => Screen::Fans,
            "settings" => Screen::Settings,
            "ring" => Screen::Ring,
            "display" => Screen::Display,
            "night" => Screen::Night,
            "nightface" => Screen::NightFace,
            "alerts" => Screen::Alerts,
            "alerttypes" => Screen::AlertTypes,
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

/// `--screen data`: the Disk screen on its Data tab.
pub fn disk_tab_for(name: &str) -> screens::DiskTab {
    if name == "data" {
        screens::DiskTab::Data
    } else {
        screens::DiskTab::Ssd
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

pub const SCREENS: [&str; 21] = [
    "home",
    "cpu",
    "temps",
    "memory",
    "disk",
    "data",
    "history",
    "events",
    "incident",
    "services",
    "host",
    "web",
    "container",
    "fans",
    "settings",
    "ring",
    "display",
    "night",
    "nightface",
    "alerts",
    "alerttypes",
];

/// The refresh icon spins at least this long, so a fast fetch is still seen to happen.
pub const REFRESH_SHOWN: Duration = Duration::from_millis(800);

/// − / + taps are sent once they stop for this long (one change instead of five).
pub const FAN_DEBOUNCE: Duration = Duration::from_millis(700);
/// A sent change is shown until fan_ctl reports it, or for at most this long.
const FAN_SHOWN: Duration = Duration::from_secs(6);

/// A fan change made on the screen, not yet confirmed by fan_ctl's status.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FanPending {
    /// the manual value wanted, or None for auto
    pub want: Option<u32>,
    pub at: Instant,
    pub sent: bool,
}

/// The values − / + walk through: case fan Off, then its floor … 100 % in 5 % steps;
/// Active Cooler 0 … its top level.
pub fn fan_steps(which: u8, t: &TempDetails) -> Vec<u32> {
    if which == 0 {
        let floor = t.case_fan.as_ref().map_or(60, |c| {
            (c.min_duty.clamp(0.0, 100.0) / 5.0).ceil() as u32 * 5
        });
        std::iter::once(0)
            .chain((floor.max(5)..=100).step_by(5))
            .collect()
    } else {
        let top = t.fan.as_ref().map_or(4, |f| f.steps.max(1));
        (0..=top).collect()
    }
}

/// The fan's speed now, as one of `fan_steps` (case duty rounded up to the next step).
fn fan_now(which: u8, t: &TempDetails) -> u32 {
    let steps = fan_steps(which, t);
    let now = if which == 0 {
        t.case_fan.as_ref().map_or(100.0, |c| c.duty)
    } else {
        t.fan.as_ref().map_or(0.0, |f| f.step as f64)
    };
    steps
        .iter()
        .copied()
        .find(|v| *v as f64 >= now - 0.5)
        .unwrap_or(*steps.last().unwrap_or(&0))
}

/// One fan's manual-control state (None: fan_ctl < 0.2, stopped, or no such fan).
pub fn fan_control(which: u8, t: &TempDetails) -> Option<&crate::details::Control> {
    if which == 0 {
        t.case_fan.as_ref()?.control.as_ref()
    } else {
        t.fan.as_ref()?.control.as_ref()
    }
}

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
    /// the data disk's I/O, one point per refresh
    pub data_reads: VecDeque<f32>,
    pub data_writes: VecDeque<f32>,
    /// the Disk screen's open tab
    pub disk_tab: screens::DiskTab,
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
    /// the LED ring's choices, where they're saved, and a pattern being previewed
    pub ring: super::ring::Settings,
    pub ring_path: Option<std::path::PathBuf>,
    pub ring_preview: Option<(super::ring::Role, super::ring::Pattern, Instant)>,
    /// display, clock and night settings, and where they're saved
    pub prefs: super::prefs::Prefs,
    pub prefs_path: Option<std::path::PathBuf>,
    /// phone alerts: what pios web reports, settings to hand the agent, a test to queue
    pub alert_status: Option<crate::alerts::Status>,
    pub alert_sync: bool,
    pub alert_test: Option<Instant>,
    /// where Fans' back button goes (Temperatures or Settings)
    fans_from: Option<Screen>,
    /// fan changes from the Fans screen (0 case, 1 cooler), until fan_ctl reports them
    pub fan_pending: [Option<FanPending>; 2],
    /// the agent refused the last fan change (shown on the Fans screen)
    pub fan_error: Option<String>,
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
            Nav::Tab(i) if self.screen() == Screen::Disk => {
                self.disk_tab = if i == 0 {
                    screens::DiskTab::Ssd
                } else {
                    screens::DiskTab::Data
                };
                Screen::Disk
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
            Nav::FanAuto(w) | Nav::FanManual(w) | Nav::FanStep(w, _) => {
                self.fan_change(nav, (w as usize).min(1));
                self.screen()
            }
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
            Nav::Fans => {
                self.fans_from = Some(self.screen());
                Screen::Fans
            }
            Nav::Settings => Screen::Settings,
            Nav::Display => Screen::Display,
            Nav::Night => Screen::Night,
            Nav::Pref(c) => {
                if let super::prefs::Change::FanFor(_) = c {
                    self.resend_manual();
                }
                self.prefs.apply(c);
                self.prefs.save(self.prefs_path.as_deref());
                super::set_clock24(self.prefs.clock24);
                // alert switches and the night window go to the agent (it applies them)
                self.alert_sync = true;
                self.screen()
            }
            Nav::Alerts => Screen::Alerts,
            Nav::AlertTypes => Screen::AlertTypes,
            Nav::AlertTest => {
                self.alert_test = Some(Instant::now());
                self.last.remove("alerts");
                self.screen()
            }
            Nav::Ring => Screen::Ring,
            Nav::RingSet(row, choice) => {
                use super::ring::{ATTENTION, BRIGHTNESS, DOWN, GOOD, Role};
                let c = (choice as usize).min(2);
                let preview = match row {
                    0 => {
                        self.ring.good = GOOD[c];
                        Some((Role::Good, GOOD[c]))
                    }
                    1 => {
                        self.ring.attention = ATTENTION[c];
                        Some((Role::Attention, ATTENTION[c]))
                    }
                    2 => {
                        self.ring.down = DOWN[c];
                        Some((Role::Down, DOWN[c]))
                    }
                    _ => {
                        self.ring.brightness = c.min(BRIGHTNESS.len() - 1);
                        Some((Role::Attention, self.ring.attention))
                    }
                };
                self.ring.save(self.ring_path.as_deref());
                self.ring_preview = preview.map(|(r, p)| (r, p, Instant::now()));
                Screen::Ring
            }
            Nav::Back => match self.screen() {
                Screen::Incident(_) => Screen::History,
                Screen::Container(_) => Screen::Services,
                Screen::Fans => self.fans_from.take().unwrap_or(Screen::Temps),
                Screen::Ring | Screen::Display | Screen::Night | Screen::Alerts => Screen::Settings,
                Screen::AlertTypes => Screen::Alerts,
                _ => Screen::Home,
            },
        };
        if next == Screen::Disk && self.screen() != Screen::Disk {
            self.reads.clear();
            self.writes.clear();
            self.data_reads.clear();
            self.data_writes.clear();
            self.disk_tab = self.problem_disk_tab();
        }
        self.screen = Some(next);
    }

    /// The value a fan's controls show: a change not yet confirmed, else fan_ctl's own.
    /// Some(None) = auto, Some(Some(v)) = manual at v, None = no control.
    pub fn fan_shown(&self, which: usize) -> Option<Option<u32>> {
        let t = self.temps.as_ref()?;
        let c = fan_control(which as u8, t)?;
        if let Some(p) = self.fan_pending[which] {
            return Some(p.want);
        }
        Some(c.manual)
    }

    /// − / + / Auto / Manual on the Fans screen: only remembered here; `update` sends it.
    fn fan_change(&mut self, nav: Nav, w: usize) {
        let Some(t) = self.temps.as_ref() else {
            return;
        };
        if fan_control(w as u8, t).is_none() {
            return;
        }
        let steps = fan_steps(w as u8, t);
        let base = || {
            self.fan_shown(w)
                .flatten()
                .unwrap_or_else(|| fan_now(w as u8, t))
        };
        let want = match nav {
            Nav::FanAuto(_) => None,
            Nav::FanManual(_) => Some(base()),
            Nav::FanStep(_, d) => {
                let b = base();
                let i = steps
                    .iter()
                    .position(|v| *v >= b)
                    .unwrap_or(steps.len() - 1) as i64;
                let i = (i + d as i64).clamp(0, steps.len() as i64 - 1) as usize;
                Some(steps[i])
            }
            _ => return,
        };
        let now = Instant::now();
        self.fan_error = None;
        self.fan_pending[w] = Some(FanPending {
            want,
            // Auto and Manual go at once; steps wait for the taps to stop
            at: if matches!(nav, Nav::FanStep(..)) {
                now
            } else {
                now.checked_sub(FAN_DEBOUNCE).unwrap_or(now)
            },
            sent: false,
        });
    }

    /// A new "manual lasts" choice applies to a speed already held, too.
    fn resend_manual(&mut self) {
        let Some(t) = self.temps.as_ref() else {
            return;
        };
        for (w, pending) in self.fan_pending.iter_mut().enumerate() {
            if pending.is_none()
                && let Some(v) = fan_control(w as u8, t).and_then(|c| c.manual)
            {
                let now = Instant::now();
                *pending = Some(FanPending {
                    want: Some(v),
                    at: now.checked_sub(FAN_DEBOUNCE).unwrap_or(now),
                    sent: false,
                });
            }
        }
    }

    /// Sends fan changes whose taps have stopped; forgets the ones fan_ctl now reports.
    fn send_fans(&mut self, socket: &Path, now: Instant) {
        for w in 0..2 {
            let Some(mut p) = self.fan_pending[w] else {
                continue;
            };
            if p.sent {
                let done = self
                    .temps
                    .as_ref()
                    .and_then(|t| fan_control(w as u8, t))
                    .is_none_or(|c| c.manual == p.want);
                if done || now.saturating_duration_since(p.at) >= FAN_SHOWN {
                    self.fan_pending[w] = None;
                }
                continue;
            }
            if now.saturating_duration_since(p.at) < FAN_DEBOUNCE {
                continue;
            }
            let fan = if w == 0 { "case" } else { "cooler" };
            let path = match p.want {
                None => format!("/fans/{fan}/auto"),
                Some(v) => format!(
                    "/fans/{fan}?{}={v}&for={}",
                    if w == 0 { "duty" } else { "level" },
                    super::prefs::FAN_FOR[self.prefs.fan_for.min(2)].1
                ),
            };
            if !self.live {
                eprintln!("pios ui: fan {path} (sample: nothing sent)");
                self.fan_pending[w] = None;
                continue;
            }
            match crate::http::post(socket, &path, Duration::from_secs(4)) {
                Ok((200, _)) => {
                    p.sent = true;
                    p.at = now;
                    self.fan_pending[w] = Some(p);
                    // fan_ctl applies it within about a second: look again soon after
                    let soon = Duration::from_secs(5) - Duration::from_millis(1_200);
                    self.last
                        .insert("temps", now.checked_sub(soon).unwrap_or(now));
                }
                Ok((_, body)) => {
                    self.fan_error = Some(
                        serde_json::from_str::<serde_json::Value>(&body)
                            .ok()
                            .and_then(|v| v.get("error")?.as_str().map(String::from))
                            .unwrap_or_else(|| "refused".into()),
                    );
                    self.fan_pending[w] = None;
                }
                Err(_) => {
                    self.fan_error = Some("agent not answering".into());
                    self.fan_pending[w] = None;
                }
            }
        }
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
            Screen::Fans => v.push(("temps", s(5))),
            Screen::Settings | Screen::Ring | Screen::Display | Screen::Night => {}
            Screen::NightFace => {}
            Screen::Alerts => v.push(("alerts", s(5))),
            Screen::AlertTypes => {}
            Screen::Container(_) => v.push(("services", s(3))),
        }
        v
    }

    /// Fetches what's due for the open screen. Returns true if anything was fetched.
    pub fn update(&mut self, socket: &Path, now: Instant) -> bool {
        if self.screen() == Screen::Fans || self.fan_pending.iter().any(Option::is_some) {
            self.send_fans(socket, now);
        }
        if !self.live {
            return false;
        }
        let mut any = false;
        if self.alert_sync {
            let q = self.prefs.alert_query();
            if crate::http::post(
                socket,
                &format!("/alerts/prefs?{q}"),
                Duration::from_secs(2),
            )
            .is_ok()
            {
                self.alert_sync = false;
            }
        }
        if let Some(t) = self.alert_test
            && self.last.get("alert_test").is_none_or(|l| *l < t)
        {
            let _ = crate::http::post(socket, "/alerts/test", Duration::from_secs(2));
            self.last.insert("alert_test", now);
        }
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
                        let (dr, dw) = d
                            .data
                            .as_ref()
                            .map_or((0.0, 0.0), |x| (x.read_bps, x.write_bps));
                        for (q, v) in [
                            (&mut self.reads, d.read_bps),
                            (&mut self.writes, d.write_bps),
                            (&mut self.data_reads, dr),
                            (&mut self.data_writes, dw),
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
                "alerts" => {
                    self.alert_status = super::get_json(socket, "/alerts/status").ok();
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
    /// The Disk screen opens on Data when the data disk has a problem (as last seen).
    fn problem_disk_tab(&self) -> screens::DiskTab {
        match self.disk.as_ref().and_then(|d| d.data.as_ref()) {
            Some(dd) if screens::data_status(dd) != super::theme::Status::Good => {
                screens::DiskTab::Data
            }
            _ => screens::DiskTab::Ssd,
        }
    }

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
        let fans = self.fan_pending.iter().flatten().map(|p| {
            let wait = if p.sent { FAN_SHOWN } else { FAN_DEBOUNCE };
            (p.at + wait).saturating_duration_since(now)
        });
        self.needs()
            .into_iter()
            .map(|(k, every)| {
                self.last.get(k).map_or(Duration::ZERO, |t| {
                    (*t + every).saturating_duration_since(now)
                })
            })
            .chain(fans)
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
        let data_reads: Vec<f32> = self.data_reads.iter().copied().collect();
        let data_writes: Vec<f32> = self.data_writes.iter().copied().collect();
        match self.screen() {
            Screen::Home => Home::build(&self.home, time, date).tree(f),
            Screen::Cpu => screens::cpu(f, self.cpu.as_ref(), &self.cpu_trend),
            Screen::Temps => screens::temps(f, self.temps.as_ref(), &self.temp_trend),
            Screen::Memory => screens::memory(f, self.mem.as_ref()),
            Screen::Disk => screens::disk(
                f,
                self.disk.as_ref(),
                self.disk_tab,
                screens::DiskIo {
                    reads: &reads,
                    writes: &writes,
                    data_reads: &data_reads,
                    data_writes: &data_writes,
                },
            ),
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
            Screen::Fans => screens::fans(
                f,
                self.temps.as_ref(),
                screens::FanUi {
                    shown: [self.fan_shown(0), self.fan_shown(1)],
                    fan_for: self.prefs.fan_for,
                    error: self.fan_error.as_deref(),
                    now_s: now_ms / 1000,
                },
            ),
            Screen::Settings => screens::settings(f, &self.ring, &self.prefs),
            Screen::Display => screens::display_screen(f, &self.prefs),
            Screen::Night => screens::night_screen(f, &self.prefs),
            Screen::Alerts => screens::alerts_screen(
                f,
                &self.prefs,
                self.alert_status.as_ref(),
                self.alert_test
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(30)),
                at,
            ),
            Screen::AlertTypes => screens::alert_types_screen(f, &self.prefs),
            Screen::NightFace => {
                let h = Home::build(&self.home, time, date);
                let temp = self
                    .home
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.cpu.temp_c)
                    .map(|t| format!(" · {t:.0} °C"))
                    .unwrap_or_default();
                screens::night_face(f, h.status, &format!("{}{temp}", h.headline), time)
            }
            Screen::Ring => screens::ring_screen(f, &self.ring),
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
