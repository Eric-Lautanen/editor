//! Runtime fallback to the symbol and emoji fonts the operating system already
//! ships.
//!
//! Liberation covers Latin, Greek, Cyrillic and a slice of punctuation. Anything
//! else — emoji, dingbats, arrows, CJK — used to land on `.notdef`, which in
//! these faces is a *hollow box*, so un-covered text rendered as a row of tofu.
//! `shapes` fills the geometric gaps procedurally, but it cannot draw a
//! pictograph, and hand-drawing thousands of them is not a serious option.
//!
//! Rather than bundle a second font (megabytes, plus a licence to redistribute
//! it), we read the user's own system fonts at runtime. Two properties make
//! that cheap enough to be the default:
//!
//! * **Nothing is bundled and nothing is redistributed.** The files come from
//!   the OS, so the binary does not grow and there is no licence question.
//! * **Loading is lazy and stops at the first hit.** A document of plain prose
//!   never touches the disk; one containing a single arrow loads only the first
//!   candidate that carries it.
//!
//! Colour-capable faces are searched *before* the monochrome ones for emoji
//! codepoints. That ordering matters: the symbols face covers most emoji
//! codepoints with a perfectly good monochrome outline, so without it the
//! emoji font would never be reached and emoji would never be in colour.
//!
//! Candidates must be readable by [`ttf`] — TrueType outlines (`glyf`), COLR
//! colour layers, or CBDT/CBLC bitmaps. That excludes CFF-flavoured fonts, and
//! it is why macOS `Apple Color Emoji` is absent: its artwork is `sbix`-only.

use super::{ttf, Glyph};
use std::{collections::HashMap, path::Path, path::PathBuf, rc::Rc};

/// Font files that can draw emoji *in colour*, most preferred first.
///
/// Kept separate from [`candidates`] because the two lists are consulted at
/// different times: these are tried first, and only for codepoints that are
/// emoji by default.
fn colour_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: &str| out.push(PathBuf::from(p));

    if cfg!(target_os = "windows") {
        let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
        push(
            &PathBuf::from(dir)
                .join("Fonts/seguiemj.ttf")
                .to_string_lossy(),
        );
    } else if cfg!(target_os = "macos") {
        // Nothing: Apple Color Emoji stores `sbix` bitmaps, which carry no
        // outlines and no COLR graph, so there is nothing here we can read.
        // Emoji on macOS fall back to the monochrome symbols face.
    } else {
        for p in [
            "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
            "/usr/share/fonts/noto/NotoColorEmoji.ttf",
            "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
        ] {
            push(p);
        }
    }
    out
}

/// Candidate font files for everything else, most preferred first.
///
/// Ordering is by coverage-per-byte: a symbols face is a couple of megabytes
/// and covers the great majority of what actually shows up as tofu in a
/// document, so it is tried before the much larger CJK faces.
fn candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: &str| out.push(PathBuf::from(p));

    if cfg!(target_os = "windows") {
        // `%WINDIR%\Fonts` is where every Windows install keeps these.
        let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
        let fonts = PathBuf::from(dir).join("Fonts");
        for name in [
            // Symbols and pictographs, monochrome, ~2.5 MB. Measured to cover
            // emoji codepoints with *better* outlines than Segoe UI Emoji's
            // `glyf` (which is a crude stand-in; its real art is COLR layers).
            "seguisym.ttf",
            // CJK, then the other big scripts. The emoji face is not here: it
            // is in `colour_candidates`, and only its COLR art is worth having.
            "msyh.ttc",
            "simsun.ttc",
            "malgun.ttf",
            "msgothic.ttc",
            "Nirmala.ttc",
            "YuGothM.ttc",
        ] {
            push(&fonts.join(name).to_string_lossy());
        }
    } else if cfg!(target_os = "macos") {
        for p in [
            // Monochrome symbols/dingbats, and the CJK faces. Apple Color
            // Emoji is deliberately absent (sbix-only, no outlines to read).
            "/System/Library/Fonts/Apple Symbols.ttf",
            "/Library/Fonts/Arial Unicode.ttf",
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        ] {
            push(p);
        }
    } else {
        for p in [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoEmoji-Regular.ttf",
            "/usr/share/fonts/noto/NotoEmoji-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansSymbols-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
            "/usr/share/fonts/truetype/ancient-scripts/Symbola_hint.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        ] {
            push(p);
        }
    }
    out
}

/// Codepoints whose default presentation is colour emoji, per UTS #51.
///
/// This only ever decides *ordering*: a codepoint that is not listed still
/// renders, just from a monochrome face first. That is why it can be a fixed
/// table without being a correctness risk — and why it is worth having, since
/// a document containing one snowman should not pull a 13 MB emoji font off
/// the disk. Note that the BMP symbols which are *text* by default (☃ U+2603,
/// ❤ U+2764 without a variation selector) are deliberately absent: browsers
/// draw those in the text colour, and so do we.
fn prefers_colour(cp: u32) -> bool {
    matches!(cp,
        // Astral emoji: every block here is emoji-presentation by default.
        0x1F000..=0x1FAFF
        // BMP codepoints with Emoji_Presentation=Yes.
        | 0x231A..=0x231B | 0x23E9..=0x23EC | 0x23F0 | 0x23F3
        | 0x25FD..=0x25FE | 0x2614..=0x2615 | 0x2648..=0x2653
        | 0x267F | 0x2693 | 0x26A1 | 0x26AA..=0x26AB
        | 0x26BD..=0x26BE | 0x26C4..=0x26C5 | 0x26CE | 0x26D4
        | 0x26EA | 0x26F2..=0x26F3 | 0x26F5 | 0x26FA | 0x26FD
        | 0x2705 | 0x270A..=0x270B | 0x2728 | 0x274C | 0x274E
        | 0x2753..=0x2755 | 0x2757 | 0x2795..=0x2797 | 0x27B0 | 0x27BF
        | 0x2B1B..=0x2B1C | 0x2B50 | 0x2B55
    )
}

/// Which presentation a codepoint should be drawn in, per UTS #51.
///
/// The selector is a *separate* codepoint — U+FE0E for text, U+FE0F for emoji —
/// that follows the one it modifies, so honouring it needs a caller who can see
/// the next character. That is why this is threaded in from the text layer
/// rather than decided here: on its own, ❤ has no way to know whether a VS16 is
/// about to arrive.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Presentation {
    /// No selector: whatever UTS #51 makes the default for this codepoint.
    Default,
    /// VS15 follows: force an ordinary outline in the text colour.
    Text,
    /// VS16 follows: force colour art.
    Emoji,
}

impl Presentation {
    /// The presentation the character after a codepoint selects for it.
    ///
    /// Anything that is not a variation selector leaves the default, so a
    /// stray selector with nothing before it is inert.
    pub(crate) fn of(next: Option<char>) -> Self {
        match next {
            Some('\u{FE0E}') => Presentation::Text,
            Some('\u{FE0F}') => Presentation::Emoji,
            _ => Presentation::Default,
        }
    }

    /// Which kind of glyph this presentation asks the chain for.
    fn want(self, cp: u32) -> Want {
        match self {
            Presentation::Text => Want::Plain,
            Presentation::Emoji => Want::Colour,
            Presentation::Default if prefers_colour(cp) => Want::Colour,
            Presentation::Default => Want::Plain,
        }
    }
}

/// Which kind of glyph a lookup is after.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Want {
    /// Either kind — the last resort.
    Any,
    /// Colour art, for a codepoint that is emoji by default.
    Colour,
    /// An ordinary outline, for everything else.
    Plain,
}

/// Faces loaded from the OS so far, plus the codepoints already resolved.
///
/// Loading is *resumable*: candidates are read one at a time, in preference
/// order, and reading stops as soon as one of them carries the codepoint being
/// asked about. A miss is remembered too, so a document full of CJK does not
/// re-scan the symbol faces for every glyph.
/// One face slot: where its bytes are, which directory in them describes it,
/// and the parsed face once something has needed it.
///
/// A `.ttc` collection (Microsoft YaHei, PingFang, NotoSansCJK) holds several
/// faces, and each parse copies per-face tables — so parsing every member of
/// every candidate file eagerly buys nothing: a lookup stops at the first face
/// that carries the codepoint, and faces past that point are never consulted.
/// They are therefore parsed on demand here, which is safe because a face either
/// carries a codepoint or it does not — the answer never depends on when it was
/// parsed, only the cost does.
struct Slot {
    data: &'static [u8],
    dir: u32,
    face: Option<ttf::Face<'static>>,
}

impl Slot {
    /// The parsed face, parsing it on first use. `None` for a member this
    /// parser cannot read (CFF-flavoured, or malformed), cached as such so a
    /// bad member is not re-parsed for every glyph.
    fn get(&mut self) -> Option<&ttf::Face<'static>> {
        if self.face.is_none() {
            self.face = ttf::Face::parse_at(self.data, self.dir).ok();
        }
        self.face.as_ref()
    }
}

pub(crate) struct Chain {
    faces: Vec<Slot>,
    /// Files brought in so far, and how many of those were *mapped* rather than
    /// read into private heap (see [`Chain::load`]) — the difference is the
    /// whole point of the mapping, so the tests assert on it.
    files: usize,
    mapped: usize,
    /// Index of the next monochrome candidate to try; earlier ones are loaded.
    next: usize,
    /// The same, for the colour candidates.
    next_colour: usize,
    /// `(cp, presentation) -> (face index, gid)`, or `None` for "no candidate
    /// carries it".
    ///
    /// The presentation is part of the key because one codepoint genuinely
    /// resolves two different ways: ❤ is an outline out of the symbols face,
    /// ❤️ a bitmap out of the emoji face. Keying on the codepoint alone would
    /// hand whichever came first to both.
    resolved: HashMap<(u32, Presentation), Option<(usize, u16)>>,
}

impl Chain {
    pub(crate) fn new() -> Self {
        Self {
            faces: Vec::new(),
            files: 0,
            mapped: 0,
            next: 0,
            next_colour: 0,
            resolved: HashMap::new(),
        }
    }

    /// Faces actually loaded, for diagnostics and tests.
    #[cfg(test)]
    pub(crate) fn loaded(&self) -> usize {
        self.faces.len()
    }

    /// Files opened so far and how many of them were mapped instead of read.
    /// Zero-length and unreadable candidates count as neither.
    #[cfg(test)]
    pub(crate) fn file_stats(&self) -> (usize, usize) {
        (self.files, self.mapped)
    }

    /// The face at `i`, parsing its member if that has not happened yet — for
    /// tests that want to look at the face a codepoint resolved to.
    #[cfg(test)]
    fn face(&mut self, i: usize) -> Option<&ttf::Face<'static>> {
        self.faces.get_mut(i)?.get()
    }

    /// How many members of every loaded file have been parsed, against how
    /// many were brought in — the measure of the lazy parse.
    #[cfg(test)]
    fn parsed_members(&self) -> (usize, usize) {
        (
            self.faces.iter().filter(|s| s.face.is_some()).count(),
            self.faces.len(),
        )
    }

    /// The face carrying `cp` in presentation `p`, and its glyph id, loading
    /// candidates as needed.
    fn resolve(&mut self, cp: u32, p: Presentation) -> Option<(usize, u16)> {
        if let Some(&hit) = self.resolved.get(&(cp, p)) {
            return hit;
        }
        let hit = self.search(cp, p);
        self.resolved.insert((cp, p), hit);
        hit
    }

    /// Bring one candidate file into the process and append its faces,
    /// returning the index of the first face added.
    ///
    /// The bytes are *mapped*, not read: a fallback font is the single largest
    /// thing this app can allocate (Segoe UI Emoji is ~13 MB, Microsoft YaHei
    /// ~20 MB), and pulling one in for a handful of glyphs used to cost every
    /// byte of the file in private memory for the rest of the run. A mapping
    /// keeps the untouched pages out of the working set entirely, shares the
    /// pages we do read with every other process using the same font, and is
    /// evictable under pressure — see `platform::map_file`. Reading is the
    /// fallback where the platform cannot map (a zero-length file, macOS until
    /// M9), and the data then has to be leaked: `Face` borrows it for `'static`
    /// because a font held for the life of the process is exactly that.
    fn load(&mut self, path: &Path) -> Option<usize> {
        let mapped = crate::platform::map_file(path);
        let lost = mapped.is_none();
        let data: &'static [u8] = match mapped {
            Some(d) => d,
            None => Box::leak(std::fs::read(path).ok()?.into_boxed_slice()),
        };
        // The mapping is the whole point being that this is small, so keep the
        // count honest for `loaded_bytes` and the diagnostics below.
        self.files += 1;
        self.mapped += usize::from(!lost);
        let dirs = ttf::faces(data).ok()?;
        if dirs.is_empty() {
            return None;
        }
        let first = self.faces.len();
        for dir in dirs {
            self.faces.push(Slot {
                data,
                dir,
                face: None,
            });
        }
        Some(first)
    }

    fn search(&mut self, cp: u32, p: Presentation) -> Option<(usize, u16)> {
        // An emoji codepoint wants colour art, so the colour faces are read
        // first — otherwise the symbols face would answer for it and the emoji
        // face would never be opened. Everything else wants an ordinary
        // outline, so that a symbol which is *text* by default stays in the
        // text colour even when the emoji face happens to be loaded already.
        let want = p.want(cp);

        // Anything already loaded that draws it the way we want wins, before
        // more disk is touched.
        if let Some(hit) = self.scan(0, cp, want) {
            return Some(hit);
        }

        if want == Want::Colour {
            let files = colour_candidates();
            while self.next_colour < files.len() {
                let path = files[self.next_colour].clone();
                self.next_colour += 1;
                let Some(first) = self.load(&path) else {
                    continue;
                };
                if let Some(hit) = self.scan(first, cp, Want::Colour) {
                    return Some(hit);
                }
            }
        }

        let files = candidates();
        while self.next < files.len() {
            let path = files[self.next].clone();
            self.next += 1;
            let Some(first) = self.load(&path) else {
                continue;
            };
            if let Some(hit) = self.scan(first, cp, want) {
                return Some(hit);
            }
        }

        // Nothing drew it the preferred way. Whatever covers it at all is
        // better than a tofu box.
        self.scan(0, cp, Want::Any)
    }

    /// First face at or after `from` that carries `cp`, matching `want`,
    /// parsing the members it walks past (see [`Slot`]).
    fn scan(&mut self, from: usize, cp: u32, want: Want) -> Option<(usize, u16)> {
        for i in from..self.faces.len() {
            let Some(f) = self.faces[i].get() else {
                continue;
            };
            let gid = f.glyph_id(cp);
            // A cmap entry pointing past `loca` is a corrupt font — and a
            // byte-swapped slot is exactly how that happens. Treat it as "this
            // face does not carry the codepoint" rather than indexing out of
            // bounds downstream.
            if gid == 0 || gid >= f.num_glyphs() {
                continue;
            }
            // A face "draws it in colour" only when it has a paint graph for
            // this glyph: a cmap entry alone is not enough.
            match want {
                Want::Any => {}
                Want::Colour if !f.has_colour(gid) => continue,
                Want::Plain if f.has_colour(gid) => continue,
                _ => {}
            }
            return Some((i, gid));
        }
        None
    }

    /// Rasterize `cp` from the first face that carries it in presentation `p`.
    pub(crate) fn glyph(&mut self, cp: u32, p: Presentation, size: f32) -> Option<Rc<Glyph>> {
        let (i, gid) = self.resolve(cp, p)?;
        self.faces[i].get()?.glyph_by_id(gid, size)
    }

    /// Advance width for `cp` in presentation `p`, in the face that carries it.
    pub(crate) fn advance(&mut self, cp: u32, p: Presentation, size: f32) -> Option<f32> {
        let (i, gid) = self.resolve(cp, p)?;
        Some(self.faces[i].get()?.advance_px(gid, size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The presentation a caller gets when it does not look ahead.
    const D: Presentation = Presentation::Default;

    /// A fallback font's bytes must never become live heap: the file is mapped,
    /// not read (see [`Chain::load`]). A whole file read is ~20 MB for the CJK
    /// faces, so this is the regression guard for the app's memory profile.
    #[test]
    fn a_fallback_font_is_mapped_and_not_retained_as_heap() {
        let path = candidates()
            .into_iter()
            .chain(colour_candidates())
            .find(|p| std::fs::metadata(p).is_ok_and(|m| m.len() >= 4 * 1024 * 1024));
        let Some(path) = path else {
            return; // nothing large enough installed (bare CI)
        };
        let size = std::fs::metadata(&path).expect("size").len();
        let before = crate::mem::snapshot().global_live;
        let mut chain = Chain::new();
        if chain.load(&path).is_none() {
            return; // unreadable by this parser (CFF, say)
        }
        let grew = crate::mem::snapshot().global_live.saturating_sub(before);
        let (files, mapped) = chain.file_stats();
        assert_eq!(files, 1, "one file loaded");
        // Elsewhere in the process the fallback is a read; there the file's
        // bytes are live heap by necessity, so only the mapped case is the
        // strong statement (and it is the case on Windows and Linux).
        if mapped == 1 {
            assert!(
                grew < size / 2,
                "loading {} ({size} B) grew live heap by {grew} B",
                path.display()
            );
        }
    }

    /// Bringing a file in must not parse any of its members: the parse happens
    /// when a lookup reaches one, so the members of a `.ttc` behind the
    /// answering face never pay for their per-face tables.
    #[test]
    fn loading_a_file_parses_nothing_until_a_lookup_asks() {
        let Some(path) = candidates().into_iter().find(|p| p.exists()) else {
            return; // nothing installed (bare CI)
        };
        let mut chain = Chain::new();
        if chain.load(&path).is_none() {
            return; // unreadable by this parser
        }
        let (parsed, slots) = chain.parsed_members();
        assert!(slots >= 1, "load() registered no members");
        assert_eq!(
            parsed, 0,
            "load() parsed {parsed} of {slots} members eagerly"
        );
    }

    /// A lookup parses the members it walks and no others, and repeating it
    /// neither parses more nor resolves differently.
    #[test]
    fn a_lookup_parses_only_what_it_walks() {
        let mut chain = Chain::new();
        let Some(cp) = [0x4E2Du32, 0x6587, 0x1F600, 0x2192]
            .into_iter()
            .find(|&cp| chain.resolve(cp, D).is_some())
        else {
            return; // no font on this machine covers any of them
        };
        let (parsed, slots) = chain.parsed_members();
        assert!(parsed <= slots, "{parsed} parsed of {slots} members");
        chain.resolve(cp, D);
        assert_eq!(
            chain.parsed_members(),
            (parsed, slots),
            "a memoized lookup parsed more members"
        );
    }

    #[test]
    fn every_platform_offers_candidates_in_preference_order() {
        let c = candidates();
        assert!(!c.is_empty(), "no fallback candidates for this platform");
        // The first candidate is the cheap symbols face, not a huge CJK one:
        // ordering is what keeps a plain document off the disk entirely.
        let first = c[0].to_string_lossy().to_ascii_lowercase();
        assert!(
            first.contains("sym") || first.contains("dejavu"),
            "unexpected first candidate: {first}"
        );
        // The colour list, where a platform has one, is small and separate.
        assert!(colour_candidates().len() <= 3);
    }

    /// The colour pass must not steal codepoints that are text by default.
    #[test]
    fn only_default_emoji_presentation_asks_for_colour() {
        for cp in [0x1F600, 0x1F44D, 0x2705, 0x26A1, 0x2B50, 0x1F1E6] {
            assert!(prefers_colour(cp), "U+{cp:04X} should want colour");
        }
        // Snowman and heart are text-presentation without a variation
        // selector, so they stay monochrome — as they do in a browser.
        for cp in [0x2603, 0x2764, 0x2192, 0x4E2D, 0x41, 0x2605] {
            assert!(!prefers_colour(cp), "U+{cp:04X} should not want colour");
        }
    }

    /// A miss must be remembered, and must not re-read the disk.
    #[test]
    fn resolving_a_missing_codepoint_is_memoized_and_survives_repeats() {
        let mut chain = Chain::new();
        // U+E0000 (a tag character) is carried by no mainstream text font.
        assert_eq!(chain.resolve(0xE0000, D), None);
        let loaded = chain.next;
        assert_eq!(chain.resolve(0xE0000, D), None, "second look must agree");
        assert_eq!(chain.next, loaded, "a memoized miss must not re-scan");
    }

    /// The chain must never claim a codepoint it cannot actually rasterize,
    /// and must never panic on odd input.
    #[test]
    fn a_resolved_glyph_is_consistent_with_its_advance() {
        let mut chain = Chain::new();
        for cp in [0x2192u32, 0x1F600, 0x4E2D, 0x2603, 0x0C05] {
            let Some((i, gid)) = chain.resolve(cp, D) else {
                continue;
            };
            let face = chain.face(i).expect("the face a codepoint resolved to");
            assert!(gid != 0 && gid < face.num_glyphs(), "U+{cp:04X} gid {gid}");
            assert!(
                face.advance_px(gid, 16.0) > 0.0,
                "U+{cp:04X} has a glyph but no advance"
            );
        }
    }

    /// Loading must actually stop early: the first codepoint should not pull
    /// in every candidate on the list.
    #[test]
    fn loading_stops_before_exhausting_the_candidate_list() {
        let mut chain = Chain::new();
        let total = candidates().len();
        if chain.resolve(0x2192, D).is_some() {
            assert!(
                chain.next < total,
                "an early hit loaded all {total} candidates"
            );
        }
        assert!(chain.loaded() <= total, "loaded more faces than candidates");
    }

    /// Once the emoji face is loaded it must not start answering for symbols
    /// that are *text* by default. Otherwise a heart would be monochrome in a
    /// document with no emoji and red in one that has a single grinning face.
    #[test]
    fn loading_the_emoji_face_does_not_recolour_text_symbols() {
        let mut chain = Chain::new();
        if chain.resolve(0x1F600, D).is_none() {
            return; // no emoji face on this machine
        }
        for cp in [0x2764u32, 0x2603, 0x2192] {
            let Some((i, gid)) = chain.resolve(cp, D) else {
                continue;
            };
            let face = chain.face(i).expect("the face a codepoint resolved to");
            assert!(
                !face.has_colour(gid),
                "U+{cp:04X} turned colour only because an emoji came first"
            );
        }
    }

    /// Where the platform ships a colour emoji face, an emoji codepoint must
    /// resolve to it and come back with actual colour pixels. Tolerates a
    /// machine without one (bare CI), where the monochrome answer is correct.
    #[test]
    fn emoji_resolve_to_colour_when_the_platform_has_a_colour_face() {
        let mut chain = Chain::new();
        let Some((i, gid)) = chain.resolve(0x1F600, D) else {
            return; // no font on this machine covers it
        };
        if !chain.face(i).is_some_and(|f| f.has_colour(gid)) {
            return; // no colour face installed
        }
        let g = chain.glyph(0x1F600, D, 32.0).expect("colour glyph");
        let rgba = g.color.as_ref().expect("colour pixels");
        assert_eq!(rgba.len(), g.width * g.height * 4);
        // A grinning face is not greyscale: some pixel must differ across
        // channels, or we rasterized a monochrome outline by mistake.
        let chromatic = rgba
            .chunks_exact(4)
            .any(|p| p[3] > 0 && (p[0].abs_diff(p[1]) > 24 || p[1].abs_diff(p[2]) > 24));
        assert!(chromatic, "the emoji came back monochrome");
    }

    /// A variation selector has to be able to flip the presentation of the
    /// character before it — that is the entire purpose of VS15 and VS16, and
    /// it is the only way `❤️` can be red while a bare `❤` stays black.
    #[test]
    fn a_variation_selector_overrides_the_default_presentation() {
        assert_eq!(Presentation::of(Some('\u{FE0F}')), Presentation::Emoji);
        assert_eq!(Presentation::of(Some('\u{FE0E}')), Presentation::Text);
        assert_eq!(Presentation::of(None), Presentation::Default);
        assert_eq!(Presentation::of(Some('x')), Presentation::Default);

        // The heart is text by default, so only the explicit selector can ask
        // for its colour form; a grinning face is the mirror image.
        assert_eq!(D.want(0x2764), Want::Plain);
        assert_eq!(Presentation::Emoji.want(0x2764), Want::Colour);
        assert_eq!(D.want(0x1F600), Want::Colour);
        assert_eq!(Presentation::Text.want(0x1F600), Want::Plain);
    }

    /// The selector must survive the memo: asking for `❤` and `❤️` in either
    /// order has to give two different answers, not whichever came first.
    #[test]
    fn the_same_codepoint_resolves_differently_as_text_and_as_emoji() {
        let mut chain = Chain::new();
        let plain = chain.resolve(0x2764, Presentation::Text);
        let emoji = chain.resolve(0x2764, Presentation::Emoji);
        // Resolving them again must not change either answer.
        assert_eq!(chain.resolve(0x2764, Presentation::Text), plain);
        assert_eq!(chain.resolve(0x2764, Presentation::Emoji), emoji);
        if let (Some((i, gid)), Some(_)) = (plain, emoji) {
            let face = chain.face(i).expect("the face the heart resolved to");
            assert!(
                !face.has_colour(gid),
                "the text form of the heart resolved to colour art"
            );
        }
    }
}
