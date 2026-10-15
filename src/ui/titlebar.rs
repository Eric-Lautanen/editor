//! Custom title bar: one small tab per open file, plus window buttons.
//! All painting is portable; the platform layer only maps `TitleHit::Drag`
//! to native window dragging (and the OS resize borders).

use crate::{
    gfx::{
        canvas::{Canvas, TextSpec},
        color::Color,
        font::{FontStyle, FontSystem},
        geom::Rect,
    },
    layout::theme::Theme,
};

/// Title bar height in logical px.
pub const HEIGHT: f32 = 34.0;
/// Width of one window button (minimize / maximize / close), logical px.
const BTN_W: f32 = 44.0;
const TAB_H: f32 = 24.0;
const TAB_MAX_W: f32 = 180.0;
const TAB_MIN_W: f32 = 56.0;
/// Left edge of the first tab: no wordmark, so tabs start at the padding.
const TABS_X: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleHit {
    /// Empty bar area: the platform turns this into a window drag.
    Drag,
    /// Below the bar: ordinary content, not the title bar's business.
    Client,
    Tab(usize),
    TabSave(usize),
    TabClose(usize),
    Minimize,
    Maximize,
    Close,
}

/// Where everything sits (physical px), shared by paint and hit testing.
pub struct Layout {
    pub bar: Rect,
    pub tabs: Vec<Rect>,
    /// Save button per tab: `Some` only while that tab has unsaved edits.
    pub saves: Vec<Option<Rect>>,
    pub closes: Vec<Rect>,
    pub min: Rect,
    pub max: Rect,
    pub close: Rect,
}

/// Tab labels: `(name, dirty)`.
pub fn layout(fonts: &FontSystem, names: &[(String, bool)], width: f32, scale: f32) -> Layout {
    let bar_h = HEIGHT * scale;
    let size = 12.0 * scale;
    let avail = (width - TABS_X * scale - 3.0 * BTN_W * scale).max(0.0);
    let ideal: Vec<f32> = names
        .iter()
        .map(|(n, _)| fonts.measure(FontStyle::Regular, n, size) + 36.0 * scale)
        .collect();
    let sum: f32 = ideal.iter().sum();
    let shrink = if sum > avail && sum > 0.0 {
        avail / sum
    } else {
        1.0
    };
    let tab_y = ((bar_h - TAB_H * scale) / 2.0).round();
    let mut tabs = Vec::with_capacity(names.len());
    let mut saves = Vec::with_capacity(names.len());
    let mut closes = Vec::with_capacity(names.len());
    let mut x = TABS_X * scale;
    for (i, w) in ideal.into_iter().enumerate() {
        // When tabs overflow, shrink proportionally down to a small floor so
        // they always fit before the window buttons.
        let w = if shrink < 1.0 {
            // Floor so the FP sum can never spill past the window buttons.
            (w * shrink).floor().max(24.0 * scale)
        } else {
            w.clamp(TAB_MIN_W * scale, TAB_MAX_W * scale)
        };
        tabs.push(Rect::new(x, tab_y, w, TAB_H * scale));
        // Close/save zones: 20 px wide and the tab's full height, so a
        // finger that lands high or low still hits them (the painted glyph
        // stays centered).
        let cs = 20.0 * scale;
        let close = Rect::new(x + w - cs - 4.0 * scale, tab_y, cs, TAB_H * scale);
        // The save button sits just left of the close ×, and only exists for
        // a tab with unsaved edits.
        saves.push(if names[i].1 {
            Some(Rect::new(close.x - cs - 2.0 * scale, close.y, cs, cs))
        } else {
            None
        });
        closes.push(close);
        x += w;
    }
    Layout {
        bar: Rect::new(0.0, 0.0, width, bar_h),
        tabs,
        saves,
        closes,
        min: Rect::new(width - 3.0 * BTN_W * scale, 0.0, BTN_W * scale, bar_h),
        max: Rect::new(width - 2.0 * BTN_W * scale, 0.0, BTN_W * scale, bar_h),
        close: Rect::new(width - BTN_W * scale, 0.0, BTN_W * scale, bar_h),
    }
}

/// Truncate `text` to `max_w` with an ellipsis.
fn ellipsize(fonts: &FontSystem, text: &str, max_w: f32, size: f32) -> String {
    if fonts.measure(FontStyle::Regular, text, size) <= max_w {
        return text.to_string();
    }
    let mut s: String = text.to_string();
    while !s.is_empty() && fonts.measure(FontStyle::Regular, &s, size) > max_w {
        s.pop();
    }
    let mut s = s.trim_end().to_string();
    s.push('…');
    s
}

/// Paint the bar. `hover` drives subtle button/tab highlighting.
pub fn paint(
    canvas: &mut Canvas,
    fonts: &FontSystem,
    theme: &Theme,
    lay: &Layout,
    names: &[(String, bool)],
    active: usize,
    hover: Option<TitleHit>,
) {
    let scale = lay.bar.h / HEIGHT;
    let size = 12.0 * scale;
    canvas.set_clip(Some(lay.bar));
    canvas.fill_rect(lay.bar, theme.code_bg);
    let rule_h = scale.max(1.0).round();
    canvas.fill_rect(
        Rect::new(0.0, lay.bar.bottom() - rule_h, lay.bar.w, rule_h),
        theme.rule,
    );
    let m = fonts.metrics(FontStyle::Regular, size);
    // The old text-drawn × kept this for glyph baselines; the rect-drawn
    // buttons no longer need it, and neither do the tab labels (they use
    // bar_baseline).
    let bar_baseline = |r: Rect| (r.y + (r.h - m.height()) / 2.0 + m.ascent).round();

    for (i, (name, dirty)) in names.iter().enumerate() {
        let r = lay.tabs[i];
        let is_active = i == active;
        let hovered = matches!(hover, Some(TitleHit::Tab(j)) if j == i);
        if is_active {
            canvas.fill_rounded_rect(r, 6.0 * scale, theme.bg);
            canvas.fill_rect(
                Rect::new(r.x, r.bottom() - 2.0 * scale, r.w, 2.0 * scale),
                theme.accent,
            );
        } else if hovered {
            canvas.fill_rounded_rect(
                r,
                6.0 * scale,
                Color::rgba(theme.rule.r, theme.rule.g, theme.rule.b, 90),
            );
        }
        // No dirty dot any more: the save button below is the edited marker.
        let close_w = 20.0 * scale;
        let reserve = if *dirty { 2.0 * close_w } else { close_w };
        let label = ellipsize(fonts, name, r.w - reserve - 10.0 * scale, size);
        canvas.draw_text(
            fonts,
            &label,
            r.x + 8.0 * scale,
            bar_baseline(r),
            TextSpec {
                size,
                style: FontStyle::Regular,
                color: if is_active { theme.fg } else { theme.muted },
            },
        );
        if let Some(sr) = lay.saves[i] {
            paint_save_icon(
                canvas,
                sr,
                theme,
                if is_active { theme.fg } else { theme.muted },
                matches!(hover, Some(TitleHit::TabSave(j)) if j == i),
                scale,
            );
        }
        let close_hover = matches!(hover, Some(TitleHit::TabClose(j)) if j == i);
        canvas.draw_text(
            fonts,
            "×",
            lay.closes[i].x + 2.0 * scale,
            bar_baseline(lay.closes[i]),
            TextSpec {
                size: 13.0 * scale,
                style: FontStyle::Regular,
                color: if close_hover { theme.fg } else { theme.muted },
            },
        );
    }

    // Window buttons: shapes drawn with rects (font-independent), × as a glyph.
    let hover_fill = Color::rgba(theme.fg.r, theme.fg.g, theme.fg.b, 26);
    if hover == Some(TitleHit::Minimize) {
        canvas.fill_rect(lay.min, hover_fill);
    }
    if hover == Some(TitleHit::Maximize) {
        canvas.fill_rect(lay.max, hover_fill);
    }
    if hover == Some(TitleHit::Close) {
        canvas.fill_rect(
            lay.close,
            Color::rgba(theme.error.r, theme.error.g, theme.error.b, 200),
        );
    }
    let cy = lay.bar.h / 2.0;
    let t = scale.max(1.0).round();
    // minimize: horizontal dash
    canvas.fill_rect(
        Rect::new(lay.min.x + 16.0 * scale, cy, 11.0 * scale, t),
        theme.fg,
    );
    // maximize: square outline
    let s = 10.0 * scale;
    let (mx, my) = (lay.max.x + lay.max.w / 2.0 - s / 2.0, cy - s / 2.0);
    canvas.fill_rect(Rect::new(mx, my, s, t), theme.fg);
    canvas.fill_rect(Rect::new(mx, my + s - t, s, t), theme.fg);
    canvas.fill_rect(Rect::new(mx, my, t, s), theme.fg);
    canvas.fill_rect(Rect::new(mx + s - t, my, t, s), theme.fg);
    // close: × drawn from bars, like the dash and square above — the text
    // pipeline's "×" glyph is stroked thinner and sits off-centre (Liberation
    // puts it high and slightly right), so the three buttons did not read as
    // one row of chrome. Two crossed diagonal bars, each as long as the
    // maximize square (10px) and stroked at the same weight `t`, drawn as
    // corner-to-corner runs of unit steps; the round()s make it crisp the
    // same way fill_rect does.
    let cs = 10.0 * scale;
    let (cx, cyc) = (
        lay.close.x + lay.close.w / 2.0,
        lay.close.y + lay.close.h / 2.0,
    );
    let (hx, hy) = (cx - cs / 2.0, cyc - cs / 2.0);
    let close_color = if hover == Some(TitleHit::Close) {
        Color::hex(0xFFFFFF)
    } else {
        theme.fg
    };
    for (dx0, dy0, dx1, dy1) in [
        (hx, hy, hx + cs, hy + cs), // \ diagonal
        (hx, hy + cs, hx + cs, hy), // / diagonal
    ] {
        let steps = (cs / t).ceil().max(1.0) as i32;
        for k in 0..=steps {
            let f = k as f32 / steps as f32;
            let px = (dx0 + (dx1 - dx0) * f).round();
            let py = (dy0 + (dy1 - dy0) * f).round();
            canvas.fill_rect(Rect::new(px, py, t, t), close_color);
        }
    }
    canvas.set_clip(None);
}

/// Paint the floppy-disk "save" affordance inside the square `r`.
///
/// Drawn from rects rather than a glyph: Liberation has no floppy/tray
/// codepoint (checked — no U+1F4BE, U+1F4E5, U+1F5AB), and the window
/// buttons already take this font-independent approach.
fn paint_save_icon(
    canvas: &mut Canvas,
    r: Rect,
    theme: &Theme,
    color: Color,
    hover: bool,
    scale: f32,
) {
    if hover {
        canvas.fill_rounded_rect(
            r,
            4.0 * scale,
            Color::rgba(theme.rule.r, theme.rule.g, theme.rule.b, 90),
        );
    }
    let t = scale.max(1.0).round();
    let s = 11.0 * scale;
    let x = (r.x + (r.w - s) / 2.0).round();
    let y = (r.y + (r.h - s) / 2.0).round();
    // Body outline (the floppy shell).
    canvas.fill_rect(Rect::new(x, y, s, t), color);
    canvas.fill_rect(Rect::new(x, y + s - t, s, t), color);
    canvas.fill_rect(Rect::new(x, y, t, s), color);
    canvas.fill_rect(Rect::new(x + s - t, y, t, s), color);
    // Shutter: the solid metal slider at the top centre.
    canvas.fill_rect(Rect::new(x + s * 0.30, y + t, s * 0.40, s * 0.32), color);
    // Label: an outlined plate across the bottom.
    let (lx, lw) = (x + s * 0.22, s * 0.56);
    let (ly, lh) = (y + s * 0.58, s * 0.30);
    canvas.fill_rect(Rect::new(lx, ly, lw, t), color);
    canvas.fill_rect(Rect::new(lx, ly + lh - t, lw, t), color);
    canvas.fill_rect(Rect::new(lx, ly, t, lh), color);
    canvas.fill_rect(Rect::new(lx + lw - t, ly, t, lh), color);
}

/// What a point in the window (client space) hits. Points below the bar
/// return `Client`; callers usually only ask about in-bar points.
pub fn hit_test(lay: &Layout, x: f32, y: f32) -> TitleHit {
    let p = crate::gfx::geom::Point { x, y };
    if !lay.bar.contains(p) {
        return TitleHit::Client;
    }
    if lay.close.contains(p) {
        return TitleHit::Close;
    }
    if lay.max.contains(p) {
        return TitleHit::Maximize;
    }
    if lay.min.contains(p) {
        return TitleHit::Minimize;
    }
    for (i, r) in lay.closes.iter().enumerate().rev() {
        if r.contains(p) {
            return TitleHit::TabClose(i);
        }
    }
    for (i, r) in lay.saves.iter().enumerate().rev() {
        if r.is_some_and(|r| r.contains(p)) {
            return TitleHit::TabSave(i);
        }
    }
    for (i, r) in lay.tabs.iter().enumerate().rev() {
        if r.contains(p) {
            return TitleHit::Tab(i);
        }
    }
    TitleHit::Drag
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_and_buttons_hit_test() {
        let fonts = FontSystem::load_embedded();
        let names = vec![("a.md".to_string(), false), ("b.json".to_string(), true)];
        let lay = layout(&fonts, &names, 800.0, 1.0);
        assert_eq!(lay.tabs.len(), 2);
        // Inside first tab, away from its close button.
        let hit = hit_test(&lay, lay.tabs[0].x + 4.0, lay.tabs[0].y + 4.0);
        assert_eq!(hit, TitleHit::Tab(0));
        // Close button of the second tab.
        let hit = hit_test(&lay, lay.closes[1].x + 2.0, lay.closes[1].y + 2.0);
        assert_eq!(hit, TitleHit::TabClose(1));
        // Only the dirty tab (index 1) carries a save button, left of its ×.
        assert!(lay.saves[0].is_none(), "clean tab has no save button");
        let save = lay.saves[1].expect("dirty tab has a save button");
        assert!(save.right() <= lay.closes[1].x + 0.5, "save sits left of ×");
        assert_eq!(
            hit_test(&lay, save.x + 2.0, save.y + 2.0),
            TitleHit::TabSave(1)
        );
        // Window buttons at the right edge.
        assert_eq!(hit_test(&lay, lay.close.x + 10.0, 10.0), TitleHit::Close);
        assert_eq!(hit_test(&lay, lay.max.x + 10.0, 10.0), TitleHit::Maximize);
        assert_eq!(hit_test(&lay, lay.min.x + 10.0, 10.0), TitleHit::Minimize);
        // Empty bar area drags the window; below the bar is plain content.
        // (x=4 sits in the left padding before the first tab, which now
        // starts at the edge since the wordmark is gone.)
        assert_eq!(hit_test(&lay, 4.0, 4.0), TitleHit::Drag);
        assert_eq!(
            hit_test(&lay, 60.0, lay.bar.bottom() + 4.0),
            TitleHit::Client
        );
    }

    #[test]
    fn many_tabs_shrink_to_fit() {
        let fonts = FontSystem::load_embedded();
        let names: Vec<(String, bool)> = (0..12)
            .map(|i| (format!("file-{i}-with-a-long-name.md"), false))
            .collect();
        let lay = layout(&fonts, &names, 800.0, 1.0);
        let rightmost = lay.tabs.last().unwrap().right();
        let buttons_left = 800.0 - 3.0 * BTN_W;
        assert!(
            rightmost <= buttons_left,
            "tabs overflow into window buttons: {rightmost} > {buttons_left}"
        );
        assert!(lay.tabs[0].w >= 24.0);
    }
}
