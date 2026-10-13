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

#[derive(Debug)]
#[allow(clippy::enum_variant_names)] // BadMagic/BadTable/BadGlyph shape the message; rename would read worse.
pub enum TtfError {
    /// Not a TrueType font (bad magic, truncated header).
    BadMagic,
    /// Truncated or malformed table.
    BadTable(&'static str),
    /// Out-of-range glyph id or a glyph table inconsistency.
    BadGlyph(u16),
}

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
        let end = self.p.checked_add(n).ok_or(TtfError::BadTable("overrun"))?;
        if end > self.b.len() {
            return Err(TtfError::BadTable("overrun"));
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

/// Look up one table in the sfnt directory. `(0, 0)` when absent — caller
/// treats missing tables as empty/unusable, never as a crash.
fn table(data: &[u8], tag: &[u8; 4]) -> (u32, u32) {
    let mut r = Reader::new(data);
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

// ============================================================ tables =======

fn parse_head(data: &[u8]) -> Result<(f32, bool), TtfError> {
    let (off, len) = table(data, b"head");
    if len < 54 {
        return Err(TtfError::BadTable("head"));
    }
    let mut r = Reader::new(&data[off as usize..(off + len) as usize]);
    r.take(18).map_err(|_| TtfError::BadTable("head"))?; // version..flags
    let upem = r.u16().map_err(|_| TtfError::BadTable("head"))?;
    r.take(30).map_err(|_| TtfError::BadTable("head"))?; // dates, bbox, macStyle…
    let loc_fmt = r.i16().map_err(|_| TtfError::BadTable("head"))?;
    Ok((upem as f32, loc_fmt == 0))
}

/// We only need `numGlyphs`; everything else is sized to it.
fn parse_maxp(data: &[u8]) -> Result<u16, TtfError> {
    let (off, len) = table(data, b"maxp");
    if len < 6 {
        return Err(TtfError::BadTable("maxp"));
    }
    let mut r = Reader::new(&data[off as usize..(off + len) as usize]);
    r.u32().map_err(|_| TtfError::BadTable("maxp"))?;
    r.u16().map_err(|_| TtfError::BadTable("maxp"))
}

fn parse_hhea(data: &[u8]) -> Result<(f32, f32, u16), TtfError> {
    let (off, len) = table(data, b"hhea");
    if len < 34 {
        return Err(TtfError::BadTable("hhea"));
    }
    let mut r = Reader::new(&data[off as usize..(off + len) as usize]);
    r.u32().map_err(|_| TtfError::BadTable("hhea"))?; // version
    let ascent = r.i16()?;
    let descent = r.i16()?;
    r.take(26).map_err(|_| TtfError::BadTable("hhea"))?; // lineGap..metricDataFormat
    let nhm = r.u16().map_err(|_| TtfError::BadTable("hhea"))?;
    Ok((ascent as f32, descent as f32, nhm))
}

fn parse_loca(data: &[u8], short: bool, num_glyphs: u16) -> Result<Vec<u32>, TtfError> {
    let (off, len) = table(data, b"loca");
    let need = num_glyphs as usize + 1;
    // Short loca stores 2-byte entries, long stores 4.
    let entry = if short { 2 } else { 4 };
    if (len as usize) < need * entry {
        return Err(TtfError::BadTable("loca"));
    }
    let mut r = Reader::new(&data[off as usize..(off + len) as usize]);
    let mut out = Vec::with_capacity(need);
    for _ in 0..need {
        out.push(if short { r.u16()? as u32 * 2 } else { r.u32()? });
    }
    Ok(out)
}

fn parse_hmtx(data: &[u8], nhm: u16, num_glyphs: u16) -> Result<Vec<(u16, i16)>, TtfError> {
    let (off, len) = table(data, b"hmtx");
    let mut r = Reader::new(&data[off as usize..(off + len) as usize]);
    let mut out = Vec::with_capacity(num_glyphs as usize);
    for _ in 0..nhm.min(num_glyphs) {
        let aw = r.u16()?;
        let lsb = r.i16()?;
        out.push((aw, lsb));
    }
    // Remaining glyphs inherit the last advance, each contributing an lsb.
    let last = out.last().map_or(0, |e| e.0);
    while out.len() < num_glyphs as usize {
        out.push((last, r.i16()?));
    }
    let _ = len;
    Ok(out)
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
    /// first array entry and the lowest codepoint it indexes.
    indirect: Option<(u32, u32)>,
}

struct Cmap<'a> {
    segs: Vec<Seg>,
    /// The font buffer, for idRangeOffset indirection (borrowed so parsing
    /// never duplicates the font bytes).
    data: &'a [u8],
}

fn parse_cmap(data: &[u8]) -> Result<Cmap<'_>, TtfError> {
    let (off, len) = table(data, b"cmap");
    if len < 4 {
        return Err(TtfError::BadTable("cmap"));
    }
    let mut r = Reader::new(&data[off as usize..(off + len) as usize]);
    r.u16().map_err(|_| TtfError::BadTable("cmap"))?; // version
    let n = r.u16().map_err(|_| TtfError::BadTable("cmap"))?;
    let mut best: Option<(u8, (u32, u32))> = None;
    for _ in 0..n {
        let pid = r.u16()?;
        let eid = r.u16()?;
        let sub = r.u32()?;
        let score = match (pid, eid) {
            (3, 1) | (0, 3) => 0,
            (3, 10) => 1,
            _ => continue,
        };
        if best.is_some_and(|(s, _)| score >= s) {
            continue;
        }
        // Subtable offsets are relative to the cmap table itself.
        let abs = off + sub;
        if abs as usize + 2 > data.len() {
            return Err(TtfError::BadTable("cmap"));
        }
        let mut sr = Reader::new(&data[abs as usize..]);
        match sr.u16().map_err(|_| TtfError::BadTable("cmap"))? {
            4 if score < 2 => best = Some((score, (abs, 4))),
            12 if score >= 1 => best = Some((score, (abs, 12))),
            _ => {}
        }
    }
    let Some((_, (sub, fmt))) = best else {
        return Err(TtfError::BadTable("cmap"));
    };
    let mut segs = match fmt {
        4 => parse_cmap4(data, sub)?,
        12 => parse_cmap12(data, sub)?,
        _ => return Err(TtfError::BadTable("cmap")),
    };
    segs.sort_by_key(|s| s.start);
    Ok(Cmap { segs, data })
}

impl Cmap<'_> {
    /// Codepoint -> glyph id; 0 = .notdef.
    fn gid(&self, cp: u32) -> u16 {
        for s in &self.segs {
            if cp < s.start {
                break; // sorted: nothing later can map
            }
            if cp > s.end {
                continue;
            }
            if let Some((garr, gstart)) = s.indirect {
                // Format-4 idRangeOffset: one array slot per codepoint.
                let at = (garr + (cp - gstart) * 2) as usize;
                if at + 2 <= self.data.len() {
                    return u16::from_be_bytes([self.data[at + 1], self.data[at]]);
                }
                return 0;
            }
            return (cp as i32 + s.delta.unwrap_or(0)) as u16;
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
    r.take(6).map_err(|_| TtfError::BadTable("cmap4"))?; // searchRange..rangeShift

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
                indirect: Some((ros_off + (2 * i as u32) + ro as u32, s as u32)),
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
        return Err(TtfError::BadTable("cmap12"));
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
    loca: Vec<u32>,
    num_glyphs: u16,
    upem: f32,
    ascent: f32,
    descent: f32,
    hmtx: Vec<(u16, i16)>,
    cache: RefCell<HashMap<CacheKey, Option<Rc<Glyph>>>>,
}

/// Key for the rasterized-glyph cache, matching the atlas (char, 1/16 px).
type CacheKey = (char, u32);

/// A flattened outline contour: polygon vertices in font units, y-up.
struct Contour {
    pts: Vec<(f32, f32)>,
}

impl Face<'_> {
    pub fn parse(data: &'static [u8]) -> Result<Self, TtfError> {
        if data.len() < 12 || data[0..4] != [0, 1, 0, 0] {
            // TrueType 1.0 open-type magic; CFF-flavored OTTO is out of scope.
            return Err(TtfError::BadMagic);
        }
        let cmap = parse_cmap(data)?;
        let (upem, short_loca) = parse_head(data)?;
        if !(upem > 0.0 && upem.is_finite()) {
            return Err(TtfError::BadTable("head upem"));
        };
        let num_glyphs = parse_maxp(data)?;
        let (ascent, descent, nhm) = parse_hhea(data)?;
        let loca = parse_loca(data, short_loca, num_glyphs)?;
        let hmtx = parse_hmtx(data, nhm, num_glyphs)?;
        Ok(Self {
            data,
            glyf_off: table(data, b"glyf").0,
            cmap,
            loca,
            num_glyphs,
            upem,
            ascent,
            descent,
            hmtx,
            cache: RefCell::new(HashMap::new()),
        })
    }

    pub fn units_per_em(&self) -> f32 {
        self.upem
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

    /// Advance width in font units.
    pub fn advance(&self, gid: u16) -> f32 {
        self.hmtx.get(gid as usize).map_or(0.0, |&(a, _)| a as f32)
    }

    /// Number of parsed glyphs (probe/debug).
    pub fn num_glyphs(&self) -> u16 {
        self.num_glyphs
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
            return Err(TtfError::BadGlyph(gid));
        }
        let start = self.loca[gid as usize] as usize;
        let end = self.loca[gid as usize + 1] as usize;
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
        r.take(8).map_err(|_| TtfError::BadGlyph(0))?; // xMin..yMax bbox
        let mut ends = Vec::with_capacity(n_contours as usize);
        for _ in 0..n_contours {
            ends.push(r.u16()? as usize);
        }
        let n_points = ends.last().map_or(0, |&e| e + 1);
        if n_points > MAX_GLYF_POINTS {
            return Err(TtfError::BadTable("glyf points"));
        }
        let instr_len = r.u16()? as usize;
        r.take(instr_len).map_err(|_| TtfError::BadGlyph(0))?;
        let flags = read_flags(r, n_points)?;
        let xs = read_coords(r, n_points, &flags, true)?;
        let ys = read_coords(r, n_points, &flags, false)?;
        let pt = |i: usize| (xs[i], ys[i]);
        let on = |i: usize| flags[i] & 1 == 1;

        let mut contours = Vec::with_capacity(n_contours as usize);
        let mut start = 0;
        for &end in &ends {
            if end < start {
                return Err(TtfError::BadGlyph(u16::MAX));
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
            let d = r.u8()?;
            cur += if f & same_flag != 0 {
                d as i32
            } else {
                d as i32 - 0x100
            };
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
        r.take(8).map_err(|_| TtfError::BadGlyph(0))?; // component bbox
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
        let key = (ch, (size * 16.0).round() as u32);
        if let Some(g) = self.cache.borrow().get(&key) {
            return g.clone();
        }
        let gid = self.glyph_id(ch as u32);
        let contours = match self.outline(gid, 0) {
            Ok(c) if !c.is_empty() => c,
            _ => {
                self.cache.borrow_mut().insert(key, None);
                return None;
            }
        };
        let scale = size / self.upem;
        let edges = edges_of(&contours, scale);
        if edges.is_empty() {
            self.cache.borrow_mut().insert(key, None);
            return None;
        }
        let g = Rc::new(Self::rasterize(&edges));
        self.cache.borrow_mut().insert(key, Some(g.clone()));
        Some(g)
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
                xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
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
        assert!((f.units_per_em() - 2048.0).abs() < 1.0);
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
        let (off, len) = table(&data, b"cmap");
        println!("table cmap -> ({off}, {len})");
        match parse_cmap(&data) {
            Ok(c) => {
                println!("cmap OK, {} segs; 'A' -> gid {}", c.segs.len(), c.gid(65));
            }
            Err(e) => println!("cmap ERR {e:?}"),
        }
        match parse_head(&data) {
            Ok((upem, short)) => println!("head OK upem={upem} short_loca={short}"),
            Err(e) => println!("head ERR {e:?}"),
        }
        match parse_maxp(&data) {
            Ok(n) => println!("maxp OK glyphs={n}"),
            Err(e) => println!("maxp ERR {e:?}"),
        }
        match parse_hhea(&data) {
            Ok(v) => println!("hhea OK {v:?}"),
            Err(e) => println!("hhea ERR {e:?}"),
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(
            Face::parse(b"nope short thing"),
            Err(TtfError::BadMagic)
        ));
        // Empty cache key must be cheap: char + 1/16-px size = 8 bytes.
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
        assert!(matches!(Face::parse(leak(bad)), Err(TtfError::BadTable(_))));
    }

    #[test]
    fn hostile_glyph_returns_empty_or_error_never_panic() {
        let f = load("LiberationSans-Regular");
        // Out-of-range gids: loca/outline paths must not read wild memory.
        assert!(matches!(f.outline(u16::MAX, 0), Err(TtfError::BadGlyph(_))));
        assert!(matches!(f.outline(50_000, 0), Err(TtfError::BadGlyph(_))));
        // Deep composite recursion self-terminates at MAX_COMPONENT_DEPTH.
        let _ = f.outline(f.glyph_id(b'A' as u32), MAX_COMPONENT_DEPTH + 1);
    }
}
