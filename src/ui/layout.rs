//! A screen is a tree of boxes. Layout code places every element as a box first; `check` proves
//! the spacing rules on it; `render` then paints it. Nothing is drawn that wasn't placed.

use super::canvas::{Canvas, Style, polyline, rounded};
use super::theme::{self, Rgb, Status, status_colors};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
    pub fn inset(&self, p: f32) -> Rect {
        Rect::new(self.x + p, self.y + p, self.w - 2.0 * p, self.h - 2.0 * p)
    }
    pub fn contains(&self, o: &Rect) -> bool {
        const E: f32 = 0.01;
        o.x >= self.x - E
            && o.y >= self.y - E
            && o.right() <= self.right() + E
            && o.bottom() <= self.bottom() + E
    }
    /// Distance between two boxes; negative when they overlap.
    pub fn gap(&self, o: &Rect) -> f32 {
        let sx = (o.x - self.right()).max(self.x - o.right());
        let sy = (o.y - self.bottom()).max(self.y - o.bottom());
        sx.max(sy)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Marker {
    Dot(Rgb),
    Triangle(Rgb),
    Square(Rgb),
    Ring(Rgb),
}

/// Where a tap on a target goes. `Back` is resolved by the app from the current screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Home,
    Cpu,
    Temps,
    Memory,
    Disk,
    History,
    Services,
    /// a page of the open paged screen (History, Services)
    Page(usize),
    Incident(i64),
    /// a container, by `services::key` of its ID
    Container(u64),
    /// restart that container (a hold target: fires after a 2 s hold, never on a tap)
    Restart(u64),
    Back,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    None,
    Panel {
        fill: Rgb,
        radius: f32,
        border: Option<Rgb>,
    },
    Text {
        text: String,
        style: Style,
        color: Rgb,
    },
    /// a big number and its unit on one baseline, e.g. "37" + "°C"
    Value {
        value: String,
        style: Style,
        color: Rgb,
        unit: String,
        unit_style: Style,
        unit_color: Rgb,
    },
    StatusIcon(Status),
    Marker(Marker),
    ClockIcon(Rgb),
    Fill(Rgb),
    Sparkline {
        values: Vec<f32>,
        min_span: f32,
        color: Rgb,
    },
    Bar {
        pct: f32,
        track: Rgb,
        fill: Rgb,
    },
    /// a pill split into coloured parts (shares of the whole, left to right)
    Segments {
        parts: Vec<(f32, Rgb)>,
        track: Rgb,
    },
    /// a 6 px bar centred in the box, with 2 px marks the full box height (limits)
    Gauge {
        pct: f32,
        track: Rgb,
        fill: Rgb,
        marks: Vec<(f32, Rgb)>,
    },
    /// `n` pills across the box, the first `on` lit (fan speed steps)
    Steps {
        n: u32,
        on: u32,
        on_color: Rgb,
        off_color: Rgb,
    },
    /// "‹" (or "›" when the flag is true) for back and paging buttons
    Chevron(Rgb, bool),
    /// a circular arrow (Restart), drawn to fit the box
    RestartIcon(Rgb),
    /// a hold's progress: the left `frac` of the box, tinted, clipped to its rounded corners
    Progress {
        frac: f32,
        color: Rgb,
        radius: f32,
    },
    /// area chart: filled under the line, optional dashed guide at a value
    Area {
        values: Vec<f32>,
        min_span: f32,
        color: Rgb,
        guide: Option<f32>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub name: String,
    pub rect: Rect,
    pub parent: Option<usize>,
    /// children must stay this far inside
    pub pad: f32,
    pub touch: bool,
    /// size the content needs (text width, line box); must fit in `rect`
    pub content: Option<(f32, f32)>,
    pub paint: Paint,
    /// where a tap goes (touch targets only)
    pub nav: Option<Nav>,
    /// fires only after a 2 s hold (panel.rs), not on a tap
    pub hold: bool,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Tree {
    pub items: Vec<Item>,
}

impl Tree {
    pub fn add(
        &mut self,
        parent: Option<usize>,
        name: impl Into<String>,
        rect: Rect,
        paint: Paint,
    ) -> usize {
        self.items.push(Item {
            name: name.into(),
            rect,
            parent,
            pad: 0.0,
            touch: false,
            content: None,
            paint,
            nav: None,
            hold: false,
        });
        self.items.len() - 1
    }
    /// Makes `i` a hold target: `nav` fires after a 2 s hold.
    pub fn hold(&mut self, i: usize, nav: Nav) -> usize {
        self.nav(i, nav);
        self.items[i].hold = true;
        i
    }
    /// Makes `i` a touch target that goes to `nav`.
    pub fn nav(&mut self, i: usize, nav: Nav) -> usize {
        self.items[i].touch = true;
        self.items[i].nav = Some(nav);
        i
    }
    pub fn pad(&mut self, i: usize, pad: f32) -> usize {
        self.items[i].pad = pad;
        i
    }
    pub fn touch(&mut self, i: usize) -> usize {
        self.items[i].touch = true;
        i
    }
    pub fn content(&mut self, i: usize, w: f32, h: f32) -> usize {
        self.items[i].content = Some((w, h));
        i
    }
    fn path(&self, mut i: usize) -> String {
        let mut parts = vec![self.items[i].name.clone()];
        while let Some(p) = self.items[i].parent {
            parts.push(self.items[p].name.clone());
            i = p;
        }
        parts.reverse();
        parts.join(" › ")
    }
    fn is_ancestor(&self, a: usize, mut b: usize) -> bool {
        while let Some(p) = self.items[b].parent {
            if p == a {
                return true;
            }
            b = p;
        }
        false
    }
}

/// All spacing-rule violations, as readable lines. Empty = the screen is clean.
pub fn check(t: &Tree) -> Vec<String> {
    let mut out = Vec::new();
    let screen = Rect::new(0.0, 0.0, theme::W, theme::H).inset(theme::SCREEN_MARGIN);
    for (i, it) in t.items.iter().enumerate() {
        let (bounds, what) = match it.parent {
            Some(p) => (
                t.items[p].rect.inset(t.items[p].pad),
                "its parent's padding",
            ),
            None => (screen, "the 12 px screen margin"),
        };
        if !bounds.contains(&it.rect) {
            out.push(format!(
                "{} goes outside {what}: {:?} not in {:?}",
                t.path(i),
                it.rect,
                bounds
            ));
        }
        if let Some((w, h)) = it.content
            && (w > it.rect.w + 0.5 || h > it.rect.h + 0.5)
        {
            out.push(format!(
                "{}: content {w:.1}×{h:.1} doesn't fit {:.1}×{:.1}",
                t.path(i),
                it.rect.w,
                it.rect.h
            ));
        }
        if it.touch && (it.rect.w < theme::TOUCH_MIN || it.rect.h < theme::TOUCH_MIN) {
            out.push(format!(
                "{}: touch target {:.0}×{:.0} is under 44×44",
                t.path(i),
                it.rect.w,
                it.rect.h
            ));
        }
    }
    for a in 0..t.items.len() {
        for b in a + 1..t.items.len() {
            let (ia, ib) = (&t.items[a], &t.items[b]);
            let gap = ia.rect.gap(&ib.rect);
            if ia.parent == ib.parent {
                let need = if ia.touch && ib.touch {
                    theme::GAP
                } else {
                    theme::MIN_GAP
                };
                if gap < need - 0.01 {
                    out.push(format!(
                        "{} and {} are {gap:.1} px apart (need {need})",
                        t.path(a),
                        t.path(b)
                    ));
                }
            } else if ia.touch
                && ib.touch
                && !t.is_ancestor(a, b)
                && !t.is_ancestor(b, a)
                && gap < theme::GAP - 0.01
            {
                out.push(format!(
                    "touch targets {} and {} are {gap:.1} px apart (need 8)",
                    t.path(a),
                    t.path(b)
                ));
            }
        }
    }
    out
}

/// Paints every item, parents before children.
pub fn render(t: &Tree, c: &mut Canvas) {
    for it in &t.items {
        let r = it.rect;
        match &it.paint {
            Paint::None => {}
            Paint::Panel {
                fill,
                radius,
                border,
            } => {
                c.fill(&rounded(r, *radius), *fill);
                if let Some(b) = border {
                    let inner = Rect::new(r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0);
                    c.stroke(&rounded(inner, radius - 0.5), *b, 1.0);
                }
            }
            Paint::Text { text, style, color } => {
                c.text(style, text, r.x, r.y, r.h, *color);
            }
            Paint::Value {
                value,
                style,
                color,
                unit,
                unit_style,
                unit_color,
            } => {
                let base = c.fonts.baseline(style, r.y, r.h);
                let end = c.text_at_baseline(style, value, r.x, base, *color);
                c.text_at_baseline(unit_style, unit, end + VALUE_UNIT_GAP, base, *unit_color);
            }
            Paint::StatusIcon(s) => status_icon(c, r, *s),
            Paint::Marker(m) => marker(c, r, *m),
            Paint::ClockIcon(col) => {
                let (x, y) = (r.x, r.y);
                let ring = c.circle(x + 8.0, y + 8.0, 6.5);
                c.stroke(&ring, *col, 1.8);
                if let Some(p) =
                    polyline(&[(x + 8.0, y + 4.5), (x + 8.0, y + 8.0), (x + 10.5, y + 9.5)])
                {
                    c.stroke(&p, *col, 1.8);
                }
            }
            Paint::Fill(col) => c.fill_rect(r, *col),
            Paint::Sparkline {
                values,
                min_span,
                color,
            } => sparkline(c, r, values, *min_span, *color),
            Paint::Bar { pct, track, fill } => c.bar(r, *pct, *track, *fill),
            Paint::Segments { parts, track } => {
                c.bar(r, 0.0, *track, *track);
                let mut x = r.x;
                let total: f32 = parts.iter().map(|p| p.0.max(0.0)).sum::<f32>().max(1.0);
                for (share, col) in parts {
                    let w = (r.w * share.max(0.0) / total).round();
                    if w > 0.0 {
                        c.fill_clipped(Rect::new(x, r.y, w, r.h), r, *col);
                    }
                    x += w;
                }
            }
            Paint::Gauge {
                pct,
                track,
                fill,
                marks,
            } => {
                let bar = Rect::new(r.x, r.cy() - 3.0, r.w, 6.0);
                c.bar(bar, *pct, *track, *fill);
                for (at, col) in marks {
                    let x = (r.x + r.w * at.clamp(0.0, 1.0) - 1.0).clamp(r.x, r.right() - 2.0);
                    c.fill(&rounded(Rect::new(x, r.y, 2.0, r.h), 1.0), *col);
                }
            }
            Paint::Steps {
                n,
                on,
                on_color,
                off_color,
            } => {
                let n = (*n).max(1);
                let gap = 4.0;
                let w = (r.w - gap * (n - 1) as f32) / n as f32;
                for i in 0..n {
                    let col = if i < *on { *on_color } else { *off_color };
                    let x = r.x + i as f32 * (w + gap);
                    c.fill(&rounded(Rect::new(x, r.y, w, r.h), r.h / 2.0), col);
                }
            }
            Paint::Chevron(col, right) => {
                let (cx, cy) = (r.x + r.w / 2.0, r.cy());
                let d = if *right { -1.0 } else { 1.0 };
                if let Some(p) = polyline(&[
                    (cx + 2.5 * d, cy - 6.0),
                    (cx - 3.5 * d, cy),
                    (cx + 2.5 * d, cy + 6.0),
                ]) {
                    c.stroke(&p, *col, 2.2);
                }
            }
            Paint::Area {
                values,
                min_span,
                color,
                guide,
            } => area(c, r, values, *min_span, *color, *guide),
            Paint::RestartIcon(col) => {
                let (cx, cy, rad) = (r.x + r.w / 2.0, r.cy(), r.w.min(r.h) / 2.0 - 2.0);
                // an open circle, gap at the top, turning clockwise; arrow at the end
                let pts: Vec<(f32, f32)> = (0..=28)
                    .map(|i| {
                        let a = (-60.0 + i as f32 * 10.0_f32).to_radians();
                        (cx + rad * a.cos(), cy + rad * a.sin())
                    })
                    .collect();
                if let Some(p) = polyline(&pts) {
                    c.stroke(&p, *col, 2.2);
                }
                // arrowhead at the end, pointing the way the arc turns (clockwise)
                let end = 220.0_f32.to_radians();
                let (ex, ey) = (cx + rad * end.cos(), cy + rad * end.sin());
                let (tx, ty) = (-end.sin(), end.cos());
                let (nx, ny) = (-ty, tx);
                let tip = (ex + tx * 2.0, ey + ty * 2.0);
                let back = (tip.0 - tx * 6.0, tip.1 - ty * 6.0);
                if let Some(p) = polyline(&[
                    (back.0 + nx * 4.5, back.1 + ny * 4.5),
                    tip,
                    (back.0 - nx * 4.5, back.1 - ny * 4.5),
                ]) {
                    c.stroke(&p, *col, 2.2);
                }
            }
            Paint::Progress {
                frac,
                color,
                radius,
            } => {
                let w = (r.w * frac.clamp(0.0, 1.0)).round();
                if w > 0.0 {
                    c.fill_alpha_clipped(Rect::new(r.x, r.y, w, r.h), r, *radius, *color, 72);
                }
            }
        }
    }
}

/// Gap between a big value and its unit ("37" "°C"), part of one element. Tabular digits
/// already leave a little room; units that read as words carry their own space (" GB").
pub const VALUE_UNIT_GAP: f32 = 0.0;

fn status_icon(c: &mut Canvas, r: Rect, s: Status) {
    let col = status_colors(s).shape;
    let (x, y) = (r.x, r.y);
    match s {
        Status::Good => {
            let ring = c.circle(x + 30.0, y + 30.0, 27.0);
            c.stroke(&ring, col, 3.0);
            if let Some(p) = polyline(&[
                (x + 18.0, y + 31.0),
                (x + 26.0, y + 39.0),
                (x + 42.0, y + 22.0),
            ]) {
                c.stroke(&p, col, 3.5);
            }
        }
        Status::Attention => {
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(x + 30.0, y + 6.0);
            pb.line_to(x + 56.0, y + 52.0);
            pb.line_to(x + 4.0, y + 52.0);
            pb.close();
            if let Some(p) = pb.finish() {
                c.stroke(&p, col, 3.0);
            }
            if let Some(p) = polyline(&[(x + 30.0, y + 22.0), (x + 30.0, y + 36.0)]) {
                c.stroke(&p, col, 3.5);
            }
            let dot = c.circle(x + 30.0, y + 44.0, 2.4);
            c.fill(&dot, col);
        }
        Status::Down => {
            c.stroke(
                &rounded(Rect::new(x + 6.0, y + 6.0, 48.0, 48.0), 6.0),
                col,
                3.0,
            );
            if let Some(p) = polyline(&[(x + 20.0, y + 30.0), (x + 40.0, y + 30.0)]) {
                c.stroke(&p, col, 3.5);
            }
        }
        Status::Unknown => {
            let ring = c.circle(x + 30.0, y + 30.0, 27.0);
            c.stroke(&ring, col, 3.0);
            for dx in [-9.0, 0.0, 9.0] {
                let dot = c.circle(x + 30.0 + dx, y + 30.0, 2.4);
                c.fill(&dot, col);
            }
        }
    }
}

fn marker(c: &mut Canvas, r: Rect, m: Marker) {
    let (cx, cy) = (r.x + r.w / 2.0, r.cy());
    match m {
        Marker::Dot(col) => {
            let p = c.circle(cx, cy, 4.0);
            c.fill(&p, col);
        }
        Marker::Ring(col) => {
            let p = c.circle(cx, cy, 3.5);
            c.stroke(&p, col, 1.5);
        }
        Marker::Triangle(col) => {
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(cx, cy - 4.5);
            pb.line_to(cx + 5.0, cy + 4.5);
            pb.line_to(cx - 5.0, cy + 4.5);
            pb.close();
            if let Some(p) = pb.finish() {
                c.fill(&p, col);
            }
        }
        Marker::Square(col) => c.fill(&rounded(Rect::new(cx - 4.0, cy - 4.0, 8.0, 8.0), 1.5), col),
    }
}

/// Values scaled to the box, with at least `min_span` of range so a flat line stays flat.
/// Scales values into a box (lo at the bottom); keeps at least `min_span` of range.
fn scale(values: &[f32], extra: Option<f32>, min_span: f32) -> (f32, f32) {
    let mut lo = values.iter().copied().fold(f32::INFINITY, f32::min);
    let mut hi = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if let Some(g) = extra {
        lo = lo.min(g);
        hi = hi.max(g);
    }
    let span = (hi - lo).max(min_span);
    let mid = (hi + lo) / 2.0;
    (mid - span / 2.0, span)
}

fn area(c: &mut Canvas, r: Rect, values: &[f32], min_span: f32, color: Rgb, guide: Option<f32>) {
    if values.len() < 2 {
        return;
    }
    let (lo, span) = scale(values, guide, min_span);
    let (top, h) = (r.y + 1.0, r.h - 2.0);
    let y_of = |v: f32| top + h - (v - lo) / span * h;
    if let Some(g) = guide {
        let y = y_of(g).round() + 0.5;
        let mut x = r.x;
        while x < r.right() {
            if let Some(p) = polyline(&[(x, y), ((x + 3.0).min(r.right()), y)]) {
                c.stroke_flat(&p, theme::INK3, 1.0);
            }
            x += 6.0;
        }
    }
    let step = (r.w - 2.0) / (values.len() - 1) as f32;
    let pts: Vec<(f32, f32)> = values
        .iter()
        .enumerate()
        .map(|(i, v)| (r.x + 1.0 + i as f32 * step, y_of(*v)))
        .collect();
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(pts[0].0, r.bottom() - 1.0);
    for p in &pts {
        pb.line_to(p.0, p.1);
    }
    pb.line_to(pts[pts.len() - 1].0, r.bottom() - 1.0);
    pb.close();
    if let Some(p) = pb.finish() {
        c.fill_alpha(&p, color, 26);
    }
    if let Some(p) = polyline(&pts) {
        c.stroke(&p, color, 1.5);
    }
}

fn sparkline(c: &mut Canvas, r: Rect, values: &[f32], min_span: f32, color: Rgb) {
    if values.len() < 2 {
        return;
    }
    let lo = values.iter().copied().fold(f32::INFINITY, f32::min);
    let hi = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let span = (hi - lo).max(min_span);
    let mid = (hi + lo) / 2.0;
    let (lo, span) = (mid - span / 2.0, span);
    // 1 px inside so the 1.5 px stroke never leaves the box
    let (top, h) = (r.y + 1.0, r.h - 2.0);
    let step = (r.w - 2.0) / (values.len() - 1) as f32;
    let pts: Vec<(f32, f32)> = values
        .iter()
        .enumerate()
        .map(|(i, v)| (r.x + 1.0 + i as f32 * step, top + h - (v - lo) / span * h))
        .collect();
    if let Some(p) = polyline(&pts) {
        c.stroke(&p, color, 1.5);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_and_contains() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(a.gap(&Rect::new(14.0, 0.0, 5.0, 5.0)), 4.0);
        assert_eq!(a.gap(&Rect::new(5.0, 5.0, 10.0, 10.0)), -5.0);
        assert!(a.contains(&Rect::new(1.0, 1.0, 9.0, 9.0)));
        assert!(!a.contains(&Rect::new(1.0, 1.0, 10.0, 9.0)));
    }

    #[test]
    fn check_catches_every_rule() {
        let mut t = Tree::default();
        let card = t.add(
            None,
            "card",
            Rect::new(12.0, 12.0, 100.0, 40.0),
            Paint::None,
        );
        t.pad(card, 16.0);
        t.touch(card);
        t.add(
            Some(card),
            "a",
            Rect::new(28.0, 28.0, 20.0, 8.0),
            Paint::None,
        );
        t.add(
            Some(card),
            "b",
            Rect::new(50.0, 28.0, 20.0, 8.0),
            Paint::None,
        ); // 2 px from a
        let tx = t.add(
            Some(card),
            "text",
            Rect::new(80.0, 28.0, 10.0, 8.0),
            Paint::None,
        );
        t.content(tx, 12.0, 8.0); // wider than its slot
        t.add(None, "edge", Rect::new(5.0, 60.0, 50.0, 50.0), Paint::None); // inside the margin
        let v = check(&t);
        assert!(v.iter().any(|l| l.contains("a and card › b")), "{v:?}");
        assert!(v.iter().any(|l| l.contains("content 12.0")), "{v:?}");
        assert!(v.iter().any(|l| l.contains("under 44×44")), "{v:?}");
        assert!(v.iter().any(|l| l.contains("edge goes outside")), "{v:?}");
    }
}
