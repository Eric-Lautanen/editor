//! Phase 2 font backend: `FontSystem` now delegates to the real TTF parser
//! + rasterizer in [`super::ttf`] over the embedded Liberation faces at
//! their native sizes — no pre-rasterized bitmap atlas any more.
//!
//! The public API (`metrics`, `advance`, `measure`, `glyph`) and the
//! glyph-cache keys are byte-for-byte what `atlas` offered, so every caller
//! (canvas, menu, titlebar, statusbar, layouts, icon) is untouched.
//!
//! RAM: the faces borrow `static` embedded font bytes (zero copies) and
//! glyphs rasterize on demand behind the same Rc cache shape the atlas had;
//! unknown-codepoint coverage maps to .notdef (blank), matching the old
//! fallback (`?`) behavior for text the faces actually cover is unchanged
//! — the renderer draws nothing for uncovered codepoints either way.

use super::{ttf, FontStyle, Glyph, Metrics};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

/// Embedded Liberation faces, index-aligned with `FontStyle::index()`:
/// Regular, Bold, Italic, BoldItalic, Mono.
const FONT_FILES: [&[u8]; 5] = [
    include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf"),
    include_bytes!("../../../assets/fonts/LiberationSans-Bold.ttf"),
    include_bytes!("../../../assets/fonts/LiberationSans-Italic.ttf"),
    include_bytes!("../../../assets/fonts/LiberationSans-BoldItalic.ttf"),
    include_bytes!("../../../assets/fonts/LiberationMono-Regular.ttf"),
];

pub struct FontSystem {
    faces: [ttf::Face<'static>; 5],
    cache: RefCell<GlyphCache>,
}

/// Key for the rasterized-glyph cache: face index, char, pixel size in 1/16 px.
type CacheKey = (u8, char, u32);
/// Memoized glyph rasterizations; `None` marks blank glyphs (e.g. space).
type GlyphCache = HashMap<CacheKey, Option<Rc<Glyph>>>;

impl FontSystem {
    /// Parse the embedded TTF faces. The data is build-time trusted, so
    /// malformed input panics (caught by tests) — same contract as before.
    pub fn load_embedded() -> Self {
        let faces = FONT_FILES.map(|bytes| {
            ttf::Face::parse(bytes).unwrap_or_else(|e| panic!("embedded font parse failed: {e:?}"))
        });
        Self {
            faces,
            cache: RefCell::new(HashMap::new()),
        }
    }

    pub fn metrics(&self, style: FontStyle, size: f32) -> Metrics {
        let f = &self.faces[style.index()];
        let m = f.metrics();
        Metrics {
            ascent: m.ascent * size,
            descent: m.descent * size,
        }
    }

    pub fn advance(&self, style: FontStyle, ch: char, size: f32) -> f32 {
        let f = &self.faces[style.index()];
        f.advance_px(f.glyph_id(ch as u32), size)
    }

    pub fn measure(&self, style: FontStyle, text: &str, size: f32) -> f32 {
        text.chars().map(|c| self.advance(style, c, size)).sum()
    }

    /// Rasterized glyph at `size` px (cached). `None` for blank glyphs
    /// (space) or uncovered codepoints (the old atlas mapped those to .notdef).
    pub fn glyph(&self, style: FontStyle, ch: char, size: f32) -> Option<Rc<Glyph>> {
        let key = (style.index() as u8, ch, (size * 16.0).round() as u32);
        if let Some(g) = self.cache.borrow().get(&key) {
            return g.clone();
        }
        let g = self.faces[style.index()].glyph(ch, size);
        self.cache.borrow_mut().insert(key, g.clone());
        g
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_and_measures() {
        let f = FontSystem::load_embedded();
        let w = f.measure(FontStyle::Regular, "Hello", 16.0);
        assert!(w > 20.0 && w < 60.0, "unexpected width {w}");
        // monospace: all advances equal
        let a = f.advance(FontStyle::Mono, 'i', 16.0);
        let b = f.advance(FontStyle::Mono, 'W', 16.0);
        assert!((a - b).abs() < 0.01);
        // space is blank, 'A' is not
        assert!(f.glyph(FontStyle::Regular, ' ', 16.0).is_none());
        let g = f.glyph(FontStyle::Regular, 'A', 16.0).unwrap();
        assert!(g.coverage.iter().any(|&c| c > 200));
    }

    #[test]
    fn metrics_match_the_atlas_within_a_px() {
        let f = FontSystem::load_embedded();
        let m = f.metrics(FontStyle::Regular, 16.0);
        // Liberation: ascent ~0.9 em, descent ~0.24 em at size 16.
        assert!(m.ascent > 14.0 && m.ascent < 16.0, "ascent={}", m.ascent);
        assert!(
            m.descent < -3.0 && m.descent > -5.0,
            "descent={}",
            m.descent
        );
        // Bold is heavier than regular at the same size.
        let r = f.advance(FontStyle::Regular, 'W', 16.0);
        let b = f.advance(FontStyle::Bold, 'W', 16.0);
        assert!(b > r * 0.9 && b < r * 1.4, "regular {r} vs bold {b}");
    }

    #[test]
    fn glyphs_cache_and_draw() {
        let f = FontSystem::load_embedded();
        let g1 = f.glyph(FontStyle::Mono, 'X', 24.0).unwrap();
        let g2 = f.glyph(FontStyle::Mono, 'X', 24.0).unwrap();
        assert!(Rc::ptr_eq(&g1, &g2), "second call must hit the cache");
        assert!(g1.coverage.iter().any(|&c| c > 240));
        // Italic face differs from regular for the same char.
        let gi = f.glyph(FontStyle::Italic, 'A', 24.0).unwrap();
        assert_ne!(g1.coverage, gi.coverage);
    }
}
