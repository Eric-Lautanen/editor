//! App icon pixels, rendered from the embedded font atlas — no .rc resource
//! needed. Windows wraps this in `CreateIconIndirect`; X11/macOS reuse it as-is.

use crate::gfx::{
    color::Color,
    font::{FontStyle, FontSystem},
};

/// Tile size in px (the classic Windows small-icon size; other platforms scale).
const SIZE: usize = 32;
/// Rounded-corner radius of the tile.
const RADIUS: f32 = 7.0;

/// 32×32 ARGB pixels (0xAARRGGBB, row-major, top-down): a rounded accent
/// tile carrying a white "R" from the embedded bold face.
pub fn argb32() -> Vec<u32> {
    let fonts = FontSystem::load_embedded();
    let glyph = fonts.glyph(FontStyle::Bold, 'R', 22.0);
    // Glyph box top-left, centered in the tile.
    let (gw, gh) = glyph.as_ref().map_or((0, 0), |g| (g.width, g.height));
    let (bx, by) = ((SIZE - gw) / 2, (SIZE - gh) / 2);
    let tile = Color::hex(0x0969DA);
    let letter = Color::rgb(255, 255, 255);
    let mix = |a: u8, b: u8, t: f32| (a as f32 + (b as f32 - a as f32) * t + 0.5) as u8;
    let mut px = Vec::with_capacity(SIZE * SIZE);
    for j in 0..SIZE {
        for i in 0..SIZE {
            // Signed distance to the rounded-rect boundary (negative inside).
            let (x, y) = (
                i as f32 + 0.5 - SIZE as f32 / 2.0,
                j as f32 + 0.5 - SIZE as f32 / 2.0,
            );
            let (qx, qy) = (
                x.abs() - SIZE as f32 / 2.0 + RADIUS,
                y.abs() - SIZE as f32 / 2.0 + RADIUS,
            );
            let dist = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - RADIUS;
            let tile_a = (0.5 - dist).clamp(0.0, 1.0);
            let cov = match &glyph {
                Some(g) => {
                    let (u, v) = (i as i64 - bx as i64, j as i64 - by as i64);
                    if u >= 0 && v >= 0 {
                        let (u, v) = (u as usize, v as usize);
                        if u < g.width && v < g.height {
                            g.coverage[v * g.width + u] as f32 / 255.0
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    }
                }
                None => 0.0,
            };
            let c = Color::rgba(
                mix(tile.r, letter.r, cov),
                mix(tile.g, letter.g, cov),
                mix(tile.b, letter.b, cov),
                (tile_a * 255.0 + 0.5) as u8,
            );
            px.push(c.pack());
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_a_centered_r_tile() {
        let px = argb32();
        assert_eq!(px.len(), SIZE * SIZE);
        let at = |i: usize, j: usize| px[j * SIZE + i];
        // Corners fall outside the rounded tile: fully transparent.
        for (i, j) in [(0, 0), (31, 0), (0, 31), (31, 31)] {
            assert_eq!(at(i, j) >> 24, 0, "corner ({i},{j}) should be transparent");
        }
        // Edge midpoints sit on the solid tile: opaque accent blue (b > r).
        for (i, j) in [(16, 0), (0, 16), (31, 16), (16, 31)] {
            let p = at(i, j);
            assert_eq!(p >> 24, 0xFF, "edge ({i},{j}) should be opaque: {p:08X}");
            assert!(
                (p & 0xFF) > ((p >> 16) & 0xFF),
                "edge ({i},{j}) should be blue: {p:08X}"
            );
        }
        // The white "R" covers a real chunk of the tile, roughly centered.
        let mut white = 0;
        let (mut min_x, mut max_x, mut min_y, mut max_y) = (SIZE, 0, SIZE, 0);
        for j in 0..SIZE {
            for i in 0..SIZE {
                let p = at(i, j);
                if p >> 24 == 0xFF && (p >> 16) & 0xFF > 230 && p & 0xFF > 230 {
                    white += 1;
                    min_x = min_x.min(i);
                    max_x = max_x.max(i);
                    min_y = min_y.min(j);
                    max_y = max_y.max(j);
                }
            }
        }
        assert!(white > 40, "R should be visible, got {white} white px");
        assert!(
            (min_x + max_x) / 2 > 8 && (min_x + max_x) / 2 < 24,
            "R should be horizontally centered, spans x {min_x}..{max_x}"
        );
        assert!(
            (min_y + max_y) / 2 > 8 && (min_y + max_y) / 2 < 24,
            "R should be vertically centered, spans y {min_y}..{max_y}"
        );
    }
}
