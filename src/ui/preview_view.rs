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
}

impl PaneScroll {
    /// A wrapped pane: vertical scrolling only.
    pub fn vertical(y: f32) -> Self {
        Self {
            y,
            ..Self::default()
        }
    }
    /// The editor pane: vertical plus no-wrap horizontal scrolling.
    pub fn editor(y: f32, h_offset: f32, h_max: f32) -> Self {
        Self { y, h_offset, h_max }
    }
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
        rows
    }

    /// Index of the row containing document-space `y`, if any.
    pub fn row_at(&self, rows: &[Row], y: f32) -> Option<usize> {
        rows.iter().position(|r| y >= r.top && y < r.bottom)
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
        scale: f32,
    ) {
        canvas.set_clip(Some(area));
        canvas.fill_rect(area, theme.bg);
        let (ox, oy) = (area.x - scroll.h_offset, (area.y - scroll.y).round());
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
        self.paint_scrollbar(canvas, theme, area, scroll.y, scale);
        // Horizontal scrollbar for the no-wrap editor lines.
        if scroll.h_max > 0.0 {
            let track = area.w - 8.0 * scale;
            let thumb_w = (area.w / (scroll.h_max + area.w) * track)
                .max(32.0 * scale)
                .min(track);
            let thumb_x = area.x
                + 4.0 * scale
                + (scroll.h_offset / scroll.h_max).clamp(0.0, 1.0) * (track - thumb_w);
            let hgt = 8.0 * scale;
            let c = Color::rgba(theme.muted.r, theme.muted.g, theme.muted.b, 110);
            canvas.fill_rounded_rect(
                Rect::new(thumb_x, area.bottom() - hgt - 3.0 * scale, thumb_w, hgt),
                hgt / 2.0,
                c,
            );
        }
        canvas.set_clip(None);
    }

    /// Thin overlay scrollbar (display only for now; dragging arrives with M5).
    fn paint_scrollbar(
        &self,
        canvas: &mut Canvas,
        theme: &Theme,
        area: Rect,
        scroll: f32,
        scale: f32,
    ) {
        let content = self.list.height;
        if content <= area.h + 1.0 {
            return;
        }
        let track = area.h - 8.0 * scale;
        let thumb_h = (area.h / content * track).max(32.0 * scale).min(track);
        let max_scroll = content - area.h;
        let thumb_y =
            area.y + 4.0 * scale + (scroll / max_scroll).clamp(0.0, 1.0) * (track - thumb_h);
        let w = 8.0 * scale;
        let c = Color::rgba(theme.muted.r, theme.muted.g, theme.muted.b, 110);
        canvas.fill_rounded_rect(
            Rect::new(area.right() - w - 3.0 * scale, thumb_y, w, thumb_h),
            w / 2.0,
            c,
        );
    }

    /// Hit zones of the overlay scrollbars for `area`, mirroring
    /// `paint_scrollbar` + the horizontal bar's layout. Cursor and click
    /// handling share this so the pointer shape always matches what draws
    /// there: the track is wider than the visible thumb for easier aiming.
    pub fn scrollbar_hit(&self, area: Rect, scroll: PaneScroll, scale: f32, p: Point) -> bool {
        let sb_margin = 3.0 * scale;
        let v_w = 8.0 * scale + sb_margin + 6.0 * scale; // thumb + margin + grab slack
        let v = Rect::new(area.right() - v_w, area.y, v_w, area.h);
        if v.contains(p) {
            return true;
        }
        if scroll.h_max > 0.0 {
            let h_h = 8.0 * scale + sb_margin + 6.0 * scale;
            let h = Rect::new(area.x, area.bottom() - h_h, area.w, h_h);
            if h.contains(p) {
                return true;
            }
        }
        false
    }
}
