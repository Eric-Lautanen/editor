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
