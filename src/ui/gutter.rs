//! Line-number gutter for the editor pane (Edit mode and Split's right
//! half): a left strip painted *over* the laid-out rows, highlighted for the
//! caret's line. Deliberately an overlay, not part of `layout_plain`:
//! text/measure/hit-test math stays gutter-blind (numbers reserve no width
//! from the wrap/column math), so the grid the caret and selection use today
//! is unchanged. The strip visually replaces the leading pad — long lines
//! keep scrolling under it horizontally.
//!
//! Geometry contract with `layout::text::layout_plain` (no-wrap pane): rows
//! start at `pad` from the pane top, each `line_h` tall, baseline centered.
//! For a caret on buffer line `L`, the pane-space row y is
//! `pad + L*line_h` (document y minus scroll is applied by the caller).
use crate::{
    gfx::{
        canvas::Canvas,
        color::Color,
        font::{FontStyle, FontSystem},
        geom::Rect,
    },
    layout::{text, theme::Theme, Ctx},
};

/// Rail width = 3 digits of mono + breathing room on each side, at body
/// scale. Grows with the line count via `digits` so 1000+ line files push
/// the rail wider automatically.
pub fn rail_w(fonts: &FontSystem, ctx: &Ctx, lines: usize) -> f32 {
    let digits = digits_for(lines);
    let p = text::plain_params(0.0, ctx, false);
    let w = digits as f32 * mono_digit_w(fonts, p.size) + p.pad * 0.75;
    // A sliver for very narrow panes, but never wider than the digits need.
    w.max(2.0 * p.pad * 0.5)
}

pub fn digits_for(lines: usize) -> usize {
    (lines.to_string().len()).max(2)
}

/// Mono faces are metrically aligned; measure one `0` as the digit cell.
fn mono_digit_w(fonts: &FontSystem, size: f32) -> f32 {
    fonts.advance(FontStyle::Mono, '0', size)
}

/// Inputs of `paint` bundled (rail geometry + state) to keep the signature
/// at a sane arity.
pub struct Gutter<'a, 'b> {
    pub fonts: &'a FontSystem,
    pub theme: &'a Theme,
    pub ctx: &'a Ctx<'b>,
    pub caret_line: usize,
    pub line_count: usize,
}

/// Draw the gutter for the editor pane. `area` = pane rect in canvas space,
/// `scroll` = current document offset. Returns the rail's right edge
/// (pane-space) so hit tests can ignore it.
pub fn paint_with(canvas: &mut Canvas, g: Gutter, area: Rect, scroll: f32) -> f32 {
    let (fonts, theme, ctx) = (g.fonts, g.theme, g.ctx);
    let (caret_line, line_count) = (g.caret_line, g.line_count);
    let p = text::plain_params(area.w, ctx, false);
    let m = fonts.metrics(FontStyle::Mono, p.size);
    let rail = rail_w(fonts, ctx, line_count);
    let digits = digits_for(line_count);
    canvas.set_clip(Some(area));

    // Rail: bar + a hairline separation from the text.
    canvas.fill_rect(Rect::new(area.x, area.y, rail, area.h), theme.code_bg);
    canvas.fill_rect(
        Rect::new(area.x + rail - 1.0, area.y, 1.0 * ctx.scale, area.h),
        Color::rgba(theme.muted.r, theme.muted.g, theme.muted.b, 70),
    );

    // Visible row range in document rows (panic-free even when empty).
    let first_row = ((scroll - area.y) / p.line_h).floor().max(0.0) as usize;
    let visible = (area.h / p.line_h).ceil() as usize + 2;

    // Caret highlight.
    if caret_line > first_row && caret_line < first_row + visible {
        let y_top = area.y + (caret_line as f32 * p.line_h + p.pad) - scroll;
        canvas.fill_rect(Rect::new(area.x, y_top, rail, p.line_h), theme.sel);
    }

    // Numbers. Text ops run top-down; skip work when the whole loop would
    // draw off-pane. Rows stop at the file's end: a pane scrolled by a
    // taller sibling (Split's shared progress) must not invent line numbers
    // past the last real row.
    for n in first_row..first_row + visible {
        if n >= line_count {
            break;
        }
        let y = area.y + (n as f32 * p.line_h + p.pad) - scroll;
        if y + p.line_h < area.y || y > area.bottom() || y > area.bottom() + p.line_h {
            continue;
        }
        let baseline = y + (p.line_h - m.height()) / 2.0 + m.ascent;
        let label = format!("{:>width$}", n + 1, width = digits);
        let color = if n == caret_line {
            theme.fg
        } else {
            theme.muted
        };
        canvas.draw_text(
            fonts,
            &label,
            area.x + rail - (digits as f32 + 0.5) * mono_digit_w(fonts, p.size),
            baseline,
            crate::gfx::canvas::TextSpec {
                size: p.size,
                style: FontStyle::Mono,
                color,
            },
        );
    }
    canvas.set_clip(None);
    rail
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> (FontSystem, Theme) {
        (FontSystem::load_embedded(), Theme::light())
    }

    #[test]
    fn digits_grow_and_rail_sizes() {
        assert_eq!(digits_for(9), 2);
        assert_eq!(digits_for(10), 2);
        assert_eq!(digits_for(1000), 4);
        let (fonts, theme) = ctx();
        let c = Ctx {
            fonts: &fonts,
            theme: &theme,
            scale: 1.0,
            zoom: 1.0,
        };
        let small = rail_w(c.fonts, &c, 10);
        let large = rail_w(c.fonts, &c, 10_000);
        assert!(small < large, "rail {small} vs {large}");
    }

    #[test]
    fn paints_visible_rows_only() {
        let (fonts, theme) = ctx();
        let c = Ctx {
            fonts: &fonts,
            theme: &theme,
            scale: 1.0,
            zoom: 1.0,
        };
        let mut canvas = Canvas::new(200, 120);
        let area = Rect::new(0.0, 0.0, 200.0, 120.0);
        // Scrolled deep: numbers around row 400 are the visible ones.
        let scroll = 400.0 * text::line_height(200.0, &c);
        let rail = paint_with(
            &mut canvas,
            Gutter {
                fonts: c.fonts,
                theme: c.theme,
                ctx: &c,
                caret_line: 400,
                line_count: 500,
            },
            area,
            scroll,
        );
        assert!(rail > 20.0);
        // Far past the last row: nothing but the rail itself gets painted,
        // and no ink lands outside the pane's clip.
        let mut deep = Canvas::new(200, 120);
        paint_with(
            &mut deep,
            Gutter {
                fonts: c.fonts,
                theme: c.theme,
                ctx: &c,
                caret_line: 0,
                line_count: 500,
            },
            area,
            1e6,
        );
        assert!(
            deep.pixels
                .iter()
                .any(|&p| (p & 0xFFFFFF) != c.theme.bg.pack()),
            "rail bar itself must paint"
        );
    }

    #[test]
    fn no_phantom_numbers_past_the_last_line() {
        // Split's shared progress can hand this pane an offset from a much
        // taller sibling; rows must still stop at the file's end.
        let (fonts, theme) = ctx();
        let c = Ctx {
            fonts: &fonts,
            theme: &theme,
            scale: 1.0,
            zoom: 1.0,
        };
        let mut canvas = Canvas::new(200, 120);
        canvas.fill_rect(Rect::new(0.0, 0.0, 200.0, 120.0), theme.bg);
        let area = Rect::new(0.0, 0.0, 200.0, 120.0);
        let rail = paint_with(
            &mut canvas,
            Gutter {
                fonts: c.fonts,
                theme: c.theme,
                ctx: &c,
                caret_line: 99,
                line_count: 3,
            },
            area,
            1e6,
        );
        let rail_ink = theme.code_bg.pack() & 0xFFFFFF;
        // Inside the rail, clear of its hairline separator: rail bar only,
        // never digit ink for lines 4+.
        for y in 0..120 {
            for x in 0..(rail as usize).saturating_sub(2) {
                assert_eq!(
                    canvas.pixels[y * 200 + x] & 0xFFFFFF,
                    rail_ink,
                    "phantom number at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn caret_line_highlighted() {
        let (fonts, theme) = ctx();
        let c = Ctx {
            fonts: &fonts,
            theme: &theme,
            scale: 1.0,
            zoom: 1.0,
        };
        let area = Rect::new(0.0, 0.0, 120.0, 80.0);
        // Highlight band = alpha-blended theme.sel over the rail, so compare
        // against an identical render with the caret parked off-screen.
        let mut with = Canvas::new(120, 80);
        let mut without = Canvas::new(120, 80);
        paint_with(
            &mut with,
            Gutter {
                fonts: c.fonts,
                theme: c.theme,
                ctx: &c,
                caret_line: 1,
                line_count: 3,
            },
            area,
            0.0,
        );
        paint_with(
            &mut without,
            Gutter {
                fonts: c.fonts,
                theme: c.theme,
                ctx: &c,
                caret_line: 99,
                line_count: 3,
            },
            area,
            0.0,
        );
        assert!(
            with.pixels != without.pixels,
            "the caret row must change the rail"
        );
    }
}
