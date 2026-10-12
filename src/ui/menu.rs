//! Right-click context menu: a small portable popup (copy/cut/paste/select
//! all, copy-all in Preview, open/save/close/help). Keeps the permanent
//! chrome minimal.

use crate::{
    gfx::{
        canvas::{Canvas, TextSpec},
        color::Color,
        font::{FontStyle, FontSystem},
        geom::Rect,
    },
    layout::theme::Theme,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Copy,
    Cut,
    Paste,
    SelectAll,
    CopyAll,
    Open,
    Save,
    CloseTab,
    Help,
}

/// A live menu: item list plus their on-screen rects.
pub struct Menu {
    pub items: Vec<(Action, &'static str)>,
    pub rects: Vec<Rect>,
    pub rect: Rect,
    pub hover: Option<usize>,
}

const ITEM_H: f32 = 26.0;
const PAD_X: f32 = 14.0;

/// Build a menu for a right-click at `(x, y)` (window space). `can_edit`
/// selects editor items (Edit/Split) vs Preview's copy-all.
pub fn open(
    fonts: &FontSystem,
    x: f32,
    y: f32,
    can_edit: bool,
    width: f32,
    height: f32,
    scale: f32,
) -> Menu {
    let items: Vec<(Action, &'static str)> = if can_edit {
        vec![
            (Action::Copy, "Copy  Ctrl+C"),
            (Action::Cut, "Cut  Ctrl+X"),
            (Action::Paste, "Paste  Ctrl+V"),
            (Action::SelectAll, "Select all  Ctrl+A"),
            (Action::Open, "Open…  Ctrl+O"),
            (Action::Save, "Save  Ctrl+S"),
            (Action::CloseTab, "Close tab  Ctrl+W"),
            (Action::Help, "Help  F1"),
        ]
    } else {
        vec![
            (Action::CopyAll, "Copy all"),
            (Action::Open, "Open…  Ctrl+O"),
            (Action::Save, "Save  Ctrl+S"),
            (Action::CloseTab, "Close tab  Ctrl+W"),
            (Action::Help, "Help  F1"),
        ]
    };
    let size = 12.5 * scale;
    let item_h = ITEM_H * scale;
    let text_w = items
        .iter()
        .map(|(_, t)| fonts.measure(FontStyle::Regular, t, size))
        .fold(0.0_f32, f32::max);
    let w = (text_w + 2.0 * PAD_X * scale).min(240.0 * scale);
    let h = items.len() as f32 * item_h + 6.0 * scale;
    // Keep the menu fully on screen (status bar included in `height`).
    let x = x.clamp(0.0, (width - w).max(0.0));
    let y = y.clamp(0.0, (height - h).max(0.0));
    let rects = items
        .iter()
        .enumerate()
        .map(|(i, _)| Rect::new(x, y + 3.0 * scale + i as f32 * item_h, w, item_h))
        .collect();
    Menu {
        items,
        rects,
        rect: Rect::new(x, y, w, h),
        hover: None,
    }
}

impl Menu {
    /// Item under a window-space point, if any.
    pub fn hit(&self, x: f32, y: f32) -> Option<Action> {
        let p = crate::gfx::geom::Point { x, y };
        self.rects
            .iter()
            .position(|r| r.contains(p))
            .map(|i| self.items[i].0)
    }

    pub fn paint(&self, canvas: &mut Canvas, fonts: &FontSystem, theme: &Theme, scale: f32) {
        let size = 12.5 * scale;
        canvas.set_clip(Some(self.rect));
        canvas.fill_rounded_rect(self.rect, 6.0 * scale, theme.bg);
        canvas.fill_rounded_rect(
            Rect::new(self.rect.x, self.rect.y, self.rect.w, 1.5 * scale),
            1.0 * scale,
            theme.rule,
        );
        let m = fonts.metrics(FontStyle::Regular, size);
        for (i, (_, text)) in self.items.iter().enumerate() {
            let r = self.rects[i];
            if self.hover == Some(i) {
                canvas.fill_rect(
                    r,
                    Color::rgba(theme.accent.r, theme.accent.g, theme.accent.b, 30),
                );
            }
            let baseline = (r.y + (r.h - m.height()) / 2.0 + m.ascent).round();
            canvas.draw_text(
                fonts,
                text,
                r.x + PAD_X * scale,
                baseline,
                TextSpec {
                    size,
                    style: FontStyle::Regular,
                    color: theme.fg,
                },
            );
        }
        canvas.set_clip(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_items_and_hits() {
        let fonts = FontSystem::load_embedded();
        let menu = open(&fonts, 100.0, 100.0, true, 800.0, 600.0, 1.0);
        assert_eq!(menu.items.len(), 8);
        assert_eq!(menu.hit(100.0, 100.0), None, "above the first item");
        let first = menu.rects[0];
        assert_eq!(
            menu.hit(first.x + 4.0, first.y + 4.0),
            Some(Action::Copy),
            "first editor item is Copy"
        );
        let preview = open(&fonts, 100.0, 100.0, false, 800.0, 600.0, 1.0);
        assert_eq!(preview.items.len(), 5);
        assert_eq!(
            preview.hit(preview.rects[0].x + 4.0, preview.rects[0].y + 4.0),
            Some(Action::CopyAll)
        );
    }

    #[test]
    fn menu_stays_on_screen() {
        let fonts = FontSystem::load_embedded();
        let menu = open(&fonts, 790.0, 590.0, false, 800.0, 600.0, 1.0);
        assert!(menu.rect.right() <= 800.0, "overflowed right");
        assert!(menu.rect.bottom() <= 600.0, "overflowed bottom");
    }
}
