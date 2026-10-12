//! X11 backend via libX11 (system library; `#[link(name = "X11")]`). Works under Wayland via XWayland.
//! M8 plan: XOpenDisplay, XCreateSimpleWindow, XSelectInput, XPutImage with a ZPixmap XImage,
//! XNextEvent loop, Xutf8LookupString for text, WM_DELETE_WINDOW, clipboard via selections,
//! file dialog by shelling out to `zenity`/`kdialog` if present (std::process::Command).
use super::Handler;
use crate::error::{Error, Result};

pub fn run(_handler: Box<dyn Handler>) -> Result<()> {
    Err(Error::Platform("Linux backend not implemented yet (M8)"))
}
