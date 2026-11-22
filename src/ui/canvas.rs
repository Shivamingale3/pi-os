//! Drawing: tiny-skia (anti-aliased shapes) + ab_glyph (TrueType text), into a 480 × 320 RGBA pixmap.
//! Text follows CSS: sizes are em sizes in px, line boxes centre the font's ascent + descent.

use ab_glyph::{Font as _, FontRef, PxScale, ScaleFont, point};
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Mask, Paint, Path, PathBuilder, Pixmap,
    PremultipliedColorU8, Rect as SkRect, Stroke, Transform,
};

use super::layout::Rect;
use super::theme::{self, Rgb};

static INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
static INTER_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");
static MONO_REGULAR: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    Regular,
    SemiBold,
    Mono,
}

pub struct Fonts {
    regular: FontRef<'static>,
    semibold: FontRef<'static>,
    mono: FontRef<'static>,
}

impl Fonts {
    pub fn load() -> Self {
        let f = |b| FontRef::try_from_slice(b).expect("embedded font");
        Fonts {
            regular: f(INTER_REGULAR),
            semibold: f(INTER_SEMIBOLD),
            mono: f(MONO_REGULAR),
        }
    }

    fn face(&self, f: Face) -> &FontRef<'static> {
        match f {
            Face::Regular => &self.regular,
            Face::SemiBold => &self.semibold,
            Face::Mono => &self.mono,
        }
    }
}

/// A text style: face, em size, letter spacing (px), tabular digits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub face: Face,
    pub size: f32,
    pub spacing: f32,
    pub tnum: bool,
}

impl Style {
    pub const fn new(face: Face, size: f32) -> Self {
        Style {
            face,
            size,
            spacing: 0.0,
            tnum: true,
        }
    }
    pub const fn spacing(mut self, px: f32) -> Self {
        self.spacing = px;
        self
    }
    pub const fn sized(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

impl Fonts {
    fn scale(&self, s: &Style) -> PxScale {
        let f = self.face(s.face);
        let upem = f.units_per_em().unwrap_or(2048.0);
        PxScale::from(s.size * f.height_unscaled() / upem)
    }

    /// (ascent, descent) in px, descent positive.
    pub fn vmetrics(&self, s: &Style) -> (f32, f32) {
        let sf = self.face(s.face).as_scaled(self.scale(s));
        (sf.ascent(), -sf.descent())
    }

    /// Baseline of a line box starting at `top` with height `lh` (CSS half-leading).
    pub fn baseline(&self, s: &Style, top: f32, lh: f32) -> f32 {
        let (a, d) = self.vmetrics(s);
        top + (lh - (a + d)) / 2.0 + a
    }

    fn advance(&self, s: &Style, c: char) -> f32 {
        let sf = self.face(s.face).as_scaled(self.scale(s));
        if s.tnum && c.is_ascii_digit() {
            ('0'..='9')
                .map(|d| sf.h_advance(sf.glyph_id(d)))
                .fold(0.0, f32::max)
        } else {
            sf.h_advance(sf.glyph_id(c))
        }
    }

    pub fn width(&self, s: &Style, text: &str) -> f32 {
        let sf = self.face(s.face).as_scaled(self.scale(s));
        let mut w = 0.0;
        let mut prev = None;
        for (i, c) in text.chars().enumerate() {
            let id = sf.glyph_id(c);
            if let Some(p) = prev
                && !(s.tnum && c.is_ascii_digit())
            {
                w += sf.kern(p, id);
            }
            if i > 0 {
                w += s.spacing;
            }
            w += self.advance(s, c);
            prev = Some(id);
        }
        w
    }

    /// Cuts `text` with "…" until it fits `max` px.
    pub fn ellipsize(&self, s: &Style, text: &str, max: f32) -> String {
        if self.width(s, text) <= max {
            return text.to_string();
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let t: String = chars.iter().collect::<String>().trim_end().to_string() + "…";
            if self.width(s, &t) <= max {
                return t;
            }
        }
        String::new()
    }

    /// Largest size from `s.size` down to `min` (1 px steps) at which `text` fits `max` px.
    /// Letter spacing scales with the size (it's an em value in the design).
    pub fn fit(&self, s: &Style, text: &str, max: f32, min: f32) -> Option<Style> {
        let mut size = s.size;
        while size >= min {
            let t = Style {
                size,
                spacing: s.spacing / s.size * size,
                ..*s
            };
            if self.width(&t, text) <= max {
                return Some(t);
            }
            size -= 1.0;
        }
        None
    }
}

pub struct Canvas<'f> {
    pub pm: Pixmap,
    pub fonts: &'f Fonts,
}

fn paint(c: Rgb) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8((c >> 16) as u8, (c >> 8) as u8, c as u8, 255);
    p.anti_alias = true;
    p
}

pub fn rounded(r: Rect, radius: f32) -> Path {
    const K: f32 = 0.552_284_8;
    let (x, y, w, h) = (r.x, r.y, r.w, r.h);
    let r = radius.min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + r * K, y, x + w, y + r - r * K, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(
        x + w,
        y + h - r + r * K,
        x + w - r + r * K,
        y + h,
        x + w - r,
        y + h,
    );
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - r * K, y + h, x, y + h - r + r * K, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - r * K, x + r - r * K, y, x + r, y);
    pb.close();
    pb.finish().expect("rounded rect path")
}

pub fn polyline(points: &[(f32, f32)]) -> Option<Path> {
    let (first, rest) = points.split_first()?;
    let mut pb = PathBuilder::new();
    pb.move_to(first.0, first.1);
    for p in rest {
        pb.line_to(p.0, p.1);
    }
    pb.finish()
}

impl<'f> Canvas<'f> {
    pub fn new(fonts: &'f Fonts) -> Self {
        let mut pm = Pixmap::new(theme::W as u32, theme::H as u32).expect("pixmap");
        let g = theme::GROUND;
        pm.fill(Color::from_rgba8(
            (g >> 16) as u8,
            (g >> 8) as u8,
            g as u8,
            255,
        ));
        Canvas { pm, fonts }
    }

    pub fn fill(&mut self, path: &Path, c: Rgb) {
        self.pm.fill_path(
            path,
            &paint(c),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    /// Fill with transparency (0–255), e.g. the area under a chart line or a press highlight.
    pub fn fill_alpha(&mut self, path: &Path, c: Rgb, alpha: u8) {
        let mut p = paint(c);
        p.set_color_rgba8((c >> 16) as u8, (c >> 8) as u8, c as u8, alpha);
        self.pm
            .fill_path(path, &p, FillRule::Winding, Transform::identity(), None);
    }

    /// `r` tinted with `c` at `alpha`, clipped to the rounded box `clip` (a hold's progress).
    pub fn fill_alpha_clipped(&mut self, r: Rect, clip: Rect, radius: f32, c: Rgb, alpha: u8) {
        let shape = rounded(clip, radius);
        let mut mask = Mask::new(self.pm.width(), self.pm.height()).expect("mask");
        mask.fill_path(&shape, FillRule::Winding, true, Transform::identity());
        let mut p = paint(c);
        p.set_color_rgba8((c >> 16) as u8, (c >> 8) as u8, c as u8, alpha);
        if let Some(sr) = SkRect::from_xywh(r.x, r.y, r.w, r.h) {
            self.pm
                .fill_rect(sr, &p, Transform::identity(), Some(&mask));
        }
    }

    /// `r` filled with `c`, clipped to the pill shape of `pill` (for segmented bars).
    pub fn fill_clipped(&mut self, r: Rect, pill: Rect, c: Rgb) {
        let shape = rounded(pill, pill.h / 2.0);
        let mut mask = Mask::new(self.pm.width(), self.pm.height()).expect("mask");
        mask.fill_path(&shape, FillRule::Winding, true, Transform::identity());
        if let Some(sr) = SkRect::from_xywh(r.x, r.y, r.w, r.h) {
            self.pm
                .fill_rect(sr, &paint(c), Transform::identity(), Some(&mask));
        }
    }

    /// A thin stroke with square ends (dashes).
    pub fn stroke_flat(&mut self, path: &Path, c: Rgb, width: f32) {
        let s = Stroke {
            width,
            ..Default::default()
        };
        self.pm
            .stroke_path(path, &paint(c), &s, Transform::identity(), None);
    }

    pub fn stroke(&mut self, path: &Path, c: Rgb, width: f32) {
        let s = Stroke {
            width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        self.pm
            .stroke_path(path, &paint(c), &s, Transform::identity(), None);
    }

    pub fn fill_rect(&mut self, r: Rect, c: Rgb) {
        if let Some(sr) = SkRect::from_xywh(r.x, r.y, r.w, r.h) {
            self.pm
                .fill_rect(sr, &paint(c), Transform::identity(), None);
        }
    }

    pub fn circle(&self, cx: f32, cy: f32, r: f32) -> Path {
        PathBuilder::from_circle(cx, cy, r).expect("circle path")
    }

    /// A pill bar: `track` full width, `fill` from the left for `pct` (0–1), clipped to the pill.
    pub fn bar(&mut self, r: Rect, pct: f32, track: Rgb, fill: Rgb) {
        let shape = rounded(r, r.h / 2.0);
        self.fill(&shape, track);
        let w = (r.w * pct.clamp(0.0, 1.0)).round();
        if w <= 0.0 {
            return;
        }
        let mut mask = Mask::new(self.pm.width(), self.pm.height()).expect("mask");
        mask.fill_path(&shape, FillRule::Winding, true, Transform::identity());
        if let Some(sr) = SkRect::from_xywh(r.x, r.y, w, r.h) {
            self.pm
                .fill_rect(sr, &paint(fill), Transform::identity(), Some(&mask));
        }
    }

    /// Draws `text` with its line box at (x, top, lh); returns the pen x after the text.
    pub fn text(&mut self, s: &Style, text: &str, x: f32, top: f32, lh: f32, color: Rgb) -> f32 {
        let baseline = self.fonts.baseline(s, top, lh);
        self.text_at_baseline(s, text, x, baseline, color)
    }

    pub fn text_at_baseline(
        &mut self,
        s: &Style,
        text: &str,
        x: f32,
        baseline: f32,
        color: Rgb,
    ) -> f32 {
        let font = self.fonts.face(s.face);
        let scale = self.fonts.scale(s);
        let sf = font.as_scaled(scale);
        let (cr, cg, cb) = ((color >> 16) as u8, (color >> 8) as u8, color as u8);
        let (pw, ph) = (self.pm.width() as i32, self.pm.height() as i32);
        let mut pen = x;
        let mut prev = None;
        for (i, c) in text.chars().enumerate() {
            let id = sf.glyph_id(c);
            let tab = s.tnum && c.is_ascii_digit();
            if let Some(p) = prev
                && !tab
            {
                pen += sf.kern(p, id);
            }
            if i > 0 {
                pen += s.spacing;
            }
            let adv = self.fonts.advance(s, c);
            let gx = if tab {
                pen + (adv - sf.h_advance(id)) / 2.0
            } else {
                pen
            };
            if let Some(og) =
                font.outline_glyph(id.with_scale_and_position(scale, point(gx, baseline)))
            {
                let b = og.px_bounds();
                let px = self.pm.pixels_mut();
                og.draw(|gx, gy, cov| {
                    let (x, y) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                    if x < 0 || y < 0 || x >= pw || y >= ph {
                        return;
                    }
                    let i = (y * pw + x) as usize;
                    let d = px[i];
                    let mix =
                        |s: u8, d: u8| (s as f32 * cov + d as f32 * (1.0 - cov)).round() as u8;
                    if let Some(c) = PremultipliedColorU8::from_rgba(
                        mix(cr, d.red()),
                        mix(cg, d.green()),
                        mix(cb, d.blue()),
                        255,
                    ) {
                        px[i] = c;
                    }
                });
            }
            pen += adv;
            prev = Some(id);
        }
        pen
    }

    /// 0x00RRGGBB per pixel, for a preview window.
    #[cfg(feature = "preview")]
    pub fn xrgb(&self) -> Vec<u32> {
        self.pm
            .pixels()
            .iter()
            .map(|p| (p.red() as u32) << 16 | (p.green() as u32) << 8 | p.blue() as u32)
            .collect()
    }
}
