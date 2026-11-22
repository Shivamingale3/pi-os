//! Incident rules. Pure logic: snapshots in, actions out. No clock, no I/O, so it's fully testable.
//!
//! A condition is "bad" (open it), "clear" (it's over) or neither (hysteresis band: keep as is).
//!   opens  after it has been bad   for OPEN_AFTER_MS without a break (shorter spikes are ignored)
//!   closes after it has been clear for CLOSE_AFTER_MS without a break
//! Metric: bad = value ≥ warn, clear = value < warn − CLOSE_MARGIN.
//! Container: bad = seen running earlier in this run, now not running. Docker: bad = was
//! reachable earlier in this run, now isn't. (Something that was never up isn't an incident.)

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::types::Snapshot;

pub const OPEN_AFTER_MS: i64 = 30_000;
pub const CLOSE_AFTER_MS: i64 = 60_000;
pub const CLOSE_MARGIN: f64 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warn,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Warn => "warn",
            Severity::Critical => "critical",
        }
    }
}

pub struct Threshold {
    pub metric: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub warn: f64,
    pub critical: f64,
    pub read: fn(&Snapshot) -> Option<f64>,
}

fn cpu_percent(s: &Snapshot) -> Option<f64> {
    Some(s.cpu.percent)
}
fn mem_percent(s: &Snapshot) -> Option<f64> {
    Some(s.memory.used_percent)
}
fn disk_percent(s: &Snapshot) -> Option<f64> {
    Some(s.disk.used_percent)
}
fn cpu_temp(s: &Snapshot) -> Option<f64> {
    s.cpu.temp_c
}

/// Same values PiDex used.
pub const THRESHOLDS: [Threshold; 4] = [
    Threshold {
        metric: "cpu.percent",
        label: "CPU",
        unit: "%",
        warn: 80.0,
        critical: 95.0,
        read: cpu_percent,
    },
    Threshold {
        metric: "mem.usedPercent",
        label: "RAM",
        unit: "%",
        warn: 85.0,
        critical: 95.0,
        read: mem_percent,
    },
    Threshold {
        metric: "disk.usedPercent",
        label: "Disk",
        unit: "%",
        warn: 85.0,
        critical: 95.0,
        read: disk_percent,
    },
    Threshold {
        metric: "cpu.tempC",
        label: "CPU temperature",
        unit: "°C",
        warn: 75.0,
        critical: 85.0,
        read: cpu_temp,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Metric,
    Container,
    Docker,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Metric => "metric",
            Kind::Container => "container",
            Kind::Docker => "docker",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Opened {
    pub key: String,
    pub kind: Kind,
    pub subject: String,
    pub title: String,
    pub severity: Severity,
    pub threshold: Option<f64>,
    /// first bad reading, not the moment it was confirmed
    pub started_at: i64,
    pub start_snapshot: Snapshot,
    pub peak_value: Option<f64>,
    pub peak_at: i64,
    pub peak_snapshot: Snapshot,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Open(Box<Opened>),
    Escalate {
        key: String,
        severity: Severity,
    },
    Peak {
        key: String,
        value: f64,
        at: i64,
        snapshot: Box<Snapshot>,
    },
    /// `ended_at` = first clear reading (when it actually recovered), with that snapshot.
    Close {
        key: String,
        ended_at: i64,
        snapshot: Box<Snapshot>,
        reason: &'static str,
    },
}

struct Cond {
    key: String,
    kind: Kind,
    subject: String,
    title: String,
    bad: bool,
    clear: bool,
    value: Option<f64>,
    severity: Severity,
    threshold: Option<f64>,
}

enum State {
    Pending {
        since: i64,
        first: Box<Snapshot>,
        severity: Severity,
        peak: Option<(f64, i64, Box<Snapshot>)>,
    },
    Open {
        severity: Severity,
        peak: Option<f64>,
        clear_since: Option<(i64, Box<Snapshot>)>,
    },
}

#[derive(Default)]
pub struct Engine {
    states: HashMap<String, State>,
    seen_running: HashSet<String>,
    docker_seen: bool,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn open_keys(&self) -> Vec<String> {
        self.states
            .iter()
            .filter(|(_, st)| matches!(st, State::Open { .. }))
            .map(|(k, _)| k.clone())
            .collect()
    }

    pub fn step(&mut self, s: &Snapshot) -> Vec<Action> {
        let mut actions = Vec::new();
        for cond in self.conditions(s, &mut actions) {
            self.apply(cond, s, &mut actions);
        }
        actions
    }

    /// Closes everything still open (agent shutting down).
    pub fn close_all(&mut self, s: &Snapshot, reason: &'static str) -> Vec<Action> {
        let mut out = Vec::new();
        for (key, st) in self.states.drain() {
            if let State::Open { .. } = st {
                out.push(Action::Close {
                    key,
                    ended_at: s.ts,
                    snapshot: Box::new(s.clone()),
                    reason,
                });
            }
        }
        out
    }

    fn conditions(&mut self, s: &Snapshot, actions: &mut Vec<Action>) -> Vec<Cond> {
        let mut out = Vec::new();
        for t in &THRESHOLDS {
            let Some(v) = (t.read)(s) else { continue };
            out.push(Cond {
                key: format!("metric:{}", t.metric),
                kind: Kind::Metric,
                subject: t.metric.to_string(),
                title: format!("{} over {}{}", t.label, t.warn, t.unit),
                bad: v >= t.warn,
                clear: v < t.warn - CLOSE_MARGIN,
                value: Some(v),
                severity: if v >= t.critical {
                    Severity::Critical
                } else {
                    Severity::Warn
                },
                threshold: Some(t.warn),
            });
        }

        let flag = |key: String, kind, subject: String, title: String, bad: bool| Cond {
            key,
            kind,
            subject,
            title,
            bad,
            clear: !bad,
            value: None,
            severity: Severity::Critical,
            threshold: None,
        };

        match &s.docker {
            Some(d) => {
                self.docker_seen = true;
                out.push(flag(
                    "docker".into(),
                    Kind::Docker,
                    "docker".into(),
                    "Docker unreachable".into(),
                    false,
                ));
                for c in &d.containers {
                    let running = c.state == "running";
                    if running {
                        self.seen_running.insert(c.name.clone());
                    }
                    if running || self.seen_running.contains(&c.name) {
                        out.push(flag(
                            format!("container:{}", c.name),
                            Kind::Container,
                            c.name.clone(),
                            format!("{} stopped", c.name),
                            !running,
                        ));
                    }
                }
                // Containers that vanished (docker rm): close, forget.
                let gone: Vec<String> = self
                    .seen_running
                    .iter()
                    .filter(|n| !d.containers.iter().any(|c| &c.name == *n))
                    .cloned()
                    .collect();
                for name in gone {
                    self.seen_running.remove(&name);
                    let key = format!("container:{name}");
                    if let Some(State::Open { .. }) = self.states.remove(&key) {
                        actions.push(Action::Close {
                            key,
                            ended_at: s.ts,
                            snapshot: Box::new(s.clone()),
                            reason: "container removed",
                        });
                    }
                }
            }
            None if self.docker_seen => {
                out.push(flag(
                    "docker".into(),
                    Kind::Docker,
                    "docker".into(),
                    "Docker unreachable".into(),
                    true,
                ));
            }
            None => {}
        }
        out
    }

    fn apply(&mut self, c: Cond, s: &Snapshot, actions: &mut Vec<Action>) {
        let state = self.states.remove(&c.key);
        let next = match state {
            None if c.bad => Some(State::Pending {
                since: s.ts,
                first: Box::new(s.clone()),
                severity: c.severity,
                peak: c.value.map(|v| (v, s.ts, Box::new(s.clone()))),
            }),
            None => None,

            Some(State::Pending {
                since,
                first,
                severity,
                peak,
            }) if c.bad => {
                let severity = severity.max(c.severity);
                let peak = match (peak, c.value) {
                    (Some(p), Some(v)) if v <= p.0 => Some(p),
                    (_, Some(v)) => Some((v, s.ts, Box::new(s.clone()))),
                    (p, None) => p,
                };
                if s.ts - since >= OPEN_AFTER_MS {
                    let (peak_value, peak_at, peak_snapshot) = match peak {
                        Some((v, at, snap)) => (Some(v), at, *snap),
                        None => (None, since, (*first).clone()),
                    };
                    actions.push(Action::Open(Box::new(Opened {
                        key: c.key.clone(),
                        kind: c.kind,
                        subject: c.subject,
                        title: c.title,
                        severity,
                        threshold: c.threshold,
                        started_at: since,
                        start_snapshot: *first,
                        peak_value,
                        peak_at,
                        peak_snapshot,
                    })));
                    Some(State::Open {
                        severity,
                        peak: peak_value,
                        clear_since: None,
                    })
                } else {
                    Some(State::Pending {
                        since,
                        first,
                        severity,
                        peak,
                    })
                }
            }
            // Dropped below warn before 30 s: a spike, not an incident.
            Some(State::Pending { .. }) => None,

            Some(State::Open {
                severity,
                peak,
                clear_since,
            }) => {
                if c.clear {
                    let (since, snap) = clear_since.unwrap_or_else(|| (s.ts, Box::new(s.clone())));
                    if s.ts - since >= CLOSE_AFTER_MS {
                        actions.push(Action::Close {
                            key: c.key.clone(),
                            ended_at: since,
                            snapshot: snap,
                            reason: "recovered",
                        });
                        None
                    } else {
                        Some(State::Open {
                            severity,
                            peak,
                            clear_since: Some((since, snap)),
                        })
                    }
                } else {
                    let mut severity = severity;
                    let mut peak = peak;
                    if c.bad {
                        if c.severity > severity {
                            severity = c.severity;
                            actions.push(Action::Escalate {
                                key: c.key.clone(),
                                severity,
                            });
                        }
                        if let Some(v) = c.value
                            && peak.is_none_or(|p| v > p)
                        {
                            peak = Some(v);
                            actions.push(Action::Peak {
                                key: c.key.clone(),
                                value: v,
                                at: s.ts,
                                snapshot: Box::new(s.clone()),
                            });
                        }
                    }
                    // Back over warn (or in the band): the recovery timer restarts.
                    Some(State::Open {
                        severity,
                        peak,
                        clear_since: None,
                    })
                }
            }
        };
        if let Some(st) = next {
            self.states.insert(c.key, st);
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::types::*;

    pub fn snap(ts: i64, cpu: f64) -> Snapshot {
        Snapshot {
            ts,
            host: Host {
                hostname: "t".into(),
                uptime_sec: 1.0,
                load: [0.0; 3],
            },
            cpu: Cpu {
                percent: cpu,
                temp_c: Some(40.0),
            },
            memory: Memory {
                total_bytes: 100,
                used_bytes: 10,
                available_bytes: 90,
                used_percent: 10.0,
            },
            disk: Disk {
                used_percent: 10.0,
                used_bytes: 1,
                total_bytes: 10,
            },
            network: Network { interfaces: vec![] },
            docker: None,
        }
    }

    fn with_docker(mut s: Snapshot, containers: &[(&str, &str)]) -> Snapshot {
        let containers: Vec<Container> = containers
            .iter()
            .map(|(n, st)| Container {
                id: String::new(),
                name: n.to_string(),
                state: st.to_string(),
            })
            .collect();
        s.docker = Some(Docker {
            running: containers.iter().filter(|c| c.state == "running").count() as u32,
            total: containers.len() as u32,
            containers,
        });
        s
    }

    /// Feeds cpu values every 5 s from t=0, returns all actions.
    fn run(e: &mut Engine, cpu: &[f64]) -> Vec<Action> {
        cpu.iter()
            .enumerate()
            .flat_map(|(i, &v)| e.step(&snap(i as i64 * 5000, v)))
            .collect()
    }

    #[test]
    fn short_spike_is_ignored() {
        let mut e = Engine::new();
        // 25 s over (6 readings: 0..25 s), then down
        let a = run(&mut e, &[90.0, 90.0, 90.0, 90.0, 90.0, 90.0, 10.0, 90.0]);
        assert!(a.is_empty(), "{a:?}");
    }

    #[test]
    fn opens_after_30s_with_start_and_peak() {
        let mut e = Engine::new();
        let a = run(&mut e, &[82.0, 85.0, 88.0, 84.0, 83.0, 81.0, 82.0]);
        assert_eq!(a.len(), 1);
        let Action::Open(o) = &a[0] else {
            panic!("{a:?}")
        };
        assert_eq!(o.started_at, 0);
        assert_eq!(o.start_snapshot.cpu.percent, 82.0);
        assert_eq!(o.peak_value, Some(88.0));
        assert_eq!(o.peak_at, 10_000);
        assert_eq!(o.severity, Severity::Warn);
        assert_eq!(e.open_keys(), ["metric:cpu.percent"]);
    }

    #[test]
    fn peak_and_escalation_after_open() {
        let mut e = Engine::new();
        let a = run(
            &mut e,
            &[85.0; 7]
                .iter()
                .copied()
                .chain([90.0, 97.0, 96.0])
                .collect::<Vec<_>>(),
        );
        assert!(matches!(a[0], Action::Open(_)));
        assert!(matches!(&a[1], Action::Peak { value, .. } if *value == 90.0));
        assert!(matches!(
            &a[2],
            Action::Escalate {
                severity: Severity::Critical,
                ..
            }
        ));
        assert!(matches!(&a[3], Action::Peak { value, .. } if *value == 97.0));
        assert_eq!(a.len(), 4);
    }

    #[test]
    fn closes_after_60s_clear_with_recovery_time() {
        let mut e = Engine::new();
        // open at 30 s; clear from 40 s (value 70 < 75); 60 s later = 100 s → close
        let mut v = vec![90.0; 8]; // 0..35 s
        v.extend([70.0; 12]); // 40..95 s: 55 s clear, still open
        assert!(
            !run(&mut e, &v)
                .iter()
                .any(|x| matches!(x, Action::Close { .. }))
        );
        let a = e.step(&snap(100_000, 70.0)); // 60 s clear → close
        let Some(Action::Close {
            ended_at,
            snapshot,
            reason,
            ..
        }) = a.last()
        else {
            panic!("{a:?}")
        };
        assert_eq!(*ended_at, 40_000);
        assert_eq!(snapshot.ts, 40_000);
        assert_eq!(*reason, "recovered");
        assert!(e.open_keys().is_empty());
    }

    #[test]
    fn hysteresis_band_does_not_close() {
        let mut e = Engine::new();
        let mut v = vec![90.0; 7];
        v.extend([78.0; 20]); // under 80 but not under 75
        let a = run(&mut e, &v);
        assert_eq!(a.len(), 1);
        assert_eq!(e.open_keys().len(), 1);
    }

    #[test]
    fn going_back_over_restarts_the_recovery_timer() {
        let mut e = Engine::new();
        let mut v = vec![90.0; 7];
        v.extend([70.0; 11]); // 55 s clear
        v.push(85.0); // back over
        v.extend([70.0; 12]); // 55 s clear again: still not 60 s
        let a = run(&mut e, &v);
        assert!(
            !a.iter().any(|x| matches!(x, Action::Close { .. })),
            "{a:?}"
        );
    }

    #[test]
    fn container_stopped_only_if_seen_running() {
        let mut e = Engine::new();
        let mut all = vec![];
        // minio was never running: no incident. redis runs, then stops for 30 s.
        all.extend(e.step(&with_docker(
            snap(0, 1.0),
            &[("redis", "running"), ("minio", "exited")],
        )));
        for i in 1..=7 {
            all.extend(e.step(&with_docker(
                snap(i * 5000, 1.0),
                &[("redis", "exited"), ("minio", "exited")],
            )));
        }
        assert_eq!(all.len(), 1, "{all:?}");
        let Action::Open(o) = &all[0] else { panic!() };
        assert_eq!(o.key, "container:redis");
        assert_eq!(o.started_at, 5000);
        assert_eq!(o.peak_value, None);
    }

    #[test]
    fn removed_container_closes() {
        let mut e = Engine::new();
        e.step(&with_docker(snap(0, 1.0), &[("redis", "running")]));
        for i in 1..=7 {
            e.step(&with_docker(snap(i * 5000, 1.0), &[("redis", "exited")]));
        }
        let a = e.step(&with_docker(snap(40_000, 1.0), &[]));
        assert!(
            matches!(
                &a[0],
                Action::Close {
                    reason: "container removed",
                    ..
                }
            ),
            "{a:?}"
        );
    }

    #[test]
    fn docker_unreachable_only_after_seen() {
        let mut e = Engine::new();
        let a: Vec<_> = (0..10).flat_map(|i| e.step(&snap(i * 5000, 1.0))).collect();
        assert!(a.is_empty());

        e.step(&with_docker(snap(50_000, 1.0), &[]));
        let a: Vec<_> = (11..20)
            .flat_map(|i| e.step(&snap(i * 5000, 1.0)))
            .collect();
        assert!(
            matches!(&a[0], Action::Open(o) if o.key == "docker"),
            "{a:?}"
        );
    }

    #[test]
    fn missing_sensor_is_not_an_incident() {
        let mut e = Engine::new();
        let mut s = snap(0, 1.0);
        s.cpu.temp_c = None;
        assert!(e.step(&s).is_empty());
    }

    #[test]
    fn close_all_on_shutdown() {
        let mut e = Engine::new();
        run(&mut e, &[90.0; 8]);
        let a = e.close_all(&snap(99_000, 90.0), "agent stopped");
        assert!(matches!(
            &a[0],
            Action::Close {
                reason: "agent stopped",
                ended_at: 99_000,
                ..
            }
        ));
    }
}
