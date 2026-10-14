//! The only module that talks to the OS. Everything else is portable.
use crate::{
    error::Result,
    gfx::canvas::Canvas,
    ui::{
        event::{CursorShape, Event, WindowGeometry},
        titlebar::TitleHit,
    },
};

pub mod install;

/// Default window size when the last session left no geometry: true 16:9.
/// One definition for every backend plus the app/CLI fallbacks, so the
/// default open size cannot drift per OS.
pub const DEFAULT_SIZE: (usize, usize) = (1280, 720);

// The `readitor_check_*` cfgs (RUSTFLAGS) type-check one backend from another
// OS: `readitor_check_win` the Win32 backend, `readitor_check_linux` the X11
// one, `readitor_check_macos` the AppKit one. Dev-only: they never link the
// foreign GUI libraries (which is also why a `cargo check`, not a build, is
// what they are for). Each selects *exactly* one module as `backend`, and the
// CI jobs for the other two OSes are what a local check stands in for.
//
// Anything every backend implements is reached through `backend::`, so the
// selection cannot drift from these cfgs. Where the set of backends differs
// (`print_*`) the arm has to name a module directly, and then its cfg must
// *imply* that module's cfg — the `windows::print_out` arms are gated on
// `any(target_os = "windows", readitor_check_win)` plus the other two check
// cfgs, which is exactly what compiles `mod windows` in the first place.
#[cfg(any(
    all(
        unix,
        not(target_os = "macos"),
        not(readitor_check_win),
        not(readitor_check_macos)
    ),
    readitor_check_linux
))]
mod linux;
#[cfg(all(
    any(target_os = "macos", readitor_check_macos),
    not(readitor_check_win),
    not(readitor_check_linux)
))]
mod macos;
#[cfg(any(target_os = "windows", readitor_check_win))]
mod windows;

#[cfg(any(
    all(
        unix,
        not(target_os = "macos"),
        not(readitor_check_win),
        not(readitor_check_macos)
    ),
    readitor_check_linux
))]
use linux as backend;
#[cfg(all(
    any(target_os = "macos", readitor_check_macos),
    not(readitor_check_win),
    not(readitor_check_linux)
))]
use macos as backend;
#[cfg(all(
    any(target_os = "windows", readitor_check_win),
    not(readitor_check_linux),
    not(readitor_check_macos)
))]
use windows as backend;

/// Services the app can ask of the host window.
/// Grows per milestone: macOS open/save dialogs arrive with M9.
pub trait Host {
    fn request_redraw(&mut self);
    fn set_title(&mut self, title: &str);
    fn clipboard_get(&mut self) -> Option<String>;
    fn clipboard_set(&mut self, text: &str);
    /// Modal file-open dialog; `None` = cancelled/unsupported.
    fn open_file_dialog(&mut self) -> Option<std::path::PathBuf>;
    /// Modal save-as dialog, pre-filled with `default_name` and opening in the
    /// user's Documents folder; `None` = cancelled/unsupported. The caller is
    /// responsible for writing the buffer to the returned path.
    fn save_file_dialog(&mut self, default_name: &str) -> Option<std::path::PathBuf>;
    /// Open `url` in the OS default browser (ShellExecute / xdg-open / open).
    fn open_url(&mut self, url: &str);
    fn minimize(&mut self);
    fn toggle_maximize(&mut self);
    fn quit(&mut self);

    /// Ask for animation frames ([`Event::Frame`]) while `on`, and to stop when
    /// `off`. The heartbeat is a ~25 Hz clock — fine for a caret blink, far too
    /// slow for a glide — so a platform that can run a faster one should, and
    /// only while something is actually moving.
    ///
    /// A platform that does not implement this is not broken: the app also
    /// advances animations from the heartbeat, just at 25 Hz.
    fn set_animating(&mut self, _on: bool) {}
}

/// Implemented by `App`.
pub trait Handler {
    fn on_event(&mut self, ev: Event, host: &mut dyn Host);
    fn paint(&mut self, canvas: &mut Canvas);
    fn title(&self) -> String;
    /// What lives at a client-space point in the custom title bar. The
    /// platform maps `Drag` to native window dragging; everything else stays
    /// a normal client click the app handles itself.
    fn title_hit(&self, x: f32, y: f32) -> TitleHit;
    /// Cursor shape for a client-space point (I-beam over text, hand over
    /// clickable things).
    fn cursor_at(&self, x: f32, y: f32) -> CursorShape;

    /// Where to open the window, asked once before it exists: the geometry the
    /// last session was closed at (see [`Event::WindowGeometry`]). `None` — the
    /// default for anything that never persists a window — means the platform
    /// places the window itself. A rect this returns is a *request*: the
    /// backend still checks it against the screens it actually has, because a
    /// rect saved on a monitor that is no longer attached would otherwise open
    /// off-screen where it cannot be dragged back.
    fn initial_window(&self) -> Option<WindowGeometry> {
        None
    }

    /// Resize strip (physical px) the platform may claim at client point
    /// `(x, y)`, given its own band `band`. Backends call this from their edge
    /// hit test so app-drawn things that live flush against a border —
    /// overlay scrollbars, window buttons — can hand the strip back and keep
    /// their pixels. Returning less than `band` makes that edge less
    /// resize-friendly, so only the points that need it should shrink it. The
    /// default claims the whole band, which is right for anything without
    /// content at the edge.
    fn grip_at(&self, _x: f32, _y: f32, band: f32) -> f32 {
        band
    }
}

/// Open the window and run the event loop until quit.
pub fn run(handler: impl Handler + 'static) -> Result<()> {
    backend::run(Box::new(handler))
}

/// Physical px of a window's top strip that a restored rect is judged by: the
/// strip a borderless window is dragged from, so a remembered position that
/// leaves none of it on a screen would open a window the user cannot move.
const REACHABLE_TOP: i32 = 32;
/// How much of that strip must be on a screen: enough to aim at and drag.
const REACHABLE_MIN_W: i32 = 64;
const REACHABLE_MIN_H: i32 = 8;
/// Sizes below this are treated as garbage rather than as a preference.
const REACHABLE_MIN_SIZE: u32 = 64;

/// The rect to create the window at: the one the session asked for, when the
/// screens we have now can still show it, else `default` — the backend's own
/// placement. The per-OS parts are therefore only the default and the screen it
/// is measured against; the rule for *using* what was asked for is shared, so
/// all three backends open a restored session the same way.
///
/// The maximized flag is deliberately not part of the answer: a window opens at
/// its normal rect and is maximized after, which is what keeps the rect to
/// restore at intact (see [`Handler::initial_window`]).
pub(crate) fn open_at(
    asked: Option<WindowGeometry>,
    screen: (i32, i32, i32, i32),
    default: (i32, i32, i32, i32),
) -> (i32, i32, i32, i32) {
    match asked {
        Some(g) if title_reachable(g.x, g.y, g.width, g.height, screen) => {
            (g.x, g.y, g.width as i32, g.height as i32)
        }
        _ => default,
    }
}

/// Is a remembered window rect usable on the screens available now?
///
/// `screen` is the whole desktop in physical px `(x, y, w, h)`: on Windows the
/// virtual screen of every monitor, on X11 the root window's size. Shared by
/// the backends because the rule is about *the user being able to reach the
/// window*, not about any one OS — only the screen it is measured against
/// differs. A rect that fails this is dropped and the platform falls back to
/// its own placement.
pub(crate) fn title_reachable(
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    screen: (i32, i32, i32, i32),
) -> bool {
    if w < REACHABLE_MIN_SIZE || h < REACHABLE_MIN_SIZE {
        return false;
    }
    let (sx, sy, sw, sh) = screen;
    if sw <= 0 || sh <= 0 {
        return false;
    }
    let strip = REACHABLE_TOP.min(h as i32);
    let overlap_w = (x + w as i32).min(sx + sw) - x.max(sx);
    let overlap_h = (y + strip).min(sy + sh) - y.max(sy);
    overlap_w >= REACHABLE_MIN_W.min(w as i32) && overlap_h >= REACHABLE_MIN_H.min(strip)
}

/// Thickness (logical 96-dpi px) of the widest strip along each window border
/// that the platform reserves for interactive resizing — the maximum it offers,
/// scaled by the caller to physical px. On a borderless window the client area
/// reaches the very edge, so app-drawn overlay scrollbars stop at this strip or
/// a press on the bar starts a window resize instead; `Handler::grip_at` narrows
/// it to just the points where a bar needs the pixels, so the edge stays as
/// easy to grab as the platform allows everywhere else. `0.0` where the backend
/// has no edge grip.
pub fn edge_grip() -> f32 {
    backend::edge_grip()
}

/// The OS dark-mode preference right now (Windows: AppsUseLightTheme,
/// Linux: gsettings color-scheme, macOS: `AppleInterfaceStyle`).
pub fn system_prefers_dark() -> bool {
    backend::system_prefers_dark()
}

/// Map `path` read-only into the process and hand back its bytes for the life
/// of the process, or `None` where the backend cannot (a zero-length file, a
/// missing path, an unsupported platform) — callers fall back to reading it.
///
/// This exists for the *fallback fonts*. A document with one CJK character
/// otherwise pulls a ~20 MB font file into private memory and keeps it there
/// for the whole run, though only a fraction of a megabyte of tables and
/// glyphs is ever read. A mapping keeps every untouched page out of the
/// working set, lets the pages that *are* read be shared with any other
/// process using the same font and evicted under memory pressure, and costs
/// one address-range reservation — the same trade the native text stacks make.
///
/// The bytes are immutable for the process: the backend holds the file open
/// and never unmaps, so mutating the file underneath would be a bug there.
pub fn map_file(path: &std::path::Path) -> Option<&'static [u8]> {
    backend::map_file(path)
}

/// Print a line to stdout. On Windows the GUI subsystem detaches the
/// console, so the backend attaches to the parent console first.
pub fn print_line(s: &str) {
    #[cfg(all(
        any(target_os = "windows", readitor_check_win),
        not(readitor_check_linux),
        not(readitor_check_macos)
    ))]
    {
        windows::print_out(&format!("{s}\n"));
    }
    #[cfg(not(all(
        any(target_os = "windows", readitor_check_win),
        not(readitor_check_linux),
        not(readitor_check_macos)
    )))]
    {
        println!("{s}");
    }
}

/// Print raw text (no trailing newline) to stdout; see `print_line`.
pub fn print_text(s: &str) {
    #[cfg(all(
        any(target_os = "windows", readitor_check_win),
        not(readitor_check_linux),
        not(readitor_check_macos)
    ))]
    {
        windows::print_out(s);
    }
    #[cfg(not(all(
        any(target_os = "windows", readitor_check_win),
        not(readitor_check_linux),
        not(readitor_check_macos)
    )))]
    {
        print!("{s}");
    }
}

/// Print a line to stderr; see `print_line`.
pub fn print_err_line(s: &str) {
    #[cfg(all(
        any(target_os = "windows", readitor_check_win),
        not(readitor_check_linux),
        not(readitor_check_macos)
    ))]
    {
        windows::print_err(&format!("{s}\n"));
    }
    #[cfg(not(all(
        any(target_os = "windows", readitor_check_win),
        not(readitor_check_linux),
        not(readitor_check_macos)
    )))]
    {
        eprintln!("{s}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The desktop `title_reachable` is measured against in these tests: two
    /// 1920×1080 monitors side by side.
    fn desktop() -> (i32, i32, i32, i32) {
        (0, 0, 3840, 1080)
    }

    #[test]
    fn a_remembered_rect_on_a_real_screen_is_kept() {
        assert!(title_reachable(100, 100, 1100, 800, desktop()));
        // Its right edge runs past the desktop and its title bar is half above
        // the top edge — still grabbable, so still kept: a window open at the
        // top-left corner is as far as this rule goes.
        assert!(title_reachable(3200, -20, 1100, 800, desktop()));
        assert!(title_reachable(-40, 0, 1100, 800, desktop()));
    }

    #[test]
    fn a_rect_off_every_screen_falls_back() {
        // Saved on a monitor that is no longer attached (or left of the
        // desktop, as a negative-origin layout can do): nothing to grab, so
        // the platform must place the window itself.
        assert!(!title_reachable(-1200, 100, 1100, 800, desktop()));
        assert!(!title_reachable(100, -900, 1100, 800, desktop()));
        assert!(!title_reachable(100, 5000, 1100, 800, desktop()));
        assert!(!title_reachable(4000, 100, 1100, 800, desktop()));
        // Barely overlapping isn't enough to aim at and drag.
        assert!(!title_reachable(-1080, 100, 1100, 800, desktop()));
    }

    #[test]
    fn a_restore_uses_the_saved_rect_only_when_it_is_still_showable() {
        let default = (10, 20, 1100, 800);
        // Hanging off the left edge of the desktop is ordinary (a monitor to
        // the left of the primary one) and still readable.
        let saved = WindowGeometry {
            x: -40,
            y: 60,
            width: 1100,
            height: 800,
            maximized: false,
        };
        assert_eq!(
            open_at(Some(saved), desktop(), default),
            (-40, 60, 1100, 800)
        );
        // A first run has nothing saved, and neither has a session whose
        // monitor is gone: both take the backend's placement.
        assert_eq!(open_at(None, desktop(), default), default);
        let gone = WindowGeometry {
            x: -3000,
            y: 60,
            ..saved
        };
        assert_eq!(open_at(Some(gone), desktop(), default), default);
        // A maximized session still opens at the normal rect (then maximizes).
        let maxed = WindowGeometry {
            maximized: true,
            ..saved
        };
        assert_eq!(
            open_at(Some(maxed), desktop(), default),
            (-40, 60, 1100, 800)
        );
    }

    #[test]
    fn a_degenerate_rect_falls_back() {
        assert!(!title_reachable(100, 100, 0, 800, desktop()));
        assert!(!title_reachable(100, 100, 1100, 0, desktop()));
        assert!(!title_reachable(100, 100, 10, 10, desktop()));
        // No screen at all (a display server that reports nothing).
        assert!(!title_reachable(100, 100, 1100, 800, (0, 0, 0, 0)));
    }

    #[test]
    fn a_small_window_is_judged_by_its_own_extent() {
        // The 64 px grab needs are capped by the window itself, so a 200 px
        // wide window that is fully on screen is not rejected for being
        // narrower than the minimum grab width.
        assert!(title_reachable(0, 0, 64, 64, desktop()));
        assert!(title_reachable(0, 0, 100, 80, desktop()));
    }
}

/// Host that does nothing — except hold clipboard text so copy/paste paths
/// are testable headlessly.
#[derive(Default)]
pub struct NullHost {
    pub quit_requested: bool,
    /// Last value passed to `set_animating`, so tests can see the app ask for
    /// frames (and stop asking for them).
    pub animating: bool,
    /// How many times it was asked, to catch a caller re-arming every frame.
    pub animate_calls: usize,
    pub clipboard: Option<String>,
    /// Returned by `open_file_dialog`; tests set it to simulate a pick.
    pub picked_file: Option<std::path::PathBuf>,
    /// Returned by `save_file_dialog`; tests set it to simulate a Save-As pick.
    pub save_file: Option<std::path::PathBuf>,
    /// Last URL passed to `open_url` (tests assert on it).
    pub opened_url: Option<String>,
}

impl Host for NullHost {
    fn request_redraw(&mut self) {}
    fn set_title(&mut self, _title: &str) {}
    fn clipboard_get(&mut self) -> Option<String> {
        self.clipboard.clone()
    }
    fn clipboard_set(&mut self, text: &str) {
        self.clipboard = Some(text.to_string());
    }
    fn open_file_dialog(&mut self) -> Option<std::path::PathBuf> {
        self.picked_file.take()
    }
    fn save_file_dialog(&mut self, _default_name: &str) -> Option<std::path::PathBuf> {
        self.save_file.take()
    }
    fn open_url(&mut self, url: &str) {
        self.opened_url = Some(url.to_string());
    }
    fn minimize(&mut self) {}
    fn toggle_maximize(&mut self) {}
    fn quit(&mut self) {
        self.quit_requested = true;
    }
    fn set_animating(&mut self, on: bool) {
        self.animating = on;
        self.animate_calls += 1;
    }
}
