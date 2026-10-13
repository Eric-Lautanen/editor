//! Colour glyphs, the COLR v1 flavour.
//!
//! Segoe UI Emoji, Noto Color Emoji and most modern emoji faces do not store
//! pictures. They store a *paint graph*: each base glyph is a small tree of
//! paint nodes that ultimately clip a solid colour or a gradient to a glyph
//! outline. This module renders that graph.
//!
//! The full COLR v1 specification is large — variable-font deltas, sweep
//! gradients, clip boxes, non-separable blend modes. What the shipped emoji
//! faces actually use is a small corner of it, so this implements that corner
//! honestly and ignores the rest rather than half-implementing everything:
//!
//! * `PaintColrLayers`, `PaintSolid`, `PaintLinearGradient`,
//!   `PaintRadialGradient`, `PaintGlyph`, `PaintColrGlyph`, `PaintComposite`,
//!   and the affine `PaintTransform` family.
//! * Palette 0 of the `CPAL` table, and the `0xFFFF` "use the text colour"
//!   palette sentinel (see [`Glyph::fg`](super::Glyph)).
//!
//! Anything else is skipped, never guessed at.
//!
//! Rendering is two passes over the same graph: the first accumulates the
//! extent in device pixels so the canvas can be sized, the second actually
//! paints. Both passes share one traversal via the [`Sink`] trait, so the node
//! formats are decoded in exactly one place.

use super::ttf::{self, Face};
use super::Glyph;

/// Recursion cap. The spec allows nesting; a hostile font would not stop.
const MAX_DEPTH: u32 = 24;
/// Node visits per pass, so a cyclic or exploding graph cannot hang a render.
const MAX_NODES: u32 = 20_000;
/// Base glyph records beyond this are treated as a malformed table.
const MAX_BASE_GLYPHS: u32 = 65_536;
/// Colour stops in one gradient.
const MAX_STOPS: u32 = 256;
/// Longest rendered side, matching the outline rasterizer's own cap.
const MAX_DIM: usize = 4_096;

// ============================================================== reading =====

fn u16(d: &[u8], at: usize) -> Option<u16> {
    d.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
}

fn i16(d: &[u8], at: usize) -> Option<i16> {
    d.get(at..at + 2).map(|b| i16::from_be_bytes([b[0], b[1]]))
}

fn u32(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// A 24-bit big-endian offset. Paint graph offsets are all `Offset24`.
fn off24(d: &[u8], at: usize) -> Option<usize> {
    let b = d.get(at..at + 3)?;
    Some(((b[0] as usize) << 16) | ((b[1] as usize) << 8) | b[2] as usize)
}

/// 2.14 fixed point: the unit used for scales, angles and alphas.
fn f2dot14(d: &[u8], at: usize) -> Option<f32> {
    i16(d, at).map(|v| v as f32 / 16384.0)
}

/// 16.16 fixed point, used by `Affine2x3` matrices.
fn fixed(d: &[u8], at: usize) -> Option<f32> {
    u32(d, at).map(|v| v as i32 as f32 / 65536.0)
}

/// Read a `FWORD` (signed font-unit coordinate) as a float pair.
fn point(d: &[u8], at: usize) -> Option<[f32; 2]> {
    Some([i16(d, at)? as f32, i16(d, at + 2)? as f32])
}

// ================================================================ affine ====

/// A 2×3 affine map, `p' = M·p + t`, in whatever space the caller is working.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Affine {
    pub(crate) xx: f32,
    pub(crate) xy: f32,
    pub(crate) yx: f32,
    pub(crate) yy: f32,
    pub(crate) dx: f32,
    pub(crate) dy: f32,
}

impl Affine {
    /// Uniform scale: the base map from font units to device pixels (y-up).
    pub(crate) fn scale(s: f32) -> Self {
        Self {
            xx: s,
            xy: 0.0,
            yx: 0.0,
            yy: s,
            dx: 0.0,
            dy: 0.0,
        }
    }

    fn translate(tx: f32, ty: f32) -> Self {
        Self {
            xx: 1.0,
            xy: 0.0,
            yx: 0.0,
            yy: 1.0,
            dx: tx,
            dy: ty,
        }
    }

    fn rotate(radians: f32) -> Self {
        let (s, c) = radians.sin_cos();
        Self {
            xx: c,
            xy: -s,
            yx: s,
            yy: c,
            dx: 0.0,
            dy: 0.0,
        }
    }

    fn skew(ax: f32, ay: f32) -> Self {
        Self {
            xx: 1.0,
            xy: ax.tan(),
            yx: ay.tan(),
            yy: 1.0,
            dx: 0.0,
            dy: 0.0,
        }
    }

    pub(crate) fn apply(&self, p: [f32; 2]) -> [f32; 2] {
        [
            self.xx * p[0] + self.xy * p[1] + self.dx,
            self.yx * p[0] + self.yy * p[1] + self.dy,
        ]
    }

    /// `self ∘ next`: `next` is applied first, then `self`. Transforms nest
    /// outside-in in a paint graph, so this is the direction composition runs.
    fn then(&self, next: &Affine) -> Affine {
        Affine {
            xx: self.xx * next.xx + self.xy * next.yx,
            xy: self.xx * next.xy + self.xy * next.yy,
            dx: self.xx * next.dx + self.xy * next.dy + self.dx,
            yx: self.yx * next.xx + self.yy * next.yx,
            yy: self.yx * next.xy + self.yy * next.yy,
            dy: self.yx * next.dx + self.yy * next.dy + self.dy,
        }
    }

    /// The inverse map, or `None` when the matrix is singular. Gradients are
    /// evaluated in the font's own space, so a device pixel has to be carried
    /// back through this.
    pub(crate) fn inverse(&self) -> Option<Affine> {
        let det = self.xx * self.yy - self.xy * self.yx;
        if det.abs() < 1e-12 || !det.is_finite() {
            return None;
        }
        let inv = 1.0 / det;
        let (xx, xy) = (self.yy * inv, -self.xy * inv);
        let (yx, yy) = (-self.yx * inv, self.xx * inv);
        Some(Affine {
            xx,
            xy,
            yx,
            yy,
            dx: -(xx * self.dx + xy * self.dy),
            dy: -(yx * self.dx + yy * self.dy),
        })
    }
}

/// Wrap `inner` so it acts about `(cx, cy)` rather than the origin.
fn about(cx: f32, cy: f32, inner: Affine) -> Affine {
    Affine::translate(cx, cy).then(&inner.then(&Affine::translate(-cx, -cy)))
}

// ================================================================ colour ====

/// A resolved colour: either concrete RGBA, or "whatever the text colour is".
///
/// The latter is COLR's `0xFFFF` palette sentinel. Segoe UI Emoji leans on it
/// heavily — its outlines and accents are meant to follow the surrounding text
/// — so it cannot be dropped, and it cannot be baked in either, because the
/// text colour is only known when the glyph is drawn. Such pixels come back as
/// a separate mask instead; see `Glyph::fg`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ink {
    Rgba([u8; 4]),
    Foreground,
}

impl Ink {
    fn alpha(self) -> u8 {
        match self {
            Self::Rgba(c) => c[3],
            Self::Foreground => 255,
        }
    }

    /// Interpolate two inks. Only ever called with both sides of the same
    /// kind: a colour line is either all concrete or all foreground.
    fn lerp(self, other: Self, t: f32) -> Self {
        match (self, other) {
            (Self::Rgba(a), Self::Rgba(b)) => {
                let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
                Self::Rgba([f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2]), f(a[3], b[3])])
            }
            _ => self,
        }
    }
}

/// One stop of a gradient colour line.
#[derive(Clone, Copy)]
struct Stop {
    t: f32,
    ink: Ink,
}

/// How a colour line extends beyond its ends.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Extend {
    Pad,
    Repeat,
    Reflect,
}

impl Extend {
    fn of(v: u8) -> Self {
        match v {
            1 => Self::Repeat,
            2 => Self::Reflect,
            _ => Self::Pad,
        }
    }

    fn apply(self, t: f32) -> f32 {
        if !t.is_finite() {
            return 0.0;
        }
        match self {
            Self::Pad => t.clamp(0.0, 1.0),
            Self::Repeat => t - t.floor(),
            Self::Reflect => {
                let m = (t / 2.0).rem_euclid(1.0) * 2.0;
                if m > 1.0 {
                    2.0 - m
                } else {
                    m
                }
            }
        }
    }
}

// ================================================================== paint ===

/// A paint node's fill, as read straight from the table.
#[derive(Clone, Copy)]
enum Paint {
    Ink(Ink),
    Linear {
        line: usize,
        p: [[f32; 2]; 3],
    },
    Radial {
        line: usize,
        c: [[f32; 2]; 2],
        r: [f32; 2],
    },
}

/// A `Paint` with its colour line already decoded, ready to sample per pixel.
enum Ready {
    Ink(Ink),
    Linear {
        extend: Extend,
        stops: Vec<Stop>,
        p: [[f32; 2]; 3],
    },
    Radial {
        extend: Extend,
        stops: Vec<Stop>,
        c: [[f32; 2]; 2],
        r: [f32; 2],
    },
}

impl Ready {
    /// Colour at a point in the font's own coordinate space.
    fn at(&self, p: [f32; 2]) -> Ink {
        match self {
            Self::Ink(i) => *i,
            Self::Linear { p: g, .. } => {
                let d1 = [g[1][0] - g[0][0], g[1][1] - g[0][1]];
                let d2 = [g[2][0] - g[0][0], g[2][1] - g[0][1]];
                // The iso-lines run parallel to p0→p2, so project onto the
                // normal of that direction. With the usual perpendicular p2
                // this collapses to the plain projection onto p0→p1.
                let n = [-d2[1], d2[0]];
                let den = d1[0] * n[0] + d1[1] * n[1];
                let t = if den.abs() < 1e-9 {
                    0.0
                } else {
                    ((p[0] - g[0][0]) * n[0] + (p[1] - g[0][1]) * n[1]) / den
                };
                self.sample(t)
            }
            Self::Radial { c, r, .. } => {
                // Solve |p − (c0 + t·Δc)| = r0 + t·Δr for the outer root.
                let cd = [c[1][0] - c[0][0], c[1][1] - c[0][1]];
                let rd = r[1] - r[0];
                let q = [p[0] - c[0][0], p[1] - c[0][1]];
                let a = cd[0] * cd[0] + cd[1] * cd[1] - rd * rd;
                let b = -2.0 * (q[0] * cd[0] + q[1] * cd[1] + r[0] * rd);
                let cc = q[0] * q[0] + q[1] * q[1] - r[0] * r[0];
                let t = if a.abs() < 1e-9 {
                    if b.abs() < 1e-9 {
                        0.0
                    } else {
                        -cc / b
                    }
                } else {
                    let disc = b * b - 4.0 * a * cc;
                    if disc < 0.0 {
                        0.0
                    } else {
                        (-b - disc.sqrt()) / (2.0 * a)
                    }
                };
                self.sample(t)
            }
        }
    }

    fn sample(&self, t: f32) -> Ink {
        let (extend, stops) = match self {
            Self::Ink(i) => return *i,
            Self::Linear { extend, stops, .. } | Self::Radial { extend, stops, .. } => {
                (*extend, stops)
            }
        };
        let t = extend.apply(t);
        let Some(first) = stops.first() else {
            return Ink::Rgba([0, 0, 0, 0]);
        };
        if t <= first.t {
            return first.ink;
        }
        for w in stops.windows(2) {
            let (lo, hi) = (w[0], w[1]);
            if t <= hi.t {
                let span = hi.t - lo.t;
                let f = if span.abs() < 1e-9 {
                    0.0
                } else {
                    (t - lo.t) / span
                };
                return lo.ink.lerp(hi.ink, f);
            }
        }
        stops.last().map_or(first.ink, |s| s.ink)
    }
}

// =================================================================== sink ===

/// What a traversal reports. Both passes implement this, so the paint formats
/// are decoded once and the passes differ only in what they do with the result.
trait Sink {
    /// Render the subtree rooted at `paint`, clipped to the outline of `gid`.
    fn mask(
        &mut self,
        colr: &Colr<'_>,
        gid: u16,
        paint: usize,
        a: Affine,
        depth: u32,
        budget: &mut u32,
    );
    /// Render the subtree rooted at `paint` with nothing to clip it.
    fn unclipped(&mut self, colr: &Colr<'_>, paint: usize, a: Affine, depth: u32, budget: &mut u32);
    /// Start a compositing group. Layers within a group stack onto each other;
    /// the finished group is then placed over its parent.
    fn begin_group(&mut self) {}
    /// Close the group opened by [`Sink::begin_group`].
    fn end_group(&mut self) {}
    /// Combine two sub-paints with a Porter-Duff mode.
    fn composite(
        &mut self,
        colr: &Colr<'_>,
        a: Affine,
        node: Compose,
        depth: u32,
        budget: &mut u32,
    );
}

/// A `PaintComposite` node: the two sub-paints and the mode joining them.
#[derive(Clone, Copy)]
struct Compose {
    source: usize,
    mode: u8,
    backdrop: usize,
}

// =================================================================== colr ===

/// The parsed COLR v1 index: where the paint graphs are, and the palette.
pub(crate) struct Colr<'a> {
    data: &'a [u8],
    /// Absolute offset of the LayerList.
    layers: usize,
    /// `(gid, absolute paint offset)`, sorted so lookups can bisect.
    base: Vec<(u16, usize)>,
    /// Palette 0, straight RGBA.
    palette: Vec<[u8; 4]>,
}

impl<'a> Colr<'a> {
    /// Parse the COLR/CPAL tables of the face at `dir`, or `None` when the
    /// face has no usable colour data (including COLR v0, which has no paint
    /// graph and is handled as plain outlines instead).
    pub(crate) fn parse(data: &'a [u8], dir: u32) -> Option<Self> {
        let (off, len) = ttf::table(data, dir, b"COLR");
        if len == 0 {
            return None;
        }
        let colr = off as usize;
        if u16(data, colr)? < 1 {
            return None;
        }
        let base_list = u32(data, colr + 14)? as usize;
        if base_list == 0 {
            return None;
        }
        let bl = colr.checked_add(base_list)?;
        let n = u32(data, bl)?;
        if n == 0 || n > MAX_BASE_GLYPHS {
            return None;
        }
        let mut base = Vec::with_capacity(n as usize);
        for i in 0..n as usize {
            let at = bl + 4 + i * 6;
            let gid = u16(data, at)?;
            let paint = u32(data, at + 2)? as usize;
            base.push((gid, bl.checked_add(paint)?));
        }
        base.sort_unstable_by_key(|&(g, _)| g);

        let layer_list = u32(data, colr + 18)? as usize;
        let layers = if layer_list == 0 {
            0
        } else {
            colr.checked_add(layer_list)?
        };
        let palette = Self::palette(data, dir)?;
        Some(Self {
            data,
            layers,
            base,
            palette,
        })
    }

    /// Palette 0 of CPAL as straight RGBA.
    fn palette(data: &[u8], dir: u32) -> Option<Vec<[u8; 4]>> {
        let (off, len) = ttf::table(data, dir, b"CPAL");
        if len == 0 {
            return None;
        }
        let cpal = off as usize;
        // CPAL header: version, numPaletteEntries, numPalettes,
        // numColorRecords, then the Offset32 to the colour-record array.
        let entries = u16(data, cpal + 2)? as usize;
        let records = u16(data, cpal + 6)? as usize;
        let array = cpal.checked_add(u32(data, cpal + 8)? as usize)?;
        // Only palette 0 is used: the app has one theme, not a palette picker.
        let start = u16(data, cpal + 12)? as usize;
        if entries == 0 || start.checked_add(entries)? > records {
            return None;
        }
        let mut out = Vec::with_capacity(entries);
        for i in 0..entries {
            let at = array.checked_add((start + i) * 4)?;
            // CPAL stores BGRA.
            let b = data.get(at..at + 4)?;
            out.push([b[2], b[1], b[0], b[3]]);
        }
        Some(out)
    }

    /// True when `gid` has a paint graph, i.e. this face can draw it in colour.
    pub(crate) fn has(&self, gid: u16) -> bool {
        self.base.binary_search_by_key(&gid, |&(g, _)| g).is_ok()
    }

    fn paint_offset(&self, gid: u16) -> Option<usize> {
        let at = self.base.binary_search_by_key(&gid, |&(g, _)| g).ok()?;
        Some(self.base[at].1)
    }

    fn layer(&self, index: u32) -> Option<usize> {
        if self.layers == 0 {
            return None;
        }
        let n = u32(self.data, self.layers)?;
        if index >= n {
            return None;
        }
        let at = self.layers + 4 + index as usize * 4;
        Some(self.layers + u32(self.data, at)? as usize)
    }

    /// Palette index → ink. `0xFFFF` is the text-colour sentinel.
    fn ink(&self, palette_index: u16, alpha: f32) -> Ink {
        let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        if palette_index == 0xFFFF {
            return Ink::Foreground;
        }
        match self.palette.get(palette_index as usize) {
            Some(&[r, g, b, pa]) => Ink::Rgba([r, g, b, (pa as u32 * a as u32 / 255) as u8]),
            None => Ink::Rgba([0, 0, 0, 0]),
        }
    }

    /// Decode a colour line into stops, or `None` if it is malformed.
    fn colour_line(&self, at: usize) -> Option<(Extend, Vec<Stop>)> {
        let extend = Extend::of(*self.data.get(at)?);
        let n = u16(self.data, at + 1)?;
        if n == 0 || n as u32 > MAX_STOPS {
            return None;
        }
        let mut stops = Vec::with_capacity(n as usize);
        for i in 0..n as usize {
            let s = at + 3 + i * 6;
            let t = f2dot14(self.data, s)?;
            let pi = u16(self.data, s + 2)?;
            let alpha = f2dot14(self.data, s + 4).unwrap_or(1.0);
            stops.push(Stop {
                t,
                ink: self.ink(pi, alpha),
            });
        }
        stops.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
        Some((extend, stops))
    }

    fn ready(&self, paint: &Paint) -> Option<Ready> {
        Some(match *paint {
            Paint::Ink(i) => Ready::Ink(i),
            Paint::Linear { line, p } => {
                let (extend, stops) = self.colour_line(line)?;
                Ready::Linear { extend, stops, p }
            }
            Paint::Radial { line, c, r } => {
                let (extend, stops) = self.colour_line(line)?;
                Ready::Radial {
                    extend,
                    stops,
                    c,
                    r,
                }
            }
        })
    }

    /// The fill described by a paint node, when that node *is* a fill.
    fn fill_of(&self, fmt: u8, at: usize) -> Option<Paint> {
        match fmt {
            2 | 3 => {
                let pi = u16(self.data, at + 1)?;
                let alpha = f2dot14(self.data, at + 3).unwrap_or(1.0);
                Some(Paint::Ink(self.ink(pi, alpha)))
            }
            4 | 5 => Some(Paint::Linear {
                line: at + off24(self.data, at + 1)?,
                p: [
                    point(self.data, at + 4)?,
                    point(self.data, at + 8)?,
                    point(self.data, at + 12)?,
                ],
            }),
            6 | 7 => Some(Paint::Radial {
                line: at + off24(self.data, at + 1)?,
                c: [point(self.data, at + 4)?, point(self.data, at + 10)?],
                r: [
                    u16(self.data, at + 8)? as f32,
                    u16(self.data, at + 14)? as f32,
                ],
            }),
            _ => None,
        }
    }

    /// The affine a transform node applies to its subtree.
    fn transform(&self, fmt: u8, at: usize) -> Option<Affine> {
        let p = |o: usize| point(self.data, o);
        let pair = |o: usize| -> Option<[f32; 2]> {
            Some([f2dot14(self.data, o)?, f2dot14(self.data, o + 2)?])
        };
        let scale = |s: [f32; 2]| Affine {
            xx: s[0],
            xy: 0.0,
            yx: 0.0,
            yy: s[1],
            dx: 0.0,
            dy: 0.0,
        };
        Some(match fmt {
            12 | 13 => {
                let m = at + off24(self.data, at + 4)?;
                Affine {
                    xx: fixed(self.data, m)?,
                    yx: fixed(self.data, m + 4)?,
                    xy: fixed(self.data, m + 8)?,
                    yy: fixed(self.data, m + 12)?,
                    dx: fixed(self.data, m + 16)?,
                    dy: fixed(self.data, m + 20)?,
                }
            }
            14 | 15 => {
                let d = p(at + 4)?;
                Affine::translate(d[0], d[1])
            }
            16 | 17 => scale(pair(at + 4)?),
            18 | 19 => {
                let c = p(at + 8)?;
                about(c[0], c[1], scale(pair(at + 4)?))
            }
            20 | 21 => Affine::scale(f2dot14(self.data, at + 4)?),
            22 | 23 => {
                let c = p(at + 6)?;
                about(c[0], c[1], Affine::scale(f2dot14(self.data, at + 4)?))
            }
            // The angle is a fraction of 180°, counter-clockwise.
            24 | 25 => Affine::rotate(f2dot14(self.data, at + 4)? * std::f32::consts::PI),
            26 | 27 => {
                let a = f2dot14(self.data, at + 4)? * std::f32::consts::PI;
                let c = p(at + 6)?;
                about(c[0], c[1], Affine::rotate(a))
            }
            28 | 29 => Affine::skew(
                f2dot14(self.data, at + 4)? * std::f32::consts::PI,
                f2dot14(self.data, at + 6)? * std::f32::consts::PI,
            ),
            30 | 31 => {
                let s = Affine::skew(
                    f2dot14(self.data, at + 4)? * std::f32::consts::PI,
                    f2dot14(self.data, at + 6)? * std::f32::consts::PI,
                );
                let c = p(at + 8)?;
                about(c[0], c[1], s)
            }
            _ => return None,
        })
    }

    /// Walk one paint node and everything below it, reporting fills to `sink`.
    fn walk<S: Sink>(&self, at: usize, a: Affine, depth: u32, sink: &mut S, budget: &mut u32) {
        if depth > MAX_DEPTH || *budget == 0 {
            return;
        }
        *budget -= 1;
        let Some(&fmt) = self.data.get(at) else {
            return;
        };
        // A fill is the end of a branch: there is nothing below it.
        if self.fill_of(fmt, at).is_some() {
            sink.unclipped(self, at, a, depth, budget);
            return;
        }
        match fmt {
            // PaintColrLayers
            1 => {
                let (Some(count), Some(first)) =
                    (self.data.get(at + 1).copied(), u32(self.data, at + 2))
                else {
                    return;
                };
                sink.begin_group();
                for k in 0..count as u32 {
                    if let Some(off) = self.layer(first + k) {
                        self.walk(off, a, depth + 1, sink, budget);
                    }
                }
                sink.end_group();
            }
            // PaintGlyph: the glyph's outline clips the *whole* subtree, which
            // is rarely just a fill — Segoe UI Emoji wraps each layer's
            // gradient in its own transform.
            10 => {
                let (Some(off), Some(gid)) = (off24(self.data, at + 1), u16(self.data, at + 4))
                else {
                    return;
                };
                sink.mask(self, gid, at + off, a, depth, budget);
            }
            // PaintColrGlyph: splice in another base glyph's graph.
            11 => {
                if let Some(gid) = u16(self.data, at + 1) {
                    if let Some(off) = self.paint_offset(gid) {
                        self.walk(off, a, depth + 1, sink, budget);
                    }
                }
            }
            // PaintComposite
            32 => {
                let (Some(s), Some(mode), Some(b)) = (
                    off24(self.data, at + 1),
                    self.data.get(at + 4).copied(),
                    off24(self.data, at + 5),
                ) else {
                    return;
                };
                sink.composite(
                    self,
                    a,
                    Compose {
                        source: at + s,
                        mode,
                        backdrop: at + b,
                    },
                    depth,
                    budget,
                );
            }
            // The transform family: recurse with the extra matrix applied.
            12..=31 => {
                let Some(off) = off24(self.data, at + 1) else {
                    return;
                };
                match self.transform(fmt, at) {
                    Some(t) => self.walk(at + off, a.then(&t), depth + 1, sink, budget),
                    None => self.walk(at + off, a, depth + 1, sink, budget),
                }
            }
            _ => {}
        }
    }

    /// Render `gid` at `size` px, or `None` when the face has no colour art
    /// for it or the graph is unusable.
    pub(crate) fn render(&self, face: &Face<'a>, gid: u16, size: f32) -> Option<Glyph> {
        let at = self.paint_offset(gid)?;
        let scale = size / face.upem();
        if !(scale > 0.0 && scale.is_finite()) {
            return None;
        }
        let base = Affine::scale(scale);

        // Pass 1: how big is it? COLR glyphs carry no bounding box of their
        // own — base glyphs usually have an empty `glyf` — so the extent has
        // to come from the outlines the graph actually clips.
        let mut extent = Extent::new(face);
        let mut budget = MAX_NODES;
        self.walk(at, base, 0, &mut extent, &mut budget);
        // A graph that is nothing but an unclipped fill has no outline to
        // measure; the em box is the honest bound for it.
        let bb = extent.done().unwrap_or_else(|| {
            let u = face.upem();
            let lo = base.apply([0.0, -u * 0.35]);
            let hi = base.apply([u, u]);
            (lo[0], lo[1], hi[0], hi[1])
        });
        let left = bb.0.floor();
        let top = bb.3.ceil();
        let w = (bb.2.ceil() - left).clamp(1.0, MAX_DIM as f32) as usize;
        let h = (top - bb.1.floor()).clamp(1.0, MAX_DIM as f32) as usize;
        let (left, top) = (left as i32, top as i32);

        // Pass 2: paint.
        let mut painter = Painter {
            face,
            layers: vec![Layer::new(w, h)],
            left,
            top,
            w,
            h,
        };
        let mut budget = MAX_NODES;
        self.walk(at, base, 0, &mut painter, &mut budget);

        let mut layer = painter.layers.remove(0);
        // The graph can paint outside the box the outlines suggested; clip
        // rather than mis-index.
        layer.clamp(w, h);
        let has_fg = layer.fg.iter().any(|&v| v != 0);
        let coverage: Vec<u8> = layer.px.chunks_exact(4).map(|p| p[3]).collect();
        Some(Glyph {
            width: w,
            height: h,
            left,
            top,
            coverage,
            color: Some(layer.px),
            fg: has_fg.then_some(layer.fg),
        })
    }
}

// ================================================================ extent ====

/// First-pass sink: the union of every clipped outline, in device pixels.
struct Extent<'a> {
    face: &'a Face<'a>,
    min: [f32; 2],
    max: [f32; 2],
    /// Whether any outline contributed at all.
    measured: bool,
}

impl<'a> Extent<'a> {
    fn new(face: &'a Face<'a>) -> Self {
        Self {
            face,
            min: [f32::MAX; 2],
            max: [f32::MIN; 2],
            measured: false,
        }
    }

    fn include(&mut self, p: [f32; 2]) {
        if p[0].is_finite() && p[1].is_finite() {
            self.min[0] = self.min[0].min(p[0]);
            self.min[1] = self.min[1].min(p[1]);
            self.max[0] = self.max[0].max(p[0]);
            self.max[1] = self.max[1].max(p[1]);
        }
    }

    /// `(min_x, min_y, max_x, max_y)`, or `None` when nothing was painted.
    fn done(&self) -> Option<(f32, f32, f32, f32)> {
        self.measured
            .then_some((self.min[0], self.min[1], self.max[0], self.max[1]))
    }
}

impl Sink for Extent<'_> {
    fn mask(
        &mut self,
        colr: &Colr<'_>,
        gid: u16,
        paint: usize,
        a: Affine,
        depth: u32,
        budget: &mut u32,
    ) {
        let mut any = false;
        for c in self.face.contours(gid) {
            for (x, y) in c.pts {
                self.include(a.apply([x, y]));
                any = true;
            }
        }
        if any {
            self.measured = true;
        }
        colr.walk(paint, a, depth + 1, self, budget);
    }

    fn unclipped(
        &mut self,
        _colr: &Colr<'_>,
        _paint: usize,
        _a: Affine,
        _depth: u32,
        _budget: &mut u32,
    ) {
        // Deliberately contributes nothing. A fill with no outline is always
        // inside some `PaintGlyph` in practice, and that glyph's outline is
        // what actually bounds it — guessing an em box here would only widen
        // the canvas around a shape that is already measured. `render` falls
        // back to the em box if *nothing* was measured.
    }

    fn composite(
        &mut self,
        colr: &Colr<'_>,
        a: Affine,
        node: Compose,
        depth: u32,
        budget: &mut u32,
    ) {
        colr.walk(node.source, a, depth + 1, self, budget);
        colr.walk(node.backdrop, a, depth + 1, self, budget);
    }
}

// ================================================================= layer ====

/// A straight-alpha RGBA buffer, plus the coverage of pixels that asked to be
/// drawn in the text colour.
struct Layer {
    w: usize,
    h: usize,
    px: Vec<u8>,
    fg: Vec<u8>,
}

impl Layer {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            px: vec![0; w * h * 4],
            fg: vec![0; w * h],
        }
    }

    /// Composite one pixel, source-over, straight alpha.
    fn blend(&mut self, x: i32, y: i32, rgb: [u8; 4], alpha: u8) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 || alpha == 0 {
            return;
        }
        let o = (y as usize * self.w + x as usize) * 4;
        let sa = alpha as f32 / 255.0;
        let da = self.px[o + 3] as f32 / 255.0;
        let oa = sa + da * (1.0 - sa);
        if oa <= 0.0 {
            return;
        }
        for (k, &channel) in rgb.iter().take(3).enumerate() {
            let sc = channel as f32 / 255.0;
            let dc = self.px[o + k] as f32 / 255.0;
            let c = (sc * sa + dc * da * (1.0 - sa)) / oa;
            self.px[o + k] = (c * 255.0 + 0.5) as u8;
        }
        self.px[o + 3] = (oa * 255.0 + 0.5) as u8;
    }

    /// Multiply the layer's alpha — colour and foreground alike — by a glyph
    /// outline's coverage. This is what `PaintGlyph` does.
    ///
    /// Coverage is zero *outside* the mask as well as partial at its edges, so
    /// this has to visit every pixel of the layer, not just the mask's own
    /// rectangle. Doing otherwise leaves the fill outside the outline intact,
    /// which is exactly a filled bounding box instead of a glyph.
    fn apply_mask(&mut self, m: &Glyph, left: i32, top: i32) {
        let (mx, my) = (m.left - left, top - m.top);
        for y in 0..self.h {
            for x in 0..self.w {
                let (i, j) = (x as i32 - mx, y as i32 - my);
                let cov = if i >= 0 && j >= 0 && (i as usize) < m.width && (j as usize) < m.height {
                    m.coverage[j as usize * m.width + i as usize] as u32
                } else {
                    0
                };
                let o = y * self.w + x;
                self.px[o * 4 + 3] = (self.px[o * 4 + 3] as u32 * cov / 255) as u8;
                self.fg[o] = (self.fg[o] as u32 * cov / 255) as u8;
            }
        }
    }

    /// Source-over of a whole layer.
    fn over(&mut self, src: &Layer) {
        for (i, p) in src.px.chunks_exact(4).enumerate() {
            let x = (i % self.w) as i32;
            let y = (i / self.w) as i32;
            if p[3] != 0 {
                self.blend(x, y, [p[0], p[1], p[2], p[3]], p[3]);
            }
            // Foreground coverage is a mask, not a colour, so groups union it
            // rather than blending it.
            if src.fg[i] != 0 {
                self.fg[i] = self.fg[i].max(src.fg[i]);
            }
        }
    }

    /// Porter-Duff compositing, `src` over `self` per `mode`.
    ///
    /// Everything is done premultiplied — that is the form in which the
    /// Porter-Duff weights are defined — and un-premultiplied again on the way
    /// out, because that is the form the canvas consumes.
    fn composite(&mut self, src: &Layer, mode: u8) {
        // (source weight, backdrop weight) as functions of the two alphas.
        let weights = |sa: f32, da: f32| -> (f32, f32) {
            match mode {
                0 => (0.0, 0.0),            // CLEAR
                1 => (1.0, 0.0),            // SRC
                2 => (0.0, 1.0),            // DST
                4 => (1.0 - da, 1.0),       // DST_OVER
                5 => (da, 0.0),             // SRC_IN
                6 => (0.0, sa),             // DST_IN
                7 => (1.0 - da, 0.0),       // SRC_OUT
                8 => (0.0, 1.0 - sa),       // DST_OUT
                9 => (da, 1.0 - sa),        // SRC_ATOP
                10 => (1.0 - da, sa),       // DST_ATOP
                11 => (1.0 - da, 1.0 - sa), // XOR
                12 => (1.0, 1.0),           // PLUS
                _ => (1.0, 1.0 - sa),       // SRC_OVER
            }
        };
        for (i, s) in src.px.chunks_exact(4).enumerate() {
            let o = i * 4;
            let sa = s[3] as f32 / 255.0;
            let da = self.px[o + 3] as f32 / 255.0;
            // MULTIPLY and SCREEN are separable blends rather than
            // Porter-Duff modes: the colours are combined first, then the
            // result is composited source-over.
            let blend: Option<fn(f32, f32) -> f32> = match mode {
                13 => Some(|cs, cb| cb + cs - cb * cs), // SCREEN
                23 => Some(|cs, cb| cs * cb),           // MULTIPLY
                _ => None,
            };
            if let Some(b) = blend {
                let oa = sa + da * (1.0 - sa);
                let denom = if oa > 0.0 { oa } else { 1.0 };
                for (k, &channel) in s.iter().take(3).enumerate() {
                    let cs = channel as f32 / 255.0;
                    let cb = self.px[o + k] as f32 / 255.0;
                    let c =
                        (sa * (1.0 - da) * cs + sa * da * b(cs, cb) + (1.0 - sa) * da * cb) / denom;
                    self.px[o + k] = (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
                self.px[o + 3] = (oa.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                continue;
            }
            let (fa, fb) = weights(sa, da);
            let oa = (sa * fa + da * fb).clamp(0.0, 1.0);
            if oa <= 0.0 {
                self.px[o..o + 4].fill(0);
                continue;
            }
            for (k, &channel) in s.iter().take(3).enumerate() {
                let cs = channel as f32 / 255.0;
                let cb = self.px[o + k] as f32 / 255.0;
                let c = (cs * sa * fa + cb * da * fb) / oa;
                self.px[o + k] = (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
            self.px[o + 3] = (oa * 255.0 + 0.5) as u8;
        }
        // The foreground mask follows the source's own coverage.
        for (i, s) in src.fg.iter().enumerate() {
            if *s != 0 {
                self.fg[i] = self.fg[i].max(*s);
            }
        }
    }

    /// Trim to `w`×`h`, in case a graph painted outside its own outlines.
    fn clamp(&mut self, w: usize, h: usize) {
        if self.w == w && self.h == h {
            return;
        }
        let mut out = Layer::new(w, h);
        for y in 0..h.min(self.h) {
            let src = y * self.w;
            let dst = y * w;
            out.px[dst * 4..(dst + w) * 4].copy_from_slice(&self.px[src * 4..(src + w) * 4]);
            out.fg[dst..dst + w].copy_from_slice(&self.fg[src..src + w]);
        }
        *self = out;
    }
}

// =============================================================== painter ====

/// Second-pass sink: rasterizes each clipped outline and fills it.
struct Painter<'a> {
    face: &'a Face<'a>,
    /// Compositing group stack; index 0 is the glyph's own canvas.
    layers: Vec<Layer>,
    /// Device-space origin of the canvas, in the rasterizer's y-up pixels.
    left: i32,
    top: i32,
    w: usize,
    h: usize,
}

impl Painter<'_> {
    fn current(&mut self) -> &mut Layer {
        self.layers.last_mut().expect("group stack is never empty")
    }

    /// The mask for one clipped glyph, in the rasterizer's own device space.
    fn rasterize(&self, gid: u16, a: Affine) -> Option<Glyph> {
        let contours = self.face.contours(gid);
        if contours.is_empty() {
            return None;
        }
        let mapped: Vec<ttf::Contour> = contours
            .into_iter()
            .map(|c| {
                ttf::Contour::poly(
                    c.pts
                        .into_iter()
                        .map(|(x, y)| {
                            let q = a.apply([x, y]);
                            (q[0], q[1])
                        })
                        .collect(),
                )
            })
            .collect();
        // Coordinates are already in device pixels, so the scale is 1.
        ttf::rasterize_outline(&mapped, 1.0)
    }

    /// Evaluate a fill at a canvas pixel and lay it down.
    fn paint_pixel(&mut self, ready: &Ready, inv: Option<Affine>, cx: i32, cy: i32, cov: u8) {
        let local = match inv {
            Some(v) => v.apply([
                self.left as f32 + cx as f32 + 0.5,
                self.top as f32 - cy as f32 - 0.5,
            ]),
            None => [0.0, 0.0],
        };
        let ink = ready.at(local);
        let w = self.w;
        let layer = self.current();
        let o = cy as usize * w + cx as usize;
        match ink {
            Ink::Foreground => {
                if let Some(slot) = layer.fg.get_mut(o) {
                    *slot = (*slot).max(cov);
                }
            }
            Ink::Rgba(rgba) => {
                let alpha = (cov as u32 * ink.alpha() as u32 / 255) as u8;
                layer.blend(cx, cy, rgba, alpha);
            }
        }
    }

    /// Cover the whole current layer with a fill — the case with no outline to
    /// clip it. Any enclosing `PaintGlyph` masks it afterwards.
    fn fill_all(&mut self, colr: &Colr<'_>, paint: &Paint, a: Affine) {
        let Some(ready) = colr.ready(paint) else {
            return;
        };
        let inv = a.inverse();
        for y in 0..self.h as i32 {
            for x in 0..self.w as i32 {
                self.paint_pixel(&ready, inv, x, y, 255);
            }
        }
    }
}

impl Sink for Painter<'_> {
    fn mask(
        &mut self,
        colr: &Colr<'_>,
        gid: u16,
        paint: usize,
        a: Affine,
        depth: u32,
        budget: &mut u32,
    ) {
        let Some(m) = self.rasterize(gid, a) else {
            return;
        };
        // The subtree renders on its own, then the outline scales its alpha.
        self.layers.push(Layer::new(self.w, self.h));
        colr.walk(paint, a, depth + 1, self, budget);
        let mut sub = self.layers.pop().expect("just pushed");
        sub.apply_mask(&m, self.left, self.top);
        self.current().over(&sub);
    }

    fn unclipped(
        &mut self,
        colr: &Colr<'_>,
        paint: usize,
        a: Affine,
        _depth: u32,
        _budget: &mut u32,
    ) {
        if let Some(fill) = colr.fill_of(*colr.data.get(paint).unwrap_or(&0xFF), paint) {
            self.fill_all(colr, &fill, a);
        }
    }

    fn begin_group(&mut self) {
        self.layers.push(Layer::new(self.w, self.h));
    }

    fn end_group(&mut self) {
        if self.layers.len() < 2 {
            return;
        }
        let group = self.layers.pop().expect("checked length");
        self.current().over(&group);
    }

    fn composite(
        &mut self,
        colr: &Colr<'_>,
        a: Affine,
        node: Compose,
        depth: u32,
        budget: &mut u32,
    ) {
        self.begin_group();
        colr.walk(node.source, a, depth + 1, self, budget);
        let src = self.layers.pop().expect("just pushed");
        self.begin_group();
        colr.walk(node.backdrop, a, depth + 1, self, budget);
        let mut back = self.layers.pop().expect("just pushed");
        back.composite(&src, node.mode);
        self.current().over(&back);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ink(r: u8, g: u8, b: u8) -> Ink {
        Ink::Rgba([r, g, b, 255])
    }

    // -- fixed-point readers -------------------------------------------------

    #[test]
    fn offsets_are_read_big_endian_and_reject_short_input() {
        assert_eq!(off24(&[0x01, 0x02, 0x03], 0), Some(0x010203));
        assert_eq!(off24(&[0xFF, 0xFF, 0xFF], 0), Some(0x00FF_FFFF));
        assert_eq!(off24(&[0, 0], 0), None);
    }

    #[test]
    fn f2dot14_reads_signed_fractions() {
        assert_eq!(f2dot14(&[0x40, 0x00], 0), Some(1.0));
        assert_eq!(f2dot14(&[0x20, 0x00], 0), Some(0.5));
        assert_eq!(f2dot14(&[0xC0, 0x00], 0), Some(-1.0));
        assert_eq!(f2dot14(&[0x00], 0), None);
    }

    #[test]
    fn fixed_reads_sixteen_sixteen() {
        assert_eq!(fixed(&[0x00, 0x01, 0x00, 0x00], 0), Some(1.0));
        assert_eq!(fixed(&[0x00, 0x00, 0x80, 0x00], 0), Some(0.5));
        assert_eq!(fixed(&[0xFF, 0xFF, 0x00, 0x00], 0), Some(-1.0));
    }

    // -- affine --------------------------------------------------------------

    #[test]
    fn then_composes_the_argument_first() {
        let t = Affine::translate(10.0, 0.0);
        let s = Affine::scale(2.0);
        // `t.then(&s)` means "scale, then translate".
        assert_eq!(t.then(&s).apply([1.0, 1.0]), [12.0, 2.0]);
        assert_eq!(s.then(&t).apply([1.0, 1.0]), [22.0, 2.0]);
    }

    #[test]
    fn a_transform_about_a_centre_leaves_the_centre_alone() {
        let m = about(5.0, 5.0, Affine::scale(3.0));
        let p = m.apply([5.0, 5.0]);
        assert!(
            (p[0] - 5.0).abs() < 1e-4 && (p[1] - 5.0).abs() < 1e-4,
            "{p:?}"
        );
        // ...and scales everything else away from it.
        assert_eq!(m.apply([6.0, 5.0])[0], 8.0);
    }

    #[test]
    fn inverse_round_trips_and_singular_maps_have_none() {
        let m = Affine::translate(3.0, -7.0)
            .then(&Affine::rotate(0.7))
            .then(&Affine::scale(1.5));
        let inv = m.inverse().expect("invertible");
        let p = [12.5, -3.25];
        let q = inv.apply(m.apply(p));
        assert!(
            (q[0] - p[0]).abs() < 1e-3 && (q[1] - p[1]).abs() < 1e-3,
            "{q:?}"
        );

        let flat = Affine {
            xx: 0.0,
            xy: 0.0,
            yx: 0.0,
            yy: 0.0,
            dx: 0.0,
            dy: 0.0,
        };
        assert!(flat.inverse().is_none());
    }

    // -- gradients -----------------------------------------------------------

    /// A two-stop black→white gradient along +x, used by the sampling tests.
    fn ramp(extend: Extend) -> Ready {
        Ready::Linear {
            extend,
            stops: vec![
                Stop {
                    t: 0.0,
                    ink: ink(0, 0, 0),
                },
                Stop {
                    t: 1.0,
                    ink: ink(255, 255, 255),
                },
            ],
            p: [[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]],
        }
    }

    #[test]
    fn a_linear_gradient_runs_along_its_axis_and_is_flat_across_it() {
        let g = ramp(Extend::Pad);
        assert_eq!(g.at([0.0, 0.0]), ink(0, 0, 0));
        assert_eq!(g.at([10.0, 0.0]), ink(255, 255, 255));
        // Halfway along, and unchanged as we move along the iso-lines.
        assert_eq!(g.at([5.0, 0.0]), ink(128, 128, 128));
        assert_eq!(g.at([5.0, 999.0]), ink(128, 128, 128));
    }

    #[test]
    fn a_skewed_linear_gradient_uses_its_second_axis() {
        // p2 is not perpendicular to p0→p1, so the iso-lines are slanted and
        // moving in y now changes the colour.
        let g = Ready::Linear {
            extend: Extend::Pad,
            stops: vec![
                Stop {
                    t: 0.0,
                    ink: ink(0, 0, 0),
                },
                Stop {
                    t: 1.0,
                    ink: ink(255, 255, 255),
                },
            ],
            p: [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]],
        };
        assert_ne!(g.at([5.0, 0.0]), g.at([5.0, 5.0]));
    }

    #[test]
    fn a_radial_gradient_reaches_one_at_the_outer_circle() {
        let g = Ready::Radial {
            extend: Extend::Pad,
            stops: vec![
                Stop {
                    t: 0.0,
                    ink: ink(0, 0, 0),
                },
                Stop {
                    t: 1.0,
                    ink: ink(255, 255, 255),
                },
            ],
            c: [[0.0, 0.0], [0.0, 0.0]],
            r: [0.0, 10.0],
        };
        assert_eq!(g.at([0.0, 0.0]), ink(0, 0, 0), "centre");
        assert_eq!(g.at([10.0, 0.0]), ink(255, 255, 255), "east");
        assert_eq!(g.at([0.0, 10.0]), ink(255, 255, 255), "north");
        assert_eq!(g.at([50.0, 0.0]), ink(255, 255, 255), "padded past the end");
    }

    #[test]
    fn a_radial_gradient_between_two_off_centre_circles_is_well_behaved() {
        let g = Ready::Radial {
            extend: Extend::Pad,
            stops: vec![
                Stop {
                    t: 0.0,
                    ink: ink(0, 0, 0),
                },
                Stop {
                    t: 1.0,
                    ink: ink(255, 255, 255),
                },
            ],
            c: [[0.0, 0.0], [10.0, 0.0]],
            r: [2.0, 6.0],
        };
        // No panic, and the ends of the cone land where the stops say.
        assert_eq!(g.at([2.0, 0.0]), ink(0, 0, 0));
        assert_eq!(g.at([16.0, 0.0]), ink(255, 255, 255));
    }

    #[test]
    fn extend_modes_clamp_repeat_and_reflect() {
        assert_eq!(Extend::Pad.apply(-3.0), 0.0);
        assert_eq!(Extend::Pad.apply(9.0), 1.0);
        assert!((Extend::Repeat.apply(2.25) - 0.25).abs() < 1e-6);
        assert!((Extend::Reflect.apply(1.25) - 0.75).abs() < 1e-6);
        assert!((Extend::Reflect.apply(2.75) - 0.75).abs() < 1e-6);
        assert!((Extend::Reflect.apply(-0.25) - 0.25).abs() < 1e-6);
        // A non-finite parameter must not propagate into the colour.
        assert_eq!(Extend::Pad.apply(f32::NAN), 0.0);
    }

    #[test]
    fn stops_outside_the_ramp_are_clamped_to_the_end_colours() {
        let g = Ready::Linear {
            extend: Extend::Pad,
            stops: vec![
                Stop {
                    t: 0.25,
                    ink: ink(0, 0, 0),
                },
                Stop {
                    t: 0.75,
                    ink: ink(255, 0, 0),
                },
            ],
            p: [[0.0, 0.0], [100.0, 0.0], [0.0, 100.0]],
        };
        assert_eq!(g.at([0.0, 0.0]), ink(0, 0, 0), "before the first stop");
        assert_eq!(g.at([100.0, 0.0]), ink(255, 0, 0), "after the last");
        assert_eq!(g.at([50.0, 0.0]), ink(128, 0, 0), "midway between them");
    }

    #[test]
    fn a_foreground_colour_line_stays_foreground() {
        // `0xFFFF` is "draw this in the text colour". Interpolating it with
        // anything else is meaningless, so it must survive untouched.
        let g = Ready::Ink(Ink::Foreground);
        assert_eq!(g.at([0.0, 0.0]), Ink::Foreground);
        assert_eq!(Ink::Foreground.alpha(), 255);
        assert_eq!(Ink::Foreground.lerp(ink(1, 2, 3), 0.5), Ink::Foreground);
    }

    // -- compositing ---------------------------------------------------------

    #[test]
    fn source_over_blends_straight_alpha() {
        let mut l = Layer::new(1, 1);
        l.blend(0, 0, [0, 0, 0, 255], 255);
        l.blend(0, 0, [255, 255, 255, 255], 128);
        assert!((l.px[0] as i32 - 128).abs() <= 1, "got {}", l.px[0]);
        assert_eq!(l.px[3], 255, "an opaque backdrop stays opaque");
    }

    #[test]
    fn a_translucent_glyph_leaves_the_backdrop_visible() {
        let mut l = Layer::new(1, 1);
        l.blend(0, 0, [0, 0, 255, 255], 255);
        l.blend(0, 0, [255, 0, 0, 255], 128);
        assert!(l.px[2] > 100, "blue should survive underneath: {:?}", l.px);
    }

    #[test]
    fn src_in_keeps_only_what_the_backdrop_covers() {
        let mut back = Layer::new(2, 1);
        back.blend(0, 0, [10, 20, 30, 255], 255);
        let mut src = Layer::new(2, 1);
        src.blend(0, 0, [200, 100, 50, 255], 255);
        src.blend(1, 0, [200, 100, 50, 255], 255);
        back.composite(&src, 5);
        assert_eq!(back.px[3], 255, "inside the backdrop");
        assert_eq!(back.px[7], 0, "outside the backdrop");
    }

    #[test]
    fn dst_out_keeps_only_what_the_source_does_not_cover() {
        let mut back = Layer::new(2, 1);
        for x in 0..2 {
            back.blend(x, 0, [10, 20, 30, 255], 255);
        }
        let mut src = Layer::new(2, 1);
        src.blend(0, 0, [200, 100, 50, 255], 255);
        back.composite(&src, 8);
        assert_eq!(back.px[3], 0, "covered by the source");
        assert_eq!(back.px[7], 255, "untouched by the source");
    }

    #[test]
    fn multiply_darkens_and_screen_lightens() {
        let mut a = Layer::new(1, 1);
        a.blend(0, 0, [128, 128, 128, 255], 255);
        let mut src = Layer::new(1, 1);
        src.blend(0, 0, [128, 128, 128, 255], 255);

        let mut m = Layer::new(1, 1);
        m.px.copy_from_slice(&a.px);
        m.composite(&src, 23);
        assert!(m.px[0] < 100, "multiply should darken: {}", m.px[0]);

        let mut s = Layer::new(1, 1);
        s.px.copy_from_slice(&a.px);
        s.composite(&src, 13);
        assert!(s.px[0] > 150, "screen should lighten: {}", s.px[0]);
    }

    /// The regression that produced solid rectangles instead of glyphs: the
    /// mask has to clear the fill *outside* the outline, not just scale it
    /// inside.
    #[test]
    fn a_mask_clears_everything_outside_the_outline() {
        let mut l = Layer::new(4, 1);
        for x in 0..4 {
            l.blend(x, 0, [255, 0, 0, 255], 255);
        }
        let m = Glyph {
            width: 2,
            height: 1,
            left: 1,
            top: 1,
            coverage: vec![255, 128],
            color: None,
            fg: None,
        };
        l.apply_mask(&m, 0, 1);
        assert_eq!(l.px[3], 0, "left of the outline must be cleared");
        assert_eq!(l.px[7], 255, "inside the outline keeps its alpha");
        assert_eq!(l.px[11], 128, "partial coverage scales the alpha");
        assert_eq!(l.px[15], 0, "right of the outline must be cleared");
    }

    #[test]
    fn a_mask_clips_foreground_coverage_too() {
        let mut l = Layer::new(3, 1);
        l.fg.copy_from_slice(&[255, 255, 255]);
        let m = Glyph {
            width: 1,
            height: 1,
            left: 1,
            top: 1,
            coverage: vec![200],
            color: None,
            fg: None,
        };
        l.apply_mask(&m, 0, 1);
        assert_eq!(l.fg, vec![0, 200, 0]);
    }

    #[test]
    fn groups_union_their_foreground_coverage() {
        let mut dst = Layer::new(2, 1);
        dst.fg[1] = 40;
        let mut src = Layer::new(2, 1);
        src.fg[0] = 90;
        src.fg[1] = 200;
        dst.over(&src);
        assert_eq!(dst.fg, vec![90, 200], "the stronger coverage wins");
    }

    #[test]
    fn clamping_trims_both_colour_and_foreground() {
        let mut l = Layer::new(3, 1);
        l.blend(0, 0, [255, 0, 0, 255], 255);
        l.fg[0] = 7;
        l.clamp(2, 1);
        assert_eq!(l.w, 2);
        assert_eq!(l.px.len(), 8);
        assert_eq!(l.fg.len(), 2);
        assert_eq!(l.px[3], 255, "kept pixels survive the trim");
    }
}
