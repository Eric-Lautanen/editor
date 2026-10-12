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

// `--cfg readitor_check_win` (RUSTFLAGS) type-checks the Win32 backend from Linux/macOS: dev-only.
#[cfg(all(unix, not(target_os = "macos"), not(readitor_check_win)))]
mod linux;
#[cfg(all(target_os = "macos", not(readitor_check_win)))]
mod macos;
#[cfg(any(target_os = "windows", readitor_check_win))]
mod windows;

#[cfg(all(unix, not(target_os = "macos"), not(readitor_check_win)))]
use linux as backend;
#[cfg(all(target_os = "macos", not(readitor_check_win)))]
use macos as backend;
#[cfg(any(target_os = "windows", readitor_check_win))]
use windows as backend;

/// Services the app can ask of the host window.
/// Grows per milestone: save dialogs arrive with M6, macOS open-file with M9.
pub trait Host {
    fn request_redraw(&mut self);
    fn set_title(&mut self, title: &str);
    fn clipboard_get(&mut self) -> Option<String>;
    fn clipboard_set(&mut self, text: &str);
    /// Modal file-open dialog; `None` = cancelled/unsupported.
    fn open_file_dialog(&mut self) -> Option<std::path::PathBuf>;
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

/// The OS dark-mode preference right now (Windows: AppsUseLightTheme).
/// Other backends report `false` until they land (M8/M9).
pub fn system_prefers_dark() -> bool {
    #[cfg(any(target_os = "windows", readitor_check_win))]
    {
        windows::system_prefers_dark()
    }
    #[cfg(not(any(target_os = "windows", readitor_check_win)))]
    {
        false
    }
}

/// Print a line to stdout. On Windows the GUI subsystem detaches the
/// console, so the backend attaches to the parent console first.
pub fn print_line(s: &str) {
    #[cfg(any(target_os = "windows", readitor_check_win))]
    {
        windows::print_out(&format!("{s}\n"));
    }
    #[cfg(not(any(target_os = "windows", readitor_check_win)))]
    {
        println!("{s}");
    }
}

/// Print raw text (no trailing newline) to stdout; see `print_line`.
pub fn print_text(s: &str) {
    #[cfg(any(target_os = "windows", readitor_check_win))]
    {
        windows::print_out(s);
    }
    #[cfg(not(any(target_os = "windows", readitor_check_win)))]
    {
        print!("{s}");
    }
}

/// Print a line to stderr; see `print_line`.
pub fn print_err_line(s: &str) {
    #[cfg(any(target_os = "windows", readitor_check_win))]
    {
        windows::print_err(&format!("{s}\n"));
    }
    #[cfg(not(any(target_os = "windows", readitor_check_win)))]
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
    fn open_url(&mut self, url: &str) {
        self.opened_url = Some(url.to_string());
    }
    fn minimize(&mut self) {}
    fn toggle_maximize(&mut self) {}
    fn quit(&mut self) {
        self.quit_requested = true;
    }
}
