//! `pios ui`: draws the screens from the agent's API. For now Home only.
//!
//!   pios ui --fb /dev/fb0 [--touch DEV]  the real panel (pios-ui.service); see panel.rs
//!   pios ui --png FILE           one frame from the running agent
//!   pios ui --preview [--scale N] live window, mouse click = tap (laptop builds with `--features preview`)
//!   pios ui --sample NAME ...    draw a built-in example instead (good, attention, down, waiting, worst)
//!   pios ui --check ...          print any spacing-rule violation and exit 1 if there is one
//!
//! Panel settings (environment): PIOS_TOUCH_MATRIX (libinput calibration, 6 numbers),
//! PIOS_BACKLIGHT (sysfs brightness file), PIOS_IDLE_MIN (screen off after, default 30).

pub mod app;
pub mod canvas;
pub mod fb;
pub mod home;
pub mod layout;
pub mod panel;
pub mod samples;
pub mod screens;
pub mod theme;
pub mod touch;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Result, err};
use crate::types::Snapshot;
use canvas::Fonts;
use home::{Data, OpenIncident};

const API_TIMEOUT: Duration = Duration::from_secs(2);

/// (time "12:04", date "Wed 18 Nov") in local time.
pub fn local_now() -> (String, String) {
    // SAFETY: time() has no preconditions.
    local_at(unsafe { libc::time(std::ptr::null_mut()) } as i64 * 1000)
}

/// (time "12:04", date "Wed 18 Nov") of a timestamp (ms), in local time.
pub fn local_at(ms: i64) -> (String, String) {
    // SAFETY: localtime_r only writes into the struct we pass.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = (ms / 1000) as libc::time_t;
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    format_tm(tm.tm_hour, tm.tm_min, tm.tm_wday, tm.tm_mday, tm.tm_mon)
}

/// The same in UTC (tests: golden images must not depend on the machine's time zone).
pub fn utc_at(ms: i64) -> (String, String) {
    // SAFETY: gmtime_r only writes into the struct we pass.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = (ms / 1000) as libc::time_t;
    unsafe {
        libc::gmtime_r(&t, &mut tm);
    }
    format_tm(tm.tm_hour, tm.tm_min, tm.tm_wday, tm.tm_mday, tm.tm_mon)
}

fn format_tm(h: i32, m: i32, wday: i32, mday: i32, mon: i32) -> (String, String) {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let day = DAYS.get(wday as usize).unwrap_or(&"");
    let month = MONTHS.get(mon as usize).unwrap_or(&"");
    (format!("{h:02}:{m:02}"), format!("{day} {mday} {month}"))
}

#[derive(Deserialize)]
struct IncidentRow {
    kind: String,
    title: String,
    #[serde(rename = "endedAt")]
    ended_at: Option<i64>,
}

#[derive(Deserialize)]
struct IncidentList {
    incidents: Vec<IncidentRow>,
}

#[derive(Deserialize)]
struct TrendResponse {
    points: Vec<(i64, f64)>,
}

pub fn get_json<T: for<'de> Deserialize<'de>>(socket: &Path, path: &str) -> Result<T> {
    let (status, body) = crate::http::get(socket, path, API_TIMEOUT)?;
    if status != 200 {
        return Err(err(format!("{path}: HTTP {status}")));
    }
    Ok(serde_json::from_str(&body)?)
}

#[derive(Deserialize)]
pub struct IncidentsOut {
    pub incidents: Vec<screens::Incident>,
}

/// Every point of the last 24 h for a trend metric, oldest first.
pub fn trend_24h(socket: &Path, metric: &str) -> Vec<f32> {
    get_json::<TrendResponse>(socket, &format!("/trend?metric={metric}"))
        .map(|t| t.points.iter().map(|p| p.1 as f32).collect())
        .unwrap_or_default()
}

fn last_10(socket: &Path, metric: &str) -> Vec<f32> {
    get_json::<TrendResponse>(socket, &format!("/trend?metric={metric}"))
        .map(|t| {
            t.points
                .iter()
                .rev()
                .take(10)
                .rev()
                .map(|p| p.1 as f32)
                .collect()
        })
        .unwrap_or_default()
}

/// Reads everything Home needs. Agent not answering → empty Data (the "waiting" screen).
pub fn fetch(socket: &Path) -> Data {
    let Ok(snapshot) = get_json::<Snapshot>(socket, "/now") else {
        return Data::default();
    };
    let open = get_json::<IncidentList>(socket, "/incidents")
        .map(|l| {
            l.incidents
                .into_iter()
                .filter(|i| i.ended_at.is_none())
                .map(|i| OpenIncident {
                    kind: i.kind,
                    title: i.title,
                })
                .collect()
        })
        .unwrap_or_default();
    Data {
        snapshot: Some(snapshot),
        open,
        cpu_trend: last_10(socket, "cpu.percent"),
        temp_trend: last_10(socket, "cpu.tempC"),
    }
}

#[cfg(test)]
pub fn draw<'f>(home: &home::Home, fonts: &'f Fonts) -> canvas::Canvas<'f> {
    let tree = home.tree(fonts);
    let mut c = canvas::Canvas::new(fonts);
    layout::render(&tree, &mut c);
    c
}

// ---------- samples (also the golden-image fixtures) ----------

pub const SAMPLES: [&str; 5] = ["good", "attention", "down", "waiting", "worst"];

pub fn sample_snapshot(
    hostname: &str,
    cpu: f64,
    temp: Option<f64>,
    mem_used_gb: f64,
    disk: f64,
    docker: Option<(u32, u32)>,
) -> Snapshot {
    use crate::types::*;
    let total = 8u64 << 30;
    let used = (mem_used_gb * (1u64 << 30) as f64) as u64;
    Snapshot {
        ts: 0,
        host: Host {
            hostname: hostname.into(),
            uptime_sec: 4.0 * 3600.0 + 8.0 * 60.0,
            load: [0.08, 0.06, 0.01],
        },
        cpu: Cpu {
            percent: cpu,
            temp_c: temp,
        },
        memory: Memory {
            total_bytes: total,
            used_bytes: used,
            available_bytes: total.saturating_sub(used),
            used_percent: used as f64 * 100.0 / total as f64,
        },
        disk: Disk {
            used_percent: disk,
            used_bytes: 0,
            total_bytes: 0,
        },
        network: Network { interfaces: vec![] },
        docker: docker.map(|(running, total)| Docker {
            running,
            total,
            containers: (0..total)
                .map(|i| Container {
                    id: format!("{i:064x}"),
                    name: format!("app{i}"),
                    state: if i < running { "running" } else { "exited" }.into(),
                })
                .collect(),
        }),
    }
}

const CPU_TREND: [f32; 10] = [6.0, 7.5, 5.0, 9.0, 6.5, 12.0, 8.0, 6.0, 10.0, 7.0];
const TEMP_TREND: [f32; 10] = [37.0, 37.0, 37.5, 37.0, 37.5, 37.5, 38.0, 37.5, 37.5, 38.0];

pub fn sample(name: &str) -> Option<Data> {
    let incident = |kind: &str, title: &str| OpenIncident {
        kind: kind.into(),
        title: title.into(),
    };
    let trends = |d: Data| Data {
        cpu_trend: CPU_TREND.to_vec(),
        temp_trend: TEMP_TREND.to_vec(),
        ..d
    };
    Some(match name {
        "good" => trends(Data {
            snapshot: Some(sample_snapshot(
                "home",
                7.0,
                Some(37.0),
                1.3,
                12.0,
                Some((9, 9)),
            )),
            ..Data::default()
        }),
        "attention" => trends(Data {
            snapshot: Some(sample_snapshot(
                "home",
                86.0,
                Some(77.0),
                1.3,
                12.0,
                Some((9, 9)),
            )),
            open: vec![
                incident("metric", "CPU over 80%"),
                incident("metric", "CPU temperature over 75°C"),
            ],
            ..Data::default()
        }),
        "down" => trends(Data {
            snapshot: Some(sample_snapshot(
                "home",
                7.0,
                Some(37.0),
                1.3,
                12.0,
                Some((8, 9)),
            )),
            open: vec![incident("container", "cache stopped")],
            ..Data::default()
        }),
        "waiting" => Data::default(),
        // everything at its widest: long hostname, maxed values, many problems, big counts
        "worst" => trends(Data {
            snapshot: Some(sample_snapshot(
                "raspberry-pi-living-room-media-server-01",
                100.0,
                Some(105.0),
                1023.0,
                100.0,
                Some((118, 120)),
            )),
            open: vec![
                incident("container", "photo-library-db-replica-secondary stopped"),
                incident("metric", "CPU over 80%"),
                incident("metric", "RAM over 85%"),
                incident("metric", "Disk over 85%"),
                incident("metric", "CPU temperature over 75°C"),
            ],
            ..Data::default()
        }),
        _ => return None,
    })
}

// ---------- command ----------

struct Args {
    fb: Option<PathBuf>,
    touch: Option<PathBuf>,
    png: Option<PathBuf>,
    preview: bool,
    sample: Option<String>,
    screen: app::Screen,
    check: bool,
    scale: usize,
}

fn parse_args(args: &[String]) -> Result<Args> {
    let mut a = Args {
        fb: None,
        touch: None,
        png: None,
        preview: false,
        sample: None,
        screen: app::Screen::Home,
        check: false,
        scale: 2,
    };
    let mut it = args.iter();
    let mut next = |what: &str| {
        it.next()
            .cloned()
            .ok_or_else(|| err(format!("{what} needs a value")))
    };
    while let Ok(arg) = next("") {
        match arg.as_str() {
            "--png" => a.png = Some(next("--png")?.into()),
            "--fb" => a.fb = Some(next("--fb")?.into()),
            "--touch" => a.touch = Some(next("--touch")?.into()),
            "--preview" => a.preview = true,
            "--sample" => a.sample = Some(next("--sample")?),
            "--screen" => {
                let s = next("--screen")?;
                a.screen = app::Screen::parse(&s).ok_or_else(|| {
                    err(format!(
                        "no screen {s:?}; try one of {}",
                        app::SCREENS.join(", ")
                    ))
                })?;
            }
            "--check" => a.check = true,
            "--scale" => {
                a.scale = next("--scale")?
                    .parse()
                    .map_err(|_| err("--scale needs 1, 2 or 4"))?
            }
            other => return Err(err(format!("unknown option {other}"))),
        }
    }
    if a.png.is_none() && !a.preview && !a.check && a.fb.is_none() {
        return Err(err(
            "usage: pios ui --fb DEV [--touch DEV] | [--sample NAME] [--screen NAME] [--png FILE | --preview [--scale N] | --check]",
        ));
    }
    Ok(a)
}

/// The app for a sample, or a live one (fetched once) on the chosen screen.
fn make_app(sample_name: Option<&str>, screen: app::Screen, socket: &Path) -> Result<app::App> {
    match sample_name {
        Some(n) => samples::sample_app(n, screen).ok_or_else(|| {
            err(format!(
                "no sample {n:?}; try one of {}",
                SAMPLES.join(", ")
            ))
        }),
        None => {
            let mut a = app::App::live();
            a.screen = Some(screen);
            a.update(socket, std::time::Instant::now());
            // `--screen container`: the first one the Services screen lists
            if screen == app::Screen::Container(0)
                && let Some(s) = a
                    .services
                    .as_ref()
                    .and_then(|d| screens::sorted(&d.services).first().map(|s| s.id.clone()))
            {
                a.screen = Some(app::Screen::Container(crate::services::key(&s)));
            }
            Ok(a)
        }
    }
}

/// Samples use a fixed clock and UTC, so pictures don't change with time or time zone.
fn tree_for(a: &app::App, fonts: &Fonts) -> layout::Tree {
    if a.live {
        panel::current_tree(a, fonts)
    } else {
        let (time, date) = utc_at(samples::NOW_MS);
        a.tree(fonts, samples::NOW_MS, &time, &date, &utc_at)
    }
}

pub fn run(args: &[String], socket: &Path) -> Result<()> {
    let a = parse_args(args)?;
    let fonts = Fonts::load();

    if a.check {
        let names: Vec<String> = match &a.sample {
            Some(n) => vec![n.clone()],
            None => SAMPLES.iter().map(|s| s.to_string()).collect(),
        };
        let mut bad = 0;
        for n in &names {
            for s in app::SCREENS {
                let ap = make_app(
                    Some(n),
                    app::Screen::parse(s).expect("known screen"),
                    socket,
                )?;
                let v = layout::check(&tree_for(&ap, &fonts));
                println!(
                    "{n} · {s}: {}",
                    if v.is_empty() {
                        "clean".to_string()
                    } else {
                        format!("{} problem(s)", v.len())
                    }
                );
                for line in &v {
                    println!("  {line}");
                }
                bad += v.len();
            }
        }
        if bad > 0 {
            return Err(err(format!("{bad} spacing problem(s)")));
        }
    }

    if let Some(path) = &a.png {
        let ap = make_app(a.sample.as_deref(), a.screen, socket)?;
        let c = panel::frame(&tree_for(&ap, &fonts), &fonts, None);
        c.pm.save_png(path)
            .map_err(|e| err(format!("{}: {e}", path.display())))?;
        println!("wrote {}", path.display());
    }

    if a.preview {
        preview(&a, socket, &fonts)?;
    }

    if let Some(fb) = &a.fb {
        let calibration = match std::env::var("PIOS_TOUCH_MATRIX") {
            Ok(m) if !m.trim().is_empty() => touch::Calibration::parse(&m)?,
            _ => touch::Calibration::IDENTITY,
        };
        let idle_min: u64 = std::env::var("PIOS_IDLE_MIN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);
        panel::run(
            panel::Options {
                fb: fb.clone(),
                touch: a.touch.clone(),
                calibration,
                backlight: std::env::var_os("PIOS_BACKLIGHT")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from),
                idle: Duration::from_secs(idle_min.max(1) * 60),
                socket: socket.to_path_buf(),
            },
            &fonts,
        )?;
    }
    Ok(())
}

#[cfg(feature = "preview")]
fn preview(a: &Args, socket: &Path, fonts: &Fonts) -> Result<()> {
    use minifb::{Key, Scale, Window, WindowOptions};
    use std::time::Instant;

    let scale = match a.scale {
        1 => Scale::X1,
        4 => Scale::X4,
        _ => Scale::X2,
    };
    let mut win = Window::new(
        "PiOS · 480 × 320",
        theme::W as usize,
        theme::H as usize,
        WindowOptions {
            scale,
            ..Default::default()
        },
    )
    .map_err(|e| err(format!("preview window: {e}")))?;
    win.set_target_fps(20);
    let mut ap = make_app(a.sample.as_deref(), a.screen, socket)?;
    let mut shown: Option<(layout::Tree, Option<usize>)> = None;
    let mut buf = vec![0u32; (theme::W * theme::H) as usize];
    let mut was_down = false;
    let mut pressed: Option<(usize, Instant, Option<layout::Nav>)> = None;
    while win.is_open() && !win.is_key_down(Key::Escape) {
        ap.update(socket, Instant::now());
        let tree = tree_for(&ap, fonts);
        let down = win.get_mouse_down(minifb::MouseButton::Left);
        if down
            && !was_down
            && let Some((x, y)) = win.get_mouse_pos(minifb::MouseMode::Discard)
            && let Some(i) = panel::hit(&tree, x, y)
        {
            println!("tap {} at {x:.0},{y:.0}", tree.items[i].name);
            pressed = Some((i, Instant::now() + panel::PRESS_SHOWN, tree.items[i].nav));
        }
        was_down = down;
        if let Some((_, until, nav)) = pressed
            && Instant::now() >= until
        {
            pressed = None;
            if let Some(nav) = nav {
                ap.go(nav);
                if !ap.live {
                    // samples have data for every screen; keep the sample's incident
                    if let (app::Screen::Incident(_), Some(i)) = (ap.screen(), &ap.incident) {
                        ap.screen = Some(app::Screen::Incident(i.incident.id));
                    }
                }
                continue;
            }
        }
        let p = pressed.map(|(i, _, _)| i);
        if shown.as_ref().is_none_or(|(t, sp)| *t != tree || *sp != p) {
            buf = panel::frame(&tree, fonts, p).xrgb();
            shown = Some((tree, p));
        }
        win.update_with_buffer(&buf, theme::W as usize, theme::H as usize)
            .map_err(|e| err(format!("preview window: {e}")))?;
    }
    Ok(())
}

#[cfg(not(feature = "preview"))]
fn preview(_: &Args, _: &Path, _: &Fonts) -> Result<()> {
    Err(err(
        "this build has no preview window; run: cargo run --features preview -- ui --preview",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::home::Home;

    fn golden_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
    }

    /// Every sample: no spacing violation, and pixels identical to the checked-in PNG.
    /// `UPDATE_GOLDEN=1 cargo test` rewrites the PNGs after an intended change.
    #[test]
    fn samples_are_clean_and_match_golden() {
        let fonts = Fonts::load();
        let update = std::env::var_os("UPDATE_GOLDEN").is_some();
        let mut failures = Vec::new();
        for name in SAMPLES {
            let home = Home::build(&sample(name).unwrap(), "12:04", "Wed 18 Nov");
            let v = layout::check(&home.tree(&fonts));
            assert!(v.is_empty(), "{name}: {v:#?}");

            let c = draw(&home, &fonts);
            let path = golden_dir().join(format!("home-{name}.png"));
            if update {
                std::fs::create_dir_all(golden_dir()).unwrap();
                c.pm.save_png(&path).unwrap();
                continue;
            }
            let want = tiny_skia::Pixmap::load_png(&path)
                .unwrap_or_else(|e| panic!("{}: {e} (run with UPDATE_GOLDEN=1)", path.display()));
            let diff = want
                .data()
                .chunks(4)
                .zip(c.pm.data().chunks(4))
                .filter(|(a, b)| a != b)
                .count();
            if diff > 0 {
                let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/golden-diff");
                std::fs::create_dir_all(&out).unwrap();
                c.pm.save_png(out.join(format!("home-{name}.png"))).unwrap();
                failures.push(format!(
                    "{name}: {diff} pixels differ (actual in target/golden-diff/)"
                ));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// Many combinations of extreme data: nothing may ever collide or overflow.
    #[test]
    fn nothing_collides_with_any_data() {
        let fonts = Fonts::load();
        let hosts = [
            "h",
            "home",
            "raspberrypi",
            "a-very-long-hostname-for-the-living-room-pi-5",
        ];
        let values = [0.0, 7.0, 49.5, 86.0, 99.9, 100.0];
        let temps = [None, Some(37.0), Some(105.0)];
        let mems = [1.3, 9.96, 1023.0];
        let dockers = [
            None,
            Some((0, 0)),
            Some((9, 9)),
            Some((3, 9)),
            Some((120, 120)),
            Some((1, 999)),
        ];
        let problems: Vec<Vec<OpenIncident>> = vec![
            vec![],
            vec![OpenIncident {
                kind: "metric".into(),
                title: "CPU over 80%".into(),
            }],
            vec![
                OpenIncident {
                    kind: "container".into(),
                    title: "an-extremely-long-container-name-that-never-ends stopped".into(),
                },
                OpenIncident {
                    kind: "metric".into(),
                    title: "Disk over 85%".into(),
                },
            ],
            (0..12)
                .map(|i| OpenIncident {
                    kind: "metric".into(),
                    title: format!("problem {i}"),
                })
                .collect(),
        ];
        let mut n = 0;
        for host in hosts {
            for &v in &values {
                for &temp in &temps {
                    for &mem in &mems {
                        for &dk in &dockers {
                            for open in &problems {
                                let d = Data {
                                    snapshot: Some(sample_snapshot(host, v, temp, mem, v, dk)),
                                    open: open.clone(),
                                    cpu_trend: CPU_TREND.to_vec(),
                                    temp_trend: vec![],
                                };
                                let home = Home::build(&d, "23:59", "Wed 30 Sep");
                                let viol = layout::check(&home.tree(&fonts));
                                assert!(
                                    viol.is_empty(),
                                    "{host} cpu={v} temp={temp:?} mem={mem} docker={dk:?} problems={}: {viol:#?}",
                                    open.len()
                                );
                                n += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(n > 2000, "{n}");
    }

    #[test]
    fn uptime_format() {
        assert_eq!(home::uptime(4.0 * 3600.0 + 8.0 * 60.0), "Up 4 h 08 m");
        assert_eq!(home::uptime(12.0 * 60.0), "Up 12 m");
        assert_eq!(home::uptime(3.0 * 86400.0 + 5.0 * 3600.0), "Up 3 d 5 h");
    }

    #[test]
    fn status_from_data() {
        let b = |n| Home::build(&sample(n).unwrap(), "12:04", "Wed 18 Nov");
        assert_eq!(
            (b("good").status, b("good").headline.as_str()),
            (theme::Status::Good, "All good")
        );
        assert_eq!(b("attention").headline, "2 need attention");
        assert_eq!(b("attention").line3, "and CPU temperature over 75°C");
        assert_eq!(
            (b("down").status, b("down").headline.as_str()),
            (theme::Status::Down, "1 is down")
        );
        assert_eq!(b("waiting").status, theme::Status::Unknown);
        assert_eq!(b("good").line2, "9 of 9 checks passing");
    }
}
