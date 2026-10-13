//! Procedural glyphs for codepoints the Liberation faces do not cover.
//!
//! Liberation Sans and Mono stop at Latin, Greek, Cyrillic and a slice of the
//! symbol blocks. Every other codepoint resolves through `cmap` to glyph 0,
//! `.notdef` — and in these faces `.notdef` is a *hollow rectangle*, so
//! un-covered text renders as a row of tofu boxes.
//!
//! Shipping a second font would fix that, at the cost of roughly 700 KB and
//! another licence in a repository that has no dependencies at all. Instead
//! this module draws the characters that actually turn up in Markdown and
//! JSON — box drawing, block elements, check marks, ballot boxes, stars,
//! arrows — as polygons, and feeds them through the same scanline rasterizer
//! the real faces use, so fallbacks anti-alias and quantize identically to
//! font outlines.
//!
//! Coverage is deliberately partial. Emoji, CJK and the exotic symbol blocks
//! are out of scope and still fall back to `.notdef`; `outlines` returning
//! `None` means "not handled here".
//!
//! Every dimension below is derived from measurements of the shipped faces
//! rather than invented, so a procedural `━` lines up exactly with the font's
//! own `─` in the same column.

use super::ttf::{self, Contour};
use super::Glyph;
use std::rc::Rc;

/// Weight of a single box-drawing arm.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arm {
    None,
    Light,
    Heavy,
}

// -- geometry, all in em units, measured from Liberation Mono ---------------

/// Light box-drawing stroke: `─` in Liberation Mono is 145/2048 em thick.
const STROKE: f32 = 145.0 / 2048.0;
/// The horizontal rule does *not* sit at the middle of the line box. `─` puts
/// it at 627.5/2048, and `▀`/`▄` split there too, so the same constant drives
/// box drawing and the half-block line.
const HMID: f32 = 627.5 / 2048.0;
/// Arms overhang the advance slightly so neighbouring cells join up; `─` spans
/// -10..1239 for an advance of 1229.
const OVERHANG: f32 = 10.0 / 2048.0;

/// Stroke for light symbol outlines (ballot box, white star, `∈`, `∅`).
const SYMBOL_STROKE: f32 = 0.070;
/// Stroke for the heavy symbol variants (`✔`, `✘`).
const SYMBOL_STROKE_HEAVY: f32 = 0.140;
/// Symbols sit on the baseline and rise to roughly cap height.
const SYMBOL_MID: f32 = 0.36;

/// The box a procedural glyph is drawn into, in em units.
pub(crate) struct Cell {
    /// Advance width — the glyph's cell.
    pub(crate) advance: f32,
    /// Top of the line box, above the baseline.
    pub(crate) ascent: f32,
    /// Bottom of the line box, below the baseline (negative).
    pub(crate) descent: f32,
}

/// Draw `ch` procedurally at `size` px. `None` when this module has no shape.
pub(crate) fn glyph(ch: char, cell: &Cell, size: f32) -> Option<Rc<Glyph>> {
    let contours = outlines(ch, cell)?;
    ttf::rasterize_outline(&contours, size).map(Rc::new)
}

/// The covered character whose advance a procedural glyph should borrow.
///
/// Box drawing and block elements have to line up with the font's own `─` in
/// the same column, so they take its advance rather than `.notdef`'s, which is
/// a different width in the proportional face.
pub(crate) fn reference(ch: char) -> Option<char> {
    matches!(ch as u32, 0x2500..=0x259F).then_some('\u{2500}')
}

fn outlines(ch: char, cell: &Cell) -> Option<Vec<Contour>> {
    let cp = ch as u32;
    if let Some(arms) = box_arms(cp) {
        return Some(arms_outline(&arms, cell));
    }
    if let Some((arm, horizontal, segments)) = dashes(cp) {
        return Some(dash_outline(arm, horizontal, segments, cell));
    }
    if let Some(corner) = arc_corner(cp) {
        return Some(arc_outline(&corner, cell));
    }
    if let Some(shape) = diagonal(cp) {
        return Some(diagonal_outline(shape, cell));
    }
    if let Some(rects) = block(cp, cell) {
        return Some(
            rects
                .into_iter()
                .map(|(x0, y0, x1, y1)| rect(x0, y0, x1, y1))
                .collect(),
        );
    }
    let mut out = Vec::new();
    if geometric(cp, cell, &mut out) {
        return Some(out);
    }
    if symbols(cp, cell.advance * 0.5, &mut out) {
        return Some(out);
    }
    None
}

// -- primitives ============================================================

/// Axis-aligned rectangle, wound counter-clockwise in y-up space.
///
/// Every primitive here shares this winding so that the non-zero fill rule
/// unions overlapping shapes instead of cancelling them.
fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Contour {
    Contour::poly(vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)])
}

/// A rectangle wound the other way, so it punches a hole in a filled area.
fn rect_hole(x0: f32, y0: f32, x1: f32, y1: f32) -> Contour {
    Contour::reversed(vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)])
}

/// A bar of width `t` running from `(x0, y0)` to `(x1, y1)`, wound like `rect`.
fn bar(x0: f32, y0: f32, x1: f32, y1: f32, t: f32) -> Option<Contour> {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-4 {
        return None;
    }
    let (nx, ny) = (-dy / len * t * 0.5, dx / len * t * 0.5);
    Some(Contour::poly(vec![
        (x0 - nx, y0 - ny),
        (x1 - nx, y1 - ny),
        (x1 + nx, y1 + ny),
        (x0 + nx, y0 + ny),
    ]))
}

/// Stroke a polyline as one bar per segment; joins overlap and union cleanly.
fn polyline(pts: &[(f32, f32)], t: f32, out: &mut Vec<Contour>) {
    for w in pts.windows(2) {
        if let Some(c) = bar(w[0].0, w[0].1, w[1].0, w[1].1, t) {
            out.push(c);
        }
    }
}

/// Sample a circular arc from `a0` to `a1` radians, then stroke it.
fn arc(cx: f32, cy: f32, r: f32, a0: f32, a1: f32, t: f32, out: &mut Vec<Contour>) {
    const STEPS: u32 = 16;
    let pts: Vec<(f32, f32)> = (0..=STEPS)
        .map(|k| {
            let a = a0 + (a1 - a0) * k as f32 / STEPS as f32;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect();
    polyline(&pts, t, out);
}

/// A triangle, wound counter-clockwise whichever way the caller lists it.
fn tri(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> Contour {
    let cross = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
    if cross >= 0.0 {
        Contour::poly(vec![a, b, c])
    } else {
        Contour::poly(vec![a, c, b])
    }
}

/// Vertices of a circle approximated by `n` segments, counter-clockwise.
fn disc_points(cx: f32, cy: f32, r: f32, n: u32) -> Vec<(f32, f32)> {
    (0..n)
        .map(|k| {
            let a = std::f32::consts::TAU * k as f32 / n as f32;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

/// Stroke a full circle.
fn circle(cx: f32, cy: f32, r: f32, t: f32, out: &mut Vec<Contour>) {
    polyline(&closed(&disc_points(cx, cy, r, 24)), t, out);
}

/// The ten vertices of a five-pointed star outline, first point on top.
fn star_points(cx: f32, cy: f32, outer: f32) -> Vec<(f32, f32)> {
    // A star's inner radius is the outer one scaled by 1/phi^2.
    let inner = outer * 0.382;
    (0..10)
        .map(|k| {
            let r = if k % 2 == 0 { outer } else { inner };
            let a = std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * k as f32 / 10.0;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

// -- box drawing ===========================================================

/// A box-drawing glyph's four arms, ordered up, right, down, left.
///
/// Generated from the Unicode character names (`BOX DRAWINGS HEAVY VERTICAL AND
/// RIGHT` and friends), which is why the table is mechanical rather than
/// hand-drawn: corners, tees and crosses all fall out of the arm model.
const BOX_ARMS: &[(u32, [Arm; 4])] = &[
    (0x2501, [Arm::None, Arm::Heavy, Arm::None, Arm::Heavy]),
    (0x2503, [Arm::Heavy, Arm::None, Arm::Heavy, Arm::None]),
    (0x250D, [Arm::None, Arm::Heavy, Arm::Light, Arm::None]),
    (0x250E, [Arm::None, Arm::Light, Arm::Heavy, Arm::None]),
    (0x250F, [Arm::None, Arm::Heavy, Arm::Heavy, Arm::None]),
    (0x2511, [Arm::None, Arm::None, Arm::Light, Arm::Heavy]),
    (0x2512, [Arm::None, Arm::None, Arm::Heavy, Arm::Light]),
    (0x2513, [Arm::None, Arm::None, Arm::Heavy, Arm::Heavy]),
    (0x2515, [Arm::Light, Arm::Heavy, Arm::None, Arm::None]),
    (0x2516, [Arm::Heavy, Arm::Light, Arm::None, Arm::None]),
    (0x2517, [Arm::Heavy, Arm::Heavy, Arm::None, Arm::None]),
    (0x2519, [Arm::Light, Arm::None, Arm::None, Arm::Heavy]),
    (0x251A, [Arm::Heavy, Arm::None, Arm::None, Arm::Light]),
    (0x251B, [Arm::Heavy, Arm::None, Arm::None, Arm::Heavy]),
    (0x251D, [Arm::Light, Arm::Heavy, Arm::Light, Arm::None]),
    (0x251E, [Arm::Heavy, Arm::Light, Arm::Light, Arm::None]),
    (0x251F, [Arm::Light, Arm::Light, Arm::Heavy, Arm::None]),
    (0x2520, [Arm::Heavy, Arm::Light, Arm::Heavy, Arm::None]),
    (0x2521, [Arm::Heavy, Arm::Heavy, Arm::Light, Arm::None]),
    (0x2522, [Arm::Light, Arm::Heavy, Arm::Heavy, Arm::None]),
    (0x2523, [Arm::Heavy, Arm::Heavy, Arm::Heavy, Arm::None]),
    (0x2525, [Arm::Light, Arm::None, Arm::Light, Arm::Heavy]),
    (0x2526, [Arm::Heavy, Arm::None, Arm::Light, Arm::Light]),
    (0x2527, [Arm::Light, Arm::None, Arm::Heavy, Arm::Light]),
    (0x2528, [Arm::Heavy, Arm::None, Arm::Heavy, Arm::Light]),
    (0x2529, [Arm::Heavy, Arm::None, Arm::Light, Arm::Heavy]),
    (0x252A, [Arm::Light, Arm::None, Arm::Heavy, Arm::Heavy]),
    (0x252B, [Arm::Heavy, Arm::None, Arm::Heavy, Arm::Heavy]),
    (0x252D, [Arm::None, Arm::Light, Arm::Light, Arm::Heavy]),
    (0x252E, [Arm::None, Arm::Heavy, Arm::Light, Arm::Light]),
    (0x252F, [Arm::None, Arm::Heavy, Arm::Light, Arm::Heavy]),
    (0x2530, [Arm::None, Arm::Light, Arm::Heavy, Arm::Light]),
    (0x2531, [Arm::None, Arm::Light, Arm::Heavy, Arm::Heavy]),
    (0x2532, [Arm::None, Arm::Heavy, Arm::Heavy, Arm::Light]),
    (0x2533, [Arm::None, Arm::Heavy, Arm::Heavy, Arm::Heavy]),
    (0x2535, [Arm::Light, Arm::Light, Arm::None, Arm::Heavy]),
    (0x2536, [Arm::Light, Arm::Heavy, Arm::None, Arm::Light]),
    (0x2537, [Arm::Light, Arm::Heavy, Arm::None, Arm::Heavy]),
    (0x2538, [Arm::Heavy, Arm::Light, Arm::None, Arm::Light]),
    (0x2539, [Arm::Heavy, Arm::Light, Arm::None, Arm::Heavy]),
    (0x253A, [Arm::Heavy, Arm::Heavy, Arm::None, Arm::Light]),
    (0x253B, [Arm::Heavy, Arm::Heavy, Arm::None, Arm::Heavy]),
    (0x253D, [Arm::Light, Arm::Light, Arm::Light, Arm::Heavy]),
    (0x253E, [Arm::Light, Arm::Heavy, Arm::Light, Arm::Light]),
    (0x253F, [Arm::Light, Arm::Heavy, Arm::Light, Arm::Heavy]),
    (0x2540, [Arm::Heavy, Arm::Light, Arm::Light, Arm::Light]),
    (0x2541, [Arm::Light, Arm::Light, Arm::Heavy, Arm::Light]),
    (0x2542, [Arm::Heavy, Arm::Light, Arm::Heavy, Arm::Light]),
    (0x2543, [Arm::Heavy, Arm::Light, Arm::Light, Arm::Heavy]),
    (0x2544, [Arm::Heavy, Arm::Heavy, Arm::Light, Arm::Light]),
    (0x2545, [Arm::Light, Arm::Light, Arm::Heavy, Arm::Heavy]),
    (0x2546, [Arm::Light, Arm::Heavy, Arm::Heavy, Arm::Light]),
    (0x2547, [Arm::Heavy, Arm::Heavy, Arm::Light, Arm::Heavy]),
    (0x2548, [Arm::Light, Arm::Heavy, Arm::Heavy, Arm::Heavy]),
    (0x2549, [Arm::Heavy, Arm::Light, Arm::Heavy, Arm::Heavy]),
    (0x254A, [Arm::Heavy, Arm::Heavy, Arm::Heavy, Arm::Light]),
    (0x254B, [Arm::Heavy, Arm::Heavy, Arm::Heavy, Arm::Heavy]),
    (0x2574, [Arm::None, Arm::None, Arm::None, Arm::Light]),
    (0x2575, [Arm::Light, Arm::None, Arm::None, Arm::None]),
    (0x2576, [Arm::None, Arm::Light, Arm::None, Arm::None]),
    (0x2577, [Arm::None, Arm::None, Arm::Light, Arm::None]),
    (0x2578, [Arm::None, Arm::None, Arm::None, Arm::Heavy]),
    (0x2579, [Arm::Heavy, Arm::None, Arm::None, Arm::None]),
    (0x257A, [Arm::None, Arm::Heavy, Arm::None, Arm::None]),
    (0x257B, [Arm::None, Arm::None, Arm::Heavy, Arm::None]),
    (0x257C, [Arm::None, Arm::Heavy, Arm::None, Arm::Light]),
    (0x257D, [Arm::Light, Arm::None, Arm::Heavy, Arm::None]),
    (0x257E, [Arm::None, Arm::Light, Arm::None, Arm::Heavy]),
    (0x257F, [Arm::Heavy, Arm::None, Arm::Light, Arm::None]),
];

/// Dashed rules: `(codepoint, weight, horizontal, segments)`.
const DASHES: &[(u32, Arm, bool, u32)] = &[
    (0x2504, Arm::Light, true, 3),
    (0x2505, Arm::Heavy, true, 3),
    (0x2506, Arm::Light, false, 3),
    (0x2507, Arm::Heavy, false, 3),
    (0x2508, Arm::Light, true, 4),
    (0x2509, Arm::Heavy, true, 4),
    (0x250A, Arm::Light, false, 4),
    (0x250B, Arm::Heavy, false, 4),
    (0x254C, Arm::Light, true, 2),
    (0x254D, Arm::Heavy, true, 2),
    (0x254E, Arm::Light, false, 2),
    (0x254F, Arm::Heavy, false, 2),
];

fn box_arms(cp: u32) -> Option<[Arm; 4]> {
    BOX_ARMS
        .iter()
        .find(|(c, _)| *c == cp)
        .map(|(_, arms)| *arms)
}

fn dashes(cp: u32) -> Option<(Arm, bool, u32)> {
    DASHES
        .iter()
        .find(|(c, ..)| *c == cp)
        .map(|&(_, arm, horizontal, segments)| (arm, horizontal, segments))
}

fn thickness(arm: Arm) -> f32 {
    match arm {
        Arm::None => 0.0,
        Arm::Light => STROKE,
        Arm::Heavy => STROKE * 2.0,
    }
}

/// Build a box-drawing glyph from its four arms.
///
/// Horizontal arms run from the cell's vertical centre out to the edges;
/// vertical arms run from the horizontal rule out to the top or bottom of the
/// line box. Both meet at `(advance / 2, HMID)`, which is exactly where the
/// font puts the corner of its own `┌`.
fn arms_outline(arms: &[Arm; 4], cell: &Cell) -> Vec<Contour> {
    let (up, right, down, left) = (arms[0], arms[1], arms[2], arms[3]);
    let vmid = cell.advance * 0.5;
    let mut out = Vec::new();
    for (arm, x0, x1) in [
        (left, -OVERHANG, vmid),
        (right, vmid, cell.advance + OVERHANG),
    ] {
        if arm != Arm::None {
            let h = thickness(arm) * 0.5;
            out.push(rect(x0, HMID - h, x1, HMID + h));
        }
    }
    for (arm, y0, y1) in [(up, HMID, cell.ascent), (down, cell.descent, HMID)] {
        if arm != Arm::None {
            let h = thickness(arm) * 0.5;
            out.push(rect(vmid - h, y0, vmid + h, y1));
        }
    }
    out
}

/// Build a dashed rule: `segments` dashes and `segments - 1` equal gaps.
fn dash_outline(arm: Arm, horizontal: bool, segments: u32, cell: &Cell) -> Vec<Contour> {
    let t = thickness(arm) * 0.5;
    let vmid = cell.advance * 0.5;
    let (a0, a1) = if horizontal {
        (-OVERHANG, cell.advance + OVERHANG)
    } else {
        (cell.descent, cell.ascent)
    };
    let unit = (a1 - a0) / (2 * segments - 1) as f32;
    (0..segments)
        .map(|k| {
            let s = a0 + unit * (2 * k) as f32;
            let e = s + unit;
            if horizontal {
                rect(s, HMID - t, e, HMID + t)
            } else {
                rect(vmid - t, s, vmid + t, e)
            }
        })
        .collect()
}

// -- arcs and diagonals ====================================================

/// A rounded corner: the direction its two arms leave the corner in.
struct Corner {
    /// +1 when the vertical arm goes up, -1 when it goes down.
    vertical: f32,
    /// +1 when the horizontal arm goes right, -1 when it goes left.
    horizontal: f32,
}

fn arc_corner(cp: u32) -> Option<Corner> {
    let (vertical, horizontal) = match cp {
        0x256D => (-1.0, 1.0),  // ╭ arc down and right
        0x256E => (-1.0, -1.0), // ╮ arc down and left
        0x256F => (1.0, -1.0),  // ╯ arc up and left
        0x2570 => (1.0, 1.0),   // ╰ arc up and right
        _ => return None,
    };
    Some(Corner {
        vertical,
        horizontal,
    })
}

/// Draw `┌`-style corner as a quarter circle instead of a right angle.
///
/// The arc is tangent to both arms, so its centre sits one radius along each
/// arm from the corner and the straight parts run out to the cell edges.
fn arc_outline(corner: &Corner, cell: &Cell) -> Vec<Contour> {
    let (v, h) = (corner.vertical, corner.horizontal);
    let vmid = cell.advance * 0.5;
    // Keep the radius inside the cell so both straight parts survive, and
    // inside the line box so the arc stays clear of the neighbours.
    let r = (cell.advance * 0.35)
        .min(cell.ascent - HMID)
        .min(HMID - cell.descent);
    let (cx, cy) = (vmid + h * r, HMID - v * r);
    let mut out = Vec::new();
    // Straight parts, from the cell edge to where the arc takes over.
    let v_end = if v > 0.0 { cell.ascent } else { cell.descent };
    if let Some(c) = bar(vmid, v_end, vmid, cy, STROKE) {
        out.push(c);
    }
    let h_end = if h > 0.0 {
        cell.advance + OVERHANG
    } else {
        -OVERHANG
    };
    if let Some(c) = bar(cx, HMID, h_end, HMID, STROKE) {
        out.push(c);
    }
    // Quarter arc, from the vertical tangent point round to the horizontal
    // one. Both endpoints are fixed by tangency, so the start angle is set by
    // the horizontal arm and the turn direction by the corner's orientation —
    // getting the latter wrong sweeps three quarters of a circle instead.
    let a0 = if h > 0.0 { std::f32::consts::PI } else { 0.0 };
    let a1 = a0 - h * v * std::f32::consts::FRAC_PI_2;
    arc(cx, cy, r, a0, a1, STROKE, &mut out);
    out
}

/// Which way a diagonal runs.
enum Diagonal {
    /// `╱` — bottom left to top right.
    Up,
    /// `╲` — top left to bottom right.
    Down,
    /// `╳` — both.
    Cross,
}

fn diagonal(cp: u32) -> Option<Diagonal> {
    match cp {
        0x2571 => Some(Diagonal::Up),
        0x2572 => Some(Diagonal::Down),
        0x2573 => Some(Diagonal::Cross),
        _ => None,
    }
}

fn diagonal_outline(shape: Diagonal, cell: &Cell) -> Vec<Contour> {
    let (w, d, a) = (cell.advance, cell.descent, cell.ascent);
    let mut out = Vec::new();
    let mut add = |x0, y0, x1, y1| {
        if let Some(c) = bar(x0, y0, x1, y1, STROKE) {
            out.push(c);
        }
    };
    match shape {
        Diagonal::Up => add(0.0, d, w, a),
        Diagonal::Down => add(0.0, a, w, d),
        Diagonal::Cross => {
            add(0.0, d, w, a);
            add(0.0, a, w, d);
        }
    }
    out
}

// -- block elements ========================================================

/// Rectangles making up a block-element glyph, in em units.
///
/// The left/right split is the cell centre and the upper/lower split is
/// `HMID`, both chosen to match the font's own `▌` and `▀` so quadrants
/// reassemble them exactly. The eighth blocks divide the whole line box, which
/// is the Unicode definition and lands within half a pixel of the font's `▄`
/// at any realistic size.
fn block(cp: u32, cell: &Cell) -> Option<Vec<(f32, f32, f32, f32)>> {
    let (w, d, a) = (cell.advance, cell.descent, cell.ascent);
    let h = a - d;
    let (mx, my) = (w * 0.5, HMID);
    let rects: Vec<(f32, f32, f32, f32)> = match cp {
        // Left eighths: seven eighths down to one eighth.
        0x2589..=0x258F => {
            let eighths = 8 - (cp - 0x2588) as i32;
            vec![(0.0, d, w * eighths as f32 / 8.0, a)]
        }
        // Lower eighths: one eighth up to seven eighths.
        0x2581..=0x2587 => {
            let eighths = (cp - 0x2580) as i32;
            vec![(0.0, d, w, d + h * eighths as f32 / 8.0)]
        }
        0x2594 => vec![(0.0, a - h / 8.0, w, a)],
        0x2595 => vec![(w * 7.0 / 8.0, d, w, a)],
        0x2596 => vec![(0.0, d, mx, my)],
        0x2597 => vec![(mx, d, w, my)],
        0x2598 => vec![(0.0, my, mx, a)],
        0x2599 => vec![(0.0, my, mx, a), (0.0, d, w, my)],
        0x259A => vec![(0.0, my, mx, a), (mx, d, w, my)],
        0x259B => vec![(0.0, my, w, a), (0.0, d, mx, my)],
        0x259C => vec![(0.0, my, w, a), (mx, d, w, my)],
        0x259D => vec![(mx, my, w, a)],
        0x259E => vec![(mx, my, w, a), (0.0, d, mx, my)],
        0x259F => vec![(mx, my, w, a), (0.0, d, w, my)],
        _ => return None,
    };
    Some(rects)
}

// -- geometric shapes ======================================================

/// Triangles, diamonds, circles, bars and parallelograms from the Geometric
/// Shapes block.
///
/// Liberation carries `▲` and `▼` but not their neighbours, so a diagram that
/// points right — `▶`, the single most common of these — fell back to a tofu
/// box. Everything here is a polygon or a rectangle, so the whole family is
/// cheap to fill in.
fn geometric(cp: u32, cell: &Cell, out: &mut Vec<Contour>) -> bool {
    let cx = cell.advance * 0.5;
    let cy = SYMBOL_MID;
    let (w, d, a) = (cell.advance, cell.descent, cell.ascent);

    // Direction (0 up, 1 right, 2 down, 3 left), half-size, filled.
    let triangle = match cp {
        0x25B3 => Some((0, 0.26, false)), // △
        0x25B4 => Some((0, 0.15, true)),  // ▴
        0x25B5 => Some((0, 0.15, false)), // ▵
        0x25B6 => Some((1, 0.26, true)),  // ▶
        0x25B7 => Some((1, 0.26, false)), // ▷
        0x25B8 => Some((1, 0.15, true)),  // ▸
        0x25B9 => Some((1, 0.15, false)), // ▹
        0x25BB => Some((1, 0.18, false)), // ▻
        0x25BD => Some((2, 0.26, false)), // ▽
        0x25BE => Some((2, 0.15, true)),  // ▾
        0x25BF => Some((2, 0.15, false)), // ▿
        0x25C0 => Some((3, 0.26, true)),  // ◀
        0x25C1 => Some((3, 0.26, false)), // ◁
        0x25C2 => Some((3, 0.15, true)),  // ◂
        0x25C3 => Some((3, 0.15, false)), // ◃
        0x25C5 => Some((3, 0.18, false)), // ◅
        _ => None,
    };
    if let Some((dir, s, filled)) = triangle {
        triangle_outline(cx, cy, s, dir, filled, out);
        return true;
    }

    let t = SYMBOL_STROKE;
    match cp {
        // Corner triangles fill half the block cell, like block elements do.
        0x25E2 => out.push(tri((0.0, d), (w, d), (w, a))), // ◢ lower right
        0x25E3 => out.push(tri((0.0, d), (w, d), (0.0, a))), // ◣ lower left
        0x25E4 => out.push(tri((0.0, a), (w, a), (0.0, d))), // ◤ upper left
        0x25E5 => out.push(tri((0.0, a), (w, a), (w, d))), // ◥ upper right
        0x25C6 => out.push(Contour::poly(diamond(cx, cy, 0.30))), // ◆
        0x25C7 => polyline(&closed(&diamond(cx, cy, 0.30)), t, out), // ◇
        0x25C8 => {
            // ◈: white diamond with a black one inside.
            polyline(&closed(&diamond(cx, cy, 0.30)), t, out);
            out.push(Contour::poly(diamond(cx, cy, 0.15)));
        }
        0x25C9 => {
            // ◉: a thick ring around a solid centre.
            out.push(Contour::poly(disc_points(cx, cy, 0.28, 24)));
            out.push(Contour::reversed(disc_points(cx, cy, 0.15, 24)));
            out.push(Contour::poly(disc_points(cx, cy, 0.08, 16)));
        }
        0x25CE => {
            // ◎: two concentric rings.
            circle(cx, cy, 0.27, t, out);
            circle(cx, cy, 0.14, t, out);
        }
        0x25AD => {
            // ▭: a wide outlined rectangle.
            let (x0, x1) = (cx - 0.30, cx + 0.30);
            out.push(rect(x0, 0.18, x1, 0.54));
            out.push(rect_hole(x0 + t, 0.18 + t, x1 - t, 0.54 - t));
        }
        0x25AE => out.push(rect(cx - 0.10, 0.04, cx + 0.10, 0.68)), // ▮
        0x25AF => {
            // ▯: the same bar, hollow.
            out.push(rect(cx - 0.10, 0.04, cx + 0.10, 0.68));
            out.push(rect_hole(cx - 0.10 + t, 0.04 + t, cx + 0.10 - t, 0.68 - t));
        }
        0x25B0 | 0x25B1 => {
            // Parallelograms ▰ ▱.
            let (skew, hh) = (0.14, 0.20);
            let p = vec![
                (cx - 0.30 + skew, cy + hh),
                (cx + 0.30, cy + hh),
                (cx + 0.30 - skew, cy - hh),
                (cx - 0.30, cy - hh),
            ];
            if cp == 0x25B0 {
                out.push(Contour::poly(p));
            } else {
                polyline(&closed(&p), t, out);
            }
        }
        _ => return false,
    }
    true
}

/// Point `dir` of a triangle: 0 up, 1 right, 2 down, 3 left.
fn triangle_outline(cx: f32, cy: f32, s: f32, dir: u32, filled: bool, out: &mut Vec<Contour>) {
    let pts = match dir {
        0 => vec![(cx, cy + s), (cx - s, cy - s), (cx + s, cy - s)],
        1 => vec![(cx + s, cy), (cx - s, cy + s), (cx - s, cy - s)],
        2 => vec![(cx, cy - s), (cx + s, cy + s), (cx - s, cy + s)],
        _ => vec![(cx - s, cy), (cx + s, cy - s), (cx + s, cy + s)],
    };
    if filled {
        out.push(tri(pts[0], pts[1], pts[2]));
    } else {
        polyline(&closed(&pts), SYMBOL_STROKE, out);
    }
}

/// A rhombus, wound counter-clockwise.
fn diamond(cx: f32, cy: f32, s: f32) -> Vec<(f32, f32)> {
    vec![(cx, cy + s), (cx - s, cy), (cx, cy - s), (cx + s, cy)]
}

// -- symbols ===============================================================

/// Append the shape for a symbol codepoint, centred on `cx`. `false` when
/// this module has no shape for it.
fn symbols(cp: u32, cx: f32, out: &mut Vec<Contour>) -> bool {
    match cp {
        0x2713 => check(cx, SYMBOL_MID, 1.0, SYMBOL_STROKE, out),
        0x2714 => check(cx, SYMBOL_MID, 1.0, SYMBOL_STROKE_HEAVY, out),
        0x2717 => cross(cx, SYMBOL_MID, 1.0, SYMBOL_STROKE, out),
        0x2718 => cross(cx, SYMBOL_MID, 1.0, SYMBOL_STROKE_HEAVY, out),
        0x2610 => ballot_box(cx, out),
        0x2611 => {
            ballot_box(cx, out);
            check(cx, SYMBOL_MID, 0.62, SYMBOL_STROKE, out);
        }
        0x2612 => {
            ballot_box(cx, out);
            cross(cx, SYMBOL_MID, 0.62, SYMBOL_STROKE, out);
        }
        0x2605 => out.push(Contour::poly(star_points(cx, SYMBOL_MID, 0.34))),
        0x2606 => polyline(
            &closed(&star_points(cx, SYMBOL_MID, 0.34)),
            SYMBOL_STROKE,
            out,
        ),
        0x21D0 => double_arrow(cx, -1.0, out),
        0x21D2 => double_arrow(cx, 1.0, out),
        0x2794 => heavy_arrow(cx, 0.10, 0.30, out),
        0x279C => heavy_arrow(cx, 0.06, 0.24, out),
        0x2208 => element_of(cx, out),
        0x2209 => {
            element_of(cx, out);
            slash(cx, out);
        }
        0x2205 => {
            circle(cx, SYMBOL_MID, 0.22, SYMBOL_STROKE, out);
            slash(cx, out);
        }
        _ => return false,
    }
    true
}

/// Repeat the first point so a polyline closes back on itself.
fn closed(pts: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut v = pts.to_vec();
    if let Some(&first) = v.first() {
        v.push(first);
    }
    v
}

/// A tick mark, scaled by `s` about `(cx, cy)`.
fn check(cx: f32, cy: f32, s: f32, t: f32, out: &mut Vec<Contour>) {
    let p = |dx: f32, dy: f32| (cx + dx * s, cy + dy * s);
    polyline(
        &[p(-0.195, -0.09), p(-0.055, -0.28), p(0.205, 0.20)],
        t * s,
        out,
    );
}

/// A saltire, scaled by `s` about `(cx, cy)`.
fn cross(cx: f32, cy: f32, s: f32, t: f32, out: &mut Vec<Contour>) {
    let p = |dx: f32, dy: f32| (cx + dx * s, cy + dy * s);
    polyline(&[p(-0.17, -0.17), p(0.17, 0.17)], t * s, out);
    polyline(&[p(-0.17, 0.17), p(0.17, -0.17)], t * s, out);
}

/// The square outline shared by `☐`, `☑` and `☒`.
fn ballot_box(cx: f32, out: &mut Vec<Contour>) {
    let (x0, x1) = (cx - 0.28, cx + 0.28);
    let (y0, y1) = (0.08, 0.64);
    out.push(rect(x0, y0, x1, y1));
    out.push(rect_hole(
        x0 + SYMBOL_STROKE,
        y0 + SYMBOL_STROKE,
        x1 - SYMBOL_STROKE,
        y1 - SYMBOL_STROKE,
    ));
}

/// A double-shafted arrow; `dir` is +1 for right, -1 for left.
fn double_arrow(cx: f32, dir: f32, out: &mut Vec<Contour>) {
    let t = SYMBOL_STROKE;
    let (near, far) = (cx - dir * 0.28, cx + dir * 0.14);
    for y in [0.33, 0.45] {
        out.push(rect(near.min(far), y, near.max(far), y + t));
    }
    let tip = cx + dir * 0.30;
    let root = cx + dir * 0.12;
    out.push(Contour::poly(vec![(root, 0.56), (tip, 0.42), (root, 0.28)]));
}

/// A single-shafted heavy arrow with a triangular head.
fn heavy_arrow(cx: f32, shaft_end: f32, head_half: f32, out: &mut Vec<Contour>) {
    let (t, mid) = (0.09, SYMBOL_MID + 0.06);
    out.push(rect(
        cx - 0.26,
        mid - t * 0.5,
        cx + shaft_end,
        mid + t * 0.5,
    ));
    out.push(Contour::poly(vec![
        (cx + shaft_end - 0.02, mid + head_half),
        (cx + 0.30, mid),
        (cx + shaft_end - 0.02, mid - head_half),
    ]));
}

/// The `∈` shape: a C opening right, crossed by a horizontal rule.
fn element_of(cx: f32, out: &mut Vec<Contour>) {
    arc(
        cx + 0.03,
        SYMBOL_MID,
        0.235,
        std::f32::consts::FRAC_PI_2,
        std::f32::consts::PI + std::f32::consts::FRAC_PI_2,
        SYMBOL_STROKE,
        out,
    );
    out.push(rect(
        cx - 0.19,
        SYMBOL_MID - SYMBOL_STROKE * 0.5,
        cx + 0.24,
        SYMBOL_MID + SYMBOL_STROKE * 0.5,
    ));
}

/// The diagonal stroke used by `∉` and `∅`.
fn slash(cx: f32, out: &mut Vec<Contour>) {
    if let Some(c) = bar(cx - 0.28, 0.09, cx + 0.28, 0.63, SYMBOL_STROKE) {
        out.push(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Liberation Mono's metrics: 0.6 em advance, ascent/descent of the line box.
    fn mono_cell() -> Cell {
        Cell {
            advance: 1229.0 / 2048.0,
            ascent: 1705.0 / 2048.0,
            descent: -615.0 / 2048.0,
        }
    }

    fn render(ch: char) -> Rc<Glyph> {
        let cell = mono_cell();
        let g = glyph(ch, &cell, 32.0).unwrap_or_else(|| panic!("no shape for {ch:?}"));
        assert!(g.width > 0 && g.height > 0, "{ch:?} rasterized to nothing");
        g
    }

    fn ink(g: &Glyph) -> usize {
        g.coverage.iter().filter(|&&c| c > 0).count()
    }

    #[test]
    fn heavy_rule_is_thicker_than_its_light_counterpart() {
        // `━` must be visibly heavier than the font's own `─`.
        let heavy = render('\u{2501}');
        let light = render('\u{2503}');
        assert!(
            heavy.width > light.width,
            "heavy horizontal {} should be wider than heavy vertical {}",
            heavy.width,
            light.width
        );
    }

    #[test]
    fn box_corner_arms_reach_the_edges_they_connect_to() {
        // `┏` is heavy down and right: it must reach the bottom of the line
        // box and the right edge of the cell, and stay clear of the top-left.
        let g = render('\u{250F}');
        let rows_with_ink = |lo: usize, hi: usize| {
            (lo..hi)
                .filter(|&j| {
                    g.coverage[j * g.width..(j + 1) * g.width]
                        .iter()
                        .any(|&c| c > 0)
                })
                .count()
        };
        assert!(rows_with_ink(0, g.height / 2) > 0, "no ink in upper half");
        assert!(
            rows_with_ink(g.height / 2, g.height) > 0,
            "no ink in lower half"
        );
    }

    #[test]
    fn quadrants_reassemble_the_half_blocks() {
        // Upper-left + lower-left quadrants must cover what `▌` covers.
        let left = render('\u{258C}');
        let upper = render('\u{2598}');
        let lower = render('\u{2596}');
        assert!(
            ink(&upper) + ink(&lower) >= ink(&left) * 9 / 10,
            "quadrants {} + {} should cover the left half block {}",
            ink(&upper),
            ink(&lower),
            ink(&left)
        );
    }

    #[test]
    fn every_tabled_codepoint_has_a_shape() {
        for &(cp, _) in BOX_ARMS {
            let ch = char::from_u32(cp).unwrap();
            assert!(outlines(ch, &mono_cell()).is_some(), "U+{cp:04X} unhandled");
        }
        for &(cp, ..) in DASHES {
            let ch = char::from_u32(cp).unwrap();
            assert!(outlines(ch, &mono_cell()).is_some(), "U+{cp:04X} unhandled");
        }
    }

    #[test]
    fn reference_covers_box_drawing_and_blocks_only() {
        assert_eq!(reference('\u{2501}'), Some('\u{2500}'));
        assert_eq!(reference('\u{2589}'), Some('\u{2500}'));
        assert_eq!(reference('A'), None);
        assert_eq!(reference('\u{2713}'), None);
    }

    /// Every arm the generated table claims must put ink where it belongs —
    /// and, just as importantly, no arm it does *not* claim may. This is what
    /// validates the machine-generated table against the renderer.
    #[test]
    fn arms_land_only_where_the_table_says() {
        let cell = mono_cell();
        let size = 32.0;
        let names = ["up", "right", "down", "left"];
        for &(cp, arms) in BOX_ARMS {
            let ch = char::from_u32(cp).unwrap();
            let g = glyph(ch, &cell, size).unwrap();
            let ink_at = |x: f32, y: f32| -> bool {
                let i = (x - g.left as f32).floor() as i32;
                let j = (g.top as f32 - y).floor() as i32;
                i >= 0
                    && j >= 0
                    && (i as usize) < g.width
                    && (j as usize) < g.height
                    && g.coverage[j as usize * g.width + i as usize] > 0
            };
            let (vmid, hmid) = (cell.advance * size * 0.5, HMID * size);
            let probes = [
                (vmid, hmid + (cell.ascent * size - hmid) * 0.6),
                (vmid + (cell.advance * size - vmid) * 0.7, hmid),
                (vmid, hmid - (hmid - cell.descent * size) * 0.6),
                (vmid - vmid * 0.7, hmid),
            ];
            for (dir, (x, y)) in probes.into_iter().enumerate() {
                let got = ink_at(x, y);
                let want = arms[dir] != Arm::None;
                assert_eq!(
                    got, want,
                    "U+{cp:04X} {ch}: {} arm ink={got}, table says {want}",
                    names[dir]
                );
            }
        }
    }

    #[test]
    fn geometric_shapes_all_render() {
        let cell = mono_cell();
        for cp in [
            0x25B3u32, 0x25B4, 0x25B5, 0x25B6, 0x25B7, 0x25B8, 0x25B9, 0x25BB, 0x25BD, 0x25BE,
            0x25BF, 0x25C0, 0x25C1, 0x25C2, 0x25C3, 0x25C5, 0x25E2, 0x25E3, 0x25E4, 0x25E5, 0x25C6,
            0x25C7, 0x25C8, 0x25C9, 0x25CE, 0x25AD, 0x25AE, 0x25AF, 0x25B0, 0x25B1,
        ] {
            let ch = char::from_u32(cp).unwrap();
            let g = glyph(ch, &cell, 24.0).unwrap_or_else(|| panic!("U+{cp:04X} rendered nothing"));
            assert!(g.width > 0 && g.height > 0, "U+{cp:04X} is empty");
        }
    }

    /// A triangle is wide at its base and narrow at its apex, so comparing the
    /// ink in the first and last rows (or columns) reveals which way it points.
    /// This holds for the filled and outlined variants alike, unlike a centroid
    /// test — an outline's base pulls its centroid back toward the middle.
    #[test]
    fn triangles_point_the_way_they_are_named() {
        let cell = mono_cell();
        for (cp, dir) in [
            (0x25B3u32, 0),
            (0x25B4, 0),
            (0x25B5, 0),
            (0x25B6, 1),
            (0x25B7, 1),
            (0x25B8, 1),
            (0x25B9, 1),
            (0x25BB, 1),
            (0x25BD, 2),
            (0x25BE, 2),
            (0x25BF, 2),
            (0x25C0, 3),
            (0x25C1, 3),
            (0x25C2, 3),
            (0x25C3, 3),
            (0x25C5, 3),
        ] {
            let ch = char::from_u32(cp).unwrap();
            let g = glyph(ch, &cell, 32.0).unwrap();
            let row_ink = |j: usize| {
                (0..g.width)
                    .filter(|&i| g.coverage[j * g.width + i] > 0)
                    .count()
            };
            let col_ink = |i: usize| {
                (0..g.height)
                    .filter(|&j| g.coverage[j * g.width + i] > 0)
                    .count()
            };
            let first_row = (0..g.height).find(|&j| row_ink(j) > 0).unwrap();
            let last_row = (0..g.height).rev().find(|&j| row_ink(j) > 0).unwrap();
            let first_col = (0..g.width).find(|&i| col_ink(i) > 0).unwrap();
            let last_col = (0..g.width).rev().find(|&i| col_ink(i) > 0).unwrap();
            // (apex ink, base ink) for the direction this glyph should point.
            let (apex, base) = match dir {
                0 => (row_ink(first_row), row_ink(last_row)),
                1 => (col_ink(last_col), col_ink(first_col)),
                2 => (row_ink(last_row), row_ink(first_row)),
                _ => (col_ink(first_col), col_ink(last_col)),
            };
            assert!(
                base > apex * 2,
                "U+{cp:04X} {ch} should point {dir}: base {base} vs apex {apex}"
            );
        }
    }

    #[test]
    fn unknown_codepoints_are_left_to_the_caller() {
        assert!(outlines('\u{4E2D}', &mono_cell()).is_none(), "CJK");
        assert!(outlines('\u{1F600}', &mono_cell()).is_none(), "emoji");
    }

    #[test]
    fn check_marks_are_denser_than_the_light_ballot_x() {
        let heavy = render('\u{2714}');
        let light = render('\u{2717}');
        assert!(
            ink(&heavy) > ink(&light),
            "heavy check {} should outweigh light X {}",
            ink(&heavy),
            ink(&light)
        );
    }

    #[test]
    fn symbols_stay_inside_their_cell() {
        let cell = mono_cell();
        for cp in [0x2713u32, 0x2610, 0x2605, 0x21D2, 0x2205, 0x2571, 0x256D] {
            let ch = char::from_u32(cp).unwrap();
            let g = glyph(ch, &cell, 24.0).unwrap();
            let advance_px = cell.advance * 24.0;
            assert!(
                g.left as f32 >= -2.0 && (g.left as f32 + g.width as f32) <= advance_px + 4.0,
                "U+{cp:04X} spills out of its {advance_px:.1}px cell: left={} w={}",
                g.left,
                g.width
            );
        }
    }
}
