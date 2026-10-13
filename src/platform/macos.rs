//! macOS backend via the Objective-C runtime (`libobjc`) + AppKit/CoreGraphics. FFI only.
//! M9 plan: objc_getClass/sel_registerName/objc_msgSend (cast per call signature),
//! NSView subclass via objc_allocateClassPair with drawRect:/keyDown:/scrollWheel:,
//! draw Canvas via CGBitmapContextCreate + CGContextDrawImage, NSApplication delegate for
//! application:openFile:, NSOpenPanel/NSSavePanel, NSPasteboard.
use super::Handler;
use crate::error::{Error, Result};

pub fn run(_handler: Box<dyn Handler>) -> Result<()> {
    Err(Error::Platform("macOS backend not implemented yet (M9)"))
}

/// No edge grip yet: AppKit is unimplemented (M9) and the portable scrollbar
/// geometry treats `0.0` as "no resize band to dodge". When the backend lands
/// this becomes the width of the borderless window's resize strip, mirroring
/// the Win32/X11 values, so `App` insets overlay scrollbars out of it.
pub fn edge_grip() -> f32 {
    0.0
}
