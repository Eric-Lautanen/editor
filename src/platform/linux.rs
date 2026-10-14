//! X11 backend via libX11 (system library; `#[link(name = "X11")]`). Works
//! under Wayland via XWayland.
//!
//! M8 scope: borderless window with the app-drawn title bar (manual move +
//! edge resize), full event translation (keys incl. Ctrl/Shift/Alt chords,
//! mouse, wheel), canvas blit via `XPutImage` (ZPixmap, depth 24), clipboard
//! through the CLIPBOARD/PRIMARY selections (we own the selection), file
//! dialogs by shelling out to `zenity`/`kdialog`, `xdg-open` for links,
//! `_NET_WM_ICON` from the portable icon renderer, `_MOTIF_WM_HINTS` to drop
//! server decorations, `_NET_WM_STATE` for maximize, dark preference via
//! `gsettings`. Not yet: XDND drag-and-drop (tracked in PLAN.md M8 follow-up).
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
    ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void},
    ptr::{null, null_mut},
};

// ---------------------------------------------------------------- FFI

// libc, for the font mapping only (`font::fallback` maps system fonts rather
// than reading them, see `platform::map_file`). No libc crate: one link
// attribute and one call needed. The file itself is opened through std
// (`File::open`), not a binding: declaring `open` here — even with the exact
// runtime signature in mind — collides with the variadic `open` that the
// standard library itself declares for its own I/O, and the
// `invalid_runtime_symbol_definitions` lint (deny by default) rejects any
// conflicting redefinition.
#[link(name = "c")]
extern "C" {
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        off: i64,
    ) -> *mut c_void;
}

const PROT_READ: c_int = 1;
const MAP_PRIVATE: c_int = 2;
const MAP_FAILED: *mut c_void = usize::MAX as *mut c_void;

type Display = c_void;
type Window = c_ulong;
type Atom = c_ulong;
type Cursor = c_ulong;
type Gc = *mut c_void;
type Xid = c_ulong;
type Time = c_ulong;

#[link(name = "X11")]
extern "C" {
    fn XOpenDisplay(name: *const c_char) -> *mut Display;
    fn XCloseDisplay(d: *mut Display) -> i32;
    fn XDefaultScreen(d: *mut Display) -> i32;
    fn XDefaultDepth(d: *mut Display, screen: i32) -> i32;
    fn XDisplayWidth(d: *mut Display, screen: i32) -> i32;
    fn XDisplayHeight(d: *mut Display, screen: i32) -> i32;
    fn XDefaultRootWindow(d: *mut Display) -> Window;
    fn XCreateSimpleWindow(
        d: *mut Display,
        parent: Window,
        x: i32,
        y: i32,
        w: c_uint,
        h: c_uint,
        border_w: c_uint,
        border: c_ulong,
        background: c_ulong,
    ) -> Window;
    fn XDestroyWindow(d: *mut Display, w: Window);
    fn XSelectInput(d: *mut Display, w: Window, mask: c_long) -> i32;
    fn XMapWindow(d: *mut Display, w: Window) -> i32;
    fn XStoreName(d: *mut Display, w: Window, name: *const c_char) -> i32;
    fn XNextEvent(d: *mut Display, ev: *mut XEvent) -> i32;
    fn XPending(d: *mut Display) -> i32;
    fn XFlush(d: *mut Display) -> i32;
    fn XCreateGC(d: *mut Display, drawable: Xid, mask: c_ulong, values: *const c_void) -> Gc;
    fn XPutImage(
        d: *mut Display,
        drawable: Xid,
        gc: Gc,
        image: *mut XImage,
        src_x: i32,
        src_y: i32,
        dest_x: i32,
        dest_y: i32,
        w: c_uint,
        h: c_uint,
    ) -> i32;
    fn XInternAtom(d: *mut Display, name: *const c_char, only_if_exists: i32) -> Atom;
    fn XSetWMProtocols(d: *mut Display, w: Window, atoms: *mut Atom, count: i32) -> i32;
    fn XChangeProperty(
        d: *mut Display,
        w: Window,
        property: Atom,
        kind: Atom,
        format: i32,
        mode: i32,
        data: *const u8,
        nelements: i32,
    ) -> i32;
    fn XDefineCursor(d: *mut Display, w: Window, cursor: Cursor) -> i32;
    fn XCreateFontCursor(d: *mut Display, shape: c_uint) -> Cursor;
    fn XLookupString(
        ev: *const XKeyEvent,
        buf: *mut c_char,
        len: i32,
        keysym: *mut c_ulong,
        status: *mut c_int,
    ) -> i32;
    fn XMoveWindow(d: *mut Display, w: Window, x: i32, y: i32) -> i32;
    fn XMoveResizeWindow(d: *mut Display, w: Window, x: i32, y: i32, w_: c_uint, h: c_uint) -> i32;
    fn XIconifyWindow(d: *mut Display, w: Window, screen: i32) -> i32;
    fn XSetSelectionOwner(d: *mut Display, selection: Atom, owner: Window, time: Time) -> i32;
    fn XConvertSelection(
        d: *mut Display,
        selection: Atom,
        target: Atom,
        property: Atom,
        requestor: Window,
        time: Time,
    ) -> i32;
    fn XGetWindowProperty(
        d: *mut Display,
        w: Window,
        property: Atom,
        offset: c_long,
        length: c_long,
        delete: i32,
        req_type: Atom,
        actual_type: *mut Atom,
        actual_format: *mut c_int,
        nitems: *mut c_ulong,
        bytes_after: *mut c_ulong,
        data: *mut *mut u8,
    ) -> i32;
    fn XSendEvent(
        d: *mut Display,
        w: Window,
        propagate: i32,
        mask: c_long,
        ev: *const XEvent,
    ) -> i32;
    fn XFree(p: *mut c_void) -> i32;
}

// ---------------------------------------------------------------- types

/// `XEvent` is a union; every member starts with `int type` and the whole
/// thing is 192 bytes, 8-aligned on 64-bit.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct XEvent {
    pub kind: i32,
    pad: [u8; 188],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XKeyEvent {
    kind: i32,
    serial: c_ulong,
    send_event: i32,
    display: *mut Display,
    window: Window,
    root: Window,
    subwindow: Window,
    time: Time,
    x: i32,
    y: i32,
    x_root: i32,
    y_root: i32,
    state: c_uint,
    keycode: c_uint,
    same_screen: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XButtonEvent {
    kind: i32,
    serial: c_ulong,
    send_event: i32,
    display: *mut Display,
    window: Window,
    root: Window,
    subwindow: Window,
    time: Time,
    x: i32,
    y: i32,
    x_root: i32,
    y_root: i32,
    state: c_uint,
    button: c_uint,
    same_screen: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XMotionEvent {
    kind: i32,
    serial: c_ulong,
    send_event: i32,
    display: *mut Display,
    window: Window,
    root: Window,
    subwindow: Window,
    time: Time,
    x: i32,
    y: i32,
    x_root: i32,
    y_root: i32,
    state: c_uint,
    is_hint: i32,
    same_screen: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XConfigureEvent {
    kind: i32,
    serial: c_ulong,
    send_event: i32,
    display: *mut Display,
    event: Window,
    window: Window,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    border_width: i32,
    above: Window,
    override_redirect: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XPropertyEvent {
    kind: i32,
    serial: c_ulong,
    send_event: i32,
    display: *mut Display,
    window: Window,
    atom: Atom,
    time: Time,
    state: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XExposeEvent {
    kind: i32,
    serial: c_ulong,
    send_event: i32,
    display: *mut Display,
    window: Window,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    count: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XClientMessageEvent {
    kind: i32,
    serial: c_ulong,
    send_event: i32,
    display: *mut Display,
    window: Window,
    message_type: Atom,
    format: i32,
    data: [c_long; 5],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XSelectionEvent {
    kind: i32,
    serial: c_ulong,
    send_event: i32,
    display: *mut Display,
    window: Window,
    selection: Atom,
    target: Atom,
    property: Atom,
    time: Time,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XSelectionRequestEvent {
    kind: i32,
    serial: c_ulong,
    send_event: i32,
    display: *mut Display,
    owner: Window,
    requestor: Window,
    selection: Atom,
    target: Atom,
    property: Atom,
    time: Time,
}

#[repr(C)]
struct XImage {
    width: c_int,
    height: c_int,
    xoffset: c_int,
    format: c_int,
    data: *mut c_char,
    byte_order: c_int,
    bitmap_unit: c_int,
    bitmap_bit_order: c_int,
    bitmap_pad: c_int,
    depth: c_int,
    bytes_per_line: c_int,
    visual: *mut c_void,
    red_mask: c_ulong,
    green_mask: c_ulong,
    blue_mask: c_ulong,
    obdata: Xid,
    // fns omitted: XPutImage only reads the header fields above.
    pad: [u8; 128],
}

// ---------------------------------------------------------------- consts

// Event types.
const KEY_PRESS: i32 = 2;
const BUTTON_PRESS: i32 = 4;
const BUTTON_RELEASE: i32 = 5;
const MOTION_NOTIFY: i32 = 6;
const FOCUS_OUT: i32 = 10;
const EXPOSE: i32 = 12;
const CONFIGURE_NOTIFY: i32 = 22;
const PROPERTY_NOTIFY: i32 = 28;
const SELECTION_CLEAR: i32 = 29;
const SELECTION_REQUEST: i32 = 30;
const SELECTION_NOTIFY: i32 = 31;
const CLIENT_MESSAGE: i32 = 33;
const DESTROY_NOTIFY: i32 = 17;

// Event masks.
const KEY_PRESS_MASK: c_long = 1 << 0;
const BUTTON_PRESS_MASK: c_long = 1 << 2;
const BUTTON_RELEASE_MASK: c_long = 1 << 3;
const POINTER_MOTION_MASK: c_long = 1 << 6;
const EXPOSURE_MASK: c_long = 1 << 15;
const STRUCTURE_NOTIFY_MASK: c_long = 1 << 17;
/// Focus changes: a grab or activation change mid-press means the release is
/// no longer ours (`cancel_gesture`).
const FOCUS_CHANGE_MASK: c_long = 1 << 21;
const PROPERTY_CHANGE_MASK: c_long = 1 << 22;
const SUBSTRUCTURE_NOTIFY_MASK: c_long = 1 << 19;
const SUBSTRUCTURE_REDIRECT_MASK: c_long = 1 << 20;

// Modifier state.
const SHIFT_MASK: c_uint = 1;
const CONTROL_MASK: c_uint = 4;
const MOD1_MASK: c_uint = 8;

// Buttons.
const BUTTON_LEFT: c_uint = 1;
const BUTTON_MIDDLE: c_uint = 2;
const BUTTON_RIGHT: c_uint = 3;
const WHEEL_UP: c_uint = 4;
const WHEEL_DOWN: c_uint = 5;
const TILT_LEFT: c_uint = 6;
const TILT_RIGHT: c_uint = 7;

// KeySyms.
const XK_BACKSPACE: c_ulong = 0xff08;
const XK_TAB: c_ulong = 0xff09;
const XK_RETURN: c_ulong = 0xff0d;
const XK_ESCAPE: c_ulong = 0xff1b;
const XK_HOME: c_ulong = 0xff50;
const XK_LEFT: c_ulong = 0xff51;
const XK_UP: c_ulong = 0xff52;
const XK_RIGHT: c_ulong = 0xff53;
const XK_DOWN: c_ulong = 0xff54;
const XK_PAGE_UP: c_ulong = 0xff55;
const XK_PAGE_DOWN: c_ulong = 0xff56;
const XK_END: c_ulong = 0xff57;
const XK_DELETE: c_ulong = 0xffff;
const XK_F1: c_ulong = 0xffbe;

// Cursor shapes (X11 font-cursor indices).
const XC_LEFT_PTR: c_uint = 68;
const XC_XTERM: c_uint = 152;
const XC_HAND2: c_uint = 60;

const XA_PRIMARY: Atom = 1;
const XA_ATOM: Atom = 4;
const XA_CARDINAL: Atom = 6;
const XA_STRING: Atom = 31;
const ZPOINTER: i32 = 2; // ZPixmap
const PROP_MODE_REPLACE: i32 = 0;
const CURRENT_TIME: Time = 0;

const WIN_W: c_uint = super::DEFAULT_SIZE.0 as c_uint;
const WIN_H: c_uint = super::DEFAULT_SIZE.1 as c_uint;
/// `_NET_WM_STATE` client-message actions (`_NET_WM_STATE_ADD` / `_TOGGLE`).
const WM_STATE_ADD: i32 = 1;
const WM_STATE_TOGGLE: i32 = 2;
/// Widest edge band (px) that starts a manual resize, matching the old thick
/// frame. X11 hands the client no sizing-border metric, so this is the backend's
/// own maximum; `App` narrows it to just the points where a scrollbar needs the
/// pixels, and the rest of the edge keeps the full band (see `Handler::grip_at`).
const RESIZE_BAND: i32 = 6;
/// Animation frame interval while the app is moving something: 5 ms, ~200 Hz —
/// well past any display this renders on and down from the first pass's 8 ms.
/// The poll loop is already awake every 1 ms, so no timer is needed to produce
/// it, and the app's easing is time-based, so extra beats only cost repaints.
const FRAME_MS: u64 = 5;

// ---------------------------------------------------------------- state

/// Everything the backend owns: the X connection, the window, and the
/// app-loop state. One `Box<X>` lives for the whole `run`.
struct X {
    d: *mut Display,
    screen: i32,
    root: Window,
    win: Window,
    gc: Gc,
    /// arrow, ibeam, hand
    cursors: [Cursor; 3],
    atoms: Atoms,
    handler: Box<dyn Handler>,
    canvas: Canvas,
    last_title: String,
    /// Set by `request_redraw`; the poll loop paints when it sees it.
    dirty: bool,
    /// The app has something animating and wants frames (see
    /// `Host::set_animating`): the poll loop then emits `Event::Frame` on its
    /// own `FRAME_MS` cadence instead of only the 40 ms heartbeat.
    animating: bool,
    /// True once the loop should exit (window destroyed / host quit).
    done: bool,
    /// Text we own as the CLIPBOARD/PRIMARY selection, if any.
    clip_text: String,
    /// Live manual window drag: (press root x, press root y, win x, win y).
    move_drag: Option<(i32, i32, i32, i32)>,
    /// Live manual resize: (press root x, press root y, win x, win y, w, h,
    /// edge flags: bit0 left, bit1 right, bit2 top, bit3 bottom).
    size_drag: Option<(i32, i32, i32, i32, i32, i32, u8)>,
    /// Last known window geometry (for move/resize math).
    geom: (i32, i32, i32, i32),
    /// Last mouse position (client px), for cursor updates and for the release
    /// synthesized when focus leaves mid-press.
    mouse: (f32, f32),
    /// Left button held with the app told about it. The app's drags end only on
    /// `MouseUp`, so this is what `FOCUS_OUT` consults.
    left_down: bool,
    /// Root depth of the window (for the XImage header).
    depth: i32,
    /// Events swallowed while waiting for a SelectionNotify, replayed later.
    deferred: Vec<XEvent>,
    /// Current cursor index (0 arrow, 1 ibeam, 2 hand) to avoid re-defining.
    cursor_idx: usize,
    /// True while the WM has the window maximized on both axes (tracked from
    /// `_NET_WM_STATE`): edge presses must not start a manual resize then.
    maximized: bool,
}

#[derive(Clone, Copy)]
struct Atoms {
    wm_protocols: Atom,
    wm_delete: Atom,
    net_wm_icon: Atom,
    net_wm_state: Atom,
    net_wm_state_maximized_vert: Atom,
    net_wm_state_maximized_horz: Atom,
    motif_wm_hints: Atom,
    clipboard: Atom,
    targets: Atom,
    utf8_string: Atom,
    /// Scratch property for selection transfers.
    editor_sel: Atom,
}

unsafe fn intern(d: *mut Display, name: &str) -> Atom {
    let mut buf = name.as_bytes().to_vec();
    buf.push(0);
    XInternAtom(d, buf.as_ptr() as *const c_char, 0)
}

unsafe fn set_str_property(x: &X, property: Atom, kind: Atom, value: &str) {
    XChangeProperty(
        x.d,
        x.win,
        property,
        kind,
        8,
        PROP_MODE_REPLACE,
        value.as_ptr(),
        value.len() as i32,
    );
}

/// Drop server decorations so the app-drawn title bar is the only chrome
/// (`_MOTIF_WM_HINTS`: flags=decorations, decorations=none).
unsafe fn undecorate(x: &X) {
    let hints: [c_long; 5] = [2, 0, 0, 0, 0];
    XChangeProperty(
        x.d,
        x.win,
        x.atoms.motif_wm_hints,
        x.atoms.motif_wm_hints,
        32,
        PROP_MODE_REPLACE,
        hints.as_ptr() as *const u8,
        5,
    );
}

/// `_NET_WM_ICON` from the portable 32×32 renderer: width, height, ARGB rows.
unsafe fn set_icon(x: &X) {
    let pixels = crate::gfx::icon::argb32();
    let mut data = Vec::<u32>::with_capacity(2 + pixels.len());
    data.push(32);
    data.push(32);
    data.extend(pixels);
    XChangeProperty(
        x.d,
        x.win,
        x.atoms.net_wm_icon,
        XA_CARDINAL,
        32,
        PROP_MODE_REPLACE,
        data.as_ptr() as *const u8,
        data.len() as i32,
    );
}

/// The dark-mode preference: GNOME/Cinnamon/Mate expose it via gsettings.
pub fn system_prefers_dark() -> bool {
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output();
    out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("dark"))
}

/// The widest manual edge-resize band (px) this backend claims, exposed through
/// `platform::edge_grip` so `App` keeps its overlay scrollbars clear of it.
pub fn edge_grip() -> f32 {
    RESIZE_BAND as f32
}

/// Read-only `MAP_PRIVATE` view of a file for the life of the process (see
/// `platform::map_file`). `None` when the file is empty or cannot be mapped, so
/// the caller reads it instead.
///
/// `MAP_PRIVATE` rather than `MAP_SHARED`: writes by another process must never
/// reach our view of a font, and the kernel then keeps the pages copy-on-write
/// and file-backed. The descriptor is deliberately left open (the view is
/// borrowed for `'static`); the path is used only to open it.
pub fn map_file(path: &std::path::Path) -> Option<&'static [u8]> {
    let len = std::fs::metadata(path).ok()?.len();
    if len == 0 || len > usize::MAX as u64 {
        return None;
    }
    unsafe {
        // `mmap` with a file descriptor doesn't need a path, so the file is
        // opened through std (`File::open`) rather than a bound `open` — see
        // the note on the libc extern block above.
        let file = std::fs::File::open(path).ok()?;
        let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
        let p = mmap(null_mut(), len as usize, PROT_READ, MAP_PRIVATE, fd, 0);
        if p == MAP_FAILED || p.is_null() {
            return None;
        }
        // Leak the file: the descriptor must stay open for the life of the
        // process, and the mapping is only ever freed with us.
        std::mem::forget(file);
        // SAFETY: `p` is a live read-only mapping of `len` bytes.
        Some(std::slice::from_raw_parts(p as *const u8, len as usize))
    }
}

/// Whether the poll loop should hand the app an animation frame now. The loop
/// wakes every millisecond, so without the interval a glide would be stepped
/// as fast as the machine can go — thousands of pointless repaints a second
/// instead of the ~120 the eye can use.
fn frame_due(animating: bool, since_last: std::time::Duration) -> bool {
    animating && since_last >= std::time::Duration::from_millis(FRAME_MS)
}

pub fn exe_path() -> Option<std::path::PathBuf> {
    std::env::current_exe().ok()
}

// ---------------------------------------------------------------- host

struct XHost {
    x: *mut X,
}

impl Host for XHost {
    fn request_redraw(&mut self) {
        unsafe { (*self.x).dirty = true };
    }
    /// Frames come from the poll loop's own clock (see `FRAME_MS`); this just
    /// says whether the app still wants them.
    fn set_animating(&mut self, on: bool) {
        unsafe { (*self.x).animating = on };
    }
    fn set_title(&mut self, title: &str) {
        unsafe {
            let x = &*self.x;
            // XStoreName is Latin-1; _NET_WM_NAME is UTF-8 for modern WM.
            let mut t = title.as_bytes().to_vec();
            t.push(0);
            XStoreName(x.d, x.win, t.as_ptr() as *const c_char);
            set_str_property(x, intern(x.d, "_NET_WM_NAME"), x.atoms.utf8_string, title);
        }
    }
    fn clipboard_get(&mut self) -> Option<String> {
        unsafe { read_clipboard(self.x) }
    }
    fn clipboard_set(&mut self, text: &str) {
        unsafe {
            (*self.x).clip_text = text.to_string();
            let x = &*self.x;
            XSetSelectionOwner(x.d, x.atoms.clipboard, x.win, CURRENT_TIME);
            XSetSelectionOwner(x.d, XA_PRIMARY, x.win, CURRENT_TIME);
        }
    }
    fn open_file_dialog(&mut self) -> Option<std::path::PathBuf> {
        file_dialog(false, "")
    }
    fn save_file_dialog(&mut self, default_name: &str) -> Option<std::path::PathBuf> {
        file_dialog(true, default_name)
    }
    fn open_url(&mut self, url: &str) {
        let _ = std::process::Command::new("xdg-open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    fn minimize(&mut self) {
        unsafe {
            let x = &*self.x;
            XIconifyWindow(x.d, x.win, x.screen);
        }
    }
    fn toggle_maximize(&mut self) {
        unsafe { wm_state(&*self.x, WM_STATE_TOGGLE) }
    }
    fn quit(&mut self) {
        unsafe {
            let x = &*self.x;
            XDestroyWindow(x.d, x.win);
            XFlush(x.d);
        }
    }
}

// ---------------------------------------------------------------- helpers

/// Ask the WM to add or toggle both maximized states on our window. The WM
/// owns maximize, so this is a request either way — `run` uses it to reopen a
/// session that ended maximized, `Host::toggle_maximize` for Ctrl+↑ and the
/// title-bar button.
unsafe fn wm_state(x: &X, action: i32) {
    let mut ev: XClientMessageEvent = std::mem::zeroed();
    ev.kind = CLIENT_MESSAGE;
    ev.window = x.win;
    ev.message_type = x.atoms.net_wm_state;
    ev.format = 32;
    ev.data[0] = action as c_long;
    ev.data[1] = x.atoms.net_wm_state_maximized_vert as c_long;
    ev.data[2] = x.atoms.net_wm_state_maximized_horz as c_long;
    let raw = &ev as *const XClientMessageEvent as *const XEvent;
    XSendEvent(
        x.d,
        x.root,
        1,
        SUBSTRUCTURE_NOTIFY_MASK | SUBSTRUCTURE_REDIRECT_MASK,
        raw,
    );
    XFlush(x.d);
}

/// Tell the app where the window is and whether it is maximized — the X11
/// counterpart of the Win32 `report_geometry`, sent on every configure and on
/// every maximize the WM decides on its own. Only `ConfigureNotify` knows a
/// move, and only `_NET_WM_STATE` knows the maximize, so both call this.
unsafe fn report_geometry(x: &mut X) {
    let (wx, wy, ww, wh) = x.geom;
    dispatch(
        x,
        Event::WindowGeometry(WindowGeometry {
            x: wx,
            y: wy,
            width: ww.max(0) as u32,
            height: wh.max(0) as u32,
            maximized: x.maximized,
        }),
    );
}

/// Modal file dialog by shelling out to `zenity`, falling back to
/// `kdialog`. `None` = cancelled or no dialog helper installed.
fn file_dialog(save: bool, default_name: &str) -> Option<std::path::PathBuf> {
    let mut z = std::process::Command::new("zenity");
    z.arg("--file-selection");
    if save {
        z.arg("--save").arg("--confirm-overwrite");
    } else {
        // Same claimed set as the app and the .desktop entry, derived from
        // EXTENSIONS so the dialog cannot drift (see `core::file_kind`).
        z.arg(format!(
            "--file-filter=Documents | {}",
            crate::core::file_kind::globs(" ")
        ));
        z.arg("--file-filter=All files | *");
    }
    if !default_name.is_empty() {
        z.arg(format!("--filename={default_name}"));
    }
    match z.output() {
        // Cancelled: stop, don't fall through to a second dialog.
        Ok(out) if !out.status.success() => return None,
        Ok(out) => {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Some(std::path::PathBuf::from(p));
            }
            return None;
        }
        Err(_) => {} // no zenity; try kdialog
    }
    let mut k = std::process::Command::new("kdialog");
    if save {
        k.arg("--getsavefilename").arg(if default_name.is_empty() {
            "."
        } else {
            default_name
        });
    } else {
        k.arg("--getopenfilename").arg(".").arg(format!(
            "{} | Documents",
            crate::core::file_kind::globs(" ")
        ));
    }
    match k.output() {
        Ok(out) if out.status.success() => {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!p.is_empty()).then(|| std::path::PathBuf::from(p))
        }
        _ => None,
    }
}

/// Read the CLIPBOARD selection (UTF-8). The X11 selection protocol is
/// async: we ask the owner to drop the text on our scratch property, then
/// spin the event queue until the SelectionNotify lands. Events that arrive
/// meanwhile are stashed and replayed by the main loop.
unsafe fn read_clipboard(x: *mut X) -> Option<String> {
    let x = &mut *x;
    XConvertSelection(
        x.d,
        x.atoms.clipboard,
        x.atoms.utf8_string,
        x.atoms.editor_sel,
        x.win,
        CURRENT_TIME,
    );
    XFlush(x.d);
    let mut ev: XEvent = std::mem::zeroed();
    let mut property: Atom = 0;
    for _ in 0..2000 {
        while XPending(x.d) > 0 {
            XNextEvent(x.d, &mut ev);
            if ev.kind == SELECTION_NOTIFY {
                let sel = &ev as *const XEvent as *const XSelectionEvent;
                property = (*sel).property;
            } else {
                x.deferred.push(ev);
            }
        }
        if property != 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    if property == 0 {
        return None;
    }
    let mut actual_type: Atom = 0;
    let mut actual_format: c_int = 0;
    let mut nitems: c_ulong = 0;
    let mut after: c_ulong = 0;
    let mut data: *mut u8 = null_mut();
    let rc = XGetWindowProperty(
        x.d,
        x.win,
        property,
        0,
        1 << 20,
        1, // delete after reading
        0, // any type
        &mut actual_type,
        &mut actual_format,
        &mut nitems,
        &mut after,
        &mut data,
    );
    if rc != 0 || data.is_null() {
        return None;
    }
    let bytes = if actual_format == 8 {
        nitems as usize
    } else {
        (nitems as usize) * (actual_format as usize / 8)
    };
    let text = String::from_utf8_lossy(std::slice::from_raw_parts(data, bytes)).into_owned();
    XFree(data as *mut c_void);
    Some(text)
}

/// Another client asked for our selection: answer with the stored text.
unsafe fn on_selection_request(x: &mut X, req: &XSelectionRequestEvent) {
    let mut ev: XSelectionEvent = std::mem::zeroed();
    ev.kind = SELECTION_NOTIFY;
    ev.display = x.d;
    // The requestor is identified by the `window` field in the reply.
    ev.window = req.requestor;
    ev.selection = req.selection;
    ev.target = req.target;
    ev.time = req.time;
    if req.target == x.atoms.targets {
        let list = [x.atoms.utf8_string, XA_STRING, x.atoms.targets];
        XChangeProperty(
            x.d,
            req.requestor,
            req.property,
            XA_ATOM,
            32,
            PROP_MODE_REPLACE,
            list.as_ptr() as *const u8,
            list.len() as i32,
        );
        ev.property = req.property;
    } else if req.target == x.atoms.utf8_string || req.target == XA_STRING {
        XChangeProperty(
            x.d,
            req.requestor,
            req.property,
            req.target,
            8,
            PROP_MODE_REPLACE,
            x.clip_text.as_ptr(),
            x.clip_text.len() as i32,
        );
        ev.property = req.property;
    } else {
        ev.property = 0; // unsupported target
    }
    XSendEvent(
        x.d,
        req.requestor,
        0,
        0,
        &ev as *const XSelectionEvent as *const XEvent,
    );
}

/// Hand-built `XImage` header wrapping the canvas buffer (ZPixmap, depth 24,
/// 4 bytes/px, host byte order). `XPutImage` only reads these fields; the
/// function-pointer tail is never called because we never use XGetPixel.
unsafe fn make_image(depth: i32, w: i32, h: i32, data: *mut c_char) -> XImage {
    XImage {
        width: w,
        height: h,
        xoffset: 0,
        format: ZPOINTER,
        data,
        // 0 = LSBFirst: every supported target is little-endian.
        byte_order: 0,
        bitmap_unit: 32,
        bitmap_bit_order: 0,
        bitmap_pad: 32,
        depth,
        bytes_per_line: w * 4,
        visual: null_mut(),
        red_mask: 0,
        green_mask: 0,
        blue_mask: 0,
        obdata: 0,
        pad: [0; 128],
    }
}

/// KeySym -> app key. Printable keys go through `XLookupString` instead.
fn key_of(keysym: c_ulong) -> Option<Key> {
    Some(match keysym {
        XK_BACKSPACE => Key::Backspace,
        XK_TAB => Key::Tab,
        XK_RETURN => Key::Enter,
        XK_ESCAPE => Key::Escape,
        XK_HOME => Key::Home,
        XK_END => Key::End,
        XK_LEFT => Key::Left,
        XK_UP => Key::Up,
        XK_RIGHT => Key::Right,
        XK_DOWN => Key::Down,
        XK_PAGE_UP => Key::PageUp,
        XK_PAGE_DOWN => Key::PageDown,
        XK_DELETE => Key::Delete,
        k if (XK_F1..XK_F1 + 24).contains(&k) => Key::F((k - XK_F1 + 1) as u8),
        _ => return None,
    })
}

fn mods_of(state: c_uint) -> Modifiers {
    Modifiers {
        ctrl: state & CONTROL_MASK != 0,
        alt: state & MOD1_MASK != 0,
        shift: state & SHIFT_MASK != 0,
    }
}

fn button_of(b: c_uint) -> Option<MouseButton> {
    Some(match b {
        BUTTON_LEFT => MouseButton::Left,
        BUTTON_MIDDLE => MouseButton::Middle,
        BUTTON_RIGHT => MouseButton::Right,
        _ => return None,
    })
}

/// Deliver an event to the app and sync the window title afterwards.
unsafe fn dispatch(x: *mut X, ev: Event) {
    let x = &mut *x;
    let mut host = XHost { x: x as *mut X };
    x.handler.on_event(ev, &mut host);
    let title = x.handler.title();
    if title != x.last_title {
        x.last_title = title.clone();
        host.set_title(&title);
    }
}

/// Re-read `_NET_WM_STATE`: the WM owns maximize, and a manual edge drag must
/// not fight the maximized geometry (the band press has to stand down).
unsafe fn read_maximized(x: &mut X) {
    let mut actual: Atom = 0;
    let mut format: c_int = 0;
    let mut nitems: c_ulong = 0;
    let mut after: c_ulong = 0;
    let mut data: *mut u8 = null_mut();
    let rc = XGetWindowProperty(
        x.d,
        x.win,
        x.atoms.net_wm_state,
        0,
        64,
        0,
        XA_ATOM,
        &mut actual,
        &mut format,
        &mut nitems,
        &mut after,
        &mut data,
    );
    let mut maximized = false;
    if rc == 0 && !data.is_null() {
        let atoms = std::slice::from_raw_parts(data as *const Atom, nitems as usize);
        maximized = atoms.contains(&x.atoms.net_wm_state_maximized_vert)
            && atoms.contains(&x.atoms.net_wm_state_maximized_horz);
        XFree(data as *mut c_void);
    }
    x.maximized = maximized;
}

/// Repaint the canvas and blit it to the window.
unsafe fn paint(x: *mut X) {
    let x = &mut *x;
    x.dirty = false;
    let (w, h) = (x.canvas.width as i32, x.canvas.height as i32);
    if w <= 0 || h <= 0 {
        return;
    }
    x.handler.paint(&mut x.canvas);
    let mut img = make_image(x.depth, w, h, x.canvas.pixels.as_ptr() as *mut c_char);
    XPutImage(
        x.d,
        x.win,
        x.gc,
        &mut img,
        0,
        0,
        0,
        0,
        w as c_uint,
        h as c_uint,
    );
    XFlush(x.d);
}

// ---------------------------------------------------------------- events

/// Translate one X event; returns true when the loop should exit.
unsafe fn handle_event(x: *mut X, ev: &XEvent) {
    let x = &mut *x;
    match ev.kind {
        KEY_PRESS => {
            let kev = ev as *const XEvent as *const XKeyEvent;
            let mut keysym: c_ulong = 0;
            let mut buf = [0u8; 16];
            let n = XLookupString(
                kev,
                buf.as_mut_ptr() as *mut c_char,
                buf.len() as i32,
                &mut keysym,
                null_mut(),
            );
            let mods = mods_of((*kev).state);
            if let Some(key) = key_of(keysym) {
                dispatch(x, Event::KeyDown { key, mods });
            } else if n > 0 {
                let text = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
                if let Some(c) = text.chars().next() {
                    if !c.is_control() {
                        dispatch(
                            x,
                            Event::KeyDown {
                                key: Key::Char(c),
                                mods,
                            },
                        );
                        if !mods.ctrl && !mods.alt {
                            dispatch(x, Event::Text(text));
                        }
                    }
                }
            }
        }
        BUTTON_PRESS => {
            let bev = ev as *const XEvent as *const XButtonEvent;
            let (bx, by) = ((*bev).x, (*bev).y);
            let (w, h) = (x.geom.2, x.geom.3);
            let hit = x.handler.title_hit(bx as f32, by as f32);
            // App-drawn controls (window buttons, tabs, their close/save) come
            // before the resize band: they sit flush with the window border, so
            // a band-first test stole the corner of the control under the
            // pointer. Only filler falls through to the band.
            let control = !matches!(hit, TitleHit::Drag | TitleHit::Client);
            let mut edges = 0u8;
            if !control && !x.maximized {
                // The band is the backend's maximum; the app hands back the
                // pixels its overlay scrollbars draw on so a press on a bar
                // scrolls instead of resizing (see `Handler::grip_at`).
                let band = x
                    .handler
                    .grip_at(bx as f32, by as f32, RESIZE_BAND as f32)
                    .round() as i32;
                if bx < band {
                    edges |= 1;
                }
                if bx > w - band {
                    edges |= 2;
                }
                if by < band {
                    edges |= 4;
                }
                if by > h - band {
                    edges |= 8;
                }
            }
            if edges != 0 && (*bev).button == BUTTON_LEFT {
                x.size_drag = Some((
                    (*bev).x_root,
                    (*bev).y_root,
                    x.geom.0,
                    x.geom.1,
                    w,
                    h,
                    edges,
                ));
                return;
            }
            if (*bev).button == BUTTON_LEFT && matches!(hit, TitleHit::Drag) {
                x.move_drag = Some(((*bev).x_root, (*bev).y_root, x.geom.0, x.geom.1));
                return;
            }
            match (*bev).button {
                WHEEL_UP => dispatch(x, Event::Scroll { dx: 0.0, dy: 120.0 }),
                WHEEL_DOWN => dispatch(
                    x,
                    Event::Scroll {
                        dx: 0.0,
                        dy: -120.0,
                    },
                ),
                TILT_LEFT => dispatch(
                    x,
                    Event::Scroll {
                        dx: -120.0,
                        dy: 0.0,
                    },
                ),
                TILT_RIGHT => dispatch(x, Event::Scroll { dx: 120.0, dy: 0.0 }),
                b => {
                    if let Some(button) = button_of(b) {
                        if button == MouseButton::Left {
                            // Held for the gesture: FOCUS_OUT is where a
                            // release that went to someone else is fulfilled.
                            x.left_down = true;
                        }
                        dispatch(
                            x,
                            Event::MouseDown {
                                x: bx as f32,
                                y: by as f32,
                                button,
                            },
                        );
                    }
                }
            }
        }
        BUTTON_RELEASE => {
            let bev = ev as *const XEvent as *const XButtonEvent;
            x.move_drag = None;
            x.size_drag = None;
            if (*bev).button == BUTTON_LEFT {
                x.left_down = false;
            }
            if let Some(button) = button_of((*bev).button) {
                dispatch(
                    x,
                    Event::MouseUp {
                        x: (*bev).x as f32,
                        y: (*bev).y as f32,
                        button,
                    },
                );
            }
        }
        FOCUS_OUT => {
            // A press keeps the pointer grabbed (X hands us the release
            // implicitly), but focus leaving takes that grab with it — the WM's
            // own Alt+drag, another client's popup, Alt+Tab. The release is then
            // someone else's message, and a gesture left running strands
            // exactly the things the app's drags touch: the thumb keeps
            // following the pointer, the selection keeps growing.
            x.move_drag = None;
            x.size_drag = None;
            if x.left_down {
                x.left_down = false;
                let (mx, my) = x.mouse;
                dispatch(
                    x,
                    Event::MouseUp {
                        x: mx,
                        y: my,
                        button: MouseButton::Left,
                    },
                );
            }
        }
        MOTION_NOTIFY => {
            let mev = ev as *const XEvent as *const XMotionEvent;
            let (mx, my) = ((*mev).x as f32, (*mev).y as f32);
            if let Some((px, py, wx, wy)) = x.move_drag {
                XMoveWindow(x.d, x.win, wx + (*mev).x_root - px, wy + (*mev).y_root - py);
                XFlush(x.d);
                return;
            }
            if let Some((px, py, wx, wy, w, h, edges)) = x.size_drag {
                let (dx, dy) = ((*mev).x_root - px, (*mev).y_root - py);
                let mut nx = wx;
                let mut ny = wy;
                let mut nw = w;
                let mut nh = h;
                if edges & 1 != 0 {
                    nx = wx + dx;
                    nw = w - dx;
                }
                if edges & 2 != 0 {
                    nw = w + dx;
                }
                if edges & 4 != 0 {
                    ny = wy + dy;
                    nh = h - dy;
                }
                if edges & 8 != 0 {
                    nh = h + dy;
                }
                if nw >= 200 && nh >= 100 {
                    XMoveResizeWindow(x.d, x.win, nx, ny, nw as c_uint, nh as c_uint);
                    XFlush(x.d);
                }
                return;
            }
            x.mouse = (mx, my);
            dispatch(x, Event::MouseMove { x: mx, y: my });
            // Cursor follows the pointer (arrow / I-beam / hand).
            let idx = match x.handler.cursor_at(mx, my) {
                CursorShape::Arrow => 0,
                CursorShape::IBeam => 1,
                CursorShape::Hand => 2,
            };
            if idx != x.cursor_idx {
                x.cursor_idx = idx;
                XDefineCursor(x.d, x.win, x.cursors[idx]);
            }
        }
        EXPOSE => {
            let eev = ev as *const XEvent as *const XExposeEvent;
            if (*eev).count == 0 {
                x.dirty = true;
            }
        }
        CONFIGURE_NOTIFY => {
            let cev = ev as *const XEvent as *const XConfigureEvent;
            let (w, h) = ((*cev).width, (*cev).height);
            x.geom = ((*cev).x, (*cev).y, w, h);
            report_geometry(x);
            if w != x.canvas.width as i32 || h != x.canvas.height as i32 {
                x.canvas.resize(w.max(1) as usize, h.max(1) as usize);
                x.dirty = true;
                dispatch(
                    x,
                    Event::Resized {
                        width: w.max(1) as usize,
                        height: h.max(1) as usize,
                        scale: 1.0,
                    },
                );
            }
        }
        PROPERTY_NOTIFY => {
            let pev = ev as *const XEvent as *const XPropertyEvent;
            if (*pev).atom == x.atoms.net_wm_state {
                let was = x.maximized;
                read_maximized(x);
                // The WM owns maximize: a state change we did not ask for (a
                // tile/snap key, a session manager) is how the app hears about
                // it, and it is also what a restore has to survive.
                if x.maximized != was {
                    report_geometry(x);
                }
            }
        }
        CLIENT_MESSAGE => {
            let cmev = ev as *const XEvent as *const XClientMessageEvent;
            if (*cmev).message_type == x.atoms.wm_protocols
                && (*cmev).data[0] as c_ulong == x.atoms.wm_delete
            {
                dispatch(x, Event::CloseRequested);
            }
        }
        SELECTION_REQUEST => {
            let srev = ev as *const XEvent as *const XSelectionRequestEvent;
            on_selection_request(x, &*srev);
        }
        SELECTION_CLEAR => {
            x.clip_text.clear();
        }
        DESTROY_NOTIFY => {
            x.done = true;
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- run

pub fn run(handler: Box<dyn Handler>) -> Result<()> {
    unsafe {
        let d = XOpenDisplay(null());
        if d.is_null() {
            return Err(Error::Platform(
                "XOpenDisplay failed (no X server / DISPLAY unset)".into(),
            ));
        }
        let screen = XDefaultScreen(d);
        let root = XDefaultRootWindow(d);
        let depth = XDefaultDepth(d, screen);
        // Where to open: the rect the last session was closed at when this
        // screen still shows it, else centered, like the Windows backend. Read
        // before the handler moves into `X`.
        let asked = handler.initial_window();
        let maximized = asked.is_some_and(|g| g.maximized);
        let (sw, sh) = (XDisplayWidth(d, screen), XDisplayHeight(d, screen));
        let centered = (
            ((sw - WIN_W as i32) / 2).max(0),
            ((sh - WIN_H as i32) / 2).max(0),
            WIN_W as i32,
            WIN_H as i32,
        );
        let (x0, y0, win_w, win_h) = super::open_at(asked, (0, 0, sw, sh), centered);
        let win = XCreateSimpleWindow(
            d,
            root,
            x0,
            y0,
            win_w.max(1) as c_uint,
            win_h.max(1) as c_uint,
            0,
            0,
            0,
        );
        if win == 0 {
            XCloseDisplay(d);
            return Err(Error::Platform("XCreateSimpleWindow failed".into()));
        }
        let atoms = Atoms {
            wm_protocols: intern(d, "WM_PROTOCOLS"),
            wm_delete: intern(d, "WM_DELETE_WINDOW"),
            net_wm_icon: intern(d, "_NET_WM_ICON"),
            net_wm_state: intern(d, "_NET_WM_STATE"),
            net_wm_state_maximized_vert: intern(d, "_NET_WM_STATE_MAXIMIZED_VERT"),
            net_wm_state_maximized_horz: intern(d, "_NET_WM_STATE_MAXIMIZED_HORZ"),
            motif_wm_hints: intern(d, "_MOTIF_WM_HINTS"),
            clipboard: intern(d, "CLIPBOARD"),
            targets: intern(d, "TARGETS"),
            utf8_string: intern(d, "UTF8_STRING"),
            editor_sel: intern(d, "EDITOR_SEL"),
        };
        let mut protocols = [atoms.wm_delete];
        XSetWMProtocols(d, win, protocols.as_mut_ptr(), 1);
        XSelectInput(
            d,
            win,
            KEY_PRESS_MASK
                | BUTTON_PRESS_MASK
                | BUTTON_RELEASE_MASK
                | POINTER_MOTION_MASK
                | EXPOSURE_MASK
                | FOCUS_CHANGE_MASK
                | STRUCTURE_NOTIFY_MASK
                | PROPERTY_CHANGE_MASK,
        );
        let gc = XCreateGC(d, win, 0, null());
        let cursors = [
            XCreateFontCursor(d, XC_LEFT_PTR),
            XCreateFontCursor(d, XC_XTERM),
            XCreateFontCursor(d, XC_HAND2),
        ];
        let title = handler.title();
        let mut x = Box::new(X {
            d,
            screen,
            root,
            win,
            gc,
            cursors,
            atoms,
            handler,
            canvas: Canvas::new(win_w.max(1) as usize, win_h.max(1) as usize),
            last_title: title,
            dirty: true,
            animating: false,
            done: false,
            clip_text: String::new(),
            move_drag: None,
            size_drag: None,
            geom: (x0, y0, win_w, win_h),
            mouse: (0.0, 0.0),
            left_down: false,
            depth,
            deferred: Vec::new(),
            cursor_idx: 0,
            maximized: false,
        });
        undecorate(&x);
        set_icon(&x);
        read_maximized(x.as_mut());
        {
            let t = x.last_title.clone();
            let mut host = XHost {
                x: x.as_mut() as *mut X,
            };
            host.set_title(&t);
        }
        XMapWindow(d, win);
        // The rect we opened at, before the WM has had a say.
        report_geometry(x.as_mut());
        if maximized {
            // A session that ended maximized opens maximized — at the normal
            // rect, so un-maximizing lands where the user left it. The WM owns
            // the state, so this is a request; the change comes back to the app
            // as geometry (`_NET_WM_STATE` → `PROPERTY_NOTIFY`).
            wm_state(&x, WM_STATE_ADD);
        }
        // Tell the app its initial size so the first paint lays out correctly.
        dispatch(
            x.as_mut() as *mut X,
            Event::Resized {
                width: win_w.max(1) as usize,
                height: win_h.max(1) as usize,
                scale: 1.0,
            },
        );

        let mut ev: XEvent = std::mem::zeroed();
        let mut last_tick = std::time::Instant::now();
        let mut last_frame = std::time::Instant::now();
        while !x.done {
            let mut handled = false;
            while XPending(d) > 0 {
                XNextEvent(d, &mut ev);
                handle_event(x.as_mut() as *mut X, &ev);
                handled = true;
            }
            // Replay events swallowed during a clipboard read.
            while let Some(deferred) = x.deferred.pop() {
                handle_event(x.as_mut() as *mut X, &deferred);
                handled = true;
            }
            if x.dirty {
                paint(x.as_mut() as *mut X);
            }
            if last_tick.elapsed() >= std::time::Duration::from_millis(40) {
                last_tick = std::time::Instant::now();
                dispatch(x.as_mut() as *mut X, Event::Tick);
            }
            // A glide or coast needs far more than 25 Hz; the app asks for
            // frames only while something is actually moving.
            if frame_due(x.animating, last_frame.elapsed()) {
                last_frame = std::time::Instant::now();
                dispatch(x.as_mut() as *mut X, Event::Frame);
            }
            if !handled {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        XCloseDisplay(d);
        Ok(())
    }
}
