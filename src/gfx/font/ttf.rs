//! Phase 2 (M7): TrueType parser + anti-aliased scanline rasterizer.
//!
//! Tables: head, maxp, cmap (formats 4/12), loca, glyf (simple + composite),
//! hmtx, hhea. Later: kern/GPOS.
//!
//! This is still parser-only: the embedded bitmap atlas in `atlas` remains
//! the app's font source (phase 1), so nothing on top of `gfx::font` depends
//! on this module yet. The API mirrors `atlas::FontSystem` — `metrics`,
//! `advance`, `glyph` returning the shared `Glyph`/`Metrics` types with the
//! same conversions — so swapping implementations later is a one-line change
//! in `app.rs`. Keys and coverage mapping match the atlas cache (size in
//! 1/16 px steps, `COVERAGE_GAMMA` contrast), so both rasterizers must look
//! identical on screen.
//!
//! `Face::parse` copies the font bytes into the face; faces are only created
//! from tests right now. When the swap-in loads system fonts we can switch to
//! a `&'static [u8]`/owned-cache split — nothing else would change.
//!
//! Composite glyphs resolve per-component recursively (nesting ≤ 32), each
//! with its own translate + optional scale/matrix, which covers accented
//! Latin-1 letters in Liberation faces.
//!
//! Unit tests run against `assets/fonts/*.ttf` (Liberation, SIL OFL — see
//! `assets/FONT-LICENSE.txt`); those exact binaries are committed, so every
//! test run sees identical input.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::{Glyph, Metrics};

/// Same contrast curve the bitmap atlas applies (`atlas::COVERAGE_GAMMA`):
/// both rasterizers must look identical on screen.
const COVERAGE_GAMMA: f32 = 0.82;

/// Composite-glyph nesting limit; sane fonts stay under 4.
const MAX_COMPONENT_DEPTH: u32 = 32;

/// A simple glyph may not claim more points than this (Liberation tops out
/// around 140; hostile fonts hit hundreds of thousands).
const MAX_GLYF_POINTS: usize = 4_096;

/// Flatten quadratic curves into segments at most ~this many font units of
/// chord deviation (≈0.06 px at 16 px — invisible, and size-independent so
/// outlines can be cached per face).
const FLATTEN_TOL_UNITS: f32 = 8.0;

/// Vertical supersamples per pixel row: coverage bands in y are quantized to
/// these; horizontal coverage stays exact per sample.
const RASTER_SAMPLES: u32 = 4;

/// Hostile fonts can carry enormous coordinate deltas; cap the bitmap size
/// in each axis so a single glyph can never ask for an absurd allocation.
const MAX_BITMAP_DIM: usize = 4_096;

/// Ceiling on the face count a `.ttc` header may claim. Real collections are
/// well under this (macOS PingFang is ~10); a hostile count must not drive a
/// huge allocation.
const MAX_COLLECTION_FACES: u32 = 128;

#[derive(Debug)]
pub enum TtfError {
    /// Not a TrueType font (bad magic, truncated header).
    NotTrueType,
    /// Truncated or malformed table.
    Table(&'static str),
    /// Out-of-range glyph id or a glyph table inconsistency.
    Glyph(u16),
}

impl std::fmt::Display for TtfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotTrueType => write!(f, "not a TrueType font"),
            Self::Table(what) => write!(f, "malformed {what} table"),
            Self::Glyph(gid) => write!(f, "bad glyph id {gid}"),
        }
    }
}

impl std::error::Error for TtfError {}

// =============================================================== reader ====

/// Big-endian cursor with bounds-checked reads. Slices only; zero copies.
struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, p: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], TtfError> {
        let end = self.p.checked_add(n).ok_or(TtfError::Table("overrun"))?;
        if end > self.b.len() {
            return Err(TtfError::Table("overrun"));
        }
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, TtfError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, TtfError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn i16(&mut self) -> Result<i16, TtfError> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, TtfError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    /// Fixed 2.14 fraction, per the glyf composite spec.
    fn f2d14(&mut self) -> Result<f32, TtfError> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()) as f32 / 16384.0)
    }
}

/// Look up one table in the sfnt directory that starts at `dir`. `(0, 0)` when
/// absent — caller treats missing tables as empty/unusable, never as a crash.
///
/// `dir` is 0 for a plain `.ttf`. In a `.ttc` collection every member has its
/// own directory, but the table *offsets* inside it stay absolute to the file
/// start (that is how collections share tables), so the base cannot simply be
/// sliced away — it has to be threaded through like this.
pub(crate) fn table(data: &[u8], dir: u32, tag: &[u8; 4]) -> (u32, u32) {
    let dir = dir as usize;
    if dir >= data.len() {
        return (0, 0);
    }
    let mut r = Reader::new(&data[dir..]);
    if r.u32().is_err() {
        return (0, 0);
    }
    let Ok(n) = r.u16() else {
        return (0, 0);
    };
    if r.take(6).is_err() {
        return (0, 0); // searchRange, entrySelector, rangeShift
    }
    for _ in 0..n {
        // Entry layout: tag(4) checksum(4) offset(4) length(4).
        let Ok(t) = r.take(4) else {
            return (0, 0);
        };
        let Ok(_checksum) = r.u32() else {
            return (0, 0);
        };
        let (Ok(off), Ok(len)) = (r.u32(), r.u32()) else {
            return (0, 0);
        };
        if t == tag {
            if (off as usize).saturating_add(len as usize) > data.len() {
                return (0, 0);
            }
            return (off, len);
        }
    }
    (0, 0)
}

/// Table-directory offsets in a font file.
///
/// A plain sfnt yields `[0]`; a `.ttc` collection yields one offset per member,
/// which is what [`Face::parse_at`] expects. Only the directory offsets are
/// resolved — each face is parsed on demand, so a member that turns out to be
/// CFF-flavoured (OTTO) fails at `parse_at` rather than poisoning the whole
/// collection. This is what lets the fallback chain use system `.ttc` files
/// (macOS PingFang, Linux NotoSansCJK, Windows msyh).
pub fn faces(data: &[u8]) -> Result<Vec<u32>, TtfError> {
    if data.len() < 12 {
        return Err(TtfError::NotTrueType);
    }
    if &data[0..4] != b"ttcf" {
        // Not a collection: a single face, whose directory sits at offset 0.
        return Ok(vec![0]);
    }
    let mut r = Reader::new(data);
    r.take(4).map_err(|_| TtfError::Table("ttcf"))?; // 'ttcf'
    r.u32().map_err(|_| TtfError::Table("ttcf version"))?; // major+minor
    let n = r.u32().map_err(|_| TtfError::Table("ttcf numFonts"))?;
    if n == 0 || n > MAX_COLLECTION_FACES {
        return Err(TtfError::Table("ttcf numFonts"));
    }
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let off = r.u32().map_err(|_| TtfError::Table("ttcf offset"))?;
        if off as usize + 12 > data.len() {
            return Err(TtfError::Table("ttcf offset"));
        }
        out.push(off);
    }
    Ok(out)
}

// ============================================================ tables =======

fn parse_head(data: &[u8], dir: u32) -> Result<(f32, bool), TtfError> {
    let (off, len) = table(data, dir, b"head");
    if len < 54 {
        return Err(TtfError::Table("head"));
    }
    let mut r = Reader::new(&data[off as usize..(off + len) as usize]);
    r.take(18).map_err(|_| TtfError::Table("head"))?; // version..flags
    let upem = r.u16().map_err(|_| TtfError::Table("head"))?;
    r.take(30).map_err(|_| TtfError::Table("head"))?; // dates, bbox, macStyle…
    let loc_fmt = r.i16().map_err(|_| TtfError::Table("head"))?;
    Ok((upem as f32, loc_fmt == 0))
}

/// We only need `numGlyphs`; everything else is sized to it.
fn parse_maxp(data: &[u8], dir: u32) -> Result<u16, TtfError> {
    let (off, len) = table(data, dir, b"maxp");
    if len < 6 {
        return Err(TtfError::Table("maxp"));
    }
    let mut r = Reader::new(&data[off as usize..(off + len) as usize]);
    r.u32().map_err(|_| TtfError::Table("maxp"))?;
    r.u16().map_err(|_| TtfError::Table("maxp"))
}

fn parse_hhea(data: &[u8], dir: u32) -> Result<(f32, f32, u16), TtfError> {
    let (off, len) = table(data, dir, b"hhea");
    if len < 34 {
        return Err(TtfError::Table("hhea"));
    }
    let mut r = Reader::new(&data[off as usize..(off + len) as usize]);
    r.u32().map_err(|_| TtfError::Table("hhea"))?; // version
    let ascent = r.i16()?;
    let descent = r.i16()?;
    r.take(26).map_err(|_| TtfError::Table("hhea"))?; // lineGap..metricDataFormat
    let nhm = r.u16().map_err(|_| TtfError::Table("hhea"))?;
    Ok((ascent as f32, descent as f32, nhm))
}

/// The `loca` table: glyph → offset of its outline inside `glyf`.
///
/// Read *through* from the font bytes instead of copied into a `Vec<u32>`. A
/// CJK face carries tens of thousands of glyphs, so the copy costs ~4 bytes per
/// glyph per face (120 KB for Microsoft YaHei) to serve the few dozen a
/// document actually draws — and the bytes are already in the address space
/// (embedded faces live in the image, fallback fonts are mapped), so an entry
/// is one bounds-checked big-endian load away. Same table, same answers, no
/// per-face heap.
#[derive(Clone, Copy)]
struct Loca<'a> {
    /// The `loca` table itself, as it appears in the file.
    entries: &'a [u8],
    /// Short entries are 2-byte *halves* of the offset; long ones are 4 bytes.
    short: bool,
}

impl Loca<'_> {
    /// Offset of `gid`'s outline within `glyf`; `0` for an out-of-range index,
    /// which callers treat as an empty glyph rather than an error.
    fn get(&self, gid: usize) -> u32 {
        let width = if self.short { 2 } else { 4 };
        let at = gid * width;
        let Some(b) = self.entries.get(at..at + width) else {
            return 0;
        };
        if self.short {
            u16::from_be_bytes([b[0], b[1]]) as u32 * 2
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        }
    }
}

fn parse_loca(data: &[u8], dir: u32, short: bool, num_glyphs: u16) -> Result<Loca<'_>, TtfError> {
    let (off, len) = table(data, dir, b"loca");
    let need = num_glyphs as usize + 1;
    // Short loca stores 2-byte entries, long stores 4.
    let entry = if short { 2 } else { 4 };
    if (len as usize) < need * entry {
        return Err(TtfError::Table("loca"));
    }
    Ok(Loca {
        entries: &data[off as usize..off as usize + len as usize],
        short,
    })
}

/// The `hmtx` table: glyph → advance width, read through for the same reason as
/// [`Loca`] (the left-side bearings in it are never used, only the advances).
#[derive(Clone, Copy)]
struct Hmtx<'a> {
    entries: &'a [u8],
    /// Glyphs with a full 4-byte long metric; anything past them reuses the
    /// last one, which is what the format's trailing bearings mean.
    nhm: usize,
    num_glyphs: usize,
}

impl Hmtx<'_> {
    /// Advance width in font units; `0` where the original copy had no entry
    /// (an out-of-range glyph, or a face with no long metrics at all).
    fn advance(&self, gid: u16) -> u16 {
        let i = gid as usize;
        if i >= self.num_glyphs || self.nhm == 0 {
            return 0;
        }
        let idx = if i < self.nhm { i } else { self.nhm - 1 };
        match self.entries.get(idx * 4..idx * 4 + 2) {
            Some(b) => u16::from_be_bytes([b[0], b[1]]),
            None => 0,
        }
    }
}

fn parse_hmtx(data: &[u8], dir: u32, nhm: u16, num_glyphs: u16) -> Result<Hmtx<'_>, TtfError> {
    let (off, len) = table(data, dir, b"hmtx");
    let entries = &data[off as usize..off as usize + len as usize];
    // `numberOfHMetrics` long metrics, then a bearing per remaining glyph.
    // Validating the whole layout here keeps the on-demand reads below from
    // ever having to fail: a table too short for its own glyph count is a
    // malformed face, exactly as it was when the entries were copied out.
    let long = nhm.min(num_glyphs) as usize;
    let need = long * 4 + (num_glyphs as usize - long) * 2;
    if entries.len() < need {
        return Err(TtfError::Table("hmtx"));
    }
    Ok(Hmtx {
        entries,
        nhm: long,
        num_glyphs: num_glyphs as usize,
    })
}

// ============================================================== cmap =======

/// One glyph-id run, in whatever addressing the source subtable used.
struct Seg {
    start: u32,
    end: u32,
    /// gid = cp + delta (format-4 arithmetic, or format-12 startGlyphId −
    /// startCodePoint), for direct segments.
    delta: Option<i32>,
    /// Indirect segments (format-4 idRangeOffset): absolute byte offset of the
    /// first array slot, the lowest codepoint it indexes, and the segment's
    /// idDelta — which still applies to the slot's value (spec: "if the
    /// glyphIndex is not 0, add idDelta").
    indirect: Option<(u32, u32, i32)>,
}

struct Cmap<'a> {
    /// Segment runs in lookup order — the highest-coverage subtable first.
    ///
    /// Each run is sorted by `start`; a lookup walks the runs in turn and takes
    /// the first hit. Keeping them separate (rather than concatenating) is what
    /// lets a font whose format-12 subtable is partial still resolve the BMP
    /// from its format-4 one, without giving up the early `break` that keeps
    /// lookups fast.
    runs: Vec<Vec<Seg>>,
    /// The font buffer, for idRangeOffset indirection (borrowed so parsing
    /// never duplicates the font bytes).
    data: &'a [u8],
}

fn parse_cmap(data: &[u8], dir: u32) -> Result<Cmap<'_>, TtfError> {
    let (off, len) = table(data, dir, b"cmap");
    if len < 4 {
        return Err(TtfError::Table("cmap"));
    }
    let mut r = Reader::new(&data[off as usize..(off + len) as usize]);
    r.u16().map_err(|_| TtfError::Table("cmap"))?; // version
    let n = r.u16().map_err(|_| TtfError::Table("cmap"))?;

    // Rank candidates by *coverage*, not by platform id. A format-12 subtable
    // spans all of Unicode including the astral planes, so it is strictly the
    // better source; format 4 only reaches the BMP. Ranking the other way
    // round is why astral emoji stayed unmapped even in fonts that carry them:
    // every such font also ships a BMP subtable, and that one won.
    let mut ranked: Vec<(u8, u32, u16)> = Vec::new();
    for _ in 0..n {
        let pid = r.u16()?;
        let eid = r.u16()?;
        let sub = r.u32()?;
        let rank = match (pid, eid) {
            (3, 10) | (0, 4) | (0, 6) => 0, // Unicode, full repertoire
            (3, 1) | (0, 3) => 1,           // BMP only
            _ => continue,
        };
        // Subtable offsets are relative to the cmap table itself.
        let abs = off + sub;
        if abs as usize + 2 > data.len() {
            continue;
        }
        let mut sr = Reader::new(&data[abs as usize..]);
        let Ok(fmt) = sr.u16() else { continue };
        if fmt == 4 || fmt == 12 {
            ranked.push((rank, abs, fmt));
        }
    }
    ranked.sort_by_key(|&(rank, _, _)| rank);

    let mut runs: Vec<Vec<Seg>> = Vec::new();
    for &(_, sub, fmt) in &ranked {
        let parsed = match fmt {
            12 => parse_cmap12(data, sub),
            _ => parse_cmap4(data, sub),
        };
        // A malformed or *empty* subtable just does not contribute a run —
        // Nirmala ships a format-12 subtable with length 0, and it must not
        // shadow the format-4 one underneath it.
        let Ok(mut segs) = parsed else { continue };
        if segs.is_empty() {
            continue;
        }
        segs.sort_by_key(|s| s.start);
        runs.push(segs);
    }
    if runs.is_empty() {
        return Err(TtfError::Table("cmap"));
    }
    Ok(Cmap { runs, data })
}

impl Cmap<'_> {
    /// Codepoint -> glyph id; 0 = .notdef.
    fn gid(&self, cp: u32) -> u16 {
        for run in &self.runs {
            for s in run {
                if cp < s.start {
                    break; // sorted: nothing later in this run can map
                }
                if cp > s.end {
                    continue;
                }
                if let Some((garr, gstart, delta)) = s.indirect {
                    // Format-4 idRangeOffset: one big-endian u16 slot per
                    // codepoint, addressed relative to the slot's own location.
                    // A zero slot means "no glyph", not "glyph zero + delta".
                    let at = (garr + (cp - gstart) * 2) as usize;
                    if at + 2 > self.data.len() {
                        return 0;
                    }
                    let raw = u16::from_be_bytes([self.data[at], self.data[at + 1]]);
                    if raw == 0 {
                        return 0;
                    }
                    return (raw as i32 + delta) as u16;
                }
                return (cp as i32 + s.delta.unwrap_or(0)) as u16;
            }
        }
        0
    }
}

fn parse_cmap4(data: &[u8], sub: u32) -> Result<Vec<Seg>, TtfError> {
    let mut r = Reader::new(&data[sub as usize..]);
    r.u16()?; // format
    r.u16()?; // length
    r.u16()?; // language
    let seg_x2 = r.u16()?; // segCountX2
    let seg = seg_x2 / 2;
    r.take(6).map_err(|_| TtfError::Table("cmap4"))?; // searchRange..rangeShift

    let mut ends = Vec::with_capacity(seg as usize);
    for _ in 0..seg {
        ends.push(r.u16()?);
    }
    r.u16()?; // reservedPad
    let mut starts = Vec::with_capacity(seg as usize);
    for _ in 0..seg {
        starts.push(r.u16()?);
    }
    let mut deltas = Vec::with_capacity(seg as usize);
    for _ in 0..seg {
        deltas.push(r.i16()?);
    } // idRangeOffset entries start here; the glyph-id array hangs off them.
    let ros_off = sub + r.p as u32;
    let mut segs = Vec::with_capacity(seg as usize);
    for i in 0..seg as usize {
        let (s, e, d, ro) = (starts[i], ends[i], deltas[i], r.u16()?);
        if s == 0xFFFF && e == 0xFFFF {
            continue; // terminator segment
        }
        if ro == 0 {
            segs.push(Seg {
                start: s as u32,
                end: e as u32,
                // Format 4: gid = (cp + delta) mod 65536.
                delta: Some(d as i32),
                indirect: None,
            });
        } else {
            segs.push(Seg {
                start: s as u32,
                end: e as u32,
                delta: None,
                indirect: Some((ros_off + (2 * i as u32) + ro as u32, s as u32, d as i32)),
            });
        }
    }
    Ok(segs)
}

fn parse_cmap12(data: &[u8], sub: u32) -> Result<Vec<Seg>, TtfError> {
    let mut r = Reader::new(&data[sub as usize..]);
    r.u16()?; // format
    r.u16()?; // reserved
    r.u32()?; // length
    r.u32()?; // language
    let n = r.u32()?;
    if n > 20_000 {
        return Err(TtfError::Table("cmap12"));
    }
    let mut segs = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let s = r.u32()?;
        let e = r.u32()?;
        let g = r.u32()?;
        segs.push(Seg {
            start: s,
            end: e,
            delta: Some(g as i32 - s as i32),
            indirect: None,
        });
    }
    Ok(segs)
}

// =============================================================== face ======

/// One parsed face over borrowed font bytes, exposing metric + raster APIs
/// that mirror `atlas::FontSystem`. Borrowing keeps parse allocation-free:
/// the caller owns the font file (embedded bytes or a lazily-loaded file).
pub struct Face<'a> {
    data: &'a [u8],
    /// Absolute file offset of the `glyf` table (loca entries index it).
    glyf_off: u32,
    cmap: Cmap<'a>,
    loca: Loca<'a>,
    num_glyphs: u16,
    upem: f32,
    ascent: f32,
    descent: f32,
    hmtx: Hmtx<'a>,
    /// COLR v1 paint graphs, when this face has them. `None` for the common
    /// outline-only face, which is every face we embed.
    colr: Option<super::colr::Colr<'a>>,
    cache: RefCell<HashMap<CacheKey, Option<Rc<Glyph>>>>,
}

/// Key for the rasterized-glyph cache: glyph id, and pixel size in 1/16 px.
type CacheKey = (u16, u32);

/// A flattened outline contour: polygon vertices in font units, y-up.
pub(crate) struct Contour {
    pub(crate) pts: Vec<(f32, f32)>,
}

impl Contour {
    /// Polygon contour from em-space vertices, wound clockwise in y-up space.
    ///
    /// `shapes` builds every primitive through this so that overlapping
    /// contours keep a consistent winding — the non-zero fill rule then unions
    /// them instead of cancelling them out. Reversing the vertices is how a
    /// counter (a hole) is punched, which is what `shapes` does for outlines.
    pub(crate) fn poly(pts: Vec<(f32, f32)>) -> Self {
        Self { pts }
    }

    /// The same polygon wound the other way, so it subtracts from a filled
    /// area under the non-zero rule.
    pub(crate) fn reversed(pts: Vec<(f32, f32)>) -> Self {
        let mut pts = pts;
        pts.reverse();
        Self { pts }
    }
}

/// Rasterize em-space contours at `scale` pixels per em.
///
/// This is the seam `shapes` uses to draw procedural glyphs through the same
/// scanline rasterizer as the real faces, so fallbacks anti-alias and quantize
/// identically to font outlines. `None` when there is nothing to fill.
pub(crate) fn rasterize_outline(contours: &[Contour], scale: f32) -> Option<Glyph> {
    let edges = edges_of(contours, scale);
    if edges.is_empty() {
        return None;
    }
    Some(Face::rasterize(&edges))
}

impl Face<'_> {
    /// Parse a plain single-font `.ttf`/`.otf`-flavoured TrueType file.
    pub fn parse(data: &'static [u8]) -> Result<Self, TtfError> {
        Self::parse_at(data, 0)
    }

    /// Parse the face whose sfnt directory starts at `dir` — 0 for a plain
    /// file, or one of [`faces`]' offsets for a `.ttc` member.
    pub fn parse_at(data: &'static [u8], dir: u32) -> Result<Self, TtfError> {
        let at = dir as usize;
        if data.len() < at + 12 || data.get(at..at + 4) != Some(&[0, 1, 0, 0]) {
            // TrueType 1.0 open-type magic; CFF-flavored OTTO is out of scope.
            return Err(TtfError::NotTrueType);
        }
        let cmap = parse_cmap(data, dir)?;
        let (upem, short_loca) = parse_head(data, dir)?;
        if !(upem > 0.0 && upem.is_finite()) {
            return Err(TtfError::Table("head upem"));
        };
        let num_glyphs = parse_maxp(data, dir)?;
        let (ascent, descent, nhm) = parse_hhea(data, dir)?;
        let loca = parse_loca(data, dir, short_loca, num_glyphs)?;
        let hmtx = parse_hmtx(data, dir, nhm, num_glyphs)?;
        Ok(Self {
            data,
            glyf_off: table(data, dir, b"glyf").0,
            cmap,
            loca,
            num_glyphs,
            upem,
            ascent,
            descent,
            hmtx,
            colr: super::colr::Colr::parse(data, dir),
            cache: RefCell::new(HashMap::new()),
        })
    }

    /// Baseline metrics as fractions of the em, matching `atlas::Metrics`.
    pub fn metrics(&self) -> Metrics {
        Metrics {
            ascent: self.ascent / self.upem,
            descent: self.descent / self.upem,
        }
    }

    /// Glyph id for a codepoint; 0 is `.notdef`.
    pub fn glyph_id(&self, cp: u32) -> u16 {
        self.cmap.gid(cp)
    }

    /// Number of glyphs in the face; every valid glyph id is below this.
    pub fn num_glyphs(&self) -> u16 {
        self.num_glyphs
    }

    /// Design units per em, the denominator of every font-unit coordinate.
    pub(crate) fn upem(&self) -> f32 {
        self.upem
    }

    /// Whether this face can draw `gid` as COLR colour art.
    pub(crate) fn has_colour(&self, gid: u16) -> bool {
        self.colr.as_ref().is_some_and(|c| c.has(gid))
    }

    /// Flattened glyph outline in font units (y-up); empty for blank glyphs.
    /// The colour renderer transforms these itself, so it needs them raw.
    pub(crate) fn contours(&self, gid: u16) -> Vec<Contour> {
        self.outline(gid, 0).unwrap_or_default()
    }

    /// Advance width in font units.
    pub fn advance(&self, gid: u16) -> f32 {
        self.hmtx.advance(gid) as f32
    }

    /// Advance width in pixels at `size`, matching the atlas scaling.
    pub fn advance_px(&self, gid: u16, size: f32) -> f32 {
        self.advance(gid) * size / self.upem
    }

    // -- outline extraction ================================================

    /// Flatten glyph `gid` into contours of straight segments (font units,
    /// y-up). Composites resolve recursively with per-component transforms.
    /// Empty glyphs (space, .notdef) yield `Ok(vec![])`.
    fn outline(&self, gid: u16, depth: u32) -> Result<Vec<Contour>, TtfError> {
        if depth > MAX_COMPONENT_DEPTH || gid >= self.num_glyphs {
            return Err(TtfError::Glyph(gid));
        }
        let start = self.loca.get(gid as usize) as usize;
        let end = self.loca.get(gid as usize + 1) as usize;
        if end <= start {
            return Ok(vec![]);
        }
        // loca offsets are relative to the glyf table, not the file. A hostile
        // face can point past the file (or glyf may be missing entirely);
        // degrade to an empty glyph instead of an out-of-bounds slice.
        let abs_start = self.glyf_off as usize + start;
        let abs_end = (self.glyf_off as usize + end).min(self.data.len());
        if abs_start >= self.data.len() || abs_start >= abs_end {
            return Ok(vec![]);
        }
        let glyph_bytes = &self.data[abs_start..abs_end];
        let mut r = Reader::new(glyph_bytes);
        let n_contours = r.i16()?;
        if n_contours >= 0 {
            self.outline_simple(&mut r, n_contours as u16)
        } else {
            self.outline_composite(&mut r, depth)
        }
    }

    fn outline_simple(&self, r: &mut Reader, n_contours: u16) -> Result<Vec<Contour>, TtfError> {
        r.take(8).map_err(|_| TtfError::Glyph(0))?; // xMin..yMax bbox
        let mut ends = Vec::with_capacity(n_contours as usize);
        for _ in 0..n_contours {
            ends.push(r.u16()? as usize);
        }
        let n_points = ends.last().map_or(0, |&e| e + 1);
        if n_points > MAX_GLYF_POINTS {
            return Err(TtfError::Table("glyf points"));
        }
        let instr_len = r.u16()? as usize;
        r.take(instr_len).map_err(|_| TtfError::Glyph(0))?;
        let flags = read_flags(r, n_points)?;
        let xs = read_coords(r, n_points, &flags, true)?;
        let ys = read_coords(r, n_points, &flags, false)?;
        let pt = |i: usize| (xs[i], ys[i]);
        let on = |i: usize| flags[i] & 1 == 1;

        let mut contours = Vec::with_capacity(n_contours as usize);
        let mut start = 0;
        for &end in &ends {
            if end < start {
                return Err(TtfError::Glyph(u16::MAX));
            }
            contours.push(Self::flatten_contour(&pt, &on, start, end));
            start = end + 1;
        }
        Ok(contours)
    }

    /// Turn one closed TrueType point run into straight segments: on-curve
    /// points are vertices, off-curve points drive quadratic flattening with
    /// the implied-midpoint rule (two consecutive off-curve points imply an
    /// on-curve midpoint between them).
    fn flatten_contour(
        pt: &dyn Fn(usize) -> (f32, f32),
        on: &dyn Fn(usize) -> bool,
        start: usize,
        end: usize,
    ) -> Contour {
        let n = end - start + 1;
        // Contours with no on-curve point start at the midpoint of the first
        // two off-curve points (all-off contours are legal TrueType).
        let first_on = (0..n).find(|&i| on(start + i));
        let Some(first_on) = first_on else {
            let (p0, p1) = (pt(start), pt(start + 1));
            return Contour {
                pts: vec![((p0.0 + p1.0) / 2.0, (p0.1 + p1.1) / 2.0)],
            };
        };
        let origin = pt(start + first_on);
        let mut pts: Vec<(f32, f32)> = vec![origin];
        let mut prev: (f32, f32) = pts[0];
        let mut ctrl: Option<(f32, f32)> = None;
        for k in 1..=n {
            let i = start + (first_on + k) % n;
            let p = pt(i);
            match (ctrl, on(i)) {
                (None, true) => {
                    pts.push(p);
                    prev = p;
                }
                (None, false) => ctrl = Some(p),
                (Some(c), true) => {
                    Self::flatten_quad(&mut pts, prev, c, p);
                    pts.push(p);
                    prev = p;
                    ctrl = None;
                }
                (Some(c), false) => {
                    // Implied midpoint keeps the chain smooth.
                    let mid = ((c.0 + p.0) / 2.0, (c.1 + p.1) / 2.0);
                    Self::flatten_quad(&mut pts, prev, c, mid);
                    pts.push(mid);
                    prev = mid;
                    ctrl = Some(p);
                }
            }
        }
        // Close the loop; a dangling control curves back to the start vertex.
        if let Some(c) = ctrl {
            Self::flatten_quad(&mut pts, prev, c, origin);
        }
        Contour { pts }
    }

    /// Append segments that interpolate the quadratic bezier p0-c-p1 within
    /// `FLATTEN_TOL_UNITS` (p1 is pushed by the caller).
    fn flatten_quad(out: &mut Vec<(f32, f32)>, p0: (f32, f32), c: (f32, f32), p1: (f32, f32)) {
        let dev = ((c.0 - (p0.0 + p1.0) / 2.0).abs() + (c.1 - (p0.1 + p1.1) / 2.0).abs()) / 2.0;
        let n = ((dev / FLATTEN_TOL_UNITS).sqrt().ceil() as usize).clamp(1, 32);
        for k in 1..n {
            let t = k as f32 / n as f32;
            let u = 1.0 - t;
            out.push((
                u * u * p0.0 + 2.0 * t * u * c.0 + t * t * p1.0,
                u * u * p0.1 + 2.0 * t * u * c.1 + t * t * p1.1,
            ));
        }
    }
}

/// Point-flag decoding with RLE (TrueType packs runs of equal flags).
fn read_flags(r: &mut Reader, n: usize) -> Result<Vec<u8>, TtfError> {
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let f = r.u8()?;
        out.push(f);
        if f & 0x08 != 0 {
            let rep = r.u8()? as usize;
            for _ in 0..rep {
                out.push(f);
            }
        }
    }
    out.truncate(n);
    Ok(out)
}

/// Delta-coded coordinate decoding: x and y each have their own flag bits
/// (byte-vs-short, accumulate-vs-set, sign packing).
fn read_coords(r: &mut Reader, n: usize, flags: &[u8], x_axis: bool) -> Result<Vec<f32>, TtfError> {
    // X: bit 1 = short vector, bit 4 = same-or-positive. Y: bit 2 = short,
    // bit 5 = same-or-positive. Bit 3 (repeat) and bit 0 (on-curve) belong to
    // the flag reader, not here.
    let short_flag = if x_axis { 0x02 } else { 0x04 };
    let same_flag = if x_axis { 0x10 } else { 0x20 };
    let mut out = Vec::with_capacity(n);
    let mut cur: i32 = 0;
    for f in flags.iter().take(n) {
        if f & short_flag != 0 {
            // Short vector: the byte is an unsigned magnitude; the flag bit
            // carries the sign (set = positive, clear = negative). It is NOT
            // a signed 8-bit delta — negating the magnitude is required, or
            // every negative short delta is off by (256 - 2*d).
            let d = r.u8()? as i32;
            cur += if f & same_flag != 0 { d } else { -d };
        } else if f & same_flag == 0 {
            cur += r.i16()? as i32;
        }
        out.push(cur as f32);
    }
    Ok(out)
}

// =============================================================== face ======

impl<'a> Face<'a> {
    fn outline_composite(&self, r: &mut Reader, depth: u32) -> Result<Vec<Contour>, TtfError> {
        r.take(8).map_err(|_| TtfError::Glyph(0))?; // component bbox
        let mut contours: Vec<Contour> = Vec::new();
        loop {
            let flags = r.u16()?;
            let comp = r.u16()?; // glyph id of the component
            let (dx, dy) = if flags & 0x0001 != 0 {
                // ARG_1_AND_2_ARE_WORDS: i16 offsets in font units.
                (r.i16()? as f32, r.i16()? as f32)
            } else {
                // Signed bytes (or point indices for identical-component
                // matching; no font we ship uses that trick).
                (r.u8()? as i8 as f32, r.u8()? as i8 as f32)
            };
            let (xx, yx, xy, yy) = if flags & 0x0008 != 0 {
                let s = r.f2d14()?;
                (s, 0.0, 0.0, s)
            } else if flags & 0x0040 != 0 {
                (r.f2d14()?, 0.0, 0.0, r.f2d14()?)
            } else if flags & 0x0080 != 0 {
                let a = r.f2d14()?;
                let b = r.f2d14()?;
                let c = r.f2d14()?;
                let d = r.f2d14()?;
                (a, b, c, d)
            } else {
                (1.0, 0.0, 0.0, 1.0)
            };
            for mut sub in self.outline(comp, depth + 1)? {
                for p in &mut sub.pts {
                    let x = p.0 * xx + p.1 * xy + dx;
                    let y = p.0 * yx + p.1 * yy + dy;
                    *p = (x, y);
                }
                contours.push(sub);
            }
            if flags & 0x0020 == 0 {
                break; // MORE_COMPONENTS clear
            }
        }
        Ok(contours)
    }

    // -- rasterization =====================================================

    /// Rasterized glyph at `size` px (cached); `None` for blank glyphs.
    pub fn glyph(&self, ch: char, size: f32) -> Option<Rc<Glyph>> {
        self.glyph_by_id(self.glyph_id(ch as u32), size)
    }

    /// Rasterized glyph addressed by id rather than codepoint (cached).
    ///
    /// The fallback chain resolves a codepoint to a face once and then wants to
    /// address the glyph directly, and colour bitmaps are keyed by id too.
    pub fn glyph_by_id(&self, gid: u16, size: f32) -> Option<Rc<Glyph>> {
        let key = (gid, (size * 16.0).round() as u32);
        if let Some(g) = self.cache.borrow().get(&key) {
            return g.clone();
        }
        // A COLR base glyph is drawn from its paint graph. The `glyf` outline
        // it also carries is a stand-in for renderers that cannot read COLR —
        // for emoji faces it is a crude approximation, so colour wins.
        let g = self
            .colr
            .as_ref()
            .and_then(|c| c.render(self, gid, size))
            .map(Rc::new)
            .or_else(|| {
                let contours = match self.outline(gid, 0) {
                    Ok(c) if !c.is_empty() => c,
                    _ => return None,
                };
                let edges = edges_of(&contours, size / self.upem);
                (!edges.is_empty()).then(|| Rc::new(Self::rasterize(&edges)))
            });
        self.cache.borrow_mut().insert(key, g.clone());
        g
    }

    /// Scanline rasterizer: per pixel row, 4 vertical supersamples; each
    /// sample line cuts the edges, crossings are paired by non-zero winding
    /// into spans, and per-pixel coverage is the exact horizontal span width
    /// averaged over the samples. Font-ups must map to rows top-down, hence
    /// the y negation.
    fn rasterize(edges: &[Edge]) -> Glyph {
        let (min_x, min_y, max_x, max_y) = bounds(edges);
        let dx0 = (min_x.floor() as i32).clamp(-(MAX_BITMAP_DIM as i32), MAX_BITMAP_DIM as i32);
        let dy_top = (max_y
            .ceil()
            .clamp(-(MAX_BITMAP_DIM as f32), MAX_BITMAP_DIM as f32)) as i32;
        let dy_bot = min_y
            .floor()
            .clamp(-(MAX_BITMAP_DIM as f32), MAX_BITMAP_DIM as f32) as i32;
        let w = (((max_x.ceil() as i32).clamp(dx0, MAX_BITMAP_DIM as i32) - dx0).max(1)) as usize;
        let h = ((dy_top - dy_bot).clamp(1, MAX_BITMAP_DIM as i32)) as usize;
        let mut cov = vec![0.0f32; w * h];

        for j in 0..h {
            // Row j covers the y-up band [dy_top-(j+1), dy_top-j), translated
            // from screen-down by the baseline sitting at y = 0.
            let band_top = dy_top - j as i32;
            let band_bot = band_top - 1;
            for k in 0..RASTER_SAMPLES {
                let y = band_bot as f32 + (k as f32 + 0.5) / RASTER_SAMPLES as f32;
                let mut xs: Vec<(f32, i8)> = edges
                    .iter()
                    // Half-open span [y0, y1): a sample exactly on an edge's
                    // top end must still register its crossing, or slanted
                    // outlines thin out into never-quite-solid areas.
                    .filter(|e| y >= e.y0 && y <= e.y1)
                    .map(|e| (e.x0 + e.slope * (y - e.y0), e.dir))
                    .collect();
                // A total ordering: malformed outlines can carry NaN
                // coordinates, and `partial_cmp().unwrap()` would panic on
                // them mid-rasterize. `total_cmp` orders NaN somewhere
                // deterministic instead; garbage spans are degenerate and
                // contribute nothing usable, which is exactly the
                // degrade-don't-crash behaviour the rest of the parser
                // gives bad input.
                xs.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut wind = 0i32;
                let mut span_start = 0.0f32;
                for (x, dir) in xs {
                    if wind == 0 {
                        span_start = x;
                    }
                    wind += dir as i32;
                    if wind == 0 && x > span_start {
                        Self::add_span(&mut cov, w, j, span_start, x, dx0);
                    } else if wind == 0 {
                        // degenerate span (zero width) — nothing to do
                    }
                }
            }
        }
        // Quantize: clamp + gamma, matching the bitmap atlas exactly.
        // `add_span` already applied the per-sample weight, so `v` is in 0..1.
        let coverage: Vec<u8> = cov
            .iter()
            .map(|&v| {
                let n = v.clamp(0.0, 1.0);
                ((n.powf(COVERAGE_GAMMA) * 255.0) + 0.5) as u8
            })
            .collect();
        Glyph {
            width: w,
            height: h,
            left: dx0,
            top: dy_top,
            coverage,
            color: None,
            fg: None,
        }
    }

    /// Add horizontal span [lo, hi) to row `j`'s coverage (1/S weight).
    fn add_span(cov: &mut [f32], w: usize, j: usize, lo: f32, hi: f32, dx0: i32) {
        let weight = 1.0 / RASTER_SAMPLES as f32;
        let col_lo = ((lo - dx0 as f32).floor() as i32).max(0) as usize;
        let col_hi = ((hi - dx0 as f32).ceil() as i32).min(w as i32) as usize;
        for c in col_lo..col_hi.min(w) {
            let l = (lo - dx0 as f32).max(c as f32);
            let r = (hi - dx0 as f32).min(c as f32 + 1.0);
            if r > l {
                cov[j * w + c] += (r - l) * weight;
            }
        }
    }

    // -- caching-native shortcuts ==========================================
}

struct Edge {
    /// Crossing x at `y0` and the slope dx/dy (y-up, y0 < y1).
    x0: f32,
    y0: f32,
    y1: f32,
    slope: f32,
    /// Winding direction (+1 up, −1 down), kept through normalization.
    dir: i8,
}

fn edges_of(contours: &[Contour], scale: f32) -> Vec<Edge> {
    let mut out = Vec::new();
    for c in contours {
        if c.pts.len() < 3 {
            continue;
        }
        for w in 0..c.pts.len() {
            let (px, py) = c.pts[w];
            let (qx, qy) = c.pts[(w + 1) % c.pts.len()];
            let (sx, sy) = (px * scale, py * scale);
            let (ex, ey) = (qx * scale, qy * scale);
            if (ey - sy).abs() < f32::EPSILON {
                continue; // horizontal: no crossings
            }
            let (up, e0, e1) = if sy < ey {
                (true, sy, ey)
            } else {
                (false, ey, sy)
            };
            let x_at = if up { sx } else { ex };
            let x_other = if up { ex } else { sx };
            out.push(Edge {
                x0: x_at,
                y0: e0,
                y1: e1,
                slope: (x_other - x_at) / (e1 - e0),
                dir: if up { 1 } else { -1 },
            });
        }
    }
    out
}

fn bounds(edges: &[Edge]) -> (f32, f32, f32, f32) {
    // Empty input falls back to the origin rather than NaN-going-wild.
    if edges.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let mut m = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for e in edges {
        let (xa, xb) = if e.slope >= 0.0 {
            (e.x0, e.x0 + e.slope * (e.y1 - e.y0))
        } else {
            (e.x0 + e.slope * (e.y1 - e.y0), e.x0)
        };
        m.0 = m.0.min(xa);
        m.1 = m.1.min(e.y0);
        m.2 = m.2.max(xb);
        m.3 = m.3.max(e.y1);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(name: &str) -> Face<'static> {
        let path = format!("assets/fonts/{name}.ttf");
        let data = std::fs::read(&path).unwrap_or_else(|e| panic!("missing {path}: {e}"));
        // Test-only leak: the parsed face borrows the font bytes, so a handful
        // of static font buffers outlive every test (dies_with tests inline).
        Face::parse(Box::leak(data.into_boxed_slice())).unwrap()
    }

    #[test]
    fn parses_all_shipped_faces() {
        for name in [
            "LiberationSans-Regular",
            "LiberationSans-Bold",
            "LiberationSans-Italic",
            "LiberationSans-BoldItalic",
            "LiberationMono-Regular",
        ] {
            let f = load(name);
            assert!(f.num_glyphs > 1000, "{name}: {}", f.num_glyphs);
            assert_eq!(f.upem, 2048.0, "{name}");
        }
    }

    /// Wrap the committed Liberation bytes in a synthetic one-face `.ttc`, so
    /// the collection path is exercised without depending on system fonts.
    ///
    /// A collection's table offsets stay absolute to the *file* start (that is
    /// how members share tables), so the embedded copy's directory must be
    /// rebased by the header size — the exact invariant `parse_at` relies on.
    fn synthetic_ttc(name: &str) -> Vec<u8> {
        const HDR: usize = 16; // 'ttcf' + version + numFonts + one offset
        let mut src = std::fs::read(format!("assets/fonts/{name}.ttf")).unwrap();
        let n = u16::from_be_bytes([src[4], src[5]]) as usize;
        for i in 0..n {
            let e = 12 + i * 16 + 8; // table entry's offset field
            let off = u32::from_be_bytes(src[e..e + 4].try_into().unwrap());
            src[e..e + 4].copy_from_slice(&(off + HDR as u32).to_be_bytes());
        }
        let mut out = Vec::with_capacity(HDR + src.len());
        out.extend_from_slice(b"ttcf");
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&(HDR as u32).to_be_bytes());
        out.extend_from_slice(&src);
        out
    }

    #[test]
    fn plain_files_report_one_face_at_offset_zero() {
        let data = std::fs::read("assets/fonts/LiberationSans-Regular.ttf").unwrap();
        assert_eq!(faces(&data).unwrap(), vec![0]);
    }

    #[test]
    fn collection_headers_resolve_to_their_member_directories() {
        let ttc = synthetic_ttc("LiberationSans-Regular");
        let dirs = faces(&ttc).unwrap();
        assert_eq!(dirs, vec![16]);
        let leaked: &'static [u8] = Box::leak(ttc.into_boxed_slice());
        // Offset 0 holds the 'ttcf' header, not a face.
        assert!(matches!(
            Face::parse_at(leaked, 0),
            Err(TtfError::NotTrueType)
        ));
        let f = Face::parse_at(leaked, dirs[0]).unwrap();
        assert_eq!(f.upem, 2048.0);
        assert!(f.num_glyphs > 1000);
    }

    /// Guards the rebasing: un-rebased offsets would leave `loca`/`glyf`
    /// pointing at the wrong bytes, so the outline would come out empty.
    #[test]
    fn a_collection_member_renders_the_same_glyphs_as_the_plain_file() {
        let plain = load("LiberationSans-Regular");
        let ttc = synthetic_ttc("LiberationSans-Regular");
        let leaked: &'static [u8] = Box::leak(ttc.into_boxed_slice());
        let member = Face::parse_at(leaked, 16).unwrap();
        let gid = plain.glyph_id('H' as u32);
        assert_ne!(gid, 0);
        assert_eq!(member.glyph_id('H' as u32), gid);
        assert_eq!(member.advance_px(gid, 32.0), plain.advance_px(gid, 32.0));
        let a = plain.glyph('H', 32.0).expect("plain H");
        let b = member.glyph('H', 32.0).expect("collection H");
        assert_eq!(
            (a.width, a.height, a.left, a.top),
            (b.width, b.height, b.left, b.top)
        );
        assert_eq!(a.coverage, b.coverage, "outline differs across containers");
    }

    /// Minimal format-4 subtable: one indirect (idRangeOffset) segment over
    /// U+0041..U+0043 plus the terminator segment.
    fn indirect_cmap(slots: [u16; 3], delta: i16) -> Vec<u8> {
        fn push16(b: &mut Vec<u8>, v: u16) {
            b.extend_from_slice(&v.to_be_bytes());
        }
        let mut b = Vec::new();
        push16(&mut b, 4); // format
        push16(&mut b, 0); // length (unvalidated on this path)
        push16(&mut b, 0); // language
        push16(&mut b, 4); // segCountX2 -> 2 segments
        push16(&mut b, 4); // searchRange
        push16(&mut b, 1); // entrySelector
        push16(&mut b, 0); // rangeShift
        push16(&mut b, 0x0043); // endCode[0]
        push16(&mut b, 0xFFFF); // endCode[1]
        push16(&mut b, 0); // reservedPad
        push16(&mut b, 0x0041); // startCode[0]
        push16(&mut b, 0xFFFF); // startCode[1]
        b.extend_from_slice(&delta.to_be_bytes()); // idDelta[0]
        b.extend_from_slice(&1i16.to_be_bytes()); // idDelta[1]
                                                  // idRangeOffset[0] = 4: the slot array begins 4 bytes past its own
                                                  // address (byte 28 -> byte 32). Terminator stays direct.
        push16(&mut b, 4);
        push16(&mut b, 0);
        for s in slots {
            push16(&mut b, s);
        }
        b
    }

    /// Regression: the idRangeOffset slot array was read little-endian, so
    /// every indirect format-4 mapping came back byte-swapped (0x0681 read as
    /// 0x8106). System fonts lean on indirect segments heavily — Nirmala and
    /// Microsoft YaHei both did — while Liberation does not use them at all,
    /// which is why this survived until `.ttc` support exercised them.
    #[test]
    fn indirect_cmap_slots_are_read_big_endian() {
        let data = indirect_cmap([0x1234, 0x0000, 0x0005], 0);
        let segs = parse_cmap4(&data, 0).unwrap();
        let cmap = Cmap {
            runs: vec![segs],
            data: &data,
        };
        assert_eq!(cmap.gid(0x41), 0x1234, "slot read with swapped bytes");
        assert_eq!(cmap.gid(0x43), 0x0005);
    }

    /// Regression: a zero slot means "no glyph", and per spec idDelta still
    /// applies to non-zero slots.
    #[test]
    fn indirect_cmap_applies_id_delta_and_treats_zero_as_missing() {
        let data = indirect_cmap([0x1234, 0x0000, 0x0005], 2);
        let segs = parse_cmap4(&data, 0).unwrap();
        let cmap = Cmap {
            runs: vec![segs],
            data: &data,
        };
        assert_eq!(cmap.gid(0x41), 0x1236, "idDelta not added to slot value");
        assert_eq!(cmap.gid(0x42), 0, "a zero slot must stay .notdef");
        assert_eq!(cmap.gid(0x43), 0x0007);
        // Below the segment start there is no mapping at all.
        assert_eq!(cmap.gid(0x40), 0);
    }

    /// A synthetic sfnt carrying one `cmap` with *both* a format-4 subtable
    /// (BMP only) and a format-12 one (astral) — the shape every real emoji
    /// font has.
    ///
    /// Format 4 maps U+0041 -> gid 5; format 12 maps U+1F600 -> gid 7 and
    /// nothing else, so each subtable covers exactly what the other does not.
    fn cmap_with_both_formats() -> Vec<u8> {
        fn p16(b: &mut Vec<u8>, v: u16) {
            b.extend_from_slice(&v.to_be_bytes());
        }
        fn p32(b: &mut Vec<u8>, v: u32) {
            b.extend_from_slice(&v.to_be_bytes());
        }
        const CMAP_OFF: u32 = 28; // sfnt header (12) + one directory entry (16)

        let mut cmap = Vec::new();
        p16(&mut cmap, 0); // version
        p16(&mut cmap, 2); // numTables
        p16(&mut cmap, 3);
        p16(&mut cmap, 1);
        p32(&mut cmap, 20); // record 0 -> format 4
        p16(&mut cmap, 3);
        p16(&mut cmap, 10);
        p32(&mut cmap, 52); // record 1 -> format 12
                            // -- format 4 (BMP): U+0041 -> gid 5, plus the terminator segment.
        p16(&mut cmap, 4);
        p16(&mut cmap, 32);
        p16(&mut cmap, 0);
        p16(&mut cmap, 4);
        p16(&mut cmap, 4);
        p16(&mut cmap, 1);
        p16(&mut cmap, 0);
        p16(&mut cmap, 0x0041);
        p16(&mut cmap, 0xFFFF); // endCode
        p16(&mut cmap, 0); // reservedPad
        p16(&mut cmap, 0x0041);
        p16(&mut cmap, 0xFFFF); // startCode
        cmap.extend_from_slice(&(-60i16).to_be_bytes()); // idDelta: 0x41 -> 5
        cmap.extend_from_slice(&1i16.to_be_bytes());
        p16(&mut cmap, 0);
        p16(&mut cmap, 0); // idRangeOffset
        assert_eq!(cmap.len(), 52, "format 4 must land at the planned offset");
        // -- format 12 (astral): U+1F600 -> gid 7.
        p16(&mut cmap, 12);
        p16(&mut cmap, 0);
        p32(&mut cmap, 28); // length: header 16 + one 12-byte group
        p32(&mut cmap, 0); // language
        p32(&mut cmap, 1); // nGroups
        p32(&mut cmap, 0x1F600);
        p32(&mut cmap, 0x1F600);
        p32(&mut cmap, 7);

        let mut f = Vec::new();
        p32(&mut f, 0x0001_0000); // sfnt version
        p16(&mut f, 1); // numTables
        p16(&mut f, 0);
        p16(&mut f, 0);
        p16(&mut f, 0); // searchRange, entrySelector, rangeShift
        f.extend_from_slice(b"cmap");
        p32(&mut f, 0); // checksum (never validated)
        p32(&mut f, CMAP_OFF);
        p32(&mut f, cmap.len() as u32);
        assert_eq!(f.len(), CMAP_OFF as usize);
        f.extend_from_slice(&cmap);
        f
    }

    /// Regression: the subtable chooser kept the *lowest*-ranked subtable, so
    /// a font's BMP-only format-4 table always beat its format-12 one — and
    /// astral emoji, which only format 12 reaches, came back as `.notdef`.
    /// Every real emoji font ships both, which is why the whole emoji block
    /// rendered as tofu even though the glyphs were right there.
    #[test]
    fn format_12_is_preferred_so_astral_codepoints_resolve() {
        let data = cmap_with_both_formats();
        let cmap = parse_cmap(&data, 0).unwrap();
        assert_eq!(cmap.gid(0x1F600), 7, "astral codepoint must use format 12");
        // The format-4 run is still consulted where format 12 has no answer,
        // so a partial format-12 subtable cannot cost the font its BMP.
        assert_eq!(cmap.gid(0x0041), 5, "BMP fallback run not consulted");
        assert_eq!(cmap.gid(0x1F601), 0, "unmapped astral codepoint");
        assert_eq!(cmap.gid(0x0042), 0, "unmapped BMP codepoint");
    }

    #[test]
    fn a_hostile_collection_header_is_rejected() {
        let mut d = b"ttcf".to_vec();
        d.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        d.extend_from_slice(&9999u32.to_be_bytes()); // absurd face count
        assert!(matches!(faces(&d), Err(TtfError::Table(_))));
        let mut zero = b"ttcf".to_vec();
        zero.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        zero.extend_from_slice(&0u32.to_be_bytes());
        assert!(matches!(faces(&zero), Err(TtfError::Table(_))));
        // Truncated input stays an error rather than a panic.
        assert!(faces(&zero[..6]).is_err());
    }

    #[test]
    fn cmap_maps_the_glyph_set() {
        let f = load("LiberationSans-Regular");
        assert_ne!(f.glyph_id(b'A' as u32), 0);
        assert_ne!(f.glyph_id(b'z' as u32), 0);
        assert_ne!(f.glyph_id(0x2019), 0, "smart quote must map");
        // Distinct letters map to distinct ids (real cmap, not a stub).
        assert_ne!(f.glyph_id(b'A' as u32), f.glyph_id(b'B' as u32));
        // Beyond Liberation's coverage: .notdef, not a crash.
        assert_eq!(f.glyph_id(0x4E2D), 0, "no CJK in Liberation");
        assert_eq!(f.glyph_id(0x10_0000), 0, "astral beyond fmt-4");
        let mono = load("LiberationMono-Regular");
        assert_ne!(mono.glyph_id(b'A' as u32), 0);
    }

    #[test]
    fn advances_match_em_metrics() {
        let f = load("LiberationSans-Regular");
        // 'W' is several times wider than 'i' in a proportional face.
        let (w, i) = (
            f.advance_px(f.glyph_id(b'W' as u32), 16.0),
            f.advance_px(f.glyph_id(b'i' as u32), 16.0),
        );
        assert!(w > 2.0 * i, "W {w} vs i {i}");
        // Full em at any size scales linearly.
        assert!((f.advance_px(f.glyph_id(b'W' as u32), 32.0) - 2.0 * w).abs() < 1e-3);
        let mono = load("LiberationMono-Regular");
        let a = mono.advance(mono.glyph_id(b'i' as u32));
        let b = mono.advance(mono.glyph_id(b'W' as u32));
        assert!((a - b).abs() < 1e-3, "mono advances must match: {a} vs {b}");
    }

    #[test]
    fn outline_shape_invariants() {
        let f = load("LiberationSans-Regular");
        // Space is an empty glyph (no contours).
        assert!(f.outline(f.glyph_id(b' ' as u32), 0).unwrap().is_empty());
        // 'O' has an outer ring and a counter.
        let o = f.outline(f.glyph_id(b'O' as u32), 0).unwrap();
        assert_eq!(o.len(), 2);
        assert!(o.iter().all(|c| c.pts.len() >= 3));
        // A dot has exactly one contour.
        assert_eq!(f.outline(f.glyph_id(b'.' as u32), 0).unwrap().len(), 1);
    }

    #[test]
    fn accented_resolves() {
        // é resolves via cmap, whether it's simple or composite internally.
        let f = load("LiberationSans-Regular");
        let g = f.glyph('é', 24.0).expect("é must rasterize");
        assert!(g.coverage.iter().any(|&v| v > 200));
    }

    #[test]
    fn rasterizes_x() {
        let f = load("LiberationSans-Regular");
        let g = f.glyph('X', 24.0).expect("X must rasterize");
        assert!(g.width > 4 && g.height > 4, "{}x{}", g.width, g.height);
        // Solid cores and anti-aliased edges both present.
        assert!(g.coverage.iter().any(|&v| v > 240));
        assert!(g.coverage.iter().any(|&v| v > 0 && v < 240));
        assert!(g.top > 12 && g.top < 24, "top={}", g.top);
        assert!(g.left.abs() < 4, "left={}", g.left);
    }

    #[test]
    fn raster_scale_cavity() {
        // Small pixel size: coverage grid stays bounded and nonempty.
        let f = load("LiberationSans-Regular");
        let g = f.glyph('m', 10.0).unwrap();
        assert!(g.coverage.iter().any(|&v| v > 0));
        assert!(g.coverage.len() < 400, "{}", g.coverage.len());
    }

    #[test]
    fn space_is_none() {
        let f = load("LiberationSans-Regular");
        assert!(f.glyph(' ', 24.0).is_none());
    }

    #[test]
    fn metrics_are_sane() {
        let f = load("LiberationSans-Regular");
        let m = f.metrics();
        assert!(m.ascent > 0.8 && m.ascent < 1.2, "ascent={}", m.ascent);
        assert!(
            m.descent < -0.1 && m.descent > -0.5,
            "descent={}",
            m.descent
        );
        assert!((f.upem - 2048.0).abs() < 1.0);
    }

    #[test]
    fn short_negative_delta_negates_the_magnitude() {
        // Regression: a short-vector byte is an *unsigned magnitude* and the
        // flag bit carries the sign. Reading it as a signed i8 (byte - 0x100)
        // corrupts every negative short delta. Three points: +100, -30, +7.
        let bytes = [100u8, 30u8, 7u8];
        let mut r = Reader::new(&bytes);
        let flags = [0x02 | 0x10, 0x02, 0x02 | 0x10];
        let xs = read_coords(&mut r, 3, &flags, true).unwrap();
        assert_eq!(xs, vec![100.0, 70.0, 77.0]);
    }

    #[test]
    fn outline_decodes_negative_short_deltas() {
        // Regression (the "garbage font" bug): 'H' uses a negative short delta
        // at point 4 (dx = -191 stored as byte 0xBF). Decoding it as signed i8
        // gives -65 and shifts every later point by +126, so the whole UI
        // rendered as smeared garbage. Pin the exact outline.
        let f = load("LiberationSans-Regular");
        let c = f.outline(f.glyph_id(b'H' as u32), 0).unwrap();
        assert_eq!(c.len(), 1);
        let xs: Vec<f32> = c[0].pts.iter().map(|p| p.0).collect();
        assert_eq!(
            xs,
            vec![
                1121.0, 1121.0, 359.0, 359.0, 168.0, 168.0, 359.0, 359.0, 1121.0, 1121.0, 1312.0,
                1312.0, 1121.0
            ]
        );
    }

    #[test]
    fn zz_probe_x_contour() {
        let f = load("LiberationSans-Regular");
        let gid = f.glyph_id(b'X' as u32);
        println!("X gid={gid}");
        for (ci, c) in f.outline(gid, 0).unwrap().iter().enumerate() {
            println!("contour {ci}: {} pts", c.pts.len());
            for (i, &p) in c.pts.iter().take(8).enumerate() {
                println!("  [{i}] ({:.1}, {:.1})", p.0, p.1);
            }
        }
        let scale = 24.0 / f.upem;
        let edges = edges_of(&f.outline(gid, 0).unwrap(), scale);
        println!("{} edges; first:", edges.len());
        for e in edges.iter().take(4) {
            println!(
                "  x0={:.3} y0={:.3} y1={:.3} slope={:.3} dir={}",
                e.x0, e.y0, e.y1, e.slope, e.dir
            );
        }
        let g = f.glyph('X', 24.0).unwrap();
        println!("raster {}x{} top={}", g.width, g.height, g.top);
        for j in 0..g.height {
            let row: String = g.coverage[j * g.width..(j + 1) * g.width]
                .iter()
                .map(|&v| {
                    if v == 0 {
                        '.'
                    } else if v > 200 {
                        '#'
                    } else {
                        '+'
                    }
                })
                .collect();
            println!("{row}");
        }
    }

    #[test]
    fn zz_probe_step_by_step() {
        let data = std::fs::read("assets/fonts/LiberationSans-Regular.ttf").unwrap();
        let (off, len) = table(&data, 0, b"cmap");
        println!("table cmap -> ({off}, {len})");
        match parse_cmap(&data, 0) {
            Ok(c) => {
                let segs: usize = c.runs.iter().map(|r| r.len()).sum();
                println!("cmap OK, {segs} segs; 'A' -> gid {}", c.gid(65));
            }
            Err(e) => println!("cmap ERR {e:?}"),
        }
        match parse_head(&data, 0) {
            Ok((upem, short)) => println!("head OK upem={upem} short_loca={short}"),
            Err(e) => println!("head ERR {e:?}"),
        }
        match parse_maxp(&data, 0) {
            Ok(n) => println!("maxp OK glyphs={n}"),
            Err(e) => println!("maxp ERR {e:?}"),
        }
        match parse_hhea(&data, 0) {
            Ok(v) => println!("hhea OK {v:?}"),
            Err(e) => println!("hhea ERR {e:?}"),
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(
            Face::parse(b"nope short thing"),
            Err(TtfError::NotTrueType)
        ));
        // Empty cache key must be cheap: gid + 1/16-px size = 8 bytes.
        assert_eq!(std::mem::size_of::<CacheKey>(), 8);
    }

    #[test]
    fn fuzz_byte_truncations_never_panic() {
        let full: &'static [u8] = Box::leak(
            std::fs::read("assets/fonts/LiberationSans-Regular.ttf")
                .unwrap()
                .into_boxed_slice(),
        );
        // Truncate every power-of-two prefix (plus last-100 bytes); each
        // shorter prefix is a worst case for table-boundary math.
        let mut n = 1usize;
        while n <= full.len() {
            if let Ok(f) = Face::parse(&full[..n]) {
                // Whatever survives parsing must be safe to rasterize.
                let _ = f.glyph('A', 16.0);
                let _ = f.glyph('X', 24.0);
                let _ = f.glyph_id(0x2019);
            }
            n = n.saturating_mul(2);
        }
        if let Ok(f) = Face::parse(&full[..full.len().saturating_sub(100)]) {
            let _ = f.glyph('W', 16.0);
        }
    }

    fn leak(v: Vec<u8>) -> &'static mut [u8] {
        Box::leak(v.into_boxed_slice())
    }

    #[test]
    fn fuzz_single_byte_corruption_never_panics() {
        let full: &'static [u8] = Box::leak(
            std::fs::read("assets/fonts/LiberationSans-Regular.ttf")
                .unwrap()
                .into_boxed_slice(),
        );
        // Corrupt one byte at each structurally interesting spot: header
        // fields, table-directory entries, and a few mid-table offsets.
        let mut spots = Vec::new();
        for k in 0..12 {
            spots.push(k);
        }
        let n_tables = u16::from_be_bytes([full[4], full[5]]) as usize;
        for t in 0..n_tables.clamp(1, 16) {
            let base = 12 + t * 16;
            if base + 16 <= full.len() {
                for d in [0usize, 4, 8, 12, 15] {
                    spots.push(base + d);
                }
            }
        }
        for s in spots {
            if s >= full.len() {
                continue;
            }
            let mut bad = full.to_vec();
            bad[s] = bad[s].wrapping_add(0xA5);
            if let Ok(f) = Face::parse(leak(bad)) {
                let _ = f.glyph('A', 16.0);
                let _ = f.glyph('é', 24.0);
            }
        }
    }

    #[test]
    fn hostile_zero_upem_yields_error_not_nan() {
        let full = std::fs::read("assets/fonts/LiberationSans-Regular.ttf").unwrap();
        // Zero head.unitsPerEm (a hostile font): parse must reject rather
        // than later divide by zero into NaN metrics.
        let mut bad = full.to_vec();
        bad[334] = 0;
        bad[335] = 0;
        assert!(matches!(Face::parse(leak(bad)), Err(TtfError::Table(_))));
    }

    #[test]
    fn hostile_glyph_returns_empty_or_error_never_panic() {
        let f = load("LiberationSans-Regular");
        // Out-of-range gids: loca/outline paths must not read wild memory.
        assert!(matches!(f.outline(u16::MAX, 0), Err(TtfError::Glyph(_))));
        assert!(matches!(f.outline(50_000, 0), Err(TtfError::Glyph(_))));
        // Deep composite recursion self-terminates at MAX_COMPONENT_DEPTH.
        let _ = f.outline(f.glyph_id(b'A' as u32), MAX_COMPONENT_DEPTH + 1);
    }

    /// Real system collections, where the platform has them.
    ///
    /// Synthetic fixtures cannot catch "this font uses a cmap feature we
    /// mishandle", which is exactly how the `idRangeOffset` byte-order bug hid
    /// for so long: it only shows up on fonts that use indirect segments.
    /// So when a system collection is present, parse every face and assert the
    /// invariant that actually broke — a mapped glyph id must be in range.
    ///
    /// Absent fonts are skipped rather than failed, so a bare CI box stays
    /// green and this still runs on a developer machine.
    #[test]
    fn system_fonts_map_glyphs_in_range() {
        let candidates: &[(&str, char)] = if cfg!(target_os = "windows") {
            &[
                ("C:/Windows/Fonts/cambria.ttc", 'A'),
                ("C:/Windows/Fonts/msyh.ttc", '\u{4E2D}'),
                ("C:/Windows/Fonts/Nirmala.ttc", '\u{0C05}'),
                ("C:/Windows/Fonts/YuGothM.ttc", '\u{3042}'),
            ]
        } else if cfg!(target_os = "macos") {
            &[
                ("/System/Library/Fonts/Apple Symbols.ttf", '\u{2603}'),
                ("/System/Library/Fonts/PingFang.ttc", '\u{4E2D}'),
            ]
        } else {
            &[
                (
                    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                    '\u{2192}',
                ),
                (
                    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
                    '\u{4E2D}',
                ),
            ]
        };
        for (path, probe) in candidates {
            let Ok(raw) = std::fs::read(path) else {
                continue;
            };
            let data: &'static [u8] = Box::leak(raw.into_boxed_slice());
            let Ok(dirs) = faces(data) else {
                continue;
            };
            for &dir in &dirs {
                // A CFF-flavoured member is out of scope, not a failure.
                let Ok(f) = Face::parse_at(data, dir) else {
                    continue;
                };
                let gid = f.glyph_id(*probe as u32);
                assert!(
                    gid < f.num_glyphs,
                    "{path} face@{dir}: U+{:04X} -> gid {gid} but the face has only {} glyphs",
                    *probe as u32,
                    f.num_glyphs
                );
            }
        }
    }
}
