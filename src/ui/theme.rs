//! Design tokens. Every colour, spacing, size and touch rule the screens use comes from here,
//! so all screens share one rhythm. Values come from the Tokens board (panel limits).
//!
//! Spacing rules (checked by `layout::check` on every screen):
//!   • 12 px from the screen edge on every side         SCREEN_MARGIN
//!   • 8 px between any two blocks or touch targets      GAP
//!   • 16 px inside cards, 12 px inside tiles/buttons    PAD_CARD, PAD_TILE
//!   • 4 px minimum between any two elements in a block  MIN_GAP
//!   • touch targets at least 44 × 44 px, 8 px apart     TOUCH_MIN, GAP

pub const W: f32 = 480.0;
pub const H: f32 = 320.0;

pub const SCREEN_MARGIN: f32 = 12.0;
pub const GAP: f32 = 8.0;
pub const MIN_GAP: f32 = 4.0;
pub const PAD_CARD: f32 = 16.0;
pub const PAD_TILE: f32 = 12.0;
pub const TOUCH_MIN: f32 = 44.0;

pub const RADIUS_CARD: f32 = 16.0;
pub const RADIUS_TILE: f32 = 12.0;

/// One colour as 0xRRGGBB.
pub type Rgb = u32;

pub const GROUND: Rgb = 0x0B0D0E; // blends into the black glass
pub const SURFACE: Rgb = 0x141719; // tiles, buttons
pub const CARD: Rgb = 0x111416; // the hero card
pub const RAISED: Rgb = 0x1E2326; // card border
pub const SEPARATOR: Rgb = 0x23282B;
pub const TRACK: Rgb = 0x262B2F; // empty part of a bar
pub const INK: Rgb = 0xF2F1EC; // 17.2:1 on ground
pub const INK2: Rgb = 0xA9AFB4; // 8.8:1
pub const INK3: Rgb = 0x80878D; // 5.3:1, labels, 13 px minimum
pub const MUTED: Rgb = 0x4A4E52; // "not set up" markers

/// Status colours appear only for status, always with a shape and a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Good,
    Attention,
    Down,
    /// no data from the agent
    Unknown,
}

pub struct StatusColors {
    /// shapes: ring, triangle, square, markers, bar fills
    pub shape: Rgb,
    /// words and numbers in this status
    pub text: Rgb,
    pub card: Rgb,
    pub border: Rgb,
    pub separator: Rgb,
}

pub fn status_colors(s: Status) -> StatusColors {
    match s {
        Status::Good => StatusColors {
            shape: 0x5FD4A6,
            text: INK,
            card: CARD,
            border: RAISED,
            separator: SEPARATOR,
        },
        Status::Attention => StatusColors {
            shape: 0xFFB547,
            text: 0xFFC76E,
            card: 0x1A1610,
            border: 0x5A4520,
            separator: 0x3A3020,
        },
        // Down: the Tokens board gives only the shape colour (#FF6E5E); text and tints follow the
        // same pattern as Attention.
        Status::Down => StatusColors {
            shape: 0xFF6E5E,
            text: 0xFF8A7D,
            card: 0x1C1211,
            border: 0x5C2A24,
            separator: 0x3D2320,
        },
        Status::Unknown => StatusColors {
            shape: INK3,
            text: INK2,
            card: CARD,
            border: RAISED,
            separator: SEPARATOR,
        },
    }
}

/// Font sizes in px (em size) and their line heights.
pub mod size {
    pub const HEADLINE: (f32, f32) = (36.0, 40.0);
    pub const HEADLINE_ALERT: (f32, f32) = (30.0, 36.0);
    pub const VALUE: (f32, f32) = (28.0, 32.0);
    pub const TITLE: (f32, f32) = (15.0, 22.0);
    pub const BUTTON: (f32, f32) = (14.0, 18.0);
    pub const BODY: (f32, f32) = (13.0, 18.0);
    pub const LABEL: (f32, f32) = (12.0, 16.0);
    pub const UNIT: f32 = 16.0;
    /// Big numbers shrink (down to this) when they don't fit; other text keeps its size and is
    /// cut with "…" instead, so nothing goes under the 13 px floor from the Tokens board.
    pub const MIN_VALUE: f32 = 20.0;
}
