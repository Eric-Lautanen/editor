pub use crate::gfx::font::FontStyle;
use crate::gfx::{color::Color, geom::Rect};

/// What layout hands to the renderer. Coordinates are in document space (scroll applied at paint).
#[derive(Debug, Clone, PartialEq)]
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
    /// Coarse y index over `ops`, so a paint can skip the chunks that are
    /// entirely off screen. `None` for lists too small to be worth one.
    pub bands: Option<Box<Bands>>,
}

/// How many ops one [`Bands`] entry covers.
const BAND_CHUNK: usize = 512;
/// Lists at or below this many ops paint linearly: indexing them would cost
/// more than the scan it saves.
const BAND_MIN_OPS: usize = 2 * BAND_CHUNK;

/// A coarse y index for a display list: every entry is the painted y extent of
/// one contiguous chunk of ops.
///
/// Paint scans the chunk headers (a few thousand at most, even for a
/// multi-million-op tree) and only walks the chunks that overlap the viewport.
/// Skipping is unconditional: a chunk is skipped only when its *whole* extent
/// lies outside the band, so the result is identical to the linear scan
/// whatever order layout emitted the ops in.
#[derive(Debug, Clone, Default)]
pub struct Bands {
    spans: Vec<(f32, f32)>,
}

impl Bands {
    fn build(ops: &[DrawOp]) -> Self {
        let mut spans = Vec::with_capacity(ops.len().div_ceil(BAND_CHUNK));
        for chunk in ops.chunks(BAND_CHUNK) {
            let mut lo = f32::INFINITY;
            let mut hi = f32::NEG_INFINITY;
            for op in chunk {
                let (a, b) = op.extent();
                lo = lo.min(a);
                hi = hi.max(b);
            }
            spans.push((lo, hi));
        }
        Self { spans }
    }

    /// The op ranges `[start, end)` whose chunks can touch `[top, bottom]`.
    pub fn touching(&self, top: f32, bottom: f32) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.spans
            .iter()
            .enumerate()
            .filter(move |(_, (lo, hi))| *hi >= top && *lo <= bottom)
            .map(|(i, _)| {
                let start = i * BAND_CHUNK;
                (start, start + BAND_CHUNK)
            })
    }
}

impl DrawOp {
    /// The y range this op can paint ink on, in document space. Text is keyed
    /// on its baseline, which is exactly what the painter tests.
    fn extent(&self) -> (f32, f32) {
        match self {
            DrawOp::Rect { rect, .. } | DrawOp::RoundedRect { rect, .. } => (rect.y, rect.bottom()),
            DrawOp::Text { y, .. } => (*y, *y),
            DrawOp::Line { y0, y1, .. } => (y0.min(*y1), y0.max(*y1)),
        }
    }
}

impl DisplayList {
    /// Build a list, indexing its ops for paint when it is big enough to
    /// matter. Every layout goes through here so a dirty frame over a huge
    /// pane never scans millions of off-screen ops.
    pub fn new(ops: Vec<DrawOp>, height: f32, hits: Vec<HitRegion>) -> Self {
        let bands = (ops.len() > BAND_MIN_OPS).then(|| Box::new(Bands::build(&ops)));
        Self {
            ops,
            height,
            hits,
            bands,
        }
    }
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
    /// A footnote marker: jump to the note with this number.
    FootnoteRef(usize),
    /// A note's number or return arrow: jump back to its first reference.
    FootnoteBack(usize),
}
