use crate::{
    core::{
        buffer::{Buffer, Pos},
        file_kind::FileKind,
    },
    error::Result,
    gfx::{
        canvas::Canvas,
        color::Color,
        font::FontSystem,
        geom::{Point, Rect},
    },
    layout::{
        self,
        display_list::{DisplayList, DrawOp},
        theme::Theme,
        Ctx,
    },
    parse::{jsonl, markdown},
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
mod preview;
mod status;
mod tab;

use self::tab::*;

// R1 moved the widest-line measurement to `layout::text`, so production code
// no longer names `FontStyle` here; the test module (R1's three-path check
// among others) still does. Scoped to the test build so the bin stays clean.
#[cfg(test)]
use crate::gfx::font::FontStyle;

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
///
/// Pointer touch is a Windows capability, so this state (like the `Pan`
/// events that drive it) exists only in that build — see `Event`.
#[cfg(any(target_os = "windows", test))]
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

/// A double-tap's row anchor for a finger drag that extends the preview
/// selection: the counterpart of [`TouchSel`] for read-only panes. A finger
/// reports `Pan`, never `MouseMove`, so the drag rides its own virtual
/// pointer from the spot the tap landed. Gated exactly like [`TouchSel`].
#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Copy)]
struct PreviewTouch {
    /// Row the double-tap selected; the drag extends from it.
    anchor: usize,
    /// The finger's virtual window position: the tap's spot plus the pan
    /// deltas since.
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

/// During a streamed load, relayout the growing document only on every Nth
/// tick instead of every pump: even the fast band relayout is O(viewport)
/// per stroke, and a 40 ms cadence made a big load freeze at each tick.
/// Staggering keeps every stroke bounded; the sealing tick always relayouts.
const STREAM_RELAYOUT_TICKS: u32 = 5;

/// JSONL files at or below this many lines take the plain (full) layout; a
/// bigger one goes virtualized (per-record plan + visible-band paint). The
/// threshold is generous — a 4000-record display list is milliseconds — and
/// small files keep the byte-identical single-path behavior.
pub const VIRTUAL_JSONL_LINES: usize = 4_000;

/// Above this many lines a reader pane is laid out on a worker thread (the
/// fast band paints until it lands). Whole-document kind layouts are linear
/// but heavy — seconds on a big JSON or code file — and this is the point
/// where doing one inside a paint stops being a frame cost.
const PREVIEW_ASYNC_LINES: usize = 20_000;

/// Lines — or Markdown blocks — above which a reader pane is planned and
/// painted as a band. Deliberately the same order as the worker threshold:
/// below it a whole-document list is a few milliseconds and keeps the pane's
/// row index (selection, hover, copy); above it the op count must stop
/// growing with the file.
pub const VIRTUAL_UNITS: usize = 20_000;

/// How much of a big pane is laid out *now*, on the UI thread, so its syntax
/// colours appear with the seal rather than seconds later: a screenful and a
/// half of line-based text is a few milliseconds' work.
const PREVIEW_PREFIX_LINES: usize = 1_200;

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
    /// The OS asked for reduced motion (see [`App::set_reduce_motion`]).
    reduce_motion: bool,
    /// Full window-edge strip the platform claims for resizing (physical px).
    /// Overlay scrollbars are inset by the *bar-safe* share of it
    /// ([`App::bar_grip`]) so a press on the bar scrolls instead of starting a
    /// window resize; `grip_at` hands the rest of it back to the OS.
    edge: f32,
    zoom: f32,
    layout_dirty: bool,
    /// The plan threshold (see [`VIRTUAL_UNITS`]). A test lowers it so a
    /// small document takes the banded path; the app itself never changes it.
    virtual_units: usize,
    /// Line count above which a whole-document layout goes to the worker
    /// (see [`PREVIEW_ASYNC_LINES`]). A test lowers it to exercise that path
    /// without a 20 000-line fixture.
    async_lines: usize,
    /// How many times an edit patched a plan instead of rebuilding it. A test
    /// reads it to prove the incremental path is the one taken, not a
    /// correct rebuild.
    #[cfg(test)]
    plan_patches: u64,
    /// S2 · How many markdown bands were laid out fresh. A scroll inside one
    /// cached band's covered units reuses the ops and must not build; a test
    /// reads this counter to prove the cache is the path taken.
    #[cfg(test)]
    md_band_built: u64,
    /// F1 help overlay visible.
    help_open: bool,
    /// F2 about box visible.
    about_open: bool,
    /// Where the About box's update-toggle row is, refreshed by every paint
    /// of the box (hit-tested in `on_mouse_down`).
    about_toggle: Option<Rect>,
    /// The About box's "Check for updates now" button, same deal.
    about_check: Option<Rect>,
    /// The manual check's result, shared with its worker thread.
    update_check: crate::update::Status,
    /// The value of `update_check` the last frame painted; the tick compares
    /// the two and repaints when the worker has written a new answer. A
    /// message box used to be how that answer arrived, which cost a modal and
    /// the system alert sound for a "nothing new" result.
    update_check_seen: crate::update::Check,
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
    /// The status bar's error detail when it is somewhere to go — a JSON tree
    /// with a node it cannot describe. Rect refreshed on every paint,
    /// hit-tested on press; `None` whenever the detail is just a fact.
    status_detail: Option<Rect>,

    /// A press on the Copy button landed: its label reads "Copied" until the
    /// selection changes. The button is the feedback, so no second place on
    /// the bar has to repeat it.
    copy_done: bool,
    /// A big tab's reader layout running off the UI thread; the pane keeps
    /// painting the fast band until it lands.
    preview_job: Option<PreviewJob>,
    /// Whether big layouts may go to a worker. Off for `--render`/`--pretty`
    /// runs, which want the finished pane in the one frame they paint.
    async_layout: bool,
    /// Visual rows of the active tab's preview (for drag-selection).
    preview_rows: Vec<PreviewRow>,
    /// Preview selection: (anchor row, caret row).
    preview_sel: Option<(usize, usize)>,
    /// A double-click's word selection; exclusive with `preview_sel`.
    preview_word: Option<preview_view::WordSel>,
    /// Row a preview press landed on, so a drag can extend from it while a
    /// press that never moves stays a click (and clears, like the editor).
    preview_drag_row: Option<usize>,
    /// Left button held over the preview (drag-selecting).
    preview_drag: bool,
    /// A double-tap's row anchor for a finger drag extending the preview
    /// selection, mirroring [`TouchSel`] for the editor pane.
    #[cfg(any(target_os = "windows", test))]
    preview_touch: Option<PreviewTouch>,
    /// The address under the pointer, shown in the status bar after the Copy
    /// button. Kept as the string, not a bool: the bar needs the value.
    hover_link: Option<String>,
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
    /// A streamed load is in flight and keeps the fast frame clock up so the
    /// preview's load gauge rotates at frame cadence. Cleared on the seal
    /// (see `pump_stream` / `on_frame`).
    stream_animating: bool,
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
    #[cfg(any(target_os = "windows", test))]
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
    #[cfg(any(test, feature = "mem-census"))]
    pub fn new(path: Option<PathBuf>) -> Result<Self> {
        Self::new_streaming(path, false)
    }

    /// [`App::new`], with a `headless` switch a `--render`/`--pretty`/
    /// `--mem-report`/perf run sets: there is no tick heartbeat in a
    /// headless run, so a streamed load is drained inline — the caller
    /// wants a fully loaded document now, not an animation of one.
    pub fn new_streaming(path: Option<PathBuf>, headless: bool) -> Result<Self> {
        let tab = match &path {
            Some(p) => Tab::from_path_streaming(p.clone(), headless)?,
            None => Tab::untitled(),
        };
        // An Untitled buffer has nothing to preview: start in Edit mode.
        let mode = if tab.path.is_none() {
            Mode::Edit
        } else {
            Mode::Preview
        };
        let mut app = Self {
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
            // Off unless the OS says otherwise; `cli::run` reads the real
            // preference, and tests set it directly rather than shelling out.
            reduce_motion: false,
            edge: platform::edge_grip(),
            zoom: 1.0,
            layout_dirty: true,
            virtual_units: VIRTUAL_UNITS,
            async_lines: PREVIEW_ASYNC_LINES,
            #[cfg(test)]
            plan_patches: 0,
            #[cfg(test)]
            md_band_built: 0,
            help_open: false,
            about_open: false,
            about_toggle: None,
            update_check: crate::update::new_status(),
            update_check_seen: crate::update::Check::Idle,
            about_check: None,
            find: None,
            menu: None,
            close_armed: false,
            hover_title: None,
            status_buttons: Vec::new(),
            copy_btn: None,
            status_detail: None,
            copy_done: false,
            preview_job: None,
            async_layout: !headless,
            preview_rows: Vec::new(),
            preview_sel: None,
            preview_word: None,
            preview_drag_row: None,
            preview_drag: false,
            #[cfg(any(target_os = "windows", test))]
            preview_touch: None,
            hover_link: None,
            last_mouse: (0.0, 0.0),
            theme_manual: false,
            ticks: 0,
            blink: 0,
            window: None,
            window_maximized: false,
            auto_update: true,
            scrolling: false,
            stream_animating: false,
            recent: Vec::new(),
            last_frame: Instant::now(),
            geometry_dirty: false,
            geometry_idle: 0,
            sb_drag: None,
            #[cfg(any(target_os = "windows", test))]
            touch_sel: None,
            clicks: 0,
            last_click: None,
        };
        // C1 · An open with no worker (`--render`, the perf gate) plans a big
        // code pane here, at load: the checkpoint fill is whole-document work
        // that otherwise lands in the first paint, and this is where the
        // gate's own contract already puts parsing and layout.
        app.preplan_pane();
        Ok(app)
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
    /// Follow the OS accessibility preference for motion. Read once at
    /// startup by `cli::run` (see `platform::prefers_reduced_motion`); the
    /// scroller is told too, so wheel glides and release coasts stop animating
    /// while the distance they cover is unchanged.
    pub fn set_reduce_motion(&mut self, on: bool) {
        self.reduce_motion = on;
        self.scroll.reduce_motion = on;
    }

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

    /// Pick up the exit code a Windows update left behind and put it in the
    /// About box's status line — the one surface an update the user asked for
    /// has. Called once at startup by `cli`, not from `new`: the suite builds
    /// `App`s everywhere and none of them ran an installer.
    pub fn note_update_result(&mut self) {
        crate::update::note_install_result(&self.update_check);
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
        // Reduced motion: the horizontal ride is the same decoration as the
        // vertical one, so it lands at once instead of gliding there.
        if step.abs() >= GLIDE_MIN_STEP && !self.reduce_motion {
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
    /// vertically. `at` is where the gesture is happening when the
    /// input carries its own position (a finger's tracked contact), falling
    /// back to the tracked mouse pointer (a horizontal wheel / tilt) or to
    /// the editor (no pointer at all — keyboard-driven).
    #[cfg(any(target_os = "windows", test))]
    fn pan_horizontal_direct_at(&mut self, dx: f32, at: Option<(f32, f32)>) {
        if self.mode != Mode::Preview && self.over_editor_pane(at) {
            self.set_h_scroll(self.h_scroll - dx);
        }
    }

    /// Whether the locate point `at` (falling back to the tracked pointer)
    /// sits over the editor pane, in whichever mode is showing one.
    /// Gated with its only caller: outside Windows (and the tests) nothing
    /// locates a horizontal gesture, and dead helpers fail clippy there.
    #[cfg(any(target_os = "windows", test))]
    fn over_editor_pane(&self, at: Option<(f32, f32)>) -> bool {
        match at {
            Some((x, y)) => self.editor_area().contains(Point { x, y }),
            None => self.pointer_or_default_over_editor(),
        }
    }

    /// Whether the pointer is over the editor pane (or, when no pointer is
    /// tracked — keyboard arrows/time, a gesture carried without a mouse
    /// position — treat it as the editor, the pane horizontal scroll
    /// belongs to). The whole-length gate: Preview has no horizontal
    /// scroll of its own, and in Split the *pointer* decides which half a
    /// horizontal gesture drives, not the mode.
    #[cfg(any(target_os = "windows", test))]
    fn pointer_or_default_over_editor(&self) -> bool {
        if self.last_mouse == (0.0, 0.0) {
            return true;
        }
        self.editor_area().contains(Point {
            x: self.last_mouse.0,
            y: self.last_mouse.1,
        })
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
    #[cfg(any(target_os = "windows", test))]
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
        // The streaming gauge keeps the frame clock alive while a load runs:
        // it is what makes the spinner rotate at frame cadence instead of the
        // coalesced 40 ms heartbeat that read as "not working".
        if self.stream_animating {
            if self.streamer.is_some() {
                host.request_redraw();
            } else {
                self.stream_animating = false;
                if !self.scrolling {
                    host.set_animating(false);
                }
            }
        }
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
        if let Some(i) = self.tabs.iter().position(|t| {
            t.path
                .as_deref()
                .is_some_and(|open| Self::same_file(open, &path))
        }) {
            self.switch_to(i);
        } else {
            match Tab::from_path_streaming(path, false) {
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

    /// Whether two paths name the same file. Exact equality first (the common
    /// case, and it costs nothing), then the filesystem's own answer, because
    /// `C:\\x\\a.md`, `c:\\x\\.\\a.md` and a relative `a.md` are one file and
    /// used to open as three tabs — three buffers over one file, each saving
    /// over the others.
    fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
        if a == b {
            return true;
        }
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(x), Ok(y)) => x == y,
            // A path that does not resolve (missing, or a name being created)
            // is only the same file as itself, which the check above answered.
            _ => false,
        }
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

    /// The find bar's matches belong to the active buffer: re-run the search
    /// whenever the content, tab, or view changed (all of which dirty layout).
    /// A full rescan, not the incremental narrow — the buffer may have moved
    /// under the old match list. No jump — paint must not move the view.
    /// The find bar's matches belong to the active buffer: re-run the search
    /// only when the text (buffer revision) changed. A resize, zoom or mode
    /// switch leaves the text alone, so those just re-land on the caret; a
    /// full rescan there rescanned the whole document for no new matches.
    fn rescan_find(&mut self) {
        if let Some(mut f) = self.find.take() {
            let caret = self.tabs[self.active].editor.caret;
            let rev = self.tabs[self.active].buffer.revision();
            if f.is_current_for(rev) {
                f.reland(caret);
            } else {
                f.rescan(&self.tabs[self.active].buffer, caret);
            }
            self.find = Some(f);
        }
    }

    /// Buffer changed: live re-render the document pane and keep the caret on
    /// screen. (Split's top pane re-renders through `relayout` on paint.)
    fn after_edit(&mut self) {
        self.layout_dirty = true;
        // The edit log is read once, here: the dirty compare takes the
        // edit watermark from it (the smallest line any edit since the
        // save touched — kept in a field because the log is capped and
        // `install_preview` eats it), the touched rows grow the cached
        // widest extent, then the plan patch (if any) rebases from the
        // same spans — a later edit would shift them, so they must not
        // be consumed twice.
        let edits = self.tabs[self.active].buffer.take_edits();
        if let Some(edits) = &edits {
            let top = edits.iter().map(|e| e.start).min().unwrap_or(usize::MAX);
            let tab = &mut self.tabs[self.active];
            tab.edited_from = tab.edited_from.min(top);
        }
        // T5 · The find bar's list rides the same batch: matches below the
        // first change are searched again from there, the ones above shift,
        // and a patch that cannot be localized leaves the list stale — the
        // next `rescan_find` then falls back to the full scan.
        if let (Some(edits), Some(f)) = (&edits, self.find.as_mut()) {
            let _ = f.patch(
                &self.tabs[self.active].buffer,
                edits,
                self.tabs[self.active].editor.caret,
            );
        }
        self.sync_dirty();
        self.poke_caret();
        self.follow_caret();
        if self.mode != Mode::Preview {
            self.grow_widest_after_edit(edits.as_deref());
        }
        self.patch_plan_for_edit(edits);
    }

    /// T1 · Keep the cached widest row (`tab.widest`) growing as edits land:
    /// the touched rows are measured with `text::touched_extent` — the same
    /// two-part rule `widest_width` runs, scoped so a keystroke prices its
    /// own lines, not the document — and the cache takes the max. A deletion
    /// that removes the widest row leaves the stored extent larger than the
    /// text needs until the next width/zoom/scale change re-runs the
    /// whole-document scan, which is exactly `tab.widest`'s trade.
    fn grow_widest_after_edit(&mut self, edits: Option<&[crate::core::buffer::EditSpan]>) {
        let edit_w = self.editor_area().w;
        // From the fields, not `self.ctx()`: the tab borrow below must not
        // cover the fonts (same rule as the paint-site ctx assemblies).
        let ctx = Ctx {
            fonts: &self.fonts,
            theme: &self.theme,
            scale: self.scale,
            zoom: self.zoom,
        };
        let tab = &mut self.tabs[self.active];
        if tab.widest.1 != edit_w || tab.widest.2 != self.zoom || tab.widest.3 != self.scale {
            let e = layout::text::widest_width(&tab.buffer, edit_w, &ctx, false);
            tab.widest = (e, edit_w, self.zoom, self.scale);
        } else if let Some(edits) = edits {
            let e = layout::text::touched_extent(&tab.buffer, edits, edit_w, &ctx);
            if e > tab.widest.0 {
                tab.widest.0 = e;
            }
        }
    }

    /// Re-base a line pane's plan right after an edit, while the buffer is
    /// still the text that edit produced: the log's line spans are in these
    /// coordinates, and a later edit would shift them. Only the edited lines
    /// are measured, so a keystroke costs those rows, not the document.
    ///
    /// The pane's editor half and scroll range are left to the next paint
    /// (`relayout` sees the plan current and installs it without rebuilding).
    fn patch_plan_for_edit(&mut self, edits: Option<Vec<crate::core::buffer::EditSpan>>) {
        if self.mode == Mode::Edit {
            return;
        }
        let w = match self.mode {
            Mode::Split => self.split_panes().0.w,
            _ => self.size.0 as f32,
        };
        let ctx = Ctx {
            fonts: &self.fonts,
            theme: &self.theme,
            scale: self.scale,
            zoom: self.zoom,
        };
        let tab = &mut self.tabs[self.active];
        // A virtual JSONL holds its records and their index on the tab, not in
        // a `PanePlan`: the same patch, a different home, and one that is a
        // whole re-parse of the file if it is skipped.
        if tab.kind == FileKind::JsonLines {
            let active = Self::jsonl_plan_active(tab, w);
            let revision = tab.buffer.revision();
            let Some(edits) = edits else {
                return;
            };
            // T4 · The errored-record count is running: the patch reports
            // this replace’s exact delta, and the status formats it.
            let (ok, patch_delta) = if active {
                layout::json_tree::Plan::patch(
                    &mut tab.plan,
                    &mut tab.records,
                    &tab.buffer,
                    &tab.collapsed,
                    w,
                    &ctx,
                    &edits,
                )
            } else {
                (false, 0)
            };
            if ok {
                tab.records_errors = (tab.records_errors as isize + patch_delta) as usize;
            }
            if !ok {
                // The stale revision is what makes the next relayout rebuild —
                // or re-parse the document, if no rebuild can be patched.
                tab.buffer.forget_edits();
                return;
            }
            tab.records_rev = revision;
            tab.status_extra = record_count_status_counted(tab.records.len(), tab.records_errors);
            #[cfg(test)]
            {
                self.plan_patches += 1;
            }
            return;
        }
        // A virtual JSON keeps its tree, ranges and plan on the tab too. The
        // keystroke re-parses the one node it landed in; an edit that cannot be
        // localized (a half-typed key, a comma in the wrong place) drops the
        // log so the next relayout re-parses the document.
        if tab.kind == FileKind::Json {
            let active = Self::json_doc_active(tab, w);
            let revision = tab.buffer.revision();
            let Some(edits) = edits else {
                return;
            };
            let patched = active
                && match tab.json_doc.as_mut() {
                    Some((rev, doc)) => {
                        let ok = doc.patch(&tab.buffer, &edits, &tab.collapsed, w, &ctx);
                        if ok {
                            *rev = revision;
                        }
                        ok
                    }
                    None => false,
                };
            if !patched {
                tab.buffer.forget_edits();
                return;
            }
            tab.status_extra = json_status(&tab.json_doc);
            #[cfg(test)]
            {
                self.plan_patches += 1;
            }
            return;
        }
        let Some(edits) = edits else {
            return;
        };
        let revision = tab.buffer.revision();
        let kind = tab.kind;
        let ext = extension_of(tab.path.as_deref());
        // T4 · The log's status is tallied from the batch's own lines,
        // before any plan patching decides what reaches the screen:
        // the counts trail the text, not the pane.
        if kind == FileKind::Log {
            let e = Some(&edits[..]);
            log_status_for(tab, e);
        }
        // S1 · A CSV pane carries its swap out of the match: the plan borrow
        // must be over before `tab.pane_plan` can be replaced. The tuple is
        // the sheet and its fresh plan, in that order.
        let mut csv_fresh: Option<(crate::parse::csv::Sheet, layout::csv::CsvPlan)> = None;
        let Some(plan) = tab.pane_plan.as_mut() else {
            tab.buffer.forget_edits();
            return;
        };
        if tab.plan_width != w {
            tab.buffer.forget_edits();
            return;
        }
        let patched = match plan {
            // Plain text and tinted lines index buffer lines, so the edit's
            // spans rebase them directly.
            layout::PanePlan::Text(rows) => {
                layout::text::patch_lines(rows, &tab.buffer, w, &ctx, &edits)
            }
            layout::PanePlan::Lines(p) => {
                let hk = line_kind(kind).unwrap_or(layout::highlight::Kind::Plain);
                p.patch(&tab.buffer, w, &ctx, hk, &ext, &edits)
            }
            // T2 · A keystroke re-parses only the block run it landed in:
            // the splice runs here, at the edit, and the next paint finds
            // the plan current (`current_plan_height` answers without a
            // measure). An edit the splice cannot trust — a blank-gap split
            // it cannot prove, a reference definition in the range — keeps
            // the pre-T2 answer: the log is dropped and the paint-time path
            // builds or re-parses whole, deferring a big document to the
            // worker exactly as before.
            layout::PanePlan::Markdown(p) => match tab.markdown_doc.take() {
                Some((_, old)) => {
                    // T2 · The keystroke re-parses only the block run it
                    // landed in and the plan re-measures only that run's
                    // blocks; an edit the splice cannot trust keeps the
                    // pre-T2 answer: the log drops, the paint-time path
                    // re-parses whole, deferring a big document to the
                    // worker exactly as before.
                    let (doc, splice) =
                        crate::parse::markdown::block::patch_document(old, &tab.buffer, &edits);
                    match splice {
                        Some(run) => {
                            let ok = p.patch_run(&doc, w, &ctx, run);
                            tab.markdown_doc = Some((revision, doc));
                            ok
                        }
                        None => {
                            tab.markdown_doc = Some((revision, doc));
                            false
                        }
                    }
                }
                None => false,
            },
            // S1/T6 · The CSV keystroke reproduces the sheet: a re-parse of
            // the capped rows and one fresh plan, both inline — the same
            // answer the paint-time path takes (the patch counter wants the
            // plan to stay owned here, not re-owned by the worker).
            layout::PanePlan::Csv(_) => {
                let sheet = crate::parse::csv::parse_with_ext(&tab.buffer.text(), None);
                csv_fresh = Some({
                    let plan = layout::csv::plan(&sheet, w, &ctx);
                    (sheet, plan)
                });
                true
            }
        };
        if let Some((sheet, fresh)) = csv_fresh {
            tab.csv_sheet = Some((revision, sheet));
            tab.pane_plan = Some(layout::PanePlan::Csv(fresh));
        }
        if patched {
            tab.pane_rev = revision;
            #[cfg(test)]
            {
                self.plan_patches += 1;
            }
        } else {
            // The stale revision is what makes the next relayout rebuild.
            tab.buffer.forget_edits();
        }
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
        let dirty = !same_lines(
            &self.buffer,
            &self.saved_text,
            self.saved_lines,
            self.edited_from.min(self.buffer.line_count()),
        );
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
        // Any key can move the caret or the selection, so the Copy button's
        // "Copied" flash ends here; Ctrl+C below starts a new one.
        self.copy_done = false;
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
                    self.copy_done = true;
                } else if let Some(text) = self.editor.copy(&self.buffer) {
                    host.clipboard_set(&text);
                    self.copy_done = true;
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
        self.preview_drag_row = None;
        self.sb_drag = None;
        // A fresh press retires the previous one's touch state too: an armed
        // double-tap selection and the preview's row anchor.
        #[cfg(any(target_os = "windows", test))]
        {
            self.preview_touch = None;
            self.touch_sel = None;
        }
        // Anything that can change the selection retires the Copy button's
        // "Copied" flash; the Copy branch below re-arms it.
        self.copy_done = false;
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
            // The button runs the update flow on its own thread and answers
            // in the box, on a status line under itself — "up to date" is not
            // worth a modal, let alone the alert sound Windows plays for one.
            // The box stays open so the answer is visible where it was asked
            // for. Deliberately independent of the checkbox above it: asking
            // off the clock is the user's call even when the automatic check
            // is switched off, which is why `update::check_now` ignores that
            // setting (as `--update` does).
            if let Some(r) = self.about_check.filter(|r| r.contains(Point { x, y })) {
                let _ = r;
                crate::update::check_now(self.update_check.clone());
                host.request_redraw();
                return;
            }
            self.about_open = false;
            self.about_toggle = None;
            self.about_check = None;
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
                // The title bar's + is the status bar's Open button in the
                // tab row: same dialog, same open-as-a-new-tab result.
                TitleHit::NewTab => self.run_menu_action(menu::Action::Open, host),
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
            // The error detail goes to the node it names: the first one a JSON
            // tree cannot describe. It is the only part of the bar's left-hand
            // text that is a target, and only while that node exists.
            if self.status_detail.is_some_and(|r| r.contains(p)) {
                self.jump_to_broken_node(host);
                host.request_redraw();
                return;
            }
            // Copy first: it is the leftmost thing on the bar and only
            // exists while there is a selection to copy.
            if let Some(cb) = &self.copy_btn {
                if cb.rect.contains(p) {
                    if let Some(text) = self
                        .preview_selection_text()
                        .or_else(|| self.editor.copy(&self.buffer))
                    {
                        host.clipboard_set(&text);
                        // The button itself says so — it reads "Copied"
                        // until the selection changes.
                        self.copy_done = true;
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
                    #[cfg(any(target_os = "windows", test))]
                    {
                        self.arm_touch_sel(x, y);
                    }
                }
                3.. => {
                    let tab = &mut self.tabs[self.active];
                    tab.editor.select_line(&tab.buffer, pos);
                    self.drag_anchor = tab.editor.selection().map(|(a, _)| a);
                    #[cfg(any(target_os = "windows", test))]
                    {
                        self.arm_touch_sel(x, y);
                    }
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
        // Preview panes: links/toggles first, then the same click language as
        // the editor — one click clears, two take the word, three the row.
        // The row under the press is remembered either way, so a press that
        // turns into a drag still selects (and a press that does not stays a
        // click, leaving nothing selected).
        if !self.preview_click(x, y, host) {
            let point = self.toggle_point(x, y);
            let row = point.and_then(|(_, dy)| self.preview.row_at(&self.preview_rows, dy));
            match self.click_seq(x, y) {
                // Double-click: the word under the pointer, or its row when
                // there is no word there (a blank line, the space past the
                // end of the text) so the gesture always selects something.
                2 => {
                    let word = point.and_then(|(dx, dy)| {
                        self.preview
                            .word_at(&self.fonts, &self.preview_rows, dx, dy)
                    });
                    match (word, row) {
                        (Some(w), _) => {
                            self.preview_drag_row = Some(w.row);
                            self.preview_word = Some(w);
                            self.preview_sel = None;
                            self.preview_drag = true;
                        }
                        (None, Some(r)) => {
                            self.preview_word = None;
                            self.preview_sel = Some((r, r));
                            self.preview_drag_row = Some(r);
                            self.preview_drag = true;
                        }
                        (None, None) => self.clear_preview_sel(),
                    }
                }
                3.. => {
                    self.preview_word = None;
                    self.preview_sel = row.map(|r| (r, r));
                    self.preview_drag_row = row;
                    self.preview_drag = row.is_some();
                }
                _ => {
                    self.preview_word = None;
                    self.preview_sel = None;
                    self.preview_drag_row = row;
                    self.preview_drag = row.is_some();
                }
            }
            #[cfg(any(target_os = "windows", test))]
            if let Some(anchor) = self.preview_drag_row {
                // A double/triple tap is how a finger selects text; the drag
                // that may follow extends it. A single tap arms nothing.
                self.arm_preview_touch(x, y, anchor);
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
        // A link under the pointer is named in the status bar. The preview is
        // read-only, so hovering is the only way to see where one goes without
        // leaving the app; the address used to appear only after the click,
        // which is too late to decide whether to make it.
        let link = self.link_under(x, y);
        if self.hover_link != link {
            self.hover_link = link;
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
        // Preview drag: extend the row selection from the press's anchor. The
        // first move onto another row is what creates the selection, so a
        // press followed by a release (a click) selects nothing.
        if self.preview_drag {
            if let (Some(anchor), Some((_, dy))) = (self.preview_drag_row, self.toggle_point(x, y))
            {
                if let Some(row) = self.preview.row_at(&self.preview_rows, dy) {
                    if row != anchor || self.preview_sel.is_some() {
                        self.preview_sel = Some((anchor, row));
                        self.preview_word = None;
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
            self.preview_drag_row = None;
            self.sb_drag = None;
        }
        host.request_redraw();
    }

    /// The address of the link under a window point, if the pointer is over
    /// one in a preview pane.
    ///
    /// The pane's own box is checked first: in Split the editor sits under the
    /// preview, and in every mode the status and title bars sit outside it, so
    /// without that a link from the document's own coordinates could be
    /// reported while the pointer is nowhere near the text.
    fn link_under(&self, x: f32, y: f32) -> Option<String> {
        let pane = match self.mode {
            Mode::Preview => self.viewport(),
            Mode::Split => self.split_panes().0,
            Mode::Edit => return None,
        };
        if !pane.contains(Point { x, y }) {
            return None;
        }
        let (dx, dy) = self.toggle_point(x, y)?;
        match self.preview.action_at(dx, dy) {
            Some(crate::layout::display_list::HitAction::Link(href)) => Some(href),
            _ => None,
        }
    }

    /// Arm the double-tap selection for a finger drag (see [`TouchSel`]):
    /// called from the double/triple-click branches, which have just made the
    /// selection the drag would extend.
    #[cfg(any(target_os = "windows", test))]
    fn arm_touch_sel(&mut self, x: f32, y: f32) {
        if let Some(anchor) = self.editor.selection().map(|(a, _)| a) {
            self.touch_sel = Some(TouchSel {
                anchor,
                pointer: (x, y),
                active: false,
            });
        }
    }

    /// Arm the preview counterpart of [`TouchSel`]: a finger drag after a tap
    /// selects rows from `anchor` instead of scrolling.
    #[cfg(any(target_os = "windows", test))]
    fn arm_preview_touch(&mut self, x: f32, y: f32, anchor: usize) {
        self.preview_touch = Some(PreviewTouch {
            anchor,
            pointer: (x, y),
            active: false,
        });
    }

    /// A finger drag while an editor double-tap's selection is armed extends
    /// that selection instead of scrolling; otherwise it does the same for the
    /// preview. Returns `true` when the pan was consumed.
    #[cfg(any(target_os = "windows", test))]
    fn on_touch_pan(&mut self, dx: f32, dy: f32, host: &mut dyn Host) -> bool {
        // A drag extends a selection, so the Copy button's "Copied" flash is
        // no longer about the text that was copied.
        self.copy_done = false;
        if self.touch_sel.is_some() {
            return self.on_editor_touch_pan(dx, dy, host);
        }
        self.on_preview_touch_pan(dx, dy, host)
    }

    /// The editor half of [`App::on_touch_pan`]. The virtual pointer starts
    /// where the tap landed and rides the pan deltas — a moving finger reports
    /// `Pan`, never `MouseMove`.
    #[cfg(any(target_os = "windows", test))]
    fn on_editor_touch_pan(&mut self, dx: f32, dy: f32, host: &mut dyn Host) -> bool {
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

    /// The preview half of [`App::on_touch_pan`]: rows from the tap's anchor
    /// to the row under the virtual pointer. The same ~1 s recency gate as the
    /// editor keeps a drag that starts long after the tap a scroll.
    #[cfg(any(target_os = "windows", test))]
    fn on_preview_touch_pan(&mut self, dx: f32, dy: f32, host: &mut dyn Host) -> bool {
        let Some(mut pt) = self.preview_touch.take() else {
            return false;
        };
        if !pt.active {
            let recent = self
                .last_click
                .is_some_and(|(_, t)| self.ticks.wrapping_sub(t) <= 25);
            if !recent {
                return false;
            }
            pt.active = true;
        }
        pt.pointer.0 += dx;
        pt.pointer.1 += dy;
        let (px, py) = pt.pointer;
        if let Some((_, doc_y)) = self.toggle_point(px, py) {
            if let Some(row) = self.preview.row_at(&self.preview_rows, doc_y) {
                self.preview_word = None;
                self.preview_sel = Some((pt.anchor, row));
                self.preview_touch = Some(pt);
                host.request_redraw();
                return true;
            }
        }
        // Off the preview pane: keep the gesture, spend no delta.
        self.preview_touch = Some(pt);
        true
    }

    /// Host heartbeat: auto-scroll (and keep extending the selection) while a
    /// drag is held near a pane edge; every ~2 s, watch for on-disk changes.
    fn on_tick(&mut self, host: &mut dyn Host) {
        self.ticks = self.ticks.wrapping_add(1);
        // Pump an in-flight streamed load: one bounded chunk per tick (a few
        // milliseconds), so the window stays responsive the whole time the
        // file loads.
        self.pump_stream(host);
        // A big tab's reader layout runs on a worker; install it the moment
        // it lands, so the band gives way to the real pane mid-load.
        self.poll_preview_job(host);
        // The manual update check runs off the UI thread and writes its answer
        // into shared state; this is where that answer reaches the screen.
        // Only when it actually changed, so a 40 ms heartbeat costs a mutex
        // read and no repaint.
        let check = crate::update::current(&self.update_check);
        if check != self.update_check_seen {
            self.update_check_seen = check;
            host.request_redraw();
        }
        // Keep an in-flight glide moving even on a host that cannot run a
        // faster frame clock than this one; `Event::Frame` normally does it.
        if self.scrolling || self.stream_animating {
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
            if let (Some(anchor), Some((_, doc_y))) =
                (self.preview_drag_row, self.toggle_point(mx, my))
            {
                if let Some(row) = self.preview.row_at(&self.preview_rows, doc_y) {
                    self.preview_sel = Some((anchor, row));
                    self.preview_word = None;
                    host.request_redraw();
                }
            }
        }
    }

    fn on_text(&mut self, s: &str, host: &mut dyn Host) {
        if s.chars().any(char::is_control) {
            return;
        }
        self.copy_done = false;
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
            // Open in the OS default browser. The address is already on the
            // bar — hovering is what shows it — so the click has nothing to
            // report; the status bar is for what the pointer is on, not a
            // log of what was pressed.
            Some(crate::layout::display_list::HitAction::Link(href)) => {
                host.open_url(&href);
                host.request_redraw();
                true
            }
            None => false,
        }
    }

    /// Drop the preview selection (mode/tab changes, Escape, a plain click).
    fn clear_preview_sel(&mut self) {
        self.preview_sel = None;
        self.preview_word = None;
        self.preview_drag = false;
        self.preview_drag_row = None;
        #[cfg(any(target_os = "windows", test))]
        {
            self.preview_touch = None;
        }
    }

    /// Text of the preview selection, if any: a double-clicked word, or else
    /// the row range (anchor..caret, ordered).
    fn preview_selection_text(&self) -> Option<String> {
        if let Some(w) = &self.preview_word {
            return Some(w.text.clone());
        }
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

    /// Paint the preview selection over a preview pane: one word's span, or
    /// the rows of a drag.
    fn paint_preview_selection(&self, canvas: &mut Canvas, area: Rect, scroll: f32) {
        if let Some(w) = &self.preview_word {
            if let Some(row) = self.preview_rows.get(w.row) {
                canvas.set_clip(Some(area));
                canvas.fill_rect(
                    Rect::new(
                        w.x0 - 1.0,
                        row.top + area.y - scroll,
                        (w.x1 - w.x0) + 2.0,
                        row.bottom - row.top,
                    ),
                    self.theme.sel,
                );
                canvas.set_clip(None);
            }
            return;
        }
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
                self.mark_saved(&text);
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
                // Wholesale replace: the per-line log tally is wordless
                // about the new lines until its next edit re-seeds it.
                self.log_tally = None;
                let saved = self.buffer.text();
                self.mark_saved(&saved);
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

    /// JSONL preview: one headed tree per record, per-line error banners.
    fn layout_jsonl(
        buf: &Buffer,
        collapsed: &HashSet<usize>,
        status_extra: &mut String,
        w: f32,
        ctx: &Ctx,
    ) -> DisplayList {
        let records = jsonl::parse_parallel(&buf.text());
        *status_extra = record_count_status(&records);
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

    /// Whether a virtual JSONL pane — its records and the record index beside
    /// them — is the pane on screen: a big JSONL at the width and collapse set
    /// its index was measured for. A resize, a shrink below the threshold or a
    /// collapse toggle means the small-file or full-rebuild path instead.
    fn jsonl_plan_active(tab: &Tab, w: f32) -> bool {
        tab.kind == FileKind::JsonLines
            && tab.buffer.line_count() > VIRTUAL_JSONL_LINES
            && !tab.plan.slots.is_empty()
            && tab.records.len() == tab.plan.slots.len()
            && tab.plan_width == w
            && tab.plan_collapsed_gen == tab.collapsed_gen
    }

    /// Whether a virtual JSON tree — its nodes, their ranges and its plan — is
    /// the pane on screen at `w`. As with a JSONL, a width or collapse change
    /// means the plan is no longer the one being painted, so the small-file or
    /// full-rebuild path takes over.
    fn json_doc_active(tab: &Tab, w: f32) -> bool {
        tab.kind == FileKind::Json
            && tab.plan_width == w
            && tab.plan_collapsed_gen == tab.collapsed_gen
            && tab
                .json_doc
                .as_ref()
                .is_some_and(|(_, d)| d.spans.len() == d.plan.slots.len())
    }

    /// The height of the active tab's planned pane when the plan is *current* —
    /// the pane was patched for every edit since it was built, so a paint only
    /// installs it and never re-measures. `None` means the pane needs a patch
    /// or a rebuild.
    fn current_plan_height(&self, w: f32) -> Option<f32> {
        let tab = &self.tabs[self.active];
        let revision = tab.buffer.revision();
        if let Some(plan) = &tab.pane_plan {
            return (tab.plan_width == w && tab.pane_rev == revision).then(|| plan.height());
        }
        if let Some((rev, doc)) = &tab.json_doc {
            let current = *rev == revision
                && tab.plan_width == w
                && tab.plan_collapsed_gen == tab.collapsed_gen;
            return current.then_some(doc.plan.height);
        }
        (Self::jsonl_plan_active(tab, w) && tab.records_rev == revision).then_some(tab.plan.height)
    }

    /// Whether the active tab holds a plan for this width that the buffer has
    /// moved past — the one case a patch can fix without a whole-document
    /// measure.
    fn plan_needs_patch(&self, w: f32) -> bool {
        let tab = &self.tabs[self.active];
        if tab.pane_plan.is_some() {
            return tab.plan_width == w && tab.pane_rev != tab.buffer.revision();
        }
        if tab.kind == FileKind::Json {
            return Self::json_doc_active(tab, w)
                && tab
                    .json_doc
                    .as_ref()
                    .is_some_and(|(rev, _)| *rev != tab.buffer.revision());
        }
        Self::jsonl_plan_active(tab, w) && tab.records_rev != tab.buffer.revision()
    }

    /// The plan slot of the first node the active JSON tree cannot describe,
    /// when the bar offers a way to it. One walk of the tree, so only a press
    /// asks.
    fn json_error_slot(&self) -> Option<usize> {
        if !self.status_names_broken_json() {
            return None;
        }
        self.json_doc
            .as_ref()
            .and_then(|(_, d)| d.first_broken_slot())
    }

    /// Test/CLI helper: how many edits patched a plan in place.
    #[cfg(test)]
    pub fn plan_patches(&self) -> u64 {
        self.plan_patches
    }

    /// Test/CLI helper: how many markdown bands were laid out fresh.
    #[cfg(test)]
    pub fn md_band_built(&self) -> u64 {
        self.md_band_built
    }

    fn paint_panes(&mut self, canvas: &mut Canvas) {
        // A pending worker layout leaves the band on screen; a scroll that
        // leaves the rows it covers rebuilds it rather than painting blanks.
        if !self.layout_dirty && self.preview_pending() && self.mode != Mode::Edit {
            let top = self.preview_scroll();
            let view = self.viewport();
            let tab = &self.tabs[self.active];
            if top < tab.stream_top || top + view.h > tab.stream_bottom {
                self.layout_dirty = true;
            }
        }
        if self.layout_dirty {
            self.relayout();
        }
        // T1 · The editor pane's list is a band: a scroll (or caret follow)
        // that left its covered rows, or a width change since it was built,
        // rebuilds it next frame rather than painting blank lines. The
        // reader pane's equivalent is the `stream_top`/`stream_bottom` check
        // above.
        if !self.layout_dirty && self.editor_band_stale() {
            self.layout_dirty = true;
        }
        // A virtualized reader pane — a big JSONL's records, a big JSON's
        // tree — paints a band laid out fresh each frame (only the blocks at
        // the scroll offset's viewport exist in ops), so the PreviewView sees
        // a normal display list.
        // Not while the pane's document is still on the worker: the band path
        // would lay out from nothing and paint an empty pane, wiping the
        // highlighted prefix the relayout just built.
        // A plan from an earlier revision must never paint: the text it
        // indexes has changed. Rebuild the pane on the next relayout.
        // Not in Edit: that mode has no reader plan on screen, and the check
        // would relayout — and re-lay-out the whole editor pane — on every
        // frame of a tab edited after it was previewed.
        if self.mode != Mode::Edit
            && self.pane_plan.is_some()
            && self.pane_rev != self.buffer.revision()
        {
            self.layout_dirty = true;
        }
        // The same rule for a planned JSONL: its records and their index are a
        // pair, and a pane painted from a stale pair would show the records of
        // the text before the edit under the text after it. Not while the
        // pane's own layout is on the worker: the plan beside the records is
        // the one being replaced, and relayouting each frame would fight the
        // band that covers the wait.
        if self.mode != Mode::Edit
            && !self.preview_pending()
            && self.kind == FileKind::JsonLines
            && !self.plan.slots.is_empty()
            && self.records_rev != self.buffer.revision()
        {
            self.layout_dirty = true;
        }
        let banded = match self.kind {
            FileKind::JsonLines => self.buffer.line_count() > VIRTUAL_JSONL_LINES,
            // A tree the tab holds is painted in bands however few nodes it
            // has: a document that is being typed into can lose most of its
            // tree to one broken keystroke, and the pane must not drop the tree
            // (and re-parse on the next one) because of it. A small document
            // that was never big enough to keep is painted whole.
            FileKind::Json => self.json_doc.is_some(),
            // Every other kind that planned itself: line panes, Markdown,
            // and the S1 CSV sheet.
            _ => self.pane_plan.is_some(),
        } && self.mode != Mode::Edit
            && !self.preview_pending();
        let view = self.viewport();
        if banded {
            // The preview half (or the whole window) paints a fresh band each
            // frame; Split additionally shows the editor half, exactly as the
            // full-list path below does.
            let split = self.mode == Mode::Split;
            let pane = if split { self.split_panes().0 } else { view };
            // Preview pixels, like every other reader-pane path: Split shares
            // one progress between panes whose documents need not be the same
            // height, so `scroll.offset` there is in the taller document's
            // units and would paint a different slice than the scrollbar and
            // hit tests describe.
            let top = self.preview_scroll();
            let bottom = top + pane.h;
            // Split's preview pane is narrower than the window; the plan was
            // built for the last relayout width, so rebuild it once per mode
            // switch. A line plan counts rows again; a Markdown plan
            // re-measures from the document it kept, never a re-parse.
            if self.plan_width != pane.w {
                if self.pane_plan.is_some() {
                    self.replan_pane(pane.w);
                } else {
                    // Built from the fields rather than `self.ctx()`: the
                    // tree is re-planned in place, so the immutable borrow of
                    // the fonts must not cover the tree as well.
                    let ctx = Ctx {
                        fonts: &self.fonts,
                        theme: &self.theme,
                        scale: self.scale,
                        zoom: self.zoom,
                    };
                    match self.kind {
                        FileKind::JsonLines => {
                            self.plan = layout::json_tree::build_plan(
                                &self.records,
                                &self.collapsed,
                                pane.w,
                                &ctx,
                                true,
                            );
                        }
                        _ => {
                            // Through the tab, not `self`: `App` derefs to its
                            // active tab, so a mutable borrow of the tree would
                            // cover the fonts too.
                            let tab = &mut self.tabs[self.active];
                            if let Some((_, doc)) = &mut tab.json_doc {
                                doc.replan(&tab.collapsed, pane.w, &ctx);
                            }
                        }
                    }
                    self.plan_width = pane.w;
                }
            }
            // S2 · Markdown's band is the tab's cache when the scroll stays
            // inside what it covers; anything else builds fresh and becomes
            // that cache.
            let band = self.band_cached(pane.w, top, bottom);

            // The band is the tab's preview for this frame: storing it makes
            // toggle clicks and hit tests read the same thing the pane shows
            // (the band used to stay local here, so in Split the editor half
            // was never painted and toggle hits missed entirely).
            self.preview = PreviewView { list: band };
            self.preview.paint(
                canvas,
                &self.fonts,
                &self.theme,
                pane,
                PaneScroll::vertical(top),
                self.metrics(),
            );
            if split {
                let rail = gutter::rail_w(&self.fonts, &self.ctx(), self.buffer.line_count());
                let right_pane = self.split_panes().1;
                let editor = self.editor_scroll();
                self.editor_pane.paint(
                    canvas,
                    &self.fonts,
                    &self.theme,
                    right_pane,
                    PaneScroll::editor(editor, self.h_scroll, self.h_max, rail),
                    self.metrics(),
                );
                self.paint_editor_overlay(canvas, right_pane, editor);
                // The seam between the panes.
                canvas.fill_rect(self.split_divider(), self.theme.rule);
            }
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
                // A streamed load is still filling the buffer: name it with a
                // spinner in the pane's corner until the seal lands.
                if self.streamer.is_some() {
                    self.preview.paint_stream_gauge(
                        canvas,
                        &self.theme,
                        view,
                        self.ticks,
                        self.metrics(),
                    );
                }
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
                // Same spinner in Split: the reading half is what the wait
                // belongs to.
                if self.streamer.is_some() {
                    self.preview.paint_stream_gauge(
                        canvas,
                        &self.theme,
                        left,
                        self.ticks,
                        self.metrics(),
                    );
                }
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
}

/// 1-based visual column of a caret position (chars, not bytes).
fn char_col(buf: &Buffer, p: Pos) -> usize {
    buf.line(p.line)[..p.col].chars().count() + 1
}

/// T7 · The dirty compare, shaped like the line walk it replaced. It takes
/// the saved text's line count from the tab's cache, and starts at the
/// tab's edit watermark: a mutation only changes lines at and below
/// `EditSpan::start`, so everything above the smallest line any edit
/// since the load or save touched is byte-identical to the snapshot and
/// is not walked. `from == line count` (the just-saved state) walks
/// nothing; a mismatched count refuses without walking.
fn same_lines(buf: &Buffer, saved: &str, saved_lines: usize, from: usize) -> bool {
    // The count cache must hold what `split(\n)` would answer: the
    // snapshot's own line count, trailing newline included as one empty
    // last line, and both saved-\n shapes normalize at load.
    if buf.line_count() != saved_lines {
        return false;
    }
    let mut it = saved.split('\n').enumerate().skip(from.min(saved_lines));
    for i in from..buf.line_count() {
        let Some((_, text)) = it.next() else {
            return false;
        };
        if buf.line(i) != text.trim_end_matches('\r') {
            return false;
        }
    }
    true
}

/// The errored half of the record status: the part a keystroke keeps
/// running instead of recounting (see `record_count_status`).
pub(super) fn record_errors(records: &[jsonl::Record]) -> usize {
    records.iter().filter(|r| r.value.is_err()).count()
}

/// T4 · The record status straight from the running counters: no list
/// walk, only the edit's own delta having moved them.
pub(super) fn record_count_status_counted(count: usize, errors: usize) -> String {
    format_status_counts(count, errors)
}
/// "N records, M errors" for a parsed JSONL list — the scan the
/// rebuilds run (see `record_count_status_counted` for the running
/// form the keystrokes use).
fn record_count_status(records: &[jsonl::Record]) -> String {
    format_status_counts(records.len(), record_errors(records))
}

/// The status text from two numbers — the formatting the scan and the
/// running counter share verbatim.
fn format_status_counts(count: usize, errors: usize) -> String {
    if errors == 0 {
        format!("{} record{}", count, if count == 1 { "" } else { "s" })
    } else {
        format!(
            "{} record{}, {} error{}",
            count,
            if count == 1 { "" } else { "s" },
            errors,
            if errors == 1 { "" } else { "s" }
        )
    }
}

/// The lowercased extension of `path`, for the syntax highlighter ("" when
/// there is none).
fn extension_of(path: Option<&std::path::Path>) -> String {
    path.and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// The tint a line-based pane uses, or `None` for a kind with a renderer of
/// its own. Plain text is a line pane too: same grid, no tint. This mapping is
/// what makes the line family closed: a new text kind either names a tint here
/// and gets planning and banding, or returns `None` and must plan itself.
fn line_kind(kind: FileKind) -> Option<layout::highlight::Kind> {
    Some(match kind {
        FileKind::PlainText => layout::highlight::Kind::Plain,
        FileKind::Code => layout::highlight::Kind::Code,
        FileKind::Log => layout::highlight::Kind::Log,
        FileKind::Yaml => layout::highlight::Kind::Yaml,
        FileKind::Toml => layout::highlight::Kind::Toml,
        FileKind::Ini => layout::highlight::Kind::Ini,
        _ => return None,
    })
}

/// Severity codes to one byte per line, so the tally beside them prices
/// the scan window a few kilobytes and a keystroke nothing: the codes
/// are the old classification, extracted verbatim, so the full scan
/// and the edit delta can never disagree.
const LOG_NONE: u8 = 0;
const LOG_ERROR: u8 = 1;
const LOG_WARN: u8 = 2;

/// T4 · The running log tally: one severity byte per line and the
/// counts over exactly the lines the full scan reads. `log_counts`
/// seeds it; `log_status` after an edit prices the batch\'s own lines
/// instead of rescanning the window per keystroke.
const LOG_WINDOW: usize = 20_000;

fn log_level_of_line(line: &str) -> u8 {
    let mut words = line.split_whitespace();
    let Some(first) = words.next() else {
        return LOG_NONE;
    };
    // Skip a leading timestamp/bracket prefix the way the tint does.
    let word = first.trim_matches(|c: char| !c.is_alphanumeric());
    let level = words
        .next()
        .filter(|_| {
            word.chars()
                .all(|c| c.is_ascii_digit() || "-:,./TZ[]".contains(c))
                && word.len() >= 4
        })
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .unwrap_or(word);
    // Classification is borrowed and ASCII-case-insensitive: the old
    // `to_uppercase` was one heap allocation per scanned line.
    match layout::highlight::level_of(level) {
        layout::highlight::Level::Error => LOG_ERROR,
        layout::highlight::Level::Warn => LOG_WARN,
        _ => LOG_NONE,
    }
}

#[derive(Debug, PartialEq)]
pub struct LogTally {
    /// One severity byte per buffer line, in line order. Its length is
    /// always the line count of the text it describes: the tally runs
    /// one edit behind the buffer between batches, and the delta
    /// application checks that before trusting it.
    pub levels: Vec<u8>,
    errors: u32,
    warns: u32,
}

impl LogTally {
    /// The ground truth the tests and the fallbacks both use: a full
    /// scan of the document, counting the window the status quotes.
    fn scan(buf: &Buffer) -> Self {
        let count = buf.line_count();
        let window = count.min(LOG_WINDOW);
        let mut t = Self {
            levels: Vec::with_capacity(count),
            errors: 0,
            warns: 0,
        };
        for i in 0..count {
            let code = log_level_of_line(buf.line(i));
            t.levels.push(code);
            if i < window {
                t.counts_add(code);
            }
        }
        t
    }

    fn counts_add(&mut self, level: u8) {
        match level {
            LOG_ERROR => self.errors += 1,
            LOG_WARN => self.warns += 1,
            _ => {}
        }
    }

    fn counts_sub(&mut self, level: u8) {
        match level {
            LOG_ERROR => self.errors -= 1,
            LOG_WARN => self.warns -= 1,
            _ => {}
        }
    }

    /// The status line the counters earn. Empty when nothing matched,
    /// so ordinary logs keep a clean bar.
    fn status(&self) -> String {
        match (self.errors, self.warns) {
            (0, 0) => String::new(),
            (e, 0) => format!("{e} error{}", if e == 1 { "" } else { "s" }),
            (0, w) => format!("{w} warn{}", if w == 1 { "" } else { "s" }),
            (e, w) => format!("{e} errors · {w} warns"),
        }
    }

    /// T4 · One edit\'s delta. The lines the edit replaced are the
    /// only ones whose text changed, and the window\'s bottom edge
    /// slides with the edit\'s net line count, so four moves price
    /// all of it: the replaced range\'s old scores out (the per-line
    /// array holds them — the text is already gone), the replaced
    /// range\'s new scores in (from the text), and the edge\'s two
    /// moves — lines pushed past the window\'s end drop and the
    /// ones sliding up into it are scored. Both edge moves vanish when
    /// the counts match or the run eats the change; both are exact
    /// because a mutation only moves lines at and below its own range.
    fn apply_edit(&mut self, buf: &Buffer, e: &crate::core::buffer::EditSpan) {
        let prev_len = self.levels.len();
        let cur = buf.line_count();
        let d = e.inserted as isize - e.removed as isize;
        // The pre-edit record this delta prices must be the text the
        // edit landed on — the array\'s length and bound say so.
        if e.start + e.removed > prev_len {
            *self = Self::scan(buf);
            return;
        }
        let s = e.start;
        let t_old = e.start + e.removed;
        let t_new = (t_old as isize + d).max(0) as usize;
        let c_o = prev_len.min(LOG_WINDOW);
        let c_n = cur.min(LOG_WINDOW);
        // The replaced range\'s old scores, from the array.
        for j in s..t_old.min(c_o) {
            self.counts_sub(self.levels[j]);
        }
        // The window edge that slid out: leaves are old lines past the
        // run whose new index is past the window.
        if d > 0 {
            let lo = t_old.max((c_n as isize - d).max(0) as usize);
            for j in lo.min(c_o)..c_o {
                self.counts_sub(self.levels[j]);
            }
        }
        // The window edge that slid in with a shrink batch: fresh lines
        // from positions the old window never counted.
        if d < 0 {
            let lo = t_new.max((c_o as isize + d).max(0) as usize);
            for j in lo.min(c_n)..c_n {
                let code = log_level_of_line(buf.line(j));
                self.levels[j] = code;
                self.counts_add(code);
            }
        }
        // The replaced range\'s new scores, from the text, as its
        // fresh array entries; the tail slides in from the old array —
        // codes move, the sliding edge\'s counts answered above.
        let mut out = Vec::with_capacity(cur);
        out.extend_from_slice(&self.levels[..s]);
        for j in s..t_new {
            let code = if j < c_n {
                let code = log_level_of_line(buf.line(j));
                self.counts_add(code);
                code
            } else {
                LOG_NONE
            };
            out.push(code);
        }
        out.extend_from_slice(&self.levels[t_old..]);
        if out.len() != cur {
            // An arithmetic shape the deltas cannot describe: the scan
            // is the answer, and it costs what the per-keystroke scan
            // always paid.
            *self = Self::scan(buf);
            return;
        }
        self.levels = out;
    }
}

/// T4 · The shared tail of both log edit paths (the edit-time dispatch
/// just above and the paint-time patch in `app/preview.rs`). The tab
/// takes `tab` so a held tab borrow can still call it.
fn log_status_for(tab: &mut Tab, edits: Option<&[crate::core::buffer::EditSpan]>) {
    let cur = tab.buffer.line_count();
    let incremental = match (edits, tab.log_tally.as_mut()) {
        (Some([e]), Some(t)) => {
            let d = e.inserted as isize - e.removed as isize;
            (t.levels.len() as isize + d) as usize == cur && e.start + e.removed <= t.levels.len()
        }
        _ => false,
    };
    if incremental {
        let Tab {
            buffer, log_tally, ..
        } = tab;
        let t = log_tally.as_mut().unwrap();
        t.apply_edit(buffer, &edits.unwrap()[0]);
    } else {
        tab.log_tally = Some(LogTally::scan(&tab.buffer));
    }
    tab.status_extra = tab.log_tally.as_ref().unwrap().status();
}

/// Log status counts over at most the first 20k lines: a full 64 MB
/// scan on every relayout would jank the window for no reader benefit.
/// Empty when nothing matched, so ordinary logs keep a clean bar.
fn log_counts(buf: &Buffer) -> String {
    LogTally::scan(buf).status()
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
                // An identical resize is not a reflow. Platforms resend the
                // current geometry (a window manager can, and the headless
                // render helper reports it every frame), and re-laying the
                // document out for it would throw away real work — and the
                // reader's word selection — to arrive at the same layout.
                let same_geometry = self.size == (width, height) && self.scale == scale;
                self.size = (width, height);
                self.scale = scale;
                // The grip is a logical width; make it physical for this DPI. A
                // DPI change arrives as a resize, so it is re-read here rather
                // than only once at startup.
                self.edge = platform::edge_grip() * scale;
                if !same_geometry {
                    self.layout_dirty = true;
                }
                host.request_redraw();
            }
            Event::Scroll { dx, dy } => {
                self.pan_horizontal(dx, host);
                self.scroll_input(-dy, host);
            }
            #[cfg(any(target_os = "windows", test))]
            Event::Pan { dx, dy, x, y } => {
                // A double-tap's finger drag extends the selection the tap
                // made (the touch counterpart of double-click-and-drag);
                // every other finger is direct manipulation: follow it
                // exactly, no glide — and drop any glide that was in flight
                // (its remaining distance belongs to the wheel, not to the
                // finger). The contact's position rides along: it, not the
                // last mouse position, decides which pane the gesture drives.
                self.last_mouse = (x, y);
                if self.on_touch_pan(dx, dy, host) {
                    return;
                }
                self.pan_horizontal_direct_at(dx, Some((x, y)));
                self.scroll_direct(-dy, host);
            }
            #[cfg(any(target_os = "windows", test))]
            Event::PanEnd { vx, vy } => {
                // A selection drag is not a flick: it retires the gesture
                // without a coast. That goes for a preview selection too.
                let dragged = self.touch_sel.take().is_some();
                let preview_dragged = self.preview_touch.take().is_some();
                if dragged || preview_dragged {
                    host.request_redraw();
                    return;
                }
                self.coast(vx, vy, host);
            }
            Event::KeyDown { key, mods } => self.on_key(key, mods, host),
            #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos")), test))]
            Event::Frame => self.on_frame(host),
            Event::MouseDown { x, y, button } => self.on_mouse_down(x, y, button, host),
            Event::MouseMove { x, y } => self.on_mouse_move(x, y, host),
            Event::MouseUp { x, y, button } => self.on_mouse_up(x, y, button, host),
            Event::Text(s) => self.on_text(&s, host),
            #[cfg(any(target_os = "windows", target_os = "macos", test))]
            Event::FileOpen(path) => self.open_path(path, host),
            Event::Tick => self.on_tick(host),
            #[cfg(any(target_os = "windows", test))]
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
        // The address under the pointer is recomputed here rather than trusted
        // from the last mouse event: scrolling the page under a still pointer
        // moves the links, and the bar should describe what is under it now.
        self.hover_link = self.link_under(self.last_mouse.0, self.last_mouse.1);
        let (info, detail) = self.status_bar_text();
        // A detail that names a place to go is a target; everything else on
        // the bar's left is a fact. The hover comes from the previous frame's
        // rect, which is how the buttons' own hit testing works.
        let action = self.status_names_broken_json();
        let hover = self.status_detail.is_some_and(|r| {
            r.contains(Point {
                x: self.last_mouse.0,
                y: self.last_mouse.1,
            })
        });
        let mut buttons = self.status_buttons();
        let mut copy = self.copy_button();
        let bar = statusbar::Bar {
            info: &info,
            detail: &detail,
            link: self.hover_link.as_deref(),
            detail_action: action,
            detail_hover: action && hover,
        };
        let mut target = None;
        statusbar::paint(
            canvas,
            &self.fonts,
            &self.theme,
            sb,
            bar,
            statusbar::Targets {
                buttons: &mut buttons,
                copy: &mut copy,
                detail: &mut target,
            },
        );
        self.status_buttons = buttons;
        self.copy_btn = copy;
        self.status_detail = target;
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
            let (_f, toggle, check) = about::paint(
                canvas,
                &self.fonts,
                &self.theme,
                canvas.width as f32,
                canvas.height as f32,
                self.scale,
                about::Updates {
                    auto: self.auto_update,
                    check: &self.update_check_seen,
                },
            );
            self.about_toggle = Some(toggle);
            self.about_check = Some(check);
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
                || self.status_detail.is_some_and(|r| r.contains(p))
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
mod tests;
