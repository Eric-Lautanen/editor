use crate::{
    core::{
        buffer::{Buffer, Pos},
        file_kind::FileKind,
    },
    error::Result,
    gfx::{canvas::Canvas, font::FontSystem, geom::Point, geom::Rect},
    layout::{
        self,
        display_list::{DisplayList, DrawOp},
        theme::Theme,
        Ctx,
    },
    parse::{json, jsonl, markdown},
    platform::{Handler, Host, NullHost},
    ui::{
        editor_view::EditorView,
        event::{CursorShape, Event, Key, Modifiers, MouseButton},
        gutter, help, menu,
        preview_view::{PaneScroll, PreviewView, Row as PreviewRow},
        scroll::ScrollState,
        statusbar,
        titlebar::{self, TitleHit},
    },
};
use std::{
    collections::HashSet,
    ops::{Deref, DerefMut},
    path::PathBuf,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Preview,
    Edit,
    Split,
}

/// Caret blink half-period, in 40 ms `Event::Tick`s (~0.5 s).
const BLINK_TICKS: u32 = 12;

/// One open file: everything that is per-document lives here.
pub struct Tab {
    pub path: Option<PathBuf>,
    pub kind: FileKind,
    pub buffer: Buffer,
    pub editor: EditorView,
    /// Plain-text pane backing Edit mode and the bottom half of Split.
    pub editor_pane: PreviewView,
    /// Collapsed JSON tree nodes (pre-order ids, stable across toggles).
    pub collapsed: HashSet<usize>,
    /// Right-side status suffix set by layout (`error 2:5`, `3 records`, …).
    pub status_extra: String,
    pub scroll: ScrollState,
    /// Horizontal scroll of the editor pane (no-wrap lines) and its max.
    pub h_scroll: f32,
    pub h_max: f32,
    /// File modification time as last seen or written (reload detection).
    pub mtime: Option<std::time::SystemTime>,
}

impl Tab {
    fn untitled() -> Self {
        Self {
            path: None,
            kind: FileKind::PlainText,
            buffer: Buffer::from_text(""),
            editor: EditorView::default(),
            editor_pane: PreviewView::default(),
            collapsed: HashSet::new(),
            status_extra: String::new(),
            scroll: ScrollState::default(),
            h_scroll: 0.0,
            h_max: 0.0,
            mtime: None,
        }
    }

    fn from_path(p: PathBuf) -> Result<Self> {
        Ok(Self {
            kind: FileKind::from_path(&p),
            buffer: Buffer::from_text(&std::fs::read_to_string(&p)?),
            mtime: std::fs::metadata(&p).and_then(|m| m.modified()).ok(),
            path: Some(p),
            ..Self::untitled()
        })
    }

    /// Short name for the title bar and window title.
    pub fn name(&self) -> &str {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("Untitled")
    }
}

pub struct App {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub mode: Mode,
    pub dark: bool,
    pub theme: Theme,
    pub fonts: FontSystem,
    /// Document preview of the active tab (rebuilt on relayout).
    pub preview: PreviewView,
    /// Left mouse button held in an editor pane: anchor and latest caret.
    drag_anchor: Option<Pos>,
    /// Window size in physical px, and OS DPI scale.
    size: (usize, usize),
    scale: f32,
    zoom: f32,
    layout_dirty: bool,
    /// F1 help overlay visible.
    help_open: bool,
    /// Live right-click context menu, if any.
    menu: Option<menu::Menu>,
    /// Second Ctrl+W on a dirty tab discards it; any other action clears.
    close_armed: bool,
    /// Title-bar hover state (tab / window-button highlighting).
    hover_title: Option<TitleHit>,
    /// Clickable status-bar segments, refreshed on every paint.
    status_buttons: Vec<statusbar::StatusButton>,
    /// Visual rows of the active tab's preview (for drag-selection).
    preview_rows: Vec<PreviewRow>,
    /// Preview selection: (anchor row, caret row).
    preview_sel: Option<(usize, usize)>,
    /// Left button held over the preview (drag-selecting).
    preview_drag: bool,
    /// Last mouse position (window space), for Tick-driven auto-scroll.
    last_mouse: (f32, f32),
    /// User pressed Ctrl+T (or passed --dark): stop following the OS theme.
    theme_manual: bool,
    /// Tick counter for the periodic file-changed check.
    ticks: u32,
    /// Caret blink phase: 40 ms ticks; the caret hides every other
    /// [`BLINK_TICKS`] window and any caret activity resets it to visible.
    blink: u32,
}

/// The active tab is what all the existing per-document code touches.
impl Deref for App {
    type Target = Tab;
    fn deref(&self) -> &Tab {
        &self.tabs[self.active]
    }
}
impl DerefMut for App {
    fn deref_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }
}

impl App {
    pub fn new(path: Option<PathBuf>) -> Result<Self> {
        let tab = match &path {
            Some(p) => Tab::from_path(p.clone())?,
            None => Tab::untitled(),
        };
        Ok(Self {
            tabs: vec![tab],
            active: 0,
            mode: Mode::Preview,
            dark: false,
            theme: Theme::light(),
            fonts: FontSystem::load_embedded(),
            preview: PreviewView::default(),
            drag_anchor: None,
            size: (1100, 800),
            scale: 1.0,
            zoom: 1.0,
            layout_dirty: true,
            help_open: false,
            menu: None,
            close_armed: false,
            hover_title: None,
            status_buttons: Vec::new(),
            preview_rows: Vec::new(),
            preview_sel: None,
            preview_drag: false,
            last_mouse: (0.0, 0.0),
            theme_manual: false,
            ticks: 0,
            blink: 0,
        })
    }

    pub fn set_dark(&mut self, dark: bool) {
        self.dark = dark;
        self.theme = if dark { Theme::dark() } else { Theme::light() };
        self.layout_dirty = true;
    }

    /// Explicit user override (CLI flag / Ctrl+T): also stops following the
    /// OS theme.
    pub fn set_dark_manual(&mut self, dark: bool) {
        self.theme_manual = true;
        self.set_dark(dark);
    }

    pub fn title(&self) -> String {
        format!(
            "{}{} — Readitor",
            self.name(),
            if self.buffer.dirty { " •" } else { "" }
        )
    }
    // ------------------------------------------------------------ tabs

    /// Open `path` as a tab: focus the existing tab when re-opened, else load
    /// it as a new one. Read failures land in the status bar, never a crash.
    pub fn open_path(&mut self, path: PathBuf, host: &mut dyn Host) {
        self.close_armed = false;
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(path.as_path()))
        {
            self.switch_to(i);
        } else {
            match Tab::from_path(path) {
                Ok(tab) => {
                    // A lone, untouched Untitled tab is a placeholder: the
                    // first opened file replaces it instead of stacking.
                    let placeholder = self.tabs.len() == 1
                        && self.path.is_none()
                        && !self.buffer.dirty
                        && self.buffer.text().is_empty();
                    if placeholder {
                        self.tabs[0] = tab;
                    } else {
                        self.tabs.push(tab);
                        self.active = self.tabs.len() - 1;
                    }
                    self.clear_preview_sel();
                    self.layout_dirty = true;
                }
                Err(e) => self.status_extra = format!("open failed: {e}"),
            }
        }
        host.request_redraw();
    }

    fn switch_to(&mut self, i: usize) {
        if i == self.active || i >= self.tabs.len() {
            return;
        }
        self.active = i;
        self.close_armed = false;
        self.clear_preview_sel();
        self.layout_dirty = true;
    }

    /// Close the active tab. A dirty tab warns once; the second press (or a
    /// second click on its ×) discards the edits.
    fn close_tab(&mut self, host: &mut dyn Host) {
        if self.buffer.dirty && !self.close_armed {
            self.close_armed = true;
            self.status_extra = "unsaved edits — Ctrl+W again to discard".into();
            host.request_redraw();
            return;
        }
        self.tabs.remove(self.active);
        if self.tabs.is_empty() {
            self.tabs.push(Tab::untitled());
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.close_armed = false;
        self.clear_preview_sel();
        self.layout_dirty = true;
        host.request_redraw();
    }

    fn cycle_tab(&mut self, back: bool) {
        if self.tabs.len() < 2 {
            return;
        }
        let n = self.tabs.len();
        self.active = if back {
            (self.active + n - 1) % n
        } else {
            (self.active + 1) % n
        };
        self.close_armed = false;
        self.clear_preview_sel();
        self.layout_dirty = true;
    }

    /// `(name, dirty)` per tab, for the title bar.
    fn tab_labels(&self) -> Vec<(String, bool)> {
        self.tabs
            .iter()
            .map(|t| (t.name().to_string(), t.buffer.dirty))
            .collect()
    }

    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            fonts: &self.fonts,
            theme: &self.theme,
            scale: self.scale,
            zoom: self.zoom,
        }
    }

    fn viewport(&self) -> Rect {
        let tb = titlebar::HEIGHT * self.scale;
        let sb = statusbar::HEIGHT * self.scale;
        Rect::new(
            0.0,
            tb,
            self.size.0 as f32,
            (self.size.1 as f32 - sb - tb).max(0.0),
        )
    }

    /// Split pane rectangles: document preview on the left, editor on the
    /// right, 50/50 with a divider strip at the seam.
    fn split_panes(&self) -> (Rect, Rect) {
        let view = self.viewport();
        let mid = view.x + (view.w * 0.5).round();
        let dw = self.divider_w();
        (
            Rect::new(view.x, view.y, (mid - dw - view.x).max(0.0), view.h),
            Rect::new(mid, view.y, (view.right() - mid).max(0.0), view.h),
        )
    }

    /// The vertical rule between the split panes.
    fn split_divider(&self) -> Rect {
        let view = self.viewport();
        let mid = view.x + (view.w * 0.5).round();
        Rect::new(mid - self.divider_w(), view.y, self.divider_w(), view.h)
    }

    fn divider_w(&self) -> f32 {
        (2.0 * self.scale).round().max(1.0)
    }

    /// The editor pane: the whole viewport in Edit, the right half in Split.
    fn editor_area(&self) -> Rect {
        match self.mode {
            Mode::Edit => self.viewport(),
            Mode::Split => self.split_panes().1,
            Mode::Preview => self.viewport(),
        }
    }

    /// True when `x, y` lands in an editor pane (Edit whole viewport, Split
    /// right half only). Returns the pane's document-space point (x includes
    /// the horizontal scroll of the no-wrap lines).
    fn editor_point(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        match self.mode {
            Mode::Edit => {
                let view = self.viewport();
                Some((x - view.x + self.h_scroll, y - view.y + self.scroll.offset))
            }
            Mode::Split => {
                let right = self.split_panes().1;
                (x >= right.x).then_some((
                    x - right.x + self.h_scroll,
                    y - right.y + self.scroll.offset,
                ))
            }
            Mode::Preview => None,
        }
    }

    /// Preview areas that carry tree toggles: full viewport in Preview, the
    /// left pane in Split. Returns the preview list's document-space point.
    fn toggle_point(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let view = self.viewport();
        match self.mode {
            Mode::Preview => Some((x - view.x, y - view.y + self.scroll.offset)),
            Mode::Split if x < self.split_panes().0.right() => {
                Some((x - view.x, y - view.y + self.scroll.offset))
            }
            _ => None,
        }
    }

    fn relayout(&mut self) {
        // Keep the reader's place when width/zoom changes reflow the document.
        let ctx = Ctx {
            fonts: &self.fonts,
            theme: &self.theme,
            scale: self.scale,
            zoom: self.zoom,
        };
        let w = match self.mode {
            // The preview list must wrap at the pane it is painted into, not
            // the whole window, or Split clips lines and mis-centers them.
            Mode::Split => self.split_panes().0.w,
            _ => self.size.0 as f32,
        };
        let view_h = self.viewport().h;
        let edit_w = self.editor_area().w;
        let tab = &mut self.tabs[self.active];
        let ratio = if tab.scroll.content_height > 0.0 {
            tab.scroll.offset / tab.scroll.content_height
        } else {
            0.0
        };
        let list = match tab.kind {
            FileKind::Json => {
                Self::layout_json(&tab.buffer, &tab.collapsed, &mut tab.status_extra, w, &ctx)
            }
            FileKind::JsonLines => {
                Self::layout_jsonl(&tab.buffer, &tab.collapsed, &mut tab.status_extra, w, &ctx)
            }
            FileKind::Markdown => {
                tab.status_extra.clear();
                let doc = markdown::parse(&tab.buffer.text());
                layout::markdown::layout(&doc, w, &ctx)
            }
            FileKind::PlainText => layout::text::layout_plain(&tab.buffer, w, &ctx, true),
        };
        if self.mode != Mode::Preview {
            // The editor pane never wraps: one row per line, h-scrolled.
            tab.editor_pane.list = layout::text::layout_plain(&tab.buffer, edit_w, &ctx, false);
            let mut extent = 0.0f32;
            for op in &tab.editor_pane.list.ops {
                if let DrawOp::Text {
                    x,
                    text,
                    size,
                    style,
                    ..
                } = op
                {
                    extent = extent.max(x + ctx.fonts.measure(*style, text, *size));
                }
            }
            tab.h_max = (extent + 32.0 * self.scale - edit_w).max(0.0);
            tab.h_scroll = tab.h_scroll.min(tab.h_max);
        }
        let content = match self.mode {
            Mode::Preview => list.height,
            Mode::Edit => tab.editor_pane.list.height,
            Mode::Split => list.height.max(tab.editor_pane.list.height),
        };
        tab.scroll.content_height = content;
        tab.scroll.viewport_height = view_h;
        tab.scroll.offset = ratio * content;
        tab.scroll.scroll_by(0.0);
        self.preview.list = list;
        self.preview_rows = self.preview.rows(&self.fonts);
        self.layout_dirty = false;
    }

    /// Buffer changed: live re-render the document pane and keep the caret on
    /// screen. (Split's top pane re-renders through `relayout` on paint.)
    fn after_edit(&mut self) {
        self.layout_dirty = true;
        self.poke_caret();
        self.follow_caret();
    }

    /// Caret blink phase: visible for the first half of each blink window.
    fn caret_visible(&self) -> bool {
        (self.blink / BLINK_TICKS).is_multiple_of(2)
    }

    /// Any caret activity restarts the blink, solid.
    fn poke_caret(&mut self) {
        self.blink = 0;
    }

    /// Keyboard dispatch: app chords, then editing keys when the editor pane
    /// is visible, then window scroll navigation.
    fn on_key(&mut self, key: Key, mods: Modifiers, host: &mut dyn Host) {
        if self.close_armed && !(mods.ctrl && key == Key::Char('w')) {
            self.close_armed = false;
        }
        let page = self.scroll.viewport_height * 0.9;
        let step = 48.0 * self.scale;
        match key {
            Key::Escape => {
                self.menu = None;
                self.help_open = false;
                self.clear_preview_sel();
            }
            Key::F(1) => self.help_open = !self.help_open,
            Key::Char('q') if mods.ctrl => host.quit(),
            Key::Char('w') if mods.ctrl => self.close_tab(host),
            Key::Char('o') if mods.ctrl => {
                if let Some(p) = host.open_file_dialog() {
                    self.open_path(p, host);
                }
            }
            Key::Tab if mods.ctrl => self.cycle_tab(mods.shift),
            Key::Char('+' | '=') if mods.ctrl => self.zoom_by(1.1),
            Key::Char('-') if mods.ctrl => self.zoom_by(1.0 / 1.1),
            Key::Char('0') if mods.ctrl => {
                self.zoom = 1.0;
                self.layout_dirty = true;
            }
            Key::Char('t') if mods.ctrl => {
                self.theme_manual = true;
                self.set_dark(!self.dark);
            }
            Key::Char('s') if mods.ctrl => self.save(host),
            Key::Char('r') if mods.ctrl => self.reload_file(host),
            Key::Char('e') if mods.ctrl => {
                self.mode = match self.mode {
                    Mode::Preview => Mode::Edit,
                    Mode::Edit => Mode::Split,
                    Mode::Split => Mode::Preview,
                };
                self.clear_preview_sel();
                self.layout_dirty = true;
            }
            // -- editing chords (need the editor pane) -----------------------
            Key::Char('a') if mods.ctrl && self.mode != Mode::Preview => {
                let tab = &mut self.tabs[self.active];
                tab.editor.select_all(&tab.buffer);
                self.follow_caret();
            }
            Key::Char('z') if mods.ctrl && self.mode != Mode::Preview => {
                let tab = &mut self.tabs[self.active];
                if tab.editor.undo(&mut tab.buffer) {
                    self.after_edit();
                } else {
                    return;
                }
            }
            Key::Char('y') if mods.ctrl && self.mode != Mode::Preview => {
                let tab = &mut self.tabs[self.active];
                if tab.editor.redo(&mut tab.buffer) {
                    self.after_edit();
                } else {
                    return;
                }
            }
            Key::Char('v') if mods.ctrl && self.mode != Mode::Preview => {
                if let Some(text) = host.clipboard_get() {
                    let tab = &mut self.tabs[self.active];
                    tab.editor.paste(&mut tab.buffer, &text);
                    self.after_edit();
                }
            }
            Key::Char('c') if mods.ctrl => {
                if let Some(text) = self.preview_selection_text() {
                    host.clipboard_set(&text);
                } else if let Some(text) = self.editor.copy(&self.buffer) {
                    host.clipboard_set(&text);
                }
            }
            Key::Char('x') if mods.ctrl && self.mode != Mode::Preview => {
                let tab = &mut self.tabs[self.active];
                if let Some(text) = tab.editor.cut(&mut tab.buffer) {
                    host.clipboard_set(&text);
                    self.after_edit();
                } else {
                    return;
                }
            }
            // -- plain editing keys ------------------------------------------
            Key::Backspace | Key::Delete
                if self.mode != Mode::Preview && !mods.ctrl && !mods.alt =>
            {
                let tab = &mut self.tabs[self.active];
                tab.editor.delete_key(&mut tab.buffer, key == Key::Delete);
                self.after_edit();
            }
            Key::Enter | Key::Tab if self.mode != Mode::Preview && !mods.ctrl && !mods.alt => {
                let text = if key == Key::Enter { "\n" } else { "    " };
                let tab = &mut self.tabs[self.active];
                tab.editor.insert_text(&mut tab.buffer, text);
                self.after_edit();
            }
            Key::Up | Key::Down | Key::Left | Key::Right | Key::Home | Key::End
                if self.mode != Mode::Preview =>
            {
                let tab = &mut self.tabs[self.active];
                if tab.editor.move_key(&tab.buffer, key, mods) {
                    self.follow_caret();
                } else {
                    return;
                }
            }
            // -- window scroll -----------------------------------------------
            Key::Down => self.scroll.scroll_by(step),
            Key::Up => self.scroll.scroll_by(-step),
            Key::PageDown => self.scroll.scroll_by(page),
            Key::PageUp => self.scroll.scroll_by(-page),
            Key::Home => self.scroll.offset = 0.0,
            Key::End => self.scroll.offset = self.scroll.max(),
            _ => return,
        }
        host.request_redraw();
    }

    fn on_mouse_down(&mut self, x: f32, y: f32, button: MouseButton, host: &mut dyn Host) {
        // A live menu consumes the click (item or dismiss).
        if let Some(m) = &self.menu {
            let action = m.hit(x, y);
            self.menu = None;
            if let Some(a) = action {
                self.run_menu_action(a, host);
            }
            host.request_redraw();
            return;
        }
        // Any click dismisses the help overlay.
        if self.help_open {
            self.help_open = false;
            host.request_redraw();
            return;
        }
        // Title bar: tabs and window buttons.
        if y < titlebar::HEIGHT * self.scale {
            let lay = titlebar::layout(
                &self.fonts,
                &self.tab_labels(),
                self.size.0 as f32,
                self.scale,
            );
            match titlebar::hit_test(&lay, x, y) {
                TitleHit::Tab(i) => self.switch_to(i),
                TitleHit::TabClose(i) => {
                    self.active = i;
                    self.close_tab(host);
                }
                TitleHit::Minimize => host.minimize(),
                TitleHit::Maximize => host.toggle_maximize(),
                TitleHit::Close => host.quit(),
                TitleHit::Drag | TitleHit::Client => {}
            }
            host.request_redraw();
            return;
        }
        // Status bar: clickable mode/theme/help buttons.
        if y >= self.viewport().bottom() {
            let p = Point { x, y };
            if let Some(b) = self.status_buttons.iter().find(|b| b.rect.contains(p)) {
                match b.id {
                    0 => self.mode = Mode::Preview,
                    1 => self.mode = Mode::Edit,
                    2 => self.mode = Mode::Split,
                    3 => self.help_open = !self.help_open,
                    // Same as Ctrl+T: manual toggle, stops following the OS.
                    _ => {
                        self.theme_manual = true;
                        self.set_dark(!self.dark);
                    }
                }
                self.clear_preview_sel();
                self.layout_dirty = true;
            }
            host.request_redraw();
            return;
        }
        // Right-click opens the context menu (editor items in Edit/Split).
        if button == MouseButton::Right {
            let can_edit = self.mode != Mode::Preview;
            self.menu = Some(menu::open(
                &self.fonts,
                x,
                y,
                can_edit,
                self.size.0 as f32,
                self.size.1 as f32,
                self.scale,
            ));
            host.request_redraw();
            return;
        }
        if button != MouseButton::Left {
            return;
        }
        if self.layout_dirty {
            self.relayout();
        }
        // Editor panes take precedence in Edit/Split; Preview and the Split top
        // pane stay read-only hit targets (toggles, links).
        if let Some((dx, doc_y)) = self.editor_point(x, y) {
            let ctx = self.ctx();
            let pos =
                self.editor
                    .caret_for_point(&self.buffer, dx, doc_y, self.editor_area().w, &ctx);
            self.editor.caret = pos;
            self.editor.anchor = None;
            self.drag_anchor = Some(pos);
            self.poke_caret();
            host.request_redraw();
            return;
        }
        // Preview panes: toggles/links first, then drag-selection of rows.
        if !self.preview_click(x, y, host) {
            if let Some((_, dy)) = self.toggle_point(x, y) {
                let row = self.preview.row_at(&self.preview_rows, dy);
                self.preview_sel = row.map(|r| (r, r));
                self.preview_drag = row.is_some();
            } else {
                self.preview_sel = None;
            }
            host.request_redraw();
        }
    }

    /// Run a context-menu action (shared with the keyboard chords).
    fn run_menu_action(&mut self, action: menu::Action, host: &mut dyn Host) {
        match action {
            menu::Action::Open => {
                if let Some(p) = host.open_file_dialog() {
                    self.open_path(p, host);
                }
            }
            menu::Action::Save => self.save(host),
            menu::Action::CloseTab => self.close_tab(host),
            menu::Action::Help => self.help_open = true,
            menu::Action::CopyAll => host.clipboard_set(&self.buffer.text()),
            menu::Action::Copy => {
                if let Some(text) = self.editor.copy(&self.buffer) {
                    host.clipboard_set(&text);
                }
            }
            menu::Action::Cut => {
                let tab = &mut self.tabs[self.active];
                if let Some(text) = tab.editor.cut(&mut tab.buffer) {
                    host.clipboard_set(&text);
                    self.after_edit();
                }
            }
            menu::Action::Paste => {
                if let Some(text) = host.clipboard_get() {
                    let tab = &mut self.tabs[self.active];
                    tab.editor.paste(&mut tab.buffer, &text);
                    self.after_edit();
                }
            }
            menu::Action::SelectAll => {
                let tab = &mut self.tabs[self.active];
                tab.editor.select_all(&tab.buffer);
                self.follow_caret();
            }
        }
    }

    fn on_mouse_move(&mut self, x: f32, y: f32, host: &mut dyn Host) {
        self.last_mouse = (x, y);
        // Menu hover highlighting.
        if let Some(m) = &mut self.menu {
            let p = Point { x, y };
            let hover = m.rects.iter().position(|r| r.contains(p));
            if m.hover != hover {
                m.hover = hover;
                host.request_redraw();
            }
            return;
        }
        // Title-bar hover highlighting.
        let in_bar = y < titlebar::HEIGHT * self.scale;
        let hit = if in_bar {
            let lay = titlebar::layout(
                &self.fonts,
                &self.tab_labels(),
                self.size.0 as f32,
                self.scale,
            );
            Some(titlebar::hit_test(&lay, x, y))
        } else {
            None
        };
        if self.hover_title != hit {
            self.hover_title = hit;
            host.request_redraw();
        }
        // Preview drag: extend the row selection.
        if self.preview_drag {
            if let Some((_, dy)) = self.toggle_point(x, y) {
                if let Some(row) = self.preview.row_at(&self.preview_rows, dy) {
                    if let Some((anchor, _)) = self.preview_sel {
                        self.preview_sel = Some((anchor, row));
                        host.request_redraw();
                    }
                }
            }
            return;
        }
        let Some(anchor) = self.drag_anchor else {
            return;
        };
        if let Some((dx, doc_y)) = self.editor_point(x, y) {
            let ctx = self.ctx();
            let pos =
                self.editor
                    .caret_for_point(&self.buffer, dx, doc_y, self.editor_area().w, &ctx);
            self.editor.caret = pos;
            self.editor.anchor = Some(anchor);
            self.poke_caret();
            host.request_redraw();
        }
    }

    fn on_mouse_up(&mut self, x: f32, y: f32, button: MouseButton, host: &mut dyn Host) {
        let _ = (x, y);
        if button == MouseButton::Left {
            self.drag_anchor = None;
            self.preview_drag = false;
        }
        host.request_redraw();
    }

    /// Host heartbeat: auto-scroll (and keep extending the selection) while a
    /// drag is held near a pane edge; every ~2 s, watch for on-disk changes.
    fn on_tick(&mut self, host: &mut dyn Host) {
        self.ticks = self.ticks.wrapping_add(1);
        // Caret blink (Edit/Split only; Preview paints no caret).
        if self.mode != Mode::Preview {
            let was = self.caret_visible();
            self.blink = self.blink.wrapping_add(1);
            if self.caret_visible() != was {
                host.request_redraw();
            }
        }
        if self.ticks.is_multiple_of(50) && self.drag_anchor.is_none() && !self.preview_drag {
            self.check_file_changed(host);
        }
        let (mx, my) = self.last_mouse;
        let margin = 28.0 * self.scale;
        let step = 14.0 * self.scale;
        if self.drag_anchor.is_some() {
            let area = self.editor_area();
            let dy = if my < area.y + margin {
                -step
            } else if my > area.bottom() - margin {
                step
            } else {
                return;
            };
            self.scroll.scroll_by(dy);
            if let Some((dx, doc_y)) = self.editor_point(mx, my) {
                let ctx = self.ctx();
                let pos = self.editor.caret_for_point(
                    &self.buffer,
                    dx,
                    doc_y,
                    self.editor_area().w,
                    &ctx,
                );
                self.editor.caret = pos;
                self.editor.anchor = self.drag_anchor;
                self.poke_caret();
                host.request_redraw();
            }
            return;
        }
        if self.preview_drag {
            let view = self.viewport();
            let dy = if my < view.y + margin {
                -step
            } else if my > view.bottom() - margin {
                step
            } else {
                return;
            };
            self.scroll.scroll_by(dy);
            if let Some((_, doc_y)) = self.toggle_point(mx, my) {
                if let Some(row) = self.preview.row_at(&self.preview_rows, doc_y) {
                    if let Some((anchor, _)) = self.preview_sel {
                        self.preview_sel = Some((anchor, row));
                        host.request_redraw();
                    }
                }
            }
        }
    }

    fn on_text(&mut self, s: &str, host: &mut dyn Host) {
        if self.mode == Mode::Preview || s.chars().any(char::is_control) {
            return;
        }
        self.close_armed = false;
        let tab = &mut self.tabs[self.active];
        tab.editor.insert_text(&mut tab.buffer, s);
        self.after_edit();
        host.request_redraw();
    }

    /// Click in a preview area: toggles and links. Returns true when the
    /// click was consumed (caller starts drag-selection otherwise).
    fn preview_click(&mut self, x: f32, y: f32, host: &mut dyn Host) -> bool {
        let Some((dx, dy)) = self.toggle_point(x, y) else {
            return false;
        };
        match self.preview.action_at(dx, dy) {
            Some(crate::layout::display_list::HitAction::ToggleNode(id)) => {
                if !self.collapsed.insert(id) {
                    self.collapsed.remove(&id);
                }
                self.layout_dirty = true;
                host.request_redraw();
                true
            }
            // Open in the OS default browser; the status bar keeps the target
            // visible for links that fail or for keyboard users.
            Some(crate::layout::display_list::HitAction::Link(href)) => {
                let short: String = href.chars().take(72).collect();
                self.status_extra = if short.len() < href.len() {
                    format!("link: {short}…")
                } else {
                    format!("link: {short}")
                };
                host.open_url(&href);
                host.request_redraw();
                true
            }
            None => false,
        }
    }

    /// Drop the preview selection (mode/tab changes, Escape).
    fn clear_preview_sel(&mut self) {
        self.preview_sel = None;
        self.preview_drag = false;
    }

    /// Text of the preview row selection (anchor..caret, ordered), if any.
    fn preview_selection_text(&self) -> Option<String> {
        let (a, b) = self.preview_sel?;
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        if self.preview_rows.is_empty() {
            return None;
        }
        let hi = hi.min(self.preview_rows.len() - 1);
        Some(
            self.preview_rows[lo..=hi]
                .iter()
                .map(|r| r.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    /// Paint the preview row-selection bands over a preview pane.
    fn paint_preview_selection(&self, canvas: &mut Canvas, area: Rect, scroll: f32) {
        let Some((a, b)) = self.preview_sel else {
            return;
        };
        if self.preview_rows.is_empty() {
            return;
        }
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let hi = hi.min(self.preview_rows.len() - 1);
        canvas.set_clip(Some(area));
        for row in &self.preview_rows[lo..=hi] {
            let y = row.top + area.y - scroll;
            if y + (row.bottom - row.top) >= area.y && y <= area.bottom() {
                canvas.fill_rect(
                    Rect::new(
                        row.x0 - 2.0,
                        y,
                        (row.x1 - row.x0) + 4.0,
                        row.bottom - row.top,
                    ),
                    self.theme.sel,
                );
            }
        }
        canvas.set_clip(None);
    }

    fn zoom_by(&mut self, factor: f32) {
        self.zoom = (self.zoom * factor).clamp(0.5, 3.0);
        self.layout_dirty = true;
    }

    /// Save the buffer back to disk (`Ctrl+S`). Untitled buffers are rejected
    /// until file dialogs land in M6.
    fn save(&mut self, host: &mut dyn Host) {
        let Some(path) = self.path.clone() else {
            self.status_extra = "nothing to save (Untitled)".into();
            host.request_redraw();
            return;
        };
        match std::fs::write(&path, self.buffer.text()) {
            Ok(()) => {
                self.buffer.dirty = false;
                self.status_extra.clear();
                self.close_armed = false;
                self.mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            }
            Err(e) => self.status_extra = format!("save failed: {e}"),
        }
        host.request_redraw();
    }

    /// Reload the active tab's file from disk (`Ctrl+R`, or automatically
    /// when a clean buffer's file changed). Keeps the scroll place.
    fn reload_file(&mut self, host: &mut dyn Host) {
        let Some(path) = self.path.clone() else {
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.buffer = Buffer::from_text(&text);
                self.editor.caret = self.buffer.clamp(self.editor.caret);
                self.editor.anchor = None;
                self.clear_preview_sel();
                self.mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                self.status_extra = "reloaded".into();
                self.layout_dirty = true;
            }
            Err(e) => self.status_extra = format!("reload failed: {e}"),
        }
        host.request_redraw();
    }

    /// Every ~2 s of ticks: reload clean buffers whose file changed on disk;
    /// dirty ones get a status hint instead.
    fn check_file_changed(&mut self, host: &mut dyn Host) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        let Ok(mtime) = meta.modified() else {
            return;
        };
        if Some(mtime) == self.mtime {
            return;
        }
        if self.buffer.dirty {
            self.status_extra = "file changed on disk — Ctrl+R to reload".into();
            host.request_redraw();
        } else {
            self.reload_file(host);
        }
    }

    /// Scroll so the caret's row is visible in whichever pane holds it —
    /// vertically, and horizontally for the no-wrap editor lines.
    fn follow_caret(&mut self) {
        if self.mode == Mode::Preview {
            return;
        }
        self.poke_caret();
        let viewport_w = self.editor_area().w;
        let ctx = self.ctx();
        let (doc_x, doc_y) = self.editor.caret_xy(&self.buffer, viewport_w, &ctx);
        let line_h = self.editor.line_height(viewport_w, &ctx);
        let view_h = self.editor_area().h;
        self.scroll.offset = self
            .editor
            .ensure_visible(doc_y, line_h, view_h, self.scroll.offset);
        self.scroll.scroll_by(0.0);
        let margin = 32.0 * self.scale;
        self.h_scroll = if doc_x < self.h_scroll + margin {
            (doc_x - margin).max(0.0)
        } else if doc_x > self.h_scroll + viewport_w - margin {
            doc_x - viewport_w + margin
        } else {
            self.h_scroll
        }
        .clamp(0.0, self.h_max);
    }

    /// Paint selection + caret over an editor pane (document space -> pane).
    fn paint_editor_overlay(&self, canvas: &mut Canvas, area: Rect, scroll: f32) {
        // Line-number gutter first (rail + current-row tint), then the
        // selection/caret overlay above it. The gutter clips itself to
        // `area` and ignores horizontal scroll: numbers stay put while long
        // lines slide under the rail.
        let ctx = self.ctx();
        gutter::paint_with(
            canvas,
            gutter::Gutter {
                fonts: &self.fonts,
                theme: &self.theme,
                ctx: &ctx,
                caret_line: self.editor.caret.line,
                line_count: self.buffer.line_count(),
            },
            area,
            scroll,
        );
        let ops = self.editor.overlay_ops(
            &self.buffer,
            area.w + self.h_scroll + gutter::rail_w(&self.fonts, &ctx, self.buffer.line_count()),
            &ctx,
            self.caret_visible(),
        );
        if ops.is_empty() {
            return;
        }
        canvas.set_clip(Some(area));
        let (ox, oy) = (area.x - self.h_scroll, area.y - scroll);
        for op in &ops {
            if let DrawOp::Rect { rect, color } = op {
                canvas.fill_rect(Rect::new(rect.x + ox, rect.y + oy, rect.w, rect.h), *color);
            }
        }
        canvas.set_clip(None);
    }

    /// JSON preview: collapsible tree, or an error banner with position.
    fn layout_json(
        buf: &Buffer,
        collapsed: &HashSet<usize>,
        status_extra: &mut String,
        w: f32,
        ctx: &Ctx,
    ) -> DisplayList {
        match json::parser::parse(&buf.text()) {
            Ok(v) => {
                status_extra.clear();
                layout::json_tree::layout_value(&v, collapsed, w, ctx)
            }
            Err(crate::error::Error::Parse { line, col, msg }) => {
                *status_extra = format!("error {line}:{col}");
                let snippet = buf.line(
                    line.saturating_sub(1)
                        .min(buf.line_count().saturating_sub(1)),
                );
                layout::json_tree::layout_error(line, col, &msg, Some(snippet), w, ctx)
            }
            Err(other) => {
                *status_extra = "error".into();
                layout::json_tree::layout_error(1, 1, &other.to_string(), None, w, ctx)
            }
        }
    }

    /// JSONL preview: one headed tree per record, per-line error banners.
    fn layout_jsonl(
        buf: &Buffer,
        collapsed: &HashSet<usize>,
        status_extra: &mut String,
        w: f32,
        ctx: &Ctx,
    ) -> DisplayList {
        let records = jsonl::parse(&buf.text());
        let errors = records.iter().filter(|r| r.value.is_err()).count();
        *status_extra = if errors == 0 {
            format!(
                "{} record{}",
                records.len(),
                if records.len() == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "{} record{}, {} error{}",
                records.len(),
                if records.len() == 1 { "" } else { "s" },
                errors,
                if errors == 1 { "" } else { "s" }
            )
        };
        layout::json_tree::layout_records(&records, buf, collapsed, w, ctx)
    }

    /// Render one frame without a window (snapshots, golden tests).
    pub fn render_headless(&mut self, width: usize, height: usize, scale: f32) -> Canvas {
        let mut host = NullHost::default();
        self.on_event(
            Event::Resized {
                width,
                height,
                scale,
            },
            &mut host,
        );
        let mut canvas = Canvas::new(width, height);
        self.paint(&mut canvas);
        canvas
    }

    /// Test/CLI helper: scroll to an absolute offset (physical px).
    pub fn set_scroll(&mut self, offset: f32) {
        self.scroll.offset = offset;
        self.scroll.scroll_by(0.0);
    }

    fn paint_panes(&mut self, canvas: &mut Canvas) {
        if self.layout_dirty {
            self.relayout();
        }
        let view = self.viewport();
        match self.mode {
            Mode::Preview => {
                self.preview.paint(
                    canvas,
                    &self.fonts,
                    &self.theme,
                    view,
                    PaneScroll::vertical(self.scroll.offset),
                    self.scale,
                );
                self.paint_preview_selection(canvas, view, self.scroll.offset);
            }
            Mode::Edit => {
                self.editor_pane.paint(
                    canvas,
                    &self.fonts,
                    &self.theme,
                    view,
                    PaneScroll::editor(self.scroll.offset, self.h_scroll, self.h_max),
                    self.scale,
                );
                self.paint_editor_overlay(canvas, view, self.scroll.offset);
            }
            Mode::Split => {
                let (left, right) = self.split_panes();
                self.preview.paint(
                    canvas,
                    &self.fonts,
                    &self.theme,
                    left,
                    PaneScroll::vertical(self.scroll.offset),
                    self.scale,
                );
                self.paint_preview_selection(canvas, left, self.scroll.offset);
                self.editor_pane.paint(
                    canvas,
                    &self.fonts,
                    &self.theme,
                    right,
                    PaneScroll::editor(self.scroll.offset, self.h_scroll, self.h_max),
                    self.scale,
                );
                self.paint_editor_overlay(canvas, right, self.scroll.offset);
                // The seam between the panes.
                canvas.fill_rect(self.split_divider(), self.theme.rule);
            }
        }
    }

    /// Status-bar buttons: the mode switcher, theme toggle and Help. Ids:
    /// 0=Preview, 1=Edit, 2=Split, 3=Help, 4=Theme. Rects are filled in by
    /// `statusbar::paint`.
    fn status_buttons(&self) -> Vec<statusbar::StatusButton> {
        let active = match self.mode {
            Mode::Preview => 0,
            Mode::Edit => 1,
            Mode::Split => 2,
        };
        (0..=4)
            .map(|id| statusbar::StatusButton {
                id,
                label: match id {
                    0 => "Preview",
                    1 => "Edit",
                    2 => "Split",
                    3 => "Help",
                    // Names the theme a click switches to.
                    _ if self.dark => "Light",
                    _ => "Dark",
                },
                active: id == active || (id == 3 && self.help_open),
                rect: Rect::default(),
            })
            .collect()
    }

    fn status_bar_text(&self) -> (String, String) {
        let left = if self.mode == Mode::Edit {
            let name = self.title().replace(" — Readitor", "");
            let sel = if self.editor.selection().is_some() {
                " [sel]"
            } else {
                ""
            };
            format!(
                "{name}  Ln {}, Col {}{sel}",
                self.editor.caret.line + 1,
                char_col(&self.buffer, self.editor.caret),
            )
        } else {
            self.title().replace(" — Readitor", "")
        };
        let lines = self.buffer.line_count();
        let right = format!(
            "{}  ·  {} line{}  ·  {}%",
            self.kind.label(),
            lines,
            if lines == 1 { "" } else { "s" },
            (self.zoom * 100.0).round() as i32
        );
        let right = if self.status_extra.is_empty() {
            right
        } else {
            format!("{right}  ·  {}", self.status_extra)
        };
        (left, right)
    }
}

/// 1-based visual column of a caret position (chars, not bytes).
fn char_col(buf: &Buffer, p: Pos) -> usize {
    buf.line(p.line)[..p.col].chars().count() + 1
}

impl Handler for App {
    fn on_event(&mut self, ev: Event, host: &mut dyn Host) {
        match ev {
            Event::CloseRequested => host.quit(),
            Event::Resized {
                width,
                height,
                scale,
            } => {
                self.size = (width, height);
                self.scale = scale;
                self.layout_dirty = true;
                host.request_redraw();
            }
            Event::Scroll { dx, dy } => {
                // Horizontal wheel/trackpad pans the no-wrap editor lines;
                // preview panes wrap, so they have nothing to pan.
                if self.mode != Mode::Preview {
                    self.h_scroll = (self.h_scroll - dx).clamp(0.0, self.h_max);
                }
                self.scroll.scroll_by(-dy);
                host.request_redraw();
            }
            Event::KeyDown { key, mods } => self.on_key(key, mods, host),
            Event::MouseDown { x, y, button } => self.on_mouse_down(x, y, button, host),
            Event::MouseMove { x, y } => self.on_mouse_move(x, y, host),
            Event::MouseUp { x, y, button } => self.on_mouse_up(x, y, button, host),
            Event::Text(s) => self.on_text(&s, host),
            Event::FileOpen(path) => self.open_path(path, host),
            Event::Tick => self.on_tick(host),
            Event::ThemeChanged { dark } => {
                if !self.theme_manual {
                    self.set_dark(dark);
                    host.request_redraw();
                }
            }
        }
    }

    fn paint(&mut self, canvas: &mut Canvas) {
        let view = self.viewport();
        self.paint_panes(canvas);
        let sb = Rect::new(
            0.0,
            view.bottom(),
            view.w,
            canvas.height as f32 - view.bottom(),
        );
        let (left, right) = self.status_bar_text();
        let mut buttons = self.status_buttons();
        statusbar::paint(
            canvas,
            &self.fonts,
            &self.theme,
            sb,
            self.scale,
            (&left, &right),
            &mut buttons,
        );
        self.status_buttons = buttons;
        let labels = self.tab_labels();
        let lay = titlebar::layout(&self.fonts, &labels, self.size.0 as f32, self.scale);
        titlebar::paint(
            canvas,
            &self.fonts,
            &self.theme,
            &lay,
            &labels,
            self.active,
            self.hover_title,
        );
        if let Some(m) = &self.menu {
            m.paint(canvas, &self.fonts, &self.theme, self.scale);
        }
        if self.help_open {
            help::paint(
                canvas,
                &self.fonts,
                &self.theme,
                canvas.width as f32,
                canvas.height as f32,
                self.scale,
            );
        }
    }

    fn title(&self) -> String {
        App::title(self)
    }

    fn title_hit(&self, x: f32, y: f32) -> TitleHit {
        if y >= titlebar::HEIGHT * self.scale {
            return TitleHit::Client;
        }
        let labels = self.tab_labels();
        let lay = titlebar::layout(&self.fonts, &labels, self.size.0 as f32, self.scale);
        titlebar::hit_test(&lay, x, y)
    }

    fn cursor_at(&self, x: f32, y: f32) -> CursorShape {
        if self.menu.is_some() || self.help_open {
            return CursorShape::Arrow;
        }
        if y < titlebar::HEIGHT * self.scale {
            let labels = self.tab_labels();
            let lay = titlebar::layout(&self.fonts, &labels, self.size.0 as f32, self.scale);
            return match titlebar::hit_test(&lay, x, y) {
                TitleHit::Drag | TitleHit::Client => CursorShape::Arrow,
                _ => CursorShape::Hand,
            };
        }
        if y >= self.viewport().bottom() {
            let p = Point { x, y };
            return if self.status_buttons.iter().any(|b| b.rect.contains(p)) {
                CursorShape::Hand
            } else {
                CursorShape::Arrow
            };
        }
        // Overlay scrollbars live on top of the panes: they get the Arrow
        // (thumb dragging lands later), never the I-beam of the text below.
        let p = Point { x, y };
        match self.mode {
            Mode::Preview => {
                if self.preview.scrollbar_hit(
                    self.viewport(),
                    PaneScroll::vertical(self.scroll.offset),
                    self.scale,
                    p,
                ) {
                    return CursorShape::Arrow;
                }
            }
            Mode::Edit => {
                if self.editor_pane.scrollbar_hit(
                    self.viewport(),
                    PaneScroll::editor(self.scroll.offset, self.h_scroll, self.h_max),
                    self.scale,
                    p,
                ) {
                    return CursorShape::Arrow;
                }
            }
            Mode::Split => {
                let (left, right) = self.split_panes();
                if self.preview.scrollbar_hit(
                    left,
                    PaneScroll::vertical(self.scroll.offset),
                    self.scale,
                    p,
                ) || self.editor_pane.scrollbar_hit(
                    right,
                    PaneScroll::editor(self.scroll.offset, self.h_scroll, self.h_max),
                    self.scale,
                    p,
                ) {
                    return CursorShape::Arrow;
                }
            }
        }
        if self.editor_point(x, y).is_some() {
            return CursorShape::IBeam;
        }
        if let Some((dx, dy)) = self.toggle_point(x, y) {
            if self.preview.action_at(dx, dy).is_some() {
                return CursorShape::Hand;
            }
            // Selectable preview text gets the I-beam.
            if self.preview.row_at(&self.preview_rows, dy).is_some() {
                return CursorShape::IBeam;
            }
        }
        CursorShape::Arrow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(name: &str, content: &str) -> App {
        let dir = std::env::temp_dir().join(name);
        std::fs::write(&dir, content).unwrap();
        App::new(Some(dir)).unwrap()
    }

    fn ctrl(key: char) -> Event {
        Event::KeyDown {
            key: Key::Char(key),
            mods: Modifiers {
                ctrl: true,
                alt: false,
                shift: false,
            },
        }
    }

    #[test]
    fn headless_render_draws_text_and_statusbar() {
        let mut app = open("readitor_test_a.txt", "Hello Readitor\nsecond line");
        let c = app.render_headless(640, 400, 1.0);
        let bg = app.theme.bg.pack() | 0xFF00_0000;
        let dark_px = c.pixels[..640 * 372].iter().filter(|&&p| p != bg).count();
        assert!(dark_px > 100, "preview area is blank");
        // status bar region differs from page background
        assert_ne!(c.pixels[390 * 640 + 300], bg);
    }

    #[test]
    fn zoom_changes_layout_height() {
        let mut app = App::new(None).unwrap();
        app.buffer = Buffer::from_text(&"line\n".repeat(50));
        let _ = app.render_headless(800, 600, 1.0);
        let h1 = app.preview.list.height;
        let mut host = NullHost::default();
        app.on_event(ctrl('+'), &mut host);
        let mut c = Canvas::new(800, 600);
        app.paint(&mut c);
        assert!(app.preview.list.height > h1);
    }

    #[test]
    fn json_renders_tinted_tree() {
        use crate::layout::display_list::DrawOp;
        let mut app = open(
            "readitor_test_b.json",
            "{\"name\": \"readitor\", \"n\": 42}",
        );
        assert_eq!(app.kind, FileKind::Json);
        let c = app.render_headless(800, 600, 1.0);
        assert!(app.status_extra.is_empty(), "valid JSON flagged");
        let bg = app.theme.bg.pack();
        let ink = c.pixels.iter().filter(|&&p| p != bg).count();
        assert!(ink > 200, "tree area is blank");
        // Key tint is in the laid-out ops (pixels are AA-blended, not exact).
        let tints: Vec<u32> = app
            .preview
            .list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { color, .. } => Some(color.pack()),
                _ => None,
            })
            .collect();
        assert!(tints.contains(&app.theme.json_key.pack()));
        assert!(tints.contains(&app.theme.json_number.pack()));
        assert!(tints.contains(&app.theme.json_string.pack()));
    }

    #[test]
    fn invalid_json_shows_error_banner() {
        use crate::layout::display_list::DrawOp;
        let mut app = open("readitor_test_c.json", "{\"a\": }");
        let c = app.render_headless(800, 600, 1.0);
        assert!(
            app.status_extra.contains("error"),
            "status: {}",
            app.status_extra
        );
        assert!(
            app.preview
                .list
                .ops
                .iter()
                .any(|op| matches!(op, DrawOp::RoundedRect { .. })),
            "error banner missing"
        );
        let bg = app.theme.bg.pack();
        assert!(c.pixels.iter().any(|&p| p != bg), "banner did not paint");
    }

    #[test]
    fn clicking_toggle_collapses_tree() {
        let mut app = open(
            "readitor_test_d.json",
            "{\"a\": {\"b\": [1, 2, 3]}, \"c\": true}",
        );
        let _ = app.render_headless(800, 600, 1.0);
        let open_h = app.preview.list.height;
        assert!(!app.preview.list.hits.is_empty());
        let r = app.preview.list.hits[0].rect;
        let mut host = NullHost::default();
        app.on_event(
            Event::MouseDown {
                x: r.x + 1.0,
                y: r.y + 1.0 + app.viewport().y,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert_eq!(app.collapsed.len(), 1);
        let mut c = Canvas::new(800, 600);
        app.paint(&mut c);
        assert!(app.preview.list.height < open_h, "collapse did not shrink");
    }

    #[test]
    fn mode_key_cycles_preview_edit_split() {
        let mut app = App::new(None).unwrap();
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        assert_eq!(app.mode, Mode::Edit);
        app.on_event(ctrl('e'), &mut host);
        assert_eq!(app.mode, Mode::Split);
        app.on_event(ctrl('e'), &mut host);
        assert_eq!(app.mode, Mode::Preview);
        // Edit/Split panes lay out without panicking.
        app.on_event(ctrl('e'), &mut host);
        let _ = app.render_headless(800, 600, 1.0);
    }

    #[test]
    fn markdown_renders_document() {
        let mut app = open(
            "readitor_test_e.md",
            "# Title\n\nHello **world**.\n\n- a\n- b\n",
        );
        assert_eq!(app.kind, FileKind::Markdown);
        let c = app.render_headless(800, 600, 1.0);
        assert!(app.status_extra.is_empty());
        let bg = app.theme.bg.pack();
        assert!(
            c.pixels.iter().filter(|&&p| p != bg).count() > 200,
            "document area is blank"
        );
    }

    #[test]
    fn clicking_link_shows_href_in_status() {
        use crate::layout::display_list::HitAction;
        let mut app = open(
            "readitor_test_f.md",
            "See [example](https://example.com/x).\n",
        );
        let _ = app.render_headless(800, 600, 1.0);
        let link = app
            .preview
            .list
            .hits
            .iter()
            .find_map(|h| match &h.action {
                HitAction::Link(href) => Some((h.rect, href.clone())),
                _ => None,
            })
            .expect("no link hit region");
        let mut host = NullHost::default();
        app.on_event(
            Event::MouseDown {
                x: link.0.x + 1.0,
                y: link.0.y + 1.0 + app.viewport().y,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(
            app.status_extra.contains("https://example.com/x"),
            "status: {}",
            app.status_extra
        );
    }

    #[test]
    fn typing_edits_buffer_in_edit_mode() {
        let mut app = open("readitor_test_g.txt", "hello");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        assert_eq!(app.mode, Mode::Edit);
        app.on_event(Event::Text("X".into()), &mut host);
        assert_eq!(app.buffer.text(), "Xhello");
        assert!(app.buffer.dirty);
        assert_eq!(app.editor.caret, Pos { line: 0, col: 1 });
        // Undo removes the typing, redo re-applies it.
        app.on_event(ctrl('z'), &mut host);
        assert_eq!(app.buffer.text(), "hello");
        app.on_event(ctrl('y'), &mut host);
        assert_eq!(app.buffer.text(), "Xhello");
    }

    #[test]
    fn typing_in_preview_mode_is_ignored() {
        let mut app = open("readitor_test_h.txt", "hello");
        let mut host = NullHost::default();
        app.on_event(Event::Text("X".into()), &mut host);
        assert_eq!(app.buffer.text(), "hello");
        assert!(!app.buffer.dirty);
    }

    #[test]
    fn enter_and_backspace_split_join_lines() {
        let mut app = open("readitor_test_i.txt", "one two");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        app.editor.caret = Pos { line: 0, col: 3 };
        app.on_event(
            Event::KeyDown {
                key: Key::Enter,
                mods: Modifiers::default(),
            },
            &mut host,
        );
        assert_eq!(app.buffer.text(), "one\n two");
        app.on_event(
            Event::KeyDown {
                key: Key::Backspace,
                mods: Modifiers::default(),
            },
            &mut host,
        );
        // Backspace at {1, 0} removes the newline: "one" + " two".
        assert_eq!(app.buffer.text(), "one two");
        app.on_event(ctrl('z'), &mut host);
        assert_eq!(app.buffer.text(), "one\n two");
    }

    #[test]
    fn clipboard_copy_cut_paste_flow() {
        let mut app = open("readitor_test_j.txt", "hello world");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        let tab = &mut *app;
        tab.editor.select_all(&tab.buffer);
        app.on_event(ctrl('c'), &mut host);
        assert_eq!(host.clipboard.as_deref(), Some("hello world"));
        app.on_event(ctrl('x'), &mut host);
        assert_eq!(app.buffer.text(), "");
        assert!(app.buffer.dirty);
        app.on_event(ctrl('v'), &mut host);
        assert_eq!(app.buffer.text(), "hello world");
    }

    #[test]
    fn save_marks_buffer_clean() {
        let mut app = open("readitor_test_k.txt", "hello");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        app.on_event(Event::Text("!".into()), &mut host);
        assert_eq!(app.title(), "readitor_test_k.txt • — Readitor");
        assert_eq!(app.buffer.text(), "!hello");
        app.on_event(ctrl('s'), &mut host);
        assert!(!app.buffer.dirty);
        assert_eq!(app.title(), "readitor_test_k.txt — Readitor");
        assert_eq!(
            std::fs::read_to_string(app.path.as_ref().unwrap()).unwrap(),
            "!hello"
        );
    }

    #[test]
    fn arrow_keys_move_caret_in_edit_mode() {
        let mut app = open("readitor_test_l.txt", "first\nsecond");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        app.on_event(
            Event::KeyDown {
                key: Key::Down,
                mods: Modifiers::default(),
            },
            &mut host,
        );
        assert_eq!(app.editor.caret, Pos { line: 1, col: 0 });
        app.on_event(
            Event::KeyDown {
                key: Key::Right,
                mods: Modifiers::default(),
            },
            &mut host,
        );
        assert_eq!(app.editor.caret.col, 1);
    }

    #[test]
    fn shift_select_and_delete_selection() {
        let mut app = open("readitor_test_m.txt", "abcdef");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        app.on_event(
            Event::KeyDown {
                key: Key::Right,
                mods: Modifiers {
                    ctrl: false,
                    alt: false,
                    shift: true,
                },
            },
            &mut host,
        );
        app.on_event(
            Event::KeyDown {
                key: Key::Right,
                mods: Modifiers {
                    ctrl: false,
                    alt: false,
                    shift: true,
                },
            },
            &mut host,
        );
        assert_eq!(
            app.editor.selection(),
            Some((Pos { line: 0, col: 0 }, Pos { line: 0, col: 2 }))
        );
        app.on_event(
            Event::KeyDown {
                key: Key::Delete,
                mods: Modifiers::default(),
            },
            &mut host,
        );
        assert_eq!(app.buffer.text(), "cdef");
    }

    #[test]
    fn split_renders_both_panes_and_keeps_place() {
        let mut app = open("readitor_test_n.txt", &"word line\n".repeat(40));
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host); // Edit
        app.on_event(ctrl('e'), &mut host); // Split
        let mut c = app.render_headless(800, 600, 1.0);
        let bg = app.theme.bg.pack();
        // Both panes painted something.
        assert!(
            c.pixels.iter().filter(|&&p| p != bg).count() > 400,
            "split panes blank"
        );
        // Editing in split re-renders the document pane (height changes).
        let h_before = app.preview.list.height;
        app.on_event(Event::Text("X".into()), &mut host);
        app.paint(&mut c);
        assert!(app.preview.list.height >= h_before);
        assert!(app.buffer.text().starts_with("Xword"));
    }

    #[test]
    fn nested_toggles_collapse_and_reexpand() {
        let mut app = open(
            "readitor_test_p.json",
            "{\"a\":{\"x\":1},\"list\":[{\"in\":true}],\"s\":\"end\"}",
        );
        let _ = app.render_headless(800, 600, 1.0);
        let hits: Vec<(Rect, usize)> = app
            .preview
            .list
            .hits
            .iter()
            .filter_map(|h| match &h.action {
                crate::layout::display_list::HitAction::ToggleNode(id) => Some((h.rect, *id)),
                crate::layout::display_list::HitAction::Link(_) => None,
            })
            .collect();
        assert!(hits.len() >= 3, "want root + 2 nested toggles");
        let mut host = NullHost::default();
        for (i, &(r, id)) in hits.iter().enumerate().skip(1) {
            let open_h = app.preview.list.height;
            // Click on the [+]/[-] glyph itself (end of the hit rect): the
            // marker sits after the key text on nested rows.
            let (mx, my) = (r.right() - 2.0, r.y + 2.0 + app.viewport().y);
            app.on_event(
                Event::MouseDown {
                    x: mx,
                    y: my,
                    button: MouseButton::Left,
                },
                &mut host,
            );
            assert!(
                app.collapsed.contains(&id),
                "click on toggle {i} (id {id}) did not collapse"
            );
            app.paint(&mut Canvas::new(800, 600));
            assert!(
                app.preview.list.height < open_h,
                "collapsing toggle {i} (id {id}) did not shrink the tree"
            );
            // Click again: re-expand.
            app.on_event(
                Event::MouseDown {
                    x: mx,
                    y: my,
                    button: MouseButton::Left,
                },
                &mut host,
            );
            assert!(!app.collapsed.contains(&id), "re-expand of {id} failed");
            app.paint(&mut Canvas::new(800, 600));
        }
    }

    #[test]
    fn tabs_open_dedupe_and_cycle() {
        let mut app = open("readitor_test_q.txt", "one");
        let second = std::env::temp_dir().join("readitor_test_r.md");
        std::fs::write(&second, "# two\n").unwrap();
        let mut host = NullHost::default();
        assert_eq!(app.tabs.len(), 1);
        app.on_event(Event::FileOpen(second.clone()), &mut host);
        assert_eq!(app.tabs.len(), 2, "drop should open a new tab");
        assert_eq!(app.active, 1);
        assert_eq!(app.kind, FileKind::Markdown);
        // Re-opening the same path focuses the existing tab instead of adding.
        app.on_event(ctrl('e'), &mut host); // leave tab 1 in Edit, then switch
        app.on_event(Event::FileOpen(second.clone()), &mut host);
        assert_eq!(app.tabs.len(), 2, "re-open must not duplicate");
        assert_eq!(app.active, 1);
        assert_eq!(app.mode, Mode::Edit, "per-tab state kept");
        // Ctrl+Tab cycles; Shift goes back.
        app.on_event(
            Event::KeyDown {
                key: Key::Tab,
                mods: Modifiers {
                    ctrl: true,
                    alt: false,
                    shift: false,
                },
            },
            &mut host,
        );
        assert_eq!(app.active, 0);
        app.on_event(
            Event::KeyDown {
                key: Key::Tab,
                mods: Modifiers {
                    ctrl: true,
                    alt: false,
                    shift: true,
                },
            },
            &mut host,
        );
        assert_eq!(app.active, 1);
    }

    #[test]
    fn close_tab_warns_when_dirty_then_discards() {
        let mut app = open("readitor_test_s.txt", "keep me");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        app.on_event(Event::Text("X".into()), &mut host);
        assert!(app.buffer.dirty);
        app.on_event(ctrl('w'), &mut host);
        assert_eq!(app.tabs.len(), 1, "dirty tab must warn, not close");
        assert!(app.status_extra.contains("unsaved"));
        app.on_event(ctrl('w'), &mut host);
        assert_eq!(app.tabs.len(), 1, "last tab becomes a fresh Untitled");
        assert!(!app.buffer.dirty);
        assert_eq!(app.buffer.text(), "");
        assert_eq!(app.name(), "Untitled");
    }

    #[test]
    fn ctrl_o_opens_host_picked_file() {
        let mut app = App::new(None).unwrap();
        let pick = std::env::temp_dir().join("readitor_test_t.json");
        std::fs::write(&pick, "{\"a\": 1}").unwrap();
        let mut host = NullHost {
            picked_file: Some(pick.clone()),
            ..NullHost::default()
        };
        app.on_event(ctrl('o'), &mut host);
        assert_eq!(app.tabs.len(), 1, "untitled app had no file tab yet");
        assert_eq!(app.active, 0);
        assert_eq!(app.path.as_deref(), Some(pick.as_path()));
        assert_eq!(app.kind, FileKind::Json);
        // A second Ctrl+O with no pick (dialog cancelled) changes nothing.
        app.on_event(ctrl('o'), &mut host);
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn help_overlay_toggles_and_click_closes() {
        let mut app = open("readitor_test_u.txt", "hello");
        let mut host = NullHost::default();
        app.on_event(
            Event::KeyDown {
                key: Key::F(1),
                mods: Modifiers::default(),
            },
            &mut host,
        );
        assert!(app.help_open);
        // Any click dismisses it (and does not place a caret).
        app.on_event(
            Event::MouseDown {
                x: 100.0,
                y: 100.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(!app.help_open);
        // Escape closes it too.
        app.on_event(
            Event::KeyDown {
                key: Key::F(1),
                mods: Modifiers::default(),
            },
            &mut host,
        );
        app.on_event(
            Event::KeyDown {
                key: Key::Escape,
                mods: Modifiers::default(),
            },
            &mut host,
        );
        assert!(!app.help_open);
    }

    #[test]
    fn right_click_menu_copy_all_and_dismiss() {
        let mut app = open("readitor_test_v.txt", "whole document");
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        app.on_event(
            Event::MouseDown {
                x: 300.0,
                y: 300.0,
                button: MouseButton::Right,
            },
            &mut host,
        );
        let menu = app.menu.as_ref().expect("right-click opens a menu");
        let copy_all = menu
            .rects
            .iter()
            .enumerate()
            .find(|(i, _)| menu.items[*i].0 == menu::Action::CopyAll)
            .map(|(i, r)| (i, *r))
            .expect("preview menu offers Copy all");
        // Clicking the item runs it and closes the menu.
        app.on_event(
            Event::MouseDown {
                x: copy_all.1.x + 4.0,
                y: copy_all.1.y + 4.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.menu.is_none());
        assert_eq!(host.clipboard.as_deref(), Some("whole document"));
        // Re-open and click outside: dismissed without action.
        app.on_event(
            Event::MouseDown {
                x: 300.0,
                y: 300.0,
                button: MouseButton::Right,
            },
            &mut host,
        );
        assert!(app.menu.is_some());
        app.on_event(
            Event::MouseDown {
                x: 5.0,
                y: 5.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.menu.is_none());
        let _ = copy_all.0;
    }

    #[test]
    fn title_bar_clicks_switch_and_close_tabs() {
        let mut app = open("readitor_test_w.txt", "one");
        let second = std::env::temp_dir().join("readitor_test_x.txt");
        std::fs::write(&second, "two").unwrap();
        let mut host = NullHost::default();
        app.on_event(Event::FileOpen(second), &mut host);
        let _ = app.render_headless(800, 600, 1.0);
        let lay = titlebar::layout(&app.fonts, &app.tab_labels(), 800.0, 1.0);
        // Click tab 0: switch.
        app.on_event(
            Event::MouseDown {
                x: lay.tabs[0].x + 4.0,
                y: lay.tabs[0].y + 4.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert_eq!(app.active, 0);
        assert_eq!(app.buffer.text(), "one");
        // Click its ×: clean tab closes immediately.
        app.on_event(
            Event::MouseDown {
                x: lay.closes[0].x + 2.0,
                y: lay.closes[0].y + 2.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.buffer.text(), "two");
    }

    #[test]
    fn cursor_shapes_over_panes_and_chrome() {
        let mut app = open("readitor_test_y.json", "{\"a\": {\"b\": 1}, \"c\": [1, 2]}");
        let _ = app.render_headless(800, 600, 1.0);
        // Hand over a tree toggle in Preview.
        let toggle = app.preview.list.hits[0].rect;
        assert_eq!(
            app.cursor_at(toggle.x + 2.0, toggle.y + 2.0 + app.viewport().y),
            CursorShape::Hand
        );
        // Arrow over empty preview space.
        assert_eq!(app.cursor_at(400.0, 500.0), CursorShape::Arrow);
        // I-beam over the editor pane in Edit mode.
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        assert_eq!(app.cursor_at(400.0, 300.0), CursorShape::IBeam);
        // Hand over the window close button in the title bar.
        let lay = titlebar::layout(&app.fonts, &app.tab_labels(), 800.0, 1.0);
        assert_eq!(app.cursor_at(lay.close.x + 10.0, 10.0), CursorShape::Hand);
    }

    #[test]
    fn caret_bar_paints_in_edit_mode() {
        let mut app = open("readitor_test_caret.txt", "hello world");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        let c = app.render_headless(800, 600, 1.0);
        let ctx = app.ctx();
        let (cx, cy) = app.editor.caret_xy(&app.buffer, 800.0, &ctx);
        let line_h = app.editor.line_height(800.0, &ctx);
        let view = app.viewport();
        // The caret bar is a thin fg-colored rect at the caret position.
        let px = |x: f32, y: f32| c.pixels[y as usize * 800 + x as usize];
        let fg = app.theme.fg.pack();
        let mid_y = (view.y + cy + line_h / 2.0) as usize;
        let x0 = (view.x + cx).round() as usize;
        let found = (0..3).any(|dx| px((x0 + dx) as f32, mid_y as f32) == fg);
        assert!(
            found,
            "no caret pixels at ({x0},{mid_y}); colors: {:?}",
            (0..3)
                .map(|dx| px((x0 + dx) as f32, mid_y as f32))
                .collect::<Vec<u32>>()
        );
    }

    #[test]
    fn click_in_edit_pane_places_caret() {
        let mut app = open("readitor_test_o.txt", &"second line of text\n".repeat(20));
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        let _ = app.render_headless(800, 600, 1.0);
        // Click near the start of the visible area (first row).
        app.on_event(
            Event::MouseDown {
                x: 50.0,
                y: 40.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        // Caret moved to a real buffer position near the top of the document.
        assert!(app.editor.caret.line < 5, "caret: {:?}", app.editor.caret);
        app.on_event(
            Event::MouseUp {
                x: 50.0,
                y: 40.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.drag_anchor.is_none());
    }

    #[test]
    fn preview_drag_selects_rows_and_copies() {
        let mut app = open("readitor_test_sel.txt", "alpha\nbeta\ngamma\ndelta\n");
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        assert!(app.preview_rows.len() >= 4, "want a row per line");
        let mid = |app: &App, i: usize| {
            app.viewport().y + (app.preview_rows[i].top + app.preview_rows[i].bottom) / 2.0
        };
        let (y0, y2) = (mid(&app, 0), mid(&app, 2));
        // Drag from row 0 down to row 2.
        app.on_event(
            Event::MouseDown {
                x: 100.0,
                y: y0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        app.on_event(Event::MouseMove { x: 100.0, y: y2 }, &mut host);
        app.on_event(
            Event::MouseUp {
                x: 100.0,
                y: y2,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert_eq!(app.preview_sel, Some((0, 2)));
        // Ctrl+C copies the selected rows; the selection survives the copy.
        app.on_event(ctrl('c'), &mut host);
        assert_eq!(host.clipboard.as_deref(), Some("alpha\nbeta\ngamma"));
        // Switching modes clears it.
        app.on_event(ctrl('e'), &mut host);
        assert_eq!(app.preview_sel, None);
    }

    #[test]
    fn status_bar_buttons_switch_modes_and_help() {
        let mut app = open("readitor_test_sb.txt", "hello");
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        let edit = app
            .status_buttons
            .iter()
            .find(|b| b.label == "Edit")
            .expect("Edit button")
            .rect;
        let help = app
            .status_buttons
            .iter()
            .find(|b| b.label == "Help")
            .expect("Help button")
            .rect;
        app.on_event(
            Event::MouseDown {
                x: edit.x + 2.0,
                y: edit.y + 2.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert_eq!(app.mode, Mode::Edit);
        app.on_event(
            Event::MouseDown {
                x: help.x + 2.0,
                y: help.y + 2.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.help_open);
    }

    #[test]
    fn clean_file_reloads_when_changed_on_disk() {
        let path = std::env::temp_dir().join("readitor_test_rc.txt");
        std::fs::write(&path, "original").unwrap();
        let mut app = App::new(Some(path.clone())).unwrap();
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        std::fs::write(&path, "changed externally").unwrap();
        // ~2 s of ticks: the periodic check reloads the clean buffer.
        for _ in 0..50 {
            app.on_event(Event::Tick, &mut host);
        }
        assert_eq!(app.buffer.text(), "changed externally");
        assert!(app.status_extra.contains("reloaded"));
        // Ctrl+R works directly too.
        std::fs::write(&path, "third version").unwrap();
        app.on_event(ctrl('r'), &mut host);
        assert_eq!(app.buffer.text(), "third version");
    }

    #[test]
    fn dirty_file_gets_reload_hint_not_silent_reload() {
        let path = std::env::temp_dir().join("readitor_test_rh.txt");
        std::fs::write(&path, "original").unwrap();
        let mut app = App::new(Some(path.clone())).unwrap();
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        app.on_event(Event::Text("X".into()), &mut host);
        assert!(app.buffer.dirty);
        std::fs::write(&path, "changed externally").unwrap();
        for _ in 0..50 {
            app.on_event(Event::Tick, &mut host);
        }
        // Unsaved edits win: a hint appears, the buffer is untouched.
        assert_eq!(app.buffer.text(), "Xoriginal");
        assert!(app.status_extra.contains("Ctrl+R"));
        // Explicit Ctrl+R discards the edits and reloads.
        app.on_event(ctrl('r'), &mut host);
        assert_eq!(app.buffer.text(), "changed externally");
        assert!(!app.buffer.dirty);
    }

    #[test]
    fn theme_follows_os_until_manual_override() {
        let mut app = open("readitor_test_theme.txt", "hello");
        let mut host = NullHost::default();
        assert!(!app.dark);
        app.on_event(Event::ThemeChanged { dark: true }, &mut host);
        assert!(app.dark, "OS dark preference should be followed");
        // After Ctrl+T the app stops following the OS.
        app.on_event(ctrl('t'), &mut host);
        assert!(!app.dark);
        app.on_event(Event::ThemeChanged { dark: true }, &mut host);
        assert!(!app.dark, "manual override must stick");
    }

    #[test]
    fn tick_auto_scrolls_editor_drag() {
        let mut app = open("readitor_test_tick.txt", &"line\n".repeat(200));
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        let _ = app.render_headless(800, 600, 1.0);
        // Start a selection drag and hold the mouse near the bottom edge.
        app.on_event(
            Event::MouseDown {
                x: 100.0,
                y: 100.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        app.on_event(Event::MouseMove { x: 100.0, y: 560.0 }, &mut host);
        let before = app.scroll.offset;
        app.on_event(Event::Tick, &mut host);
        assert!(app.scroll.offset > before, "tick should auto-scroll down");
        assert!(app.editor.selection().is_some(), "selection extends");
    }

    #[test]
    fn caret_blinks_on_tick_and_resets_on_activity() {
        let mut app = open("readitor_test_blink.txt", "hello");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        assert!(app.caret_visible(), "caret starts solid");
        for _ in 0..12 {
            app.on_event(Event::Tick, &mut host);
        }
        assert!(
            !app.caret_visible(),
            "caret should hide after a half-period"
        );
        for _ in 0..12 {
            app.on_event(Event::Tick, &mut host);
        }
        assert!(app.caret_visible(), "and come back on the next one");
        for _ in 0..12 {
            app.on_event(Event::Tick, &mut host);
        }
        assert!(!app.caret_visible());
        // Typing restarts the blink, solid.
        app.on_event(Event::Text("X".into()), &mut host);
        assert!(app.caret_visible());
        // So does moving the caret.
        for _ in 0..12 {
            app.on_event(Event::Tick, &mut host);
        }
        assert!(!app.caret_visible());
        app.on_event(
            Event::KeyDown {
                key: Key::Right,
                mods: Modifiers::default(),
            },
            &mut host,
        );
        assert!(app.caret_visible());
    }

    #[test]
    fn theme_button_toggles_dark() {
        let mut app = open("readitor_test_themebtn.txt", "hello");
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        let btn = app
            .status_buttons
            .iter()
            .find(|b| b.id == 4)
            .expect("theme button");
        assert_eq!(btn.label, "Dark", "light theme offers to go dark");
        app.on_event(
            Event::MouseDown {
                x: btn.rect.x + 2.0,
                y: btn.rect.y + 2.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.dark, "click should switch to dark");
        assert!(app.theme_manual, "button is a manual override");
        // The label now offers the way back.
        let _ = app.render_headless(800, 600, 1.0);
        let btn = app
            .status_buttons
            .iter()
            .find(|b| b.id == 4)
            .expect("theme button");
        assert_eq!(btn.label, "Light");
        app.on_event(
            Event::MouseDown {
                x: btn.rect.x + 2.0,
                y: btn.rect.y + 2.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(!app.dark);
    }

    #[test]
    fn split_panes_are_half_half_with_divider() {
        let mut app = open("readitor_test_splitdiv.txt", "hello");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        app.on_event(ctrl('e'), &mut host); // Split
        let _ = app.render_headless(1000, 600, 1.0);
        let (left, right) = app.split_panes();
        assert!(
            (left.w - right.w).abs() <= 2.0,
            "panes should be ~equal: {left:?} {right:?}"
        );
        let div = app.split_divider();
        assert!(div.w >= 1.0, "divider must be visible");
        assert_eq!(div.right(), right.x, "divider sits at the seam");
        assert!(
            div.x >= left.right() - 0.5,
            "divider starts at the left pane edge"
        );
    }

    #[test]
    fn split_preview_wraps_at_pane_width() {
        // A long single line: in Split the preview column must fit the left
        // pane (it used to lay out at the full window width and clip).
        let mut app = open(
            "readitor_test_splitwrap.txt",
            "word ".repeat(120).trim_end(),
        );
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        app.on_event(ctrl('e'), &mut host); // Split
        let _ = app.render_headless(1000, 600, 1.0);
        let left = app.split_panes().0;
        let ctx = app.ctx();
        for op in &app.preview.list.ops {
            if let crate::layout::display_list::DrawOp::Text {
                x,
                text,
                size,
                style,
                ..
            } = op
            {
                let w = ctx.fonts.measure(*style, text, *size);
                assert!(
                    x + w <= left.right() + 0.5,
                    "preview text overflows the left pane: x={x} w={w}"
                );
            }
        }
        // And the column is centered in the pane, not the window: laid out at
        // the old full-window width the column would start at x≈90 (window
        // centering of the 820px cap); at pane width it hugs the pane padding.
        let xs = app
            .preview
            .list
            .ops
            .iter()
            .filter_map(|op| match op {
                crate::layout::display_list::DrawOp::Text { x, .. } => Some(*x),
                _ => None,
            })
            .collect::<Vec<_>>();
        let min = xs.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            min - left.x < 60.0,
            "column should be centered in the pane, not the window (min x {min} vs pane {left:?})"
        );
    }

    #[test]
    fn cursor_over_scrollbars_is_arrow_not_ibeam() {
        let mut app = open(
            "readitor_test_sbcur.txt",
            &"long line without scrolling\n".repeat(60),
        );
        app.on_event(ctrl('e'), &mut NullHost::default()); // Edit (=> editor pane)
        let _ = app.render_headless(800, 600, 1.0);
        // Right edge over the vertical overlay scrollbar zone.
        let view = app.viewport();
        let sb_x = view.right() - 4.0;
        let y_mid = view.y + view.h / 2.0;
        assert_eq!(app.cursor_at(sb_x, y_mid), CursorShape::Arrow);
        // Just inside the text area, same row: still I-beam.
        assert_eq!(
            app.cursor_at(view.right() - 30.0, y_mid),
            CursorShape::IBeam
        );
        // Horizontal bar zone (long line in Edit => h_max > 0). One file:
        // re-use with a wide line via a fresh tab.
        let wide = std::env::temp_dir().join("readitor_test_sbcursor_h.txt");
        std::fs::write(&wide, format!("x{}", "y".repeat(300))).unwrap();
        app.open_path(wide, &mut NullHost::default());
        let _ = app.render_headless(800, 600, 1.0);
        assert!(app.h_max > 0.0, "setup: need a wide line");
        let h_y = app.viewport().bottom() - 4.0;
        assert_eq!(app.cursor_at(view.x + 200.0, h_y), CursorShape::Arrow);
    }

    #[test]
    fn editor_does_not_wrap_and_scrolls_horizontally() {
        let long = format!("{} end", "word ".repeat(200));
        let mut app = open("readitor_test_nw.txt", &long);
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host); // Edit
        let _ = app.render_headless(600, 400, 1.0);
        // One row per line, no folding.
        assert_eq!(app.editor_pane.list.ops.len(), 1);
        assert!(app.h_max > 0.0, "long line must create scroll range");
        // Horizontal wheel pans the editor pane.
        app.on_event(
            Event::Scroll {
                dx: -120.0,
                dy: 0.0,
            },
            &mut host,
        );
        assert!(app.h_scroll > 0.0);
        // Clamped at the max.
        app.on_event(
            Event::Scroll {
                dx: -1_000_000.0,
                dy: 0.0,
            },
            &mut host,
        );
        assert_eq!(app.h_scroll, app.h_max);
        app.on_event(
            Event::Scroll {
                dx: 1_000_000.0,
                dy: 0.0,
            },
            &mut host,
        );
        assert_eq!(app.h_scroll, 0.0);
        // End key puts the caret past the pane edge; follow_caret pans to it.
        app.on_event(
            Event::KeyDown {
                key: Key::End,
                mods: Modifiers::default(),
            },
            &mut host,
        );
        assert!(app.h_scroll > 0.0, "caret at line end should pan right");
        // Home pans back.
        app.on_event(
            Event::KeyDown {
                key: Key::Home,
                mods: Modifiers::default(),
            },
            &mut host,
        );
        assert_eq!(app.h_scroll, 0.0);
    }

    #[test]
    fn preview_still_wraps_in_split() {
        let mut app = open("readitor_test_pw.txt", "word ".repeat(120).trim_end());
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        app.on_event(ctrl('e'), &mut host); // Split
        let _ = app.render_headless(800, 600, 1.0);
        // The preview pane wraps (many rows); the editor pane does not (1 row).
        assert!(app.preview_rows.len() > 1, "preview should wrap");
        assert_eq!(app.editor_pane.list.ops.len(), 1, "editor must not wrap");
    }
}
