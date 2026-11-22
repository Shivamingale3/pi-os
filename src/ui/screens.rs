//! The detail screens: CPU, Temperatures, Memory, Disk, History, one incident, Services and
//! one container.
//! Same rules as Home (layout::check proves them): 12 px from the edges, 8 px between cards,
//! 12 px inside cards, a 44 × 44 back button, text cut with "…" or shrunk, never clipped.
//!
//! ┌ 12 ──────────────────────────────────────────────────────────────┐
//! │ [‹] Title                                                   meta │  header 44   y 12–56
//! │ 8                                                                │
//! │ cards …                                                          │  y 64–308
//! └ 12 ──────────────────────────────────────────────────────────────┘

use serde::Deserialize;

use super::canvas::{Face, Fonts, Style};
use super::layout::{Marker, Nav, Paint, Rect, Tree};
use super::theme::{self, GAP, INK, INK2, INK3, MIN_GAP, PAD_TILE, Status, size, status_colors};
use crate::details::{CpuDetails, DiskDetails, MemDetails, TempDetails};
use crate::events::Event;
use crate::rules::THRESHOLDS;
use crate::services::{Service, Services};
use crate::types::Snapshot;
use crate::types::{Web, WebCheck};

const M: f32 = theme::SCREEN_MARGIN;
const WIDTH: f32 = theme::W - 2.0 * theme::SCREEN_MARGIN;
const TOP: f32 = 64.0; // first card row, under the header
const CACHE_GREY: theme::Rgb = 0x5E6266;

// ---------------- data the History screens read ----------------

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Incident {
    pub id: i64,
    pub kind: String,
    pub subject: String,
    pub title: String,
    pub severity: String,
    pub threshold: Option<f64>,
    pub started_at: i64,
    pub peak_at: Option<i64>,
    pub peak_value: Option<f64>,
    pub ended_at: Option<i64>,
    pub end_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Snapshots {
    pub start: Option<Snapshot>,
    pub peak: Option<Snapshot>,
    pub end: Option<Snapshot>,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct IncidentFull {
    #[serde(flatten)]
    pub incident: Incident,
    pub snapshots: Snapshots,
}

// ---------------- formatting ----------------

/// 1024-based, like the Home tiles: ("1.2", " GB"), ("545", " MB").
pub fn bytes(b: u64) -> (String, String) {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    let n = if u == 0 || v >= 10.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    };
    (n, format!(" {}", UNITS[u]))
}

pub fn bytes_s(b: u64) -> String {
    let (n, u) = bytes(b);
    format!("{n}{u}")
}

pub fn rate(bps: f64) -> String {
    format!("{}/s", bytes_s(bps.max(0.0) as u64))
}

pub fn duration(ms: i64) -> String {
    let s = (ms.max(0) / 1000) as u64;
    let (d, h, m) = (s / 86400, s / 3600 % 24, s / 60 % 60);
    if d > 0 {
        format!("{d} d {h} h")
    } else if h > 0 {
        format!("{h} h {m:02} m")
    } else if m > 0 {
        format!("{m} m")
    } else {
        format!("{s} s")
    }
}

fn unit_for(subject: &str) -> &'static str {
    match subject {
        "cpu.tempC" => "°C",
        "cpu.percent" | "mem.usedPercent" | "disk.usedPercent" => "%",
        _ => "",
    }
}

fn level(metric: &str, v: f64) -> Status {
    match THRESHOLDS.iter().find(|t| t.metric == metric) {
        Some(t) if v >= t.critical => Status::Down,
        Some(t) if v >= t.warn => Status::Attention,
        _ => Status::Good,
    }
}

fn level_color(s: Status) -> theme::Rgb {
    if s == Status::Good {
        INK
    } else {
        status_colors(s).text
    }
}

// ---------------- building blocks ----------------

fn label_style() -> Style {
    Style::new(Face::SemiBold, size::LABEL.0).spacing(0.06 * size::LABEL.0)
}
fn body() -> Style {
    Style::new(Face::Regular, size::BODY.0)
}
fn small() -> Style {
    Style::new(Face::Regular, size::LABEL.0)
}
fn mono() -> Style {
    Style::new(Face::Mono, size::BODY.0)
}
fn title_style() -> Style {
    Style::new(Face::Regular, size::TITLE.0)
}

pub struct B<'f> {
    pub t: Tree,
    pub f: &'f Fonts,
}

impl<'f> B<'f> {
    pub fn new(f: &'f Fonts) -> Self {
        B {
            t: Tree::default(),
            f,
        }
    }

    /// Text in `r` (cut with "…" to fit), left-aligned.
    pub fn text(
        &mut self,
        parent: usize,
        name: &str,
        r: Rect,
        s: &str,
        style: Style,
        color: theme::Rgb,
    ) -> usize {
        let s = self.f.ellipsize(&style, s, r.w);
        let w = self.f.width(&style, &s);
        let i = self.t.add(
            Some(parent),
            name,
            Rect::new(r.x, r.y, w.max(1.0), r.h),
            Paint::Text {
                text: s,
                style,
                color,
            },
        );
        self.t.content(i, w, r.h)
    }

    /// Text right-aligned against `r.right()`, at most `r.w` wide.
    pub fn text_right(
        &mut self,
        parent: usize,
        name: &str,
        r: Rect,
        s: &str,
        style: Style,
        color: theme::Rgb,
    ) -> usize {
        let s = self.f.ellipsize(&style, s, r.w);
        let w = self.f.width(&style, &s);
        let i = self.t.add(
            Some(parent),
            name,
            Rect::new(r.right() - w.max(1.0), r.y, w.max(1.0), r.h),
            Paint::Text {
                text: s,
                style,
                color,
            },
        );
        self.t.content(i, w, r.h)
    }

    /// A big number + unit on one baseline, shrunk (28 → 20 px) to fit `r.w`.
    pub fn value(
        &mut self,
        parent: usize,
        name: &str,
        r: Rect,
        v: &str,
        unit: &str,
        color: theme::Rgb,
    ) -> usize {
        let unit_style = Style::new(Face::Regular, size::UNIT);
        let base = Style::new(Face::SemiBold, size::VALUE.0).spacing(-0.02 * size::VALUE.0);
        let uw = if unit.is_empty() {
            0.0
        } else {
            self.f.width(&unit_style, unit)
        };
        let vs = self
            .f
            .fit(&base, v, r.w - uw, size::MIN_VALUE)
            .unwrap_or(base.sized(size::MIN_VALUE).spacing(-0.4));
        let v = self.f.ellipsize(&vs, v, (r.w - uw).max(10.0));
        let w = self.f.width(&vs, &v) + uw;
        let i = self.t.add(
            Some(parent),
            name,
            Rect::new(r.x, r.y, r.w, r.h),
            Paint::Value {
                value: v,
                style: vs,
                color,
                unit: unit.into(),
                unit_style,
                unit_color: INK2,
            },
        );
        self.t.content(i, w, r.h)
    }

    /// The 44 px header: back button, title, meta on the right.
    pub fn header(&mut self, title: &str, meta: &str) {
        let h = self
            .t
            .add(None, "header", Rect::new(M, M, WIDTH, 44.0), Paint::None);
        let b = self.t.add(
            Some(h),
            "back",
            Rect::new(M, M, 44.0, 44.0),
            Paint::Panel {
                fill: theme::SURFACE,
                radius: theme::RADIUS_TILE,
                border: None,
            },
        );
        self.t.nav(b, Nav::Back);
        self.t.add(
            Some(b),
            "chevron",
            Rect::new(M + 12.0, M + 12.0, 20.0, 20.0),
            Paint::Chevron(INK, false),
        );
        let refresh = Rect::new(theme::W - M - 44.0, M, 44.0, 44.0);
        self.t.refresh_button(Some(h), refresh);
        let ts = Style::new(Face::SemiBold, 20.0).spacing(-0.2);
        let x = M + 44.0 + GAP;
        let meta_w = self.f.width(&body(), meta).min(WIDTH * 0.4);
        let title_room =
            WIDTH - 2.0 * (44.0 + GAP) - if meta.is_empty() { 0.0 } else { meta_w + GAP };
        self.text(
            h,
            "title",
            Rect::new(x, M, title_room, 44.0),
            title,
            ts,
            INK,
        );
        if !meta.is_empty() {
            self.text_right(
                h,
                "meta",
                Rect::new(refresh.x - GAP - meta_w, M, meta_w, 44.0),
                meta,
                body(),
                INK3,
            );
        }
    }

    /// A card with an optional label row; returns (card index, area under the label).
    pub fn card(&mut self, name: &str, r: Rect, label: &str, right: &str) -> (usize, Rect) {
        let c = self.t.add(
            None,
            name,
            r,
            Paint::Panel {
                fill: theme::SURFACE,
                radius: theme::RADIUS_TILE,
                border: None,
            },
        );
        self.t.pad(c, PAD_TILE);
        let inner = r.inset(PAD_TILE);
        if label.is_empty() {
            return (c, inner);
        }
        let lh = size::LABEL.1;
        let right_style = Style::new(Face::Regular, size::LABEL.0);
        let lw = self.f.width(&label_style(), label);
        let rw = if right.is_empty() {
            0.0
        } else {
            (inner.w - lw - GAP).max(0.0)
        };
        self.text(
            c,
            "label",
            Rect::new(
                inner.x,
                inner.y,
                inner.w - if rw > 0.0 { rw + GAP } else { 0.0 },
                lh,
            ),
            label,
            label_style(),
            INK3,
        );
        if rw > 0.0 {
            self.text_right(
                c,
                "label right",
                Rect::new(inner.right() - rw, inner.y, rw, lh),
                right,
                right_style,
                INK3,
            );
        }
        (
            c,
            Rect::new(
                inner.x,
                inner.y + lh + MIN_GAP,
                inner.w,
                inner.h - lh - MIN_GAP,
            ),
        )
    }

    /// One message in the middle of a card (no data yet, nothing to show).
    pub fn note(&mut self, parent: usize, r: Rect, msg: &str) {
        self.text(
            parent,
            "note",
            Rect::new(r.x, r.y, r.w, 18.0),
            msg,
            body(),
            INK3,
        );
    }

    /// The 24 h chart with its time axis, in `r` (needs ≥ 2 points, else a note).
    fn chart(&mut self, parent: usize, r: Rect, values: &[f32], min_span: f32, guide: Option<f32>) {
        let axis_h = 16.0;
        if values.len() < 2 {
            self.note(parent, r, "Collecting · one point a minute");
            return;
        }
        let ch = Rect::new(r.x, r.y, r.w, r.h - axis_h - MIN_GAP);
        self.t.add(
            Some(parent),
            "chart",
            ch,
            Paint::Area {
                values: values.to_vec(),
                min_span,
                color: INK2,
                guide,
            },
        );
        let ay = ch.bottom() + MIN_GAP;
        let third = (r.w - 2.0 * GAP) / 3.0;
        self.text(
            parent,
            "axis start",
            Rect::new(r.x, ay, third, axis_h),
            "24 h ago",
            small(),
            INK3,
        );
        let mid = "12 h";
        let mw = self.f.width(&small(), mid);
        self.text(
            parent,
            "axis mid",
            Rect::new(r.x + (r.w - mw) / 2.0, ay, mw, axis_h),
            mid,
            small(),
            INK3,
        );
        self.text_right(
            parent,
            "axis end",
            Rect::new(r.right() - third, ay, third, axis_h),
            "now",
            small(),
            INK3,
        );
    }
}

/// "avg 3% · max 13%" / "min 38 · avg 41 · max 46"; empty until the chart has a line to show.
fn stats(values: &[f32], unit: &str) -> String {
    if values.len() < 2 {
        return String::new();
    }
    let lo = values.iter().copied().fold(f32::INFINITY, f32::min);
    let hi = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let avg = values.iter().sum::<f32>() / values.len() as f32;
    if unit == "%" {
        format!("avg {avg:.0}% · max {hi:.0}%")
    } else {
        format!("min {lo:.0} · avg {avg:.0} · max {hi:.0}")
    }
}

fn reading(b: &mut B, title: &str) -> Tree {
    b.header(title, "");
    let (c, r) = b.card("reading", Rect::new(M, TOP, WIDTH, 244.0), "", "");
    b.note(c, r, "Reading…");
    std::mem::take(&mut b.t)
}

// ---------------- CPU ----------------

pub fn cpu(f: &Fonts, d: Option<&CpuDetails>, trend: &[f32]) -> Tree {
    let mut b = B::new(f);
    let Some(d) = d else {
        return reading(&mut b, "CPU");
    };
    let n = d.cores.len();
    b.header("CPU", "");

    // total + load
    let (c, r) = b.card("total", Rect::new(M, TOP, 144.0, 120.0), "TOTAL", "");
    b.value(
        c,
        "value",
        Rect::new(r.x, r.y, r.w, 32.0),
        &format!("{:.0}", d.percent),
        "%",
        level_color(level("cpu.percent", d.percent)),
    );
    let y = r.y + 32.0 + MIN_GAP;
    b.text(
        c,
        "load label",
        Rect::new(r.x, y, r.w, 16.0),
        "LOAD 1·5·15 MIN",
        label_style(),
        INK3,
    );
    let load = format!("{:.2} {:.2} {:.2}", d.load[0], d.load[1], d.load[2]);
    b.text(
        c,
        "load",
        Rect::new(r.x, y + 16.0 + MIN_GAP, r.w, 18.0),
        &load,
        mono(),
        INK,
    );

    // cores: one row per core, or 4 rows of groups when there are more
    let range = if d.max_mhz > 0 {
        format!(
            "{:.1}–{:.1} GHz · {}",
            d.min_mhz as f32 / 1000.0,
            d.max_mhz as f32 / 1000.0,
            d.governor
        )
    } else {
        d.governor.clone()
    };
    let (c, r) = b.card(
        "cores",
        Rect::new(M + 144.0 + GAP, TOP, WIDTH - 144.0 - GAP, 120.0),
        &format!("{n} {}", if n == 1 { "CORE" } else { "CORES" }),
        &range,
    );
    let groups = n.div_ceil(4).max(1);
    let mut y = r.y;
    for (gi, chunk) in d.cores.chunks(groups).take(4).enumerate() {
        let first = gi * groups;
        let name = if chunk.len() == 1 {
            format!("C{first}")
        } else {
            format!("C{first}–{}", first + chunk.len() - 1)
        };
        let pct = chunk.iter().map(|c| c.percent).sum::<f64>() / chunk.len() as f64;
        let mhz = chunk.iter().map(|c| c.mhz as f64).sum::<f64>() / chunk.len() as f64;
        let row =
            b.t.add(Some(c), "core", Rect::new(r.x, y, r.w, 16.0), Paint::None);
        b.text(
            row,
            "name",
            Rect::new(r.x, y, 48.0, 16.0),
            &name,
            mono(),
            INK3,
        );
        let (pw, fw) = (40.0, 64.0);
        let bar_x = r.x + 48.0 + GAP;
        let bar_w = r.w - 48.0 - pw - fw - 3.0 * GAP;
        b.t.add(
            Some(row),
            "bar",
            Rect::new(bar_x, y + 5.0, bar_w, 6.0),
            Paint::Bar {
                pct: (pct / 100.0) as f32,
                track: theme::TRACK,
                fill: INK2,
            },
        );
        b.text_right(
            row,
            "pct",
            Rect::new(bar_x + bar_w + GAP, y, pw, 16.0),
            &format!("{pct:.0}%"),
            mono(),
            INK,
        );
        let ghz = if mhz > 0.0 {
            format!("{:.1} GHz", mhz / 1000.0)
        } else {
            "—".into()
        };
        b.text_right(
            row,
            "freq",
            Rect::new(r.right() - fw, y, fw, 16.0),
            &ghz,
            mono(),
            INK2,
        );
        y += 16.0 + MIN_GAP;
    }

    // 24 h
    let (c, r) = b.card(
        "trend",
        Rect::new(M, TOP + 128.0, 304.0, 116.0),
        "LAST 24 H",
        &stats(trend, "%"),
    );
    b.chart(c, r, trend, 10.0, None);

    // busiest
    let (c, r) = b.card(
        "busiest",
        Rect::new(M + 304.0 + GAP, TOP + 128.0, WIDTH - 304.0 - GAP, 116.0),
        "BUSIEST NOW",
        "",
    );
    if d.top.is_empty() {
        b.note(c, r, "All idle");
    }
    let mut y = r.y;
    for u in d.top.iter().take(3) {
        let v = format!("{:.1}%", u.value);
        let vw = f.width(&mono(), &v);
        let row =
            b.t.add(Some(c), "proc", Rect::new(r.x, y, r.w, 16.0), Paint::None);
        b.text(
            row,
            "name",
            Rect::new(r.x, y, r.w - vw - GAP, 16.0),
            &u.name,
            body(),
            INK2,
        );
        b.text_right(
            row,
            "value",
            Rect::new(r.right() - vw, y, vw, 16.0),
            &v,
            mono(),
            INK,
        );
        y += 16.0 + MIN_GAP;
    }
    b.t
}

// ---------------- Temperatures ----------------

pub fn temps(f: &Fonts, d: Option<&TempDetails>, trend: &[f32]) -> Tree {
    let mut b = B::new(f);
    let Some(d) = d else {
        return reading(&mut b, "Temperatures");
    };
    b.header("Temperatures", "");

    let cpu_limit = THRESHOLDS
        .iter()
        .find(|t| t.metric == "cpu.tempC")
        .map(|t| t.critical);
    let widths = [146.0, 146.0, 148.0];
    let mut x = M;
    if d.sensors.is_empty() {
        let (c, r) = b.card("sensors", Rect::new(M, TOP, WIDTH, 128.0), "SENSORS", "");
        b.note(c, r, "No temperature sensors found");
    }
    for (s, w) in d.sensors.iter().take(3).zip(widths) {
        let (c, r) = b.card(
            &s.name,
            Rect::new(x, TOP, w, 128.0),
            &s.name.to_uppercase(),
            "",
        );
        let lvl = if s.name == "CPU" {
            level("cpu.tempC", s.celsius)
        } else {
            Status::Good
        };
        b.value(
            c,
            "value",
            Rect::new(r.x, r.y, r.w, 32.0),
            &format!("{:.0}", s.celsius),
            "°C",
            level_color(lvl),
        );
        let limit = if s.name == "CPU" {
            s.limit.or(cpu_limit)
        } else {
            s.limit
        };
        let hi = s.critical.or(limit).unwrap_or(90.0).max(s.celsius + 5.0);
        let lo = 20.0_f64.min(s.celsius - 5.0);
        let at = |v: f64| ((v - lo) / (hi - lo)) as f32;
        // the fan's start temperature lives in the fan card only
        let mut marks = Vec::new();
        if let Some(l) = limit {
            marks.push((at(l), status_colors(Status::Attention).shape));
        }
        let gy = r.y + 32.0 + GAP;
        b.t.add(
            Some(c),
            "gauge",
            Rect::new(r.x, gy, r.w, 12.0),
            Paint::Gauge {
                pct: at(s.celsius),
                track: theme::TRACK,
                fill: INK2,
                marks,
            },
        );
        let caption = match (limit, s.critical) {
            (Some(l), Some(cr)) if cr > l => format!("limit {l:.0} · crit {cr:.0}"),
            (Some(l), _) => format!("limit {l:.0}"),
            (None, Some(cr)) => format!("critical {cr:.0}"),
            _ => "no limit reported".into(),
        };
        b.text(
            c,
            "caption",
            Rect::new(r.x, gy + 12.0 + GAP, r.w, 16.0),
            &caption,
            small(),
            INK3,
        );
        x += w + GAP;
    }

    let (c, r) = b.card(
        "trend",
        Rect::new(M, TOP + 136.0, 304.0, 108.0),
        "CPU · LAST 24 H",
        &stats(trend, "°C"),
    );
    b.chart(c, r, trend, 4.0, None);

    let (c, r) = b.card(
        "fan",
        Rect::new(M + 304.0 + GAP, TOP + 136.0, WIDTH - 304.0 - GAP, 108.0),
        "FAN",
        "",
    );
    match &d.fan {
        Some(fan) => {
            let on = fan.step > 0 || fan.rpm.unwrap_or(0) > 0;
            b.value(
                c,
                "value",
                Rect::new(r.x, r.y, r.w, 32.0),
                if on { "On" } else { "Off" },
                "",
                INK,
            );
            let steps = fan.steps.max(1);
            b.t.add(
                Some(c),
                "steps",
                Rect::new(r.x, r.y + 32.0 + MIN_GAP, r.w, 8.0),
                Paint::Steps {
                    n: steps,
                    on: fan.step.min(steps),
                    on_color: INK2,
                    off_color: theme::TRACK,
                },
            );
            let cap = match fan.starts_at {
                Some(t) => format!("{} rpm · on at {t:.0} °C", fan.rpm.unwrap_or(0)),
                None => format!("{} rpm", fan.rpm.unwrap_or(0)),
            };
            b.text(
                c,
                "caption",
                Rect::new(r.x, r.y + 32.0 + MIN_GAP + 8.0 + MIN_GAP, r.w, 16.0),
                &cap,
                small(),
                INK3,
            );
        }
        None => {
            b.value(c, "value", Rect::new(r.x, r.y, r.w, 32.0), "None", "", INK2);
            b.text(
                c,
                "caption",
                Rect::new(r.x, r.y + 32.0 + MIN_GAP, r.w, 16.0),
                "no fan found",
                small(),
                INK3,
            );
        }
    }
    b.t
}

// ---------------- Memory ----------------

pub fn memory(f: &Fonts, d: Option<&MemDetails>) -> Tree {
    let mut b = B::new(f);
    let Some(d) = d else {
        return reading(&mut b, "Memory");
    };
    b.header("Memory", "");

    // headline + stacked bar + legend
    let (c, r) = b.card("usage", Rect::new(M, TOP, WIDTH, 92.0), "", "");
    let used = d.total.saturating_sub(d.available);
    let (n, u) = bytes(used);
    let avail = format!("{} available", bytes_s(d.available));
    let aw = f.width(&body(), &avail);
    let pct = if d.total > 0 {
        used as f64 * 100.0 / d.total as f64
    } else {
        0.0
    };
    b.value(
        c,
        "used",
        Rect::new(r.x, r.y, r.w - aw - GAP, 28.0),
        &n,
        &format!("{u} used of {}", bytes_s(d.total)),
        level_color(level("mem.usedPercent", pct)),
    );
    b.text_right(
        c,
        "available",
        Rect::new(r.right() - aw, r.y, aw, 28.0),
        &avail,
        body(),
        INK3,
    );
    let parts = vec![
        (d.apps as f32, INK),
        (d.kernel as f32, INK2),
        (d.cache as f32, CACHE_GREY),
        (d.free as f32, theme::TRACK),
    ];
    b.t.add(
        Some(c),
        "breakdown",
        Rect::new(r.x, r.y + 28.0 + MIN_GAP, r.w, 10.0),
        Paint::Segments {
            parts,
            track: theme::TRACK,
        },
    );
    let mut x = r.x;
    let ly = r.y + 28.0 + MIN_GAP + 10.0 + MIN_GAP + 4.0;
    for (name, v, col) in [
        ("Apps", d.apps, INK),
        ("Kernel", d.kernel, INK2),
        ("Cache", d.cache, CACHE_GREY),
        ("Free", d.free, theme::TRACK),
    ] {
        let label = name.to_string();
        let val = bytes_s(v);
        let w = 10.0 + 4.0 + f.width(&body(), &label) + MIN_GAP + f.width(&mono(), &val);
        if x + w > r.right() {
            break;
        }
        let item =
            b.t.add(Some(c), name, Rect::new(x, ly, w, 16.0), Paint::None);
        let mk = if col == theme::TRACK {
            Marker::Ring(CACHE_GREY)
        } else {
            Marker::Square(col)
        };
        b.t.add(
            Some(item),
            "swatch",
            Rect::new(x, ly + 3.0, 10.0, 10.0),
            Paint::Marker(mk),
        );
        let lw = f.width(&body(), &label);
        let li = b.t.add(
            Some(item),
            "name",
            Rect::new(x + 14.0, ly, lw, 16.0),
            Paint::Text {
                text: label,
                style: body(),
                color: INK2,
            },
        );
        b.t.content(li, lw, 16.0);
        let vw = f.width(&mono(), &val);
        let vi = b.t.add(
            Some(item),
            "v",
            Rect::new(x + 14.0 + lw + MIN_GAP, ly, vw, 16.0),
            Paint::Text {
                text: val,
                style: mono(),
                color: INK,
            },
        );
        b.t.content(vi, vw, 16.0);
        x += w + 10.0;
    }

    // biggest users
    let (c, r) = b.card(
        "users",
        Rect::new(M, TOP + 100.0, 304.0, 144.0),
        "BIGGEST USERS",
        &d.top_kind,
    );
    if d.top.is_empty() {
        b.note(c, r, "Nothing to show");
    }
    let max = d.top.iter().map(|u| u.value).fold(1.0, f64::max);
    let mut y = r.y;
    for uu in d.top.iter().take(5) {
        let v = bytes_s(uu.value as u64);
        let vw = 64.0;
        let row =
            b.t.add(Some(c), "user", Rect::new(r.x, y, r.w, 16.0), Paint::None);
        b.text(
            row,
            "name",
            Rect::new(r.x, y, 112.0, 16.0),
            &uu.name,
            body(),
            INK2,
        );
        let bx = r.x + 112.0 + GAP;
        let bw = r.w - 112.0 - vw - 2.0 * GAP;
        b.t.add(
            Some(row),
            "bar",
            Rect::new(bx, y + 5.0, bw, 6.0),
            Paint::Bar {
                pct: (uu.value / max) as f32,
                track: theme::TRACK,
                fill: INK2,
            },
        );
        b.text_right(
            row,
            "value",
            Rect::new(r.right() - vw, y, vw, 16.0),
            &v,
            mono(),
            INK,
        );
        y += 16.0 + MIN_GAP;
    }

    // swap
    let label = if d.swap.as_ref().is_some_and(|s| s.kind.starts_with("zram")) {
        "SWAP · ZRAM"
    } else {
        "SWAP"
    };
    let (c, r) = b.card(
        "swap",
        Rect::new(M + 304.0 + GAP, TOP + 100.0, WIDTH - 304.0 - GAP, 144.0),
        label,
        "",
    );
    match &d.swap {
        Some(s) => {
            let (n, u) = bytes(s.used);
            b.value(c, "used", Rect::new(r.x, r.y, r.w, 32.0), &n, &u, INK);
            let y = r.y + 32.0 + MIN_GAP;
            b.text(
                c,
                "of",
                Rect::new(r.x, y, r.w, 16.0),
                &format!("of {}", bytes_s(s.total)),
                body(),
                INK2,
            );
            let pct = if s.total > 0 {
                s.used as f32 / s.total as f32
            } else {
                0.0
            };
            b.t.add(
                Some(c),
                "bar",
                Rect::new(r.x, y + 16.0 + MIN_GAP, r.w, 6.0),
                Paint::Bar {
                    pct,
                    track: theme::TRACK,
                    fill: INK2,
                },
            );
            // "zstd · empty" / "zstd · in RAM" for zram, the swap type otherwise
            let algo = s.kind.split(" · ").nth(1).map(str::to_string);
            let cap = match (algo, s.used == 0) {
                (Some(a), true) => format!("{a} · empty"),
                (Some(a), false) => format!("{a} · in RAM"),
                (None, true) => format!("{} · empty", s.kind),
                (None, false) => s.kind.clone(),
            };
            b.text(
                c,
                "kind",
                Rect::new(r.x, y + 16.0 + MIN_GAP + 6.0 + MIN_GAP, r.w, 16.0),
                &cap,
                small(),
                INK3,
            );
        }
        None => {
            b.value(c, "used", Rect::new(r.x, r.y, r.w, 32.0), "None", "", INK2);
            b.text(
                c,
                "kind",
                Rect::new(r.x, r.y + 32.0 + MIN_GAP, r.w, 16.0),
                "no swap set up",
                small(),
                INK3,
            );
        }
    }
    b.t
}

// ---------------- Disk ----------------

pub fn disk(f: &Fonts, d: Option<&DiskDetails>, reads: &[f32], writes: &[f32]) -> Tree {
    let mut b = B::new(f);
    let Some(d) = d else {
        return reading(&mut b, "Disk");
    };
    b.header("Disk", "");

    // root + one more filesystem
    let root = d.filesystems.first();
    let lab = root
        .map(|fs| {
            if fs.mount == "/" {
                "/ ROOT".to_string()
            } else {
                fs.mount.to_uppercase()
            }
        })
        .unwrap_or_else(|| "FILESYSTEM".into());
    let (c, r) = b.card("root", Rect::new(M, TOP, 304.0, 116.0), &lab, "");
    match root {
        Some(fs) => {
            let pct = fs.percent;
            let p = format!("{pct:.0}%");
            let pw = f.width(&mono(), &p);
            let (n, _) = bytes(fs.used);
            b.value(
                c,
                "used",
                Rect::new(r.x, r.y, r.w - pw - GAP, 32.0),
                &n,
                &format!(" of {}", bytes_s(fs.size)),
                level_color(level("disk.usedPercent", pct)),
            );
            b.text_right(
                c,
                "pct",
                Rect::new(r.right() - pw, r.y, pw, 32.0),
                &p,
                mono(),
                INK2,
            );
            let fill = if level("disk.usedPercent", pct) == Status::Good {
                INK2
            } else {
                status_colors(level("disk.usedPercent", pct)).shape
            };
            b.t.add(
                Some(c),
                "bar",
                Rect::new(r.x, r.y + 32.0 + MIN_GAP, r.w, 10.0),
                Paint::Bar {
                    pct: (pct / 100.0) as f32,
                    track: theme::TRACK,
                    fill,
                },
            );
            if let Some(fs2) = d.filesystems.get(1) {
                let y = r.y + 32.0 + MIN_GAP + 10.0 + MIN_GAP;
                let v = format!("{} of {}", bytes_s(fs2.used), bytes_s(fs2.size));
                let vw = f.width(&mono(), &v).min(r.w * 0.6);
                let row =
                    b.t.add(Some(c), "second", Rect::new(r.x, y, r.w, 16.0), Paint::None);
                b.text(
                    row,
                    "mount",
                    Rect::new(r.x, y, r.w - vw - GAP, 16.0),
                    &fs2.mount,
                    body(),
                    INK3,
                );
                b.text_right(
                    row,
                    "value",
                    Rect::new(r.right() - vw, y, vw, 16.0),
                    &v,
                    mono(),
                    INK2,
                );
            }
        }
        None => b.note(c, r, "No filesystem found"),
    }

    // drive
    // (its temperature is on the Temperatures screen)
    let (c, r) = b.card(
        "drive",
        Rect::new(M + 304.0 + GAP, TOP, WIDTH - 304.0 - GAP, 116.0),
        "DRIVE",
        d.drive.as_ref().map_or("", |dr| dr.kind.as_str()),
    );
    match &d.drive {
        Some(dr) => {
            b.text(
                c,
                "model",
                Rect::new(r.x, r.y, r.w, 16.0),
                &format!("{} · {}", bytes_s(dr.size), dr.model),
                body(),
                INK2,
            );
            let (n, u) = bytes(dr.written);
            b.value(
                c,
                "written",
                Rect::new(r.x, r.y + 16.0 + MIN_GAP, r.w, 28.0),
                &n,
                &u,
                INK,
            );
            b.text(
                c,
                "caption",
                Rect::new(r.x, r.y + 16.0 + MIN_GAP + 28.0 + MIN_GAP, r.w, 16.0),
                "written since boot",
                small(),
                INK3,
            );
        }
        None => b.note(c, r, "Unknown drive"),
    }

    // I/O right now
    let (c, r) = b.card(
        "io",
        Rect::new(M, TOP + 124.0, 222.0, 120.0),
        "RIGHT NOW",
        "",
    );
    let mut y = r.y;
    for (name, v, hist) in [("Read", d.read_bps, reads), ("Write", d.write_bps, writes)] {
        let row =
            b.t.add(Some(c), name, Rect::new(r.x, y, r.w, 32.0), Paint::None);
        b.text(
            row,
            "name",
            Rect::new(r.x, y, 40.0, 32.0),
            name,
            body(),
            INK2,
        );
        b.text(
            row,
            "value",
            Rect::new(r.x + 40.0 + GAP, y, 76.0, 32.0),
            &rate(v),
            mono(),
            INK,
        );
        let sx = r.x + 40.0 + 76.0 + 2.0 * GAP;
        b.t.add(
            Some(row),
            "spark",
            Rect::new(sx, y + 2.0, r.right() - sx, 28.0),
            Paint::Area {
                values: hist.to_vec(),
                min_span: 1024.0,
                color: INK2,
                guide: None,
            },
        );
        y += 32.0 + GAP;
    }

    // Docker
    let (right, rows): (String, Vec<(String, String)>) = match &d.docker {
        Some(dk) => (
            if dk.reclaimable > 0 {
                format!("{} can be freed", bytes_s(dk.reclaimable))
            } else {
                String::new()
            },
            vec![
                (format!("Images · {}", dk.images), bytes_s(dk.images_bytes)),
                (
                    format!("Containers · {}", dk.containers),
                    bytes_s(dk.containers_bytes),
                ),
                (
                    format!("Volumes · {}", dk.volumes),
                    bytes_s(dk.volumes_bytes),
                ),
            ],
        ),
        None => (String::new(), vec![]),
    };
    let (c, r) = b.card(
        "docker",
        Rect::new(M + 222.0 + GAP, TOP + 124.0, WIDTH - 222.0 - GAP, 120.0),
        "DOCKER",
        &right,
    );
    if rows.is_empty() {
        b.note(c, r, "Docker not reachable");
    }
    let mut y = r.y;
    for (name, v) in rows {
        let vw = f.width(&mono(), &v);
        let row =
            b.t.add(Some(c), "row", Rect::new(r.x, y, r.w, 18.0), Paint::None);
        b.text(
            row,
            "name",
            Rect::new(r.x, y, r.w - vw - GAP, 18.0),
            &name,
            body(),
            INK2,
        );
        b.text_right(
            row,
            "value",
            Rect::new(r.right() - vw, y, vw, 18.0),
            &v,
            mono(),
            INK,
        );
        y += 18.0 + 6.0;
    }
    b.t
}

// ---------------- History ----------------

pub const ROWS_PER_PAGE: usize = 3;

fn incident_status(i: &Incident) -> Status {
    match (i.ended_at.is_some(), i.severity.as_str()) {
        (true, _) => Status::Unknown,
        (false, "critical") => Status::Down,
        (false, _) => Status::Attention,
    }
}

/// "15:42 Sun 22 Nov · lasted 3 m" / "since 15:42 · 2 h 05 m so far"
fn when(i: &Incident, now_ms: i64, at: &dyn Fn(i64) -> (String, String)) -> String {
    let (t, d) = at(i.started_at);
    match i.ended_at {
        Some(e) => format!("{t} {d} · lasted {}", duration(e - i.started_at)),
        None => format!("since {t} {d} · {} so far", duration(now_ms - i.started_at)),
    }
}

/// History's two tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HistTab {
    #[default]
    Incidents,
    Events,
}

const HIST_TAB_W: f32 = 96.0;

#[allow(clippy::too_many_arguments)]
pub fn history(
    f: &Fonts,
    list: Option<&[Incident]>,
    events: Option<&[Event]>,
    tab: HistTab,
    page: usize,
    now_ms: i64,
    at: &dyn Fn(i64) -> (String, String),
) -> Tree {
    let mut b = B::new(f);
    tabs_header(
        &mut b,
        "History",
        &["Incidents", "Events"],
        HIST_TAB_W,
        tab as usize,
        &[],
    );
    match (tab, list, events) {
        (HistTab::Incidents, Some(l), _) => incidents_list(&mut b, l, page, now_ms, at),
        (HistTab::Events, _, Some(e)) => events_list(&mut b, e, page, now_ms, at),
        _ => empty_note(&mut b, "Reading…"),
    }
    b.t
}

fn incidents_list(
    b: &mut B,
    list: &[Incident],
    page: usize,
    now_ms: i64,
    at: &dyn Fn(i64) -> (String, String),
) {
    let f = b.f;
    let n = list.len();
    if list.is_empty() {
        let (c, r) = b.card("empty", Rect::new(M, TOP, WIDTH, 244.0), "", "");
        b.t.add(
            Some(c),
            "icon",
            Rect::new(r.x + (r.w - 60.0) / 2.0, r.y + 40.0, 60.0, 60.0),
            Paint::StatusIcon(Status::Good),
        );
        let msg = "No incidents in the last 7 days";
        let mw = f.width(&title_style(), msg);
        b.text(
            c,
            "msg",
            Rect::new(r.x + (r.w - mw) / 2.0, r.y + 112.0, mw, 22.0),
            msg,
            title_style(),
            INK,
        );
        let sub = "CPU, memory, disk, temperature and containers are watched";
        let sw = f.width(&body(), sub).min(r.w);
        b.text(
            c,
            "sub",
            Rect::new(r.x + (r.w - sw) / 2.0, r.y + 138.0, sw, 18.0),
            sub,
            body(),
            INK3,
        );
        return;
    }
    let pages = n.div_ceil(ROWS_PER_PAGE);
    let page = page.min(pages - 1);
    let mut y = TOP;
    for i in list.iter().skip(page * ROWS_PER_PAGE).take(ROWS_PER_PAGE) {
        let row = b.t.add(
            None,
            "incident",
            Rect::new(M, y, WIDTH, 56.0),
            Paint::Panel {
                fill: theme::SURFACE,
                radius: theme::RADIUS_TILE,
                border: None,
            },
        );
        b.t.nav(row, Nav::Incident(i.id));
        b.t.pad(row, 8.0);
        let inner = Rect::new(M, y, WIDTH, 56.0).inset(8.0);
        let st = incident_status(i);
        let mk = match st {
            Status::Down => Marker::Square(status_colors(st).shape),
            Status::Attention => Marker::Triangle(status_colors(st).shape),
            _ => Marker::Ring(INK3),
        };
        b.t.add(
            Some(row),
            "marker",
            Rect::new(inner.x + 4.0, inner.cy() - 5.0, 10.0, 10.0),
            Paint::Marker(mk),
        );
        let tx = inner.x + 4.0 + 10.0 + 12.0;
        let peak = match (i.peak_value, unit_for(&i.subject)) {
            (Some(v), u) if !u.is_empty() => format!("{v:.0}{u}"),
            _ => String::new(),
        };
        let pw = if peak.is_empty() {
            0.0
        } else {
            f.width(&mono(), &peak)
        };
        let tw = inner.right() - tx - if pw > 0.0 { pw + GAP } else { 0.0 };
        let title_color = if st == Status::Unknown {
            INK
        } else {
            status_colors(st).text
        };
        b.text(
            row,
            "title",
            Rect::new(tx, inner.y + 1.0, tw, 18.0),
            &i.title,
            title_style(),
            title_color,
        );
        b.text(
            row,
            "when",
            Rect::new(tx, inner.y + 1.0 + 18.0 + MIN_GAP, inner.right() - tx, 16.0),
            &when(i, now_ms, at),
            body(),
            INK3,
        );
        if pw > 0.0 {
            b.text_right(
                row,
                "peak",
                Rect::new(inner.right() - pw, inner.y + 1.0, pw, 18.0),
                &peak,
                mono(),
                INK2,
            );
        }
        y += 56.0 + GAP;
    }
    // pager (only when there is more than one page)
    if pages > 1 {
        pager(b, TOP + ROWS_PER_PAGE as f32 * (56.0 + GAP), page, pages);
    }
}

pub const EVENT_ROWS: usize = 4;

/// (title, rest, marker) of one event row: "SSH" · "admin from 100.64.0.7 · ×3".
fn event_text(e: &Event) -> (String, String, Marker) {
    let times = if e.count > 1 {
        format!(" · ×{}", e.count)
    } else {
        String::new()
    };
    match e.kind.as_str() {
        "ssh" => (
            "SSH".into(),
            format!("{} from {}{times}", e.user, e.from),
            Marker::Dot(INK2),
        ),
        "ssh_failed" => (
            "Failed SSH".into(),
            format!("{} from {}{times}", e.user, e.from),
            Marker::Square(status_colors(Status::Down).shape),
        ),
        "sudo" => (
            "sudo".into(),
            format!("{}: {}", e.user, e.detail),
            Marker::Dot(status_colors(Status::Attention).shape),
        ),
        "deploy" if e.detail == "--rollback" => (
            "Rolled back".into(),
            "by Jenkins".into(),
            Marker::Dot(status_colors(Status::Good).shape),
        ),
        "deploy" => (
            format!("Deployed {}", e.detail),
            "by Jenkins".into(),
            Marker::Dot(status_colors(Status::Good).shape),
        ),
        "boot" => (
            "Booted".into(),
            e.detail
                .parse::<i64>()
                .map(|ms| format!("was off {}", duration(ms)))
                .unwrap_or_default(),
            Marker::Ring(INK2),
        ),
        other => (other.into(), e.detail.clone(), Marker::Ring(INK3)),
    }
}

/// Pages of at most EVENT_ROWS, never mixing two days (the card label names the day).
pub fn event_pages<'a>(
    list: &'a [Event],
    at: &dyn Fn(i64) -> (String, String),
) -> Vec<(String, Vec<&'a Event>)> {
    let mut pages: Vec<(String, Vec<&Event>)> = Vec::new();
    for e in list {
        let day = at(e.ts).1;
        match pages.last_mut() {
            Some((d, rows)) if *d == day && rows.len() < EVENT_ROWS => rows.push(e),
            _ => pages.push((day, vec![e])),
        }
    }
    pages
}

/// "TODAY · 14 SSH · 31 SUDO · 3 DEPLOYS" for today, else the day ("SAT 21 NOV").
fn day_label(
    list: &[Event],
    day: &str,
    today: &str,
    at: &dyn Fn(i64) -> (String, String),
) -> String {
    if day != today {
        return day.to_uppercase();
    }
    let mut n = [0u32; 4]; // ssh, sudo, deploys, failed
    for e in list.iter().filter(|e| at(e.ts).1 == day) {
        match e.kind.as_str() {
            "ssh" => n[0] += e.count,
            "sudo" => n[1] += 1,
            "deploy" => n[2] += 1,
            "ssh_failed" => n[3] += e.count,
            _ => {}
        }
    }
    let mut s = "TODAY".to_string();
    for (k, word) in n.iter().zip(["SSH", "SUDO", "DEPLOY", "FAILED"]) {
        if *k > 0 {
            let plural = if word == "DEPLOY" && *k != 1 { "S" } else { "" };
            s.push_str(&format!(" · {k} {word}{plural}"));
        }
    }
    s
}

fn events_list(
    b: &mut B,
    list: &[Event],
    page: usize,
    now_ms: i64,
    at: &dyn Fn(i64) -> (String, String),
) {
    if list.is_empty() {
        empty_note(
            b,
            "No SSH logins, sudo, deploys or boots in the last 7 days",
        );
        return;
    }
    let pages = event_pages(list, at);
    let page = page.min(pages.len() - 1);
    let (day, rows) = &pages[page];
    let label = day_label(list, day, &at(now_ms).1, at);
    let (c, r) = b.card("events", Rect::new(M, TOP, WIDTH, 192.0), &label, "");
    let row_h = 34.0;
    let time_w = b.f.width(&mono(), "00:00 AM");
    let title_style = Style::new(Face::SemiBold, size::BODY.0);
    let mut y = r.y;
    for e in rows {
        let (title, rest, mk) = event_text(e);
        let row =
            b.t.add(Some(c), "row", Rect::new(r.x, y, r.w, row_h), Paint::None);
        b.t.add(
            Some(row),
            "marker",
            Rect::new(r.x, y + (row_h - 10.0) / 2.0, 10.0, 10.0),
            Paint::Marker(mk),
        );
        let nx = r.x + 10.0 + GAP;
        let text_right = r.right() - time_w - GAP;
        let tw = b.f.width(&title_style, &title).min(text_right - nx);
        b.text(
            row,
            "title",
            Rect::new(nx, y, tw, row_h),
            &title,
            title_style,
            INK,
        );
        let rx = nx + tw + MIN_GAP;
        if !rest.is_empty() && text_right - rx > 24.0 {
            b.text(
                row,
                "rest",
                Rect::new(rx, y, text_right - rx, row_h),
                &format!("· {rest}"),
                body(),
                INK3,
            );
        }
        b.text_right(
            row,
            "time",
            Rect::new(r.right() - time_w, y, time_w, row_h),
            &at(e.ts).0,
            mono(),
            INK3,
        );
        y += row_h + MIN_GAP;
    }
    if pages.len() > 1 {
        pager(b, theme::H - M - 44.0, page, pages.len());
    }
}

/// ‹ 1 / 3 › at `py`, full width; the arrows keep the open screen and change its page.
fn pager(b: &mut B, py: f32, page: usize, pages: usize) {
    {
        let f = b.f;
        let pager =
            b.t.add(None, "pager", Rect::new(M, py, WIDTH, 44.0), Paint::None);
        for (name, x, enabled, nav, flip) in [
            (
                "previous",
                M,
                page > 0,
                Nav::Page(page.saturating_sub(1)),
                false,
            ),
            (
                "next",
                M + WIDTH - 44.0,
                page + 1 < pages,
                Nav::Page(page + 1),
                true,
            ),
        ] {
            let bt = b.t.add(
                Some(pager),
                name,
                Rect::new(x, py, 44.0, 44.0),
                Paint::Panel {
                    fill: theme::SURFACE,
                    radius: theme::RADIUS_TILE,
                    border: None,
                },
            );
            if enabled {
                b.t.nav(bt, nav);
            }
            let col = if enabled { INK } else { theme::MUTED };
            b.t.add(
                Some(bt),
                "chevron",
                Rect::new(x + 12.0, py + 12.0, 20.0, 20.0),
                Paint::Chevron(col, flip),
            );
        }
        let label = format!("{} / {}", page + 1, pages);
        let lw = f.width(&mono(), &label);
        b.text(
            pager,
            "page",
            Rect::new(M + (WIDTH - lw) / 2.0, py, lw, 44.0),
            &label,
            mono(),
            INK2,
        );
    }
}

// ---------------- one incident ----------------

fn snap_rows(s: &Snapshot) -> Vec<(&'static str, String, Status)> {
    let mut v = vec![
        (
            "CPU",
            format!("{:.0}%", s.cpu.percent),
            level("cpu.percent", s.cpu.percent),
        ),
        (
            "Temp",
            s.cpu
                .temp_c
                .map(|t| format!("{t:.0}°C"))
                .unwrap_or("—".into()),
            s.cpu
                .temp_c
                .map(|t| level("cpu.tempC", t))
                .unwrap_or(Status::Good),
        ),
        (
            "RAM",
            format!("{:.0}%", s.memory.used_percent),
            level("mem.usedPercent", s.memory.used_percent),
        ),
        (
            "Disk",
            format!("{:.0}%", s.disk.used_percent),
            level("disk.usedPercent", s.disk.used_percent),
        ),
    ];
    v.push(match &s.docker {
        Some(d) => (
            "Apps",
            format!("{}/{}", d.running, d.total),
            if d.running < d.total {
                Status::Attention
            } else {
                Status::Good
            },
        ),
        None => ("Apps", "—".into(), Status::Down),
    });
    v
}

pub fn incident(
    f: &Fonts,
    d: Option<&IncidentFull>,
    now_ms: i64,
    at: &dyn Fn(i64) -> (String, String),
) -> Tree {
    let mut b = B::new(f);
    let Some(d) = d else {
        return reading(&mut b, "Incident");
    };
    let i = &d.incident;
    let st = incident_status(i);
    b.header(
        &i.title,
        if i.ended_at.is_none() {
            "ongoing"
        } else {
            &i.severity
        },
    );

    let (c, r) = b.card("when", Rect::new(M, TOP, WIDTH, 76.0), "", "");
    let (t, day) = at(i.started_at);
    let peak = match (i.peak_value, unit_for(&i.subject)) {
        (Some(v), u) if !u.is_empty() => format!("peak {v:.0}{u}"),
        _ => String::new(),
    };
    let pw = f.width(&mono(), &peak);
    b.text(
        c,
        "started",
        Rect::new(r.x, r.y, r.w - pw - GAP, 22.0),
        &format!("Started {t} · {day}"),
        title_style(),
        INK,
    );
    if !peak.is_empty() {
        b.text_right(
            c,
            "peak",
            Rect::new(r.right() - pw, r.y, pw, 22.0),
            &peak,
            mono(),
            level_color(if st == Status::Unknown {
                Status::Good
            } else {
                st
            }),
        );
    }
    let line2 = match i.ended_at {
        Some(e) => {
            let (et, _) = at(e);
            let why = match i.end_reason.as_deref() {
                Some("recovered") | None => String::new(),
                Some(r) => format!(" · {r}"),
            };
            format!("Lasted {} · ended {et}{why}", duration(e - i.started_at))
        }
        None => format!("Still going · {} so far", duration(now_ms - i.started_at)),
    };
    b.text(
        c,
        "ended",
        Rect::new(r.x, r.y + 22.0 + MIN_GAP, r.w, 18.0),
        &line2,
        body(),
        INK2,
    );

    let widths = [146.0, 146.0, 148.0];
    let mut x = M;
    for ((label, snap), w) in [
        ("START", d.snapshots.start.as_ref()),
        ("PEAK", d.snapshots.peak.as_ref()),
        ("END", d.snapshots.end.as_ref()),
    ]
    .into_iter()
    .zip(widths)
    {
        let when = snap.map(|s| at(s.ts).0).unwrap_or_default();
        let (c, r) = b.card(label, Rect::new(x, TOP + 84.0, w, 160.0), label, &when);
        match snap {
            Some(s) => {
                let mut y = r.y;
                for (name, v, lvl) in snap_rows(s) {
                    let vw = f.width(&mono(), &v);
                    let row =
                        b.t.add(Some(c), name, Rect::new(r.x, y, r.w, 18.0), Paint::None);
                    b.text(
                        row,
                        "name",
                        Rect::new(r.x, y, r.w - vw - GAP, 18.0),
                        name,
                        body(),
                        INK2,
                    );
                    b.text_right(
                        row,
                        "value",
                        Rect::new(r.right() - vw, y, vw, 18.0),
                        &v,
                        mono(),
                        level_color(lvl),
                    );
                    y += 18.0 + MIN_GAP;
                }
            }
            None => b.note(
                c,
                r,
                if label == "END" && i.ended_at.is_none() {
                    "Still going"
                } else {
                    "Not recorded"
                },
            ),
        }
        x += w + GAP;
    }
    b.t
}

// ---------------- Services ----------------

/// How a container is doing: red when it isn't running (as Home counts it), amber when Docker
/// says it's unhealthy, restarting or paused.
pub fn service_status(s: &Service) -> Status {
    match s.state.as_str() {
        "running" if s.health.as_deref() == Some("unhealthy") => Status::Attention,
        "running" => Status::Good,
        "restarting" | "paused" => Status::Attention,
        _ => Status::Down,
    }
}

/// Problems first (red, then amber), then by name.
pub fn sorted(list: &[Service]) -> Vec<&Service> {
    let rank = |s: &Service| match service_status(s) {
        Status::Down => 0,
        Status::Attention => 1,
        _ => 2,
    };
    let mut v: Vec<&Service> = list.iter().collect();
    v.sort_by(|a, b| rank(a).cmp(&rank(b)).then(a.name.cmp(&b.name)));
    v
}

/// "4 h", "12 m", "3 d"
fn short_dur(sec: u64) -> String {
    match sec {
        0..60 => format!("{sec} s"),
        60..3600 => format!("{} m", sec / 60),
        3600..86400 => format!("{} h", sec / 3600),
        _ => format!("{} d", sec / 86400),
    }
}

/// The tile's second line: health or uptime, and memory; or why it isn't running.
fn tile_line(s: &Service) -> String {
    let mem = s
        .mem
        .map(|m| format!(" · {}", bytes_s(m)))
        .unwrap_or_default();
    match s.state.as_str() {
        "running" => match (&s.health, s.up_sec) {
            (Some(h), _) => format!("{h}{mem}"),
            (None, Some(u)) => format!("up {}{mem}", short_dur(u)),
            (None, None) => format!("running{mem}"),
        },
        "exited" => match &s.ago {
            Some(a) => format!("exited {a}"),
            None => "exited".into(),
        },
        "created" => "never started".into(),
        "stopped" => "not running".into(),
        "paused" => format!("paused{mem}"),
        other => other.into(),
    }
}

fn status_marker(st: Status) -> Marker {
    match st {
        Status::Down => Marker::Square(status_colors(st).shape),
        Status::Attention => Marker::Triangle(status_colors(st).shape),
        Status::Good => Marker::Dot(status_colors(st).shape),
        Status::Unknown => Marker::Ring(INK3),
    }
}

/// Rows per page in the Apps and Host lists: 44 px rows (touch targets, 8 px apart), so every
/// name gets the full width. 4 fit when there is no pager; with the pager, 3.
pub fn list_rows(n: usize) -> usize {
    if n <= 4 { 4 } else { 3 }
}
/// Taller than the 44 px minimum: easier to hit, and 4 rows still fit without the pager.
const ROW_H: f32 = 54.0;

/// The three tabs of the Services screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Apps,
    Host,
    Web,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Apps, Tab::Host, Tab::Web];
    pub fn label(self) -> &'static str {
        match self {
            Tab::Apps => "Apps",
            Tab::Host => "Host",
            Tab::Web => "Web",
        }
    }
}

pub fn web_status(c: &WebCheck) -> Status {
    if c.ok {
        Status::Good
    } else if c.down() {
        Status::Down
    } else {
        Status::Attention
    }
}

/// Which tabs hold a problem (▲ on the tab).
pub fn tab_problems(d: &Services) -> [bool; 3] {
    [
        !d.reachable || d.services.iter().any(|s| service_status(s) != Status::Good),
        d.host.iter().any(|s| service_status(s) != Status::Good),
        d.web
            .as_ref()
            .is_some_and(|w| w.checks.iter().any(|c| !c.ok)),
    ]
}

pub const TAB_W: f32 = 72.0;

/// Header with the title on the left and the three tabs (44 px touch targets) on the right.
fn tabs_header(
    b: &mut B,
    title: &str,
    labels: &[&str],
    tab_w: f32,
    active: usize,
    problems: &[bool],
) {
    b.header(title, "");
    let n = labels.len() as f32;
    let tabs_w = n * tab_w + (n - 1.0) * GAP;
    let x0 = theme::W - M - 44.0 - GAP - tabs_w; // left of the refresh button
    // keep the title clear of the tabs
    if let Some(t) = b.t.items.iter_mut().find(|i| i.name == "title") {
        t.rect.w = t.rect.w.min(x0 - GAP - t.rect.x);
    }
    let h = 0; // the header is the first item
    let ls = Style::new(Face::SemiBold, size::BODY.0);
    for (i, label) in labels.iter().enumerate() {
        let r = Rect::new(x0 + i as f32 * (tab_w + GAP), M, tab_w, 44.0);
        let on = i == active;
        let warn = problems.get(i).copied().unwrap_or(false);
        let it = b.t.add(
            Some(h),
            *label,
            r,
            Paint::Panel {
                fill: if on { theme::TRACK } else { theme::SURFACE },
                radius: theme::RADIUS_TILE,
                border: None,
            },
        );
        b.t.nav(it, Nav::Tab(i as u8));
        b.t.pad(it, 8.0);
        let lw = b.f.width(&ls, label);
        let mark = if warn { 10.0 + MIN_GAP } else { 0.0 };
        let x = r.x + (r.w - lw - mark) / 2.0;
        b.text(
            it,
            "label",
            Rect::new(x, r.y + 8.0, lw, 28.0),
            label,
            ls,
            if on { INK } else { INK2 },
        );
        if warn {
            b.t.add(
                Some(it),
                "warn",
                Rect::new(x + lw + MIN_GAP, r.cy() - 5.0, 10.0, 10.0),
                Paint::Marker(Marker::Triangle(status_colors(Status::Attention).shape)),
            );
        }
    }
}

fn empty_note(b: &mut B, msg: &str) {
    let (c, r) = b.card("empty", Rect::new(M, TOP, WIDTH, 244.0), "", "");
    b.note(c, r, msg);
}

pub fn services(
    f: &Fonts,
    d: Option<&Services>,
    tab: Tab,
    page: usize,
    at: &dyn Fn(i64) -> (String, String),
) -> Tree {
    let mut b = B::new(f);
    let Some(d) = d else {
        return reading(&mut b, "Services");
    };
    let labels = Tab::ALL.map(Tab::label);
    tabs_header(
        &mut b,
        "Services",
        &labels,
        TAB_W,
        tab as usize,
        &tab_problems(d),
    );
    match tab {
        Tab::Apps if !d.reachable => empty_note(&mut b, "Docker not reachable"),
        Tab::Apps if d.services.is_empty() => empty_note(&mut b, "No containers"),
        Tab::Apps => grid(&mut b, &sorted(&d.services), page),
        Tab::Host if d.host.is_empty() => empty_note(
            &mut b,
            "No host services set up (host lines in /etc/pios/checks)",
        ),
        Tab::Host => grid(&mut b, &sorted(&d.host), page),
        Tab::Web => match &d.web {
            Some(w) if !w.checks.is_empty() => web_list(&mut b, w, page, at),
            _ => empty_note(&mut b, "No web results (web lines in /etc/pios/checks)"),
        },
    }
    b.t
}

/// One row per container or host service: status, full name, health or uptime and memory.
/// More than 4: 3 per page and the ‹ › pager.
fn grid(b: &mut B, list: &[&Service], page: usize) {
    let per = list_rows(list.len());
    let pages = list.len().div_ceil(per).max(1);
    let page = page.min(pages - 1);
    let name_style = Style::new(Face::SemiBold, size::TITLE.0);
    for (i, s) in list.iter().skip(page * per).take(per).enumerate() {
        let r = Rect::new(M, TOP + i as f32 * (ROW_H + GAP), WIDTH, ROW_H);
        let st = service_status(s);
        let sc = status_colors(st);
        let row = b.t.add(
            None,
            "service",
            r,
            Paint::Panel {
                fill: if st == Status::Good {
                    theme::SURFACE
                } else {
                    sc.card
                },
                radius: theme::RADIUS_TILE,
                border: (st != Status::Good).then_some(sc.border),
            },
        );
        b.t.nav(row, Nav::Container(crate::services::key(&s.id)));
        b.t.pad(row, PAD_TILE);
        let inner = r.inset(PAD_TILE);
        b.t.add(
            Some(row),
            "marker",
            Rect::new(inner.x, inner.cy() - 5.0, 10.0, 10.0),
            Paint::Marker(status_marker(st)),
        );
        // the detail on the right takes what it needs (up to 45 %), the name gets the rest
        let line = tile_line(s);
        let lw = b.f.width(&body(), &line).min(inner.w * 0.45);
        let nx = inner.x + 10.0 + GAP;
        b.text(
            row,
            "name",
            Rect::new(nx, inner.y, inner.right() - lw - GAP - nx, inner.h),
            &s.name,
            name_style,
            INK,
        );
        b.text_right(
            row,
            "line",
            Rect::new(inner.right() - lw, inner.y, lw, inner.h),
            &line,
            body(),
            if st == Status::Good { INK2 } else { sc.text },
        );
    }
    if pages > 1 {
        pager(b, theme::H - M - 44.0, page, pages);
    }
}

pub const WEB_ROWS: usize = 4;

/// The domain every host shares ("example.com"), if they all do: shown once, in the label.
fn shared_domain(hosts: &[&str]) -> Option<String> {
    let first = hosts.first()?;
    let dom = first.split_once('.')?.1;
    (dom.contains('.') || hosts.len() > 1)
        .then_some(())
        .filter(|_| {
            hosts
                .iter()
                .all(|h| h.split_once('.').is_some_and(|(_, d)| d == dom))
        })
        .map(|_| dom.to_string())
}

/// What a failing check says instead of its time: "want 200", "timeout", "no DNS"…
fn web_note(c: &WebCheck) -> String {
    if c.ok {
        return c
            .ms
            .map(|ms| format!("{:.2} s", ms as f64 / 1000.0))
            .unwrap_or_default();
    }
    if c.code.is_some() {
        return format!("want {}", c.expect);
    }
    let e = c.error.as_deref().unwrap_or("").to_ascii_lowercase();
    if e.contains("timed out") {
        "timeout"
    } else if e.contains("resolve") {
        "no DNS"
    } else if e.contains("ssl") || e.contains("certificate") || e.contains("tls") {
        "TLS error"
    } else if e.contains("connect") {
        "no connection"
    } else {
        "no answer"
    }
    .into()
}

fn web_list(b: &mut B, w: &Web, page: usize, at: &dyn Fn(i64) -> (String, String)) {
    let mut list: Vec<&WebCheck> = w.checks.iter().collect();
    let rank = |c: &WebCheck| match web_status(c) {
        Status::Down => 0,
        Status::Attention => 1,
        _ => 2,
    };
    list.sort_by_key(|c| rank(c)); // stable: keeps the file's order within each group
    // "drive.example.com/status.php": the domain from the hostname part only
    let hosts: Vec<&str> = list
        .iter()
        .map(|c| c.host.split('/').next().unwrap_or(&c.host))
        .collect();
    let domain = shared_domain(&hosts);
    let pages = list.len().div_ceil(WEB_ROWS).max(1);
    let page = page.min(pages - 1);
    let label = domain
        .as_ref()
        .map(|d| format!("*.{}", d.to_uppercase()))
        .unwrap_or_else(|| "PUBLIC HOSTNAMES".into());
    let (t, _) = at(w.checked_at);
    let (c, r) = b.card(
        "web",
        Rect::new(M, TOP, WIDTH, 192.0),
        &label,
        &format!("checked {t} · every {} s", w.every_sec),
    );
    let row_h = 34.0;
    let code_w = 52.0;
    let note_w = 104.0;
    let mut y = r.y;
    for ch in list.iter().skip(page * WEB_ROWS).take(WEB_ROWS) {
        let st = web_status(ch);
        let sc = status_colors(st);
        let row =
            b.t.add(Some(c), "row", Rect::new(r.x, y, r.w, row_h), Paint::None);
        b.t.add(
            Some(row),
            "marker",
            Rect::new(r.x, y + (row_h - 10.0) / 2.0, 10.0, 10.0),
            Paint::Marker(status_marker(st)),
        );
        let name = match &domain {
            Some(d) => {
                let (host, path) = ch.host.split_once('/').unwrap_or((&ch.host, ""));
                let label = host.strip_suffix(&format!(".{d}")).unwrap_or(host);
                if path.is_empty() {
                    label.to_string()
                } else {
                    format!("{label}/{path}")
                }
            }
            None => ch.host.clone(),
        };
        let nx = r.x + 10.0 + GAP;
        let right = code_w + GAP + note_w + GAP;
        b.text(
            row,
            "host",
            Rect::new(nx, y, r.right() - right - nx, row_h),
            &name,
            Style::new(Face::SemiBold, size::BODY.0),
            INK,
        );
        b.text_right(
            row,
            "note",
            Rect::new(r.right() - code_w - GAP - note_w, y, note_w, row_h),
            &web_note(ch),
            mono(),
            if st == Status::Good { INK3 } else { sc.text },
        );
        let badge = b.t.add(
            Some(row),
            "code",
            Rect::new(r.right() - code_w, y + 6.0, code_w, row_h - 12.0),
            Paint::Panel {
                fill: theme::RAISED,
                radius: 6.0,
                border: None,
            },
        );
        let code = ch.code.map(|c| c.to_string()).unwrap_or_else(|| "—".into());
        let cw = b.f.width(&mono(), &code);
        b.text(
            badge,
            "value",
            Rect::new(
                r.right() - code_w + (code_w - cw) / 2.0,
                y + 6.0,
                cw,
                row_h - 12.0,
            ),
            &code,
            mono(),
            if st == Status::Good { INK2 } else { sc.text },
        );
        y += row_h + MIN_GAP;
    }
    if pages > 1 {
        pager(b, theme::H - M - 44.0, page, pages);
    }
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// The Restart button's state, beyond what the agent reports.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Hold<'a> {
    /// being held: 0–1 of the 2 s
    pub progress: Option<f32>,
    /// the agent refused or didn't answer the request
    pub refused: Option<&'a str>,
}

pub const HOLD_SECS: f32 = 2.0;

pub fn container(
    f: &Fonts,
    s: Option<&Service>,
    cpu: &[f32],
    mem: &[f32],
    loaded: bool,
    hold: Hold,
) -> Tree {
    let mut b = B::new(f);
    let Some(s) = s else {
        if !loaded {
            return reading(&mut b, "Container");
        }
        b.header("Container", "");
        let (c, r) = b.card("gone", Rect::new(M, TOP, WIDTH, 244.0), "", "");
        b.note(c, r, "This container no longer exists");
        return b.t;
    };
    b.header(&s.name, "");
    let pending = s.restart.as_ref().filter(|r| !r.done);
    let st = if pending.is_some() {
        Status::Attention
    } else {
        service_status(s)
    };
    let sc = status_colors(st);
    let bad = st != Status::Good;

    // state
    let r0 = Rect::new(M, TOP, 186.0, 120.0);
    let (c, r) = b.card("state", r0, "STATE", "");
    if bad {
        b.t.items[c].paint = Paint::Panel {
            fill: sc.card,
            radius: theme::RADIUS_TILE,
            border: Some(sc.border),
        };
    }
    b.value(
        c,
        "value",
        Rect::new(r.x, r.y, r.w, 32.0),
        &if pending.is_some() {
            "Restarting".into()
        } else {
            capitalized(&s.state)
        },
        "",
        if bad { sc.text } else { INK },
    );
    let line1 = match (pending, s.state.as_str(), s.up_sec, &s.ago) {
        (Some(p), ..) => format!("you asked {} ago", duration(p.ago_sec as i64 * 1000)),
        (_, "running" | "paused", Some(u), _) => format!("for {}", duration(u as i64 * 1000)),
        (_, _, _, Some(a)) => a.clone(),
        _ => String::new(),
    };
    let (line2, c2) = match (&s.health, s.exit_code) {
        _ if pending.is_some() => (String::new(), INK3),
        (Some(h), _) if s.state == "running" => {
            (h.clone(), if h == "unhealthy" { sc.text } else { INK3 })
        }
        (_, Some(code)) => (
            format!("exit code {code}"),
            if code == 0 { INK3 } else { sc.text },
        ),
        _ => (String::new(), INK3),
    };
    let mut y = r.y + 32.0 + MIN_GAP;
    for (name, text, color) in [("since", line1, INK2), ("health", line2, c2)] {
        if text.is_empty() {
            continue;
        }
        b.text(c, name, Rect::new(r.x, y, r.w, 18.0), &text, body(), color);
        y += 18.0 + MIN_GAP;
    }

    // right now
    let limit = s
        .mem_limit
        .filter(|_| s.state == "running")
        .map(|l| format!("limit {}", bytes_s(l)))
        .unwrap_or_default();
    let (c, r) = b.card(
        "usage",
        Rect::new(M + 186.0 + GAP, TOP, WIDTH - 186.0 - GAP, 120.0),
        "RIGHT NOW",
        &limit,
    );
    if s.state != "running" {
        b.text(
            c,
            "note",
            Rect::new(r.x, r.y, r.w, 18.0),
            "Not running",
            body(),
            INK3,
        );
        b.text(
            c,
            "note 2",
            Rect::new(r.x, r.y + 18.0 + MIN_GAP, r.w, 18.0),
            "no CPU or memory in use",
            body(),
            INK3,
        );
    } else {
        let label_w = 56.0;
        let vw = 72.0;
        let mut y = r.y;
        for (name, v, hist) in [
            ("CPU", s.cpu.map(|c| format!("{c:.1}%")), cpu),
            ("Memory", s.mem.map(bytes_s), mem),
        ] {
            let row =
                b.t.add(Some(c), name, Rect::new(r.x, y, r.w, 32.0), Paint::None);
            b.text(
                row,
                "name",
                Rect::new(r.x, y, label_w, 32.0),
                name,
                body(),
                INK3,
            );
            b.text_right(
                row,
                "value",
                Rect::new(r.x + label_w, y, vw, 32.0),
                v.as_deref().unwrap_or("—"),
                mono(),
                INK,
            );
            if hist.len() >= 2 {
                let sx = r.x + label_w + vw + GAP;
                b.t.add(
                    Some(row),
                    "spark",
                    Rect::new(sx, y + 4.0, r.right() - sx, 24.0),
                    Paint::Sparkline {
                        values: hist.to_vec(),
                        min_span: if name == "CPU" { 2.0 } else { 1.0 },
                        color: INK2,
                    },
                );
            }
            y += 32.0 + GAP;
        }
    }

    // container (or host service: no Restart, so the card takes the full width)
    let (c, r) = b.card(
        "container",
        Rect::new(
            M,
            TOP + 120.0 + GAP,
            if s.host { WIDTH } else { 304.0 },
            116.0,
        ),
        if s.host { "SERVICE" } else { "CONTAINER" },
        "",
    );
    let rows: [(&str, String); 3] = if s.host {
        let running = s.state == "running";
        [
            ("Unit", format!("{}.service", s.name)),
            (
                "Process",
                if s.image.is_empty() {
                    "none while stopped".into()
                } else {
                    s.image.clone()
                },
            ),
            (
                "Tasks",
                match s.tasks {
                    Some(n) if running => n.to_string(),
                    _ => "none".into(),
                },
            ),
        ]
    } else {
        let compose = match (&s.project, &s.service) {
            (Some(p), Some(sv)) => format!("{p} · {sv}"),
            (Some(p), None) => p.clone(),
            _ => "not started by Compose".into(),
        };
        let ports = if !s.ports.is_empty() {
            s.ports.join(", ")
        } else if s.state == "running" {
            "none".into()
        } else {
            "none while stopped".into()
        };
        [
            ("Image", s.image.clone()),
            ("Compose", compose),
            ("Ports", ports),
        ]
    };
    let mut y = r.y;
    for (name, v) in rows {
        let key_w = 72.0;
        let row =
            b.t.add(Some(c), "row", Rect::new(r.x, y, r.w, 18.0), Paint::None);
        b.text(
            row,
            "name",
            Rect::new(r.x, y, key_w, 18.0),
            name,
            body(),
            INK2,
        );
        b.text_right(
            row,
            "value",
            Rect::new(r.x + key_w + GAP, y, r.w - key_w - GAP, 18.0),
            &v,
            mono(),
            INK,
        );
        y += 18.0 + MIN_GAP;
    }

    if s.host {
        return b.t;
    }
    restart_button(
        &mut b,
        s,
        Rect::new(
            M + 304.0 + GAP,
            TOP + 120.0 + GAP,
            WIDTH - 304.0 - GAP,
            116.0,
        ),
        hold,
    );
    b.t
}

/// Restart (running) or Start (stopped): hold 2 s. Greyed while one runs and for a minute after.
fn restart_button(b: &mut B, s: &Service, r: Rect, hold: Hold) {
    let running = s.state == "running";
    let tone = if running {
        status_colors(Status::Attention).shape
    } else {
        status_colors(Status::Good).shape
    };
    let verb = if running { "Restart" } else { "Start" };
    let down_text = status_colors(Status::Down).text;
    let left = |ago: u64| crate::services::RESTART_EVERY.as_secs().saturating_sub(ago);
    let (enabled, label, sub, sub_color) = match (&s.restart, hold.progress, hold.refused) {
        (Some(x), ..) if !x.done => (false, "Please wait".into(), "up to 30 s".into(), INK3),
        (Some(x), ..) if x.error.is_none() => (
            false,
            "Restarted".to_string(),
            format!("again in {} s", left(x.ago_sec).max(1)),
            INK3,
        ),
        (_, Some(p), _) => (
            true,
            "Keep holding".into(),
            format!("{:.1} s of 2", (p * HOLD_SECS).min(HOLD_SECS)),
            INK3,
        ),
        (_, None, Some(why)) => (true, verb.into(), why.to_string(), down_text),
        (Some(x), None, None) => (
            true,
            verb.into(),
            format!("failed: {}", x.error.as_deref().unwrap_or("")),
            down_text,
        ),
        _ => (true, verb.into(), "hold 2 s".into(), INK3),
    };
    let bt = b.t.add(
        None,
        "restart",
        r,
        Paint::Panel {
            fill: theme::SURFACE,
            radius: theme::RADIUS_TILE,
            border: enabled.then_some(tone),
        },
    );
    if enabled {
        b.t.hold(bt, Nav::Restart(crate::services::key(&s.id)));
    }
    // the hold's bar is the layer the icon and text sit on
    let bt = b.t.add(
        Some(bt),
        "progress",
        r,
        Paint::Progress {
            frac: hold.progress.filter(|_| enabled).unwrap_or(0.0),
            color: tone,
            radius: theme::RADIUS_TILE,
        },
    );
    b.t.pad(bt, PAD_TILE);
    let inner = r.inset(PAD_TILE);
    b.t.add(
        Some(bt),
        "icon",
        Rect::new(inner.x + (inner.w - 28.0) / 2.0, inner.y + 6.0, 28.0, 28.0),
        Paint::RestartIcon(if enabled { tone } else { INK3 }),
    );
    let ls = Style::new(Face::SemiBold, size::TITLE.0);
    let label_w = b.f.width(&ls, &label).min(inner.w);
    b.text(
        bt,
        "label",
        Rect::new(
            inner.x + (inner.w - label_w) / 2.0,
            inner.y + 42.0,
            label_w,
            22.0,
        ),
        &label,
        ls,
        if enabled { INK } else { INK3 },
    );
    let sub_w = b.f.width(&small(), &sub).min(inner.w);
    b.text(
        bt,
        "sub",
        Rect::new(
            inner.x + (inner.w - sub_w) / 2.0,
            inner.y + 68.0,
            sub_w,
            16.0,
        ),
        &sub,
        small(),
        sub_color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(bytes(0), ("0".into(), " B".into()));
        assert_eq!(bytes(1300 * 1024 * 1024), ("1.3".into(), " GB".into()));
        assert_eq!(bytes(545 * 1024 * 1024), ("545".into(), " MB".into()));
        assert_eq!(rate(200704.0), "196 KB/s");
        assert_eq!(duration(45_000), "45 s");
        assert_eq!(duration(3 * 60_000), "3 m");
        assert_eq!(duration((2 * 3600 + 5 * 60) * 1000), "2 h 05 m");
        assert_eq!(duration((3 * 86400 + 4 * 3600) * 1000), "3 d 4 h");
    }
}
