//! Built-in example data for every screen: `pios ui --sample NAME --screen SCREEN`, the spacing
//! check, and the golden images. The repo is public, so names here are generic on purpose.

use super::app::{App, Screen};
use super::home::Data;
use super::screens::{Incident, IncidentFull, Snapshots};
use crate::details::*;

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
        percent: 1.3,
        cores: vec![
            Core {
                percent: 2.0,
                mhz: 2000,
            },
            Core {
                percent: 0.0,
                mhz: 2000,
            },
            Core {
                percent: 2.0,
                mhz: 1900,
            },
            Core {
                percent: 1.0,
                mhz: 2000,
            },
        ],
        load: [0.06, 0.07, 0.05],
        min_mhz: 1500,
        max_mhz: 2400,
        governor: "ondemand".into(),
        top: vec![
            usage("database", 1.2),
            usage("java", 0.5),
            usage("dockerd", 0.3),
        ],
    }
}

fn good_temps() -> TempDetails {
    TempDetails {
        sensors: vec![
            Sensor {
                name: "CPU".into(),
                celsius: 43.0,
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
            },
            Fs {
                mount: "/boot/firmware".into(),
                used: 68_063_232,
                size: 528_592_896,
            },
        ],
        drive: Some(Drive {
            model: "SAMPLE NVME 512G".into(),
            size: 512_110_190_592,
            kind: "NVMe".into(),
            celsius: Some(52.85),
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
            None,
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
    let mut peak = s.clone();
    if let Some(p) = peak.as_mut() {
        p.cpu.percent = 97.0;
        p.ts = i.peak_at.unwrap_or(i.started_at);
    }
    let mut start = s.clone();
    if let Some(st) = start.as_mut() {
        st.cpu.percent = 84.0;
        st.ts = i.started_at;
    }
    let end = if ended {
        s.map(|mut e| {
            e.ts = i.ended_at.unwrap_or(NOW_MS);
            e
        })
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
            },
            Fs {
                mount: "/mnt/a-very-long-mount-point-name/with/many/levels".into(),
                used: 900 * GB * 1024,
                size: 999 * GB * 1024,
            },
        ],
        drive: Some(Drive {
            model: "A VERY LONG DRIVE MODEL NAME WITH MANY WORDS 8TB".into(),
            size: 8000 * GB,
            kind: "SD card".into(),
            celsius: Some(109.0),
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
        }
        _ => {
            a.cpu = Some(good_cpu());
            a.temps = Some(good_temps());
            a.mem = Some(good_mem());
            a.disk = Some(good_disk());
            a.cpu_trend = curve(96, 3.0, 1.5, 0.4, Some((60, 11.0)));
            a.temp_trend = curve(96, 41.0, 2.2, 1.2, Some((60, 4.0)));
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
                let a = sample_app(sample, Screen::parse(s).unwrap()).unwrap();
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
