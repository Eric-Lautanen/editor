//! Fonts. Phase 1 (now): pre-rasterized anti-aliased atlas (Liberation, SIL OFL) embedded in the binary.
//! Phase 2 (M7): `ttf` loads system fonts and rasterizes on demand behind the same `FontSystem` API.
pub mod atlas;
pub mod ttf;

pub use atlas::FontSystem;

/// Available faces. Serif regular/bold/italic/bold-italic drive Markdown
/// emphasis (M4); monospace drives plain text, code, and JSON trees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontStyle {
    Regular,
    Bold,
    Italic,
    BoldItalic,
    Mono,
}

impl FontStyle {
    /// Index into the embedded atlas faces (see `tools/gen_font.py` face order).
    pub(crate) fn index(self) -> usize {
        match self {
            FontStyle::Regular => 0,
            FontStyle::Bold => 1,
            FontStyle::Italic => 2,
            FontStyle::BoldItalic => 3,
            FontStyle::Mono => 4,
        }
    }
}

/// A rasterized glyph at a specific pixel size: 8-bit coverage, row-major, top-down.
pub struct Glyph {
    pub width: usize,
    pub height: usize,
    /// Pixels right of the pen origin to the bitmap's left edge.
    pub left: i32,
    /// Pixels *above* the baseline to the bitmap's top row.
    pub top: i32,
    pub coverage: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub ascent: f32,
    pub descent: f32,
}

impl Metrics {
    pub fn height(&self) -> f32 {
        self.ascent + self.descent
    }
}
