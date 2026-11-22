//! Home: one look tells you whether the server is fine.
//!
//! ┌ 12 ───────────────────────────────────────────────────────────── 12 ┐
//! │ header 44:  hostname · clock · date                     [History]   │  y 12–56
//! │ 8                                                                    │
//! │ hero card 136 (tap → Services): status · headline · 2 lines │ groups │  y 64–200
//! │ 8                                                                    │
//! │ vitals 100: CPU · TEMP · MEMORY · DISK tiles, 8 apart                │  y 208–308
//! └ 12 ──────────────────────────────────────────────────────────────────┘

use super::canvas::{Face, Fonts, Style};
use super::layout::{Marker, Nav, Paint, Rect, Tree};
use super::theme::{
    self, GAP, INK, INK2, INK3, MIN_GAP, MUTED, PAD_CARD, PAD_TILE, Status, size, status_colors,
};
use crate::rules::THRESHOLDS;
use crate::types::Snapshot;

/// An open incident, as the UI needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenIncident {
    pub kind: String,
    pub title: String,
}

/// Everything Home shows, read from the agent.
#[derive(Debug, Clone, Default)]
pub struct Data {
    pub snapshot: Option<Snapshot>,
    pub open: Vec<OpenIncident>,
    /// last 10 minutes, one point a minute
    pub cpu_trend: Vec<f32>,
    pub temp_trend: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub name: &'static str,
    /// None = not set up yet
    pub value: Option<String>,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Detail {
    Spark { values: Vec<f32>, min_span: f32 },
    Bar(f32),
    Empty,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub label: &'static str,
    pub value: String,
    pub unit: String,
    /// Good = normal; Attention / Down = over warn / critical
    pub level: Status,
    pub detail: Detail,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Home {
    pub hostname: String,
    pub time: String,
    pub date: String,
    pub status: Status,
    pub headline: String,
    pub line2: String,
    pub line3: String,
    pub groups: [Group; 3],
    pub tiles: [Tile; 4],
}

fn level(metric: &str, v: f64) -> Status {
    match THRESHOLDS.iter().find(|t| t.metric == metric) {
        Some(t) if v >= t.critical => Status::Down,
        Some(t) if v >= t.warn => Status::Attention,
        _ => Status::Good,
    }
}

pub fn uptime(sec: f64) -> String {
    let m = (sec / 60.0) as u64;
    let (d, h, m) = (m / 1440, m / 60 % 24, m % 60);
    if d > 0 {
        format!("Up {d} d {h} h")
    } else if h > 0 {
        format!("Up {h} h {m:02} m")
    } else {
        format!("Up {m} m")
    }
}

fn gigabytes(bytes: u64) -> String {
    let gb = bytes as f64 / (1u64 << 30) as f64;
    if gb < 10.0 {
        format!("{gb:.1}")
    } else {
        format!("{gb:.0}")
    }
}

impl Home {
    pub fn build(d: &Data, time: &str, date: &str) -> Home {
        let Some(s) = &d.snapshot else {
            return Home::waiting(time, date);
        };

        // problems, worst first
        let mut problems: Vec<(Status, String)> = d
            .open
            .iter()
            .map(|i| {
                let st = if i.kind == "metric" {
                    Status::Attention
                } else {
                    Status::Down
                };
                (st, i.title.clone())
            })
            .collect();
        let apps = match &s.docker {
            None => {
                if !problems.iter().any(|(_, t)| t == "Docker unreachable") {
                    problems.push((Status::Down, "Docker unreachable".into()));
                }
                Group {
                    name: "Apps",
                    value: Some("—".into()),
                    status: Status::Down,
                }
            }
            Some(dk) => {
                let stopped = dk.total - dk.running;
                let any_down = d.open.iter().any(|i| i.kind == "container");
                if stopped > 0 && !any_down {
                    let noun = if stopped == 1 {
                        "container isn’t"
                    } else {
                        "containers aren’t"
                    };
                    problems.push((Status::Attention, format!("{stopped} {noun} running")));
                }
                let st = if any_down {
                    Status::Down
                } else if stopped > 0 {
                    Status::Attention
                } else {
                    Status::Good
                };
                Group {
                    name: "Apps",
                    value: Some(format!("{}/{}", dk.running, dk.total)),
                    status: st,
                }
            }
        };
        problems.sort_by_key(|p| std::cmp::Reverse(p.0));
        let status = problems.first().map(|p| p.0).unwrap_or(Status::Good);

        let up = format!("{} · load {:.2}", uptime(s.host.uptime_sec), s.host.load[0]);
        let (headline, line2, line3) = match status {
            Status::Good => {
                let (ok, total) = s
                    .docker
                    .as_ref()
                    .map(|d| (d.running, d.total))
                    .unwrap_or((0, 0));
                let checks = if total == 0 {
                    "No checks set up yet".to_string()
                } else {
                    format!("{ok} of {total} checks passing")
                };
                ("All good".to_string(), checks, up)
            }
            _ => {
                let n = problems.iter().filter(|p| p.0 == status).count();
                let head = match (status, n) {
                    (Status::Down, 1) => "1 is down".to_string(),
                    (Status::Down, n) => format!("{n} are down"),
                    (_, 1) => "1 needs attention".to_string(),
                    (_, n) => format!("{n} need attention"),
                };
                let line3 = match problems.len() {
                    1 => up,
                    2 => format!("and {}", problems[1].1),
                    k => format!("and {} more", k - 1),
                };
                (head, problems[0].1.clone(), line3)
            }
        };

        let not_set_up = |name| Group {
            name,
            value: None,
            status: Status::Unknown,
        };
        let temp = s.cpu.temp_c;
        let tiles = [
            Tile {
                label: "CPU",
                value: format!("{:.0}", s.cpu.percent),
                unit: "%".into(),
                level: level("cpu.percent", s.cpu.percent),
                detail: Detail::Spark {
                    values: d.cpu_trend.clone(),
                    min_span: 10.0,
                },
            },
            Tile {
                label: "TEMP",
                value: temp
                    .map(|t| format!("{t:.0}"))
                    .unwrap_or_else(|| "—".into()),
                unit: if temp.is_some() {
                    "°C".into()
                } else {
                    String::new()
                },
                level: temp.map(|t| level("cpu.tempC", t)).unwrap_or(Status::Good),
                detail: Detail::Spark {
                    values: d.temp_trend.clone(),
                    min_span: 4.0,
                },
            },
            Tile {
                label: "MEMORY",
                value: gigabytes(s.memory.used_bytes),
                unit: " GB".into(),
                level: level("mem.usedPercent", s.memory.used_percent),
                detail: Detail::Bar((s.memory.used_percent / 100.0) as f32),
            },
            Tile {
                label: "DISK",
                value: format!("{:.0}", s.disk.used_percent),
                unit: "%".into(),
                level: level("disk.usedPercent", s.disk.used_percent),
                detail: Detail::Bar((s.disk.used_percent / 100.0) as f32),
            },
        ];
        Home {
            hostname: s.host.hostname.clone(),
            time: time.into(),
            date: date.into(),
            status,
            headline,
            line2,
            line3,
            groups: [apps, not_set_up("Host"), not_set_up("Web")],
            tiles,
        }
    }

    fn waiting(time: &str, date: &str) -> Home {
        let empty = |label| Tile {
            label,
            value: "—".into(),
            unit: String::new(),
            level: Status::Good,
            detail: Detail::Empty,
        };
        let unknown = |name| Group {
            name,
            value: None,
            status: Status::Unknown,
        };
        Home {
            hostname: "PiOS".into(),
            time: time.into(),
            date: date.into(),
            status: Status::Unknown,
            headline: "Waiting for data".into(),
            line2: "The PiOS agent isn’t answering".into(),
            line3: "Start it with: pios agent".into(),
            groups: [unknown("Apps"), unknown("Host"), unknown("Web")],
            tiles: [empty("CPU"), empty("TEMP"), empty("MEMORY"), empty("DISK")],
        }
    }

    /// Places every element. `layout::check` must find nothing for any data.
    pub fn tree(&self, f: &Fonts) -> Tree {
        let mut t = Tree::default();
        let m = theme::SCREEN_MARGIN;
        let width = theme::W - 2.0 * m;

        // ── header ─────────────────────────────────────────────
        let header = Rect::new(m, m, width, 44.0);
        let hdr = t.add(None, "header", header, Paint::None);

        let btn_style = Style::new(Face::SemiBold, size::BUTTON.0);
        let btn_lh = size::BUTTON.1;
        let btn_text_w = f.width(&btn_style, "History");
        let btn_w = PAD_TILE + 16.0 + GAP + btn_text_w + PAD_TILE;
        let btn = Rect::new(header.right() - btn_w, header.y, btn_w, 44.0);
        let b = t.add(
            Some(hdr),
            "history button",
            btn,
            Paint::Panel {
                fill: theme::SURFACE,
                radius: theme::RADIUS_TILE,
                border: None,
            },
        );
        t.nav(b, Nav::History);
        t.pad(b, PAD_TILE);
        t.add(
            Some(b),
            "history icon",
            Rect::new(btn.x + PAD_TILE, btn.cy() - 8.0, 16.0, 16.0),
            Paint::ClockIcon(INK2),
        );
        let bt = t.add(
            Some(b),
            "history label",
            Rect::new(
                btn.x + PAD_TILE + 16.0 + GAP,
                btn.cy() - btn_lh / 2.0,
                btn_text_w,
                btn_lh,
            ),
            Paint::Text {
                text: "History".into(),
                style: btn_style,
                color: INK,
            },
        );
        t.content(bt, btn_text_w, btn_lh);

        // hostname · time · date, on one baseline; the date goes first, then the hostname shrinks
        let title = Style::new(Face::SemiBold, size::TITLE.0);
        let small = Style::new(Face::Regular, size::BODY.0);
        let lh = size::TITLE.1;
        let room = btn.x - GAP - header.x;
        let time_w = f.width(&title, &self.time);
        let date_w = f.width(&small, &self.date);
        let host_full = f.width(&title, &self.hostname);
        let show_date = host_full + GAP + time_w + GAP + date_w <= room;
        let host_room = room - GAP - time_w - if show_date { GAP + date_w } else { 0.0 };
        let host = f.ellipsize(&title, &self.hostname, host_room);
        let host_w = f.width(&title, &host);
        let top = header.cy() - lh / 2.0;
        let baseline = f.baseline(&title, top, lh);
        let mut x = header.x;
        for (name, text, style, w, color) in [
            ("hostname", host, title, host_w, INK),
            ("clock", self.time.clone(), title, time_w, INK),
            (
                "date",
                if show_date {
                    self.date.clone()
                } else {
                    String::new()
                },
                small,
                date_w,
                INK3,
            ),
        ] {
            if text.is_empty() {
                continue;
            }
            // line box placed so this face sits on the shared baseline
            let y = baseline - (f.baseline(&style, 0.0, lh));
            let i = t.add(
                Some(hdr),
                name,
                Rect::new(x, y, w, lh),
                Paint::Text { text, style, color },
            );
            t.content(i, w, lh);
            x += w + GAP;
        }

        // ── hero card ──────────────────────────────────────────
        let sc = status_colors(self.status);
        let hero = Rect::new(m, header.bottom() + GAP, width, 136.0);
        let h = t.add(
            None,
            "hero card",
            hero,
            Paint::Panel {
                fill: sc.card,
                radius: theme::RADIUS_CARD,
                border: Some(sc.border),
            },
        );
        t.touch(h);
        t.pad(h, PAD_CARD);
        let inner = hero.inset(PAD_CARD);
        let icon = Rect::new(inner.x, inner.cy() - 30.0, 60.0, 60.0);
        t.add(Some(h), "status icon", icon, Paint::StatusIcon(self.status));

        // groups column, sized to its widest row (at least 104 px like the mock)
        let row_label = Style::new(Face::Regular, size::BODY.0);
        let row_value = Style::new(Face::Mono, size::BODY.0);
        let values: Vec<String> = self
            .groups
            .iter()
            .map(|g| g.value.clone().unwrap_or_else(|| "—".into()))
            .collect();
        let label_w = self
            .groups
            .iter()
            .map(|g| f.width(&row_label, g.name))
            .fold(0.0, f32::max);
        let value_w = values
            .iter()
            .map(|v| f.width(&row_value, v))
            .fold(0.0, f32::max);
        const MARK: f32 = 10.0;
        let col_inset = 13.0; // 1 px separator + 13 = the mock's 14 px
        let rows_w = MARK + GAP + label_w + GAP + value_w;
        let col_w = (1.0 + col_inset + rows_w).max(104.0);
        let sep = Rect::new(inner.right() - col_w, inner.y, 1.0, inner.h);
        t.add(Some(h), "separator", sep, Paint::Fill(sc.separator));
        let rows_x = sep.right() + col_inset;
        let rows = Rect::new(rows_x, inner.y, inner.right() - rows_x, inner.h);
        let col = t.add(Some(h), "groups", rows, Paint::None);
        let (row_h, row_gap) = (18.0, 10.0);
        let mut ry = rows.cy() - (3.0 * row_h + 2.0 * row_gap) / 2.0;
        for (g, v) in self.groups.iter().zip(&values) {
            let r = t.add(
                Some(col),
                g.name,
                Rect::new(rows.x, ry, rows.w, row_h),
                Paint::None,
            );
            let gc = status_colors(g.status);
            let mk = match g.status {
                Status::Good => Marker::Dot(gc.shape),
                Status::Attention => Marker::Triangle(gc.shape),
                Status::Down => Marker::Square(gc.shape),
                Status::Unknown => Marker::Ring(MUTED),
            };
            t.add(
                Some(r),
                "marker",
                Rect::new(rows.x, ry + (row_h - MARK) / 2.0, MARK, MARK),
                Paint::Marker(mk),
            );
            let lw = f.width(&row_label, g.name);
            let set_up = g.value.is_some();
            let l = t.add(
                Some(r),
                "label",
                Rect::new(rows.x + MARK + GAP, ry, lw, row_h),
                Paint::Text {
                    text: g.name.into(),
                    style: row_label,
                    color: if set_up { INK2 } else { INK3 },
                },
            );
            t.content(l, lw, row_h);
            let vw = f.width(&row_value, v);
            let vcol = match g.status {
                Status::Good => INK,
                Status::Unknown => INK3,
                s => status_colors(s).text,
            };
            let vi = t.add(
                Some(r),
                "value",
                Rect::new(rows.right() - vw, ry, vw, row_h),
                Paint::Text {
                    text: v.clone(),
                    style: row_value,
                    color: vcol,
                },
            );
            t.content(vi, vw, row_h);
            ry += row_h + row_gap;
        }

        // text column between the icon and the groups, 16 px from each
        let tx = icon.right() + PAD_CARD;
        let tw = sep.x - PAD_CARD - tx;
        let (hs, hlh) = if self.status == Status::Good || self.status == Status::Unknown {
            size::HEADLINE
        } else {
            size::HEADLINE_ALERT
        };
        let head_base = Style::new(Face::SemiBold, hs).spacing(-0.02 * hs);
        let head = f
            .fit(&head_base, &self.headline, tw, 24.0)
            .unwrap_or(head_base.sized(24.0).spacing(-0.48));
        let head_text = f.ellipsize(&head, &self.headline, tw);
        let l2 = Style::new(Face::Regular, size::TITLE.0);
        let l3 = Style::new(Face::Regular, size::BODY.0);
        let block_h = hlh + MIN_GAP + size::TITLE.1 + MIN_GAP + size::BODY.1;
        let mut y = inner.cy() - block_h / 2.0;
        let problem = matches!(self.status, Status::Attention | Status::Down);
        let lines = [
            (
                "headline",
                head_text,
                head,
                hlh,
                if self.status == Status::Good {
                    INK
                } else {
                    sc.text
                },
            ),
            (
                "line 2",
                f.ellipsize(&l2, &self.line2, tw),
                l2,
                size::TITLE.1,
                if problem { INK } else { INK2 },
            ),
            (
                "line 3",
                f.ellipsize(&l3, &self.line3, tw),
                l3,
                size::BODY.1,
                if problem { INK2 } else { INK3 },
            ),
        ];
        for (name, text, style, lh, color) in lines {
            let w = f.width(&style, &text);
            let i = t.add(
                Some(h),
                name,
                Rect::new(tx, y, tw, lh),
                Paint::Text { text, style, color },
            );
            t.content(i, w, lh);
            y += lh + MIN_GAP;
        }

        // ── vitals ─────────────────────────────────────────────
        let vy = hero.bottom() + GAP;
        let tile_w = (width - 3.0 * GAP) / 4.0;
        let label_style = Style::new(Face::SemiBold, size::LABEL.0).spacing(0.06 * size::LABEL.0);
        let value_base = Style::new(Face::SemiBold, size::VALUE.0).spacing(-0.02 * size::VALUE.0);
        let unit_style = Style::new(Face::Regular, size::UNIT);
        for (i, tile) in self.tiles.iter().enumerate() {
            let r = Rect::new(m + i as f32 * (tile_w + GAP), vy, tile_w, 100.0);
            let ti = t.add(
                None,
                tile.label,
                r,
                Paint::Panel {
                    fill: theme::SURFACE,
                    radius: theme::RADIUS_TILE,
                    border: None,
                },
            );
            match tile.label {
                "CPU" => t.nav(ti, Nav::Cpu),
                "TEMP" => t.nav(ti, Nav::Temps),
                "MEMORY" => t.nav(ti, Nav::Memory),
                "DISK" => t.nav(ti, Nav::Disk),
                _ => t.touch(ti),
            };
            t.pad(ti, PAD_TILE);
            let inn = r.inset(PAD_TILE);

            let lw = f.width(&label_style, tile.label);
            let li = t.add(
                Some(ti),
                "label",
                Rect::new(inn.x, inn.y, inn.w, size::LABEL.1),
                Paint::Text {
                    text: tile.label.into(),
                    style: label_style,
                    color: INK3,
                },
            );
            t.content(li, lw, size::LABEL.1);

            // value + unit, shrunk (28 → 20 px) until both fit the tile
            let unit_w = if tile.unit.is_empty() {
                0.0
            } else {
                f.width(&unit_style, &tile.unit) + super::layout::VALUE_UNIT_GAP
            };
            let vs = f
                .fit(&value_base, &tile.value, inn.w - unit_w, size::MIN_VALUE)
                .unwrap_or(
                    value_base
                        .sized(size::MIN_VALUE)
                        .spacing(-0.02 * size::MIN_VALUE),
                );
            let vw = f.width(&vs, &tile.value) + unit_w;
            let vcol = if tile.level == Status::Good {
                INK
            } else {
                status_colors(tile.level).text
            };
            let vtop = inn.y + size::LABEL.1 + MIN_GAP;
            let vi = t.add(
                Some(ti),
                "value",
                Rect::new(inn.x, vtop, inn.w, size::VALUE.1),
                Paint::Value {
                    value: tile.value.clone(),
                    style: vs,
                    color: vcol,
                    unit: tile.unit.clone(),
                    unit_style,
                    unit_color: INK2,
                },
            );
            t.content(vi, vw, size::VALUE.1);

            let dr = Rect::new(
                inn.x,
                vtop + size::VALUE.1 + MIN_GAP,
                inn.w,
                inn.bottom() - (vtop + size::VALUE.1 + MIN_GAP),
            );
            let fill = if tile.level == Status::Good {
                INK2
            } else {
                status_colors(tile.level).shape
            };
            match &tile.detail {
                Detail::Spark { values, min_span } => {
                    t.add(
                        Some(ti),
                        "trend",
                        dr,
                        Paint::Sparkline {
                            values: values.clone(),
                            min_span: *min_span,
                            color: INK2,
                        },
                    );
                }
                Detail::Bar(pct) => {
                    t.add(
                        Some(ti),
                        "bar",
                        Rect::new(dr.x, dr.cy() - 3.0, dr.w, 6.0),
                        Paint::Bar {
                            pct: *pct,
                            track: theme::TRACK,
                            fill,
                        },
                    );
                }
                Detail::Empty => {}
            }
        }
        t
    }
}
