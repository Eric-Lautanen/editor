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
//! `gsettings`. Text input goes through XIM (dead keys, Compose; CJK with
//! ibus/fcitx via `XMODIFIERS`), with the bare `XLookupString` path as the
//! fallback when no IM is there. The single-instance session lease and the
//! per-process scratch name live in `settings.rs` (F18); what is still owed
//! here is XDND drag-and-drop.
use super::{Handler, WHEEL_LINES_PER_NOTCH};
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
    sync::atomic::{AtomicBool, Ordering},
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

// F11: X11 input-method (XIM/XIC) handles, named so clippy accepts them.
type Xim = *mut c_void;
type Xic = *mut c_void;
type XimStyle = c_ulong;

#[repr(C)]
struct XimStyles {
    count_styles: u16,
    supported_styles: *mut XimStyle,
}
type Xid = c_ulong;
type Time = c_ulong;

/// F16: the resource name/class (`WM_CLASS`) that desktop shells match
/// against a `.desktop` file — how a window finds its icon and its pin
/// target. The convention is CamelCase for the class, lowercase for the
/// instance, mirroring what `StartupWMClass` will say.
#[repr(C)]
struct XClassHint {
    res_name: *const c_char,
    res_class: *const c_char,
}

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
    fn XSetClassHint(d: *mut Display, w: Window, hint: *const XClassHint) -> i32;
    fn XNextEvent(d: *mut Display, ev: *mut XEvent) -> i32;
    fn XPending(d: *mut Display) -> i32;
    fn XFlush(d: *mut Display) -> i32;
    fn XCreateGC(d: *mut Display, drawable: Xid, mask: c_ulong, values: *const c_void) -> Gc;
    fn XDefaultVisual(d: *mut Display, screen: i32) -> *mut c_void;
    fn XCreateImage(
        d: *mut Display,
        visual: *mut c_void,
        depth: c_uint,
        format: c_int,
        offset: c_int,
        data: *mut c_char,
        width: c_uint,
        height: c_uint,
        bitmap_pad: c_int,
        bytes_per_line: c_int,
    ) -> *mut XImage;
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
    fn XDeleteProperty(d: *mut Display, w: Window, property: Atom) -> i32;
    fn XConvertSelection(
        d: *mut Display,
        selection: Atom,
        target: Atom,
        property: Atom,
        requestor: Window,
        time: Time,
    ) -> i32;

    // F11: XIM input-method surface. The three X*CValues functions are
    // variadic in C — `(name, value, ..., NULL)` — declared here with a
    // bare `...` tail and called with an explicit `null_mut::<c_char>()`
    // terminator, exactly as libX11's own headers leave them for the
    // caller.
    fn XSetLocaleModifiers(mods: *const c_char) -> *mut c_char;
    fn XOpenIM(
        d: *mut Display,
        db: *mut c_void,
        res_name: *const c_char,
        res_class: *const c_char,
    ) -> Xim;
    fn XGetIMValues(im: Xim, ...) -> *mut c_char;
    fn XCreateIC(im: Xim, ...) -> Xic;
    fn XGetICValues(ic: Xic, ...) -> *mut c_char;
    fn XSetICFocus(ic: Xic);
    fn XUnsetICFocus(ic: Xic);
    fn XDestroyIC(ic: Xic);
    fn XFilterEvent(ev: *mut XEvent, w: Window) -> i32;
    fn Xutf8LookupString(
        ic: Xic,
        ev: *const XKeyEvent,
        buf: *mut c_char,
        len: i32,
        keysym: *mut c_ulong,
        status: *mut c_int,
    ) -> i32;
    fn XRegisterIMInstantiateCallback(
        d: *mut Display,
        db: *mut c_void,
        res_name: *const c_char,
        res_class: *const c_char,
        cb: extern "C" fn(*mut Display, *mut c_char, *mut c_char),
        data: *mut c_char,
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

/// `XImage`, in libX11's field order. Nothing here is ever filled in from
/// Rust: the image comes from `XCreateImage` and only the four fields
/// [`sync_image`] restates are written. The tail still has to be declared at
/// its true offsets — `XPutImage` calls through `f.put_image`, so an image
/// assembled by hand (with the tail left zero) segfaults on the first blit.
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
    bits_per_pixel: c_int,
    red_mask: c_ulong,
    green_mask: c_ulong,
    blue_mask: c_ulong,
    obdata: Xid,
    /// `struct funcs`: create_image, destroy_image, get_pixel, put_pixel,
    /// sub_image, add_pixel.
    funcs: [*mut c_void; 6],
}

// ---------------------------------------------------------------- consts

// Event types.
const KEY_PRESS: i32 = 2;
const BUTTON_PRESS: i32 = 4;
const BUTTON_RELEASE: i32 = 5;
const MOTION_NOTIFY: i32 = 6;
const FOCUS_IN: i32 = 9;
const FOCUS_OUT: i32 = 10;

// F11: Xim constants. Style bits from Xim.h; the *Nothing* pair is the
// zero-UI style the local/lib model input methods always offer, and
// `XNFilterEvents` is the event mask the IC wants added to the window's
// `XSelectInput` (the IM's own keys — skipping it loses Backspace and
// arrows inside preedit).
const XIM_PREEDIT_NOTHING: XimStyle = 0x8;
const XIM_STATUS_NOTHING: XimStyle = 0x80;
const XN_QUERY_INPUT_STYLE: &[u8] = b"queryInputStyle\0";
const XN_INPUT_STYLE: &[u8] = b"inputStyle\0";
const XN_CLIENT_WINDOW: &[u8] = b"clientWindow\0";
const XN_FOCUS_WINDOW: &[u8] = b"focusWindow\0";
const XN_FILTER_EVENTS: &[u8] = b"filterEvents\0";

/// An IM that starts after us (ibus/fcitx arriving late, restarted) cannot
/// be caught by the one `XOpenIM` at startup; libX11 calls this registered
/// callback from `XNextEvent` when one appears, `run`'s loop rebuilds the
/// IC on the next iteration. The callback context is process-global
/// because the C callback carries no Rust state pointer we can borrow.
static IM_ANNOUNCED: AtomicBool = AtomicBool::new(false);
extern "C" fn im_instantiated(_d: *mut Display, _a: *mut c_char, _b: *mut c_char) {
    IM_ANNOUNCED.store(true, Ordering::Release);
}
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
/// Selections with more than this many bytes travel as an `INCR` transfer
/// (F6): a single `XChangeProperty` request may not exceed the server's max
/// request size (about 256 KiB on a stock server), and larger text dies with
/// `BadLength` while the pasting client's SelectionNotify just never says
/// why. The chunk size below must also fit one request; it is half the
/// announcement threshold, so chunk writes are comfortably inside the cap on
/// every server this can reach.
const INCR_MIN: usize = 100_000;
const INCR_CHUNK: usize = 65_536;
/// X `PropertyNotify` change states: a property became readable
/// (`PROPERTY_NEW_VALUE`, how INCR chunks arrive) or was consumed away
/// (`PROPERTY_DELETED`, how we count the requestor's reads).
const PROPERTY_NEW_VALUE: i32 = 0;
const PROPERTY_DELETED: i32 = 1;
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
    /// The canvas's standing `XImage`, made by `XCreateImage` in `run` and
    /// re-pointed at the canvas by `sync_image` before each blit.
    image: *mut XImage,
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
    /// An `INCR` selection transfer in progress to another client (F6, see
    /// `on_selection_request` / `feed_incr`): one requestor at a time.
    incr_send: Option<IncrSend>,
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
    /// Events swallowed while waiting for a SelectionNotify, replayed later.
    deferred: Vec<XEvent>,
    /// Current cursor index (0 arrow, 1 ibeam, 2 hand) to avoid re-defining.
    cursor_idx: usize,
    /// True while the WM has the window maximized on both axes (tracked from
    /// `_NET_WM_STATE`): edge presses must not start a manual resize then.
    maximized: bool,
    /// F11: input-method handles. `im` is the connection to the input
    /// method (the local/lib model compose engine, or ibus/fcitx via
    /// `XMODIFIERS`); `ic` is the conversion context owned by this window.
    /// Either can be null: no IM, or an IM that offers no usable style —
    /// `KEY_PRESS` then falls back to `XLookupString` alone.
    im: Xim,
    ic: Xic,
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
    /// `INCR` — the announced-transfer type of the selection protocol (F6).
    incr: Atom,
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

/// Publish `WM_CLASS` (**F16**): res_name `editor`, res_class `Editor`.
/// GNOME Shell and KDE match a window to its `.desktop` entry by this pair
/// first; without it, association falls to heuristics and the dock shows a
/// generic icon for the un-pinnable window. `XSetClassHint` copies both
/// strings, so the buffers only have to live through the call.
unsafe fn set_class(x: &X) {
    let class: Vec<u8> = "Editor\0".as_bytes().to_vec();
    let name: Vec<u8> = "editor\0".as_bytes().to_vec();
    let hint = XClassHint {
        res_name: name.as_ptr() as *const c_char,
        res_class: class.as_ptr() as *const c_char,
    };
    XSetClassHint(x.d, x.win, &hint);
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

/// Is GNOME (or any desktop that honours it) asked to reduce motion?
///
/// `enable-animations` is the GTK/GNOME-wide switch; `gsettings` missing or a
/// non-GNOME desktop leaves the answer `false`, which keeps the app's own
/// animations on rather than guessing.
pub fn prefers_reduced_motion() -> bool {
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "enable-animations"])
        .output();
    out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("false"))
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
    // `editor_check_linux` compiles this module on a host that has no
    // `std::os::unix` and so no descriptor to map; that seam checks the code
    // without running it, and nothing here is mapped in that configuration.
    // Exactly one of these blocks is the function's tail (see
    // `platform::forward_to_running` for the same shape).
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
    #[cfg(unix)]
    {
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
}

/// Whether the poll loop should hand the app an animation frame now. The loop
/// wakes every millisecond, so without the interval a glide would be stepped
/// as fast as the machine can go — thousands of pointless repaints a second
/// instead of the ~120 the eye can use.
fn frame_due(animating: bool, since_last: std::time::Duration) -> bool {
    animating && since_last >= std::time::Duration::from_millis(FRAME_MS)
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

// Read the CLIPBOARD selection (UTF-8). The X11 selection protocol is
// async: we ask the owner to drop the text on our scratch property, then
// spin the event queue until the SelectionNotify lands. Events that arrive
// meanwhile are stashed and replayed by the main loop. An owner with more
// text than one request can carry answers with an `INCR` announcement
// instead (F6), and the chunks are collected as they arrive below.

/// One property read as raw bytes, deleting it on read — the consumption
/// half of a property transfer: a read-with-delete is what an INCR
/// requestor owes the owner, and its deletion is the notify the owner
/// watches. `None`: a protocol error or a property already gone;
/// `Some(empty)` is the in-band end marker of an INCR transfer.
unsafe fn read_property_bytes(d: *mut Display, win: Window, property: Atom) -> Option<Vec<u8>> {
    let mut kind: Atom = 0;
    let mut format: c_int = 0;
    let mut nitems: c_ulong = 0;
    let mut after: c_ulong = 0;
    let mut data: *mut u8 = null_mut();
    let rc = XGetWindowProperty(
        d,
        win,
        property,
        0,
        1 << 20,
        1, // delete on read
        0, // any type
        &mut kind,
        &mut format,
        &mut nitems,
        &mut after,
        &mut data,
    );
    if rc != 0 || data.is_null() {
        return None;
    }
    let bytes = if format == 8 {
        nitems as usize
    } else {
        (nitems as usize) * (format as usize / 8)
    };
    let out = std::slice::from_raw_parts(data, bytes).to_vec();
    XFree(data as *mut c_void);
    Some(out)
}

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
            // F11: the input method filters while we spool too — a filtered
            // event is consumed *here*, never replayed (it is not our
            // event and its raw form must not reach the app twice).
            if XFilterEvent(&mut ev, x.win) != 0 {
                continue;
            }
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
    // First read WITHOUT deleting: the property's type is what says whether
    // this is the answer or an INCR announcement, and the deletion is the
    // announcement's ack — treating the announcement as text would paste
    // the byte count, not the selection.
    let mut kind: Atom = 0;
    let mut format: c_int = 0;
    let mut nitems: c_ulong = 0;
    let mut after: c_ulong = 0;
    let mut data: *mut u8 = null_mut();
    let rc = XGetWindowProperty(
        x.d,
        x.win,
        property,
        0,
        1 << 20,
        0, // keep: the deletion is the protocol's trigger, a cleanup later
        0, // any type
        &mut kind,
        &mut format,
        &mut nitems,
        &mut after,
        &mut data,
    );
    if rc != 0 || data.is_null() {
        return None;
    }
    let bytes = if format == 8 {
        nitems as usize
    } else {
        (nitems as usize) * (format as usize / 8)
    };
    if kind == x.atoms.incr {
        // F6: a large selection. Delete the announcement to start the
        // transfer, then collect the chunks that arrive as NEW-value
        // notifications on this property — each read with its delete is
        // what drives the owner to write the next one — until an empty
        // chunk ends it (ICCCM 2.5). The loop mirrors the one above: other
        // events defer for the main loop, an owner that stalls ends on the
        // same spin cap, and whatever arrived is what pastes.
        XFree(data as *mut c_void);
        XDeleteProperty(x.d, x.win, property);
        let mut text: Vec<u8> = Vec::new();
        for _ in 0..2000 {
            let mut done = false;
            while XPending(x.d) > 0 {
                XNextEvent(x.d, &mut ev);
                // F11: same filtering — a filtered keystroke is consumed
                // here, never replayed.
                if XFilterEvent(&mut ev, x.win) != 0 {
                    continue;
                }
                let mut ours = false;
                if ev.kind == PROPERTY_NOTIFY {
                    let pev = &ev as *const XEvent as *const XPropertyEvent;
                    if (*pev).window == x.win
                        && (*pev).atom == property
                        && (*pev).state == PROPERTY_NEW_VALUE
                    {
                        match read_property_bytes(x.d, x.win, property) {
                            None => done = true,
                            Some(chunk) => {
                                if chunk.is_empty() {
                                    done = true;
                                } else {
                                    text.extend_from_slice(&chunk);
                                }
                            }
                        }
                        ours = true;
                    }
                }
                if !ours {
                    x.deferred.push(ev);
                }
            }
            if done {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        return Some(String::from_utf8_lossy(&text).into_owned());
    }
    // A single-property answer: consume away the property it came on, then
    // hand the bytes over like before.
    XDeleteProperty(x.d, x.win, property);
    let text = String::from_utf8_lossy(std::slice::from_raw_parts(data, bytes)).into_owned();
    XFree(data as *mut c_void);
    Some(text)
}

/// One INCR selection transfer in the sending direction (F6): what was
/// announced, and how much of it the requestor has consumed so far.
struct IncrSend {
    requestor: Window,
    property: Atom,
    target: Atom,
    /// A snapshot at the announcement: the transfer says "this many bytes"
    /// and then has to deliver exactly that, even if the app copies
    /// something else the moment after.
    text: String,
    /// Bytes of `text` written so far; when it reaches the announced total,
    /// the requestor's last deletion is answered with an empty property,
    /// which is the protocol's end-of-transfer.
    offset: usize,
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
        if x.clip_text.len() > INCR_MIN && x.incr_send.is_none() {
            // F6: one property write of this size dies on the server's max
            // request size, and the paste starves with no word of why.
            // Instead an INCR transfer begins (ICCCM 2.5): the property now
            // carries the total byte count, and its deletion — visible here
            // because the owner may select PropertyChangeMask on any window
            // — is the requestor saying "next chunk". `feed_incr` answers
            // each deletion until an empty write ends the transfer. A
            // second concurrent INCR gets the single-shot path (same reason
            // the text is snapshotted: only one promise is outstanding).
            XSelectInput(x.d, req.requestor, PROPERTY_CHANGE_MASK);
            let text: String = x.clip_text.clone();
            let target = req.target;
            let total = text.len() as c_ulong;
            XChangeProperty(
                x.d,
                req.requestor,
                req.property,
                x.atoms.incr,
                32,
                PROP_MODE_REPLACE,
                &total as *const c_ulong as *const u8,
                1,
            );
            ev.property = req.property;
            x.incr_send = Some(IncrSend {
                requestor: req.requestor,
                property: req.property,
                target,
                text,
                offset: 0,
            });
        } else {
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
        }
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

/// The sending half of an INCR transfer (F6): each deletion of our last
/// property write on the requestor's window is the requestor asking for the
/// next chunk; the empty write after the final deletion ends the transfer
/// and releases the watch on its window. Events outside the transfer come
/// back false so the caller keeps their usual meaning.
unsafe fn feed_incr(x: &mut X, ev: &XPropertyEvent) -> bool {
    let Some(t) = x.incr_send.as_mut() else {
        return false;
    };
    if ev.window != t.requestor || ev.atom != t.property || ev.state != PROPERTY_DELETED {
        return false;
    }
    if t.offset < t.text.len() {
        let end = (t.offset + INCR_CHUNK).min(t.text.len());
        XChangeProperty(
            x.d,
            t.requestor,
            t.property,
            t.target,
            8,
            PROP_MODE_REPLACE,
            t.text.as_bytes().as_ptr().add(t.offset),
            (end - t.offset) as i32,
        );
        t.offset = end;
    } else {
        XChangeProperty(
            x.d,
            t.requestor,
            t.property,
            t.target,
            8,
            PROP_MODE_REPLACE,
            std::ptr::null(),
            0,
        );
        XSelectInput(x.d, t.requestor, 0);
        x.incr_send = None;
    }
    true
}

/// Point the blit's image at the buffer it should read. The window can be
/// resized at any time and `Canvas::resize` reallocates the pixels, so the
/// pointer and the row stride are restated before every blit instead of being
/// captured once. Everything else in the header (byte order, `bits_per_pixel`,
/// the channel masks) is Xlib's own, set by `XCreateImage` when it built the
/// image for this window's depth and visual.
unsafe fn sync_image(x: &mut X) {
    let (w, h, pixels) = (
        x.canvas.width as c_int,
        x.canvas.height as c_int,
        x.canvas.pixels.as_mut_ptr() as *mut c_char,
    );
    let img = &mut *x.image;
    img.width = w;
    img.height = h;
    // 4 bytes/px: the canvas is `u32` per pixel, the same assumption every
    // backend makes about it.
    img.bytes_per_line = w * 4;
    img.data = pixels;
}

/// KeySym -> app key. Plain typing goes through `XLookupString` instead.
///
/// With `ControlMask` set, libX11 converts the buffer `XLookupString` fills
/// into an ASCII control character (0x13 for `s`), leaving the keysym alone —
/// so every Ctrl+letter chord used to fall through this function and be
/// dropped by the `is_control` guard in the `KEY_PRESS` arm below. Printing
/// keysyms now map like Windows' `map_key`: only while Ctrl or Alt is held
/// (the shortcut chords; plain typing still arrives as `Event::Text` from the
/// buffer), and lowercased so `Ctrl+Shift+S` still matches the `Key::Char('s')`
/// arms. Latin-1 keysyms 0xa0..=0xff are their Unicode codepoint.
fn key_of(keysym: c_ulong, mods: Modifiers) -> Option<Key> {
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
        // Shortcut chords only; the `mods` knock on the arm matters, since a
        // plain letter or digit must keep taking the buffer path (its Text).
        k if (mods.ctrl || mods.alt)
            && ((0x20..=0x7e).contains(&k) || (0xa0..=0xff).contains(&k)) =>
        {
            let c = char::from_u32(k as u32).unwrap_or(' ');
            Key::Char(c.to_ascii_lowercase())
        }
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
    sync_image(x);
    XPutImage(
        x.d,
        x.win,
        x.gc,
        x.image,
        0,
        0,
        0,
        0,
        w as c_uint,
        h as c_uint,
    );
    XFlush(x.d);
}

// ----------------------------------------------------------------
// F11: Xim input-method context

/// Pure negotiation: which style bit-pair to ask the IM for. The
/// `Nothing`/`Nothing` pair is the one an in-process (lib model) input
/// method offers without any callback UI, and the only one this backend
/// can host — everything else (on-the-spot preedit, status callbacks)
/// needs a window the app does not draw. Details in `pick_im`.
fn pick_style(styles: *const XimStyles) -> XimStyle {
    let want = XIM_PREEDIT_NOTHING | XIM_STATUS_NOTHING;
    if unsafe { (*styles).supported_styles.is_null() } || unsafe { (*styles).count_styles == 0 } {
        return 0;
    }
    for i in 0..unsafe { (*styles).count_styles } {
        if unsafe { *(*styles).supported_styles.add(i as usize) } == want {
            return want;
        }
    }
    0
}

/// Open the input method and build the IC for `win`. Returns
/// `(im, ic, filter_mask)`: either handle may be null (no method at all,
/// or none that offers a style we can host), in which case text arrives
/// bare through `XLookupString` as before F11 — dead keys and Compose
/// lost, but nothing broken. `filter_mask` is the IC's `XNFilterEvents`
/// value, which must be OR-ed into `XSelectInput` for the IM's own keys
/// (preedit editing) to reach it.
unsafe fn create_im_ic(d: *mut Display, win: Window) -> (Xim, Xic, c_long) {
    // Empty, not "@im=none": the latter *overrides* the user's XMODIFIERS
    // and silently disables ibus/fcitx. Empty defers to what the user set.
    XSetLocaleModifiers(c"".as_ptr());
    let im = XOpenIM(d, null_mut(), null(), null());
    if im.is_null() {
        return (null_mut(), null_mut(), 0);
    }
    let mut styles: XimStyles = std::mem::zeroed();
    XGetIMValues(
        im,
        XN_QUERY_INPUT_STYLE.as_ptr() as *const c_char,
        &mut styles as *mut XimStyles,
        null_mut::<c_char>(),
    );
    let style = pick_style(&styles);
    if style == 0 {
        return (im, null_mut(), 0);
    }
    let ic = XCreateIC(
        im,
        XN_INPUT_STYLE.as_ptr() as *const c_char,
        style,
        XN_CLIENT_WINDOW.as_ptr() as *const c_char,
        win,
        XN_FOCUS_WINDOW.as_ptr() as *const c_char,
        win,
        null_mut::<c_char>(),
    );
    if ic.is_null() {
        return (im, null_mut(), 0);
    }
    let mut filter: c_long = 0;
    XGetICValues(
        ic,
        XN_FILTER_EVENTS.as_ptr() as *const c_char,
        &mut filter as *mut c_long,
        null_mut::<c_char>(),
    );
    (im, ic, filter)
}

// ---------------------------------------------------------------- events

/// Translate one X event; returns true when the loop should exit.
unsafe fn handle_event(x: *mut X, ev: &XEvent) {
    let x = &mut *x;
    match ev.kind {
        KEY_PRESS => {
            let kev = ev as *const XEvent as *const XKeyEvent;
            let mut keysym: c_ulong = 0;
            let mut buf = [0u8; 64];
            // F11: with an IC, text is composed by the input method — dead
            // keys, Compose, ibus/fcitx preedit — and a keystroke inside a
            // composition already had its raw form filtered away at
            // `XFilterEvent`. `Xutf8LookupString` is the documented text
            // decode for an IC (`XLookupString`'s status argument is
            // implementation-private and does no composition); without an
            // IC (no IM, or an unusable one) the old bare path is kept.
            let n = if x.ic.is_null() {
                XLookupString(
                    kev,
                    buf.as_mut_ptr() as *mut c_char,
                    buf.len() as i32,
                    &mut keysym,
                    null_mut(),
                )
            } else {
                Xutf8LookupString(
                    x.ic,
                    kev,
                    buf.as_mut_ptr() as *mut c_char,
                    buf.len() as i32,
                    &mut keysym,
                    null_mut(),
                )
            };
            let mods = mods_of((*kev).state);
            if let Some(key) = key_of(keysym, mods) {
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
            // One click of the wheel or the tilt is a notch: three lines of
            // the text on screen, the same distance Win32 and AppKit move it
            // (a hardcoded pixel count made the same wheel scroll twice as
            // far on X11 as on Windows).
            let step = WHEEL_LINES_PER_NOTCH * x.handler.line_step_px();
            match (*bev).button {
                // Buttons: a real wheel and tilts — discrete steps, the
                // app may glide (F13's `precise` flag stays false).
                WHEEL_UP => dispatch(
                    x,
                    Event::Scroll {
                        dx: 0.0,
                        dy: step,
                        precise: false,
                    },
                ),
                WHEEL_DOWN => dispatch(
                    x,
                    Event::Scroll {
                        dx: 0.0,
                        dy: -step,
                        precise: false,
                    },
                ),
                TILT_LEFT => dispatch(
                    x,
                    Event::Scroll {
                        dx: -step,
                        dy: 0.0,
                        precise: false,
                    },
                ),
                TILT_RIGHT => dispatch(
                    x,
                    Event::Scroll {
                        dx: step,
                        dy: 0.0,
                        precise: false,
                    },
                ),
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
        FOCUS_IN => {
            // F11: the IM registers what window has keyboard focus; without
            // this a started composition keeps its preedit in the old
            // window after Alt+Tab.
            if !x.ic.is_null() {
                XSetICFocus(x.ic);
            }
        }
        FOCUS_OUT => {
            // F11: mirror of FOCUS_IN — the IM stops composing for us.
            if !x.ic.is_null() {
                XUnsetICFocus(x.ic);
            }
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
            // An INCR transfer in progress has first claim: it watches
            // another client's window, and its deletion events are the
            // requestor's reads, not geometry. `feed_incr` only claims what
            // belongs to it.
            if !feed_incr(x, &*pev) && (*pev).atom == x.atoms.net_wm_state {
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
            // This client lost the selection: abandon any in-flight INCR
            // transfer alongside the text it was feeding (a snapshot kept
            // beyond ownership would answer for text nobody owns anymore).
            if let Some(t) = x.incr_send.take() {
                XSelectInput(x.d, t.requestor, 0);
            }
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
            incr: intern(d, "INCR"),
        };
        let mut protocols = [atoms.wm_delete];
        XSetWMProtocols(d, win, protocols.as_mut_ptr(), 1);
        // F11: the input-method context is owned by this window. Created
        // before `XSelectInput` because the IC contributes a mask
        // (`XNFilterEvents`) that must join the base set in one call.
        let (im, ic, im_filter) = create_im_ic(d, win);
        if im.is_null() {
            // Retry on the main loop when an IM appears later.
            XRegisterIMInstantiateCallback(
                d,
                null_mut(),
                null(),
                null(),
                im_instantiated,
                null_mut(),
            );
        }
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
                | PROPERTY_CHANGE_MASK
                | im_filter,
        );
        let gc = XCreateGC(d, win, 0, null());
        let cursors = [
            XCreateFontCursor(d, XC_LEFT_PTR),
            XCreateFontCursor(d, XC_XTERM),
            XCreateFontCursor(d, XC_HAND2),
        ];
        let title = handler.title();
        let mut canvas = Canvas::new(win_w.max(1) as usize, win_h.max(1) as usize);
        // The blit goes through `XPutImage`, which calls the image's own
        // `f.put_image` — so the image must come from Xlib. It is created once
        // and never given to `XDestroyImage`: the pixels belong to the canvas
        // (Xlib only stores the pointer when `data` is non-null), and
        // destroying the image would free Rust's buffer out from under it.
        // `sync_image` restates the pointer before every blit.
        let image = XCreateImage(
            d,
            XDefaultVisual(d, screen),
            depth as c_uint,
            ZPOINTER,
            0,
            canvas.pixels.as_mut_ptr() as *mut c_char,
            win_w.max(1) as c_uint,
            win_h.max(1) as c_uint,
            32,
            0,
        );
        if image.is_null() {
            XCloseDisplay(d);
            return Err(Error::Platform("XCreateImage failed".into()));
        }
        let mut x = Box::new(X {
            d,
            screen,
            root,
            win,
            gc,
            cursors,
            atoms,
            handler,
            canvas,
            image,
            last_title: title,
            dirty: true,
            animating: false,
            done: false,
            clip_text: String::new(),
            incr_send: None,
            move_drag: None,
            size_drag: None,
            geom: (x0, y0, win_w, win_h),
            mouse: (0.0, 0.0),
            left_down: false,
            deferred: Vec::new(),
            cursor_idx: 0,
            maximized: false,
            im,
            ic,
        });
        undecorate(&x);
        set_icon(&x);
        set_class(&x);
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
                // F11: an input method consumes the keystrokes it is
                // composing (dead keys, CJK preedit). Filtering before the
                // app sees them is the protocol's own contract.
                if XFilterEvent(&mut ev, x.win) != 0 {
                    continue;
                }
                handle_event(x.as_mut() as *mut X, &ev);
                handled = true;
            }
            // F11: an IM that started after us announced itself through the
            // registered callback; rebuild here, on the loop thread.
            if x.ic.is_null() && IM_ANNOUNCED.swap(false, Ordering::AcqRel) {
                let (im2, ic2, filter2) = create_im_ic(d, x.win);
                if !im2.is_null() {
                    x.im = im2;
                    x.ic = ic2;
                    if filter2 != 0 {
                        XSelectInput(
                            d,
                            x.win,
                            KEY_PRESS_MASK
                                | BUTTON_PRESS_MASK
                                | BUTTON_RELEASE_MASK
                                | POINTER_MOTION_MASK
                                | EXPOSURE_MASK
                                | FOCUS_CHANGE_MASK
                                | STRUCTURE_NOTIFY_MASK
                                | PROPERTY_CHANGE_MASK
                                | filter2,
                        );
                    }
                }
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
        // F11: the input context is ours; the display close frees the server
        // side, but the client-side IC is released here (an IC must never
        // outlive the IM it was created from).
        if !x.ic.is_null() {
            XDestroyIC(x.ic);
        }
        XCloseDisplay(d);
        Ok(())
    }
}

// ---------------------------------------------------------------- tests

// Headless by construction: `key_of` is a pure table, and `cargo test` runs on
// a Linux runner (see `.github/workflows/ci.yml`), so the chord mapping has a
// regression test that needs no display. The live proof — open a file, send
// `ctrl+s` through the real backend, watch the mtime move — is a step in
// `tools/linux-window-smoke.sh`, because `cargo test` passing says nothing
// about a backend that is only compiled headlessly.
#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: false,
    };
    const CTRL: Modifiers = Modifiers {
        ctrl: true,
        alt: false,
        shift: false,
    };

    /// The style negotiation (`pick_style`) is the whole of F11 that runs
    /// headless: an IC needs a display, but choosing the style is a pure
    /// table scan, and the three wrong answers here are exactly the three
    /// ways the paste path can degrade silently (pick a style the
    /// backend's UI cannot host, pick none when the IM offers one, or
    /// crash on an empty/off the IM's empty list).
    #[test]
    fn the_nothing_style_is_picked_when_offered() {
        let mut offered: [XimStyle; 3] = [
            XIM_PREEDIT_NOTHING | XIM_STATUS_NOTHING,
            0x1 | 0x20, // PreeditArea|StatusArea — needs windows we do not draw
            0x2 | 0x40, // PreeditPosition|StatusCallbacks — likewise
        ];
        let styles = XimStyles {
            count_styles: 3,
            supported_styles: offered.as_mut_ptr(),
        };
        assert_eq!(
            pick_style(&styles),
            XIM_PREEDIT_NOTHING | XIM_STATUS_NOTHING,
            "the one hostable style is chosen even among ranged ones"
        );
    }

    /// Without the hostable style there is no IC — the caller must see 0
    /// and stay on the bare `XLookupString` path, not pick something
    /// `XCreateIC` would fail for anyway (or worse, succeed on and then
    /// demand preedit windows).
    #[test]
    fn a_styleless_input_method_anchors_to_the_bare_path() {
        let mut offered: [XimStyle; 2] = [0x1 | 0x20, 0x2 | 0x40];
        let styles = XimStyles {
            count_styles: 2,
            supported_styles: offered.as_mut_ptr(),
        };
        assert_eq!(pick_style(&styles), 0);
    }

    /// A method that reports no styles (or a null pointer) is the same
    /// refusal, reached without indexing anything.
    #[test]
    fn an_empty_style_list_anchors_to_none() {
        let styles = XimStyles {
            count_styles: 0,
            supported_styles: null_mut(),
        };
        assert_eq!(pick_style(&styles), 0);
    }

    /// Every Ctrl+letter chord in the legend dispatches from its keysym.
    /// These are the letters `XLookupString` reduces to control characters in
    /// the returned buffer (0x03, 0x05, 0x12, 0x13, ...), which used to make
    /// the `KEY_PRESS` arm drop the event entirely (audit F1).
    #[test]
    fn ctrl_letters_come_from_the_keysym() {
        for c in b"eorwsacxvzyftq".iter().copied() {
            assert_eq!(
                key_of(c as c_ulong, CTRL),
                Some(Key::Char(c as char)),
                "Ctrl+{}",
                c as char
            );
            // Shift held too: the keysym is the capital, the chord is not.
            assert_eq!(
                key_of((c - 32) as c_ulong, CTRL),
                Some(Key::Char(c as char)),
                "Ctrl+Shift+{}",
                (c as char).to_ascii_uppercase()
            );
        }
    }

    /// Zoom's punctuation chords survive the same path.
    #[test]
    fn ctrl_punctuation_maps() {
        for (ks, want) in [(0x3d, '='), (0x2b, '+'), (0x2d, '-'), (0x30, '0')] {
            assert_eq!(
                key_of(ks as c_ulong, CTRL),
                Some(Key::Char(want)),
                "Ctrl+{want}"
            );
        }
    }

    /// Without Ctrl/Alt, printables keep taking the buffer path: `key_of`
    /// returning `Some` here would strand plain typing (that arm dispatches
    /// `Event::Text` only in the `else if n > 0` branch).
    #[test]
    fn plain_typing_still_goes_through_the_buffer() {
        for ks in [0x20, 0x73, 0x53, 0x68, 0x30, 0x3d, 0xa0, 0xff] {
            assert_eq!(key_of(ks as c_ulong, NONE), None, "keysym {ks:#x}");
        }
    }

    /// Navigation keys and F-keys are unchanged by the chord arm.
    #[test]
    fn existing_arms_unchanged() {
        assert_eq!(key_of(XK_RETURN, CTRL), Some(Key::Enter));
        assert_eq!(key_of(XK_UP, NONE), Some(Key::Up));
        assert_eq!(key_of(XK_F1, NONE), Some(Key::F(1)));
        // Latin-1 in a chord, e.g. Ctrl+µ on a European layout.
        assert_eq!(key_of(0xb5, CTRL), Some(Key::Char('µ')));
    }
}
