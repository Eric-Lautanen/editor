//! Shared chrome for the full-window overlays — the F1 help legend and the
//! About box.
//!
//! Both dim the page, drop a rounded panel with an accent rule across its top
//! edge, and lay their rows out on the same line grid, so the two read as the
//! same kind of thing rather than two bespoke popups.

use crate::{
    gfx::{
        canvas::Canvas,
        color::Color,
        font::{FontStyle, FontSystem},
        geom::Rect,
    },
    layout::theme::Theme,
};

/// Body text size in physical px, before `scale`.
const BODY_SIZE: f32 = 13.0;
/// Line height as a multiple of the body size.
const LINE_SPACING: f32 = 1.6;
/// Inset from the panel edge to its content, before `scale`.
const PAD: f32 = 22.0;

/// The painted panel plus the metrics its content is laid out on.
pub struct Frame {
    /// The panel rect, in window space.
    pub panel: Rect,
    /// Inset from the panel edge to its content.
    pub pad: f32,
    /// Body text size, physical px.
    pub size: f32,
    /// Distance from a row's top edge down to its text baseline.
    offset: f32,
}

impl Frame {
    /// Baseline y for a row whose top edge sits at `top`.
    pub fn baseline(&self, top: f32) -> f32 {
        (top + self.offset).round()
    }

    /// Left edge of the panel's content column.
    pub fn content_x(&self) -> f32 {
        self.panel.x + self.pad
    }
}

/// The body size and line height both overlays lay out on.
pub fn metrics(scale: f32) -> (f32, f32) {
    let size = BODY_SIZE * scale;
    (size, (size * LINE_SPACING).round())
}

/// The inner padding both overlays use.
pub fn pad(scale: f32) -> f32 {
    PAD * scale
}

/// Dim the page and draw the panel, centred in `window`.
///
/// `panel` is the content-driven `(width, height)`; the caller sizes it from
/// measured text so the box hugs its contents. The panel never starts above
/// `8px`, so a tall legend on a short window still shows its heading.
pub fn frame(
    canvas: &mut Canvas,
    fonts: &FontSystem,
    theme: &Theme,
    window: (f32, f32),
    panel: (f32, f32),
    scale: f32,
) -> Frame {
    let (width, height) = window;
    let (panel_w, panel_h) = panel;
    let (size, line_h) = metrics(scale);
    let pad = pad(scale);
    let m = fonts.metrics(FontStyle::Regular, size);

    let px = ((width - panel_w) / 2.0).round();
    let py = ((height - panel_h) / 2.0).round().max(8.0);

    // Dim the page, then the panel, then the accent rule along its top edge.
    canvas.fill_rect(Rect::new(0.0, 0.0, width, height), Color::rgba(0, 0, 0, 96));
    canvas.fill_rounded_rect(Rect::new(px, py, panel_w, panel_h), 8.0 * scale, theme.bg);
    canvas.fill_rounded_rect(
        Rect::new(px, py, panel_w, 2.0 * scale),
        1.0 * scale,
        theme.accent,
    );

    Frame {
        panel: Rect::new(px, py, panel_w, panel_h),
        pad,
        size,
        // Centre the text's cap-height band inside the row.
        offset: (line_h - m.height()) / 2.0 + m.ascent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::canvas::Canvas;

    #[test]
    fn frame_dims_the_page_and_centres_the_panel() {
        let fonts = FontSystem::load_embedded();
        let theme = Theme::light();
        let (w, h) = (900.0, 520.0);
        let mut c = Canvas::new(900, 520);
        let bg = theme.bg.pack() & 0xFFFFFF;
        c.fill_rect(Rect::new(0.0, 0.0, w, h), theme.bg);

        let f = frame(&mut c, &fonts, &theme, (w, h), (400.0, 300.0), 1.0);
        assert!((f.panel.x - (w - 400.0) / 2.0).abs() < 1.0, "not centred");
        assert!((f.panel.y - (h - 300.0) / 2.0).abs() < 1.0, "not centred");

        // Outside the panel the page is dimmed.
        assert_ne!(c.pixels[10 * 900 + 2] & 0xFFFFFF, bg, "page not dimmed");

        // Inside the padding — clear of both the accent rule and the rounded
        // corner — the panel is plain background again.
        let (x, y) = ((f.panel.x + 12.0) as usize, (f.panel.y + 12.0) as usize);
        assert_eq!(c.pixels[y * 900 + x] & 0xFFFFFF, bg, "panel not painted");

        // The accent rule rides the panel's top edge.
        let x = (f.panel.x + f.panel.w / 2.0) as usize;
        assert_ne!(
            c.pixels[f.panel.y as usize * 900 + x] & 0xFFFFFF,
            bg,
            "accent rule missing"
        );
    }

    #[test]
    fn panel_never_starts_above_the_window_edge() {
        let fonts = FontSystem::load_embedded();
        let theme = Theme::dark();
        let mut c = Canvas::new(400, 200);
        // A panel taller than the window would centre to a negative y.
        let f = frame(&mut c, &fonts, &theme, (400.0, 200.0), (380.0, 600.0), 1.0);
        assert!(f.panel.y >= 8.0, "panel starts off-screen at {}", f.panel.y);
    }
}
