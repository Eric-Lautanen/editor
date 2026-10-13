//! The only module that talks to the OS. Everything else is portable.
use crate::{
    error::Result,
    gfx::canvas::Canvas,
    ui::{
        event::{CursorShape, Event},
        titlebar::TitleHit,
    },
};

pub mod install;

// `--cfg readitor_check_win` (RUSTFLAGS) type-checks the Win32 backend from
// Linux/macOS; `--cfg readitor_check_linux` type-checks the X11 backend from
// Windows/macOS: dev-only, neither links the foreign GUI library.
#[cfg(any(
    all(unix, not(target_os = "macos"), not(readitor_check_win)),
    readitor_check_linux
))]
mod linux;
#[cfg(all(target_os = "macos", not(readitor_check_win)))]
mod macos;
#[cfg(any(target_os = "windows", readitor_check_win))]
mod windows;

#[cfg(any(
    all(unix, not(target_os = "macos"), not(readitor_check_win)),
    readitor_check_linux
))]
use linux as backend;
#[cfg(all(target_os = "macos", not(readitor_check_win)))]
use macos as backend;
#[cfg(all(
    any(target_os = "windows", readitor_check_win),
    not(readitor_check_linux)
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
}

/// Open the window and run the event loop until quit.
pub fn run(handler: impl Handler + 'static) -> Result<()> {
    backend::run(Box::new(handler))
}

/// Thickness (logical 96-dpi px) of the strip along each window border that
/// the platform reserves for interactive resizing. The caller scales it by the
/// window DPI to get physical px. On a borderless window the client area
/// reaches the very edge, so app-drawn overlay scrollbars stop at this strip or
/// a press on the bar starts a window resize instead. `0.0` where the backend
/// has no edge grip.
pub fn edge_grip() -> f32 {
    backend::edge_grip()
}

/// The OS dark-mode preference right now (Windows: AppsUseLightTheme,
/// Linux: gsettings color-scheme). Other backends report `false` (M9).
pub fn system_prefers_dark() -> bool {
    #[cfg(all(
        any(target_os = "windows", readitor_check_win),
        not(readitor_check_linux)
    ))]
    {
        windows::system_prefers_dark()
    }
    #[cfg(any(
        all(unix, not(target_os = "macos"), not(readitor_check_win)),
        readitor_check_linux
    ))]
    {
        linux::system_prefers_dark()
    }
    #[cfg(all(
        target_os = "macos",
        not(readitor_check_win),
        not(readitor_check_linux)
    ))]
    {
        false
    }
}

/// Print a line to stdout. On Windows the GUI subsystem detaches the
/// console, so the backend attaches to the parent console first.
pub fn print_line(s: &str) {
    #[cfg(all(
        any(target_os = "windows", readitor_check_win),
        not(readitor_check_linux)
    ))]
    {
        windows::print_out(&format!("{s}\n"));
    }
    #[cfg(not(all(
        any(target_os = "windows", readitor_check_win),
        not(readitor_check_linux)
    )))]
    {
        println!("{s}");
    }
}

/// Print raw text (no trailing newline) to stdout; see `print_line`.
pub fn print_text(s: &str) {
    #[cfg(all(
        any(target_os = "windows", readitor_check_win),
        not(readitor_check_linux)
    ))]
    {
        windows::print_out(s);
    }
    #[cfg(not(all(
        any(target_os = "windows", readitor_check_win),
        not(readitor_check_linux)
    )))]
    {
        print!("{s}");
    }
}

/// Print a line to stderr; see `print_line`.
pub fn print_err_line(s: &str) {
    #[cfg(all(
        any(target_os = "windows", readitor_check_win),
        not(readitor_check_linux)
    ))]
    {
        windows::print_err(&format!("{s}\n"));
    }
    #[cfg(not(all(
        any(target_os = "windows", readitor_check_win),
        not(readitor_check_linux)
    )))]
    {
        eprintln!("{s}");
    }
}

/// Host that does nothing — except hold clipboard text so copy/paste paths
/// are testable headlessly.
#[derive(Default)]
pub struct NullHost {
    pub quit_requested: bool,
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
}
