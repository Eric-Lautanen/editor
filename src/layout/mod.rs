pub mod display_list;
pub mod json_tree;
pub mod markdown;
pub mod text;
pub mod theme;

use crate::gfx::font::FontSystem;
use theme::Theme;

/// Margin (logical px) the reading column keeps from the viewport edge.
const COLUMN_PAD: f32 = 32.0;
/// Share of the width the margins leave that the column takes. This is what
/// makes the page reflow as the window grows instead of sitting at a fixed
/// measure.
const COLUMN_SHARE: f32 = 0.82;
/// Narrowest the column may get, in logical px. A window this small has nothing
/// to give back, so below it the column keeps its old width until the viewport
/// itself is narrower (see [`column`]).
const COLUMN_MIN: f32 = 820.0;

/// The reading column for a viewport `viewport_w` physical px wide: its width
/// and its left origin, both physical px.
///
/// Every wrapped pane (Markdown, the JSON tree, plain text) centers its text in
/// a column, and the column tracks the window rather than sitting at a fixed
/// width: a fixed 820 px column leaves more than half of a maximized 1920 px
/// window empty, which reads as a page that never adjusted at all.
///
/// The width is `COLUMN_SHARE` of what the margins leave, floored at
/// `COLUMN_MIN` (so a normal window is unchanged) and capped at the theme's
/// `content_max_width` (so a very wide window does not turn into 250-character
/// lines). Both the floor and the cap are logical, so the column re-scales with
/// DPI and with `Ctrl +/-` zoom along with the text in it.
///
/// The column never exceeds the space the margins leave, however narrow the
/// viewport is, so text cannot run under the window edge.
pub fn column(viewport_w: f32, ctx: &Ctx) -> (f32, f32) {
    let pad = ctx.px(COLUMN_PAD);
    let avail = (viewport_w - 2.0 * pad).max(1.0);
    let col_w = (avail * COLUMN_SHARE)
        .max(ctx.px(COLUMN_MIN))
        .min(ctx.px(ctx.theme.content_max_width))
        .min(avail);
    let x0 = ((viewport_w - col_w) / 2.0)
        .max(pad.min(viewport_w / 4.0))
        .round();
    (col_w, x0)
}

/// Everything layout needs besides the document itself. All outputs are in *physical pixels*.
pub struct Ctx<'a> {
    pub fonts: &'a FontSystem,
    pub theme: &'a Theme,
    /// OS DPI scale (1.0 = 96 dpi).
    pub scale: f32,
    /// User zoom (Ctrl +/-).
    pub zoom: f32,
}

impl Ctx<'_> {
    /// Logical px -> physical px.
    pub fn px(&self, logical: f32) -> f32 {
        logical * self.scale * self.zoom
    }
    pub fn body_size(&self) -> f32 {
        self.px(self.theme.base_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(fonts: &'a FontSystem, theme: &'a Theme) -> Ctx<'a> {
        Ctx {
            fonts,
            theme,
            scale: 1.0,
            zoom: 1.0,
        }
    }

    #[test]
    fn the_column_grows_with_the_window_and_stops_at_the_cap() {
        // The complaint this answers: on a maximized window the text sat in a
        // fixed-width strip with most of the screen empty beside it.
        let fonts = FontSystem::load_embedded();
        let theme = Theme::light();
        let c = ctx(&fonts, &theme);
        let (narrow, _) = column(1100.0, &c);
        let (wide, _) = column(1400.0, &c);
        let (maxed, _) = column(1920.0, &c);
        let (ultra, _) = column(3440.0, &c);
        assert!(narrow < wide && wide < maxed, "{narrow} {wide} {maxed}");
        // Past the cap the column stops growing: a very wide window must not
        // become a 250-character line.
        assert_eq!(ultra, maxed, "capped at the theme's ceiling");
        assert_eq!(maxed, c.px(theme.content_max_width));
        // A maximized window gives the text a real share of itself instead of
        // a third.
        assert!(maxed > 0.7 * 1920.0, "{maxed} of 1920");
    }

    #[test]
    fn the_column_stays_centered_and_inside_a_narrow_window() {
        let fonts = FontSystem::load_embedded();
        let theme = Theme::light();
        let c = ctx(&fonts, &theme);
        // An ordinary window is unchanged: the measure it has always had,
        // centered, with the margins this app has always drawn.
        let (col_w, x0) = column(1100.0, &c);
        assert!((820.0..1100.0).contains(&col_w), "col {col_w}");
        assert!((x0 - (1100.0 - col_w) / 2.0).abs() <= 1.0, "x0 {x0}");
        // A window narrower than the minimum measure gets the space it has,
        // never more: the column must not run under the window edge.
        for w in [280.0, 400.0, 700.0, 900.0] {
            let (col_w, x0) = column(w, &c);
            assert!(col_w <= w - 2.0 * 32.0 + 1.0, "col {col_w} in {w}");
            assert!(x0 >= 0.0 && x0 + col_w <= w, "x0 {x0} col {col_w} in {w}");
        }
    }

    #[test]
    fn the_column_scales_with_dpi_and_zoom_like_the_text_in_it() {
        let fonts = FontSystem::load_embedded();
        let theme = Theme::light();
        let plain = ctx(&fonts, &theme);
        let zoomed = Ctx {
            fonts: &fonts,
            theme: &theme,
            scale: 1.0,
            zoom: 1.25,
        };
        // The floor and the cap are logical, so a zoomed-in window takes the
        // same share of itself and its text keeps the same measure in
        // characters rather than half of the screen.
        let (a, _) = column(2560.0, &plain);
        let (b, _) = column(2560.0, &zoomed);
        assert_eq!(a, theme.content_max_width, "capped at 100%");
        assert_eq!(b, theme.content_max_width * 1.25, "capped at 125% zoom");
    }
}
