use crate::{
    gfx::{
        canvas::{Canvas, TextSpec},
        color::Color,
        font::{FontStyle, FontSystem},
        geom::{Point, Rect},
    },
    layout::{
        display_list::{DisplayList, DrawOp, HitAction},
        theme::Theme,
    },
};

/// Thickness (logical 96-dpi px) of the overlay scrollbars.
pub const BAR_W_LOGICAL: f32 = 8.0;

/// Largest share of its own thickness a bar gives up to the platform's resize
/// strip. Half is the most that can go before the bar stops reading as flush
/// with the window edge, and it keeps the *outer* half of the edge band with
/// the app: the strip then never contains a bar pixel.
pub const GRIP_SHARE: f32 = 0.5;

/// The bar-safe resize strip at `scale`: what a scrollbar keeps clear of, and
/// what a press on a bar asks the platform to give back. Derived rather than
/// chosen — the platform's own band (`band`, physical px, `0.0` where the
/// backend claims no edge grip) capped by [`GRIP_SHARE`] of a bar's thickness,
/// so widening the band on a machine that allows it can never reintroduce the
/// overlap the strip exists to avoid.
pub fn bar_grip(scale: f32, band: f32) -> f32 {
    (BAR_W_LOGICAL * GRIP_SHARE * scale).min(band).max(0.0)
}

/// DPI scale plus the window-edge resize strip that applies where a bar is, in
/// physical px.
///
/// The platform's resize band is the strip along the window border the OS
/// claims for resizing; on a borderless window the client area reaches the very
/// edge, so the overlay scrollbar hugs the edge but stops at the *bar-safe*
/// strip's boundary (see [`bar_grip`]): its outer edge lands on `edge`, keeping
/// the whole bar clickable without making it float away from the window edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneMetrics {
    pub scale: f32,
    /// Bar-safe resize-grip thickness at the window edges (0 where there is
    /// none). Not the platform's full band — see [`bar_grip`].
    pub edge: f32,
}

impl PaneMetrics {
    /// Gap between the window edge and the bar's outer edge: the resize strip,
    /// or the bar's own 3 px breathing room where there is no strip.
    fn margin(&self) -> f32 {
        self.edge.max(3.0 * self.scale)
    }
}

impl PaneMetrics {
    pub const fn new(scale: f32, edge: f32) -> Self {
        Self { scale, edge }
    }
}

/// Geometry of one overlay scrollbar: the visible thumb, the interactive
/// zone around it (wider, for aiming), and the thumb's travel math shared
/// by painting, the cursor, and drag interaction.
#[derive(Clone, Copy, Debug)]
pub struct SbGeom {
    /// The visible thumb.
    pub thumb: Rect,
    /// Interactive strip the pointer can grab (thumb + slack).
    pub zone: Rect,
    /// Leading edge of the thumb's travel (x for horizontal, y vertical).
    pub start: f32,
    /// Travel length available to the thumb's leading edge.
    pub travel: f32,
    /// Max scroll offset this bar controls.
    pub max: f32,
}

impl SbGeom {
    /// Scroll offset that puts the thumb's leading edge at `pos`.
    pub fn offset_for(&self, pos: f32) -> f32 {
        ((pos - self.start) / self.travel.max(1.0) * self.max).clamp(0.0, self.max)
    }
    /// Leading-edge position of the thumb for a scroll offset. The inverse of
    /// `offset_for`, and the single definition of thumb placement — `v_geom`
    /// builds the bar with this rather than repeating the formula.
    pub fn pos_for(&self, offset: f32) -> f32 {
        self.start + (offset / self.max.max(1.0)).clamp(0.0, 1.0) * self.travel
    }
}

/// Vertical bar for a pane of `area` holding `content` px at `scroll`.
/// `None` when everything fits (no bar is shown). The bar hugs the pane's
/// right edge, its outer edge exactly on the resize strip's boundary (see
/// [`PaneMetrics`]).
pub fn v_geom(area: Rect, content: f32, scroll: f32, m: PaneMetrics) -> Option<SbGeom> {
    if content <= area.h + 1.0 {
        return None;
    }
    let scale = m.scale;
    let outer = area.right() - m.margin(); // bar's outer edge
    let w = BAR_W_LOGICAL * scale;
    let track = area.h - BAR_W_LOGICAL * scale;
    let thumb_h = (area.h / content * track).max(32.0 * scale).min(track);
    let mut geom = SbGeom {
        thumb: Rect::new(outer - w, 0.0, w, thumb_h),
        // The zone is the thumb plus slack *inward*; it never reaches past the
        // bar into the resize strip. `sb_at` keys the per-point grip on this
        // zone, so the strip stays clear of every pixel it covers.
        zone: Rect::new(outer - w - 6.0 * scale, area.y, w + 6.0 * scale, area.h),
        start: area.y + BAR_W_LOGICAL * 0.5 * scale,
        travel: track - thumb_h,
        max: content - area.h,
    };
    geom.thumb.y = geom.pos_for(scroll);
    Some(geom)
}

/// Horizontal bar for the no-wrap editor lines. `None` when they fit.
/// Callers pass the gutter-inset area (see [`PreviewView::h_sb`]): the bar
/// lives right of the line-number rail so the gutter never paints over it.
pub fn h_geom(area: Rect, h_max: f32, h_offset: f32, m: PaneMetrics) -> Option<SbGeom> {
    if h_max <= 0.0 || area.w <= 0.0 {
        return None;
    }
    let scale = m.scale;
    let outer = area.bottom() - m.margin(); // bar's outer edge
    let hgt = BAR_W_LOGICAL * scale;
    let track = area.w - BAR_W_LOGICAL * scale;
    let thumb_w = (area.w / (h_max + area.w) * track)
        .max(32.0 * scale)
        .min(track);
    let travel = track - thumb_w;
    Some(SbGeom {
        thumb: Rect::new(
            area.x + BAR_W_LOGICAL * 0.5 * scale + (h_offset / h_max).clamp(0.0, 1.0) * travel,
            outer - hgt,
            thumb_w,
            hgt,
        ),
        zone: Rect::new(area.x, outer - hgt - 6.0 * scale, area.w, hgt + 6.0 * scale),
        start: area.x + BAR_W_LOGICAL * 0.5 * scale,
        travel,
        max: h_max,
    })
}

/// Holds the last laid-out preview and paints it with a scroll offset.
#[derive(Default)]
pub struct PreviewView {
    pub list: DisplayList,
}

/// Scroll position of a pane: the vertical content offset plus the
/// horizontal range of its no-wrap lines (zero for wrapped previews).
#[derive(Clone, Copy, Default)]
pub struct PaneScroll {
    /// Vertical content offset.
    pub y: f32,
    /// Horizontal offset and max of the no-wrap editor lines.
    pub h_offset: f32,
    pub h_max: f32,
    /// Extra left inset of the editor content (the line-number rail), so
    /// text starts right of the gutter instead of sliding under it.
    pub content_x: f32,
}

impl PaneScroll {
    /// A wrapped pane: vertical scrolling only.
    pub fn vertical(y: f32) -> Self {
        Self {
            y,
            ..Self::default()
        }
    }
    /// The editor pane: vertical plus no-wrap horizontal scrolling, content
    /// inset by the gutter rail width.
    pub fn editor(y: f32, h_offset: f32, h_max: f32, content_x: f32) -> Self {
        Self {
            y,
            h_offset,
            h_max,
            content_x,
        }
    }
}

/// A word picked out of the preview by a double-click.
///
/// Preview selection is row-granular (a drag takes whole rows), so a word needs
/// its own shape: the row it sits in, the exact span it occupies — for the
/// highlight — and the text, which is what Copy hands to the clipboard.
#[derive(Clone, Debug, PartialEq)]
pub struct WordSel {
    /// Index into the row list the word was found in.
    pub row: usize,
    /// Painted span, in document space (baseline-relative `y` comes from the
    /// row).
    pub x0: f32,
    pub x1: f32,
    pub text: String,
}

/// A character that a double-click treats as part of a word. Deliberately the
/// editor's idea of one (`is_alphanumeric` plus underscore), so the same
/// gesture picks the same text in both panes.
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// One visual row of the preview: text ops grouped by baseline. Backs
/// drag-selection in Preview mode.
#[derive(Clone, Debug)]
pub struct Row {
    pub baseline: f32,
    pub top: f32,
    pub bottom: f32,
    pub x0: f32,
    pub x1: f32,
    pub text: String,
}

impl PreviewView {
    /// Group the laid-out text ops into visual rows, top to bottom.
    pub fn rows(&self, fonts: &FontSystem) -> Vec<Row> {
        #[cfg(any(test, feature = "mem-census"))]
        crate::mem::begin(crate::mem::Component::DisplayList);
        let mut texts: Vec<(f32, f32, f32, FontStyle, String)> = Vec::new();
        for op in &self.list.ops {
            if let DrawOp::Text {
                x,
                y,
                text,
                size,
                style,
                ..
            } = op
            {
                texts.push((*y, *x, *size, *style, text.clone()));
            }
        }
        texts.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        let mut rows: Vec<Row> = Vec::new();
        for (baseline, x, size, style, text) in texts {
            let w = fonts.measure(style, &text, size);
            match rows.last_mut() {
                Some(r) if r.baseline == baseline => {
                    r.x0 = r.x0.min(x);
                    r.x1 = r.x1.max(x + w);
                    r.top = r.top.min(baseline - size * 0.8);
                    r.bottom = r.bottom.max(baseline + size * 0.3);
                    r.text.push_str(&text);
                }
                _ => rows.push(Row {
                    baseline,
                    top: baseline - size * 0.8,
                    bottom: baseline + size * 0.3,
                    x0: x,
                    x1: x + w,
                    text,
                }),
            }
        }
        #[cfg(any(test, feature = "mem-census"))]
        crate::mem::end();
        rows
    }

    /// Index of the row containing document-space `y`, if any.
    pub fn row_at(&self, rows: &[Row], y: f32) -> Option<usize> {
        rows.iter().position(|r| y >= r.top && y < r.bottom)
    }

    /// The word under a document-space point (`x`, `doc_y`), for the
    /// double-click that selects one. `None` over blank space, over the gap
    /// past a row's text, and over whitespace between words — callers fall
    /// back to the row there, which keeps every double-click selecting
    /// *something*.
    ///
    /// Runs once per double-click, not per paint, so the allocations here
    /// (one `Vec` of chars, one `String` for the word) are not a hot path.
    pub fn word_at(&self, fonts: &FontSystem, rows: &[Row], x: f32, doc_y: f32) -> Option<WordSel> {
        let row = rows
            .iter()
            .position(|r| doc_y >= r.top && doc_y < r.bottom)?;
        // The text op under the pointer, in reverse paint order (the topmost
        // run wins where two overlap, which is how `action_at` resolves hits
        // too). A row can be several ops — bold, code, plain — and the word
        // comes out of whichever one the pointer is actually on.
        let mut hit = None;
        for op in self.list.ops.iter().rev() {
            if let DrawOp::Text {
                x: ox,
                y: oy,
                text,
                size,
                style,
                ..
            } = op
            {
                if text.is_empty() || *oy < rows[row].top || *oy > rows[row].bottom {
                    continue;
                }
                if x >= *ox && x <= *ox + fonts.measure(*style, text, *size) {
                    hit = Some((text.as_str(), *ox, *style, *size));
                    break;
                }
            }
        }
        let (text, ox, style, size) = hit?;
        let chars: Vec<char> = text.chars().collect();
        // Which character is under `x`: walk the advances, which is exact
        // where measuring prefixes and subtracting is not.
        let rel = x - ox;
        let mut acc = 0.0;
        let mut idx = None;
        for (i, ch) in chars.iter().enumerate() {
            let adv = fonts.advance(style, *ch, size);
            if rel < acc + adv {
                idx = Some(i);
                break;
            }
            acc += adv;
        }
        let idx = idx?;
        let clicked = chars[idx];
        // A double-click on a space has nothing to take; punctuation takes its
        // own run, so "..." or "--" highlights as a unit instead of nothing.
        if clicked.is_whitespace() {
            return None;
        }
        let same = |c: char| {
            if is_word_char(clicked) {
                is_word_char(c)
            } else {
                !c.is_whitespace() && !is_word_char(c)
            }
        };
        let mut start = idx;
        while start > 0 && same(chars[start - 1]) {
            start -= 1;
        }
        let mut end = idx + 1;
        while end < chars.len() && same(chars[end]) {
            end += 1;
        }
        let x0 = ox
            + chars[..start]
                .iter()
                .map(|c| fonts.advance(style, *c, size))
                .sum::<f32>();
        let x1 = x0
            + chars[start..end]
                .iter()
                .map(|c| fonts.advance(style, *c, size))
                .sum::<f32>();
        Some(WordSel {
            row,
            x0,
            x1,
            text: chars[start..end].iter().collect(),
        })
    }

    /// Hit action under a document-space point (`x`, `doc_y`), topmost first.
    /// Callers convert view coords with `doc_y = y - area.y + scroll`.
    pub fn action_at(&self, x: f32, doc_y: f32) -> Option<HitAction> {
        let p = Point { x, y: doc_y };
        self.list
            .hits
            .iter()
            .rev()
            .find(|h| h.rect.contains(p))
            .map(|h| h.action.clone())
    }

    /// Paint into `area` (physical px), content shifted by the pane scroll.
    pub fn paint(
        &self,
        canvas: &mut Canvas,
        fonts: &FontSystem,
        theme: &Theme,
        area: Rect,
        scroll: PaneScroll,
        m: PaneMetrics,
    ) {
        let scale = m.scale;
        canvas.set_clip(Some(area));
        canvas.fill_rect(area, theme.bg);
        let (ox, oy) = (
            area.x - scroll.h_offset + scroll.content_x,
            (area.y - scroll.y).round(),
        );
        let (top, bottom) = (area.y - 64.0 * scale, area.bottom() + 64.0 * scale);

        for op in &self.list.ops {
            match op {
                DrawOp::Rect { rect, color } => {
                    let r = Rect::new(rect.x + ox, rect.y + oy, rect.w, rect.h);
                    if r.bottom() >= top && r.y <= bottom {
                        canvas.fill_rect(r, *color);
                    }
                }
                DrawOp::RoundedRect {
                    rect,
                    radius,
                    color,
                } => {
                    let r = Rect::new(rect.x + ox, rect.y + oy, rect.w, rect.h);
                    if r.bottom() >= top && r.y <= bottom {
                        canvas.fill_rounded_rect(r, *radius, *color);
                    }
                }
                DrawOp::Text {
                    x,
                    y,
                    text,
                    size,
                    style,
                    color,
                } => {
                    let by = y + oy;
                    if by >= top && by <= bottom {
                        let spec = TextSpec {
                            size: *size,
                            style: *style,
                            color: *color,
                        };
                        canvas.draw_text(fonts, text, x + ox, by, spec);
                    }
                }
                DrawOp::Line {
                    x0,
                    y0,
                    x1,
                    y1,
                    color,
                } => {
                    let (lx, rx) = (x0.min(*x1) + ox, x0.max(*x1) + ox);
                    let (ty, by) = (y0.min(*y1) + oy, y0.max(*y1) + oy);
                    let thick = scale.max(1.0).round();
                    canvas.fill_rect(
                        Rect::new(lx, ty, (rx - lx).max(thick), (by - ty).max(thick)),
                        *color,
                    );
                }
            }
        }
        self.paint_scrollbar(canvas, theme, area, scroll.y, m);
        // Horizontal scrollbar for the no-wrap editor lines, inset right of
        // the gutter rail (content_x) so the rail never paints over it.
        if let Some(g) = self.h_sb(area, scroll, m) {
            let c = Color::rgba(theme.muted.r, theme.muted.g, theme.muted.b, 110);
            canvas.fill_rounded_rect(g.thumb, g.thumb.h / 2.0, c);
        }
        canvas.set_clip(None);
    }

    /// Thin overlay scrollbar, painted from the shared geometry.
    fn paint_scrollbar(
        &self,
        canvas: &mut Canvas,
        theme: &Theme,
        area: Rect,
        scroll: f32,
        m: PaneMetrics,
    ) {
        let Some(g) = v_geom(area, self.list.height, scroll, m) else {
            return;
        };
        let c = Color::rgba(theme.muted.r, theme.muted.g, theme.muted.b, 110);
        canvas.fill_rounded_rect(g.thumb, g.thumb.w / 2.0, c);
    }

    /// Vertical scrollbar geometry for this pane's content, if scrollable.
    pub fn v_sb(&self, area: Rect, scroll: f32, m: PaneMetrics) -> Option<SbGeom> {
        v_geom(area, self.list.height, scroll, m)
    }

    /// Horizontal scrollbar geometry (no-wrap editor lines), if scrollable.
    /// Inset right of the gutter rail (`scroll.content_x`, 0 for previews)
    /// so the rail — painted after the bar — never covers the thumb.
    pub fn h_sb(&self, area: Rect, scroll: PaneScroll, m: PaneMetrics) -> Option<SbGeom> {
        let bar = Rect::new(
            area.x + scroll.content_x,
            area.y,
            (area.w - scroll.content_x).max(0.0),
            area.h,
        );
        h_geom(bar, scroll.h_max, scroll.h_offset, m)
    }

    /// Hit zones of the overlay scrollbars for `area`, mirroring what
    /// paints there: the track is wider than the visible thumb for easier
    /// aiming. Cursor and click handling share this so the pointer shape
    /// always matches what draws there.
    pub fn scrollbar_hit(&self, area: Rect, scroll: PaneScroll, m: PaneMetrics, p: Point) -> bool {
        self.v_sb(area, scroll.y, m)
            .is_some_and(|g| g.zone.contains(p))
            || self
                .h_sb(area, scroll, m)
                .is_some_and(|g| g.zone.contains(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scale-only metrics: a pane painted away from any window border.
    fn m() -> PaneMetrics {
        PaneMetrics::new(1.0, 0.0)
    }

    #[test]
    fn v_geom_hidden_when_content_fits() {
        let area = Rect::new(0.0, 0.0, 400.0, 300.0);
        assert!(v_geom(area, 300.0, 0.0, m()).is_none());
        assert!(v_geom(area, 1000.0, 0.0, m()).is_some());
    }

    #[test]
    fn v_geom_drag_round_trip() {
        let area = Rect::new(0.0, 40.0, 400.0, 300.0);
        let g = v_geom(area, 3000.0, 500.0, m()).unwrap();
        // Thumb sits where the scroll ratio says, inside the zone.
        assert!(g.thumb.y >= g.start);
        assert!(g.thumb.y + g.thumb.h <= g.start + g.travel + g.thumb.h);
        assert!(g.zone.contains(Point {
            x: g.thumb.x + 1.0,
            y: g.thumb.y + 1.0
        }));
        // Dragging maps the pointer back to a clamped scroll offset.
        assert!((g.offset_for(g.start + g.travel / 2.0) - g.max / 2.0).abs() < 0.5);
        // Clamped at both ends of the travel.
        assert_eq!(g.offset_for(g.start), 0.0);
        assert!((g.offset_for(g.start + g.travel) - g.max).abs() < 0.5);
    }

    #[test]
    fn h_geom_drag_round_trip() {
        let area = Rect::new(0.0, 40.0, 400.0, 300.0);
        assert!(h_geom(area, 0.0, 0.0, m()).is_none());
        let g = h_geom(area, 1200.0, 300.0, m()).unwrap();
        let pos = g.pos_for(300.0);
        assert!((g.offset_for(pos) - 300.0).abs() < 0.5);
        assert_eq!(g.offset_for(g.start), 0.0);
        assert!((g.offset_for(g.start + g.travel) - g.max).abs() < 0.5);
    }

    #[test]
    fn v_geom_thumb_tracks_offset_and_round_trips() {
        let area = Rect::new(0.0, 0.0, 400.0, 300.0);
        let g = v_geom(area, 1000.0, 350.0, m()).unwrap();
        // Thumb sits proportionally along the track.
        assert!((g.thumb.y - g.start - 0.5 * g.travel).abs() < 1.0);
        // Dragging the thumb to a position maps back to the same offset.
        let pos = g.pos_for(350.0);
        assert!((g.offset_for(pos) - 350.0).abs() < 0.5);
        // Clamped at both ends.
        assert_eq!(g.offset_for(g.start - 50.0), 0.0);
        assert_eq!(g.offset_for(g.start + g.travel + 50.0), g.max);
        // The thumb stays inside the pane.
        assert!(g.thumb.y >= area.y && g.thumb.bottom() <= area.bottom());
    }

    #[test]
    fn h_geom_hidden_when_lines_fit() {
        let area = Rect::new(0.0, 0.0, 400.0, 300.0);
        assert!(h_geom(area, 0.0, 0.0, m()).is_none());
        let g = h_geom(area, 600.0, 300.0, m()).unwrap();
        assert!((g.offset_for(g.pos_for(300.0)) - 300.0).abs() < 0.5);
        assert!(g.thumb.x >= area.x && g.thumb.right() <= area.right());
    }

    #[test]
    fn h_bar_starts_right_of_the_gutter_rail() {
        // Regression: the editor's horizontal bar used to span the full pane,
        // so its left end hid behind the line-number rail painted after it.
        let view = PreviewView::default();
        let area = Rect::new(0.0, 40.0, 400.0, 300.0);
        let rail = 48.0;
        let scroll = PaneScroll::editor(0.0, 0.0, 600.0, rail);
        let g = view.h_sb(area, scroll, m()).unwrap();
        assert!(
            g.thumb.x >= area.x + rail,
            "thumb {} starts under the {}px rail",
            g.thumb.x,
            rail
        );
        assert!(
            g.zone.x >= area.x + rail - 0.001,
            "zone {} reaches under the rail",
            g.zone.x
        );
        assert!(g.thumb.right() <= area.right());
        // No rail: the bar still spans the full pane.
        let full = view
            .h_sb(area, PaneScroll::editor(0.0, 0.0, 600.0, 0.0), m())
            .unwrap();
        assert!(full.thumb.x < area.x + rail);
    }

    #[test]
    fn zones_contain_their_thumbs() {
        let area = Rect::new(0.0, 0.0, 400.0, 300.0);
        let v = v_geom(area, 1000.0, 123.0, m()).unwrap();
        assert!(v.zone.contains(Point {
            x: v.thumb.x + 1.0,
            y: v.thumb.y + 1.0
        }));
        let h = h_geom(area, 600.0, 123.0, m()).unwrap();
        assert!(h.zone.contains(Point {
            x: h.thumb.x + 1.0,
            y: h.thumb.y + 1.0
        }));
    }

    #[test]
    fn bar_grip_is_a_share_of_the_bar_capped_by_the_platform_band() {
        // Derived rather than chosen, so the two things that must never
        // overlap are related by construction: half a bar's thickness, capped
        // by whatever the platform claims, and zero where it claims nothing.
        assert_eq!(bar_grip(1.0, 8.0), BAR_W_LOGICAL * GRIP_SHARE);
        assert_eq!(bar_grip(2.0, 16.0), BAR_W_LOGICAL * GRIP_SHARE * 2.0);
        assert_eq!(bar_grip(1.0, 3.0), 3.0, "a narrow band is not widened");
        assert_eq!(bar_grip(1.0, 0.0), 0.0);
        for band in [0.0, 1.0, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0] {
            for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
                let g = bar_grip(scale, band);
                assert!(g <= band + 0.001, "grip {g} exceeds the band {band}");
                assert!(
                    g <= BAR_W_LOGICAL * GRIP_SHARE * scale + 0.001,
                    "grip {g} at scale {scale} eats into the bar"
                );
            }
        }
    }

    #[test]
    fn bars_clear_the_window_edge_resize_grip() {
        // Regression: a borderless window's client area reaches the very edge,
        // so a scrollbar flush with the border sat inside the OS resize strip
        // and its thumb was nearly impossible to press. The bar must end at
        // least one grip inside the window edge, on both axes. A grip wider
        // than the bar is included: the geometry honours whatever strip it is
        // handed, which is what keeps paint and hit testing in agreement.
        let area = Rect::new(0.0, 40.0, 400.0, 300.0);
        for grip in [bar_grip(1.0, 8.0), 12.0] {
            let m = PaneMetrics::new(1.0, grip);
            let v = v_geom(area, 3000.0, 500.0, m).unwrap();
            assert!(v.thumb.right() <= area.right() - grip);
            assert!(v.zone.right() <= area.right() - grip + 0.001);
            // ...and still hugs the edge: the gap is the strip, not a wide inset.
            assert!(area.right() - v.thumb.right() <= grip + 0.001);
            assert!(
                v.zone.contains(Point {
                    x: v.thumb.x + 1.0,
                    y: v.thumb.y + 1.0
                }),
                "the whole thumb stays inside the live zone"
            );
            let h = h_geom(area, 1200.0, 300.0, m).unwrap();
            assert!(h.thumb.bottom() <= area.bottom() - grip);
            assert!(h.zone.bottom() <= area.bottom() - grip + 0.001);
            assert!(area.bottom() - h.thumb.bottom() <= grip + 0.001);
        }
    }
}
