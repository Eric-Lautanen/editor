//! Right-click context menu: a small portable popup (copy/cut/paste/select
//! all, copy-all in Preview, open/save/close/help, plus the recent files).
//! Keeps the permanent chrome minimal.

use crate::{
    gfx::{
        canvas::{Canvas, TextSpec},
        color::Color,
        font::{FontStyle, FontSystem},
        geom::Rect,
    },
    layout::theme::Theme,
};
use std::path::PathBuf;

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
    About,
    /// Section caption ("Recent") — painted, never hit.
    Header,
    /// Open the i-th recent file (see [`open`]'s `recent` slice).
    Recent(usize),
}

/// A live menu: item list plus their on-screen rects.
pub struct Menu {
    pub items: Vec<(Action, String)>,
    pub rects: Vec<Rect>,
    pub rect: Rect,
    pub hover: Option<usize>,
}

const ITEM_H: f32 = 26.0;
const PAD_X: f32 = 14.0;
/// How many recent files the menu lists (the session keeps more).
const RECENT_SHOWN: usize = 6;

/// Build a menu for a right-click at `(x, y)` (window space). `can_edit`
/// selects editor items (Edit/Split) vs Preview's copy-all; `recent` adds an
/// "Open recent" section, newest first.
pub fn open(
    fonts: &FontSystem,
    x: f32,
    y: f32,
    can_edit: bool,
    recent: &[PathBuf],
    // Area the menu must stay inside (the window's client rect).
    bounds: Rect,
    scale: f32,
) -> Menu {
    let mut items: Vec<(Action, String)> = if can_edit {
        vec![
            (Action::Copy, "Copy  Ctrl+C".into()),
            (Action::Cut, "Cut  Ctrl+X".into()),
            (Action::Paste, "Paste  Ctrl+V".into()),
            (Action::SelectAll, "Select all  Ctrl+A".into()),
            (Action::Open, "Open…  Ctrl+O".into()),
            (Action::Save, "Save  Ctrl+S".into()),
            (Action::CloseTab, "Close tab  Ctrl+W".into()),
            (Action::Help, "Help  F1".into()),
            (Action::About, "About Readitor  F2".into()),
        ]
    } else {
        vec![
            (Action::CopyAll, "Copy all".into()),
            (Action::Open, "Open…  Ctrl+O".into()),
            (Action::Save, "Save  Ctrl+S".into()),
            (Action::CloseTab, "Close tab  Ctrl+W".into()),
            (Action::Help, "Help  F1".into()),
            (Action::About, "About Readitor  F2".into()),
        ]
    };
    let shown: Vec<&PathBuf> = recent.iter().take(RECENT_SHOWN).collect();
    if !shown.is_empty() {
        items.push((Action::Header, "Recent".into()));
        for (i, p) in shown.iter().enumerate() {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string_lossy().into_owned());
            items.push((Action::Recent(i), name));
        }
    }
    let size = 12.5 * scale;
    let item_h = ITEM_H * scale;
    let text_w = items
        .iter()
        .map(|(_, t)| fonts.measure(FontStyle::Regular, t, size))
        .fold(0.0_f32, f32::max);
    let w = (text_w + 2.0 * PAD_X * scale).min(240.0 * scale);
    let h = items.len() as f32 * item_h + 6.0 * scale;
    // Keep the menu fully on screen (status bar included in `height`).
    let x = x.clamp(0.0, (bounds.w - w).max(0.0));
    let y = y.clamp(0.0, (bounds.h - h).max(0.0));
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
    /// Item under a window-space point, if any. Headers are painted rows but
    /// never answer a click.
    pub fn hit(&self, x: f32, y: f32) -> Option<Action> {
        let p = crate::gfx::geom::Point { x, y };
        self.rects
            .iter()
            .position(|r| r.contains(p))
            .map(|i| self.items[i].0)
            .filter(|a| *a != Action::Header)
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
        for (i, (action, text)) in self.items.iter().enumerate() {
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
                    color: if *action == Action::Header {
                        theme.muted
                    } else {
                        theme.fg
                    },
                },
            );
        }
        canvas.set_clip(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn menu_items_and_hits() {
        let fonts = FontSystem::load_embedded();
        let menu = open(
            &fonts,
            100.0,
            100.0,
            true,
            &[],
            Rect::new(0.0, 0.0, 800.0, 600.0),
            1.0,
        );
        assert_eq!(menu.items.len(), 9);
        assert_eq!(menu.hit(100.0, 100.0), None, "above the first item");
        let first = menu.rects[0];
        assert_eq!(
            menu.hit(first.x + 4.0, first.y + 4.0),
            Some(Action::Copy),
            "first editor item is Copy"
        );
        let preview = open(
            &fonts,
            100.0,
            100.0,
            false,
            &[],
            Rect::new(0.0, 0.0, 800.0, 600.0),
            1.0,
        );
        assert_eq!(preview.items.len(), 6);
        assert_eq!(
            preview.hit(preview.rects[0].x + 4.0, preview.rects[0].y + 4.0),
            Some(Action::CopyAll)
        );
    }

    #[test]
    fn recent_files_list_after_a_caption_that_does_not_click() {
        let fonts = FontSystem::load_embedded();
        let recent = vec![
            PathBuf::from("/home/u/notes.md"),
            PathBuf::from("/home/u/deep/dir/very long file name that goes on.json"),
        ];
        let menu = open(
            &fonts,
            100.0,
            100.0,
            false,
            &recent,
            Rect::new(0.0, 0.0, 800.0, 600.0),
            1.0,
        );
        // Preview menu: 6 standard items + caption + 2 recents.
        assert_eq!(menu.items.len(), 9);
        assert_eq!(menu.items[6], (Action::Header, "Recent".to_string()));
        assert_eq!(menu.items[7].0, Action::Recent(0));
        assert_eq!(menu.items[7].1, "notes.md");
        // The caption row is painted but a click on it does nothing.
        let caption = menu.rects[6];
        assert_eq!(menu.hit(caption.x + 4.0, caption.y + 4.0), None);
        // The first recent answers with its index.
        let first = menu.rects[7];
        assert_eq!(
            menu.hit(first.x + 4.0, first.y + 4.0),
            Some(Action::Recent(0))
        );
        // Only the first RECENT_SHOWN make it in.
        let many: Vec<PathBuf> = (0..10)
            .map(|i| PathBuf::from(format!("/f{i}.md")))
            .collect();
        let capped = open(
            &fonts,
            100.0,
            100.0,
            false,
            &many,
            Rect::new(0.0, 0.0, 800.0, 600.0),
            1.0,
        );
        assert_eq!(capped.items.len(), 6 + 1 + RECENT_SHOWN);
    }

    #[test]
    fn menu_stays_on_screen() {
        let fonts = FontSystem::load_embedded();
        let menu = open(
            &fonts,
            790.0,
            590.0,
            false,
            &[],
            Rect::new(0.0, 0.0, 800.0, 600.0),
            1.0,
        );
        assert!(menu.rect.right() <= 800.0, "overflowed right");
        assert!(menu.rect.bottom() <= 600.0, "overflowed bottom");
    }
}
