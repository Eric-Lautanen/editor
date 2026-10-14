//! macOS backend via the Objective-C runtime (`libobjc`) + AppKit +
//! CoreGraphics. FFI only, no crates: `objc_msgSend` is declared once and
//! transmuted to the exact signature each call site needs (the standard
//! no-crate objc pattern).
//!
//! M9 scope: borderless window (`NSWindowStyleMaskBorderless`) with the
//! app-drawn title bar (manual move + edge resize, like the X11 backend),
//! an `NSView` subclass for drawing and events (`drawRect:`, `keyDown:`,
//! mouse, `scrollWheel:`, Finder drag-drop), canvas blit via `CGImageCreate`
//! + `CGContextDrawImage` (the view is flipped so the image lands upright),
//! `NSPasteboard` clipboard, `NSOpenPanel`/`NSSavePanel` dialogs, `NSTimer`
//! (40 ms) for `Event::Tick`, `NSCursor` arrow/IBeam/hand, dock icon from
//! the portable icon renderer, dark preference via `defaults read -g
//! AppleInterfaceStyle`, `open` for links, `mmap(MAP_PRIVATE)` font mapping.
//! Not yet: IME/`NSTextInputClient` (`characters` covers Latin-1 and
//! composed keys), live theme-change notification (no KVO on the
//! appearance), and the scroll direction/multipliers want a real-wheel pass.
use super::Handler;
use crate::{
    error::{Error, Result},
    gfx::canvas::Canvas,
    platform::Host,
    ui::{
        event::{CursorShape, Event, Key, Modifiers, MouseButton, WindowGeometry},
        titlebar::TitleHit,
    },
};
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    ptr::{null, null_mut},
};

// ---------------------------------------------------------------- FFI

/// An Objective-C object (`id`), class, or CF type — all opaque pointers.
type Id = *mut c_void;
/// A selector (`SEL`).
type Sel = *const c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NSPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NSSize {
    w: f64,
    h: f64,
}

/// `NSRect` / `CGRect` (layout-identical; `CGFloat` is `f64` on 64-bit).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NSRect {
    origin: NSPoint,
    size: NSSize,
}

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    /// Variadic in reality; every call site transmutes it to the exact
    /// signature it needs (see `msg`).
    fn objc_msgSend();
    /// x86_64 only: struct returns larger than 16 bytes (`NSRect`) go
    /// through the stret variant; arm64 returns everything via
    /// `objc_msgSend`.
    #[cfg(target_arch = "x86_64")]
    fn objc_msgSend_stret();
    fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra: usize) -> Id;
    fn class_addMethod(cls: Id, name: Sel, imp: unsafe extern "C" fn(), types: *const c_char)
        -> i8;
    fn objc_registerClassPair(cls: Id);
}

// CoreGraphics: the canvas blit and screen geometry.
#[link(name = "CoreGraphics")]
extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> NSRect;
    fn CGColorSpaceCreateDeviceRGB() -> Id;
    fn CGDataProviderCreateWithData(
        info: *mut c_void,
        data: *const c_void,
        size: usize,
        release: *const c_void,
    ) -> Id;
    fn CGDataProviderRelease(provider: Id);
    fn CGImageCreate(
        width: usize,
        height: usize,
        bits_per_component: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        space: Id,
        info: u32,
        provider: Id,
        decode: *const f64,
        interpolate: bool,
        intent: u32,
    ) -> Id;
    fn CGImageRelease(image: Id);
    fn CGContextDrawImage(ctx: Id, rect: NSRect, image: Id);
}

// libSystem, for the font mapping only (`font::fallback` maps system fonts
// rather than reading them, see `platform::map_file`). No libc crate: one
// link attribute and the two calls needed.
#[link(name = "System")]
extern "C" {
    fn open(path: *const c_char, flags: c_int) -> c_int;
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        off: i64,
    ) -> *mut c_void;
}

// AppKit/Foundation are reached only through `objc_msgSend`, but the
// frameworks must be *loaded* for `objc_getClass` to find their classes.
#[link(name = "AppKit")]
extern "C" {}

// ------------------------------------------------------- objc call helpers

/// `[obj cmd]` with no arguments. `objc_msgSend` is variadic in truth; the
/// transmute gives it the exact C signature of the method being called,
/// which is how every objc binding without a crate does it.
unsafe fn msg<R>(obj: Id, cmd: Sel) -> R {
    let f: unsafe extern "C" fn(Id, Sel) -> R =
        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    f(obj, cmd)
}

unsafe fn msg1<A, R>(obj: Id, cmd: Sel, a: A) -> R {
    let f: unsafe extern "C" fn(Id, Sel, A) -> R =
        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    f(obj, cmd, a)
}

unsafe fn msg2<A, B, R>(obj: Id, cmd: Sel, a: A, b: B) -> R {
    let f: unsafe extern "C" fn(Id, Sel, A, B) -> R =
        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    f(obj, cmd, a, b)
}

unsafe fn msg4<A, B, C, D, R>(obj: Id, cmd: Sel, a: A, b: B, c: C, d: D) -> R {
    let f: unsafe extern "C" fn(Id, Sel, A, B, C, D) -> R =
        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    f(obj, cmd, a, b, c, d)
}

unsafe fn msg5<A, B, C, D, E, R>(obj: Id, cmd: Sel, a: A, b: B, c: C, d: D, e: E) -> R {
    let f: unsafe extern "C" fn(Id, Sel, A, B, C, D, E) -> R =
        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    f(obj, cmd, a, b, c, d, e)
}

/// `[obj cmd]` returning an `NSRect` — `objc_msgSend_stret` on x86_64
/// (32-byte struct), plain `objc_msgSend` on arm64.
#[cfg(target_arch = "x86_64")]
unsafe fn msg_rect(obj: Id, cmd: Sel) -> NSRect {
    let f: unsafe extern "C" fn(*mut NSRect, Id, Sel) =
        std::mem::transmute(objc_msgSend_stret as unsafe extern "C" fn());
    let mut r = NSRect::default();
    f(&mut r, obj, cmd);
    r
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn msg_rect(obj: Id, cmd: Sel) -> NSRect {
    msg(obj, cmd)
}

/// `sel_registerName` over a stack buffer (selectors are interned by the
/// runtime, so this is a lookup after the first call).
fn sel(name: &str) -> Sel {
    let mut buf = [0u8; 80];
    buf[..name.len()].copy_from_slice(name.as_bytes());
    unsafe { sel_registerName(buf.as_ptr() as *const c_char) }
}

fn class(name: &str) -> Id {
    let mut buf = [0u8; 64];
    buf[..name.len()].copy_from_slice(name.as_bytes());
    unsafe { objc_getClass(buf.as_ptr() as *const c_char) }
}

/// An `NSString` from UTF-8 (autoreleased; the run loop drains the pool).
unsafe fn ns_str(s: &str) -> Id {
    let mut b = s.as_bytes().to_vec();
    b.push(0);
    msg1::<*const c_char, Id>(
        class("NSString"),
        sel("stringWithUTF8String:"),
        b.as_ptr() as *const c_char,
    )
}

/// The UTF-8 contents of an `NSString` (empty for nil).
unsafe fn str_from_ns(s: Id) -> String {
    let p = msg::<*const c_char>(s, sel("UTF8String"));
    if p.is_null() {
        return String::new();
    }
    CStr::from_ptr(p).to_string_lossy().into_owned()
}

// ---------------------------------------------------------------- consts

// NSEvent modifier flags.
const NS_SHIFT: u64 = 1 << 17;
const NS_CONTROL: u64 = 1 << 18;
const NS_ALTERNATE: u64 = 1 << 19;
const NS_COMMAND: u64 = 1 << 20;

// kVK_* keycodes (HID usage values).
const VK_ENTER: u64 = 36;
const VK_BACKSPACE: u64 = 51;
const VK_FORWARD_DELETE: u64 = 117;
const VK_TAB: u64 = 48;
const VK_ESCAPE: u64 = 53;
const VK_HOME: u64 = 115;
const VK_END: u64 = 119;
const VK_LEFT: u64 = 123;
const VK_RIGHT: u64 = 124;
const VK_DOWN: u64 = 125;
const VK_UP: u64 = 126;
const VK_PAGE_UP: u64 = 116;
const VK_PAGE_DOWN: u64 = 121;
/// kVK_F1..kVK_F12, in F-key order.
const FKEYS: [u64; 12] = [122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111];

/// `kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little`: the
/// canvas's `0xAARRGGBB` `u32`s are BGRA bytes in memory, which is exactly
/// this CG layout.
const BITMAP_INFO: u32 = (3 << 12) | 2;

/// `NSPasteboardTypeString`.
const PASTEBOARD_TEXT: &str = "public.utf8-plain-text";
/// `NSFilenamesPboardType` — Finder drags still populate it.
const DRAG_TYPE: &str = "NSFilenamesPboardType";

const WIN_W: f64 = super::DEFAULT_SIZE.0 as f64;
const WIN_H: f64 = super::DEFAULT_SIZE.1 as f64;
/// Widest manual edge-resize band (logical px), matching the X11 backend.
const RESIZE_BAND: f32 = 6.0;
const MIN_W: f64 = 200.0;
const MIN_H: f64 = 100.0;
const TICK_SECS: f64 = 0.04;

// ---------------------------------------------------------------- state

/// Everything the backend owns. One `Box<Mac>` is leaked for the process
/// lifetime and reached through the `MAC` static: AppKit delivers every
/// callback on the main thread, which is the thread `run` is called on, so
/// there is exactly one accessor and no races.
struct Mac {
    handler: Box<dyn Handler>,
    canvas: Canvas,
    app: Id,
    win: Id,
    view: Id,
    colorspace: Id,
    /// arrow, ibeam, hand
    cursors: [Id; 3],
    last_title: String,
    /// Set by `request_redraw`; `drawRect:` paints when it sees it.
    dirty: bool,
    /// Backing-store factor (2.0 on retina): canvas px = points × scale.
    scale: f32,
    /// Current cursor index, to avoid re-setting the same one.
    cursor_idx: usize,
    /// Live manual move: (press mouse screen loc, press window origin).
    move_drag: Option<(NSPoint, NSPoint)>,
    /// Live manual resize: (press mouse loc, press frame, edge flags).
    size_drag: Option<(NSPoint, NSRect, u8)>,
    /// True while maximized to the visible screen: edge presses must not
    /// start a manual resize then.
    maximized: bool,
    /// Frame to restore after un-maximizing.
    pre_max: NSRect,
}

static mut MAC: *mut Mac = null_mut();

unsafe fn state() -> &'static mut Mac {
    &mut *MAC
}

/// Deliver an event to the app and sync the window title afterwards.
unsafe fn dispatch(ev: Event) {
    let m = state();
    let mut host = MacHost;
    m.handler.on_event(ev, &mut host);
    let title = m.handler.title();
    if title != m.last_title {
        m.last_title = title.clone();
        host.set_title(&title);
    }
}

// ---------------------------------------------------------------- host

struct MacHost;

impl Host for MacHost {
    fn request_redraw(&mut self) {
        unsafe {
            let m = state();
            m.dirty = true;
            msg1::<i8, ()>(m.view, sel("setNeedsDisplay:"), 1);
        }
    }
    fn set_title(&mut self, title: &str) {
        unsafe {
            msg1::<Id, ()>(state().win, sel("setTitle:"), ns_str(title));
        }
    }
    fn clipboard_get(&mut self) -> Option<String> {
        unsafe {
            let pb = msg::<Id>(class("NSPasteboard"), sel("generalPasteboard"));
            if pb.is_null() {
                return None;
            }
            let s = msg1::<Id, Id>(pb, sel("stringForType:"), ns_str(PASTEBOARD_TEXT));
            if s.is_null() {
                None
            } else {
                Some(str_from_ns(s))
            }
        }
    }
    fn clipboard_set(&mut self, text: &str) {
        unsafe {
            let pb = msg::<Id>(class("NSPasteboard"), sel("generalPasteboard"));
            if pb.is_null() {
                return;
            }
            msg::<()>(pb, sel("clearContents"));
            msg2::<Id, Id, ()>(
                pb,
                sel("setString:forType:"),
                ns_str(text),
                ns_str(PASTEBOARD_TEXT),
            );
        }
    }
    fn open_file_dialog(&mut self) -> Option<std::path::PathBuf> {
        unsafe {
            let panel = msg::<Id>(class("NSOpenPanel"), sel("openPanel"));
            if panel.is_null() {
                return None;
            }
            msg1::<i8, ()>(panel, sel("setCanChooseFiles:"), 1);
            msg1::<i8, ()>(panel, sel("setAllowsMultipleSelection:"), 0);
            // Same claimed set as the app and Info.plist, derived from
            // EXTENSIONS so the dialog cannot drift (see `core::file_kind`).
            let types = msg::<Id>(class("NSMutableArray"), sel("array"));
            if !types.is_null() {
                for ext in crate::core::file_kind::EXTENSIONS {
                    msg1::<Id, ()>(types, sel("addObject:"), ns_str(ext));
                }
                msg1::<Id, ()>(panel, sel("setAllowedFileTypes:"), types);
            }
            if msg::<i64>(panel, sel("runModal")) != 1 {
                return None;
            }
            url_path(msg::<Id>(msg::<Id>(panel, sel("URLs")), sel("firstObject")))
        }
    }
    fn save_file_dialog(&mut self, default_name: &str) -> Option<std::path::PathBuf> {
        unsafe {
            let panel = msg::<Id>(class("NSSavePanel"), sel("savePanel"));
            if panel.is_null() {
                return None;
            }
            // The panel opens in the user's Documents folder by default.
            msg1::<Id, ()>(panel, sel("setNameFieldStringValue:"), ns_str(default_name));
            if msg::<i64>(panel, sel("runModal")) != 1 {
                return None;
            }
            url_path(msg::<Id>(panel, sel("URL")))
        }
    }
    fn open_url(&mut self, url: &str) {
        let _ = std::process::Command::new("open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    fn minimize(&mut self) {
        unsafe {
            msg1::<Id, ()>(state().win, sel("miniaturize:"), null_mut());
        }
    }
    fn toggle_maximize(&mut self) {
        unsafe {
            let m = state();
            if m.maximized {
                m.maximized = false;
                msg2::<NSRect, i8, ()>(m.win, sel("setFrame:display:"), m.pre_max, 1);
            } else {
                maximize_to_screen(m);
            }
            report_geometry();
        }
    }
    fn quit(&mut self) {
        unsafe {
            msg1::<Id, ()>(state().app, sel("terminate:"), null_mut());
        }
    }
}

// ---------------------------------------------------------------- helpers

/// kVK keycode → app key. Printable keys go through `characters` instead.
fn key_of(code: u64) -> Option<Key> {
    Some(match code {
        VK_ENTER => Key::Enter,
        VK_BACKSPACE => Key::Backspace,
        VK_FORWARD_DELETE => Key::Delete,
        VK_TAB => Key::Tab,
        VK_ESCAPE => Key::Escape,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_DOWN => Key::Down,
        VK_UP => Key::Up,
        VK_PAGE_UP => Key::PageUp,
        VK_PAGE_DOWN => Key::PageDown,
        c if FKEYS.contains(&c) => {
            Key::F(FKEYS.iter().position(|&k| k == c).unwrap_or(0) as u8 + 1)
        }
        _ => return None,
    })
}

fn mods_of(flags: u64) -> Modifiers {
    Modifiers {
        // Command is the macOS shortcut modifier and the app's chords are
        // Ctrl-keyed, so both Command and Control land on `ctrl`.
        ctrl: flags & (NS_CONTROL | NS_COMMAND) != 0,
        alt: flags & NS_ALTERNATE != 0,
        shift: flags & NS_SHIFT != 0,
    }
}

/// Edge-resize bits for a press at canvas `(x, y)`: bit0 left, bit1 right,
/// bit2 top, bit3 bottom.
fn edge_flags(x: f32, y: f32, w: f32, h: f32, band: f32) -> u8 {
    let mut e = 0u8;
    if x < band {
        e |= 1;
    }
    if x > w - band {
        e |= 2;
    }
    if y < band {
        e |= 4;
    }
    if y > h - band {
        e |= 8;
    }
    e
}

/// The frame a manual edge drag produces. `dx`/`dy` are the global pointer
/// deltas in AppKit's bottom-left coordinates, so the top edge grows the
/// height and the bottom edge moves the origin.
fn resized_frame(frame: NSRect, dx: f64, dy: f64, edges: u8) -> NSRect {
    let mut r = frame;
    if edges & 1 != 0 {
        r.origin.x += dx;
        r.size.w -= dx;
    }
    if edges & 2 != 0 {
        r.size.w += dx;
    }
    if edges & 4 != 0 {
        r.size.h += dy;
    }
    if edges & 8 != 0 {
        r.origin.y += dy;
        r.size.h -= dy;
    }
    r
}

/// An event's window point → canvas px: scale by the backing factor and
/// flip y (AppKit windows are bottom-left origin, the canvas is top-down).
unsafe fn canvas_xy(ev: Id) -> (f32, f32) {
    let m = state();
    let loc = msg::<NSPoint>(ev, sel("locationInWindow"));
    let h_pt = msg_rect(m.view, sel("bounds")).size.h;
    (
        (loc.x * m.scale as f64) as f32,
        ((h_pt - loc.y) * m.scale as f64) as f32,
    )
}

/// The global mouse position (screen coords, bottom-left origin).
unsafe fn mouse_location() -> NSPoint {
    msg::<NSPoint>(class("NSEvent"), sel("mouseLocation"))
}

/// Follow the pointer with the cursor the app asks for.
unsafe fn update_cursor(x: f32, y: f32) {
    let m = state();
    let idx = match m.handler.cursor_at(x, y) {
        CursorShape::Arrow => 0,
        CursorShape::IBeam => 1,
        CursorShape::Hand => 2,
    };
    if idx != m.cursor_idx {
        m.cursor_idx = idx;
        msg::<()>(m.cursors[idx], sel("set"));
    }
}

/// `NSURL` → path; `None` for a nil URL.
unsafe fn url_path(url: Id) -> Option<std::path::PathBuf> {
    if url.is_null() {
        return None;
    }
    let p = msg::<Id>(url, sel("path"));
    if p.is_null() {
        return None;
    }
    Some(std::path::PathBuf::from(str_from_ns(p)))
}

/// The primary screen minus the menu bar and dock (`NSScreen visibleFrame`),
/// falling back to the raw display bounds.
unsafe fn screen_visible() -> NSRect {
    let s = msg::<Id>(class("NSScreen"), sel("mainScreen"));
    if !s.is_null() {
        let v = msg_rect(s, sel("visibleFrame"));
        if v.size.w > 0.0 && v.size.h > 0.0 {
            return v;
        }
    }
    CGDisplayBounds(CGMainDisplayID())
}

/// Maximize to the visible screen, remembering the rect to restore at.
unsafe fn maximize_to_screen(m: &mut Mac) {
    m.pre_max = msg_rect(m.win, sel("frame"));
    m.maximized = true;
    msg2::<NSRect, i8, ()>(m.win, sel("setFrame:display:"), screen_visible(), 1);
}

/// Tell the app where the window is and whether it is maximized — the AppKit
/// counterpart of the Win32/X11 geometry reports. Only this backend moves the
/// window (title-bar drag, edge resize, maximize), so each of those paths
/// reports; the frame is AppKit's bottom-left points, the app wants top-left
/// physical px.
unsafe fn report_geometry() {
    let m = state();
    let f = msg_rect(m.win, sel("frame"));
    let dh = CGDisplayBounds(CGMainDisplayID()).size.h;
    dispatch(Event::WindowGeometry(WindowGeometry {
        x: (f.origin.x * m.scale as f64) as i32,
        y: ((dh - f.origin.y - f.size.h) * m.scale as f64) as i32,
        width: (f.size.w * m.scale as f64) as u32,
        height: (f.size.h * m.scale as f64) as u32,
        maximized: m.maximized,
    }));
}

/// Wrap the canvas buffer in a `CGImage` and draw it over the whole view.

/// Wrap the canvas buffer in a `CGImage` and draw it over the whole view.
/// The view is flipped, so the image lands upright, row 0 at the top.
unsafe fn blit(m: &Mac, w_pt: f64, h_pt: f64) {
    let (w, h) = (m.canvas.width, m.canvas.height);
    if w == 0 || h == 0 {
        return;
    }
    let ctx_obj = msg::<Id>(class("NSGraphicsContext"), sel("currentContext"));
    if ctx_obj.is_null() {
        return;
    }
    let ctx = msg::<Id>(ctx_obj, sel("CGContext"));
    if ctx.is_null() {
        return;
    }
    let provider = CGDataProviderCreateWithData(
        null_mut(),
        m.canvas.pixels.as_ptr() as *const c_void,
        m.canvas.pixels.len() * 4,
        null_mut(),
    );
    if provider.is_null() {
        return;
    }
    let img = CGImageCreate(
        w,
        h,
        8,
        32,
        w * 4,
        m.colorspace,
        BITMAP_INFO,
        provider,
        null(),
        false,
        0,
    );
    if !img.is_null() {
        CGContextDrawImage(
            ctx,
            NSRect {
                origin: NSPoint { x: 0.0, y: 0.0 },
                size: NSSize { w: w_pt, h: h_pt },
            },
            img,
        );
        CGImageRelease(img);
    }
    CGDataProviderRelease(provider);
}

// ---------------------------------------------------------------- callbacks

unsafe extern "C" fn view_is_flipped(_: Id, _: Sel) -> i8 {
    // Top-left origin: matches the canvas, and `CGContextDrawImage` then
    // blits upright instead of mirrored.
    1
}

unsafe extern "C" fn view_accepts_first_responder(_: Id, _: Sel) -> i8 {
    1
}

unsafe extern "C" fn view_draw_rect(_: Id, _: Sel, _: NSRect) {
    let m = state();
    let bounds = msg_rect(m.view, sel("bounds"));
    let w = ((bounds.size.w * m.scale as f64).round().max(1.0)) as usize;
    let h = ((bounds.size.h * m.scale as f64).round().max(1.0)) as usize;
    if w != m.canvas.width || h != m.canvas.height {
        m.canvas.resize(w, h);
        dispatch(Event::Resized {
            width: w,
            height: h,
            scale: m.scale,
        });
    }
    if m.dirty {
        m.dirty = false;
        m.handler.paint(&mut m.canvas);
    }
    blit(m, bounds.size.w.max(1.0), bounds.size.h.max(1.0));
}

unsafe extern "C" fn view_key_down(_: Id, _: Sel, ev: Id) {
    let mods = mods_of(msg::<u64>(ev, sel("modifierFlags")));
    if let Some(key) = key_of(msg::<u64>(ev, sel("keyCode"))) {
        dispatch(Event::KeyDown { key, mods });
        return;
    }
    let chars = msg::<Id>(ev, sel("characters"));
    if chars.is_null() {
        return;
    }
    let text = str_from_ns(chars);
    if let Some(c) = text.chars().next() {
        if !c.is_control() {
            dispatch(Event::KeyDown {
                key: Key::Char(c),
                mods,
            });
            if !mods.ctrl && !mods.alt {
                dispatch(Event::Text(text));
            }
        }
    }
}

unsafe extern "C" fn view_mouse_down(_: Id, _: Sel, ev: Id) {
    let m = state();
    let (x, y) = canvas_xy(ev);
    let hit = m.handler.title_hit(x, y);
    // App-drawn controls (window buttons, tabs) sit flush with the border,
    // so they win over the resize band; only filler falls through to it.
    let control = !matches!(hit, TitleHit::Drag | TitleHit::Client);
    let edges = if control || m.maximized {
        0
    } else {
        // The band is the backend's maximum; the app hands back the pixels
        // its overlay scrollbars draw on (see `Handler::grip_at`).
        let band = m.handler.grip_at(x, y, RESIZE_BAND * m.scale).round();
        edge_flags(x, y, m.canvas.width as f32, m.canvas.height as f32, band)
    };
    if edges != 0 {
        m.size_drag = Some((mouse_location(), msg_rect(m.win, sel("frame")), edges));
        return;
    }
    if matches!(hit, TitleHit::Drag) {
        m.move_drag = Some((mouse_location(), msg_rect(m.win, sel("frame")).origin));
        return;
    }
    dispatch(Event::MouseDown {
        x,
        y,
        button: MouseButton::Left,
    });
}

unsafe extern "C" fn view_mouse_up(_: Id, _: Sel, ev: Id) {
    let m = state();
    m.move_drag = None;
    m.size_drag = None;
    let (x, y) = canvas_xy(ev);
    dispatch(Event::MouseUp {
        x,
        y,
        button: MouseButton::Left,
    });
}

unsafe extern "C" fn view_mouse_dragged(_: Id, _: Sel, ev: Id) {
    let m = state();
    let now = mouse_location();
    if let Some((press, origin)) = m.move_drag {
        msg1::<NSPoint, ()>(
            m.win,
            sel("setFrameOrigin:"),
            NSPoint {
                x: origin.x + now.x - press.x,
                y: origin.y + now.y - press.y,
            },
        );
        report_geometry();
        return;
    }
    if let Some((press, frame, edges)) = m.size_drag {
        let r = resized_frame(frame, now.x - press.x, now.y - press.y, edges);
        if r.size.w >= MIN_W && r.size.h >= MIN_H {
            msg2::<NSRect, i8, ()>(m.win, sel("setFrame:display:"), r, 1);
            report_geometry();
        }
        return;
    }
    let (x, y) = canvas_xy(ev);
    dispatch(Event::MouseMove { x, y });
    update_cursor(x, y);
}

unsafe extern "C" fn view_mouse_moved(_: Id, _: Sel, ev: Id) {
    let (x, y) = canvas_xy(ev);
    dispatch(Event::MouseMove { x, y });
    update_cursor(x, y);
}

unsafe extern "C" fn view_right_mouse_down(_: Id, _: Sel, ev: Id) {
    let (x, y) = canvas_xy(ev);
    dispatch(Event::MouseDown {
        x,
        y,
        button: MouseButton::Right,
    });
}

unsafe extern "C" fn view_right_mouse_up(_: Id, _: Sel, ev: Id) {
    let (x, y) = canvas_xy(ev);
    dispatch(Event::MouseUp {
        x,
        y,
        button: MouseButton::Right,
    });
}

unsafe extern "C" fn view_other_mouse_down(_: Id, _: Sel, ev: Id) {
    if msg::<u64>(ev, sel("buttonNumber")) == 2 {
        let (x, y) = canvas_xy(ev);
        dispatch(Event::MouseDown {
            x,
            y,
            button: MouseButton::Middle,
        });
    }
}

unsafe extern "C" fn view_other_mouse_up(_: Id, _: Sel, ev: Id) {
    if msg::<u64>(ev, sel("buttonNumber")) == 2 {
        let (x, y) = canvas_xy(ev);
        dispatch(Event::MouseUp {
            x,
            y,
            button: MouseButton::Middle,
        });
    }
}

unsafe extern "C" fn view_scroll_wheel(_: Id, _: Sel, ev: Id) {
    let m = state();
    // Line-mode deltas are ~1 line per notch (a line ≈ the 48 px the Win32
    // backend scrolls per notch); precise (trackpad) deltas are points.
    // AppKit already applies the user's natural-scrolling preference to the
    // sign, so positive means "scroll up" in both modes.
    let precise = msg::<i8>(ev, sel("hasPreciseScrollingDeltas")) != 0;
    let k = if precise {
        m.scale as f64
    } else {
        48.0 * m.scale as f64
    };
    dispatch(Event::Scroll {
        dx: (msg::<f64>(ev, sel("deltaX")) * k) as f32,
        dy: (msg::<f64>(ev, sel("deltaY")) * k) as f32,
    });
}

unsafe extern "C" fn view_dragging_entered(_: Id, _: Sel, _: Id) -> u64 {
    1 // NSDragOperationCopy
}

unsafe extern "C" fn view_perform_drag(_: Id, _: Sel, sender: Id) -> i8 {
    let pb = msg::<Id>(sender, sel("draggingPasteboard"));
    if pb.is_null() {
        return 0;
    }
    let files = msg1::<Id, Id>(pb, sel("propertyListForType:"), ns_str(DRAG_TYPE));
    if files.is_null() {
        return 0;
    }
    let n = msg::<u64>(files, sel("count"));
    for i in 0..n {
        let f = msg1::<u64, Id>(files, sel("objectAtIndex:"), i);
        if !f.is_null() {
            dispatch(Event::FileOpen(std::path::PathBuf::from(str_from_ns(f))));
        }
    }
    1
}

unsafe extern "C" fn win_can_become_key(_: Id, _: Sel) -> i8 {
    // Borderless windows refuse key status by default, which would starve
    // the view of keyDown:.
    1
}

unsafe extern "C" fn app_did_finish_launching(_: Id, _: Sel, _: Id) {
    let m = state();
    msg1::<Id, ()>(m.win, sel("makeKeyAndOrderFront:"), null_mut());
    msg1::<i8, ()>(m.app, sel("activateIgnoringOtherApps:"), 1);
}

unsafe extern "C" fn app_should_terminate(_: Id, _: Sel, _: Id) -> i8 {
    1
}

unsafe extern "C" fn app_after_last_window(_: Id, _: Sel, _: Id) -> i8 {
    1
}

unsafe extern "C" fn app_open_file(_: Id, _: Sel, _: Id, file: Id) -> i8 {
    if !file.is_null() {
        dispatch(Event::FileOpen(std::path::PathBuf::from(str_from_ns(file))));
    }
    1
}

unsafe extern "C" fn win_should_close(_: Id, _: Sel, _: Id) -> i8 {
    // The app decides; it calls host.quit() if it wants to exit.
    dispatch(Event::CloseRequested);
    0
}

unsafe extern "C" fn timer_tick(_: Id, _: Sel, _: Id) {
    dispatch(Event::Tick);
}

// ---------------------------------------------------------------- classes

struct MethodDef {
    name: &'static str,
    imp: unsafe extern "C" fn(),
    /// ObjC type encoding; the runtime does not verify it, but keep it honest.
    types: &'static str,
}

/// Erase a callback's signature to the `IMP` type `class_addMethod` takes
/// (a macro, so the transmute sees the concrete fn-pointer type at each
/// call site rather than an unsized generic; callers are in `run`'s unsafe
/// block).
macro_rules! tr {
    ($f:expr) => {
        std::mem::transmute::<_, unsafe extern "C" fn()>($f)
    };
}

unsafe fn make_class(super_name: &str, name: &str, methods: &[MethodDef]) -> Result<Id> {
    let sup = class(super_name);
    if sup.is_null() {
        return Err(Error::Platform(format!(
            "objc_getClass({super_name}) failed"
        )));
    }
    let mut nb = [0u8; 64];
    nb[..name.len()].copy_from_slice(name.as_bytes());
    let cls = objc_allocateClassPair(sup, nb.as_ptr() as *const c_char, 0);
    if cls.is_null() {
        return Err(Error::Platform(format!(
            "objc_allocateClassPair({name}) failed"
        )));
    }
    for m in methods {
        let mut tb = [0u8; 96];
        tb[..m.types.len()].copy_from_slice(m.types.as_bytes());
        class_addMethod(cls, sel(m.name), m.imp, tb.as_ptr() as *const c_char);
    }
    objc_registerClassPair(cls);
    Ok(cls)
}

/// Dock / `Cmd-Tab` icon from the portable 32×32 ARGB renderer.
unsafe fn set_app_icon(app: Id, colorspace: Id) {
    let px = crate::gfx::icon::argb32();
    let provider = CGDataProviderCreateWithData(
        null_mut(),
        px.as_ptr() as *const c_void,
        px.len() * 4,
        null_mut(),
    );
    if provider.is_null() {
        return;
    }
    let img = CGImageCreate(
        32,
        32,
        8,
        32,
        32 * 4,
        colorspace,
        BITMAP_INFO,
        provider,
        null(),
        false,
        0,
    );
    if !img.is_null() {
        let nsimg = msg2::<Id, NSSize, Id>(
            msg::<Id>(class("NSImage"), sel("alloc")),
            sel("initWithCGImage:size:"),
            img,
            NSSize { w: 32.0, h: 32.0 },
        );
        if !nsimg.is_null() {
            msg1::<Id, ()>(app, sel("setApplicationIconImage:"), nsimg);
        }
        CGImageRelease(img);
    }
    CGDataProviderRelease(provider);
}

// ---------------------------------------------------------------- entry

pub fn run(handler: Box<dyn Handler>) -> Result<()> {
    unsafe {
        let app = msg::<Id>(class("NSApplication"), sel("sharedApplication"));
        if app.is_null() {
            return Err(Error::Platform("NSApplication failed to start".into()));
        }
        // Regular: dock icon, focus, and the panels behave modally.
        msg1::<u64, ()>(app, sel("setActivationPolicy:"), 0);

        let view_cls = make_class(
            "NSView",
            "ReaditorView",
            &[
                MethodDef {
                    name: "isFlipped",
                    imp: tr!(view_is_flipped as unsafe extern "C" fn(Id, Sel) -> i8),
                    types: "B@:",
                },
                MethodDef {
                    name: "acceptsFirstResponder",
                    imp: tr!(view_accepts_first_responder as unsafe extern "C" fn(Id, Sel) -> i8),
                    types: "B@:",
                },
                MethodDef {
                    name: "drawRect:",
                    imp: tr!(view_draw_rect as unsafe extern "C" fn(Id, Sel, NSRect)),
                    types: "v@:{_NSRect={_NSPoint=dd}{_NSSize=dd}}",
                },
                MethodDef {
                    name: "keyDown:",
                    imp: tr!(view_key_down as unsafe extern "C" fn(Id, Sel, Id)),
                    types: "v@:@",
                },
                MethodDef {
                    name: "mouseDown:",
                    imp: tr!(view_mouse_down as unsafe extern "C" fn(Id, Sel, Id)),
                    types: "v@:@",
                },
                MethodDef {
                    name: "mouseUp:",
                    imp: tr!(view_mouse_up as unsafe extern "C" fn(Id, Sel, Id)),
                    types: "v@:@",
                },
                MethodDef {
                    name: "mouseDragged:",
                    imp: tr!(view_mouse_dragged as unsafe extern "C" fn(Id, Sel, Id)),
                    types: "v@:@",
                },
                MethodDef {
                    name: "mouseMoved:",
                    imp: tr!(view_mouse_moved as unsafe extern "C" fn(Id, Sel, Id)),
                    types: "v@:@",
                },
                MethodDef {
                    name: "rightMouseDown:",
                    imp: tr!(view_right_mouse_down as unsafe extern "C" fn(Id, Sel, Id)),
                    types: "v@:@",
                },
                MethodDef {
                    name: "rightMouseUp:",
                    imp: tr!(view_right_mouse_up as unsafe extern "C" fn(Id, Sel, Id)),
                    types: "v@:@",
                },
                MethodDef {
                    name: "otherMouseDown:",
                    imp: tr!(view_other_mouse_down as unsafe extern "C" fn(Id, Sel, Id)),
                    types: "v@:@",
                },
                MethodDef {
                    name: "otherMouseUp:",
                    imp: tr!(view_other_mouse_up as unsafe extern "C" fn(Id, Sel, Id)),
                    types: "v@:@",
                },
                MethodDef {
                    name: "scrollWheel:",
                    imp: tr!(view_scroll_wheel as unsafe extern "C" fn(Id, Sel, Id)),
                    types: "v@:@",
                },
                MethodDef {
                    name: "draggingEntered:",
                    imp: tr!(view_dragging_entered as unsafe extern "C" fn(Id, Sel, Id) -> u64),
                    types: "Q@:@",
                },
                MethodDef {
                    name: "performDragOperation:",
                    imp: tr!(view_perform_drag as unsafe extern "C" fn(Id, Sel, Id) -> i8),
                    types: "B@:@",
                },
            ],
        )?;
        let win_cls = make_class(
            "NSWindow",
            "ReaditorWindow",
            &[MethodDef {
                name: "canBecomeKeyWindow",
                imp: tr!(win_can_become_key as unsafe extern "C" fn(Id, Sel) -> i8),
                types: "B@:",
            }],
        )?;
        let del_cls = make_class(
            "NSObject",
            "ReaditorDelegate",
            &[
                MethodDef {
                    name: "applicationDidFinishLaunching:",
                    imp: tr!(app_did_finish_launching as unsafe extern "C" fn(Id, Sel, Id)),
                    types: "v@:@",
                },
                MethodDef {
                    name: "applicationShouldTerminate:",
                    imp: tr!(app_should_terminate as unsafe extern "C" fn(Id, Sel, Id) -> i8),
                    types: "B@:@",
                },
                MethodDef {
                    name: "applicationShouldTerminateAfterLastWindowClosed:",
                    imp: tr!(app_after_last_window as unsafe extern "C" fn(Id, Sel, Id) -> i8),
                    types: "B@:@",
                },
                MethodDef {
                    name: "application:openFile:",
                    imp: tr!(app_open_file as unsafe extern "C" fn(Id, Sel, Id, Id) -> i8),
                    types: "B@:@@",
                },
                MethodDef {
                    name: "windowShouldClose:",
                    imp: tr!(win_should_close as unsafe extern "C" fn(Id, Sel, Id) -> i8),
                    types: "B@:@",
                },
                MethodDef {
                    name: "tick:",
                    imp: tr!(timer_tick as unsafe extern "C" fn(Id, Sel, Id)),
                    types: "v@:@",
                },
            ],
        )?;

        // Setup-phase autoreleases (ns_str and friends) collect here.
        let pool = msg::<Id>(
            msg::<Id>(class("NSAutoreleasePool"), sel("alloc")),
            sel("init"),
        );

        let delegate = msg::<Id>(msg::<Id>(del_cls, sel("alloc")), sel("init"));
        msg1::<Id, ()>(app, sel("setDelegate:"), delegate);

        // Where to open: the rect the last session was closed at when this
        // screen still shows it, else centered, like the other backends.
        // Read before the handler moves into `Mac`.
        let asked = handler.initial_window();
        let maximized = asked.is_some_and(|g| g.maximized);
        let full = CGDisplayBounds(CGMainDisplayID());
        let screen_obj = msg::<Id>(class("NSScreen"), sel("mainScreen"));
        let scale = if screen_obj.is_null() {
            1.0
        } else {
            msg::<f64>(screen_obj, sel("backingScaleFactor")).max(1.0)
        } as f32;
        // `open_at` speaks top-left physical px; AppKit wants bottom-left points.
        let desktop = (
            0,
            0,
            (full.size.w * scale as f64) as i32,
            (full.size.h * scale as f64) as i32,
        );
        let centered = (
            ((full.size.w - WIN_W) / 2.0).max(0.0) as i32,
            ((full.size.h - WIN_H) / 2.0).max(0.0) as i32,
            WIN_W as i32,
            WIN_H as i32,
        );
        let (gx, gy, gw, gh) = super::open_at(asked, desktop, centered);
        let to_pt = |v: i32| v as f64 / scale as f64;
        let rect = NSRect {
            origin: NSPoint {
                x: to_pt(gx),
                y: full.size.h - to_pt(gy) - to_pt(gh),
            },
            size: NSSize {
                w: to_pt(gw),
                h: to_pt(gh),
            },
        };
        let win = msg4::<NSRect, u64, u64, i8, Id>(
            msg::<Id>(win_cls, sel("alloc")),
            sel("initWithContentRect:styleMask:backing:defer:"),
            rect,
            0, // NSWindowStyleMaskBorderless: the app draws the title bar
            2, // NSBackingStoreBuffered
            0,
        );
        if win.is_null() {
            return Err(Error::Platform("NSWindow init failed".into()));
        }
        let view = msg1::<NSRect, Id>(
            msg::<Id>(view_cls, sel("alloc")),
            sel("initWithFrame:"),
            NSRect {
                origin: NSPoint { x: 0.0, y: 0.0 },
                size: rect.size,
            },
        );
        if view.is_null() {
            return Err(Error::Platform("NSView init failed".into()));
        }
        msg1::<Id, ()>(win, sel("setContentView:"), view);
        msg1::<Id, ()>(win, sel("setDelegate:"), delegate);
        msg1::<i8, ()>(win, sel("setAcceptsMouseMovedEvents:"), 1);
        msg1::<Id, ()>(
            view,
            sel("registerForDraggedTypes:"),
            msg1::<Id, Id>(class("NSArray"), sel("arrayWithObject:"), ns_str(DRAG_TYPE)),
        );

        let scale = (msg::<f64>(win, sel("backingScaleFactor")) as f32).max(1.0);
        let cursors = [
            msg::<Id>(class("NSCursor"), sel("arrowCursor")),
            msg::<Id>(class("NSCursor"), sel("IBeamCursor")),
            msg::<Id>(class("NSCursor"), sel("pointingHandCursor")),
        ];
        let colorspace = CGColorSpaceCreateDeviceRGB();

        let title = handler.title();
        let (cw, ch) = (
            (WIN_W * scale as f64) as usize,
            (WIN_H * scale as f64) as usize,
        );
        let mac = Box::new(Mac {
            handler,
            canvas: Canvas::new(cw, ch),
            app,
            win,
            view,
            colorspace,
            cursors,
            last_title: title,
            dirty: true,
            scale,
            cursor_idx: 0,
            move_drag: None,
            size_drag: None,
            maximized: false,
            pre_max: rect,
        });
        MAC = Box::into_raw(mac);
        msg1::<Id, ()>(win, sel("setTitle:"), ns_str(&state().last_title));
        set_app_icon(app, colorspace);
        msg5::<f64, Id, Sel, Id, i8, Id>(
            class("NSTimer"),
            sel("scheduledTimerWithTimeInterval:target:selector:userInfo:repeats:"),
            TICK_SECS,
            delegate,
            sel("tick:"),
            null_mut(),
            1,
        );
        // Tell the app its initial size so the first drawRect lays out right.
        dispatch(Event::Resized {
            width: cw,
            height: ch,
            scale,
        });
        // The rect we opened at, before anything else has had a say.
        report_geometry();
        if maximized {
            // A session that ended maximized opens maximized — at the normal
            // rect, so un-maximizing lands where the user left it.
            maximize_to_screen(state());
            report_geometry();
        }
        msg::<()>(pool, sel("drain"));
        msg::<()>(app, sel("run"));
        drop(Box::from_raw(MAC));
        Ok(())
    }
}

// ---------------------------------------------------------------- platform

/// The widest manual edge-resize band (logical px) this backend claims — the
/// same 6 px the X11 backend offers — exposed through `platform::edge_grip`
/// so `App` keeps its overlay scrollbars clear of it.
pub fn edge_grip() -> f32 {
    RESIZE_BAND
}

/// The dark-mode preference: `AppleInterfaceStyle` reads "Dark" exactly when
/// the user picked the dark appearance (the key is absent in light mode).
pub fn system_prefers_dark() -> bool {
    let out = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output();
    out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("Dark"))
}

/// `O_RDONLY`; the value is Darwin's, which is what this backend targets.
const O_RDONLY: c_int = 0;
const PROT_READ: c_int = 1;
const MAP_PRIVATE: c_int = 2;
const MAP_FAILED: *mut c_void = usize::MAX as *mut c_void;

/// Read-only `MAP_PRIVATE` view of a file for the life of the process (see
/// `platform::map_file`). `None` when the file is empty or cannot be mapped,
/// so the caller reads it instead.
///
/// `MAP_PRIVATE` rather than `MAP_SHARED`: writes by another process must
/// never reach our view of a font, and the kernel then keeps the pages
/// copy-on-write and file-backed. The descriptor is deliberately left open
/// (the view is borrowed for `'static`); the path is used only to open it.
pub fn map_file(path: &std::path::Path) -> Option<&'static [u8]> {
    let len = std::fs::metadata(path).ok()?.len();
    if len == 0 || len > usize::MAX as u64 {
        return None;
    }
    let c = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    unsafe {
        let fd = open(c.as_ptr(), O_RDONLY);
        if fd < 0 {
            return None;
        }
        let p = mmap(null_mut(), len as usize, PROT_READ, MAP_PRIVATE, fd, 0);
        if p == MAP_FAILED || p.is_null() {
            return None;
        }
        Some(std::slice::from_raw_parts(p as *const u8, len as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_keycodes_map() {
        assert_eq!(key_of(VK_ENTER), Some(Key::Enter));
        assert_eq!(key_of(VK_BACKSPACE), Some(Key::Backspace));
        assert_eq!(key_of(VK_FORWARD_DELETE), Some(Key::Delete));
        assert_eq!(key_of(VK_LEFT), Some(Key::Left));
        assert_eq!(key_of(VK_PAGE_DOWN), Some(Key::PageDown));
        assert_eq!(key_of(FKEYS[0]), Some(Key::F(1)));
        assert_eq!(key_of(FKEYS[11]), Some(Key::F(12)));
        // Printable keys arrive through `characters`, not the keycode.
        assert_eq!(key_of(0), None);
        assert_eq!(key_of(999), None);
    }

    #[test]
    fn command_maps_to_the_apps_ctrl_chords() {
        assert!(mods_of(NS_COMMAND).ctrl);
        assert!(mods_of(NS_CONTROL).ctrl);
        assert!(mods_of(NS_ALTERNATE).alt);
        assert!(mods_of(NS_SHIFT).shift);
        let none = mods_of(0);
        assert!(!none.ctrl && !none.alt && !none.shift);
    }

    #[test]
    fn edge_flags_cover_the_borders_only() {
        // Center: nothing; each border: its bit; corner: two bits.
        assert_eq!(edge_flags(500.0, 400.0, 1100.0, 800.0, 6.0), 0);
        assert_eq!(edge_flags(2.0, 400.0, 1100.0, 800.0, 6.0), 1);
        assert_eq!(edge_flags(1098.0, 400.0, 1100.0, 800.0, 6.0), 2);
        assert_eq!(edge_flags(500.0, 2.0, 1100.0, 800.0, 6.0), 4);
        assert_eq!(edge_flags(500.0, 798.0, 1100.0, 800.0, 6.0), 8);
        assert_eq!(edge_flags(1.0, 1.0, 1100.0, 800.0, 6.0), 1 | 4);
        assert_eq!(edge_flags(500.0, 400.0, 1100.0, 800.0, 0.0), 0);
    }

    #[test]
    fn manual_resize_grows_the_dragged_edges() {
        // Bottom-left global coords: the top edge grows the height, the
        // bottom edge moves the origin.
        let f = NSRect {
            origin: NSPoint { x: 100.0, y: 100.0 },
            size: NSSize { w: 400.0, h: 300.0 },
        };
        let right = resized_frame(f, 10.0, 0.0, 2);
        assert_eq!(right.size.w, 410.0);
        let left = resized_frame(f, 10.0, 0.0, 1);
        assert_eq!((left.origin.x, left.size.w), (110.0, 390.0));
        let top = resized_frame(f, 0.0, 10.0, 4);
        assert_eq!(top.size.h, 310.0);
        let bottom = resized_frame(f, 0.0, -10.0, 8);
        assert_eq!((bottom.origin.y, bottom.size.h), (90.0, 310.0));
    }
}
