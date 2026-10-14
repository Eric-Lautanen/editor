//! App icon pixels, drawn procedurally — no .rc resource needed. Windows
//! wraps this in `CreateIconIndirect`; X11/macOS reuse it as-is.
//!
//! The mark is a reader, not a letter: a slate document page with a folded
//! corner and ruled lines (one brand-blue) on a light gradient tile with a
//! hairline border, so it separates from dark and light taskbars alike and
//! still reads as a page at 16 px.

use crate::gfx::color::Color;

/// Tile size in px (the classic Windows small-icon size; other platforms scale).
const SIZE: usize = 32;
/// Rounded-corner radius of the tile.
const RADIUS: f32 = 7.0;

/// Signed distance to an axis-aligned rounded rect centered on the origin
/// (negative inside), for anti-aliased edges.
fn sd_rr(x: f32, y: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let (qx, qy) = (x.abs() - hw + r, y.abs() - hh + r);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t + 0.5) as u8;
    Color::rgba(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b), 255)
}

/// 32×32 ARGB pixels (0xAARRGGBB, row-major, top-down).
pub fn argb32() -> Vec<u32> {
    let tile_top = Color::hex(0xFFFFFF);
    let tile_bottom = Color::hex(0xD9E0E9);
    let border_ink = Color::hex(0x24292F);
    let page = Color::hex(0x24292F);
    let fold_shade = Color::hex(0x161C24);
    let rule = Color::hex(0xC7D0DC);
    let accent = Color::hex(0x2F81F7);
    // Document page: 14×18 centered, with a 5 px folded corner top-right.
    const PAGE_HW: f32 = 7.0;
    const PAGE_HH: f32 = 9.0;
    const FOLD: f32 = 5.0;
    // Ruled lines: (center y, half length, color index 0=rule 1=accent).
    const LINES: [(f32, f32, u8); 3] = [(13.0, 5.0, 0), (17.0, 5.0, 0), (21.0, 3.5, 1)];
    let mut px = Vec::with_capacity(SIZE * SIZE);
    for j in 0..SIZE {
        for i in 0..SIZE {
            let (px_, py) = (i as f32 + 0.5, j as f32 + 0.5);
            // Tile body + vertical gradient.
            let dist = sd_rr(
                px_ - SIZE as f32 / 2.0,
                py - SIZE as f32 / 2.0,
                SIZE as f32 / 2.0,
                SIZE as f32 / 2.0,
                RADIUS,
            );
            let tile_a = (0.5 - dist).clamp(0.0, 1.0);
            let t = j as f32 / (SIZE - 1) as f32;
            let mut c = mix(tile_top, tile_bottom, t);
            // Hairline border: a 1 px ring just inside the edge.
            let ring = (1.0 - (dist + 0.75).abs()).clamp(0.0, 1.0);
            c = mix(c, border_ink, ring * 0.45);
            // Document page.
            let page_d = sd_rr(px_ - 16.0, py - 16.0, PAGE_HW, PAGE_HH, 1.5);
            let page_cov = (0.5 - page_d).clamp(0.0, 1.0);
            c = mix(c, page, page_cov);
            // Ruled lines (pill bars).
            for &(cy, hl, kind) in &LINES {
                let dx = (px_ - 16.0).abs() - hl;
                let dy = (py - cy).abs() - 1.0;
                let d = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0);
                let cov = (0.5 - d).clamp(0.0, 1.0).min(page_cov);
                c = mix(c, if kind == 0 { rule } else { accent }, cov);
            }
            // Folded corner: the page's top-right 5×5 square, cut above the
            // diagonal back to the tile gradient, with a shaded crease
            // along it. The corner mask keeps the cut from leaking down the
            // page over the ruled lines.
            let in_cx = ((px_ - (16.0 + PAGE_HW - FOLD)) + 0.5).clamp(0.0, 1.0);
            let in_cy = (((16.0 - PAGE_HH + FOLD) - py) + 0.5).clamp(0.0, 1.0);
            let corner = in_cx.min(in_cy);
            let fx = (px_ - (16.0 + PAGE_HW - FOLD)) + (py - (16.0 - PAGE_HH)) - FOLD;
            let cut = (fx + 0.5).clamp(0.0, 1.0).min(corner).min(page_cov);
            c = mix(c, mix(tile_top, tile_bottom, t), cut);
            let crease = (1.0 - fx.abs()).clamp(0.0, 1.0).min(corner).min(page_cov);
            c = mix(c, fold_shade, crease * 0.55);
            px.push(Color::rgba(c.r, c.g, c.b, (tile_a * 255.0 + 0.5) as u8).pack());
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_a_centered_document_tile() {
        let px = argb32();
        assert_eq!(px.len(), SIZE * SIZE);
        let at = |i: usize, j: usize| px[j * SIZE + i];
        // Corners fall outside the rounded tile: fully transparent.
        for (i, j) in [(0, 0), (31, 0), (0, 31), (31, 31)] {
            assert_eq!(at(i, j) >> 24, 0, "corner ({i},{j}) should be transparent");
        }
        // Edge midpoints sit on the tile: opaque and light (the tile is
        // near-white even under its hairline border).
        for (i, j) in [(16, 0), (0, 16), (31, 16), (16, 31)] {
            let p = at(i, j);
            assert_eq!(p >> 24, 0xFF, "edge ({i},{j}) should be opaque: {p:08X}");
            let (r, g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
            assert!(
                r > 150 && g > 150 && b > 150,
                "edge ({i},{j}) should be light: {p:08X}"
            );
        }
        // The slate page covers a real chunk of the tile, roughly centered.
        let mut dark = 0;
        let (mut min_x, mut max_x) = (SIZE, 0);
        for j in 0..SIZE {
            for i in 0..SIZE {
                let p = at(i, j);
                let (r, g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
                if p >> 24 == 0xFF && r < 100 && g < 100 && b < 100 {
                    dark += 1;
                    min_x = min_x.min(i);
                    max_x = max_x.max(i);
                }
            }
        }
        assert!(dark > 60, "page should be visible, got {dark} dark px");
        assert!(
            (min_x + max_x) / 2 > 8 && (min_x + max_x) / 2 < 24,
            "page should be horizontally centered, spans x {min_x}..{max_x}"
        );
        // The accent rule: blue-dominant pixels from the short brand line.
        let blue = (0..SIZE)
            .flat_map(|j| (0..SIZE).map(move |i| at(i, j)))
            .filter(|&p| {
                let (r, _g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
                p >> 24 == 0xFF && b > r + 40 && b > 120
            })
            .count();
        assert!(
            blue > 6,
            "accent rule should be visible, got {blue} blue px"
        );
    }
}
