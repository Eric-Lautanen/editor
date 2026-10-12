pub use crate::gfx::font::FontStyle;
use crate::gfx::{color::Color, geom::Rect};

/// What layout hands to the renderer. Coordinates are in document space (scroll applied at paint).
#[derive(Debug, Clone)]
pub enum DrawOp {
    Rect {
        rect: Rect,
        color: Color,
    },
    RoundedRect {
        rect: Rect,
        radius: f32,
        color: Color,
    },
    /// `y` is the text BASELINE.
    Text {
        x: f32,
        y: f32,
        text: String,
        size: f32,
        style: FontStyle,
        color: Color,
    },
    Line {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: Color,
    },
}

#[derive(Debug, Default, Clone)]
pub struct DisplayList {
    pub ops: Vec<DrawOp>,
    /// Total laid-out height, for scrollbar range.
    pub height: f32,
    /// Hit regions: collapse toggles and links.
    pub hits: Vec<HitRegion>,
}

#[derive(Debug, Clone)]
pub struct HitRegion {
    pub rect: Rect,
    pub action: HitAction,
}

#[derive(Debug, Clone)]
pub enum HitAction {
    Link(String),
    ToggleNode(usize),
}
