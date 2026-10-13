//! Phase 2 font backend: `FontSystem` now delegates to the real TTF parser and
//! rasterizer in [`super::ttf`] over the embedded Liberation faces at their
//! native sizes — no pre-rasterized bitmap atlas any more.
//!
//! The public API (`metrics`, `advance`, `measure`, `glyph`) and the
//! glyph-cache keys are byte-for-byte what `atlas` offered, so every caller
//! (canvas, menu, titlebar, statusbar, layouts, icon) is untouched.
//!
//! Coverage gaps — codepoints Liberation does not carry — are filled by
//! [`super::shapes`], which draws box drawing, block elements and common
//! symbols procedurally. Note that an uncovered codepoint maps to `.notdef`,
//! which in these faces is a *hollow rectangle* rather than a blank, so
//! without that fallback un-covered text renders as a row of tofu boxes.
//!
//! RAM: the faces borrow `static` embedded font bytes (zero copies) and
//! glyphs rasterize on demand behind the same Rc cache shape the atlas had.

use super::{fallback, is_invisible, shapes, ttf, FontStyle, Glyph, Metrics};
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
    /// System-font fallback, built on the first codepoint Liberation cannot
    /// draw. Stays `None` until then, so a plain document never touches the
    /// disk.
    fallback: RefCell<Option<fallback::Chain>>,
}

/// Key for the rasterized-glyph cache: face index, char, pixel size in 1/16 px,
/// and the presentation it was asked for.
///
/// The presentation belongs in the key because `❤` and `❤️` are different
/// rasters out of different faces; caching both under one key would hand back
/// whichever was drawn first.
type CacheKey = (u8, char, u32, fallback::Presentation);
/// Memoized glyph rasterizations; `None` marks blank glyphs (e.g. space).
type GlyphCache = HashMap<CacheKey, Option<Rc<Glyph>>>;

impl FontSystem {
    /// Parse the embedded TTF faces. The data is build-time trusted, so
    /// malformed input panics (caught by tests) — same contract as before.
    pub fn load_embedded() -> Self {
        let faces = FONT_FILES.map(|bytes| {
            ttf::Face::parse(bytes).unwrap_or_else(|e| panic!("embedded font parse failed: {e}"))
        });
        Self {
            faces,
            cache: RefCell::new(HashMap::new()),
            fallback: RefCell::new(None),
        }
    }

    /// Run `f` against the fallback chain, building it on first use.
    ///
    /// The `RefCell` borrow is released before the value is returned, so a
    /// caller can hold the resulting `Rc<Glyph>` while other code borrows the
    /// chain again.
    fn with_fallback<T>(&self, f: impl FnOnce(&mut fallback::Chain) -> T) -> T {
        let mut slot = self.fallback.borrow_mut();
        let chain = slot.get_or_insert_with(fallback::Chain::new);
        f(chain)
    }

    pub fn metrics(&self, style: FontStyle, size: f32) -> Metrics {
        let f = &self.faces[style.index()];
        let m = f.metrics();
        Metrics {
            ascent: m.ascent * size,
            descent: m.descent * size,
        }
    }

    /// Advance for `ch` with no lookahead — the presentation UTS #51 makes the
    /// default for it.
    pub fn advance(&self, style: FontStyle, ch: char, size: f32) -> f32 {
        self.advance_at(style, ch, None, size)
    }

    /// [`Self::advance`] for a caller that can see what follows `ch`, so a
    /// variation selector can choose between the text and the emoji form.
    ///
    /// A selector is the one case that overrides the primary face's precedence:
    /// the emoji form of © is not Liberation's ©, so the chain has to be asked
    /// before the face that would otherwise answer.
    pub fn advance_at(&self, style: FontStyle, ch: char, next: Option<char>, size: f32) -> f32 {
        if is_invisible(ch) {
            return 0.0;
        }
        let p = fallback::Presentation::of(next);
        let f = &self.faces[style.index()];
        let gid = f.glyph_id(ch as u32);
        if p != fallback::Presentation::Default {
            if let Some(w) = self.with_fallback(|c| c.advance(ch as u32, p, size)) {
                return w;
            }
        }
        if gid != 0 {
            return f.advance_px(gid, size);
        }
        // Uncovered by the primary face. Procedural box drawing borrows the
        // font's light rule so columns stay aligned; then the OS's own fonts;
        // and `.notdef`'s width only if nothing can draw it.
        if let Some(r) = shapes::reference(ch) {
            return f.advance_px(f.glyph_id(r as u32), size);
        }
        if p == fallback::Presentation::Default {
            if let Some(w) = self.with_fallback(|c| c.advance(ch as u32, p, size)) {
                return w;
            }
        }
        f.advance_px(0, size)
    }

    pub fn measure(&self, style: FontStyle, text: &str, size: f32) -> f32 {
        let mut it = text.chars().peekable();
        let mut w = 0.0;
        while let Some(ch) = it.next() {
            w += self.advance_at(style, ch, it.peek().copied(), size);
        }
        w
    }

    /// [`Self::measure`] over the char range `s..e` of `text`.
    ///
    /// Exists so the layout can measure a *piece* of a span — a wrapped row, a
    /// table cell — without collecting it, while still giving each character
    /// the lookahead that decides its presentation.
    pub fn measure_range(
        &self,
        style: FontStyle,
        text: &str,
        s: usize,
        e: usize,
        size: f32,
    ) -> f32 {
        let mut it = text.chars().skip(s).take(e.saturating_sub(s)).peekable();
        let mut w = 0.0;
        while let Some(ch) = it.next() {
            w += self.advance_at(style, ch, it.peek().copied(), size);
        }
        w
    }

    /// Rasterized glyph at `size` px (cached). `None` for blank glyphs
    /// (space) or codepoints nothing can draw.
    pub fn glyph(&self, style: FontStyle, ch: char, size: f32) -> Option<Rc<Glyph>> {
        self.glyph_at(style, ch, None, size)
    }

    /// [`Self::glyph`] for a caller that can see what follows `ch`.
    ///
    /// With a variation selector in play the chain is consulted *first*, for
    /// the same reason [`Self::advance_at`] does it: the selector names a
    /// presentation, and only the OS faces can supply the colour one.
    pub fn glyph_at(
        &self,
        style: FontStyle,
        ch: char,
        next: Option<char>,
        size: f32,
    ) -> Option<Rc<Glyph>> {
        if is_invisible(ch) {
            return None;
        }
        let p = fallback::Presentation::of(next);
        let key = (style.index() as u8, ch, (size * 16.0).round() as u32, p);
        if let Some(g) = self.cache.borrow().get(&key) {
            return g.clone();
        }
        let face = &self.faces[style.index()];
        let gid = face.glyph_id(ch as u32);
        let g = if p == fallback::Presentation::Default {
            if gid == 0 {
                // Uncovered: the face would draw `.notdef`, a hollow tofu box.
                // Prefer a procedural shape, then the OS's own fonts, and only
                // fall back to the box for what nothing on this machine can draw.
                let cell = Self::cell_for(face, ch, size);
                shapes::glyph(ch, &cell, size)
                    .or_else(|| self.with_fallback(|c| c.glyph(ch as u32, p, size)))
                    .or_else(|| face.glyph(ch, size))
            } else {
                face.glyph(ch, size)
            }
        } else {
            // A selector asked for a specific presentation, so the chain gets
            // the first word; the primary face, a procedural shape, and finally
            // `.notdef` are the graceful degradations if it has nothing.
            self.with_fallback(|c| c.glyph(ch as u32, p, size))
                .or_else(|| if gid != 0 { face.glyph(ch, size) } else { None })
                .or_else(|| {
                    let cell = Self::cell_for(face, ch, size);
                    shapes::glyph(ch, &cell, size)
                })
                .or_else(|| face.glyph(ch, size))
        };
        self.cache.borrow_mut().insert(key, g.clone());
        g
    }

    /// The em-space box a procedural glyph is drawn into, sized from the face
    /// so fallbacks line up with its own glyphs.
    fn cell_for(face: &ttf::Face<'_>, ch: char, size: f32) -> shapes::Cell {
        let m = face.metrics();
        let gid = shapes::reference(ch).map_or(0, |r| face.glyph_id(r as u32));
        shapes::Cell {
            advance: face.advance_px(gid, size) / size,
            ascent: m.ascent,
            descent: m.descent,
        }
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

    #[test]
    fn procedural_heavy_rule_lines_up_with_the_fonts_light_rule() {
        let f = FontSystem::load_embedded();
        // `─` comes from the face; `━` is drawn procedurally.
        let light = f.glyph(FontStyle::Mono, '\u{2500}', 32.0).unwrap();
        let heavy = f.glyph(FontStyle::Mono, '\u{2501}', 32.0).unwrap();
        let ink = |g: &Glyph| g.coverage.iter().map(|&c| c as usize).sum::<usize>();
        // Same cell, so a column of rules stays aligned...
        assert_eq!(
            heavy.width, light.width,
            "heavy rule must share the light rule's cell"
        );
        assert_eq!(heavy.left, light.left, "and start at the same offset");
        // ...but roughly twice the stroke weight.
        assert!(
            ink(&heavy) > ink(&light) * 3 / 2,
            "heavy {} should clearly outweigh light {}",
            ink(&heavy),
            ink(&light)
        );
        // The advance must match too, or the glyphs would drift apart.
        assert!(
            (f.advance(FontStyle::Mono, '\u{2501}', 32.0)
                - f.advance(FontStyle::Mono, '\u{2500}', 32.0))
            .abs()
                < 0.01
        );
    }

    #[test]
    fn uncovered_codepoints_get_a_shape_instead_of_a_tofu_box() {
        let f = FontSystem::load_embedded();
        // `☐` is absent from Liberation; `.notdef` would be a hollow box, and
        // our ballot box is hollow too, so compare against a *filled* mark.
        let star = f.glyph(FontStyle::Mono, '\u{2605}', 32.0).unwrap();
        let center = (star.height / 2) * star.width + star.width / 2;
        assert!(
            star.coverage[center] > 200,
            "the star's centre should be solid, not hollow"
        );
        // A codepoint `shapes` does not attempt still renders something: the
        // fallback chain when the machine has a system font, and `.notdef`
        // otherwise. Either way it must not come back empty.
        assert!(f.glyph(FontStyle::Mono, '\u{4E2D}', 32.0).is_some());
    }

    /// The point of the chain: a codepoint Liberation cannot draw comes back as
    /// a real glyph rather than the `.notdef` tofu box.
    ///
    /// Tolerates a machine with no usable system font (bare CI), where the old
    /// `.notdef` behaviour is still the honest answer.
    #[test]
    fn system_fonts_replace_the_notdef_box() {
        let f = FontSystem::load_embedded();
        // An unmapped codepoint falls all the way through to `.notdef`; its
        // raster is our reference "tofu".
        let Some(notdef) = f.glyph(FontStyle::Regular, '\u{E0000}', 32.0) else {
            return;
        };
        // A snowman is absent from Liberation *and* from `shapes`, but is
        // present in every platform's symbols font.
        let Some(snowman) = f.glyph(FontStyle::Regular, '\u{2603}', 32.0) else {
            return; // no system symbols font on this machine
        };
        assert_ne!(
            snowman.coverage, notdef.coverage,
            "the snowman fell back to .notdef: the chain did not engage"
        );
        // And the chain supplies a sane advance, not `.notdef`'s.
        assert!(f.advance(FontStyle::Regular, '\u{2603}', 32.0) > 1.0);
    }

    /// Zero-width format characters must draw nothing and move the pen not at
    /// all, whatever the face happens to carry for them.
    ///
    /// U+20E3 is in the list because its only rendering is a keycap composed
    /// with the digit before it, which needs `GSUB`; on its own it can only be
    /// a tofu box sitting on top of that digit.
    #[test]
    fn zero_width_format_characters_are_skipped() {
        let f = FontSystem::load_embedded();
        for ch in [
            '\u{200D}', '\u{FE0F}', '\u{FE0E}', '\u{200B}', '\u{FEFF}', '\u{20E3}',
        ] {
            assert_eq!(f.advance(FontStyle::Regular, ch, 32.0), 0.0, "{ch:?}");
            assert!(f.glyph(FontStyle::Regular, ch, 32.0).is_none(), "{ch:?}");
        }
    }

    /// A variation selector has to change the *raster*, not just the advance:
    /// `❤` is an outline and `❤️` colour art, and the two must not be served
    /// out of the same cache slot.
    #[test]
    fn a_variation_selector_changes_the_rasterized_glyph() {
        let f = FontSystem::load_embedded();
        let Some(plain) = f.glyph(FontStyle::Regular, '\u{2764}', 32.0) else {
            return; // no face on this machine carries the heart
        };
        let Some(emoji) = f.glyph_at(FontStyle::Regular, '\u{2764}', Some('\u{FE0F}'), 32.0) else {
            return;
        };
        assert!(plain.color.is_none(), "the bare heart should be an outline");
        if emoji.color.is_none() {
            return; // no colour face installed: monochrome is the honest answer
        }
        // Ask again, in the other order: a cache keyed on the codepoint alone
        // would hand back whichever form happened to be rasterized first.
        let again = f
            .glyph_at(FontStyle::Regular, '\u{2764}', Some('\u{FE0F}'), 32.0)
            .expect("cached colour glyph");
        assert!(again.color.is_some(), "the cache returned the text form");
        assert!(
            f.glyph(FontStyle::Regular, '\u{2764}', 32.0)
                .expect("cached text glyph")
                .color
                .is_none(),
            "the colour form leaked into the text slot"
        );
    }

    /// VS15 is the mirror image: it must keep an emoji codepoint monochrome
    /// even though UTS #51 gives that codepoint colour by default.
    #[test]
    fn vs15_keeps_an_emoji_codepoint_monochrome() {
        let f = FontSystem::load_embedded();
        let Some(emoji) = f.glyph(FontStyle::Regular, '\u{1F600}', 32.0) else {
            return;
        };
        if emoji.color.is_none() {
            return; // no colour face installed
        }
        if let Some(text) = f.glyph_at(FontStyle::Regular, '\u{1F600}', Some('\u{FE0E}'), 32.0) {
            assert!(text.color.is_none(), "VS15 did not force the text form");
        }
    }

    /// `measure` must agree with summing the per-character advances, or the
    /// wrapper and the renderer would disagree about where a line ends.
    #[test]
    fn measure_matches_the_per_character_advances() {
        let f = FontSystem::load_embedded();
        for s in ["a\u{2764}\u{FE0F}b", "\u{1F600}\u{FE0E}", "plain text"] {
            let chars: Vec<char> = s.chars().collect();
            let want: f32 = (0..chars.len())
                .map(|i| {
                    f.advance_at(
                        FontStyle::Regular,
                        chars[i],
                        chars.get(i + 1).copied(),
                        16.0,
                    )
                })
                .sum();
            assert!(
                (f.measure(FontStyle::Regular, s, 16.0) - want).abs() < 0.01,
                "measure disagreed with the advances for {s:?}"
            );
        }
    }

    /// Measuring a sub-range has to give the same answer as measuring those
    /// characters as their own string — the layout uses the two
    /// interchangeably, and the lookahead must not see past the range.
    #[test]
    fn measure_range_agrees_with_measure() {
        let f = FontSystem::load_embedded();
        let s = "x\u{2764}\u{FE0F}y";
        let piece = f.measure(FontStyle::Regular, "\u{2764}\u{FE0F}y", 16.0);
        assert!((f.measure_range(FontStyle::Regular, s, 1, 4, 16.0) - piece).abs() < 0.01);
        // An empty or inverted range measures nothing rather than panicking.
        assert_eq!(f.measure_range(FontStyle::Regular, s, 2, 2, 16.0), 0.0);
        assert_eq!(f.measure_range(FontStyle::Regular, s, 3, 1, 16.0), 0.0);
    }
}
