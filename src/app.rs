use crate::{
    core::{
        buffer::{Buffer, Pos},
        file_kind::FileKind,
    },
    error::Result,
    gfx::{
        canvas::Canvas,
        color::Color,
        font::{FontStyle, FontSystem},
        geom::{Point, Rect},
    },
    layout::{
        self,
        display_list::{DisplayList, DrawOp},
        theme::Theme,
        Ctx,
    },
    parse::{json, jsonl, markdown},
    platform::{self, Handler, Host, NullHost},
    settings,
    ui::{
        about,
        editor_view::EditorView,
        event::{CursorShape, Event, Key, Modifiers, MouseButton, WindowGeometry},
        find, gutter, help, menu,
        preview_view::{self, PaneMetrics, PaneScroll, PreviewView, Row as PreviewRow, SbGeom},
        scroll::ScrollState,
        statusbar,
        titlebar::{self, TitleHit},
    },
};
use std::{
    collections::HashSet,
    ops::{Deref, DerefMut},
    path::PathBuf,
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Preview,
    Edit,
    Split,
}

/// An in-progress overlay-scrollbar drag: press to release, the thumb tracking
/// the pointer in between. Held by [`App::sb_drag`], which `on_mouse_down`
/// clears before it looks at where the new press landed and `on_mouse_up`
/// clears on release — so a bar can never outlive its gesture.
#[derive(Clone, Copy)]
struct SbDrag {
    /// True for the editor pane's bar, false for the preview pane's.
    editor: bool,
    /// True for the horizontal bar, false for the vertical one.
    horizontal: bool,
    /// Pointer minus the thumb's leading edge, along the bar's axis.
    grab: f32,
}

/// A double-tap's selection, armed for a finger drag: the touch counterpart
/// of double-click-and-drag. The tap made the selection (word or line); a
/// finger that comes down soon after and pans extends it — the pan deltas
/// drive a virtual pointer from where the tap landed, because a moving
/// finger reports `Pan`, never `MouseMove`. Retired by `PanEnd` (no coast —
/// a selection drag is not a flick) and by the next press.
#[derive(Clone, Copy)]
struct TouchSel {
    /// Selection anchor the drag extends from.
    anchor: Pos,
    /// The finger's virtual window position: the double-tap's spot plus the
    /// pan deltas since.
    pointer: (f32, f32),
    /// The drag has begun — the recency window only gates the start.
    active: bool,
}

/// Caret blink half-period, in 40 ms `Event::Tick`s (~0.5 s).
const BLINK_TICKS: u32 = 12;

/// Smallest scroll delta treated as a discrete step worth gliding to. A wheel
/// notch is ~48 px and a PageDown a whole pane; a touchpad or a finger arrives
/// in deltas of a few px, and those must be applied the instant they land or
/// the input feels sticky.
const GLIDE_MIN_STEP: f32 = 16.0;

/// How long a reported window size/place has to hold still before it is
/// written to the session file, in 40 ms `Event::Tick`s (~1 s).
///
/// The session is written on the way out anyway, but that only covers a clean
/// exit: a killed process, a power cut or a crash never gets there, and a
/// window that moved in that session would reopen at the rect it had at the
/// last *clean* exit. So the write also happens from the heartbeat, once the
/// reports stop — the delay being what keeps a drag (which reports every
/// intermediate rect) to a single write at the end.
const GEOMETRY_SAVE_TICKS: u32 = 25;

/// One open file: everything that is per-document lives here.
pub struct Tab {
    pub path: Option<PathBuf>,
    pub kind: FileKind,
    pub buffer: Buffer,
    pub editor: EditorView,
    /// Plain-text pane backing Edit mode and the bottom half of Split.
    pub editor_pane: PreviewView,
    /// Last saved (or loaded) text, normalized exactly as the buffer holds
    /// it. `dirty` is recomputed against this after every edit, so undoing
    /// back to the saved content clears the tab's save button again.
    pub saved_text: String,
    /// Collapsed JSON tree nodes (pre-order ids, stable across toggles).
    pub collapsed: HashSet<usize>,
    /// Right-side status suffix set by layout (`error 2:5`, `3 records`, …).
    pub status_extra: String,
    pub scroll: ScrollState,
    /// Horizontal scroll of the editor pane (no-wrap lines), its glide
    /// destination, and its max. Mirrors the vertical scroller: coarse
    /// wheel steps glide `h_scroll` toward `h_target` over frames, while
    /// pointer-driven input (thumb drag, finger, caret) sets both at once.
    pub h_scroll: f32,
    pub h_target: f32,
    pub h_max: f32,
    /// File modification time as last seen or written (reload detection).
    pub mtime: Option<std::time::SystemTime>,
    /// Virtual-layout plan for a big JSONL (record tops, heights, id bases).
    /// Empty for every other kind — small JSONLs take the plain layout.
    pub plan: layout::json_tree::Plan,
    /// The parsed JSONL records, for the virtualized path: the plan and the
    /// band layouts index into this. Restated only when the buffer changes —
    /// relayouts (scroll, resize, collapse) reuse it instead of re-parsing
    /// a 33 MB document per frame.
    pub records: Vec<jsonl::Record>,
    /// Width the current `records`/`plan` were built at, so a resize rebuilds
    /// them and a repaint at the same width does not.
    pub plan_width: f32,
    /// Collapsed-set generation at which `plan` was built; a toggle changes
    /// record heights, so the plan is rebuilt against the new set.
    pub plan_collapsed_gen: u32,
    /// Bumped on every collapse toggle so the plan knows to rebuild.
    pub collapsed_gen: u32,
}

/// JSONL files at or below this many lines take the plain (full) layout; a
/// bigger one goes virtualized (per-record plan + visible-band paint). The
/// threshold is generous — a 4000-record display list is milliseconds — and
/// small files keep the byte-identical single-path behavior.
pub const VIRTUAL_JSONL_LINES: usize = 4_000;

/// Files above this are refused rather than opened (see [`size_error`]):
/// everything Editor does — buffer, layout, render — is whole-document, so
/// a huge file is a memory and latency trap, and windowing it is not in this
/// milestone.
const MAX_OPEN_BYTES: u64 = 64 * 1024 * 1024;

/// The refusal message for an oversized file, or `None` when it fits.
/// Split out so the rule is testable without writing a 64 MB fixture.
fn size_error(len: u64) -> Option<String> {
    (len > MAX_OPEN_BYTES).then(|| {
        format!(
            "file is {} MB — larger than the {} MB open limit",
            len >> 20,
            MAX_OPEN_BYTES >> 20
        )
    })
}

impl Tab {
    fn untitled() -> Self {
        Self {
            path: None,
            kind: FileKind::PlainText,
            buffer: Buffer::from_text(""),
            editor: EditorView::default(),
            editor_pane: PreviewView::default(),
            saved_text: String::new(),
            collapsed: HashSet::new(),
            status_extra: String::new(),
            scroll: ScrollState::default(),
            h_scroll: 0.0,
            h_target: 0.0,
            h_max: 0.0,
            mtime: None,
            plan: layout::json_tree::Plan {
                slots: Vec::new(),
                height: 0.0,
            },
            records: Vec::new(),
            plan_width: 0.0,
            plan_collapsed_gen: 0,
            collapsed_gen: 0,
        }
    }

    fn from_path(p: PathBuf) -> Result<Self> {
        // Refuse oversized files before reading: the whole text is held in
        // memory and laid out at once, so a huge file is a memory and
        // latency trap — a clear refusal beats a frozen window.
        if let Some(msg) = size_error(std::fs::metadata(&p)?.len()) {
            return Err(crate::error::Error::Platform(msg));
        }
        let buffer = Buffer::from_text(&std::fs::read_to_string(&p)?);
        // Snapshot the normalized text (line endings are stripped on load),
        // so a file the user never touches never compares dirty.
        let saved_text = buffer.text();
        Ok(Self {
            kind: FileKind::from_path(&p),
            buffer,
            saved_text,
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
    /// Full window-edge strip the platform claims for resizing (physical px).
    /// Overlay scrollbars are inset by the *bar-safe* share of it
    /// ([`App::bar_grip`]) so a press on the bar scrolls instead of starting a
    /// window resize; `grip_at` hands the rest of it back to the OS.
    edge: f32,
    zoom: f32,
    layout_dirty: bool,
    /// F1 help overlay visible.
    help_open: bool,
    /// F2 about box visible.
    about_open: bool,
    /// Where the About box's update-toggle row is, refreshed by every paint
    /// of the box (hit-tested in `on_mouse_down`).
    about_toggle: Option<Rect>,
    /// Live Ctrl+F find bar, if open.
    find: Option<find::Find>,
    /// Live right-click context menu, if any.
    menu: Option<menu::Menu>,
    /// Second Ctrl+W on a dirty tab discards it; any other action clears.
    close_armed: bool,
    /// Title-bar hover state (tab / window-button highlighting).
    hover_title: Option<TitleHit>,
    /// Clickable status-bar segments, refreshed on every paint.
    status_buttons: Vec<statusbar::StatusButton>,
    /// The status bar's Copy button, present only while a selection exists.
    /// Rect refreshed on every paint, hit-tested on press.
    copy_btn: Option<statusbar::StatusButton>,
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
    /// Last window geometry the platform reported while the window was *not*
    /// maximized: the size and place the next session should reopen at, and the
    /// answer to [`Handler::initial_window`]. `None` until the platform reports
    /// one (a first run, or a headless one, which never has a window).
    window: Option<WindowGeometry>,
    /// Whether the window is maximized right now. The OS owns this — we only
    /// remember it, so the next session opens the same way without losing the
    /// rect above.
    window_maximized: bool,
    /// Auto-update checks on launch (About box toggle, persisted).
    auto_update: bool,
    /// A glide is in flight, so the host is currently running its fast frame
    /// clock (and must be told when it can stop).
    scrolling: bool,
    /// Recently opened files, most recent first, per [`settings::RECENT_MAX`].
    /// Restored from the session at startup and re-noted on every open, so the
    /// next start can offer the way back in.
    recent: Vec<PathBuf>,
    /// When the last animation frame was handled. The glide is eased over
    /// *time*, so each frame needs to know how much of it passed — a host that
    /// stalls, or one that only has the 40 ms heartbeat, still advances by the
    /// time that actually went by.
    last_frame: Instant,
    /// A size/place/maximized change the platform reported that is not on disk
    /// yet. Waiting for the window to hold still is what makes a drag — a
    /// stream of reports, of which only the last matters — one write instead of
    /// one per frame (see [`GEOMETRY_SAVE_TICKS`]).
    geometry_dirty: bool,
    /// Ticks of stillness left before that pending change is written; only
    /// meaningful while `geometry_dirty`.
    geometry_idle: u32,
    /// Live overlay-scrollbar drag, if any.
    sb_drag: Option<SbDrag>,
    /// A double-tap's selection armed for a finger drag (see [`TouchSel`]).
    touch_sel: Option<TouchSel>,
    /// Click counting for double/triple selection: the running count and
    /// where/when (tick) the last press landed.
    clicks: u8,
    last_click: Option<((f32, f32), u32)>,
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
        // An Untitled buffer has nothing to preview: start in Edit mode.
        let mode = if tab.path.is_none() {
            Mode::Edit
        } else {
            Mode::Preview
        };
        Ok(Self {
            tabs: vec![tab],
            active: 0,
            mode,
            dark: false,
            theme: Theme::light(),
            fonts: FontSystem::load_embedded(),
            preview: PreviewView::default(),
            drag_anchor: None,
            size: platform::DEFAULT_SIZE,
            scale: 1.0,
            edge: platform::edge_grip(),
            zoom: 1.0,
            layout_dirty: true,
            help_open: false,
            about_open: false,
            about_toggle: None,
            find: None,
            menu: None,
            close_armed: false,
            hover_title: None,
            status_buttons: Vec::new(),
            copy_btn: None,
            preview_rows: Vec::new(),
            preview_sel: None,
            preview_drag: false,
            last_mouse: (0.0, 0.0),
            theme_manual: false,
            ticks: 0,
            blink: 0,
            window: None,
            window_maximized: false,
            auto_update: true,
            scrolling: false,
            recent: Vec::new(),
            last_frame: Instant::now(),
            geometry_dirty: false,
            geometry_idle: 0,
            sb_drag: None,
            touch_sel: None,
            clicks: 0,
            last_click: None,
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

    /// Pick the view mode directly (Ctrl+E cycles into it, `--mode` names it).
    /// Any preview selection belongs to the pane that just went away.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.clear_preview_sel();
        self.layout_dirty = true;
    }

    /// Toggle background update checks (About box row / cli startup). On by
    /// default; turning it off only stops the *check* — `--update` keeps
    /// working, and every install still goes through the consent + checksum
    /// gates in `update.rs`.
    pub fn set_auto_update(&mut self, on: bool) {
        self.auto_update = on;
    }

    /// Restore a persisted session (cli startup): reopen the tabs that still
    /// exist, focus `active`, switch to the saved view mode.
    pub fn restore(&mut self, files: &[PathBuf], active: usize, mode: Mode) {
        let mut host = NullHost::default();
        for path in files {
            if path.is_file() {
                self.open_path(path.clone(), &mut host);
            }
        }
        self.active = active.min(self.tabs.len().saturating_sub(1));
        self.mode = mode;
        self.focus_untitled_in_edit();
        self.clear_preview_sel();
        self.layout_dirty = true;
    }

    /// Take over the recent-files list the previous session kept. Called from
    /// `cli` startup alongside [`Self::restore`]; an app constructed later in
    /// a test keeps an empty list unless it restores.
    pub fn restore_recent(&mut self, recent: &[PathBuf]) {
        self.recent = recent.to_vec();
    }

    /// Remember `path` as recently opened. `open_path` and save-as do this on
    /// their own; the CLI's file argument goes through here, because it is
    /// opened by `App::new` rather than `open_path`.
    pub fn note_recent(&mut self, path: &std::path::Path) {
        settings::note_recent(&mut self.recent, path);
    }

    /// Adopt the window rect the last session closed at. Called before
    /// `platform::run`, because the window does not exist yet: the platform
    /// reads it back through [`Handler::initial_window`] while creating it, and
    /// the OS stays the authority on where it actually ends up.
    pub fn restore_window(&mut self, rect: Option<(i32, i32, u32, u32)>, maximized: bool) {
        self.window = rect.map(|(x, y, width, height)| WindowGeometry {
            x,
            y,
            width,
            height,
            maximized: false,
        });
        self.window_maximized = maximized;
    }

    /// The session to write for the next start (theme, view mode, open tabs,
    /// and where the window was) — split out from [`App::persist`] so what gets
    /// written is testable without touching the config directory.
    fn settings(&self) -> settings::Settings {
        settings::Settings {
            dark: self.dark,
            manual: self.theme_manual,
            mode: match self.mode {
                Mode::Preview => 0,
                Mode::Edit => 1,
                Mode::Split => 2,
            },
            files: self.tabs.iter().filter_map(|t| t.path.clone()).collect(),
            recent: self.recent.clone(),
            active: self.active,
            // The *normal* rect, not the maximized one: reopening at the work
            // area's size would leave a window that is the size of the screen
            // as soon as it is un-maximized.
            window: self.window.map(|g| (g.x, g.y, g.width, g.height)),
            maximized: self.window_maximized,
            auto_update: self.auto_update,
        }
    }

    /// Write the session (theme, view mode, open tabs, window) for the next
    /// start.
    fn persist(&self) {
        settings::save(&self.settings());
    }

    /// Leave: remember the session first. Every way out goes through here — the
    /// window's ×, Ctrl+Q, and the OS's close request — because where the window
    /// *is* only exists while it does. Nothing else writes it on a move or a
    /// resize: a drag produces a stream of those, and the next session only
    /// needs the last one.
    fn quit(&mut self, host: &mut dyn Host) {
        self.persist();
        host.quit();
    }

    /// Set the horizontal position outright: thumb drags, track pages, the
    /// finger, caret follow. Pointer-driven, so any wheel glide ends with it.
    fn set_h_scroll(&mut self, offset: f32) {
        self.h_scroll = offset.clamp(0.0, self.h_max);
        self.h_target = self.h_scroll;
    }

    /// Horizontal wheel/trackpad movement pans the no-wrap editor lines with
    /// the same glide coarse steps get vertically; fine deltas go straight
    /// through. Preview panes wrap, so they have nothing to pan.
    fn pan_horizontal(&mut self, dx: f32, host: &mut dyn Host) {
        if self.mode == Mode::Preview {
            return;
        }
        let step = -dx;
        if step.abs() >= GLIDE_MIN_STEP {
            self.h_target = (self.h_target + step).clamp(0.0, self.h_max);
            self.h_scroll = self.h_scroll.clamp(0.0, self.h_max);
            if (self.h_target - self.h_scroll).abs() <= crate::ui::scroll::GLIDE_SETTLED {
                self.h_scroll = self.h_target;
            }
            self.start_glide_clock(host);
        } else {
            self.set_h_scroll(self.h_scroll + step);
        }
        host.request_redraw();
    }

    /// Touch panning stays exactly on the finger: direct, and any wheel glide
    /// in flight ends (its remaining distance belongs to the wheel, not to
    /// the finger) — the horizontal half of what `scroll_direct` does
    /// vertically.
    fn pan_horizontal_direct(&mut self, dx: f32) {
        if self.mode != Mode::Preview {
            self.set_h_scroll(self.h_scroll - dx);
        }
    }

    /// Start (or refresh) the animation clock: the glide or coast needs
    /// frames, and on a host without one the 40 ms heartbeat steps it.
    fn start_glide_clock(&mut self, host: &mut dyn Host) {
        if (self.scroll.animating() || self.h_scroll != self.h_target) && !self.scrolling {
            // A zero-velocity release leaves nothing to animate; don't start
            // the host's frame clock for it.
            self.scrolling = true;
            self.last_frame = Instant::now();
            host.set_animating(true);
        }
    }

    /// Scroll to a coarse step's distance *over the next few frames* rather
    /// than in one jump, which is what removes the stepping from wheel and
    /// paging input, and start the host's fast frame clock if it is not
    /// already running.
    fn scroll_glide(&mut self, dy: f32, host: &mut dyn Host) {
        self.scroll.scroll_smooth(dy);
        self.start_glide_clock(host);
        host.request_redraw();
    }

    /// Start a glide for input whose delta is a discrete step, and let anything
    /// finer through unanimated (see [`GLIDE_MIN_STEP`]).
    fn scroll_input(&mut self, dy: f32, host: &mut dyn Host) {
        if dy.abs() >= GLIDE_MIN_STEP {
            self.scroll_glide(dy, host);
        } else {
            self.scroll_direct(dy, host);
        }
    }

    /// The finger let go: hand the ride to momentum. A flick keeps the view
    /// moving the way it was moving and bleeds off — the rolodex feel — and a
    /// release after the finger held still coasts zero and stays put. Pan's
    /// sign flips like every other vertical input: dragging up the screen
    /// moves the content down the document, so the velocity does too.
    fn coast(&mut self, _vx: f32, vy: f32, host: &mut dyn Host) {
        // Horizontal momentum is dropped on purpose: the coast state lives on
        // the vertical scroller, preview panes have nothing to pan sideways,
        // and a flick is vertical by nature. Only the vertical ride continues.
        self.scroll.coast_from(-vy);
        self.start_glide_clock(host);
        host.request_redraw();
    }

    /// Move the view *now*, because the pointer is driving it — a scrollbar
    /// drag, drag auto-scroll, a finger. Any glide in flight is over (the view
    /// is where the pointer puts it), and the host's fast clock goes with it
    /// unless the horizontal glide still needs frames.
    fn scroll_direct(&mut self, dy: f32, host: &mut dyn Host) {
        self.scroll.scroll_by(dy);
        self.end_glide(host);
        host.request_redraw();
    }

    /// Stop the animation clock. Safe when anything is still animating: the
    /// clock stays up until both axes have arrived.
    fn end_glide(&mut self, host: &mut dyn Host) {
        if self.scrolling && !self.scroll.animating() && self.h_scroll == self.h_target {
            self.scrolling = false;
            host.set_animating(false);
        }
    }

    /// One animation frame: advance in-flight glides, and stop asking for
    /// frames once everything has arrived (the last frame is still painted).
    fn on_frame(&mut self, host: &mut dyn Host) {
        if !self.scrolling {
            return;
        }
        let now = Instant::now();
        // A long gap (a stall, a host that only runs the heartbeat) is clamped:
        // the easing must not jump the whole distance because a frame was late.
        let dt_ms = ((now - self.last_frame).as_secs_f32() * 1000.0).clamp(1.0, 100.0);
        self.last_frame = now;
        self.scroll.step(dt_ms);
        // The horizontal glide rides the same clock with the same easing.
        self.h_target = self.h_target.clamp(0.0, self.h_max);
        if self.h_scroll != self.h_target {
            self.h_scroll = crate::ui::scroll::glide_step(self.h_scroll, self.h_target, dt_ms);
        }
        if self.scroll.animating() || self.h_scroll != self.h_target {
            host.request_redraw();
        } else {
            self.scrolling = false;
            host.set_animating(false);
            host.request_redraw();
        }
    }

    /// Window title. Unsaved state is *not* encoded here any more — the tab's
    /// save button carries it (the status bar and window title stay clean).
    pub fn title(&self) -> String {
        format!("{} — Editor", self.name())
    }
    // ------------------------------------------------------------ tabs

    /// Open `path` as a tab: focus the existing tab when re-opened, else load
    /// it as a new one. Read failures land in the status bar, never a crash.
    pub fn open_path(&mut self, path: PathBuf, host: &mut dyn Host) {
        self.close_armed = false;
        settings::note_recent(&mut self.recent, &path);
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
                    self.persist();
                }
                Err(e) => self.status_extra = format!("open failed: {e}"),
            }
        }
        host.request_redraw();
    }

    /// Landing on an Untitled buffer drops into Edit mode: there is nothing to
    /// preview and the user is about to type.
    fn focus_untitled_in_edit(&mut self) {
        if self.path.is_none() {
            self.mode = Mode::Edit;
        }
    }

    fn switch_to(&mut self, i: usize) {
        if i == self.active || i >= self.tabs.len() {
            return;
        }
        self.active = i;
        self.focus_untitled_in_edit();
        self.close_armed = false;
        self.clear_preview_sel();
        self.layout_dirty = true;
        self.persist();
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
        self.focus_untitled_in_edit();
        self.close_armed = false;
        self.clear_preview_sel();
        self.layout_dirty = true;
        self.persist();
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
        self.focus_untitled_in_edit();
        self.close_armed = false;
        self.clear_preview_sel();
        self.layout_dirty = true;
        self.persist();
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
    /// the horizontal scroll of the no-wrap lines and excludes the gutter
    /// rail, which the content is inset by when painted).
    fn editor_point(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let rail = gutter::rail_w(&self.fonts, &self.ctx(), self.buffer.line_count());
        match self.mode {
            Mode::Edit => {
                let view = self.viewport();
                Some((
                    x - view.x - rail + self.h_scroll,
                    y - view.y + self.editor_scroll(),
                ))
            }
            Mode::Split => {
                let right = self.split_panes().1;
                (x >= right.x).then_some((
                    x - right.x - rail + self.h_scroll,
                    y - right.y + self.editor_scroll(),
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
            Mode::Preview => Some((x - view.x, y - view.y + self.preview_scroll())),
            Mode::Split if x < self.split_panes().0.right() => {
                Some((x - view.x, y - view.y + self.preview_scroll()))
            }
            _ => None,
        }
    }

    /// Shared 0..1 progress through the document: the thing both Split
    /// thumbs stay locked to. One pixel offset cannot serve two panes of
    /// different heights — the short pane would sail past its own end — so
    /// each pane paints its own share of this ratio instead.
    fn scroll_ratio(&self) -> f32 {
        let max = self.scroll.max();
        if max <= 0.0 {
            0.0
        } else {
            (self.scroll.offset / max).clamp(0.0, 1.0)
        }
    }

    /// Max scrollable pixels of the preview pane.
    fn preview_max(&self) -> f32 {
        (self.preview.list.height - self.viewport().h).max(0.0)
    }

    /// Max scrollable pixels of the editor pane.
    fn editor_max(&self) -> f32 {
        (self.editor_pane.list.height - self.editor_area().h).max(0.0)
    }

    /// What the preview pane paints and hit-tests at: its own share of the
    /// shared progress. Identity outside Split.
    fn preview_scroll(&self) -> f32 {
        if self.mode == Mode::Split {
            self.scroll_ratio() * self.preview_max()
        } else {
            self.scroll.offset
        }
    }

    /// What the editor pane paints and hit-tests at (text, gutter, caret,
    /// thumb): its own share of the shared progress, so it only ever shows
    /// real lines. Identity outside Split.
    fn editor_scroll(&self) -> f32 {
        if self.mode == Mode::Split {
            self.scroll_ratio() * self.editor_max()
        } else {
            self.scroll.offset
        }
    }

    /// A pane-space position back onto the shared scroller (thumb drag,
    /// caret follow): keep the ratio, end any ride, like `scroll_by(0)`.
    fn set_scroll_ratio(&mut self, ratio: f32) {
        let max = self.scroll.max();
        self.scroll.offset = (ratio.clamp(0.0, 1.0) * max).clamp(0.0, max);
        self.scroll.scroll_by(0.0);
    }

    /// Current geometry of one pane's scrollbar (editor pane?, horizontal?).
    fn sb_geom(&self, editor: bool, horizontal: bool) -> Option<SbGeom> {
        let rail = gutter::rail_w(&self.fonts, &self.ctx(), self.buffer.line_count());
        let ps = PaneScroll::editor(self.editor_scroll(), self.h_scroll, self.h_max, rail);
        match (self.mode, editor, horizontal) {
            (Mode::Preview, _, false) => {
                self.preview
                    .v_sb(self.viewport(), self.preview_scroll(), self.metrics())
            }
            (Mode::Edit, true, true) => self.editor_pane.h_sb(self.viewport(), ps, self.metrics()),
            (Mode::Edit, _, false) => self.editor_pane.v_sb(self.viewport(), ps.y, self.metrics()),
            (Mode::Split, false, false) => {
                self.preview
                    .v_sb(self.split_panes().0, self.preview_scroll(), self.metrics())
            }
            (Mode::Split, true, true) => {
                self.editor_pane
                    .h_sb(self.split_panes().1, ps, self.metrics())
            }
            (Mode::Split, true, false) => {
                self.editor_pane
                    .v_sb(self.split_panes().1, ps.y, self.metrics())
            }
            _ => None,
        }
    }

    /// The overlay scrollbar under a client-space point, if any.
    fn sb_at(&self, x: f32, y: f32) -> Option<(bool, bool, SbGeom)> {
        let p = Point { x, y };
        let bars: &[(bool, bool)] = match self.mode {
            Mode::Preview => &[(false, false)],
            Mode::Edit => &[(true, true), (true, false)],
            Mode::Split => &[(false, false), (true, true), (true, false)],
        };
        bars.iter().find_map(|&(e, h)| {
            self.sb_geom(e, h)
                .filter(|g| g.zone.contains(p))
                .map(|g| (e, h, g))
        })
    }

    /// True when (x, y) is over the editor pane's line-number rail: clicks
    /// there must not place the caret, and the pointer stays an arrow.
    fn over_gutter(&self, x: f32, y: f32) -> bool {
        if self.mode == Mode::Preview {
            return false;
        }
        let area = self.editor_area();
        if x < area.x || x >= area.right() || y < area.y || y > area.bottom() {
            return false;
        }
        x < area.x + gutter::rail_w(&self.fonts, &self.ctx(), self.buffer.line_count())
    }

    /// 1/2/3 for a single/double/triple click at (x, y): a press within
    /// ~0.5 s and a few px of the last one counts up, anything else resets.
    fn click_seq(&mut self, x: f32, y: f32) -> u8 {
        let again = self.last_click.is_some_and(|(p, t)| {
            (p.0 - x).abs() <= 6.0 * self.scale
                && (p.1 - y).abs() <= 6.0 * self.scale
                && self.ticks.wrapping_sub(t) <= BLINK_TICKS
        });
        self.clicks = if again { self.clicks % 3 + 1 } else { 1 };
        self.last_click = Some(((x, y), self.ticks));
        self.clicks
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
        // Everything on screen persists; a big document's own scale (this
        // runs on every resize, zoom and pane switch, so whole-document work
        // at 1M+ rows is what turns those into freezes).

        if self.mode == Mode::Edit {
            // One pass over the buffer's lines and the scroll range is the
            // whole job. status_extra is untouched here: absent a reader
            // pane there is no suffix to rewrite.
            tab.status_extra.clear();
            tab.editor_pane.list = layout::text::layout_plain(&tab.buffer, edit_w, &ctx, false);
            let rail = gutter::rail_w(&self.fonts, &ctx, tab.buffer.line_count());
            // h_max needs the widest display row. The old loop measured
            // every laid-out op — for a million-line pane that is a second
            // full walk *and* a full glyph-measure per line (13 s on the
            // 34 MB log), all to answer "how wide is the longest line".
            // In Edit the pane is one row per line, so it is the widest
            // expanded buffer line: tracking it during one line walk and
            // measuring only that line gives the identical max.
            let p = layout::text::plain_params(edit_w, &ctx, false);
            let mut longest: String = String::new();
            let mut longest_w: usize = 0;
            for i in 0..tab.buffer.line_count() {
                let line = tab.buffer.line(i).replace('\t', "    ");
                let w = line.len();
                if w > longest_w {
                    longest_w = w;
                    longest = line;
                }
            }
            let extent = ctx.fonts.measure(FontStyle::Mono, &longest, p.size);
            // Content is painted inset by the rail, so it scrolls that much
            // farther before hitting the right edge.
            tab.h_max = (extent + rail + 32.0 * self.scale - edit_w).max(0.0);
            tab.h_scroll = tab.h_scroll.min(tab.h_max);
            tab.h_target = tab.h_target.min(tab.h_max);
            let content = tab.editor_pane.list.height;
            tab.scroll.content_height = content;
            tab.scroll.viewport_height = view_h;
            tab.scroll.offset = ratio * content;
            tab.scroll.scroll_by(0.0);
            return;
        }
        let list = match tab.kind {
            FileKind::Json => {
                Self::layout_json(&tab.buffer, &tab.collapsed, &mut tab.status_extra, w, &ctx)
            }
            FileKind::JsonLines if tab.buffer.line_count() <= VIRTUAL_JSONL_LINES => {
                Self::layout_jsonl(&tab.buffer, &tab.collapsed, &mut tab.status_extra, w, &ctx)
            }
            FileKind::JsonLines => {
                // Virtualized: parse once into the tab (relayouts reuse it),
                // plan once per width/zoom, and let paint lay out only the
                // visible band of records. The display list height comes
                // from the plan, so scrollbar math is unchanged.
                if tab.records.is_empty() {
                    let text = tab.buffer.text();
                    tab.records = jsonl::parse_parallel(&text);
                }
                let errors = tab.records.iter().filter(|r| r.value.is_err()).count();
                tab.status_extra = if errors == 0 {
                    format!(
                        "{} record{}",
                        tab.records.len(),
                        if tab.records.len() == 1 { "" } else { "s" }
                    )
                } else {
                    format!(
                        "{} record{}, {} error{}",
                        tab.records.len(),
                        if tab.records.len() == 1 { "" } else { "s" },
                        errors,
                        if errors == 1 { "" } else { "s" }
                    )
                };
                if tab.plan_width != w
                    || tab.plan.height == 0.0
                    || tab.plan.slots.len() != tab.records.len()
                    || tab.plan_collapsed_gen != tab.collapsed_gen
                {
                    tab.plan =
                        layout::json_tree::build_plan(&tab.records, &tab.collapsed, w, &ctx, true);
                    tab.plan_width = w;
                    tab.plan_collapsed_gen = tab.collapsed_gen;
                }
                layout::display_list::DisplayList {
                    ops: Vec::new(), // painted per-frame by paint_panes band path
                    height: tab.plan.height,
                    hits: Vec::new(),
                }
            }
            FileKind::Markdown => {
                tab.status_extra.clear();
                let doc = markdown::parse(&tab.buffer.text());
                layout::markdown::layout(&doc, w, &ctx)
            }
            FileKind::Csv => {
                let sheet = crate::parse::csv::parse(&tab.buffer.text());
                tab.status_extra = if sheet.cols == 0 {
                    String::new()
                } else if sheet.capped {
                    format!(
                        "{} rows × {} cols (showing {})",
                        sheet.total_rows,
                        sheet.cols,
                        sheet.rows.len()
                    )
                } else {
                    format!("{} rows × {} cols", sheet.total_rows, sheet.cols)
                };
                layout::csv::layout(&sheet, w, &ctx)
            }
            FileKind::Yaml => {
                tab.status_extra.clear();
                layout::highlight::layout(&tab.buffer, w, &ctx, layout::highlight::Kind::Yaml, "")
            }
            FileKind::Toml => {
                tab.status_extra.clear();
                layout::highlight::layout(&tab.buffer, w, &ctx, layout::highlight::Kind::Toml, "")
            }
            FileKind::Ini => {
                tab.status_extra.clear();
                layout::highlight::layout(&tab.buffer, w, &ctx, layout::highlight::Kind::Ini, "")
            }
            FileKind::Log => {
                tab.status_extra = log_counts(&tab.buffer);
                layout::highlight::layout(&tab.buffer, w, &ctx, layout::highlight::Kind::Log, "")
            }
            FileKind::Code => {
                tab.status_extra.clear();
                let ext = tab
                    .path
                    .as_ref()
                    .and_then(|p| p.extension())
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                layout::highlight::layout(&tab.buffer, w, &ctx, layout::highlight::Kind::Code, &ext)
            }
            FileKind::PlainText => layout::text::layout_plain(&tab.buffer, w, &ctx, true),
        };
        if self.mode != Mode::Preview {
            // The editor pane never wraps: one row per line, h-scrolled.
            tab.editor_pane.list = layout::text::layout_plain(&tab.buffer, edit_w, &ctx, false);
            let rail = gutter::rail_w(&self.fonts, &ctx, tab.buffer.line_count());
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
            // Content is painted inset by the rail, so it scrolls that much
            // farther before hitting the right edge.
            tab.h_max = (extent + rail + 32.0 * self.scale - edit_w).max(0.0);
            tab.h_scroll = tab.h_scroll.min(tab.h_max);
            tab.h_target = tab.h_target.min(tab.h_max);
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
        // The find bar's matches belong to the active buffer: re-run the
        // search whenever the content, tab, or view changed (all of which
        // dirty layout). A full rescan, not the incremental narrow — the
        // buffer may have moved under the old match list. No jump — paint
        // must not move the view.
        if let Some(mut f) = self.find.take() {
            let caret = self.tabs[self.active].editor.caret;
            f.rescan(&self.tabs[self.active].buffer, caret);
            self.find = Some(f);
        }
        self.layout_dirty = false;
    }

    /// Buffer changed: live re-render the document pane and keep the caret on
    /// screen. (Split's top pane re-renders through `relayout` on paint.)
    fn after_edit(&mut self) {
        self.layout_dirty = true;
        self.sync_dirty();
        self.poke_caret();
        self.follow_caret();
    }

    /// Recompute the dirty flag against the last saved text: every mutation
    /// (typing, delete, paste, cut, undo, redo) funnels through
    /// `after_edit`, so undoing back to the saved content clears the tab's
    /// save button again instead of leaving it stuck on.
    ///
    /// The compare is byte-identical equality, so the O(n²) `text()` join a
    /// direct `!=` (or `PartialEq`) compare of whole documents would run —
    /// per-mutation, per keystroke — is skipped in favor of a same-shaped
    /// line walk, keeping huge buffers responsive while editing.
    fn sync_dirty(&mut self) {
        let dirty = !same_lines(&self.buffer, &self.saved_text);
        self.buffer.dirty = dirty;
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
                self.about_open = false;
                self.find = None;
                self.clear_preview_sel();
            }
            // The two overlays are mutually exclusive.
            Key::F(1) => {
                self.help_open = !self.help_open;
                self.about_open = false;
            }
            Key::F(2) => {
                self.about_open = !self.about_open;
                self.help_open = false;
            }
            Key::Char('q') if mods.ctrl => self.quit(host),
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
                self.set_dark_manual(!self.dark);
                self.persist();
            }
            Key::Char('s') if mods.ctrl => self.save(host),
            Key::Char('r') if mods.ctrl => self.reload_file(host),
            Key::Char('e') if mods.ctrl => {
                let next = match self.mode {
                    Mode::Preview => Mode::Edit,
                    Mode::Edit => Mode::Split,
                    Mode::Split => Mode::Preview,
                };
                self.set_mode(next);
                self.persist();
            }
            Key::Char('f') if mods.ctrl => {
                // Opening the bar closes the full-window overlays; an already
                // open bar keeps its query (Ctrl+F is "focus", not "reset").
                if self.find.is_none() {
                    self.find = Some(find::Find::default());
                    self.help_open = false;
                    self.about_open = false;
                }
            }
            // -- find bar (while open) ---------------------------------------
            // The bar owns the plain editing keys: typing goes to the query
            // (via `Event::Text`), Backspace edits it, Enter/F3 step through
            // the matches. Ctrl chords stay app chords.
            Key::Backspace | Key::Delete if self.find.is_some() && !mods.ctrl && !mods.alt => {
                if key == Key::Backspace {
                    self.find_pop();
                }
            }
            Key::Enter if self.find.is_some() && !mods.ctrl && !mods.alt => {
                self.find_step(mods.shift, host);
            }
            Key::Tab if self.find.is_some() && !mods.ctrl && !mods.alt => {}
            Key::F(3) if self.find.is_some() => self.find_step(mods.shift, host),
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
            // Keyboard steps glide like wheel notches: they are the same kind
            // of discrete jump, and holding a key then blends into one motion.
            Key::Down => self.scroll_input(step, host),
            Key::Up => self.scroll_input(-step, host),
            Key::PageDown => self.scroll_input(page, host),
            Key::PageUp => self.scroll_input(-page, host),
            Key::Home => self.scroll.offset = 0.0,
            Key::End => self.scroll.offset = self.scroll.max(),
            _ => return,
        }
        host.request_redraw();
    }

    fn on_mouse_down(&mut self, x: f32, y: f32, button: MouseButton, host: &mut dyn Host) {
        // Every press ends the gesture before it. A drag lasts from its press
        // until its release and nothing else ends it, so a release the platform
        // could not deliver — the pointer let go outside the client area, the
        // window deactivated mid-drag — would leave the thumb glued to the
        // pointer (and text selection stuck) until some later click happened to
        // end it. Starting fresh here bounds a stranded drag to one click
        // rather than leaving the app waiting for a release that never comes.
        self.drag_anchor = None;
        self.preview_drag = false;
        self.sb_drag = None;
        // A fresh press retires any armed double-tap selection.
        self.touch_sel = None;
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
        // The About box's update toggle is clickable: flip it and keep the
        // box open. Anywhere else on the overlay dismisses it.
        if self.about_open {
            if let Some(r) = self.about_toggle.filter(|r| r.contains(Point { x, y })) {
                self.set_auto_update(!self.auto_update);
                self.persist();
                let _ = r;
                host.request_redraw();
                return;
            }
            self.about_open = false;
            self.about_toggle = None;
            host.request_redraw();
            return;
        }
        if self.help_open {
            self.help_open = false;
            host.request_redraw();
            return;
        }
        // The find bar swallows presses on itself — except the × at its
        // right edge, which closes it. A click elsewhere just does what it
        // normally does.
        if self.find.is_some() && self.find_bar_rect().contains(Point { x, y }) {
            let bar = self.find_bar_rect();
            if find::close_rect(bar, self.scale).contains(Point { x, y }) {
                self.find = None;
            }
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
                TitleHit::TabSave(i) => {
                    self.active = i;
                    self.save(host);
                }
                TitleHit::TabClose(i) => {
                    self.active = i;
                    self.close_tab(host);
                }
                TitleHit::Minimize => host.minimize(),
                TitleHit::Maximize => host.toggle_maximize(),
                TitleHit::Close => self.quit(host),
                TitleHit::Drag | TitleHit::Client => {}
            }
            host.request_redraw();
            return;
        }
        // Status bar: clickable mode/theme/help/open/find buttons, plus the
        // Copy button that appears while something is selected.
        if y >= self.viewport().bottom() {
            let p = Point { x, y };
            // Copy first: it is the leftmost thing on the bar and only
            // exists while there is a selection to copy.
            if let Some(cb) = &self.copy_btn {
                if cb.rect.contains(p) {
                    if let Some(text) = self
                        .preview_selection_text()
                        .or_else(|| self.editor.copy(&self.buffer))
                    {
                        host.clipboard_set(&text);
                        self.status_extra = "copied".into();
                    }
                    host.request_redraw();
                    return;
                }
            }
            let hit = self
                .status_buttons
                .iter()
                .find(|b| b.rect.contains(p))
                .map(|b| b.id);
            if let Some(id) = hit {
                match id {
                    0 => self.mode = Mode::Preview,
                    1 => self.mode = Mode::Edit,
                    2 => self.mode = Mode::Split,
                    3 => {
                        self.help_open = !self.help_open;
                        self.about_open = false;
                    }
                    6 => {
                        self.about_open = !self.about_open;
                        self.help_open = false;
                    }
                    5 => {
                        if let Some(p) = host.open_file_dialog() {
                            self.open_path(p, host);
                        }
                    }
                    // The bar's on-screen counterpart: opens it, or closes it
                    // when it is already up (a touch-friendly way out).
                    7 => {
                        if self.find.is_some() {
                            self.find = None;
                        } else {
                            self.find = Some(find::Find::default());
                            self.help_open = false;
                            self.about_open = false;
                        }
                    }
                    // Same as Ctrl+T: manual toggle, stops following the OS.
                    _ => {
                        self.set_dark_manual(!self.dark);
                    }
                }
                self.clear_preview_sel();
                self.layout_dirty = true;
                self.persist();
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
                &self.recent,
                Rect::new(0.0, 0.0, self.size.0 as f32, self.size.1 as f32),
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
        // Overlay scrollbars: a thumb press starts a drag; a track click
        // pages toward the pointer and grabs the thumb there.
        if let Some((editor, horizontal, geom)) = self.sb_at(x, y) {
            let p = Point { x, y };
            let (pointer, lead, len) = if horizontal {
                (x, geom.thumb.x, geom.thumb.w)
            } else {
                (y, geom.thumb.y, geom.thumb.h)
            };
            if geom.thumb.contains(p) {
                self.sb_drag = Some(SbDrag {
                    editor,
                    horizontal,
                    grab: pointer - lead,
                });
            } else {
                let page = if horizontal {
                    self.editor_area().w * 0.9
                } else {
                    self.scroll.viewport_height * 0.9
                };
                let delta = if lead > pointer { -page } else { page };
                if horizontal {
                    self.set_h_scroll(self.h_scroll + delta);
                } else {
                    self.scroll_direct(delta, host);
                }
                self.sb_drag = Some(SbDrag {
                    editor,
                    horizontal,
                    grab: len / 2.0,
                });
            }
            host.request_redraw();
            return;
        }
        // Editor panes take precedence in Edit/Split; Preview and the Split top
        // pane stay read-only hit targets (toggles, links).
        if let Some((dx, doc_y)) = self.editor_point(x, y) {
            if self.over_gutter(x, y) {
                // The line-number rail is not text: no caret placement.
                host.request_redraw();
                return;
            }
            let ctx = self.ctx();
            let pos =
                self.editor
                    .caret_for_point(&self.buffer, dx, doc_y, self.editor_area().w, &ctx);
            match self.click_seq(x, y) {
                // Double-click selects the word, triple the whole line;
                // dragging afterwards extends from the selection's start.
                2 => {
                    let tab = &mut self.tabs[self.active];
                    tab.editor.select_word(&tab.buffer, pos);
                    self.drag_anchor = tab.editor.selection().map(|(a, _)| a);
                    self.arm_touch_sel(x, y);
                }
                3.. => {
                    let tab = &mut self.tabs[self.active];
                    tab.editor.select_line(&tab.buffer, pos);
                    self.drag_anchor = tab.editor.selection().map(|(a, _)| a);
                    self.arm_touch_sel(x, y);
                }
                _ => {
                    self.editor.caret = pos;
                    self.editor.anchor = None;
                    self.drag_anchor = Some(pos);
                }
            }
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
            menu::Action::About => self.about_open = true,
            // A recent-file entry: open it as a tab, bumping it to the front
            // of the list (open_path notes it).
            menu::Action::Recent(i) => {
                if let Some(p) = self.recent.get(i).cloned() {
                    self.open_path(p, host);
                }
            }
            // Painted section label between the clipboard items and the
            // recent list; clicking it does nothing.
            menu::Action::Header => {}
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
        // Overlay-scrollbar drag: the thumb follows the pointer. A vertical
        // drag sets the shared ratio through whichever pane's bar is held,
        // so both Split thumbs stay locked together.
        if let Some(drag) = self.sb_drag.as_ref().copied() {
            if let Some(g) = self.sb_geom(drag.editor, drag.horizontal) {
                let pointer = if drag.horizontal { x } else { y };
                let offset = g.offset_for(pointer - drag.grab);
                if drag.horizontal {
                    self.set_h_scroll(offset);
                } else if g.max > 0.0 {
                    self.set_scroll_ratio(offset / g.max);
                }
                host.request_redraw();
            }
            return;
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

    fn on_mouse_up(&mut self, _x: f32, _y: f32, button: MouseButton, host: &mut dyn Host) {
        if button == MouseButton::Left {
            self.drag_anchor = None;
            self.preview_drag = false;
            self.sb_drag = None;
        }
        host.request_redraw();
    }

    /// Arm the double-tap selection for a finger drag (see [`TouchSel`]):
    /// called from the double/triple-click branches, which have just made the
    /// selection the drag would extend.
    fn arm_touch_sel(&mut self, x: f32, y: f32) {
        if let Some(anchor) = self.editor.selection().map(|(a, _)| a) {
            self.touch_sel = Some(TouchSel {
                anchor,
                pointer: (x, y),
                active: false,
            });
        }
    }

    /// A finger drag while a double-tap's selection is armed extends that
    /// selection instead of scrolling. Returns `true` when the pan was
    /// consumed. The virtual pointer starts where the tap landed and rides
    /// the pan deltas — a moving finger reports `Pan`, never `MouseMove`.
    fn on_touch_pan(&mut self, dx: f32, dy: f32, host: &mut dyn Host) -> bool {
        let Some(mut ts) = self.touch_sel.take() else {
            return false;
        };
        // Only the *start* is gated: a finger that comes down within ~1 s of
        // the double-tap extends the selection; a later drag scrolls.
        if !ts.active {
            let recent = self
                .last_click
                .is_some_and(|(_, t)| self.ticks.wrapping_sub(t) <= 25);
            if !recent {
                return false;
            }
            ts.active = true;
        }
        ts.pointer.0 += dx;
        ts.pointer.1 += dy;
        let (px, py) = ts.pointer;
        if let Some((doc_x, doc_y)) = self.editor_point(px, py) {
            let ctx = self.ctx();
            let pos =
                self.editor
                    .caret_for_point(&self.buffer, doc_x, doc_y, self.editor_area().w, &ctx);
            self.editor.caret = pos;
            self.editor.anchor = Some(ts.anchor);
            self.poke_caret();
            self.touch_sel = Some(ts);
            host.request_redraw();
            return true;
        }
        // Off the editor pane (over the gutter rail, say): keep the gesture
        // armed but do nothing with this delta.
        self.touch_sel = Some(ts);
        true
    }

    /// Host heartbeat: auto-scroll (and keep extending the selection) while a
    /// drag is held near a pane edge; every ~2 s, watch for on-disk changes.
    fn on_tick(&mut self, host: &mut dyn Host) {
        self.ticks = self.ticks.wrapping_add(1);
        // Keep an in-flight glide moving even on a host that cannot run a
        // faster frame clock than this one; `Event::Frame` normally does it.
        if self.scrolling {
            self.on_frame(host);
        }
        // Write a window geometry the platform reported, once it has held
        // still, so that a session which is killed rather than closed still
        // reopens where it was left. Ahead of the drag branches below, which
        // return early — this must not be skipped by a gesture.
        if self.geometry_dirty {
            self.geometry_idle = self.geometry_idle.saturating_sub(1);
            if self.geometry_idle == 0 {
                self.geometry_dirty = false;
                self.persist();
            }
        }
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
            self.scroll_direct(dy, host);
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
            self.scroll_direct(dy, host);
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
        if s.chars().any(char::is_control) {
            return;
        }
        // The find bar owns plain typing while it is open.
        if let Some(f) = self.find.as_mut() {
            f.query.push_str(s);
            self.find_refresh_and_jump();
            host.request_redraw();
            return;
        }
        if self.mode == Mode::Preview {
            return;
        }
        self.close_armed = false;
        let tab = &mut self.tabs[self.active];
        tab.editor.insert_text(&mut tab.buffer, s);
        self.after_edit();
        host.request_redraw();
    }

    /// Find-bar glue (the bar's state lives in `ui::find`; these are the App
    /// side effects the key/text bindings call).
    /// Backspace in the query: drop a char and re-land on a match.
    fn find_pop(&mut self) {
        let Some(f) = self.find.as_mut() else { return };
        f.query.pop();
        self.find_refresh_and_jump();
    }

    /// Enter/F3: step to the next (Shift: previous) match and show it.
    fn find_step(&mut self, back: bool, _host: &mut dyn Host) {
        let Some(f) = self.find.as_mut() else { return };
        f.step(back);
        self.find_jump();
    }

    /// Typing in the query: recompute matches from the caret and show one.
    /// The state is taken out and put back so `refresh` can see the buffer:
    /// both reach through the tab `Deref`, which the borrow checker reads
    /// as a whole-`App` borrow either way.
    fn find_refresh_and_jump(&mut self) {
        let caret = self.editor.caret;
        if let Some(mut f) = self.find.take() {
            f.refresh(&self.buffer, caret);
            self.find = Some(f);
        } else {
            return;
        }
        self.find_jump();
    }

    /// Show the current find match: the editor selection in Edit/Split (with
    /// the caret following it). Preview has no buffer-offset geometry, so
    /// there the view scrolls to the row containing the current occurrence.
    fn find_jump(&mut self) {
        if self.mode == Mode::Preview {
            if let Some(n) = self
                .find
                .as_ref()
                .filter(|f| !f.query.is_empty())
                .map(|f| f.current)
            {
                self.find_scroll_preview(n);
            }
            return;
        }
        let Some((a, b)) = self.find.as_ref().and_then(|f| f.current_match()) else {
            return;
        };
        self.editor.anchor = Some(a);
        self.editor.caret = b;
        self.follow_caret();
    }

    /// Preview's best effort at "go to the current match": scroll to the row
    /// holding that occurrence of the query, counting occurrences across
    /// rows in order. Rendered rows can differ from the source (Markdown), so
    /// a query the preview cannot show just leaves the view where it is.
    fn find_scroll_preview(&mut self, occurrence: usize) {
        let needle = match &self.find {
            Some(f) if !f.query.is_empty() => f.query.to_lowercase(),
            _ => return,
        };
        let mut want = occurrence;
        let mut target = None;
        // One scratch buffer for every row: the jump runs per keystroke, so
        // per-row lowercase allocation is the difference between churning
        // the heap for every row of a big preview and not.
        let mut lower = String::new();
        for row in &self.preview_rows {
            lower.clear();
            lower.extend(row.text.chars().flat_map(char::to_lowercase));
            let mut at = 0;
            let mut hits = 0;
            while let Some(i) = lower[at..].find(&needle) {
                hits += 1;
                at += i + needle.len();
            }
            if hits == 0 {
                continue;
            }
            if want < hits {
                target = Some(row.top);
                break;
            }
            want -= hits;
        }
        if let Some(top) = target {
            let tab = &mut self.tabs[self.active];
            tab.scroll.offset = (top - 32.0 * self.scale).max(0.0);
            tab.scroll.scroll_by(0.0);
        }
    }

    /// The find bar's rect right now (pure `find::bar_rect`), or a rect no
    /// point can hit while the bar is closed.
    fn find_bar_rect(&self) -> Rect {
        match &self.find {
            Some(f) => {
                let status = f.status();
                find::bar_rect(&self.fonts, self.viewport(), self.scale, &f.query, &status)
            }
            None => Rect::new(0.0, 0.0, 0.0, 0.0),
        }
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
                self.collapsed_gen += 1;
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

    /// Save the buffer to disk (`Ctrl+S`, the tab's save button, or the
    /// context menu). A buffer with no path yet — an Untitled tab — opens the
    /// save-as dialog first, defaulting to the OS Documents folder, and adopts
    /// whatever path the user picks.
    fn save(&mut self, host: &mut dyn Host) {
        let path = match self.path.clone() {
            Some(p) => p,
            None => {
                let default = format!("Untitled.{}", self.kind.default_ext());
                let Some(p) = host.save_file_dialog(&default) else {
                    self.status_extra = "save cancelled".into();
                    host.request_redraw();
                    return;
                };
                // The tab becomes that file: name, kind and the file watcher's
                // baseline all move over to the chosen path.
                self.tabs[self.active].path = Some(p.clone());
                self.tabs[self.active].kind = FileKind::from_path(&p);
                self.mtime = None;
                self.layout_dirty = true;
                // A brand-new file is as recent as an opened one.
                settings::note_recent(&mut self.recent, &p);
                self.persist();
                p
            }
        };
        let text = self.buffer.text();
        match std::fs::write(&path, &text) {
            Ok(()) => {
                self.saved_text = text;
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
                self.saved_text = self.buffer.text();
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
        // The pane shows content inset by the gutter rail: the caret's
        // screen x is rail + doc_x - h_scroll, so the visible content span
        // is [0, viewport_w - rail] and the pan math runs in content space.
        let rail = gutter::rail_w(&self.fonts, &ctx, self.buffer.line_count());
        let vis_w = (viewport_w - rail).max(1.0);
        let line_h = self.editor.line_height(viewport_w, &ctx);
        let view_h = self.editor_area().h;
        // Keep the caret visible in editor space, then carry the shared
        // ratio along: identity outside Split.
        let want = self
            .editor
            .ensure_visible(doc_y, line_h, view_h, self.editor_scroll());
        let ed_max = self.editor_max();
        self.set_scroll_ratio(if ed_max > 0.0 { want / ed_max } else { 0.0 });
        let margin = 32.0 * self.scale;
        // Caret tracking is pointer-like: it sets the position outright,
        // ending any wheel glide rather than fighting it.
        self.set_h_scroll(if doc_x < self.h_scroll + margin {
            (doc_x - margin).max(0.0)
        } else if doc_x > self.h_scroll + vis_w - margin {
            doc_x - vis_w + margin
        } else {
            self.h_scroll
        });
    }

    /// Paint selection + caret over an editor pane (document space -> pane).
    fn paint_editor_overlay(&self, canvas: &mut Canvas, area: Rect, scroll: f32) {
        // Line-number gutter first (rail + current-row tint), then the
        // selection/caret overlay above it. The gutter clips itself to
        // `area` and ignores horizontal scroll: numbers stay put while long
        // lines slide under the rail.
        let ctx = self.ctx();
        let rail = gutter::paint_with(
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
            area.w + self.h_scroll + rail,
            &ctx,
            self.caret_visible(),
        );
        // Find-bar matches tint the pane below the selection/caret: the
        // current match is the editor selection, so it is skipped here.
        if let Some(f) = &self.find {
            let tint = Color::rgba(
                self.theme.accent.r,
                self.theme.accent.g,
                self.theme.accent.b,
                56,
            );
            let line_h = self.editor.line_height(area.w + self.h_scroll + rail, &ctx);
            // The editor pane never wraps, so line i sits at pad + i*line_h:
            // only the matches whose rows can be on screen pay for rects,
            // and one `match_rects` pass measures them all.
            let first = ((scroll / line_h) as usize).saturating_sub(1);
            let last = (((scroll + area.h) / line_h) as usize).saturating_add(1);
            let ranges: Vec<(Pos, Pos)> = f
                .matches
                .iter()
                .enumerate()
                .filter(|&(i, &(from, _))| {
                    from.line >= first && from.line <= last && i != f.current
                })
                .map(|(_, &(from, to))| (from, to))
                .collect();
            if !ranges.is_empty() {
                canvas.set_clip(Some(area));
                for rect in layout::text::match_rects(
                    &self.buffer,
                    &ranges,
                    area.w + self.h_scroll + rail,
                    &ctx,
                ) {
                    canvas.fill_rect(
                        Rect::new(
                            rect.x + area.x - self.h_scroll + rail,
                            rect.y + area.y - scroll,
                            rect.w,
                            rect.h,
                        ),
                        tint,
                    );
                }
                canvas.set_clip(None);
            }
        }
        if ops.is_empty() {
            return;
        }
        canvas.set_clip(Some(area));
        // Content is inset by the rail (PaneScroll::content_x), so the
        // selection/caret overlay shifts with it.
        let (ox, oy) = (area.x - self.h_scroll + rail, area.y - scroll);
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
        let records = jsonl::parse_parallel(&buf.text());
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

    /// DPI scale + bar-safe window-edge grip for pane geometry and scrollbar
    /// hit tests.
    fn metrics(&self) -> PaneMetrics {
        PaneMetrics::new(self.scale, self.bar_grip())
    }

    /// The strip the overlay scrollbars keep clear of: the platform's band
    /// capped by a share of a bar's own thickness ([`preview_view::bar_grip`]).
    /// Derived, so a wider platform band can never grow into the bars.
    fn bar_grip(&self) -> f32 {
        preview_view::bar_grip(self.scale, self.edge)
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
        // A virtualized JSONL tab paints a band of records laid out fresh
        // each frame (only records at the scroll offset's viewport exist in
        // ops), so the PreviewView sees a normal display list.
        let virtual_jsonl = self.kind == FileKind::JsonLines
            && self.buffer.line_count() > VIRTUAL_JSONL_LINES
            && self.mode != Mode::Edit;
        let view = self.viewport();
        if virtual_jsonl {
            let top = self.scroll.offset;
            let bottom = self.scroll.offset + view.h;
            let ctx = self.ctx();
            let band = layout::json_tree::layout_band(layout::json_tree::Band {
                plan: &self.plan,
                records: &self.records,
                buf: &self.buffer,
                collapsed: &self.collapsed,
                width: self.size.0 as f32,
                ctx: &ctx,
                top,
                bottom,
            });
            // Paint the band like the preview does: reuse PreviewView by
            // handing it the fresh list for this frame. Hits are from the
            // band only — matches what's clickable on screen.
            let painted = PreviewView { list: band };
            let scroll = self.scroll.offset;
            painted.paint(
                canvas,
                &self.fonts,
                &self.theme,
                view,
                PaneScroll::vertical(scroll),
                self.metrics(),
            );
            return;
        }
        match self.mode {
            Mode::Preview => {
                let scroll = self.preview_scroll();
                self.preview.paint(
                    canvas,
                    &self.fonts,
                    &self.theme,
                    view,
                    PaneScroll::vertical(scroll),
                    self.metrics(),
                );
                self.paint_preview_selection(canvas, view, scroll);
            }
            Mode::Edit => {
                let rail = gutter::rail_w(&self.fonts, &self.ctx(), self.buffer.line_count());
                let scroll = self.editor_scroll();
                self.editor_pane.paint(
                    canvas,
                    &self.fonts,
                    &self.theme,
                    view,
                    PaneScroll::editor(scroll, self.h_scroll, self.h_max, rail),
                    self.metrics(),
                );
                self.paint_editor_overlay(canvas, view, scroll);
            }
            Mode::Split => {
                let (left, right) = self.split_panes();
                let rail = gutter::rail_w(&self.fonts, &self.ctx(), self.buffer.line_count());
                let preview = self.preview_scroll();
                let editor = self.editor_scroll();
                self.preview.paint(
                    canvas,
                    &self.fonts,
                    &self.theme,
                    left,
                    PaneScroll::vertical(preview),
                    self.metrics(),
                );
                self.paint_preview_selection(canvas, left, preview);
                self.editor_pane.paint(
                    canvas,
                    &self.fonts,
                    &self.theme,
                    right,
                    PaneScroll::editor(editor, self.h_scroll, self.h_max, rail),
                    self.metrics(),
                );
                self.paint_editor_overlay(canvas, right, editor);
                // The seam between the panes.
                canvas.fill_rect(self.split_divider(), self.theme.rule);
            }
        }
    }

    /// Status-bar buttons. Ids: 0=Preview, 1=Edit, 2=Split, 3=Help, 4=Theme,
    /// 5=Open, 6=About, 7=Find. The vec is in left-to-right order (paint
    /// walks it in reverse), giving `Find Open | Preview Edit Split | Help
    /// About | Dark/Light`. Rects are filled in by `statusbar::paint`.
    fn status_buttons(&self) -> Vec<statusbar::StatusButton> {
        let active = match self.mode {
            Mode::Preview => 0,
            Mode::Edit => 1,
            Mode::Split => 2,
        };
        let rows: [(u8, &'static str, bool, bool); 8] = [
            // id, label, active, starts-a-group
            (7, "Find", self.find.is_some(), false),
            (5, "Open", false, false),
            (0, "Preview", active == 0, true),
            (1, "Edit", active == 1, false),
            (2, "Split", active == 2, false),
            (3, "Help", self.help_open, true),
            (6, "About", self.about_open, false),
            // Names the theme a click switches to.
            (4, if self.dark { "Light" } else { "Dark" }, false, true),
        ];
        rows.iter()
            .map(
                |&(id, label, active, group_start)| statusbar::StatusButton {
                    id,
                    label,
                    active,
                    group_start,
                    rect: Rect::default(),
                },
            )
            .collect()
    }

    /// The status bar's Copy button: present only while there is a
    /// selection to copy (editor selection in Edit/Split, preview rows in
    /// Preview). Rect is filled in by `statusbar::paint`.
    fn copy_button(&self) -> Option<statusbar::StatusButton> {
        let selected = self.preview_selection_text().is_some() || self.editor.selection().is_some();
        selected.then(|| statusbar::StatusButton {
            id: 8,
            label: "Copy",
            active: false,
            group_start: false,
            rect: Rect::default(),
        })
    }

    fn status_bar_text(&self) -> (String, String) {
        // The tab already names the file, so the bar leaves it out: caret
        // position (and selection) in Edit, otherwise nothing on the left.
        let left = if self.mode == Mode::Edit {
            let sel = if self.editor.selection().is_some() {
                " [sel]"
            } else {
                ""
            };
            format!(
                "Ln {}, Col {}{sel}",
                self.editor.caret.line + 1,
                char_col(&self.buffer, self.editor.caret),
            )
        } else {
            String::new()
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

/// Whole-document byte equality shaped like a line walk instead of a join.
/// [`Buffer::text`] assembles a second full copy of the document; against a
/// huge buffer that is O(n) bytes *and* an O(n) String allocation per
/// keystroke, and the incremental shading that pushes caller care about
/// compares whole documents against the saved snapshot. Comparing line by
/// line, with the lengths checked up front, allocates nothing and short-
/// circuits on the first mismatched line.
fn same_lines(buf: &Buffer, saved: &str) -> bool {
    // Line counts must agree: a saved snapshot of "a\nb" is 2 lines and a saved
    // "a\n" is 2 lines too (the trailing newline is one empty last line), and
    // both saved-\r\n shapes normalize to the same \n-joined shape at load.
    let expected = saved.split('\n').count();
    if buf.line_count() != expected {
        return false;
    }
    let mut it = saved.split('\n').enumerate();
    for i in 0..buf.line_count() {
        let Some((_, text)) = it.next() else {
            return false;
        };
        if buf.line(i) != text.trim_end_matches('\r') {
            return false;
        }
    }
    true
}

/// Log status counts over at most the first 20k lines: a full 64 MB scan on
/// every relayout would jank the window for no reader benefit. Empty when
/// nothing matched, so ordinary logs keep a clean bar.
fn log_counts(buf: &Buffer) -> String {
    let mut errors = 0u32;
    let mut warns = 0u32;
    for i in 0..buf.line_count().min(20_000) {
        let line = buf.line(i);
        let mut words = line.split_whitespace();
        let Some(first) = words.next() else { continue };
        // Skip a leading timestamp/bracket prefix the way the tint does.
        let word = first.trim_matches(|c: char| !c.is_alphanumeric());
        let level = words
            .next()
            .filter(|_| {
                word.chars()
                    .all(|c| c.is_ascii_digit() || "-:,./TZ[]".contains(c))
                    && word.len() >= 4
            })
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_uppercase()
            })
            .unwrap_or_else(|| word.to_uppercase());
        if [
            "ERROR", "ERR", "FATAL", "CRIT", "CRITICAL", "FAIL", "FAILED",
        ]
        .contains(&level.as_str())
        {
            errors += 1;
        } else if ["WARN", "WARNING"].contains(&level.as_str()) {
            warns += 1;
        }
    }
    match (errors, warns) {
        (0, 0) => String::new(),
        (e, 0) => format!("{e} error{}", if e == 1 { "" } else { "s" }),
        (0, w) => format!("{w} warn{}", if w == 1 { "" } else { "s" }),
        (e, w) => format!("{e} errors · {w} warns"),
    }
}

impl Handler for App {
    fn on_event(&mut self, ev: Event, host: &mut dyn Host) {
        match ev {
            Event::CloseRequested => self.quit(host),
            Event::Resized {
                width,
                height,
                scale,
            } => {
                self.size = (width, height);
                self.scale = scale;
                // The grip is a logical width; make it physical for this DPI. A
                // DPI change arrives as a resize, so it is re-read here rather
                // than only once at startup.
                self.edge = platform::edge_grip() * scale;
                self.layout_dirty = true;
                host.request_redraw();
            }
            Event::Scroll { dx, dy } => {
                self.pan_horizontal(dx, host);
                self.scroll_input(-dy, host);
            }
            Event::Pan { dx, dy } => {
                // A double-tap's finger drag extends the selection the tap
                // made (the touch counterpart of double-click-and-drag);
                // every other finger is direct manipulation: follow it
                // exactly, no glide — and drop any glide that was in flight
                // (its remaining distance belongs to the wheel, not to the
                // finger).
                if self.on_touch_pan(dx, dy, host) {
                    return;
                }
                self.pan_horizontal_direct(dx);
                self.scroll_direct(-dy, host);
            }
            Event::PanEnd { vx, vy } => {
                // A selection drag is not a flick: it retires the gesture
                // without a coast.
                if self.touch_sel.take().is_some() {
                    host.request_redraw();
                    return;
                }
                self.coast(vx, vy, host);
            }
            Event::KeyDown { key, mods } => self.on_key(key, mods, host),
            Event::Frame => self.on_frame(host),
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
            Event::WindowGeometry(g) => {
                // Only a *change* is worth writing, and each change restarts
                // the wait: a platform that reports the same rect over and over
                // must not be able to push the write out indefinitely, and a
                // drag must still collapse to its last rect.
                let changed = self.window_maximized != g.maximized
                    || (!g.maximized && self.window != Some(g));
                self.window_maximized = g.maximized;
                // Only the normal rect is worth keeping: a maximized window
                // reports the work area, and a restore that reopened at those
                // dimensions would hand back a screen-sized window the moment
                // it was un-maximized. No redraw either — the platform sends
                // `Resized` for the canvas, and an OS-driven move repaints on
                // its own.
                if !g.maximized {
                    self.window = Some(g);
                }
                if changed {
                    self.geometry_dirty = true;
                    self.geometry_idle = GEOMETRY_SAVE_TICKS;
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
        let mut copy = self.copy_button();
        statusbar::paint(
            canvas,
            &self.fonts,
            &self.theme,
            sb,
            (&left, &right),
            &mut buttons,
            &mut copy,
        );
        self.status_buttons = buttons;
        self.copy_btn = copy;
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
        if let Some(f) = &self.find {
            find::paint(
                canvas,
                &self.fonts,
                &self.theme,
                view,
                self.scale,
                f,
                self.caret_visible(),
            );
        }
        if self.help_open {
            let _ = help::paint(
                canvas,
                &self.fonts,
                &self.theme,
                canvas.width as f32,
                canvas.height as f32,
                self.scale,
            );
        }
        if self.about_open {
            let (_f, toggle) = about::paint(
                canvas,
                &self.fonts,
                &self.theme,
                canvas.width as f32,
                canvas.height as f32,
                self.scale,
                self.auto_update,
            );
            self.about_toggle = Some(toggle);
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

    /// Self-tuning resize strip: the platform's full band (`band`) everywhere,
    /// except over a scrollbar, where only the bar-safe share is claimed so a
    /// press on the bar can never start a window resize. Kept per-point rather
    /// than per-window because the bars are the only thing at the edge that
    /// needs the pixels — everywhere else the edge stays as easy to grab as
    /// the platform allows.
    fn grip_at(&self, x: f32, y: f32, band: f32) -> f32 {
        if self.sb_at(x, y).is_some() {
            preview_view::bar_grip(self.scale, band)
        } else {
            band
        }
    }

    /// Reopen where the last session was left. The geometry is a request: the
    /// backend checks it against the screens it has (see
    /// `platform::title_reachable`) and falls back to its own placement when the
    /// remembered monitor is gone.
    fn initial_window(&self) -> Option<WindowGeometry> {
        self.window.map(|g| WindowGeometry {
            maximized: self.window_maximized,
            ..g
        })
    }

    fn cursor_at(&self, x: f32, y: f32) -> CursorShape {
        if self.menu.is_some() || self.help_open || self.about_open {
            return CursorShape::Arrow;
        }
        // The find bar is chrome, not text (the query has no click-to-place)
        // — except the ×, which is a button.
        if self.find.is_some() && self.find_bar_rect().contains(Point { x, y }) {
            let bar = self.find_bar_rect();
            return if find::close_rect(bar, self.scale).contains(Point { x, y }) {
                CursorShape::Hand
            } else {
                CursorShape::Arrow
            };
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
            return if self.status_buttons.iter().any(|b| b.rect.contains(p))
                || self.copy_btn.is_some_and(|b| b.rect.contains(p))
            {
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
                    PaneScroll::vertical(self.preview_scroll()),
                    self.metrics(),
                    p,
                ) {
                    return CursorShape::Arrow;
                }
            }
            Mode::Edit => {
                let rail = gutter::rail_w(&self.fonts, &self.ctx(), self.buffer.line_count());
                if self.editor_pane.scrollbar_hit(
                    self.viewport(),
                    PaneScroll::editor(self.editor_scroll(), self.h_scroll, self.h_max, rail),
                    self.metrics(),
                    p,
                ) {
                    return CursorShape::Arrow;
                }
            }
            Mode::Split => {
                let (left, right) = self.split_panes();
                let rail = gutter::rail_w(&self.fonts, &self.ctx(), self.buffer.line_count());
                if self.preview.scrollbar_hit(
                    left,
                    PaneScroll::vertical(self.preview_scroll()),
                    self.metrics(),
                    p,
                ) || self.editor_pane.scrollbar_hit(
                    right,
                    PaneScroll::editor(self.editor_scroll(), self.h_scroll, self.h_max, rail),
                    self.metrics(),
                    p,
                ) {
                    return CursorShape::Arrow;
                }
            }
        }
        if self.over_gutter(x, y) {
            return CursorShape::Arrow;
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
    fn recent_files_track_opens_and_the_menu_reopens_them() {
        let a = std::env::temp_dir().join("editor_test_recent_a.md");
        std::fs::write(&a, "a\n").unwrap();
        let b = std::env::temp_dir().join("editor_test_recent_b.md");
        std::fs::write(&b, "b\n").unwrap();
        let mut app = App::new(None).unwrap();
        let mut host = NullHost::default();
        // Opening notes the file; re-opening moves it back to the front.
        app.open_path(b.clone(), &mut host);
        assert_eq!(app.settings().recent, vec![b.clone()]);
        app.open_path(a.clone(), &mut host);
        assert_eq!(app.settings().recent, vec![a.clone(), b.clone()]);
        // The context menu lists them under a caption that does not click.
        let menu = menu::open(
            &app.fonts,
            10.0,
            10.0,
            false,
            &app.recent,
            Rect::new(0.0, 0.0, 800.0, 600.0),
            1.0,
        );
        assert_eq!(menu.items[6].0, menu::Action::Header);
        assert_eq!(menu.items[7].0, menu::Action::Recent(0));
        assert_eq!(menu.items[7].1, "editor_test_recent_a.md");
        // Picking the second recent focuses that file's tab…
        app.run_menu_action(menu::Action::Recent(1), &mut host);
        assert_eq!(app.path.as_deref(), Some(b.as_path()));
        // …and bumps it back to the front of the list.
        assert_eq!(app.settings().recent, vec![b.clone(), a.clone()]);
    }

    #[test]
    fn oversized_files_are_refused_with_a_message() {
        // The rule itself: anything at or under the limit opens, anything
        // over it names the size and the limit.
        assert_eq!(size_error(0), None);
        assert_eq!(size_error(MAX_OPEN_BYTES), None);
        let msg = size_error(MAX_OPEN_BYTES + 1).expect("over the limit refuses");
        assert!(msg.contains("64 MB"), "names the limit: {msg}");
        assert!(msg.contains("larger than"), "says which way: {msg}");
    }

    /// `Event::KeyDown` for a plain (unmodified) key.
    fn plain(key: Key) -> Event {
        Event::KeyDown {
            key,
            mods: Modifiers::default(),
        }
    }

    /// `Event::KeyDown` for a key with only Shift held.
    fn shifted(key: Key) -> Event {
        Event::KeyDown {
            key,
            mods: Modifiers {
                ctrl: false,
                alt: false,
                shift: true,
            },
        }
    }

    #[test]
    fn find_bar_opens_types_and_selects_the_first_match() {
        let mut app = App::new(None).unwrap();
        app.buffer = Buffer::from_text("one two\nthree one\none");
        let mut host = NullHost::default();
        app.on_event(ctrl('f'), &mut host);
        assert!(app.find.is_some(), "Ctrl+F opens the bar");
        // Typing goes to the query, never the buffer.
        app.on_event(Event::Text("one".into()), &mut host);
        assert_eq!(app.buffer.text(), "one two\nthree one\none");
        assert!(!app.buffer.dirty);
        let f = app.find.as_ref().unwrap();
        assert_eq!(f.matches.len(), 3);
        assert_eq!(f.status(), "1/3");
        // The current match is the editor selection.
        assert_eq!(app.editor.selection(), Some(f.matches[0]));
        // Ctrl+F again keeps the query; Escape closes.
        app.on_event(ctrl('f'), &mut host);
        assert_eq!(app.find.as_ref().unwrap().query, "one");
        app.on_event(plain(Key::Escape), &mut host);
        assert!(app.find.is_none());
    }

    #[test]
    fn find_bar_enter_and_f3_step_with_wrap() {
        let mut app = App::new(None).unwrap();
        app.buffer = Buffer::from_text("a a\nb\na");
        let mut host = NullHost::default();
        app.on_event(ctrl('f'), &mut host);
        app.on_event(Event::Text("a".into()), &mut host);
        app.on_event(plain(Key::Enter), &mut host);
        assert_eq!(app.find.as_ref().unwrap().status(), "2/3");
        app.on_event(plain(Key::Enter), &mut host);
        app.on_event(plain(Key::Enter), &mut host);
        assert_eq!(app.find.as_ref().unwrap().status(), "1/3", "wraps forward");
        app.on_event(shifted(Key::Enter), &mut host);
        assert_eq!(app.find.as_ref().unwrap().status(), "3/3", "wraps backward");
        app.on_event(plain(Key::F(3)), &mut host);
        assert_eq!(app.find.as_ref().unwrap().status(), "1/3", "F3 steps too");
    }

    #[test]
    fn find_bar_reports_zero_and_backspace_edits_the_query() {
        let mut app = App::new(None).unwrap();
        app.buffer = Buffer::from_text("one two");
        let mut host = NullHost::default();
        app.on_event(ctrl('f'), &mut host);
        app.on_event(Event::Text("onex".into()), &mut host);
        assert_eq!(app.find.as_ref().unwrap().status(), "0 matches");
        app.on_event(plain(Key::Backspace), &mut host);
        let f = app.find.as_ref().unwrap();
        assert_eq!(f.query, "one");
        assert_eq!(f.status(), "1/1");
        assert_eq!(app.buffer.text(), "one two", "buffer untouched");
    }

    #[test]
    fn find_bar_search_is_case_insensitive() {
        let mut app = App::new(None).unwrap();
        app.buffer = Buffer::from_text("Alpha ALPHA alPHA");
        let mut host = NullHost::default();
        app.on_event(ctrl('f'), &mut host);
        app.on_event(Event::Text("alpha".into()), &mut host);
        assert_eq!(app.find.as_ref().unwrap().matches.len(), 3);
    }

    #[test]
    fn find_bar_in_preview_counts_and_jumps_to_the_row() {
        let text = format!("{}the needle\n", "filler row of text\n".repeat(80));
        let mut app = open("editor_test_findprev.txt", &text);
        let _ = app.render_headless(400, 300, 1.0);
        let mut host = NullHost::default();
        app.on_event(ctrl('f'), &mut host);
        app.on_event(Event::Text("needle".into()), &mut host);
        let f = app.find.as_ref().unwrap();
        assert_eq!(f.matches.len(), 1);
        assert_eq!(f.status(), "1/1");
        assert!(
            app.scroll.offset > 0.0,
            "preview should scroll to the match row, offset {}",
            app.scroll.offset
        );
        // Escape closes the bar there too.
        app.on_event(plain(Key::Escape), &mut host);
        assert!(app.find.is_none());
    }

    /// The middle of a rect, for mouse-event tests.
    fn mid(r: Rect) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    #[test]
    fn typing_in_the_find_bar_keeps_the_current_match_and_view_still() {
        // The runtime probe caught the view jumping while a query was typed
        // quickly: `Find::land` compared match STARTS against the caret,
        // which `find_jump` puts at the current match's END, so every
        // keystroke skipped ahead one match — and once that landed below the
        // fold, `follow_caret` scrolled. One Text event per character is
        // exactly what WM_CHAR delivers.
        let mut lines = vec!["find probe document".to_string()];
        for i in 1..=300u32 {
            if i % 10 == 5 {
                lines.push(format!("alpha marker {i}"));
            } else {
                lines.push(format!("line {i} bravo charlie delta echo"));
            }
        }
        let mut app = open("editor_test_findfast.txt", &lines.join("\n"));
        let _ = app.render_headless(1100, 800, 1.0);
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        let _ = app.render_headless(1100, 800, 1.0);
        app.on_event(ctrl('f'), &mut host);
        for ch in "alpha".chars() {
            app.on_event(Event::Text(ch.to_string()), &mut host);
            assert_eq!(
                app.find.as_ref().unwrap().current,
                0,
                "typing '{ch}' must keep the current match"
            );
        }
        assert_eq!(app.editor.caret.line, 5, "the selection stays on the match");
        assert_eq!(
            app.scroll.offset, 0.0,
            "typing in the find bar must not scroll the editor"
        );
    }

    #[test]
    fn find_button_and_bar_close_button_toggle_the_bar() {
        let mut app = App::new(None).unwrap();
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        let (fx, fy) = mid(app
            .status_buttons
            .iter()
            .find(|b| b.id == 7)
            .expect("Find")
            .rect);
        app.on_event(
            Event::MouseDown {
                x: fx,
                y: fy,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.find.is_some(), "the Find button opens the bar");
        // The × on the bar's right edge closes it.
        let bar = app.find_bar_rect();
        let (cx, cy) = mid(find::close_rect(bar, 1.0));
        app.on_event(
            Event::MouseDown {
                x: cx,
                y: cy,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.find.is_none(), "the × closes the bar");
        // The Find button toggles both ways.
        app.on_event(
            Event::MouseDown {
                x: fx,
                y: fy,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.find.is_some());
        app.on_event(
            Event::MouseDown {
                x: fx,
                y: fy,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.find.is_none());
    }

    #[test]
    fn double_tap_drag_extends_the_selection_like_double_click() {
        let mut app = App::new(None).unwrap();
        app.buffer = Buffer::from_text("hello world\nsecond line here\n");
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        let down = Event::MouseDown {
            x: 100.0,
            y: 60.0,
            button: MouseButton::Left,
        };
        let up = Event::MouseUp {
            x: 100.0,
            y: 60.0,
            button: MouseButton::Left,
        };
        // Tap, tap: the second press is a double-tap and selects the word.
        app.on_event(down.clone(), &mut host);
        app.on_event(up.clone(), &mut host);
        app.on_event(down.clone(), &mut host);
        app.on_event(up.clone(), &mut host);
        assert!(
            !app.editor.sel_text(&app.buffer).is_empty(),
            "double-tap selects a word"
        );
        // The finger drags: Pan extends the selection instead of scrolling.
        let before = app.scroll.offset;
        app.on_event(Event::Pan { dx: 0.0, dy: 120.0 }, &mut host);
        let sel = app.editor.selection().expect("selection survives the drag");
        assert!(sel.1.line >= 1, "drag extended onto a later line");
        assert_eq!(
            app.scroll.offset, before,
            "a selection drag does not scroll"
        );
        // PanEnd retires the gesture without a coast.
        app.on_event(Event::PanEnd { vx: 0.0, vy: -3.0 }, &mut host);
        assert!(app.touch_sel.is_none());
        assert_eq!(app.scroll.offset, before, "no coast after a selection drag");
    }

    #[test]
    fn copy_button_appears_with_a_selection_and_copies() {
        let mut app = App::new(None).unwrap();
        app.buffer = Buffer::from_text("hello world\n");
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        assert!(app.copy_btn.is_none(), "no selection, no button");
        app.on_event(ctrl('a'), &mut host);
        let _ = app.render_headless(800, 600, 1.0);
        let cb = app.copy_btn.expect("Copy button with a selection").rect;
        assert!(cb.x < 60.0, "far left of the bar, at {}", cb.x);
        assert_eq!(cb.h, statusbar::HEIGHT, "full bar height for a finger");
        let (cx, cy) = mid(cb);
        app.on_event(
            Event::MouseDown {
                x: cx,
                y: cy,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert_eq!(host.clipboard.as_deref(), Some("hello world\n"));
    }

    #[test]
    fn headless_render_draws_text_and_statusbar() {
        let mut app = open("editor_test_a.txt", "Hello Editor\nsecond line");
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
        // Zooming in Edit mode defers the wrapped list (it is not painted
        // there); the pane-switch button relayouts before the pane shows it.
        app.mode = Mode::Preview;
        app.layout_dirty = true;
        let mut c = Canvas::new(800, 600);
        app.paint(&mut c);
        assert!(app.preview.list.height > h1);
    }

    #[test]
    fn json_renders_tinted_tree() {
        use crate::layout::display_list::DrawOp;
        let mut app = open("editor_test_b.json", "{\"name\": \"editor\", \"n\": 42}");
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
        let mut app = open("editor_test_c.json", "{\"a\": }");
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
            "editor_test_d.json",
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
        // An Untitled buffer opens straight in Edit mode.
        assert_eq!(app.mode, Mode::Edit);
        app.on_event(ctrl('e'), &mut host);
        assert_eq!(app.mode, Mode::Split);
        app.on_event(ctrl('e'), &mut host);
        assert_eq!(app.mode, Mode::Preview);
        app.on_event(ctrl('e'), &mut host);
        assert_eq!(app.mode, Mode::Edit);
        // Edit/Split panes lay out without panicking.
        app.on_event(ctrl('e'), &mut host);
        let _ = app.render_headless(800, 600, 1.0);
    }

    #[test]
    fn untitled_tabs_switch_to_edit_mode() {
        // Closing the last tab leaves a fresh Untitled, which drops into Edit.
        let mut app = open("editor_test_untitled.txt", "hello");
        let mut host = NullHost::default();
        assert_eq!(app.mode, Mode::Preview);
        app.on_event(ctrl('w'), &mut host);
        assert!(app.path.is_none(), "last tab is a fresh Untitled");
        assert_eq!(app.mode, Mode::Edit);
        // Give it content so the next open stacks a tab instead of replacing it.
        app.on_event(Event::Text("draft".into()), &mut host);
        app.mode = Mode::Preview; // as if the user flipped to Preview
        let out = std::env::temp_dir().join("editor_test_untitled2.txt");
        std::fs::write(&out, "x").unwrap();
        app.open_path(out.clone(), &mut host);
        assert_eq!(app.path.as_ref(), Some(&out));
        assert_eq!(app.mode, Mode::Preview, "a file tab keeps the mode");
        // Cycling back onto the Untitled tab forces Edit again.
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
        assert!(app.path.is_none(), "cycled back to the Untitled tab");
        assert_eq!(app.mode, Mode::Edit);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn markdown_renders_document() {
        let mut app = open(
            "editor_test_e.md",
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
    fn new_kinds_render_with_status() {
        // CSV: table + row/col counts.
        let mut app = open("editor_test_kinds.csv", "a,b\n1,2\n3,4\n");
        assert_eq!(app.kind, FileKind::Csv);
        let c = app.render_headless(800, 600, 1.0);
        assert!(app.status_extra.contains("2 rows"), "{}", app.status_extra);
        let bg = app.theme.bg.pack();
        assert!(c.pixels.iter().filter(|&&p| p != bg).count() > 200);
        // Configs: tinted preview, clean bar.
        for (name, content, kind) in [
            ("editor_test_kinds.yaml", "port: 8080\n", FileKind::Yaml),
            ("editor_test_kinds.toml", "port = 8080\n", FileKind::Toml),
            ("editor_test_kinds.ini", "[s]\nport = 8080\n", FileKind::Ini),
            ("editor_test_kinds.rs", "fn main() {}\n", FileKind::Code),
        ] {
            let mut app = open(name, content);
            assert_eq!(app.kind, kind);
            let c = app.render_headless(800, 600, 1.0);
            assert!(
                c.pixels.iter().filter(|&&p| p != bg).count() > 50,
                "{name} blank"
            );
        }
        // Logs: level counts in the bar.
        let mut app = open("editor_test_kinds.log", "INFO up\nERROR boom\nWARN slow\n");
        assert_eq!(app.kind, FileKind::Log);
        let _ = app.render_headless(800, 600, 1.0);
        assert!(app.status_extra.contains("1 error"), "{}", app.status_extra);
    }

    #[test]
    fn clicking_link_shows_href_in_status() {
        use crate::layout::display_list::HitAction;
        let mut app = open(
            "editor_test_f.md",
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
        let mut app = open("editor_test_g.txt", "hello");
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
        let mut app = open("editor_test_h.txt", "hello");
        let mut host = NullHost::default();
        app.on_event(Event::Text("X".into()), &mut host);
        assert_eq!(app.buffer.text(), "hello");
        assert!(!app.buffer.dirty);
    }

    #[test]
    fn enter_and_backspace_split_join_lines() {
        let mut app = open("editor_test_i.txt", "one two");
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
        let mut app = open("editor_test_j.txt", "hello world");
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
        let mut app = open("editor_test_k.txt", "hello");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        app.on_event(Event::Text("!".into()), &mut host);
        // The dirty marker lives on the tab's save button now, not the title.
        assert!(app.buffer.dirty);
        assert_eq!(app.title(), "editor_test_k.txt — Editor");
        assert_eq!(app.buffer.text(), "!hello");
        app.on_event(ctrl('s'), &mut host);
        assert!(!app.buffer.dirty);
        assert_eq!(app.title(), "editor_test_k.txt — Editor");
        assert_eq!(
            std::fs::read_to_string(app.path.as_ref().unwrap()).unwrap(),
            "!hello"
        );
    }

    #[test]
    fn save_as_writes_an_untitled_buffer() {
        // An Untitled buffer has no path: Ctrl+S must ask the host where to
        // put it, then adopt that path for the tab.
        let mut app = App::new(None).unwrap();
        assert!(app.path.is_none());
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        app.on_event(Event::Text("draft".into()), &mut host);
        assert!(app.buffer.dirty);
        // Cancelling changes nothing: still Untitled, still dirty.
        app.on_event(ctrl('s'), &mut host);
        assert!(app.path.is_none());
        assert!(app.buffer.dirty);
        assert_eq!(app.status_extra, "save cancelled");
        // Choosing a path writes the file and re-homes the tab.
        let out = std::env::temp_dir().join("editor_test_saveas.md");
        let _ = std::fs::remove_file(&out);
        host.save_file = Some(out.clone());
        app.on_event(ctrl('s'), &mut host);
        assert_eq!(app.path.as_ref(), Some(&out));
        assert_eq!(app.kind, FileKind::Markdown, "kind follows the suffix");
        assert!(!app.buffer.dirty);
        assert_eq!(app.title(), "editor_test_saveas.md — Editor");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "draft");
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn tab_save_button_saves_the_tab() {
        let mut app = open("editor_test_savebtn.txt", "hello");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        app.on_event(Event::Text("!".into()), &mut host);
        assert!(app.buffer.dirty);
        let _ = app.render_headless(800, 600, 1.0);
        let lay = titlebar::layout(&app.fonts, &app.tab_labels(), 800.0, app.scale);
        let save = lay.saves[0].expect("dirty tab shows a save button");
        assert!(
            save.right() <= lay.closes[0].x + 0.5,
            "save button must sit left of the close ×"
        );
        assert_eq!(
            titlebar::hit_test(&lay, save.x + 2.0, save.y + 2.0),
            TitleHit::TabSave(0)
        );
        app.on_event(
            Event::MouseDown {
                x: save.x + 2.0,
                y: save.y + 2.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(!app.buffer.dirty, "clicking the tab save button saves");
        assert_eq!(
            std::fs::read_to_string(app.path.as_ref().unwrap()).unwrap(),
            "!hello"
        );
        // A clean tab carries no save button.
        let _ = app.render_headless(800, 600, 1.0);
        let lay = titlebar::layout(&app.fonts, &app.tab_labels(), 800.0, app.scale);
        assert!(lay.saves[0].is_none());
    }

    #[test]
    fn undo_back_to_saved_clears_the_save_button() {
        // The report: type, see the save button pop up, undo it all — and
        // the button stayed up. Dirty now means "differs from saved",
        // recomputed on every edit, so it clears again.
        let mut app = open("editor_test_undoclean.txt", "hello");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host); // Edit
        assert!(!app.buffer.dirty, "a just-opened file is clean");
        app.on_event(Event::Text("!".into()), &mut host);
        assert!(app.buffer.dirty);
        let _ = app.render_headless(800, 600, 1.0);
        let lay = titlebar::layout(&app.fonts, &app.tab_labels(), 800.0, app.scale);
        assert!(lay.saves[0].is_some(), "dirty tab shows a save button");
        // Undo the keystroke: the content matches the file again.
        app.on_event(ctrl('z'), &mut host);
        assert_eq!(app.buffer.text(), "hello");
        assert!(!app.buffer.dirty, "undo-to-saved must clear dirty");
        let _ = app.render_headless(800, 600, 1.0);
        let lay = titlebar::layout(&app.fonts, &app.tab_labels(), 800.0, app.scale);
        assert!(lay.saves[0].is_none(), "the save button must go away");
        // And re-editing raises it again.
        app.on_event(Event::Text("?".into()), &mut host);
        assert!(app.buffer.dirty);
    }

    #[test]
    fn status_bar_open_button_opens_a_file() {
        let mut app = App::new(None).unwrap();
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        let open_btn = app
            .status_buttons
            .iter()
            .find(|b| b.id == 5)
            .expect("Open button")
            .rect;
        // Leftmost in the row: Find Open | Preview Edit Split | Help | Theme.
        let find_btn = app
            .status_buttons
            .iter()
            .find(|b| b.id == 7)
            .expect("Find button")
            .rect;
        let leftmost = app
            .status_buttons
            .iter()
            .map(|b| b.rect.x)
            .fold(f32::MAX, f32::min);
        assert!((find_btn.x - leftmost).abs() < 0.5, "Find is leftmost");
        assert!(
            (open_btn.x - find_btn.right()).abs() < 8.0,
            "Open sits right of Find"
        );
        let out = std::env::temp_dir().join("editor_test_openbtn.txt");
        std::fs::write(&out, "opened").unwrap();
        host.picked_file = Some(out.clone());
        app.on_event(
            Event::MouseDown {
                x: open_btn.x + 2.0,
                y: open_btn.y + 2.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert_eq!(app.path.as_ref(), Some(&out));
        assert_eq!(app.buffer.text(), "opened");
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn arrow_keys_move_caret_in_edit_mode() {
        let mut app = open("editor_test_l.txt", "first\nsecond");
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
        let mut app = open("editor_test_m.txt", "abcdef");
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
        let mut app = open("editor_test_n.txt", &"word line\n".repeat(40));
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
            "editor_test_p.json",
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
        let mut app = open("editor_test_q.txt", "one");
        let second = std::env::temp_dir().join("editor_test_r.md");
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
        let mut app = open("editor_test_s.txt", "keep me");
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
        let pick = std::env::temp_dir().join("editor_test_t.json");
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
        let mut app = open("editor_test_u.txt", "hello");
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
    fn about_overlay_toggles_with_f2_and_closes() {
        let mut app = open("editor_test_ab.txt", "hello");
        let mut host = NullHost::default();
        let f2 = || Event::KeyDown {
            key: Key::F(2),
            mods: Modifiers::default(),
        };
        app.on_event(f2(), &mut host);
        assert!(app.about_open);
        // F2 toggles it back off.
        app.on_event(f2(), &mut host);
        assert!(!app.about_open);
        // Escape closes it too.
        app.on_event(f2(), &mut host);
        assert!(app.about_open);
        app.on_event(
            Event::KeyDown {
                key: Key::Escape,
                mods: Modifiers::default(),
            },
            &mut host,
        );
        assert!(!app.about_open);
    }

    #[test]
    fn about_and_help_overlays_are_mutually_exclusive() {
        let mut app = open("editor_test_ab2.txt", "hello");
        let mut host = NullHost::default();
        let key = |k| Event::KeyDown {
            key: k,
            mods: Modifiers::default(),
        };
        app.on_event(key(Key::F(1)), &mut host);
        assert!(app.help_open && !app.about_open);
        // F2 swaps help for the about box rather than stacking them.
        app.on_event(key(Key::F(2)), &mut host);
        assert!(app.about_open && !app.help_open);
        app.on_event(key(Key::F(1)), &mut host);
        assert!(app.help_open && !app.about_open);
        // Any click dismisses whichever is up.
        app.on_event(
            Event::MouseDown {
                x: 100.0,
                y: 100.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(!app.help_open && !app.about_open);
    }

    #[test]
    fn status_bar_about_button_opens_the_box() {
        let mut app = open("editor_test_ab3.txt", "hello");
        let mut host = NullHost::default();
        let _ = app.render_headless(900, 600, 1.0);
        let about = app
            .status_buttons
            .iter()
            .find(|b| b.label == "About")
            .expect("About button")
            .rect;
        app.on_event(
            Event::MouseDown {
                x: about.x + 2.0,
                y: about.y + 2.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.about_open);
    }

    #[test]
    fn menu_about_item_opens_the_box() {
        let mut app = open("editor_test_ab4.txt", "hello");
        let mut host = NullHost::default();
        let _ = app.render_headless(900, 600, 1.0);
        app.on_event(
            Event::MouseDown {
                x: 300.0,
                y: 300.0,
                button: MouseButton::Right,
            },
            &mut host,
        );
        let menu = app.menu.as_ref().expect("right-click opens a menu");
        let item = menu
            .rects
            .iter()
            .enumerate()
            .find(|(i, _)| menu.items[*i].0 == menu::Action::About)
            .map(|(_, r)| *r)
            .expect("menu offers About");
        app.on_event(
            Event::MouseDown {
                x: item.x + 4.0,
                y: item.y + 4.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.about_open);
        assert!(app.menu.is_none());
    }

    #[test]
    fn right_click_menu_copy_all_and_dismiss() {
        let mut app = open("editor_test_v.txt", "whole document");
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
        let mut app = open("editor_test_w.txt", "one");
        let second = std::env::temp_dir().join("editor_test_x.txt");
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
        let mut app = open("editor_test_y.json", "{\"a\": {\"b\": 1}, \"c\": [1, 2]}");
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
        let mut app = open("editor_test_caret.txt", "hello world");
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
        // Content (and the caret with it) is inset by the gutter rail.
        let rail = gutter::rail_w(&app.fonts, &ctx, app.buffer.line_count());
        let x0 = (view.x + cx + rail - app.h_scroll).round() as usize;
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
        let mut app = open("editor_test_o.txt", &"second line of text\n".repeat(20));
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
        let mut app = open("editor_test_sel.txt", "alpha\nbeta\ngamma\ndelta\n");
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
        let mut app = open("editor_test_sb.txt", "hello");
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
        let path = std::env::temp_dir().join("editor_test_rc.txt");
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
        let path = std::env::temp_dir().join("editor_test_rh.txt");
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
        let mut app = open("editor_test_theme.txt", "hello");
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
        let mut app = open("editor_test_tick.txt", &"line\n".repeat(200));
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
        let mut app = open("editor_test_blink.txt", "hello");
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
        let mut app = open("editor_test_themebtn.txt", "hello");
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
        let mut app = open("editor_test_splitdiv.txt", "hello");
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
        let mut app = open("editor_test_splitwrap.txt", "word ".repeat(120).trim_end());
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

    /// Long wrapping paragraphs: the preview runs much taller than the
    /// source lines, which is the Split case the ratio lock exists for.
    fn open_long_preview(name: &str) -> App {
        let para = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do ";
        let text = (0..40)
            .map(|_| para.repeat(6).trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut app = open(name, &format!("# Long\n\n{text}\n"));
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        app.on_event(ctrl('e'), &mut host); // Split
        app
    }

    #[test]
    fn split_scroll_shares_a_ratio_not_pixels() {
        // The report: with a long preview the shared pixel offset sailed the
        // short editor pane past its own end (phantom gutter rows, pinned
        // thumb). Each pane now paints its own share of one ratio.
        let mut app = open_long_preview("editor_test_splitratio.md");
        let _ = app.render_headless(1000, 700, 1.0);
        assert!(
            app.preview_max() > app.editor_max() * 1.5,
            "setup needs a much longer preview: {} vs {}",
            app.preview_max(),
            app.editor_max()
        );
        // Bottom: the editor stops at its own end while the ratios agree.
        app.scroll.offset = app.scroll.max();
        assert!(
            app.editor_scroll() <= app.editor_max() + 1.0,
            "editor past its end: {} > {}",
            app.editor_scroll(),
            app.editor_max()
        );
        for (got, name) in [
            (app.scroll_ratio(), "shared"),
            (app.preview_scroll() / app.preview_max(), "preview"),
            (app.editor_scroll() / app.editor_max(), "editor"),
        ] {
            assert!(
                (got - 1.0).abs() < 0.01,
                "{name} ratio should be 1 at the bottom: {got}"
            );
        }
        // Middle: both panes sit halfway through their own content, so the
        // preview covers more ground over the same thumb travel.
        app.scroll.offset = app.scroll.max() / 2.0;
        let preview_ratio = app.preview_scroll() / app.preview_max();
        let editor_ratio = app.editor_scroll() / app.editor_max();
        assert!((preview_ratio - 0.5).abs() < 0.02, "{preview_ratio}");
        assert!((editor_ratio - 0.5).abs() < 0.02, "{editor_ratio}");
        assert!(
            app.preview_scroll() > app.editor_scroll() * 1.5,
            "preview should move further per ratio point"
        );
        // Both thumbs ride at the same relative travel.
        let pv = app.sb_geom(false, false).expect("preview bar");
        let ed = app.sb_geom(true, false).expect("editor bar");
        let rel = |g: &crate::ui::preview_view::SbGeom| (g.thumb.y - g.start) / g.travel.max(1.0);
        assert!(
            (rel(&pv) - rel(&ed)).abs() < 0.05,
            "thumbs must stay locked: {} vs {}",
            rel(&pv),
            rel(&ed)
        );
    }

    #[test]
    fn split_thumb_drag_drives_both_panes_by_ratio() {
        // Dragging either thumb sets the shared ratio, so the other pane
        // follows: grab the short editor bar and pull it to the bottom.
        let mut app = open_long_preview("editor_test_splitdrag.md");
        let mut host = NullHost::default();
        let _ = app.render_headless(1000, 700, 1.0);
        let g = app.sb_geom(true, false).expect("editor bar");
        app.on_event(
            Event::MouseDown {
                x: g.thumb.x + 1.0,
                y: g.thumb.y + 1.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.sb_drag.is_some(), "thumb press starts a drag");
        app.on_event(
            Event::MouseMove {
                x: g.thumb.x + 1.0,
                y: g.start + g.travel,
            },
            &mut host,
        );
        assert!(
            app.scroll_ratio() > 0.95,
            "dragging the editor thumb to its end must reach the document end: {}",
            app.scroll_ratio()
        );
        // A 1 px grab offset keeps the last pixel of travel out of reach,
        // so allow a relative epsilon instead of exactness.
        assert!(
            (app.preview_scroll() - app.preview_max()).abs() <= app.preview_max() * 0.01 + 1.0,
            "preview follows to its own end: {} vs {}",
            app.preview_scroll(),
            app.preview_max()
        );
        app.on_event(
            Event::MouseUp {
                x: 0.0,
                y: 0.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.sb_drag.is_none());
    }

    /// Leftmost and rightmost edge (physical px) of the preview's text.
    fn preview_text_extent(app: &App) -> (f32, f32) {
        let ctx = app.ctx();
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for op in &app.preview.list.ops {
            if let crate::layout::display_list::DrawOp::Text {
                x,
                text,
                size,
                style,
                ..
            } = op
            {
                min = min.min(*x);
                max = max.max(*x + ctx.fonts.measure(*style, text, *size));
            }
        }
        (min, max)
    }

    #[test]
    fn a_wider_window_gets_a_wider_reading_column() {
        // Regression for "maximizing doesn't readjust": the wrapped panes kept a
        // fixed 820 px measure however wide the window got, so maximizing left
        // most of the screen empty instead of reflowing the text into it.
        let mut app = open(
            "editor_test_column.txt",
            &format!("# Wide\n\n{}\n", "word ".repeat(400)),
        );
        let _ = app.render_headless(1100, 800, 1.0);
        let (n_min, n_max) = preview_text_extent(&app);
        let _ = app.render_headless(1920, 1168, 1.0);
        let (w_min, w_max) = preview_text_extent(&app);
        assert!(
            w_max - w_min > (n_max - n_min) * 1.5,
            "the column must reflow into a maximized window: {n_min}..{n_max} -> {w_min}..{w_max}"
        );
        // Centered, and inside the pane: a wider column must not run under the
        // window edge or the overlay scrollbar.
        let pane = app.viewport();
        assert!(w_min > pane.x + 100.0 && w_max < pane.right() - 100.0);
        // The right margin is a word short of the left one at most.
        assert!(
            (w_min - pane.x - (pane.right() - w_max)).abs() < 60.0,
            "not centered: {w_min} .. {w_max} in {pane:?}"
        );
        // A first row of the document is still the heading, i.e. the whole page
        // was laid out again for the new width rather than scaled or clipped.
        assert!(w_max - w_min > 0.0);
    }

    /// How long one frame costs, for the scroll-smoothness work: run with
    /// `cargo test --release -- --ignored --nocapture paint_cost`.
    #[test]
    #[ignore]
    fn paint_cost() {
        let body = "word ".repeat(60);
        let doc = (0..1200)
            .map(|i| format!("## Section {i}\n\n{body}\n"))
            .collect::<String>();
        let mut app = open("editor_bench.md", &doc);
        let mut host = NullHost::default();
        for (w, h) in [(1100usize, 800usize), (1920, 1168)] {
            let _ = app.render_headless(w, h, 1.0);
            let n = 60;
            let t = std::time::Instant::now();
            for i in 0..n {
                app.set_scroll((i * 37) as f32);
                app.on_event(Event::Tick, &mut host);
                let _ = app.render_headless(w, h, 1.0);
            }
            let per = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
            println!("paint {w}x{h}: {per:.2} ms/frame ({:.0} fps)", 1000.0 / per);
            println!(
                "  preview ops {}, status buttons {}, rows {}",
                app.preview.list.ops.len(),
                app.status_buttons.len(),
                app.preview_rows.len()
            );
            // Split: canvas allocation vs the painted content.
            let mut c = crate::gfx::canvas::Canvas::new(w, h);
            let t_alloc = t.elapsed();
            let t2 = std::time::Instant::now();
            crate::platform::Handler::paint(&mut app, &mut c);
            let one = t2.elapsed().as_secs_f64() * 1000.0;
            println!(
                "  one paint {one:.2} ms (loop total {:.2})",
                t_alloc.as_secs_f64() * 1000.0
            );
        }
    }

    #[test]
    fn cursor_over_scrollbars_is_arrow_not_ibeam() {
        let mut app = open(
            "editor_test_sbcur.txt",
            &"long line without scrolling\n".repeat(60),
        );
        app.on_event(ctrl('e'), &mut NullHost::default()); // Edit (=> editor pane)
        let _ = app.render_headless(800, 600, 1.0);
        // Over the vertical overlay scrollbar zone (which sits a window-edge
        // grip clear of the border so the OS resize border cannot eat it).
        let view = app.viewport();
        let v = app
            .editor_pane
            .v_sb(view, app.scroll.offset, app.metrics())
            .expect("setup: editor pane must scroll");
        let sb_x = v.zone.x + 1.0;
        let y_mid = view.y + view.h / 2.0;
        assert_eq!(app.cursor_at(sb_x, y_mid), CursorShape::Arrow);
        // Just inside the text area, same row: still I-beam.
        assert_eq!(
            app.cursor_at(view.right() - v.zone.w - 30.0, y_mid),
            CursorShape::IBeam
        );
        // Horizontal bar zone (long line in Edit => h_max > 0). One file:
        // re-use with a wide line via a fresh tab.
        let wide = std::env::temp_dir().join("editor_test_sbcursor_h.txt");
        std::fs::write(&wide, format!("x{}", "y".repeat(300))).unwrap();
        app.open_path(wide, &mut NullHost::default());
        let _ = app.render_headless(800, 600, 1.0);
        assert!(app.h_max > 0.0, "setup: need a wide line");
        let h = app
            .editor_pane
            .h_sb(
                view,
                PaneScroll::editor(app.scroll.offset, app.h_scroll, app.h_max, 0.0),
                app.metrics(),
            )
            .expect("setup: wide line must overflow");
        assert_eq!(
            app.cursor_at(view.x + 200.0, h.zone.y + 1.0),
            CursorShape::Arrow
        );
    }

    #[test]
    fn editor_does_not_wrap_and_scrolls_horizontally() {
        let long = format!("{} end", "word ".repeat(200));
        let mut app = open("editor_test_nw.txt", &long);
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host); // Edit
        let _ = app.render_headless(600, 400, 1.0);
        // One row per line, no folding.
        assert_eq!(app.editor_pane.list.ops.len(), 1);
        assert!(app.h_max > 0.0, "long line must create scroll range");
        // Horizontal wheel pans the editor pane with a glide, like vertical
        // notches: nothing lands on the event's own frame.
        app.on_event(
            Event::Scroll {
                dx: -120.0,
                dy: 0.0,
            },
            &mut host,
        );
        assert_eq!(app.h_scroll, 0.0, "a notch must not land in one jump");
        assert_eq!(app.h_target, 120.0);
        assert!(app.scrolling, "a glide needs the frame clock");
        for _ in 0..600 {
            if !app.scrolling {
                break;
            }
            app.on_event(Event::Frame, &mut host);
        }
        assert_eq!(app.h_scroll, 120.0);
        assert!(!app.scrolling);
        // Clamped at the max.
        app.on_event(
            Event::Scroll {
                dx: -1_000_000.0,
                dy: 0.0,
            },
            &mut host,
        );
        assert_eq!(app.h_target, app.h_max);
        app.on_event(
            Event::Scroll {
                dx: 1_000_000.0,
                dy: 0.0,
            },
            &mut host,
        );
        assert_eq!(app.h_target, 0.0);
        for _ in 0..600 {
            if !app.scrolling {
                break;
            }
            app.on_event(Event::Frame, &mut host);
        }
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

    /// Long-line setup with a real horizontal scroll range, in Edit mode.
    fn open_wide(name: &str) -> (App, NullHost) {
        let long = format!("{} end", "word ".repeat(200));
        let mut app = open(name, &long);
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host); // Edit
        let _ = app.render_headless(600, 400, 1.0);
        assert!(app.h_max > 100.0, "setup: need room to glide");
        (app, host)
    }

    #[test]
    fn horizontal_tilt_glides_over_frames_instead_of_jumping() {
        // The report: the horizontal bar felt choppy next to the vertical
        // one, because every tilt landed in one jump while vertical notches
        // glide. Same easing, same clock discipline now.
        let (mut app, mut host) = open_wide("editor_test_hglide.txt");
        app.on_event(Event::Scroll { dx: -90.0, dy: 0.0 }, &mut host);
        assert_eq!(app.h_scroll, 0.0, "a tilt must not land in one jump");
        assert_eq!(app.h_target, 90.0);
        assert!(host.animating, "a glide needs the frame clock");
        let mut seen = vec![app.h_scroll];
        for _ in 0..600 {
            if !app.scrolling {
                break;
            }
            app.on_event(Event::Frame, &mut host);
            seen.push(app.h_scroll);
        }
        assert!(
            seen.len() > 5,
            "a tilt should arrive over several frames, not {}: {seen:?}",
            seen.len()
        );
        for w in seen.windows(2) {
            assert!(
                w[1] > w[0],
                "each frame moves a little, never back: {seen:?}"
            );
        }
        assert_eq!(app.h_scroll, 90.0);
        assert_eq!(app.h_target, 90.0);
        assert!(!app.scrolling);
        assert!(!host.animating);
    }

    #[test]
    fn fine_horizontal_tilt_applies_directly() {
        // Trackpad-grade deltas are already smooth: gliding them would only
        // add latency, exactly like the vertical fine path.
        let (mut app, mut host) = open_wide("editor_test_hfine.txt");
        app.on_event(Event::Scroll { dx: -3.0, dy: 0.0 }, &mut host);
        assert_eq!(app.h_scroll, 3.0);
        assert_eq!(app.h_target, 3.0);
        assert!(!app.scrolling, "no glide stands behind a fine delta");
    }

    #[test]
    fn touch_pan_stays_on_the_finger_and_ends_the_h_glide() {
        // A finger landing mid-glide takes the view over horizontally too:
        // the tilt's leftover travel is dropped, not resumed under the finger.
        let (mut app, mut host) = open_wide("editor_test_hpan.txt");
        app.on_event(Event::Scroll { dx: -90.0, dy: 0.0 }, &mut host);
        assert!(app.scrolling);
        app.on_event(Event::Pan { dx: -17.0, dy: 0.0 }, &mut host);
        assert_eq!(app.h_scroll, 17.0);
        assert_eq!(app.h_target, 17.0, "the finger owns the view now");
    }

    #[test]
    fn horizontal_thumb_drag_tracks_the_pointer_exactly() {
        // Pointer-driven input never glides: the thumb is the pointer.
        let (mut app, mut host) = open_wide("editor_test_hdrag.txt");
        let g = app.sb_geom(true, true).expect("horizontal bar");
        app.on_event(
            Event::MouseDown {
                x: g.thumb.x + 1.0,
                y: g.thumb.y + 1.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.sb_drag.is_some(), "thumb press starts a drag");
        let px = g.start + g.travel / 2.0;
        app.on_event(
            Event::MouseMove {
                x: px,
                y: g.thumb.y + 1.0,
            },
            &mut host,
        );
        let want = g.offset_for(px - 1.0);
        assert_eq!(app.h_scroll, want);
        assert_eq!(app.h_target, want, "no glide behind a drag");
    }

    #[test]
    fn h_glide_target_clamps_when_the_line_shrinks() {
        // A relayout or frame that narrows the range must not leave the
        // target unreachable past the end.
        let (mut app, mut host) = open_wide("editor_test_hclamp.txt");
        app.h_scroll = app.h_max - 100.0;
        app.h_target = app.h_max + 500.0;
        app.scrolling = true;
        app.on_event(Event::Frame, &mut host);
        assert_eq!(
            app.h_target, app.h_max,
            "the frame clamps an unreachable target first"
        );
        for _ in 0..600 {
            if !app.scrolling {
                break;
            }
            app.on_event(Event::Frame, &mut host);
        }
        assert_eq!(app.h_scroll, app.h_max);
        assert_eq!(app.h_target, app.h_max);
        assert!(!app.scrolling);
    }

    #[test]
    fn preview_still_wraps_in_split() {
        let mut app = open("editor_test_pw.txt", "word ".repeat(120).trim_end());
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        app.on_event(ctrl('e'), &mut host); // Split
        let _ = app.render_headless(800, 600, 1.0);
        // The preview pane wraps (many rows); the editor pane does not (1 row).
        assert!(app.preview_rows.len() > 1, "preview should wrap");
        assert_eq!(app.editor_pane.list.ops.len(), 1, "editor must not wrap");
    }

    #[test]
    fn vertical_scrollbar_clears_the_resize_grip_in_every_mode() {
        // Regression: the overlay scrollbar sat flush with the window's right
        // edge, inside the strip the OS keeps for resizing, so pressing the
        // thumb started a window resize instead of scrolling. Preview, Edit
        // and Split reach the bar through different paths (`sb_geom`, `sb_at`,
        // `paint_panes` all branch on mode), so every mode is checked: the
        // zone and thumb clear the grip, the pointer is an arrow there, a
        // press on the visible thumb starts a drag, and the painted bar never
        // shows up inside the strip.
        let band = platform::edge_grip();
        let (w, h) = (800usize, 600usize);
        // One block per paragraph guarantees a document taller than any pane.
        let content = "a paragraph of text\n\n".repeat(120);
        for (presses, mode) in [(0u8, Mode::Preview), (1, Mode::Edit), (2, Mode::Split)] {
            let mut app = open("editor_test_sbedge.txt", &content);
            let mut host = NullHost::default();
            for _ in 0..presses {
                app.on_event(ctrl('e'), &mut host); // Preview -> Edit -> Split
            }
            assert_eq!(app.mode, mode, "setup: mode after {presses} Ctrl+E");
            assert_eq!(
                app.edge,
                band * app.scale,
                "{mode:?}: app tracks the platform band (physical px)"
            );
            // The strip the bars clear is derived from a bar's own thickness
            // under the platform band, so it can never grow into the bar.
            let grip = app.bar_grip();
            assert_eq!(
                grip,
                preview_view::bar_grip(app.scale, app.edge),
                "{mode:?}: bar-safe strip is derived, not chosen"
            );
            assert!(
                grip <= preview_view::BAR_W_LOGICAL * preview_view::GRIP_SHARE * app.scale + 0.001,
                "{mode:?}: strip {grip} eats into the bar"
            );
            // Lay out first: the bar exists only once the pane knows its
            // content height.
            let canvas = app.render_headless(w, h, 1.0);
            // The pane that owns the window's right edge: the preview when it
            // fills the window, otherwise the editor pane.
            let editor = mode != Mode::Preview;
            let area = if editor {
                app.editor_area()
            } else {
                app.viewport()
            };
            let g = app
                .sb_geom(editor, false)
                .unwrap_or_else(|| panic!("{mode:?}: expected a vertical scrollbar"));
            assert!(
                g.thumb.right() <= area.right() - grip,
                "{mode:?}: thumb leaves no room for the resize strip: {g:?}"
            );
            assert!(
                g.zone.right() <= area.right() - grip + 0.001,
                "{mode:?}: interactive zone reaches into the strip: {g:?}"
            );
            // The bar still hugs the window edge — the gap is the resize
            // strip, not a wide inset that reads as a floating bar.
            assert!(
                area.right() - g.thumb.right() <= grip.max(3.0 * app.scale) + 0.001,
                "{mode:?}: bar floats {} px from the edge",
                area.right() - g.thumb.right()
            );
            let (px, py) = (g.thumb.x + 1.0, g.thumb.y + 2.0);
            assert_eq!(
                app.cursor_at(px, py),
                CursorShape::Arrow,
                "{mode:?}: the pointer over the bar is an arrow"
            );
            app.on_event(
                Event::MouseDown {
                    x: px,
                    y: py,
                    button: MouseButton::Left,
                },
                &mut host,
            );
            assert!(
                app.sb_drag.is_some(),
                "{mode:?}: a press on the visible thumb must start a scroll drag"
            );
            // Painted pixels agree with the geometry *and* with the strip that
            // applies at each of them: the strip is asked for per point, so a
            // painted bar column must be a column the platform does not claim
            // at that row — anywhere else the pixel would be unreachable.
            let bar = canvas.pixels[(py as usize) * w + px as usize];
            let samples: Vec<(usize, usize)> = (48..h - 48)
                .step_by(7)
                .flat_map(|y| ((w - 24)..w).map(move |x| (x, y)))
                .filter(|&(x, y)| canvas.pixels[y * w + x] == bar)
                .collect();
            for &(x, y) in &samples {
                let g = app.grip_at(x as f32, y as f32, band);
                assert!(
                    x as f32 + g <= w as f32,
                    "{mode:?}: bar pixel at ({x}, {y}) is inside the {g} px strip the platform claims there"
                );
            }
            assert!(
                samples.len() > 20,
                "{mode:?}: sampled colour is not the bar ({} px)",
                samples.len()
            );
            let inside = (40..h - 40)
                .flat_map(|y| ((w - grip as usize - 20)..(w - grip as usize)).map(move |x| (x, y)))
                .filter(|&(x, y)| canvas.pixels[y * w + x] == bar)
                .count();
            assert!(
                inside > 20,
                "{mode:?}: sampled colour is not the bar ({inside} px)"
            );
        }
    }

    #[test]
    fn resize_grip_self_tunes_to_what_is_at_the_edge() {
        // The strip is asked for per point, not chosen once: the platform's full
        // band wherever the app draws nothing, and only the bar-safe share over
        // a bar. Both halves matter — widening an edge is only safe as long as
        // the bars still get their pixels back — so each is asserted in every
        // mode the bar is reachable through.
        let band = platform::edge_grip();
        let (w, h) = (800usize, 600usize);
        let mid = h as f32 / 2.0;
        let near_edge = w as f32 - 1.0;
        // A document that fits: no bar at all, so the whole edge is resizable
        // at the platform's full band rather than the bar-safe share.
        let mut short = open("editor_test_grip.txt", "one short line\n");
        let _ = short.render_headless(w, h, 1.0);
        assert!(
            short.sb_geom(false, false).is_none(),
            "setup: a short document needs no scrollbar"
        );
        assert_eq!(
            short.grip_at(near_edge, mid, band),
            band,
            "no bar at the edge: it keeps the platform's full band"
        );
        let content = "a paragraph of text\n\n".repeat(120);
        for (presses, mode) in [(0u8, Mode::Preview), (1, Mode::Edit), (2, Mode::Split)] {
            let mut app = open("editor_test_grip2.txt", &content);
            let mut host = NullHost::default();
            for _ in 0..presses {
                app.on_event(ctrl('e'), &mut host); // Preview -> Edit -> Split
            }
            let _ = app.render_headless(w, h, 1.0);
            let editor = mode != Mode::Preview;
            let grip = app.bar_grip();
            // Never wider than the band the platform claims, and strictly
            // narrower over a bar — the second half only means something when
            // there is a band at all: macOS reports `0.0` (no edge grip) until
            // the AppKit backend lands (M9), and then there is nothing for a
            // bar to hand back, which the `<=` pins exactly.
            assert!(
                grip <= band * app.scale,
                "{mode:?}: the bar-safe strip is never wider than the platform's band"
            );
            if band > 0.0 {
                assert!(
                    grip < band * app.scale,
                    "{mode:?}: the bar-safe strip must be the narrower one"
                );
            }
            assert_eq!(
                grip,
                preview_view::bar_grip(app.scale, band),
                "{mode:?}: the bar-safe strip follows the band it is given"
            );
            let g = app
                .sb_geom(editor, false)
                .unwrap_or_else(|| panic!("{mode:?}: expected a vertical scrollbar"));
            // Over the bar the app keeps its pixels; the columns between the
            // bar and the edge stay available for resizing, which is what the
            // platform does with the band it gets back there.
            for (name, x) in [
                ("thumb", g.thumb.x + 1.0),
                ("bar's outer column", g.thumb.right() - 1.0),
            ] {
                assert_eq!(
                    app.grip_at(x, mid, band),
                    grip,
                    "{mode:?}: {name} must be handed back to the app"
                );
            }
            assert_eq!(
                app.grip_at(near_edge, mid, band),
                band,
                "{mode:?}: the outermost columns stay the platform's to resize"
            );
            // Away from the bar the whole band is available: a point on the far
            // edge is inside the band's width but nothing the app draws on.
            assert_eq!(
                app.grip_at(1.0, mid, band),
                band,
                "{mode:?}: the far edge is untouched by the bar"
            );
        }
    }

    #[test]
    fn a_wheel_notch_glides_over_frames_instead_of_jumping() {
        // The choppiness is the *step*, not the speed: a notch that lands in
        // one frame is four or five visible jumps. Nothing moves on the
        // notch's own frame, the host is asked for a clock fast enough to
        // animate on, and the travel arrives over several frames.
        let mut app = open("editor_test_glide.txt", &"line\n".repeat(400));
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        assert_eq!(app.scroll.offset, 0.0);
        app.on_event(Event::Scroll { dx: 0.0, dy: -48.0 }, &mut host);
        assert_eq!(app.scroll.offset, 0.0, "a notch must not land in one jump");
        assert!(
            host.animating,
            "a glide needs frames the heartbeat cannot give"
        );
        let mut seen = vec![app.scroll.offset];
        for _ in 0..600 {
            if !app.scrolling {
                break;
            }
            app.on_event(Event::Frame, &mut host);
            seen.push(app.scroll.offset);
        }
        assert!(
            seen.len() > 5,
            "a notch should arrive over several frames, not {}: {seen:?}",
            seen.len()
        );
        for w in seen.windows(2) {
            assert!(
                w[1] > w[0],
                "each frame must move a little, and never backwards: {seen:?}"
            );
        }
        // It lands exactly where the notch pointed, and the clock stops with
        // it — a host left animating forever is a host repainting forever.
        assert_eq!(app.scroll.offset, 48.0);
        assert!(!app.scrolling);
        assert!(!host.animating);
    }

    #[test]
    fn a_finger_pan_follows_the_input_and_drops_a_glide() {
        let mut app = open("editor_test_pan.txt", &"line\n".repeat(400));
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        // Direct manipulation tracks the input exactly: gliding a finger's own
        // movement would read as lag, not as smoothness.
        app.on_event(Event::Pan { dx: 0.0, dy: -17.0 }, &mut host);
        assert_eq!(app.scroll.offset, 17.0);
        assert!(
            !app.scrolling,
            "no glide stands between the finger and the view"
        );
        // A finger landing mid-glide takes the view over: the notch's leftover
        // travel is dropped rather than resumed under the finger.
        app.on_event(Event::Scroll { dx: 0.0, dy: -48.0 }, &mut host);
        assert!(app.scrolling);
        app.on_event(Event::Pan { dx: 0.0, dy: -5.0 }, &mut host);
        assert_eq!(app.scroll.offset, 22.0);
        assert_eq!(
            app.scroll.target, 22.0,
            "the glide is abandoned, not pending"
        );
        assert!(!app.scrolling);
        assert!(!host.animating, "the frame clock goes with the glide");
    }

    #[test]
    fn a_flick_coasts_past_the_finger() {
        // The report: touch scrolling should "coast a little after scroll".
        // PanEnd carries the finger's release velocity; the app hands it to
        // the scroller and runs the frame clock, and the view keeps moving
        // with no further input until the momentum is spent.
        let mut app = open("editor_test_fling.txt", &"line\n".repeat(400));
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        // Drag down (dy < 0 → content down … see Pan's sign) then lift with
        // the motion still going: a solid 1.5 px/ms upward flick.
        for _ in 0..5 {
            app.on_event(Event::Pan { dx: 0.0, dy: -30.0 }, &mut host);
        }
        assert_eq!(app.scroll.offset, 150.0);
        app.on_event(Event::PanEnd { vx: 0.0, vy: -1.5 }, &mut host);
        assert!(app.scrolling, "a flick needs the frame clock");
        assert!(host.animating);
        assert!(app.scroll.coast != 0.0);
        assert_eq!(app.scroll.target, 150.0, "a coast has no destination");
        // Frames keep the ride moving past the finger's last touch, in the
        // same direction, decaying — never reversing, never snapping to a
        // destination.
        let mut prev = app.scroll.offset;
        assert!(prev > 0.0);
        for _ in 0..12 {
            app.on_event(Event::Frame, &mut host);
            assert!(
                app.scroll.offset >= prev,
                "a coast never reverses: {} after {}",
                app.scroll.offset,
                prev
            );
            prev = app.scroll.offset;
        }
        assert!(prev > 160.0, "the coast travelled past the finger: {prev}");
        // The clock stops when the ride is over, and the last frame lands.
        while app.scrolling {
            app.on_event(Event::Frame, &mut host);
        }
        assert!(!host.animating);
        assert_eq!(app.scroll.offset, app.scroll.target);
    }

    #[test]
    fn a_release_that_held_still_stops_the_ride() {
        // The finger arrived, paused, then lifted: ~0 velocity, no coast. The
        // clock ends and the view stays exactly where the finger left it.
        let mut app = open("editor_test_stop.txt", &"line\n".repeat(400));
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        app.on_event(Event::Pan { dx: 0.0, dy: -30.0 }, &mut host);
        assert_eq!(app.scroll.offset, 30.0);
        app.on_event(Event::PanEnd { vx: 0.0, vy: 0.0 }, &mut host);
        assert!(!app.scrolling);
        assert!(app.scroll.coast == 0.0);
        assert_eq!(app.scroll.offset, 30.0);
    }

    #[test]
    fn a_wheel_notch_ends_a_coast_rather_than_fighting_it() {
        // Mid-coast, the wheel speaks: the ride must end and the glide start
        // *from where the view now is* — a glide to the coast's stale target
        // would yank the view backwards under the user's hand.
        let mut app = open("editor_test_wc.txt", &"line\n".repeat(400));
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        app.on_event(Event::PanEnd { vx: 0.0, vy: -2.0 }, &mut host);
        assert!(app.scroll.coast != 0.0);
        app.on_event(Event::Frame, &mut host);
        let mid_coast = app.scroll.offset;
        assert!(mid_coast > 0.0);
        app.on_event(Event::Scroll { dx: 0.0, dy: -48.0 }, &mut host);
        assert!(app.scroll.coast == 0.0, "the wheel ends the ride");
        assert!(app.scrolling, "and runs its own clock");
        assert_eq!(app.scroll.target, mid_coast + 48.0);
        while app.scrolling {
            app.on_event(Event::Frame, &mut host);
        }
        assert_eq!(
            app.scroll.offset,
            mid_coast + 48.0,
            "landed ahead, not back"
        );
    }

    #[test]
    fn scrollbar_thumb_drag_scrolls() {
        let mut app = open("editor_test_sbd.txt", &"line\n".repeat(200));
        let _ = app.render_headless(800, 600, 1.0);
        let area = app.viewport();
        let g = app
            .preview
            .v_sb(area, app.scroll.offset, app.metrics())
            .unwrap();
        assert!(app.scroll.offset < 10.0, "start at top");
        let mut host = NullHost::default();
        // Press on the thumb, drag to the bottom of the track.
        app.on_event(
            Event::MouseDown {
                x: g.thumb.x + 2.0,
                y: g.thumb.y + 4.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.sb_drag.is_some(), "thumb press starts a drag");
        app.on_event(
            Event::MouseMove {
                x: g.thumb.x + 2.0,
                y: g.start + g.travel,
            },
            &mut host,
        );
        assert!(
            app.scroll.offset > g.max * 0.9,
            "drag to bottom: {} / {}",
            app.scroll.offset,
            g.max
        );
        // Release ends the drag.
        app.on_event(
            Event::MouseUp {
                x: g.thumb.x + 2.0,
                y: g.start + g.travel,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.sb_drag.is_none());
        // A track click below the thumb pages down and grabs the thumb.
        app.scroll.offset = 0.0;
        let g2 = app.preview.v_sb(area, 0.0, app.metrics()).unwrap();
        app.on_event(
            Event::MouseDown {
                x: g2.thumb.x + 2.0,
                y: g2.thumb.y + g2.thumb.h + 20.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(app.scroll.offset > 100.0, "track click pages down");
        assert!(app.sb_drag.is_some(), "track click grabs the thumb");
    }

    #[test]
    fn a_maximized_window_keeps_the_rect_it_restores_to() {
        // The window the user reopens wants the size and place they were using
        // *before* maximizing: a maximized report carries the work area, and
        // restoring that would hand back a screen-sized window as soon as it
        // was un-maximized.
        let mut app = open("editor_test_win.txt", "hello\n");
        let mut host = NullHost::default();
        let normal = WindowGeometry {
            x: -1200,
            y: 60,
            width: 1100,
            height: 800,
            maximized: false,
        };
        app.on_event(Event::WindowGeometry(normal), &mut host);
        assert_eq!(app.settings().window, Some((-1200, 60, 1100, 800)));
        assert!(!app.settings().maximized);
        app.on_event(
            Event::WindowGeometry(WindowGeometry {
                x: 0,
                y: 0,
                width: 1920,
                height: 1040,
                maximized: true,
            }),
            &mut host,
        );
        assert_eq!(
            app.settings().window,
            Some((-1200, 60, 1100, 800)),
            "maximizing must not overwrite the rect to restore at"
        );
        assert!(app.settings().maximized);
        // Un-maximizing reports the real rect again, and a later move updates
        // it — the layout the OS restored is authoritative either way.
        let moved = WindowGeometry {
            x: 300,
            y: 200,
            width: 900,
            height: 700,
            maximized: false,
        };
        app.on_event(Event::WindowGeometry(moved), &mut host);
        assert_eq!(app.settings().window, Some((300, 200, 900, 700)));
        assert!(!app.settings().maximized);
    }

    #[test]
    fn leaving_remembers_the_window() {
        // `persist` runs from many places, but the window's rect is only
        // readable while the window exists, so the way *out* is the one that
        // has to write the last one.
        let mut app = open("editor_test_quit.txt", "hello\n");
        let mut host = NullHost::default();
        app.on_event(
            Event::WindowGeometry(WindowGeometry {
                x: 300,
                y: 200,
                width: 900,
                height: 700,
                maximized: false,
            }),
            &mut host,
        );
        app.quit(&mut host);
        assert!(host.quit_requested, "quitting still asks the host to quit");
        assert_eq!(app.settings().window, Some((300, 200, 900, 700)));
    }

    /// The rect a test's window is reported at, normal (not maximized).
    fn geometry(x: i32, y: i32, width: u32, height: u32) -> WindowGeometry {
        WindowGeometry {
            x,
            y,
            width,
            height,
            maximized: false,
        }
    }

    #[test]
    fn a_moved_window_is_written_without_a_clean_exit() {
        // The session is written on the way out, but a killed process, a crash
        // or a power cut never gets there — so the heartbeat has to write a
        // reported geometry on its own, or the next launch reopens at the rect
        // of the last *clean* exit.
        let mut app = open("editor_test_geom_beat.txt", "hello\n");
        let mut host = NullHost::default();
        app.on_event(
            Event::WindowGeometry(geometry(300, 200, 900, 700)),
            &mut host,
        );
        assert!(app.geometry_dirty, "a report is queued for writing");
        for _ in 0..GEOMETRY_SAVE_TICKS - 1 {
            app.on_event(Event::Tick, &mut host);
        }
        assert!(
            app.geometry_dirty,
            "not written while the window is still settling"
        );
        app.on_event(Event::Tick, &mut host);
        assert!(!app.geometry_dirty, "written once the window held still");
        assert_eq!(app.settings().window, Some((300, 200, 900, 700)));
    }

    #[test]
    fn a_repeated_report_cannot_starve_the_write() {
        // Some platforms re-report a rect they have already reported (a state
        // change the window manager echoes back). If every report restarted
        // the wait, a window that only ever re-reported would never reach the
        // file at all.
        let mut app = open("editor_test_geom_repeat.txt", "hello\n");
        let mut host = NullHost::default();
        let g = geometry(10, 20, 800, 600);
        app.on_event(Event::WindowGeometry(g), &mut host);
        for _ in 0..GEOMETRY_SAVE_TICKS {
            app.on_event(Event::WindowGeometry(g), &mut host);
            app.on_event(Event::Tick, &mut host);
        }
        assert!(
            !app.geometry_dirty,
            "the same rect re-reported is still one pending write"
        );
        assert_eq!(app.settings().window, Some((10, 20, 800, 600)));
    }

    #[test]
    fn a_drag_writes_once_at_the_rect_it_ended_on() {
        // A move reports every intermediate rect. Each one defers the write
        // (so a long drag is not a write per frame), and the one that lands is
        // the rect the gesture ended on.
        let mut app = open("editor_test_geom_drag.txt", "hello\n");
        let mut host = NullHost::default();
        for i in 0..6 {
            app.on_event(
                Event::WindowGeometry(geometry(100 + i * 40, 150 + i * 10, 900, 700)),
                &mut host,
            );
            for _ in 0..5 {
                app.on_event(Event::Tick, &mut host);
            }
            assert!(app.geometry_dirty, "mid-drag the write is still pending");
        }
        for _ in 0..GEOMETRY_SAVE_TICKS {
            app.on_event(Event::Tick, &mut host);
        }
        assert_eq!(
            app.settings().window,
            Some((300, 200, 900, 700)),
            "the rect the drag ended on is the one remembered"
        );
    }

    #[test]
    fn a_maximized_window_is_written_even_though_its_rect_is_not() {
        // Maximizing changes what the next session does (open maximized)
        // without changing the rect it reopens at, so the change has to be
        // queued on its own — the rect of a maximized report is the work area,
        // which must not replace the one to restore.
        let mut app = open("editor_test_geom_max.txt", "hello\n");
        let mut host = NullHost::default();
        app.restore_window(Some((120, 80, 1100, 800)), false);
        app.on_event(
            Event::WindowGeometry(WindowGeometry {
                maximized: true,
                ..geometry(0, 0, 1920, 1040)
            }),
            &mut host,
        );
        assert!(app.geometry_dirty, "the maximize is queued for writing");
        for _ in 0..GEOMETRY_SAVE_TICKS {
            app.on_event(Event::Tick, &mut host);
        }
        assert!(!app.geometry_dirty);
        assert_eq!(app.settings().window, Some((120, 80, 1100, 800)));
        assert!(app.settings().maximized);
        // A first report that only confirms the restored state is not a change,
        // so a session reopened maximized does not rewrite itself on launch.
        let mut again = open("editor_test_geom_max2.txt", "hello\n");
        again.restore_window(Some((120, 80, 1100, 800)), true);
        again.on_event(
            Event::WindowGeometry(WindowGeometry {
                maximized: true,
                ..geometry(0, 0, 1920, 1040)
            }),
            &mut host,
        );
        assert!(!again.geometry_dirty);
    }

    #[test]
    fn a_saved_window_is_what_the_platform_is_asked_to_open_at() {
        let mut app = open("editor_test_win2.txt", "hello\n");
        let mut host = NullHost::default();
        // Nothing saved yet (a first run): the platform places the window.
        assert_eq!(app.initial_window(), None);
        app.restore_window(Some((120, 80, 1100, 800)), true);
        assert_eq!(
            app.initial_window(),
            Some(WindowGeometry {
                x: 120,
                y: 80,
                width: 1100,
                height: 800,
                maximized: true,
            })
        );
        // And the whole thing survives a settings round trip, which is what
        // the next launch reads.
        let round_tripped = app.settings();
        assert_eq!(round_tripped.window, Some((120, 80, 1100, 800)));
        assert!(round_tripped.maximized);
        let mut next = open("editor_test_win3.txt", "hello\n");
        next.restore_window(round_tripped.window, round_tripped.maximized);
        next.on_event(
            Event::WindowGeometry(WindowGeometry {
                x: 0,
                y: 0,
                width: 2560,
                height: 1400,
                maximized: true,
            }),
            &mut host,
        );
        assert_eq!(
            next.initial_window(),
            Some(WindowGeometry {
                x: 120,
                y: 80,
                width: 1100,
                height: 800,
                maximized: true,
            }),
            "a maximized first report must not lose the saved rect"
        );
    }

    #[test]
    fn a_stranded_scroll_drag_ends_on_the_next_press() {
        // Regression: the thumb follows the pointer for as long as a drag is
        // live, and only `MouseUp` ends it. A release the platform cannot
        // deliver — the pointer let go outside the client area, the window
        // deactivated mid-drag — used to leave the bar glued to the pointer,
        // scrolling on every move with no button held, until a later click
        // happened to free it. The click that starts the next gesture is where
        // that has to end, so a stranded drag can never outlive one press.
        let mut app = open("editor_test_stranded.txt", &"line\n".repeat(200));
        let mut host = NullHost::default();
        let _ = app.render_headless(800, 600, 1.0);
        let area = app.viewport();
        let g = app
            .preview
            .v_sb(area, app.scroll.offset, app.metrics())
            .unwrap();
        app.on_event(
            Event::MouseDown {
                x: g.thumb.x + 2.0,
                y: g.thumb.y + 4.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(
            app.sb_drag.is_some(),
            "setup: the press starts a thumb drag"
        );
        app.on_event(
            Event::MouseMove {
                x: g.thumb.x + 2.0,
                y: g.start + g.travel,
            },
            &mut host,
        );
        let dragged = app.scroll.offset;
        assert!(dragged > 100.0, "setup: the drag scrolls to the bottom");
        // The release never arrives. The user presses a line of text instead.
        app.on_event(
            Event::MouseDown {
                x: 40.0,
                y: 200.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(
            app.sb_drag.is_none(),
            "the next press must free a drag whose release was lost"
        );
        // ...and from there the pointer moves without touching the bar.
        app.on_event(
            Event::MouseMove {
                x: g.thumb.x + 2.0,
                y: g.start,
            },
            &mut host,
        );
        assert_eq!(
            app.scroll.offset, dragged,
            "a pointer with no button on it must not drag the bar"
        );
    }

    #[test]
    fn gutter_click_does_not_place_caret() {
        let mut app = open("editor_test_gut.txt", "hello world\nsecond line");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        let _ = app.render_headless(800, 600, 1.0);
        app.on_event(
            Event::KeyDown {
                key: Key::Right,
                mods: Modifiers::default(),
            },
            &mut host,
        );
        assert_eq!(app.editor.caret.col, 1);
        // A click inside the line-number rail must not move the caret.
        let y = app.viewport().y + 10.0;
        app.on_event(
            Event::MouseDown {
                x: 5.0,
                y,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert_eq!(app.editor.caret.col, 1, "gutter click must be ignored");
        // The pointer is an arrow over the rail, not an I-beam.
        assert_eq!(app.cursor_at(5.0, y), CursorShape::Arrow);
    }

    #[test]
    fn double_and_triple_click_select_word_and_line() {
        let mut app = open("editor_test_dbl.txt", "hello world\nsecond line");
        let mut host = NullHost::default();
        app.on_event(ctrl('e'), &mut host);
        let _ = app.render_headless(800, 600, 1.0);
        let ctx = app.ctx();
        let p = crate::layout::text::plain_params(800.0, &ctx, false);
        let rail = gutter::rail_w(&app.fonts, &ctx, app.buffer.line_count());
        // Middle of "world" (6.5 mono cells past the line start).
        let adv = app
            .fonts
            .advance(crate::gfx::font::FontStyle::Mono, 'o', p.size);
        let x = app.viewport().x + rail + p.x0 + 6.5 * adv;
        let y = app.viewport().y + p.pad + p.line_h / 2.0;
        let click = |app: &mut App, host: &mut NullHost| {
            app.on_event(
                Event::MouseDown {
                    x,
                    y,
                    button: MouseButton::Left,
                },
                host,
            );
        };
        click(&mut app, &mut host); // single: places the caret
        assert!(app.editor.selection().is_none());
        click(&mut app, &mut host); // double: selects the word
        assert_eq!(app.editor.sel_text(&app.buffer), "world");
        click(&mut app, &mut host); // triple: selects the whole line
        assert_eq!(app.editor.sel_text(&app.buffer), "hello world");
    }
}
