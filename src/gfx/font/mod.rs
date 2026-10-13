//! Fonts. Liberation faces (SIL OFL, see `assets/FONT-LICENSE.txt`) are
//! embedded in the binary and parsed at runtime by `ttf`, which rasterizes
//! glyphs on demand behind the `FontSystem` API.
//!
//! Two layers fill the coverage gaps those faces leave, because an uncovered
//! codepoint renders as `.notdef` — a hollow tofu box in Liberation:
//!
//! 1. `shapes` draws the geometric families procedurally (box drawing, blocks,
//!    triangles, check marks).
//! 2. `fallback` reads the symbol and emoji fonts the OS already ships.
pub mod atlas;
pub mod ttf;

mod colr;
mod fallback;
mod shapes;

pub use atlas::FontSystem;

/// Codepoints that must render as nothing and must not advance the pen.
///
/// Emoji sequences are assembled from these: the zero-width joiner glues the
/// components together and VS15/VS16 choose text vs emoji presentation. Without
/// shaping we draw the components separately, but drawing the *joiners* as well
/// would leave a stray mark between them — and the primary face may well have
/// an outline for one (Liberation maps U+200D to a zero-advance glyph that
/// still carries contours), so this has to be an explicit skip rather than a
/// check on the advance.
///
/// U+20E3 (combining enclosing keycap) is here for a related reason. It has no
/// standalone form: every renderer composes it with the digit before it into a
/// keycap image, which needs a `GSUB` ligature we do not implement. So the
/// choice is between a zero-width mark that nothing can draw — `.notdef`, a
/// hollow tofu box sitting on top of the digit — and dropping it, which leaves
/// the plain digit. The digit is the honest half of the pair.
pub(crate) fn is_invisible(ch: char) -> bool {
    matches!(ch,
        '\u{200B}'..='\u{200F}'   // ZWSP, ZWNJ, ZWJ, LRM, RLM
        | '\u{2060}'              // word joiner
        | '\u{20E3}'              // combining enclosing keycap
        | '\u{FE00}'..='\u{FE0F}' // variation selectors 1-16
        | '\u{FEFF}'              // ZWNBSP / BOM
    )
}

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
    /// Ink coverage, one byte per pixel. This is what an outline rasterizes
    /// to, and for a colour bitmap it is the image's own alpha — so "how much
    /// ink is here" works the same for both kinds of glyph.
    pub coverage: Vec<u8>,
    /// Premultiplied-by-nothing RGBA, one 4-byte pixel per pixel, present only
    /// for *colour* glyphs (emoji bitmaps). `None` — the overwhelming majority
    /// — means the glyph is an outline to be tinted with the text colour.
    pub color: Option<Vec<u8>>,
    /// Coverage of the pixels a colour glyph asked to draw in the *text*
    /// colour, from COLR's `0xFFFF` palette sentinel. Segoe UI Emoji uses it
    /// for outlines and accents, so it has to survive the trip from the
    /// rasterizer to the canvas, which is the only place the text colour is
    /// known. `None` for outline glyphs and for colour glyphs that do not use
    /// the sentinel.
    pub fg: Option<Vec<u8>>,
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
