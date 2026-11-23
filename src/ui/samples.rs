//! Built-in example data for every screen: `pios ui --sample NAME --screen SCREEN`, the spacing
//! check, and the golden images. The repo is public, so names here are generic on purpose.

use super::app::{App, Screen};
use super::home::Data;
use super::screens::{Incident, IncidentFull, Snapshots};
use crate::details::*;
use crate::services::{Service, Services, key};

/// Fixed "now" for samples and golden images (Sun 22 Nov 2026, 10:00 UTC).
pub const NOW_MS: i64 = 1_795_341_600_000;
const MIN: i64 = 60_000;

const GB: u64 = 1 << 30;
const MB: u64 = 1 << 20;

fn curve(n: usize, base: f32, amp: f32, seed: f32, spike: Option<(usize, f32)>) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let x = i as f32 / n as f32 * std::f32::consts::TAU;
            let mut v =
                base + amp * (x + seed).sin() + amp * 0.4 * (i as f32 * 1.7 + seed * 3.0).sin();
            if let Some((at, s)) = spike {
                v += s * (-((i as f32 - at as f32) / 2.0).powi(2)).exp();
            }
            v.max(0.0)
        })
        .collect()
}

fn usage(name: &str, value: f64) -> Usage {
    Usage {
        name: name.into(),
        value,
    }
}

fn good_cpu() -> CpuDetails {
    CpuDetails {
        percent: 7.0,
        cores: vec![
            Core {
                percent: 9.0,
                mhz: 2000,
            },
            Core {
                percent: 5.0,
                mhz: 2000,
            },
            Core {
                percent: 8.0,
                mhz: 1900,
            },
            Core {
                percent: 6.0,
                mhz: 2000,
            },
        ],
        load: [0.08, 0.06, 0.01],
        min_mhz: 1500,
        max_mhz: 2400,
        governor: "ondemand".into(),
        top: vec![
            usage("database", 3.1),
            usage("java", 1.6),
            usage("dockerd", 0.8),
        ],
    }
}

fn good_temps() -> TempDetails {
    TempDetails {
        sensors: vec![
            Sensor {
                name: "CPU".into(),
                celsius: 37.0,
                fan_on: Some(50.0),
                limit: None,
                critical: None,
            },
            Sensor {
                name: "NVMe".into(),
                celsius: 52.85,
                fan_on: None,
                limit: Some(99.85),
                critical: Some(109.85),
            },
            Sensor {
                name: "RP1 I/O chip".into(),
                celsius: 48.5,
                fan_on: None,
                limit: None,
                critical: None,
            },
        ],
        fan: Some(Fan {
            rpm: Some(0),
            step: 0,
            steps: 4,
            starts_at: Some(50.0),
            trips: vec![50.0, 60.0, 67.5, 75.0],
        }),
        case_fan: Some(CaseFan {
            duty: 60.0,
            controller: true,
            min_duty: 60.0,
            nvme: (55.0, 72.0),
            cpu: (60.0, 80.0),
        }),
    }
}

fn good_mem() -> MemDetails {
    MemDetails {
        total: 8_255_792 * 1024,
        available: 6_922_992 * 1024,
        apps: 972_896 * 1024,
        kernel: 166_720 * 1024,
        cache: 1_765_552 * 1024,
        free: (8_255_792 - 972_896 - 166_720 - 1_765_552) * 1024,
        swap: Some(Swap {
            total: 2_097_136 * 1024,
            used: 0,
            kind: "zram · zstd".into(),
        }),
        top: vec![
            usage("app-ci", 571_162_624.0),
            usage("db-docs", 370_278_400.0),
            usage("api-backend", 227_115_008.0),
            usage("object-store", 196_001_792.0),
            usage("web-api", 154_009_600.0),
        ],
        top_kind: "containers".into(),
    }
}

fn good_disk() -> DiskDetails {
    DiskDetails {
        filesystems: vec![
            Fs {
                mount: "/".into(),
                used: 54_815_719_424,
                size: 503_460_380_672,
                percent: 12.0,
            },
            Fs {
                mount: "/boot/firmware".into(),
                used: 68_063_232,
                size: 528_592_896,
                percent: 12.9,
            },
        ],
        drive: Some(Drive {
            model: "SAMPLE NVME 512G".into(),
            size: 512_110_190_592,
            kind: "NVMe".into(),
            written: 38_654_705_664,
        }),
        read_bps: 0.0,
        write_bps: 200_704.0,
        docker: Some(DockerDf {
            images: 34,
            images_bytes: 11_430 * MB,
            reclaimable: 1_759 * MB,
            containers: 9,
            containers_bytes: 34 * MB,
            volumes: 5,
            volumes_bytes: 45 * 1024,
        }),
    }
}

#[allow(clippy::too_many_arguments)] // test data: one row per field
fn incident(
    id: i64,
    kind: &str,
    subject: &str,
    title: &str,
    severity: &str,
    start_min_ago: i64,
    len_min: Option<i64>,
    peak: Option<f64>,
) -> Incident {
    let started_at = NOW_MS - start_min_ago * MIN;
    Incident {
        id,
        kind: kind.into(),
        subject: subject.into(),
        title: title.into(),
        severity: severity.into(),
        threshold: None,
        started_at,
        peak_at: peak.map(|_| started_at + 2 * MIN),
        peak_value: peak,
        ended_at: len_min.map(|l| started_at + l * MIN),
        end_reason: len_min.map(|_| "recovered".into()),
    }
}

fn good_incidents() -> Vec<Incident> {
    vec![
        incident(
            4,
            "metric",
            "cpu.percent",
            "CPU over 80%",
            "critical",
            65,
            Some(6),
            Some(97.0),
        ),
        incident(
            3,
            "container",
            "api-backend",
            "api-backend stopped",
            "critical",
            300,
            Some(4),
            None,
        ),
        incident(
            2,
            "metric",
            "disk.usedPercent",
            "Disk over 85%",
            "warn",
            1500,
            Some(95),
            Some(88.0),
        ),
        incident(
            1,
            "metric",
            "cpu.tempC",
            "CPU temperature over 75°C",
            "warn",
            4000,
            Some(12),
            Some(79.0),
        ),
    ]
}

fn full(i: Incident, data: &Data, ended: bool) -> IncidentFull {
    let s = data.snapshot.clone();
    let peak_v = i.peak_value.unwrap_or(90.0);
    let at = |ts: i64, v: f64| {
        s.clone().map(|mut x| {
            x.ts = ts;
            match i.subject.as_str() {
                "cpu.percent" => x.cpu.percent = v,
                "cpu.tempC" => x.cpu.temp_c = Some(v),
                "mem.usedPercent" => x.memory.used_percent = v,
                "disk.usedPercent" => x.disk.used_percent = v,
                _ => {}
            }
            x
        })
    };
    let start = at(i.started_at, peak_v - 2.0);
    let peak = at(i.peak_at.unwrap_or(i.started_at), peak_v);
    let end = if ended {
        // recovered: back below warn − 5
        at(i.ended_at.unwrap_or(NOW_MS), peak_v - 12.0)
    } else {
        None
    };
    IncidentFull {
        incident: i,
        snapshots: Snapshots { start, peak, end },
    }
}

fn worst_cpu() -> CpuDetails {
    CpuDetails {
        percent: 100.0,
        cores: (0..64)
            .map(|i| Core {
                percent: 100.0,
                mhz: 4800 + i,
            })
            .collect(),
        load: [128.55, 99.99, 87.12],
        min_mhz: 400,
        max_mhz: 5800,
        governor: "performance-with-a-very-long-name".into(),
        top: vec![
            usage("an-extremely-long-process-name-indeed", 6400.0),
            usage("x", 999.9),
            usage("y", 0.1),
        ],
    }
}

fn worst_temps() -> TempDetails {
    TempDetails {
        sensors: (0..6)
            .map(|i| Sensor {
                name: format!("Very long sensor name {i}"),
                celsius: 105.0 + i as f64,
                fan_on: Some(50.0),
                limit: Some(100.0),
                critical: Some(110.0),
            })
            .collect(),
        fan: Some(Fan {
            rpm: Some(12_000),
            step: 4,
            steps: 4,
            starts_at: Some(50.0),
            trips: vec![50.0, 60.0, 67.5, 75.0, 80.0, 85.0, 90.0, 95.0],
        }),
        // fan_ctl stopped: the fan sits at 100 %
        case_fan: Some(CaseFan {
            duty: 100.0,
            controller: false,
            min_duty: 0.0,
            nvme: (55.0, 72.0),
            cpu: (60.0, 80.0),
        }),
    }
}

fn worst_mem() -> MemDetails {
    MemDetails {
        total: 2048 * GB * 1024,
        available: 0,
        apps: 2000 * GB * 1024,
        kernel: 40 * GB * 1024,
        cache: 8 * GB * 1024,
        free: 0,
        swap: Some(Swap {
            total: 999 * GB * 1024,
            used: 999 * GB * 1024,
            kind: "a-swap-file-with-a-long-name".into(),
        }),
        top: (0..5)
            .map(|i| {
                usage(
                    &format!("container-with-an-extremely-long-name-{i}"),
                    (900 - i) as f64 * GB as f64 * 1024.0,
                )
            })
            .collect(),
        top_kind: "containers".into(),
    }
}

fn worst_disk() -> DiskDetails {
    DiskDetails {
        filesystems: vec![
            Fs {
                mount: "/".into(),
                used: 999 * GB * 1024 * 1024,
                size: 1000 * GB * 1024 * 1024,
                percent: 99.9,
            },
            Fs {
                mount: "/mnt/a-very-long-mount-point-name/with/many/levels".into(),
                used: 900 * GB * 1024,
                size: 999 * GB * 1024,
                percent: 90.1,
            },
        ],
        drive: Some(Drive {
            model: "A VERY LONG DRIVE MODEL NAME WITH MANY WORDS 8TB".into(),
            size: 8000 * GB,
            kind: "SD card".into(),
            written: 999 * GB * 1024 * 1024,
        }),
        read_bps: 9.9e12,
        write_bps: 9.9e12,
        docker: Some(DockerDf {
            images: 9999,
            images_bytes: 999 * GB * 1024,
            reclaimable: 998 * GB * 1024,
            containers: 9999,
            containers_bytes: 999 * GB * 1024,
            volumes: 9999,
            volumes_bytes: 999 * GB * 1024,
        }),
    }
}

fn worst_incidents() -> Vec<Incident> {
    (0..12)
        .map(|i| {
            incident(
                100 - i,
                "container",
                "x",
                "photo-library-db-replica-secondary-with-a-very-long-name stopped",
                if i % 2 == 0 { "critical" } else { "warn" },
                10 + i * 90,
                if i == 0 { None } else { Some(60 * 26) },
                Some(105.0),
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)] // test data: one row per field
fn svc(
    n: u64,
    name: &str,
    state: &str,
    health: Option<&str>,
    mem_mb: u64,
    up_min: u64,
    image: &str,
    ports: &[&str],
) -> Service {
    let running = state == "running";
    Service {
        id: format!("{n:016x}{}", "0".repeat(48)),
        name: name.into(),
        image: image.into(),
        state: state.into(),
        health: health.map(String::from),
        exit_code: (!running).then_some(137),
        ago: (!running).then(|| "12 m ago".into()),
        project: Some("apps".into()),
        service: Some(name.rsplit('-').next().unwrap_or(name).into()),
        ports: ports.iter().map(|p| p.to_string()).collect(),
        cpu: running.then_some(0.4 + (n % 5) as f64 * 0.8),
        mem: running.then_some(mem_mb * MB),
        mem_limit: (n == 2).then_some(512 * MB),
        up_sec: running.then_some(up_min * 60),
        restart: None,
        host: false,
        tasks: None,
    }
}

/// The samples' host services as the Host tab gets them (`stopped` aren't running).
fn host_services(stopped: &[&str], long: bool) -> Vec<Service> {
    let mem = [259, 9, 55, 58, 13, 159, 7, 8];
    super::SAMPLE_UNITS
        .iter()
        .zip(mem)
        .map(|(n, m)| {
            let running = !stopped.contains(n);
            let name = if long {
                format!("{n}-with-an-extremely-long-unit-name-for-testing")
            } else {
                n.to_string()
            };
            Service {
                id: crate::services::host_id(&name),
                state: if running { "running" } else { "stopped" }.into(),
                host: true,
                image: if running {
                    n.split('-').next().unwrap_or(n).into()
                } else {
                    String::new()
                },
                cpu: running.then_some(0.3),
                mem: running.then_some(if long { 999 * GB * 1024 } else { m * MB }),
                up_sec: running.then_some(if long { 999 * 86400 } else { 248 * 60 }),
                tasks: running.then_some(if long { 99_999 } else { 4 }),
                name,
                ..Service::default()
            }
        })
        .collect()
}

/// The good sample's 9 containers (Home: 9 of 9 running). `stopped`: this one has exited.
fn good_services(stopped: Option<&str>) -> Services {
    let mut v = vec![
        svc(
            1,
            "app-ci",
            "running",
            None,
            349,
            248,
            "ci/server:2.4",
            &["8080 → 8080"],
        ),
        svc(
            2,
            "api-backend",
            "running",
            Some("healthy"),
            146,
            248,
            "ghcr.io/example/api:1.4",
            &["127.0.0.1:8081 → 80"],
        ),
        svc(
            3,
            "database",
            "running",
            Some("healthy"),
            62,
            248,
            "postgres:17",
            &["5432 inside"],
        ),
        svc(
            4,
            "db-docs",
            "running",
            None,
            242,
            248,
            "mongo:8",
            &["127.0.0.1:27017 → 27017"],
        ),
        svc(
            5,
            "object-store",
            "running",
            None,
            135,
            248,
            "minio/minio:latest",
            &["9000 → 9000", "9001 → 9001"],
        ),
        svc(
            6,
            "cache",
            "running",
            None,
            32,
            248,
            "redis:7",
            &["6379 inside"],
        ),
        svc(
            7,
            "web-api",
            "running",
            None,
            114,
            248,
            "ghcr.io/example/web-api:3",
            &["127.0.0.1:3000 → 3000"],
        ),
        svc(
            8,
            "web-front",
            "running",
            None,
            13,
            248,
            "nginx:1.27",
            &["80 → 80", "443 → 443"],
        ),
        svc(
            9,
            "tunnel",
            "running",
            None,
            21,
            248,
            "cloudflare/cloudflared:2026.11",
            &[],
        ),
    ];
    for s in v.iter_mut().filter(|s| Some(s.name.as_str()) == stopped) {
        *s = svc(6, &s.name, "exited", None, 0, 0, &s.image, &[]);
        s.ports.clear();
    }
    Services {
        reachable: true,
        services: v,
        host: host_services(&[], false),
        web: Some(super::sample_web(&[])),
    }
}

/// Every state and the longest values, over several pages.
fn worst_services() -> Services {
    let states = [
        "running",
        "exited",
        "restarting",
        "paused",
        "created",
        "dead",
        "running",
    ];
    let services = (0..40u64)
        .map(|i| {
            let st = states[i as usize % states.len()];
            let mut s = svc(
                i + 1,
                &format!("photo-library-db-replica-secondary-with-a-very-long-name-{i}"),
                st,
                [None, Some("unhealthy"), Some("starting"), Some("healthy")][i as usize % 4],
                999 * 1024 * 1024,
                60 * 24 * 999,
                "registry.example.internal:5000/a/very/long/image/path/name:with-a-long-tag-1.2.3",
                &[
                    "[2001:db8::1]:65535 → 65535/udp",
                    "127.0.0.1:65534 → 65534",
                    "65533 inside",
                ],
            );
            s.cpu = s.cpu.map(|_| 3999.9);
            s.mem_limit = Some(999 * GB * 1024);
            s.exit_code = s.exit_code.map(|_| -2147483648);
            s.ago = s.ago.map(|_| "999 mo ago".into());
            s.project = Some("a-very-long-compose-project-name".into());
            s.service = Some("and-an-equally-long-service-name".into());
            s
        })
        .collect();
    let mut web = super::sample_web(&[]);
    for (i, c) in web.checks.iter_mut().enumerate() {
        c.host = format!("a-very-long-public-hostname-number-{i}.with.many.subdomains.example.com");
        if i % 3 == 0 {
            (c.code, c.ms, c.ok, c.fails) = (None, None, false, 99);
            c.error = Some("Could not resolve host: a very long error message from curl".into());
        } else if i % 3 == 1 {
            (c.code, c.ok, c.fails, c.ms) = (Some(599), false, 2, Some(9_999_999));
        }
    }
    Services {
        reachable: true,
        services,
        host: host_services(&["docker", "ssh", "redis-queue"], true),
        web: Some(web),
    }
}

/// `sample_app` for a screen name from `SCREENS` (`host` and `web` pick the Services tab).
#[cfg(test)]
pub fn sample_named(sample: &str, screen: &str) -> Option<App> {
    let mut a = sample_app(sample, Screen::parse(screen)?)?;
    a.tab = super::app::tab_for(screen);
    a.hist_tab = super::app::hist_tab_for(screen);
    Some(a)
}

fn event(
    min_ago: i64,
    kind: &str,
    user: &str,
    from: &str,
    detail: &str,
    count: u32,
) -> crate::events::Event {
    crate::events::Event {
        ts: NOW_MS - min_ago * MIN,
        kind: kind.into(),
        user: user.into(),
        from: from.into(),
        detail: detail.into(),
        count,
    }
}

/// A plausible week: logins, sudo, deploys, a boot, a burst of failed logins.
fn good_events() -> Vec<crate::events::Event> {
    vec![
        event(19, "ssh", "admin", "100.64.0.7", "", 3),
        event(22, "deploy", "pios-deploy", "", "v0.7.3", 1),
        event(48, "sudo", "admin", "", "systemctl status nginx", 1),
        event(60, "sudo", "admin", "", "apt upgrade", 1),
        event(110, "ssh", "admin", "100.64.0.7", "", 1),
        event(540, "boot", "", "", "2000", 1),
        event(1500, "ssh_failed", "root", "203.0.113.9", "", 12),
        event(1600, "boot", "", "", "13000", 1),
        event(1610, "sudo", "admin", "", "reboot", 1),
        event(3000, "deploy", "pios-deploy", "", "--rollback", 1),
    ]
}

fn worst_events() -> Vec<crate::events::Event> {
    (0..60)
        .map(|i| {
            let kinds = ["ssh", "ssh_failed", "sudo", "deploy", "boot", "other"];
            event(
                i * 97,
                kinds[i as usize % kinds.len()],
                "a-very-long-user-name-for-testing",
                "2001:db8:1234:5678:9abc:def0:1234:5678",
                "/usr/bin/some-very-long-command --with --many --arguments and/a/very/long/path",
                9999,
            )
        })
        .collect()
}

/// An app showing `screen` with the named sample (good, attention, down, waiting, worst).
pub fn sample_app(name: &str, screen: Screen) -> Option<App> {
    let home = super::sample(name)?;
    let mut a = App::default();
    a.screen = Some(screen);
    a.home = home.clone();
    match name {
        "waiting" => {}
        "worst" => {
            a.cpu = Some(worst_cpu());
            a.temps = Some(worst_temps());
            a.mem = Some(worst_mem());
            a.disk = Some(worst_disk());
            a.cpu_trend = curve(96, 90.0, 10.0, 0.3, None);
            a.temp_trend = curve(96, 100.0, 8.0, 1.0, None);
            a.reads = curve(12, 9e12, 1e12, 0.0, None).into();
            a.writes = curve(12, 9e12, 1e12, 1.0, None).into();
            let list = worst_incidents();
            a.incident = Some(IncidentFull {
                incident: list[0].clone(),
                snapshots: Snapshots::default(),
            });
            a.incidents = Some(list);
            a.events = Some(worst_events());
            a.services = Some(worst_services());
            a.svc_cpu = curve(12, 3999.0, 1.0, 0.0, None).into();
            a.svc_mem = curve(12, 9e15, 1e14, 1.0, None).into();
        }
        _ => {
            a.cpu = Some(good_cpu());
            a.temps = Some(good_temps());
            a.mem = Some(good_mem());
            a.disk = Some(good_disk());
            a.cpu_trend = curve(96, 3.0, 1.5, 0.4, Some((60, 11.0)));
            a.temp_trend = curve(96, 38.0, 1.0, 1.2, Some((60, 2.0)));
            a.reads = vec![
                0.0, 0.0, 4096.0, 0.0, 0.0, 12288.0, 0.0, 0.0, 0.0, 4096.0, 0.0, 0.0,
            ]
            .into();
            a.writes = vec![
                120e3, 180e3, 150e3, 210e3, 160e3, 190e3, 240e3, 170e3, 150e3, 200e3, 210e3, 196e3,
            ]
            .into();
            let list = good_incidents();
            a.incident = Some(full(list[2].clone(), &home, true));
            a.incidents = Some(list);
            a.events = Some(good_events());
            a.services = Some(good_services((name == "down").then_some("cache")));
            a.svc_cpu = vec![1.2, 1.4, 1.1, 2.0, 1.8, 1.5, 1.6, 2.4, 1.9, 1.7, 2.1, 2.0].into();
            a.svc_mem = [140, 141, 141, 142, 143, 143, 144, 145, 145, 146, 146, 146]
                .map(|m| (m * MB) as f32)
                .into();
        }
    }
    if let Screen::Container(_) = screen
        && let Some(d) = &a.services
    {
        // good: a healthy one with a limit; down: the stopped one; worst: the first problem
        let pick = d
            .services
            .iter()
            .find(|s| s.name == "api-backend")
            .filter(|_| name != "down")
            .or_else(|| super::screens::sorted(&d.services).first().copied());
        if let Some(s) = pick {
            a.screen = Some(Screen::Container(key(&s.id)));
        }
    }
    if let Screen::Incident(_) = screen
        && let Some(i) = &a.incident
    {
        a.screen = Some(Screen::Incident(i.incident.id));
    }
    Some(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::Restart;
    use crate::ui::app::SCREENS;
    use crate::ui::canvas::Fonts;
    use crate::ui::layout::{self, Nav};
    use crate::ui::{panel, utc_at};
    use std::path::{Path, PathBuf};

    fn tree(a: &App, f: &Fonts) -> layout::Tree {
        let (t, d) = utc_at(NOW_MS);
        a.tree(f, NOW_MS, &t, &d, &utc_at)
    }

    fn golden(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(name)
    }

    /// Every sample on every screen: clean. good + worst of each detail screen: pixel-exact
    /// against tests/golden (`UPDATE_GOLDEN=1 cargo test` after an intended change).
    #[test]
    fn screens_are_clean_and_match_golden() {
        let f = Fonts::load();
        let update = std::env::var_os("UPDATE_GOLDEN").is_some();
        let mut failures = vec![];
        for sample in super::super::SAMPLES {
            for s in SCREENS {
                let a = sample_named(sample, s).unwrap();
                let t = tree(&a, &f);
                let v = layout::check(&t);
                assert!(v.is_empty(), "{sample} · {s}: {v:#?}");
                if s == "home" || !matches!(sample, "good" | "worst") {
                    continue;
                }
                let c = panel::frame(&t, &f, None);
                let path = golden(&format!("{s}-{sample}.png"));
                if update {
                    c.pm.save_png(&path).unwrap();
                    continue;
                }
                let want = tiny_skia::Pixmap::load_png(&path).unwrap_or_else(|e| {
                    panic!("{}: {e} (run with UPDATE_GOLDEN=1)", path.display())
                });
                let diff = want
                    .data()
                    .chunks(4)
                    .zip(c.pm.data().chunks(4))
                    .filter(|(x, y)| x != y)
                    .count();
                if diff > 0 {
                    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/golden-diff");
                    std::fs::create_dir_all(&out).unwrap();
                    c.pm.save_png(out.join(format!("{s}-{sample}.png")))
                        .unwrap();
                    failures.push(format!("{s}-{sample}: {diff} pixels differ"));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// Each screen on many shapes of data: never a collision or an overflow.
    #[test]
    fn detail_screens_never_collide() {
        let f = Fonts::load();
        let mut n = 0;
        let check = |a: &App, what: &str| {
            let v = layout::check(&tree(a, &f));
            assert!(v.is_empty(), "{what}: {v:#?}");
        };
        for base in ["good", "worst"] {
            let mut a = sample_app(base, Screen::Cpu).unwrap();
            for cores in [1usize, 2, 3, 4, 5, 8, 16, 64, 256] {
                for top in [0usize, 1, 3] {
                    let mut c = a.cpu.clone().unwrap();
                    c.cores = (0..cores)
                        .map(|i| Core {
                            percent: (i * 37 % 101) as f64,
                            mhz: if i % 3 == 0 { 0 } else { 3000 },
                        })
                        .collect();
                    c.top.truncate(top);
                    a.cpu = Some(c);
                    for trend in [vec![], vec![5.0], a.cpu_trend.clone()] {
                        a.cpu_trend = trend;
                        check(&a, &format!("{base} cpu cores={cores} top={top}"));
                        n += 1;
                    }
                }
            }
            let mut a = sample_app(base, Screen::Temps).unwrap();
            let all = a.temps.clone().unwrap();
            for k in 0..=all.sensors.len().max(4) {
                for fan in [
                    None,
                    all.fan.clone(),
                    Some(Fan {
                        rpm: None,
                        step: 9,
                        steps: 0,
                        starts_at: None,
                        trips: vec![],
                    }),
                ] {
                    let mut t = all.clone();
                    t.sensors.truncate(k);
                    t.fan = fan;
                    a.temps = Some(t);
                    check(&a, &format!("{base} temps k={k}"));
                    n += 1;
                }
            }
            let mut a = sample_app(base, Screen::Memory).unwrap();
            let m0 = a.mem.clone().unwrap();
            for total in [0u64, 512 * MB, 8 * GB, 4096 * GB * 1024] {
                for swap in [
                    None,
                    m0.swap.clone(),
                    Some(Swap {
                        total: 0,
                        used: 0,
                        kind: "partition".into(),
                    }),
                ] {
                    for k in [0usize, 2, 5] {
                        let mut m = m0.clone();
                        m.total = total;
                        m.available = total / 3;
                        m.swap = swap.clone();
                        m.top.truncate(k);
                        a.mem = Some(m);
                        check(&a, &format!("{base} memory total={total} k={k}"));
                        n += 1;
                    }
                }
            }
            let mut a = sample_app(base, Screen::Disk).unwrap();
            let d0 = a.disk.clone().unwrap();
            for fs in [0usize, 1, 2] {
                for (drive, docker) in [(None, None), (d0.drive.clone(), d0.docker.clone())] {
                    let mut d = d0.clone();
                    d.filesystems.truncate(fs);
                    d.drive = drive.clone();
                    d.docker = docker;
                    a.disk = Some(d);
                    check(&a, &format!("{base} disk fs={fs}"));
                    n += 1;
                }
            }
            let mut a = sample_app(base, Screen::History).unwrap();
            let l0 = a.incidents.clone().unwrap();
            for k in [0usize, 1, 3, 4, 7, 12] {
                for page in [0usize, 1, 2, 9] {
                    a.incidents = Some(l0.iter().cycle().take(k).cloned().collect());
                    a.page = page;
                    check(&a, &format!("{base} history k={k} page={page}"));
                    n += 1;
                }
            }
        }
        assert!(n > 200, "{n}");
    }

    /// Taps: every target on every screen goes where it says; nothing in the gaps.
    #[test]
    fn services_never_collide() {
        let f = Fonts::load();
        let mut n = 0;
        for base in ["good", "down", "worst"] {
            let a0 = sample_app(base, Screen::Services).unwrap();
            let all = a0.services.clone().unwrap();
            for count in 0..=all.services.len().min(20) {
                for reachable in [true, false] {
                    let mut a = sample_app(base, Screen::Services).unwrap();
                    let mut d = all.clone();
                    d.services.truncate(count);
                    d.reachable = reachable;
                    let pages = count.div_ceil(crate::ui::screens::list_rows(count)).max(1);
                    a.services = Some(d);
                    for p in 0..=pages {
                        a.page = p;
                        let v = layout::check(&tree(&a, &f));
                        assert!(v.is_empty(), "{base} services n={count} page={p}: {v:#?}");
                        n += 1;
                    }
                }
            }
            for (si, s) in all.services.iter().enumerate() {
                let mut a = sample_app(base, Screen::Services).unwrap();
                a.go(Nav::Container(key(&s.id)));
                for hist in [0usize, 1, 12] {
                    a.svc_cpu.truncate(hist);
                    a.svc_mem.truncate(hist);
                    let v = layout::check(&tree(&a, &f));
                    assert!(
                        v.is_empty(),
                        "{base} container {} hist={hist}: {v:#?}",
                        s.name
                    );
                    n += 1;
                }
                // every Restart button state (the first 8 cover every container state)
                if si >= 8 {
                    continue;
                }
                let long =
                    "Error response from daemon: Cannot restart container: a very long reason";
                let k = key(&s.id);
                let states = [
                    None,
                    Some(Restart {
                        ago_sec: 3,
                        done: false,
                        error: None,
                    }),
                    Some(Restart {
                        ago_sec: 15,
                        done: true,
                        error: None,
                    }),
                    Some(Restart {
                        ago_sec: 99_999,
                        done: true,
                        error: Some(long.into()),
                    }),
                ];
                for r in states {
                    for (hold, refused) in [
                        (None, None),
                        (Some(0.0), None),
                        (Some(0.6), None),
                        (Some(1.0), None),
                        (None, Some(long)),
                    ] {
                        if let Some(c) = a
                            .services
                            .as_mut()
                            .unwrap()
                            .services
                            .iter_mut()
                            .find(|c| key(&c.id) == k)
                        {
                            c.restart = r.clone();
                        }
                        a.holding = hold;
                        a.refused = refused.map(|w| (k, w.to_string()));
                        let v = layout::check(&tree(&a, &f));
                        assert!(
                            v.is_empty(),
                            "{base} {} restart={r:?} hold={hold:?}: {v:#?}",
                            s.name
                        );
                        n += 1;
                    }
                }
            }
            // removed while open, and not loaded yet
            let mut a = sample_app(base, Screen::Container(0)).unwrap();
            a.screen = Some(Screen::Container(u64::MAX));
            assert!(layout::check(&tree(&a, &f)).is_empty());
            a.services = None;
            assert!(layout::check(&tree(&a, &f)).is_empty());
        }
        assert!(n > 200, "{n}");
    }

    #[test]
    fn host_and_web_never_collide() {
        use crate::ui::screens::{Tab, WEB_ROWS};
        let f = Fonts::load();
        let mut n = 0;
        for base in ["good", "down", "worst"] {
            let all = sample_app(base, Screen::Services)
                .unwrap()
                .services
                .unwrap();
            // Host tab: every count, every page; each one's detail screen
            for count in 0..=all.host.len() {
                let mut a = sample_app(base, Screen::Services).unwrap();
                a.tab = Tab::Host;
                a.services.as_mut().unwrap().host.truncate(count);
                for p in 0..3 {
                    a.page = p;
                    let v = layout::check(&tree(&a, &f));
                    assert!(v.is_empty(), "{base} host n={count} page={p}: {v:#?}");
                    n += 1;
                }
            }
            for s in &all.host {
                let mut a = sample_app(base, Screen::Services).unwrap();
                a.tab = Tab::Host;
                a.go(Nav::Container(key(&s.id)));
                let t = tree(&a, &f);
                assert!(layout::check(&t).is_empty(), "{base} host {}", s.name);
                assert!(
                    !t.items.iter().any(|i| i.name == "restart"),
                    "no Restart for host services"
                );
                n += 1;
            }
            // Web tab: every count, every page, and nothing set up
            let web = all.web.clone().unwrap();
            for count in 0..=web.checks.len() {
                let mut a = sample_app(base, Screen::Services).unwrap();
                a.tab = Tab::Web;
                let mut w = web.clone();
                w.checks.truncate(count);
                a.services.as_mut().unwrap().web = (count > 0).then_some(w);
                for p in 0..=count.div_ceil(WEB_ROWS) {
                    a.page = p;
                    let v = layout::check(&tree(&a, &f));
                    assert!(v.is_empty(), "{base} web n={count} page={p}: {v:#?}");
                    n += 1;
                }
            }
        }
        assert!(n > 60, "{n}");
    }

    #[test]
    fn events_never_collide() {
        use crate::ui::screens::HistTab;
        let f = Fonts::load();
        let mut n = 0;
        for base in ["good", "worst"] {
            let all = sample_app(base, Screen::History).unwrap().events.unwrap();
            for count in [0, 1, 3, 4, 5, 9, all.len()] {
                let mut a = sample_app(base, Screen::History).unwrap();
                a.hist_tab = HistTab::Events;
                a.events = Some(all[..count.min(all.len())].to_vec());
                for p in 0..6 {
                    a.page = p;
                    let v = layout::check(&tree(&a, &f));
                    assert!(v.is_empty(), "{base} events n={count} page={p}: {v:#?}");
                    n += 1;
                }
            }
            let mut a = sample_app(base, Screen::History).unwrap();
            a.hist_tab = HistTab::Events;
            a.events = None;
            assert!(layout::check(&tree(&a, &f)).is_empty());
        }
        assert!(n > 50);
        // a page never mixes two days, and holds at most 4 rows
        let ev = good_events();
        let pages = crate::ui::screens::event_pages(&ev, &crate::ui::utc_at);
        for (day, rows) in &pages {
            assert!(rows.len() <= crate::ui::screens::EVENT_ROWS);
            assert!(rows.iter().all(|e| &crate::ui::utc_at(e.ts).1 == day));
        }
    }

    #[test]
    fn fans_and_ring_never_collide() {
        use crate::ui::ring::{ATTENTION, DOWN, GOOD, Settings};
        let f = Fonts::load();
        for base in ["good", "worst"] {
            let all = sample_app(base, Screen::Fans).unwrap().temps.unwrap();
            for case in [
                None,
                all.case_fan.clone(),
                all.case_fan.clone().map(|mut c| {
                    c.controller = !c.controller;
                    c
                }),
            ] {
                for cooler in [None, all.fan.clone()] {
                    for sensors in [0, all.sensors.len()] {
                        let mut a = sample_app(base, Screen::Fans).unwrap();
                        let mut t = all.clone();
                        (t.case_fan, t.fan) = (case.clone(), cooler.clone());
                        t.sensors.truncate(sensors);
                        a.temps = Some(t.clone());
                        let v = layout::check(&tree(&a, &f));
                        assert!(v.is_empty(), "{base} fans: {v:#?}");
                        a.screen = Some(Screen::Temps);
                        let v = layout::check(&tree(&a, &f));
                        assert!(v.is_empty(), "{base} temps fan card: {v:#?}");
                    }
                }
            }
        }
        let mut a = sample_app("good", Screen::Ring).unwrap();
        for g in GOOD {
            for at in ATTENTION {
                for d in DOWN {
                    for br in 0..3 {
                        a.ring = Settings {
                            good: g,
                            attention: at,
                            down: d,
                            brightness: br,
                        };
                        for s in [Screen::Ring, Screen::Settings] {
                            a.screen = Some(s);
                            let v = layout::check(&tree(&a, &f));
                            assert!(v.is_empty(), "{s:?} {:?}: {v:#?}", a.ring);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn settings_night_and_shutdown_never_collide() {
        use crate::ui::prefs::{Change, Prefs};
        let f = Fonts::load();
        let mut a = sample_app("good", Screen::Display).unwrap();
        // walk every setting to both ends, checking each screen on the way
        let mut n = 0;
        for c in [
            Change::OffAfter(1),
            Change::OffAfter(-1),
            Change::NightAfter(1),
            Change::NightAfter(-1),
            Change::NightStart(1),
            Change::NightEnd(-1),
        ] {
            for _ in 0..50 {
                a.prefs.apply(c);
                for s in [Screen::Display, Screen::Night, Screen::Settings] {
                    a.screen = Some(s);
                    let v = layout::check(&tree(&a, &f));
                    assert!(v.is_empty(), "{s:?} {:?}: {v:#?}", a.prefs);
                    n += 1;
                }
            }
        }
        for c in [
            Change::Clock24(true),
            Change::Night(false),
            Change::NightRing(2),
            Change::Clock24(false),
        ] {
            a.prefs.apply(c);
            for s in [Screen::Display, Screen::Night, Screen::Settings] {
                a.screen = Some(s);
                assert!(layout::check(&tree(&a, &f)).is_empty());
            }
        }
        assert!(n > 500);
        let _ = Prefs::default();
        // night face: every status, long headlines, both clocks
        for base in ["good", "attention", "down", "waiting", "worst"] {
            let mut a = sample_app(base, Screen::NightFace).unwrap();
            a.screen = Some(Screen::NightFace);
            for t in ["12:04 PM", "11:59 PM", "23:59", "0:00"] {
                let tr = a.tree(&f, NOW_MS, t, "Sun 22 Nov", &crate::ui::utc_at);
                let v = layout::check(&tr);
                assert!(v.is_empty(), "{base} night face {t}: {v:#?}");
                // a tap anywhere opens Home
                let i = panel::hit(&tr, 240.0, 300.0).unwrap();
                assert_eq!(tr.items[i].nav, Some(Nav::Home));
            }
        }
        // alerts: every status, and the types page
        let st = |c, ch, e: Option<&str>, last| crate::alerts::Status {
            configured: c,
            chat: ch,
            error: e.map(String::from),
            last_sent: last,
        };
        let long =
            "Bad Request: chat not found and a much longer reason than usual from the server";
        for status in [
            None,
            Some(st(false, false, None, None)),
            Some(st(
                true,
                false,
                Some("send your bot any message once"),
                None,
            )),
            Some(st(true, true, Some(long), None)),
            Some(st(true, true, None, None)),
            Some(st(true, true, None, Some(NOW_MS))),
        ] {
            for test in [false, true] {
                let t = crate::ui::screens::alerts_screen(
                    &f,
                    &a.prefs,
                    status.as_ref(),
                    test,
                    &crate::ui::utc_at,
                );
                let v = layout::check(&t);
                assert!(v.is_empty(), "alerts {status:?}: {v:#?}");
            }
        }
        assert!(layout::check(&crate::ui::screens::alert_types_screen(&f, &a.prefs)).is_empty());
        // boot: every step, long uptimes
        for (up, sys, agent, n) in [
            (3, false, false, 0),
            (40, true, false, 4),
            (299, true, true, 999),
        ] {
            let t = crate::ui::screens::boot(&f, up, sys, agent, n);
            let v = layout::check(&t);
            assert!(v.is_empty(), "boot {up}: {v:#?}");
            assert!(
                !t.items.iter().any(|i| i.touch),
                "nothing to tap while booting"
            );
        }
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            crate::ui::panel::frame(&crate::ui::screens::boot(&f, 42, true, false, 4), &f, None)
                .pm
                .save_png(golden("boot.png"))
                .unwrap();
        }
        // shutdown: every step
        for (reboot, left, total) in [
            (false, 9, 9),
            (false, 6, 9),
            (true, 0, 9),
            (false, 0, 0),
            (true, 999, 999),
        ] {
            let t = crate::ui::screens::shutdown(&f, reboot, "11:59 PM", left, total);
            let v = layout::check(&t);
            assert!(v.is_empty(), "shutdown {left}/{total}: {v:#?}");
        }
    }

    #[test]
    fn menu_ring_and_fans_navigation() {
        use crate::ui::ring::Pattern;
        let f = Fonts::load();
        let tap = |a: &mut App, name: &str| {
            let t = tree(a, &f);
            let (x, y) = panel::centre(&t, name);
            let nav = t.items[panel::hit(&t, x, y).unwrap()].nav.unwrap();
            a.go(nav);
        };
        let mut a = sample_app("good", Screen::Home).unwrap();
        tap(&mut a, "menu");
        assert_eq!(a.screen(), Screen::Settings);
        tap(&mut a, "LED ring");
        assert_eq!(a.screen(), Screen::Ring);
        tap(&mut a, "Steady"); // the first "Steady" row: All good
        assert_eq!(a.ring.good, Pattern::Steady);
        assert!(a.ring_preview.is_some(), "a choice plays on the ring");
        tap(&mut a, "back");
        assert_eq!(a.screen(), Screen::Settings);
        tap(&mut a, "Alerts");
        assert_eq!(a.screen(), Screen::Alerts);
        tap(&mut a, "types");
        assert_eq!(a.screen(), Screen::AlertTypes);
        tap(&mut a, "On"); // the first row: Failed SSH (already on)
        assert!(
            a.prefs.alerts[2] && a.alert_sync,
            "a switch goes to the agent"
        );
        tap(&mut a, "back");
        assert_eq!(a.screen(), Screen::Alerts);
        tap(&mut a, "test");
        assert!(a.alert_test.is_some());
        tap(&mut a, "back");
        assert_eq!(a.screen(), Screen::Settings);
        tap(&mut a, "back");
        assert_eq!(a.screen(), Screen::Home);
        // Temperatures' fan card opens Fans, and back returns there
        a.go(Nav::Temps);
        tap(&mut a, "fan");
        assert_eq!(a.screen(), Screen::Fans);
        tap(&mut a, "back");
        assert_eq!(a.screen(), Screen::Temps);
    }

    #[test]
    fn history_tabs() {
        use crate::ui::screens::HistTab;
        let f = Fonts::load();
        let tap = |a: &mut App, name: &str| {
            let t = tree(a, &f);
            let (x, y) = panel::centre(&t, name);
            let nav = t.items[panel::hit(&t, x, y).unwrap()].nav.unwrap();
            a.go(nav);
        };
        let mut a = sample_app("good", Screen::Home).unwrap();
        a.go(Nav::History);
        assert_eq!(a.hist_tab, HistTab::Incidents);
        tap(&mut a, "Events");
        assert_eq!((a.screen(), a.hist_tab), (Screen::History, HistTab::Events));
        let keys: Vec<_> = a.needs().into_iter().map(|(k, _)| k).collect();
        assert!(keys.contains(&"events") && !keys.contains(&"incidents"));
        tap(&mut a, "Incidents");
        assert_eq!(a.hist_tab, HistTab::Incidents);
    }

    #[test]
    fn tabs_switch_and_back_keeps_the_tab() {
        use crate::ui::screens::Tab;
        let f = Fonts::load();
        let tap = |a: &mut App, name: &str| {
            let t = tree(a, &f);
            let (x, y) = panel::centre(&t, name);
            let nav = t.items[panel::hit(&t, x, y).unwrap()].nav.unwrap();
            a.go(nav);
        };
        let mut a = sample_app("good", Screen::Home).unwrap();
        a.go(Nav::Services);
        assert_eq!(a.tab, Tab::Apps, "nothing wrong: opens on Apps");
        tap(&mut a, "Host");
        assert_eq!((a.screen(), a.tab), (Screen::Services, Tab::Host));
        tap(&mut a, "service");
        assert!(a.open_container().is_some_and(|s| s.host));
        tap(&mut a, "back");
        assert_eq!((a.screen(), a.tab), (Screen::Services, Tab::Host));
        tap(&mut a, "Web");
        assert_eq!(a.tab, Tab::Web);
        // a website failing: Services opens on Web
        let mut a = sample_app("good", Screen::Home).unwrap();
        a.home.snapshot.as_mut().unwrap().web =
            Some(super::super::sample_web(&[("logs.example.com", None)]));
        a.go(Nav::Services);
        assert_eq!(a.tab, Tab::Web);
    }

    #[test]
    fn taps_reach_services_and_back() {
        let f = Fonts::load();
        let centre = |t: &layout::Tree, name: &str| {
            let r = t.items.iter().find(|i| i.name == name).unwrap().rect;
            (r.x + r.w / 2.0, r.cy())
        };
        let tap = |a: &mut App, name: &str| {
            let t = tree(a, &f);
            let (x, y) = centre(&t, name);
            let nav = t.items[panel::hit(&t, x, y).unwrap()].nav.unwrap();
            a.go(nav);
        };
        let mut a = sample_app("down", Screen::Home).unwrap();
        tap(&mut a, "hero card");
        assert_eq!(a.screen(), Screen::Services);
        tap(&mut a, "service"); // the first tile is the stopped one
        assert_eq!(a.open_container().map(|s| s.name.as_str()), Some("cache"));
        tap(&mut a, "back");
        assert_eq!(a.screen(), Screen::Services);
        tap(&mut a, "back");
        assert_eq!(a.screen(), Screen::Home);
        // Restart is a hold target: a tap does nothing, a press starts a hold
        let mut a = sample_app("good", Screen::Container(0)).unwrap();
        let t = tree(&a, &f);
        let (x, y) = centre(&t, "restart");
        let i = panel::hit(&t, x, y).unwrap();
        assert!(t.items[i].hold);
        let now = std::time::Instant::now();
        let h = panel::Holding::start(&t, x, y, now).expect("hold starts on Restart");
        let k = key(&a.open_container().unwrap().id);
        assert_eq!(h.nav, Nav::Restart(k));
        assert!(!h.moved_away(x + 10.0, y) && h.moved_away(x + 25.0, y));
        let (bx, by) = centre(&t, "back");
        assert!(panel::Holding::start(&t, bx, by, now).is_none());
        let before = a.screen();
        a.act(h.nav, Path::new("/nonexistent"), now); // a sample: logs only
        assert_eq!(a.screen(), before);
        // paging keeps the Services screen
        let mut a = sample_app("worst", Screen::Services).unwrap();
        tap(&mut a, "next");
        assert_eq!((a.screen(), a.page), (Screen::Services, 1));
        tap(&mut a, "service");
        tap(&mut a, "back");
        assert_eq!((a.screen(), a.page), (Screen::Services, 1));
    }

    #[test]
    fn taps_navigate() {
        let f = Fonts::load();
        let centre = |t: &layout::Tree, name: &str| {
            let r = t
                .items
                .iter()
                .find(|i| i.name == name)
                .unwrap_or_else(|| panic!("no {name}"))
                .rect;
            (r.x + r.w / 2.0, r.cy())
        };
        let mut a = sample_app("good", Screen::Home).unwrap();
        for (target, want) in [
            ("CPU", Screen::Cpu),
            ("TEMP", Screen::Temps),
            ("MEMORY", Screen::Memory),
            ("DISK", Screen::Disk),
            ("history button", Screen::History),
        ] {
            a.screen = Some(Screen::Home);
            let t = tree(&a, &f);
            let (x, y) = centre(&t, target);
            let i = panel::hit(&t, x, y).unwrap();
            a.go(t.items[i].nav.unwrap());
            assert_eq!(a.screen(), want, "{target}");
            let t = tree(&a, &f);
            let (x, y) = centre(&t, "back");
            let i = panel::hit(&t, x, y).unwrap();
            assert_eq!(t.items[i].nav, Some(Nav::Back));
        }
        // History: a row opens its incident, next page works, back returns to History
        a.go(Nav::History);
        let t = tree(&a, &f);
        let (x, y) = centre(&t, "incident");
        let nav = t.items[panel::hit(&t, x, y).unwrap()].nav.unwrap();
        assert!(matches!(nav, Nav::Incident(4)), "{nav:?}");
        let (x, y) = centre(&t, "next");
        assert_eq!(
            t.items[panel::hit(&t, x, y).unwrap()].nav,
            Some(Nav::Page(1))
        );
        let (x, y) = centre(&t, "previous");
        assert_eq!(
            panel::hit(&t, x, y).and_then(|i| t.items[i].nav),
            None,
            "previous is disabled on page 1"
        );
        a.go(nav);
        a.go(Nav::Back);
        assert_eq!(a.screen(), Screen::History);
    }
}
