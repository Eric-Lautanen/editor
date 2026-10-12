//! Custom title bar: wordmark, one small tab per open file, window buttons.
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
const MARK_X: f32 = 14.0;
const TABS_X: f32 = 92.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleHit {
    /// Empty bar area: the platform turns this into a window drag.
    Drag,
    /// Below the bar: ordinary content, not the title bar's business.
    Client,
    Tab(usize),
    TabClose(usize),
    Minimize,
    Maximize,
    Close,
}

/// Where everything sits (physical px), shared by paint and hit testing.
pub struct Layout {
    pub bar: Rect,
    pub tabs: Vec<Rect>,
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
    let mut closes = Vec::with_capacity(names.len());
    let mut x = TABS_X * scale;
    for w in ideal {
        // When tabs overflow, shrink proportionally down to a small floor so
        // they always fit before the window buttons.
        let w = if shrink < 1.0 {
            // Floor so the FP sum can never spill past the window buttons.
            (w * shrink).floor().max(24.0 * scale)
        } else {
            w.clamp(TAB_MIN_W * scale, TAB_MAX_W * scale)
        };
        tabs.push(Rect::new(x, tab_y, w, TAB_H * scale));
        let cs = 16.0 * scale;
        closes.push(Rect::new(
            x + w - cs - 4.0 * scale,
            tab_y + (TAB_H * scale - cs) / 2.0,
            cs,
            cs,
        ));
        x += w;
    }
    Layout {
        bar: Rect::new(0.0, 0.0, width, bar_h),
        tabs,
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
    let baseline = |top: f32| (top + (m.height() - m.height()) / 2.0 + m.ascent).round();
    let bar_baseline = |r: Rect| (r.y + (r.h - m.height()) / 2.0 + m.ascent).round();
    canvas.draw_text(
        fonts,
        "Readitor",
        MARK_X * scale,
        bar_baseline(lay.bar),
        TextSpec {
            size,
            style: FontStyle::Bold,
            color: theme.muted,
        },
    );

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
        let label = if *dirty {
            format!("{name} •")
        } else {
            name.clone()
        };
        let close_w = 20.0 * scale;
        let label = ellipsize(fonts, &label, r.w - close_w - 10.0 * scale, size);
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
    // close: × glyph
    canvas.draw_text(
        fonts,
        "×",
        lay.close.x + lay.close.w / 2.0 - 5.0 * scale,
        baseline(0.0) + (lay.bar.h - m.height()) / 2.0,
        TextSpec {
            size: 15.0 * scale,
            style: FontStyle::Regular,
            color: if hover == Some(TitleHit::Close) {
                Color::hex(0xFFFFFF)
            } else {
                theme.fg
            },
        },
    );
    canvas.set_clip(None);
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
        // Window buttons at the right edge.
        assert_eq!(hit_test(&lay, lay.close.x + 10.0, 10.0), TitleHit::Close);
        assert_eq!(hit_test(&lay, lay.max.x + 10.0, 10.0), TitleHit::Maximize);
        assert_eq!(hit_test(&lay, lay.min.x + 10.0, 10.0), TitleHit::Minimize);
        // Empty bar area drags the window; below the bar is plain content.
        assert_eq!(hit_test(&lay, 60.0, 4.0), TitleHit::Drag);
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
