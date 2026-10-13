//! Win32 backend: std + hand-written FFI to user32 / gdi32 / kernel32 / shell32
//! / comdlg32. No crates.
//!
//! Model: one borderless window (`WS_POPUP | WS_THICKFRAME`) — we draw the
//! title bar (tabs, window buttons) ourselves; `WM_NCHITTEST` maps the app's
//! `TitleHit` to native drag/resize. The portable `Canvas` (BGRA, top-down) is
//! blitted with `stretch_dibits` (Win32 `StretchDIBits`) on WM_PAINT. All
//! sizes/coordinates are *physical pixels*; `scale` = dpi/96 is reported in
//! `Event::Resized` so the app can scale fonts/layout.
//!
//! Threading: single thread. `State` lives in a leaked Box whose pointer is stored in
//! GWLP_USERDATA and freed after the message loop ends.
//!
//! TODO(M6): comdlg32 file dialogs, WM_DROPFILES,
//! parent-console attach so `--help` prints under the GUI subsystem.
//! GUI subsystem. TODO(M9): macOS parity notes live in docs/PLAN.md.
use super::{Handler, Host};
use crate::{
    error::{Error, Result},
    gfx::canvas::Canvas,
    ui::{
        event::{CursorShape, Event, Key, Modifiers, MouseButton, WindowGeometry},
        titlebar::TitleHit,
    },
};
use std::{
    ffi::c_void,
    mem,
    ptr::{null, null_mut},
};

/// Raw Win32/GDI types and imports. Rust-style names are used throughout; each
/// function carries `link_name` with its real Win32 symbol so the binary links
/// against user32/gdi32/kernel32 exactly like the C API.
mod ffi {
    use std::ffi::c_void;

    pub type Hwnd = *mut c_void;
    pub type Hinstance = *mut c_void;
    pub type Hdc = *mut c_void;
    pub type Handle = *mut c_void;
    pub type Wparam = usize;
    pub type Lparam = isize;
    pub type Lresult = isize;
    pub type WndProc = Option<unsafe extern "system" fn(Hwnd, u32, Wparam, Lparam) -> Lresult>;

    #[repr(C)]
    pub struct Point {
        pub x: i32,
        pub y: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    #[repr(C)]
    pub struct Msg {
        pub hwnd: Hwnd,
        pub message: u32,
        pub w_param: Wparam,
        pub l_param: Lparam,
        pub time: u32,
        pub pt: Point,
        pub l_private: u32,
    }

    #[repr(C)]
    pub struct WndClassExW {
        pub cb_size: u32,
        pub style: u32,
        pub wnd_proc: WndProc,
        pub cls_extra: i32,
        pub wnd_extra: i32,
        pub instance: Hinstance,
        pub icon: Handle,
        pub cursor: Handle,
        pub background: Handle,
        pub menu_name: *const u16,
        pub class_name: *const u16,
        pub icon_sm: Handle,
    }

    #[repr(C)]
    pub struct CreateStructW {
        pub create_params: *mut c_void,
        pub instance: Hinstance,
        pub menu: Handle,
        pub parent: Hwnd,
        pub cy: i32,
        pub cx: i32,
        pub y: i32,
        pub x: i32,
        pub style: i32,
        pub name: *const u16,
        pub class: *const u16,
        pub ex_style: u32,
    }

    #[repr(C)]
    pub struct PaintStruct {
        pub hdc: Hdc,
        pub erase: i32,
        pub paint_rect: Rect,
        pub restore: i32,
        pub inc_update: i32,
        pub reserved: [u8; 32],
    }

    #[repr(C)]
    pub struct BitmapInfoHeader {
        pub size: u32,
        pub width: i32,
        pub height: i32,
        pub planes: u16,
        pub bit_count: u16,
        pub compression: u32,
        pub image_size: u32,
        pub x_pels_per_meter: i32,
        pub y_pels_per_meter: i32,
        pub used: u32,
        pub important: u32,
    }

    #[repr(C)]
    pub struct BitmapInfo {
        pub header: BitmapInfoHeader,
        pub colors: [u32; 1],
    }

    /// `NCCALCSIZE_PARAMS`: we only touch `rgrc[0]` (the new client rect).
    #[repr(C)]
    pub struct NcCalcSizeParams {
        pub rgrc: [Rect; 3],
        pub lppos: *mut c_void,
    }

    /// `MINMAXINFO` for `WM_GETMINMAXINFO`. All five fields are part of the
    /// struct Win32 writes through, so they are all declared. `ptMaxPosition`
    /// is relative to the monitor origin, not the virtual-screen origin.
    #[repr(C)]
    pub struct MinMaxInfo {
        pub reserved: Point,
        pub max_size: Point,
        pub max_position: Point,
        pub min_track_size: Point,
        pub max_track_size: Point,
    }

    /// `MONITORINFO` — `cbSize` must be `size_of::<MonitorInfo>()` before the
    /// `GetMonitorInfoW` call or it fails.
    #[repr(C)]
    pub struct MonitorInfo {
        pub size: u32,
        pub monitor: Rect,
        pub work: Rect,
        pub flags: u32,
    }

    /// `OPENFILENAMEW` for `GetOpenFileNameW`.
    #[repr(C)]
    pub struct OpenFileNameW {
        pub l_struct_size: u32,
        pub hwnd_owner: Hwnd,
        pub h_instance: Hinstance,
        pub lpstr_filter: *const u16,
        pub lpstr_custom_filter: *mut u16,
        pub n_max_cust_filter: u32,
        pub n_filter_index: u32,
        pub lpstr_file: *mut u16,
        pub n_max_file: u32,
        pub lpstr_file_title: *mut u16,
        pub n_max_file_title: u32,
        pub lpstr_initial_dir: *const u16,
        pub lpstr_title: *const u16,
        pub flags: u32,
        pub n_file_offset: u16,
        pub n_file_extension: u16,
        pub lpstr_def_ext: *const u16,
        pub l_cust_data: isize,
        pub lpfn_hook: *const c_void,
        pub lp_template_name: *const u16,
        pub pv_reserved: *mut c_void,
        pub dw_reserved: u32,
        pub flags_ex: u32,
    }

    /// `KNOWNFOLDERID` / `REFKNOWNFOLDERID` — a plain GUID.
    #[repr(C)]
    pub struct Guid {
        pub data1: u32,
        pub data2: u16,
        pub data3: u16,
        pub data4: [u8; 8],
    }

    /// `FOLDERID_Documents` = {FDD39AD0-238F-46AF-ADB4-6C85480369C7}.
    pub const FOLDERID_DOCUMENTS: Guid = Guid {
        data1: 0xFDD3_9AD0,
        data2: 0x238F,
        data3: 0x46AF,
        data4: [0xAD, 0xB4, 0x6C, 0x85, 0x48, 0x03, 0x69, 0xC7],
    };

    /// `TOUCHINPUT` for `WM_TOUCH` (coordinates are screen px × 100).
    ///
    /// Field order and size matter: `GetTouchInputInfo` rejects a `cbSize`
    /// that is not exactly `sizeof(TOUCHINPUT)`, so a missing member silently
    /// disables all touch handling.
    #[repr(C)]
    pub struct TouchInput {
        pub x: i32,
        pub y: i32,
        pub source: Handle,
        /// `dwID` — the touch contact identifier.
        pub id: u32,
        pub flags: u32,
        pub mask: u32,
        pub time: u32,
        pub extra_info: usize,
        pub contact_x: u32,
        pub contact_y: u32,
    }

    /// `ICONINFO` for `CreateIconIndirect`.
    #[repr(C)]
    pub struct IconInfo {
        pub f_icon: i32,
        pub x_hotspot: u32,
        pub y_hotspot: u32,
        pub hbm_mask: Handle,
        pub hbm_color: Handle,
    }

    /// `ATTACH_PARENT_PROCESS` for `AttachConsole`.
    pub const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    /// `STD_OUTPUT_HANDLE`.
    pub const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
    /// `STD_ERROR_HANDLE`.
    pub const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4;
    /// `FILE_TYPE_CHAR` — the std handle is a console screen buffer.
    pub const FILE_TYPE_CHAR: u32 = 2;
    /// `GENERIC_WRITE` for `CreateFileW`.
    pub const GENERIC_WRITE: u32 = 0x8000_0000;
    /// `FILE_SHARE_READ | FILE_SHARE_WRITE` for `CreateFileW` on CONOUT$.
    pub const FILE_SHARE_RW: u32 = 3;
    /// `OPEN_EXISTING` for `CreateFileW`.
    pub const OPEN_EXISTING: u32 = 3;

    // Window messages
    pub const WM_DESTROY: u32 = 0x0002;
    /// The window moved; the app's remembered rect follows it.
    pub const WM_MOVE: u32 = 0x0003;
    pub const WM_SIZE: u32 = 0x0005;
    /// Focus loss (Alt+Tab, another window activated). A client drag holds the
    /// mouse, so losing focus means the release is no longer ours to receive.
    pub const WM_KILLFOCUS: u32 = 0x0008;
    /// Sent before a size/position change; the hook for clamping maximize to
    /// the monitor work area so a borderless window does not bury the taskbar.
    pub const WM_GETMINMAXINFO: u32 = 0x0024;
    pub const WM_PAINT: u32 = 0x000F;
    pub const WM_CLOSE: u32 = 0x0010;
    pub const WM_ERASEBKGND: u32 = 0x0014;
    /// Activation change; returning TRUE without DefWindowProc keeps the
    /// legacy inactive frame off our borderless client edges.
    pub const WM_NCACTIVATE: u32 = 0x0086;
    pub const WM_NCCREATE: u32 = 0x0081;
    pub const WM_NCCALCSIZE: u32 = 0x0083;
    pub const WM_NCHITTEST: u32 = 0x0084;
    pub const WM_KEYDOWN: u32 = 0x0100;
    pub const WM_CHAR: u32 = 0x0102;
    pub const WM_SYSKEYDOWN: u32 = 0x0104;
    pub const WM_SETCURSOR: u32 = 0x0020;
    pub const WM_MOUSEMOVE: u32 = 0x0200;
    pub const WM_LBUTTONDOWN: u32 = 0x0201;
    pub const WM_LBUTTONUP: u32 = 0x0202;
    pub const WM_RBUTTONDOWN: u32 = 0x0204;
    pub const WM_RBUTTONUP: u32 = 0x0205;
    pub const WM_MBUTTONDOWN: u32 = 0x0207;
    pub const WM_MBUTTONUP: u32 = 0x0208;
    pub const WM_MOUSEWHEEL: u32 = 0x020A;
    pub const WM_MOUSEHWHEEL: u32 = 0x020E;
    /// Capture changed hands, so the button-up now belongs to whoever holds it;
    /// `end_gesture` is the reconciliation point for this and `WM_KILLFOCUS`.
    pub const WM_CAPTURECHANGED: u32 = 0x0215;
    pub const WM_DROPFILES: u32 = 0x0233;
    pub const WM_TIMER: u32 = 0x0113;
    /// `WM_TIMER` ids. `TIMER_TICK` is the app's slow heartbeat (caret blink,
    /// drag auto-scroll); `TIMER_FRAME` only runs while something is animating,
    /// which is what a glide needs and what 40 ms cannot give it.
    pub const TIMER_TICK: usize = 1;
    pub const TIMER_FRAME: usize = 2;
    pub const WM_TOUCH: u32 = 0x0240;
    pub const WM_DPICHANGED: u32 = 0x02E0;
    pub const WM_SETICON: u32 = 0x0080;
    /// `ICON_SMALL` wParam for `WM_SETICON`.
    pub const ICON_SMALL: usize = 0;
    /// `ICON_BIG` wParam for `WM_SETICON`.
    pub const ICON_BIG: usize = 1;

    // Touch input flags (TOUCHEVENTF_*). The values are Win32's, in the order
    // Win32 declares them — MOVE is 0x1 and DOWN is 0x2, which is the opposite
    // of what reading the names suggests. With them swapped a finger drag does
    // nothing but end in a synthetic click at the release point (see
    // [`touch_action`]).
    pub const TOUCHEVENTF_MOVE: u32 = 0x0000_0001;
    pub const TOUCHEVENTF_DOWN: u32 = 0x0000_0002;
    pub const TOUCHEVENTF_UP: u32 = 0x0000_0004;
    /// The first contact of a gesture: the one a pan should follow.
    pub const TOUCHEVENTF_PRIMARY: u32 = 0x0000_0010;

    // Theme detection (registry)
    pub const WM_SETTINGCHANGE: u32 = 0x001A;
    /// `HKEY_CURRENT_USER`
    pub const HKEY_CURRENT_USER: isize = 0x8000_0001;
    /// `RRF_RT_REG_DWORD`
    pub const RRF_RT_REG_DWORD: u32 = 0x0000_0010;

    // Styles & misc
    pub const CS_HREDRAW: u32 = 0x0002;
    pub const CS_VREDRAW: u32 = 0x0001;
    /// Borderless custom-title-bar style: resize frame + min/max/system menu, no caption.
    pub const WS_POPUP: u32 = 0x8000_0000;
    pub const WS_SYSMENU: u32 = 0x0008_0000;
    pub const WS_THICKFRAME: u32 = 0x0004_0000;
    pub const WS_MINIMIZEBOX: u32 = 0x0002_0000;
    pub const WS_MAXIMIZEBOX: u32 = 0x0001_0000;
    pub const CW_USEDEFAULT: i32 = i32::MIN; // 0x80000000
    pub const SW_SHOW: i32 = 5;
    pub const SW_MINIMIZE: i32 = 6;
    pub const SW_MAXIMIZE: i32 = 3;
    pub const SW_RESTORE: i32 = 9;
    pub const SIZE_MINIMIZED: usize = 1;
    pub const IDC_ARROW: usize = 32512;
    /// `IDC_IBEAM` — 32513, NOT 32613 (that is OCR_IBEAM and fails to load).
    pub const IDC_IBEAM: usize = 32513;
    pub const IDC_HAND: usize = 32649;
    pub const GWLP_USERDATA: i32 = -21;
    pub const SRCCOPY: u32 = 0x00CC_0020;
    pub const DIB_RGB_COLORS: u32 = 0;
    pub const BI_RGB: u32 = 0;
    pub const SWP_NOZORDER: u32 = 0x0004;
    pub const SWP_NOACTIVATE: u32 = 0x0010;
    /// `CF_UNICODETEXT` for clipboard get/set.
    pub const CF_UNICODETEXT: u32 = 13;
    /// `GMEM_MOVEABLE` for clipboard-owned allocations.
    pub const GMEM_MOVEABLE: u32 = 0x0002;
    /// DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
    pub const DPI_PER_MONITOR_V2: isize = -4;

    // Non-client hit codes (WM_NCHITTEST / WM_SETCURSOR)
    pub const HTCLIENT: isize = 1;
    pub const HTCAPTION: isize = 2;
    pub const HTLEFT: isize = 10;
    pub const HTRIGHT: isize = 11;
    pub const HTTOP: isize = 12;
    pub const HTTOPLEFT: isize = 13;
    pub const HTTOPRIGHT: isize = 14;
    pub const HTBOTTOM: isize = 15;
    pub const HTBOTTOMLEFT: isize = 16;
    pub const HTBOTTOMRIGHT: isize = 17;

    // System metrics
    pub const SM_CXSCREEN: i32 = 0;
    pub const SM_CYSCREEN: i32 = 1;
    /// Virtual screen (every monitor, which is the desktop a remembered rect
    /// has to be reachable on).
    pub const SM_XVIRTUALSCREEN: i32 = 76;
    pub const SM_YVIRTUALSCREEN: i32 = 77;
    pub const SM_CXVIRTUALSCREEN: i32 = 78;
    pub const SM_CYVIRTUALSCREEN: i32 = 79;
    pub const SM_CXFRAME: i32 = 32;
    pub const SM_CYFRAME: i32 = 33;
    pub const SM_CXPADDEDBORDER: i32 = 92;

    /// `MONITOR_DEFAULTTONEAREST` for `MonitorFromWindow`.
    pub const MONITOR_DEFAULTTONEAREST: u32 = 2;

    // GetOpenFileNameW / GetSaveFileNameW flags
    pub const OFN_OVERWRITEPROMPT: u32 = 0x0000_0002;
    pub const OFN_HIDEREADONLY: u32 = 0x0000_0004;
    pub const OFN_NOCHANGEDIR: u32 = 0x0000_0008;
    pub const OFN_PATHMUSTEXIST: u32 = 0x0000_0800;
    pub const OFN_FILEMUSTEXIST: u32 = 0x0000_1000;

    // Virtual keys
    pub const VK_BACK: u32 = 0x08;
    pub const VK_TAB: u32 = 0x09;
    pub const VK_RETURN: u32 = 0x0D;
    pub const VK_CONTROL: i32 = 0x11;
    pub const VK_MENU: i32 = 0x12;
    pub const VK_SHIFT: i32 = 0x10;
    pub const VK_ESCAPE: u32 = 0x1B;
    pub const VK_PRIOR: u32 = 0x21;
    pub const VK_NEXT: u32 = 0x22;
    pub const VK_END: u32 = 0x23;
    pub const VK_HOME: u32 = 0x24;
    pub const VK_LEFT: u32 = 0x25;
    pub const VK_UP: u32 = 0x26;
    pub const VK_RIGHT: u32 = 0x27;
    pub const VK_DOWN: u32 = 0x28;
    pub const VK_DELETE: u32 = 0x2E;
    pub const VK_F1: u32 = 0x70;
    pub const VK_F12: u32 = 0x7B;
    pub const VK_OEM_PLUS: u32 = 0xBB;
    pub const VK_OEM_MINUS: u32 = 0xBD;

    #[link(name = "kernel32")]
    extern "system" {
        #[link_name = "GetModuleHandleW"]
        pub fn get_module_handle_w(name: *const u16) -> Hinstance;
        #[link_name = "GetModuleFileNameW"]
        pub fn get_module_file_name_w(module: Hinstance, buf: *mut u16, size: u32) -> u32;
        #[link_name = "GlobalAlloc"]
        pub fn global_alloc(flags: u32, bytes: usize) -> *mut c_void;
        #[link_name = "GlobalLock"]
        pub fn global_lock(mem: *mut c_void) -> *mut c_void;
        #[link_name = "GlobalUnlock"]
        pub fn global_unlock(mem: *mut c_void) -> i32;
        #[link_name = "GlobalFree"]
        pub fn global_free(mem: *mut c_void) -> *mut c_void;
        #[link_name = "AttachConsole"]
        pub fn attach_console(pid: u32) -> i32;
        #[link_name = "GetStdHandle"]
        pub fn get_std_handle(id: u32) -> Handle;
        #[link_name = "WriteConsoleW"]
        pub fn write_console_w(
            handle: Handle,
            buf: *const u16,
            chars: u32,
            written: *mut u32,
            reserved: *mut c_void,
        ) -> i32;
        #[link_name = "WriteFile"]
        pub fn write_file(
            handle: Handle,
            buf: *const u8,
            bytes: u32,
            written: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        #[link_name = "GetFileType"]
        pub fn get_file_type(handle: Handle) -> u32;
        #[link_name = "CloseHandle"]
        pub fn close_handle(handle: Handle) -> i32;
        #[link_name = "CreateFileW"]
        pub fn create_file_w(
            name: *const u16,
            access: u32,
            share: u32,
            security: *mut c_void,
            disposition: u32,
            flags: u32,
            template: Handle,
        ) -> Handle;
        #[link_name = "GetFileSizeEx"]
        pub fn get_file_size_ex(handle: Handle, size: *mut i64) -> i32;
        #[link_name = "CreateFileMappingW"]
        pub fn create_file_mapping_w(
            file: Handle,
            attributes: *mut c_void,
            protect: u32,
            max_high: u32,
            max_low: u32,
            name: *const u16,
        ) -> Handle;
        #[link_name = "MapViewOfFile"]
        pub fn map_view_of_file(
            mapping: Handle,
            access: u32,
            high: u32,
            low: u32,
            bytes: usize,
        ) -> *mut c_void;
    }

    #[link(name = "user32")]
    extern "system" {
        #[link_name = "SetProcessDpiAwarenessContext"]
        pub fn set_process_dpi_awareness_context(ctx: isize) -> i32;
        #[link_name = "GetDpiForWindow"]
        pub fn get_dpi_for_window(hwnd: Hwnd) -> u32;
        #[link_name = "RegisterClassExW"]
        pub fn register_class_ex_w(wc: *const WndClassExW) -> u16;
        #[link_name = "CreateWindowExW"]
        pub fn create_window_ex_w(
            ex_style: u32,
            class: *const u16,
            title: *const u16,
            style: u32,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            parent: Hwnd,
            menu: Handle,
            inst: Hinstance,
            param: *mut c_void,
        ) -> Hwnd;
        #[link_name = "DefWindowProcW"]
        pub fn def_window_proc_w(hwnd: Hwnd, msg: u32, wp: Wparam, lp: Lparam) -> Lresult;
        #[link_name = "ShowWindow"]
        pub fn show_window(hwnd: Hwnd, cmd: i32) -> i32;
        #[link_name = "UpdateWindow"]
        pub fn update_window(hwnd: Hwnd) -> i32;
        #[link_name = "DestroyWindow"]
        pub fn destroy_window(hwnd: Hwnd) -> i32;
        #[link_name = "PostQuitMessage"]
        pub fn post_quit_message(code: i32);
        #[link_name = "GetMessageW"]
        pub fn get_message_w(msg: *mut Msg, hwnd: Hwnd, min: u32, max: u32) -> i32;
        #[link_name = "TranslateMessage"]
        pub fn translate_message(msg: *const Msg) -> i32;
        #[link_name = "DispatchMessageW"]
        pub fn dispatch_message_w(msg: *const Msg) -> Lresult;
        #[link_name = "BeginPaint"]
        pub fn begin_paint(hwnd: Hwnd, ps: *mut PaintStruct) -> Hdc;
        #[link_name = "EndPaint"]
        pub fn end_paint(hwnd: Hwnd, ps: *const PaintStruct) -> i32;
        #[link_name = "InvalidateRect"]
        pub fn invalidate_rect(hwnd: Hwnd, rect: *const Rect, erase: i32) -> i32;
        #[link_name = "SetWindowTextW"]
        pub fn set_window_text_w(hwnd: Hwnd, text: *const u16) -> i32;
        #[link_name = "LoadCursorW"]
        pub fn load_cursor_w(inst: Hinstance, name: *const u16) -> Handle;
        #[link_name = "SetWindowLongPtrW"]
        pub fn set_window_long_ptr_w(hwnd: Hwnd, index: i32, value: isize) -> isize;
        #[link_name = "GetWindowLongPtrW"]
        pub fn get_window_long_ptr_w(hwnd: Hwnd, index: i32) -> isize;
        /// The window's full rect (identical to the client rect here: the
        /// borderless `WM_NCCALCSIZE` leaves no frame to subtract).
        #[link_name = "GetWindowRect"]
        pub fn get_window_rect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        #[link_name = "SetWindowPos"]
        pub fn set_window_pos(
            hwnd: Hwnd,
            after: Hwnd,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
        #[link_name = "GetKeyState"]
        pub fn get_key_state(vk: i32) -> i16;
        #[link_name = "OpenClipboard"]
        pub fn open_clipboard(hwnd: Hwnd) -> i32;
        #[link_name = "CloseClipboard"]
        pub fn close_clipboard() -> i32;
        #[link_name = "EmptyClipboard"]
        pub fn empty_clipboard() -> i32;
        #[link_name = "GetClipboardData"]
        pub fn get_clipboard_data(format: u32) -> *mut c_void;
        #[link_name = "SetClipboardData"]
        pub fn set_clipboard_data(format: u32, mem: *mut c_void) -> *mut c_void;
        #[link_name = "IsClipboardFormatAvailable"]
        pub fn is_clipboard_format_available(format: u32) -> i32;
        #[link_name = "SetCursor"]
        pub fn set_cursor(cursor: Handle) -> Handle;
        /// Hold the mouse for as long as our own drag runs (a scrollbar thumb,
        /// a text selection). A borderless window has no implicit capture: the
        /// release is delivered to whatever is under the pointer, so letting
        /// go outside the client area silently loses the `MouseUp` the app's
        /// drags end on.
        #[link_name = "SetCapture"]
        pub fn set_capture(hwnd: Hwnd) -> Hwnd;
        #[link_name = "ReleaseCapture"]
        pub fn release_capture() -> i32;
        #[link_name = "ScreenToClient"]
        pub fn screen_to_client(hwnd: Hwnd, pt: *mut Point) -> i32;
        #[link_name = "IsZoomed"]
        pub fn is_zoomed(hwnd: Hwnd) -> i32;
        #[link_name = "MonitorFromWindow"]
        pub fn monitor_from_window(hwnd: Hwnd, flags: u32) -> Handle;
        #[link_name = "GetMonitorInfoW"]
        pub fn get_monitor_info_w(monitor: Handle, info: *mut MonitorInfo) -> i32;
        #[link_name = "GetSystemMetrics"]
        pub fn get_system_metrics(index: i32) -> i32;
        #[link_name = "GetCursorPos"]
        pub fn get_cursor_pos(pt: *mut Point) -> i32;
        #[link_name = "SetTimer"]
        pub fn set_timer(hwnd: Hwnd, id: usize, ms: u32, proc: *const c_void) -> usize;
        #[link_name = "KillTimer"]
        pub fn kill_timer(hwnd: Hwnd, id: usize) -> i32;
        #[link_name = "RegisterTouchWindow"]
        pub fn register_touch_window(hwnd: Hwnd, flags: u32) -> i32;
        #[link_name = "GetTouchInputInfo"]
        pub fn get_touch_input_info(
            input: Handle,
            count: u32,
            inputs: *mut TouchInput,
            size: u32,
        ) -> i32;
        #[link_name = "CloseTouchInputHandle"]
        pub fn close_touch_input_handle(input: Handle) -> i32;
        #[link_name = "SendMessageW"]
        pub fn send_message_w(hwnd: Hwnd, msg: u32, wp: Wparam, lp: Lparam) -> Lresult;
        #[link_name = "GetDC"]
        pub fn get_dc(hwnd: Hwnd) -> Hdc;
        #[link_name = "ReleaseDC"]
        pub fn release_dc(hwnd: Hwnd, hdc: Hdc) -> i32;
        #[link_name = "CreateIconIndirect"]
        pub fn create_icon_indirect(info: *const IconInfo) -> Handle;
    }

    #[link(name = "shell32")]
    extern "system" {
        #[link_name = "DragAcceptFiles"]
        pub fn drag_accept_files(hwnd: Hwnd, accept: i32);
        #[link_name = "DragQueryFileW"]
        pub fn drag_query_file_w(drop: Handle, i_file: u32, buf: *mut u16, size: u32) -> u32;
        #[link_name = "DragFinish"]
        pub fn drag_finish(drop: Handle);
        #[link_name = "ShellExecuteW"]
        pub fn shell_execute_w(
            hwnd: Hwnd,
            op: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> Hinstance;
        /// `SHGetKnownFolderPath` — resolve a KNOWNFOLDERID to a path. The
        /// returned buffer is CoTaskMem-allocated (free with `co_task_mem_free`).
        #[link_name = "SHGetKnownFolderPath"]
        pub fn sh_get_known_folder_path(
            rfid: *const Guid,
            flags: u32,
            token: Handle,
            out: *mut *mut u16,
        ) -> i32;
    }

    #[link(name = "ole32")]
    extern "system" {
        /// Frees the buffer `SHGetKnownFolderPath` hands back.
        #[link_name = "CoTaskMemFree"]
        pub fn co_task_mem_free(p: *mut c_void);
    }

    #[link(name = "advapi32")]
    extern "system" {
        /// `RegGetValueW` — read a registry value without opening keys.
        #[link_name = "RegGetValueW"]
        pub fn reg_get_value_w(
            key: Handle,
            subkey: *const u16,
            name: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut c_void,
            size: *mut u32,
        ) -> i32;
    }

    #[link(name = "comdlg32")]
    extern "system" {
        #[link_name = "GetOpenFileNameW"]
        pub fn get_open_file_name_w(ofn: *mut OpenFileNameW) -> i32;
        #[link_name = "GetSaveFileNameW"]
        pub fn get_save_file_name_w(ofn: *mut OpenFileNameW) -> i32;
    }

    #[link(name = "gdi32")]
    extern "system" {
        #[link_name = "StretchDIBits"]
        pub fn stretch_dibits(
            hdc: Hdc,
            xd: i32,
            yd: i32,
            wd: i32,
            hd: i32,
            xs: i32,
            ys: i32,
            ws: i32,
            hs: i32,
            bits: *const c_void,
            info: *const BitmapInfo,
            usage: u32,
            rop: u32,
        ) -> i32;
        #[link_name = "CreateDIBSection"]
        pub fn create_dib_section(
            hdc: Hdc,
            info: *const BitmapInfo,
            usage: u32,
            bits: *mut *mut c_void,
            section: Handle,
            offset: u32,
        ) -> Handle;
        #[link_name = "CreateBitmap"]
        pub fn create_bitmap(
            w: i32,
            h: i32,
            planes: u32,
            bit_count: u32,
            bits: *const c_void,
        ) -> Handle;
        #[link_name = "DeleteObject"]
        pub fn delete_object(obj: Handle) -> i32;
    }
}
use ffi::*;

/// Touch travel (client px) under which a contact is a tap rather than a pan.
const TAP_SLOP: f32 = 8.0;

/// What one `TOUCHINPUT` contact is asking for, decoded from its flags.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TouchAction {
    /// Not a phase this handler acts on (a hover, a palm, a pen).
    Ignore,
    Down,
    Move,
    Up,
}

/// Decode a touch contact's flags.
///
/// This is split out from the message handler because the failure mode of
/// getting the bits wrong is invisible: the flags arrive from the driver as
/// `0x1A` on contact, `0x19` per move and `0x14` on lift-off, so reading
/// `TOUCHEVENTF_DOWN` as `0x1` (its name order, not its value) makes every
/// *down* look like a move — the pan never dispatches anything — and every
/// *move* look like a down, which resets the travel counter and ends the
/// gesture in a synthetic click where the finger came up.
fn touch_action(flags: u32) -> TouchAction {
    if flags & TOUCHEVENTF_DOWN != 0 {
        TouchAction::Down
    } else if flags & TOUCHEVENTF_MOVE != 0 {
        TouchAction::Move
    } else if flags & TOUCHEVENTF_UP != 0 {
        TouchAction::Up
    } else {
        TouchAction::Ignore
    }
}

// ---------------------------------------------------------------- state & host

struct State {
    handler: Box<dyn Handler>,
    canvas: Canvas,
    last_title: String,
    /// Pending UTF-16 high surrogate from WM_CHAR (emoji etc. arrive as two messages).
    high_surrogate: Option<u16>,
    /// Last touch contact (client px) while a touch drag pans the view.
    touch: Option<(f32, f32)>,
    /// Touch travel since the contact began: a tap barely moves.
    touch_travel: f32,
    /// A mouse message arrived while a touch was live (Windows promotes touch
    /// to mouse input). A tap then needs no synthetic click — the OS sent one.
    touch_promoted: bool,
    /// Left button held inside the client area. The app's drags last exactly
    /// that long and end only on `MouseUp`, so this is the platform's record of
    /// the gesture the app believes is in flight: `end_gesture` needs it to
    /// know whether a lost release still has to be delivered.
    left_down: bool,
    /// Last client-space pointer position, so a synthesized release lands
    /// where the pointer was last seen rather than at the origin.
    last_mouse: (f32, f32),
    /// OS dark preference as last seen; WM_SETTINGCHANGE compares against it.
    handler_dark: bool,
}

impl State {
    fn handler_prefers_dark(&self) -> bool {
        self.handler_dark
    }
}

struct WinHost {
    hwnd: Hwnd,
}

impl Host for WinHost {
    fn request_redraw(&mut self) {
        unsafe { invalidate_rect(self.hwnd, null(), 0) };
    }
    /// Start (or stop) the fast frame clock. `WM_TIMER` is coalesced to the
    /// system tick — 15.6 ms by default, whatever period is asked for — so this
    /// runs at ~64 Hz, which is at or above a typical display's refresh and far
    /// off the 25 Hz heartbeat a glide would otherwise advance on. The timer is
    /// killed again the moment nothing is moving, so it costs nothing at rest.
    fn set_animating(&mut self, on: bool) {
        unsafe {
            if on {
                set_timer(self.hwnd, TIMER_FRAME, 1, null());
            } else {
                kill_timer(self.hwnd, TIMER_FRAME);
            }
        }
    }
    fn set_title(&mut self, title: &str) {
        let w = wide(title);
        unsafe { set_window_text_w(self.hwnd, w.as_ptr()) };
    }
    fn clipboard_get(&mut self) -> Option<String> {
        unsafe {
            if is_clipboard_format_available(CF_UNICODETEXT) == 0 {
                return None;
            }
            if open_clipboard(null_mut()) == 0 {
                return None;
            }
            let text = Self::read_clipboard();
            close_clipboard();
            text
        }
    }
    fn clipboard_set(&mut self, text: &str) {
        unsafe {
            if open_clipboard(null_mut()) == 0 {
                return;
            }
            Self::write_clipboard(text);
            close_clipboard();
        }
    }
    fn quit(&mut self) {
        unsafe { destroy_window(self.hwnd) };
    }
    fn open_file_dialog(&mut self) -> Option<std::path::PathBuf> {
        unsafe {
            // Filter pairs, double-NUL terminated: label\0pattern\0…\0\0.
            let mut filter: Vec<u16> = Vec::new();
            for s in [
                "Documents (*.md;*.markdown;*.json;*.jsonl;*.ndjson;*.txt;*.log)",
                "*.md;*.markdown;*.json;*.jsonl;*.ndjson;*.txt;*.log",
                "All files (*.*)",
                "*.*",
            ] {
                filter.extend(s.encode_utf16());
                filter.push(0);
            }
            filter.push(0);
            let mut file = vec![0u16; 8192];
            let mut ofn: OpenFileNameW = mem::zeroed();
            ofn.l_struct_size = mem::size_of::<OpenFileNameW>() as u32;
            ofn.hwnd_owner = self.hwnd;
            ofn.lpstr_filter = filter.as_ptr();
            ofn.n_filter_index = 1;
            ofn.lpstr_file = file.as_mut_ptr();
            ofn.n_max_file = file.len() as u32;
            ofn.flags = OFN_HIDEREADONLY | OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST;
            if get_open_file_name_w(&mut ofn) != 0 {
                let len = file.iter().position(|&c| c == 0).unwrap_or(0);
                Some(std::path::PathBuf::from(String::from_utf16_lossy(
                    &file[..len],
                )))
            } else {
                None
            }
        }
    }
    fn save_file_dialog(&mut self, default_name: &str) -> Option<std::path::PathBuf> {
        unsafe {
            // Filter pairs, double-NUL terminated: label\0pattern\0…\0\0.
            let mut filter: Vec<u16> = Vec::new();
            for s in [
                "Markdown (*.md;*.markdown)",
                "*.md;*.markdown",
                "JSON (*.json)",
                "*.json",
                "JSON Lines (*.jsonl;*.ndjson)",
                "*.jsonl;*.ndjson",
                "Text (*.txt;*.log)",
                "*.txt;*.log",
                "All files (*.*)",
                "*.*",
            ] {
                filter.extend(s.encode_utf16());
                filter.push(0);
            }
            filter.push(0);

            let mut file: Vec<u16> = default_name.encode_utf16().collect();
            file.push(0);
            file.resize(8192, 0);

            let ext = wide("txt");
            let title = wide("Save As");
            // Default to the OS Documents folder unless the user navigates away.
            let docs = documents_dir();

            let mut ofn: OpenFileNameW = mem::zeroed();
            ofn.l_struct_size = mem::size_of::<OpenFileNameW>() as u32;
            ofn.hwnd_owner = self.hwnd;
            ofn.lpstr_filter = filter.as_ptr();
            ofn.n_filter_index = 1;
            ofn.lpstr_file = file.as_mut_ptr();
            ofn.n_max_file = file.len() as u32;
            ofn.lpstr_def_ext = ext.as_ptr();
            ofn.lpstr_title = title.as_ptr();
            if let Some(d) = docs.as_deref() {
                ofn.lpstr_initial_dir = d.as_ptr();
            }
            ofn.flags = OFN_OVERWRITEPROMPT | OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST;
            if get_save_file_name_w(&mut ofn) != 0 {
                let len = file.iter().position(|&c| c == 0).unwrap_or(0);
                if len == 0 {
                    return None;
                }
                Some(std::path::PathBuf::from(String::from_utf16_lossy(
                    &file[..len],
                )))
            } else {
                None
            }
        }
    }
    fn minimize(&mut self) {
        unsafe { show_window(self.hwnd, SW_MINIMIZE) };
    }
    fn open_url(&mut self, url: &str) {
        // ShellExecute with the "open" verb launches the default browser.
        unsafe {
            let op = wide("open");
            let w = wide(url);
            shell_execute_w(self.hwnd, op.as_ptr(), w.as_ptr(), null(), null(), SW_SHOW);
        }
    }
    fn toggle_maximize(&mut self) {
        unsafe {
            if is_zoomed(self.hwnd) != 0 {
                show_window(self.hwnd, SW_RESTORE);
            } else {
                show_window(self.hwnd, SW_MAXIMIZE);
            }
        }
    }
}

impl WinHost {
    /// Read `CF_UNICODETEXT`. Caller must hold the clipboard open.
    unsafe fn read_clipboard() -> Option<String> {
        let mem = get_clipboard_data(CF_UNICODETEXT);
        if mem.is_null() {
            return None;
        }
        let chars = global_lock(mem) as *const u16;
        if chars.is_null() {
            return None;
        }
        // NUL-terminated UTF-16 with a sanity bound.
        let mut len = 0usize;
        while len < 10_000_000 && *chars.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16(std::slice::from_raw_parts(chars, len)).ok();
        global_unlock(mem);
        text
    }

    /// Publish `CF_UNICODETEXT`. Caller must hold the clipboard open; the
    /// system owns the allocation once `SetClipboardData` succeeds.
    unsafe fn write_clipboard(text: &str) {
        if empty_clipboard() == 0 {
            return;
        }
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let mem = global_alloc(GMEM_MOVEABLE, wide.len() * 2);
        if mem.is_null() {
            return;
        }
        let dst = global_lock(mem) as *mut u16;
        if dst.is_null() {
            global_free(mem);
            return;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
        global_unlock(mem);
        if set_clipboard_data(CF_UNICODETEXT, mem).is_null() {
            global_free(mem);
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Read-only view of a file that lives as long as the process (see
/// `platform::map_file`): `None` for anything that cannot be mapped, which is
/// the caller's cue to read the file instead.
///
/// The handle and the section are deliberately left open, because the slice
/// handed out borrows the view for `'static`. Sharing is read/write/delete —
/// the same choice the browsers' font loaders make — so mapping a font never
/// locks it against an installer or another process; the price is that
/// overwriting a mapped font in place is undefined for us, which for a
/// system-installed font under a running viewer is not a real case.
pub fn map_file(path: &std::path::Path) -> Option<&'static [u8]> {
    // GENERIC_READ | share all | OPEN_EXISTING | FILE_ATTRIBUTE_NORMAL.
    const GENERIC_READ: u32 = 0x8000_0000;
    const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
    const OPEN_EXISTING: u32 = 3;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
    const PAGE_READONLY: u32 = 0x02;
    const FILE_MAP_READ: u32 = 0x0004;
    /// `CreateFileW`'s failure value; the `HANDLE` type here is a raw pointer
    /// (see `ffi`), so it is not `null`.
    const INVALID_HANDLE_VALUE: Handle = usize::MAX as *mut c_void;
    unsafe {
        let name = wide(&path.to_string_lossy());
        let file = create_file_w(
            name.as_ptr(),
            GENERIC_READ,
            SHARE_ALL,
            null_mut(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        );
        if file == INVALID_HANDLE_VALUE || file.is_null() {
            return None;
        }
        let mut size: i64 = 0;
        if get_file_size_ex(file, &mut size) == 0 || size <= 0 {
            close_handle(file);
            return None;
        }
        // A zero-sized section is an error, hence the size check above.
        let mapping = create_file_mapping_w(file, null_mut(), PAGE_READONLY, 0, 0, null());
        if mapping.is_null() {
            close_handle(file);
            return None;
        }
        let view = map_view_of_file(mapping, FILE_MAP_READ, 0, 0, 0) as *const u8;
        if view.is_null() {
            close_handle(mapping);
            close_handle(file);
            return None;
        }
        Some(std::slice::from_raw_parts(view, size as usize))
    }
}

/// The user's Documents folder as a NUL-terminated UTF-16 path, or `None`
/// when the shell cannot resolve it (the dialog then uses its own default).
fn documents_dir() -> Option<Vec<u16>> {
    unsafe {
        let mut p: *mut u16 = null_mut();
        if sh_get_known_folder_path(&FOLDERID_DOCUMENTS, 0, null_mut(), &mut p) != 0 || p.is_null()
        {
            return None;
        }
        let mut len = 0usize;
        while *p.add(len) != 0 {
            len += 1;
        }
        let mut v = std::slice::from_raw_parts(p, len).to_vec();
        co_task_mem_free(p as *mut c_void);
        v.push(0);
        Some(v)
    }
}

fn loword(v: isize) -> i32 {
    (v & 0xFFFF) as i32
}
fn hiword(v: isize) -> i32 {
    ((v >> 16) & 0xFFFF) as i32
}
/// Signed x/y from a mouse lparam (coords can be negative on multi-monitor setups).
fn xy(lp: isize) -> (f32, f32) {
    (
        (lp & 0xFFFF) as i16 as f32,
        ((lp >> 16) & 0xFFFF) as i16 as f32,
    )
}

fn key_down(vk: i32) -> bool {
    unsafe { get_key_state(vk) < 0 }
}

/// Absolute path of the running exe (for `--install` registration).
pub fn exe_path() -> Option<std::path::PathBuf> {
    unsafe {
        let mut buf = vec![0u16; 1024];
        let n = get_module_file_name_w(null_mut(), buf.as_mut_ptr(), buf.len() as u32);
        if n == 0 || n as usize >= buf.len() {
            return None;
        }
        Some(std::path::PathBuf::from(String::from_utf16_lossy(
            &buf[..n as usize],
        )))
    }
}

/// Console output for the GUI-subsystem binary: attach to the parent
/// console once, then write UTF-16 to the screen buffer (or raw UTF-8 when
/// stdout is redirected to a file/pipe, e.g. `cargo run` or `--pretty > f`).
pub fn print_out(s: &str) {
    write_std(STD_OUTPUT_HANDLE, s);
}

/// `print_out` for stderr.
pub fn print_err(s: &str) {
    write_std(STD_ERROR_HANDLE, s);
}

fn write_std(handle_id: u32, s: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ATTACHED: AtomicBool = AtomicBool::new(false);
    if !ATTACHED.swap(true, Ordering::Relaxed) {
        // Best effort: fails when the parent has no console (double-click).
        unsafe { attach_console(ATTACH_PARENT_PROCESS) };
    }
    unsafe {
        // The std handle is a console, a redirected file/pipe, or (when the
        // GUI subsystem started us detached) NULL — in which case the attach
        // above gives us a console via CONOUT$.
        let mut h = get_std_handle(handle_id);
        let mut conout = false;
        if h.is_null() {
            let name = wide("CONOUT$");
            h = create_file_w(
                name.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_RW,
                null_mut(),
                OPEN_EXISTING,
                0,
                null_mut(),
            );
            conout = !h.is_null();
        }
        if h.is_null() {
            return;
        }
        if get_file_type(h) == FILE_TYPE_CHAR {
            let w: Vec<u16> = s.encode_utf16().collect();
            let mut n = 0u32;
            for chunk in w.chunks(8192) {
                write_console_w(h, chunk.as_ptr(), chunk.len() as u32, &mut n, null_mut());
            }
        } else {
            let mut bytes = s.as_bytes();
            while !bytes.is_empty() {
                let mut n = 0u32;
                if write_file(h, bytes.as_ptr(), bytes.len() as u32, &mut n, null_mut()) == 0
                    || n == 0
                {
                    break;
                }
                bytes = &bytes[n as usize..];
            }
        }
        if conout {
            close_handle(h);
        }
    }
}

/// The Windows dark-mode preference: `AppsUseLightTheme` (0 = dark).
pub fn system_prefers_dark() -> bool {
    unsafe {
        let subkey = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        let name = wide("AppsUseLightTheme");
        let mut value: u32 = 0;
        let mut size: u32 = 4;
        let rc = reg_get_value_w(
            HKEY_CURRENT_USER as Handle,
            subkey.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut value as *mut u32 as *mut c_void,
            &mut size,
        );
        rc == 0 && value == 0
    }
}

/// Horizontal frame thickness Win32 adds around a *maximized* `WS_THICKFRAME`
/// window (frame plus padded border). Used to compensate `WM_NCCALCSIZE` and
/// to build the maximize rect, so both agree and maximize lands on the work
/// area. Not the resize strip — see `resize_band`.
fn frame_inset() -> i32 {
    unsafe { (get_system_metrics(SM_CXFRAME) + get_system_metrics(SM_CXPADDEDBORDER)).max(4) }
}

/// Vertical frame thickness; see `frame_inset`.
fn frame_inset_y() -> i32 {
    unsafe { (get_system_metrics(SM_CYFRAME) + get_system_metrics(SM_CXPADDEDBORDER)).max(4) }
}

/// Logical thickness (96-dpi px) of the widest strip along the window edge
/// where a press starts a resize: the classic sizing border, matching what
/// Windows itself gives a `WS_THICKFRAME` window (8 px on current Windows, and
/// smaller where the theme or a user's accessibility settings say so).
///
/// This is the *maximum* the backend offers. A wide strip forces an overlay
/// scrollbar to float away from the edge to stay clickable (strip and bar
/// cannot share pixels), so the app narrows it to just the points where a bar
/// needs the pixels — see `Handler::grip_at`.
const EDGE_BAND_LOGICAL: f32 = 8.0;

/// `hwnd`'s monitor DPI as a 96-dpi scale factor.
fn dpi_scale(hwnd: Hwnd) -> f32 {
    let dpi = unsafe { get_dpi_for_window(hwnd) };
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

/// Physical resize strip width for `hwnd`'s monitor DPI.
fn resize_band(hwnd: Hwnd) -> i32 {
    (EDGE_BAND_LOGICAL * dpi_scale(hwnd)).round().max(2.0) as i32
}

/// The widest window-edge strip this backend claims for resizing, in logical
/// px. `App` scales it by the window DPI and keeps its overlay scrollbars clear
/// of it (see `platform::edge_grip`).
pub fn edge_grip() -> f32 {
    EDGE_BAND_LOGICAL
}

/// Maximize geometry for a borderless window, as `WM_GETMINMAXINFO` wants it:
/// the window's `(max position, max size)`.
///
/// The client rect is what must land on the monitor work area (`rcWork`, i.e.
/// everything the taskbar/docked bars leave free), but the maximized
/// `WM_NCCALCSIZE` handler insets the client by the frame on each side, and
/// `ptMaxSize` is the *window*, so the frame is added back here. `work` and
/// `monitor` are in virtual-screen coords and may be negative; `ptMaxPosition`
/// is relative to the monitor's own origin — using the raw `work.left/top`
/// there offsets every window on a secondary monitor.
fn maximize_metrics(monitor: Rect, work: Rect, fx: i32, fy: i32) -> (Point, Point) {
    let position = Point {
        x: work.left - monitor.left - fx,
        y: work.top - monitor.top - fy,
    };
    let size = Point {
        x: (work.right - work.left) + 2 * fx,
        y: (work.bottom - work.top) + 2 * fy,
    };
    (position, size)
}

fn modifiers() -> Modifiers {
    Modifiers {
        ctrl: key_down(VK_CONTROL),
        alt: key_down(VK_MENU),
        shift: key_down(VK_SHIFT),
    }
}

fn map_key(vk: u32, mods: Modifiers) -> Option<Key> {
    Some(match vk {
        VK_BACK => Key::Backspace,
        VK_TAB => Key::Tab,
        VK_RETURN => Key::Enter,
        VK_ESCAPE => Key::Escape,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_END => Key::End,
        VK_HOME => Key::Home,
        VK_LEFT => Key::Left,
        VK_UP => Key::Up,
        VK_RIGHT => Key::Right,
        VK_DOWN => Key::Down,
        VK_DELETE => Key::Delete,
        VK_F1..=VK_F12 => Key::F((vk - VK_F1 + 1) as u8),
        // Shortcut chords only; plain typing arrives via WM_CHAR as Event::Text.
        0x30..=0x39 | 0x41..=0x5A if mods.ctrl || mods.alt => {
            Key::Char((vk as u8 as char).to_ascii_lowercase())
        }
        VK_OEM_PLUS if mods.ctrl => Key::Char('+'),
        VK_OEM_MINUS if mods.ctrl => Key::Char('-'),
        _ => return None,
    })
}

// ---------------------------------------------------------------- window proc

/// Which event a `WM_TIMER` id means. Two clocks share the one message, so the
/// id is the whole routing: the frame timer is a glide's clock, and the other
/// one is the app's heartbeat (caret blink, drag auto-scroll).
fn timer_event(id: usize) -> Event {
    if id == TIMER_FRAME {
        Event::Frame
    } else {
        Event::Tick
    }
}

/// Deliver an event, then sync the window title if the app's title changed.
fn dispatch(st: &mut State, hwnd: Hwnd, ev: Event) {
    let mut host = WinHost { hwnd };
    st.handler.on_event(ev, &mut host);
    let t = st.handler.title();
    if t != st.last_title {
        host.set_title(&t);
        st.last_title = t;
    }
}

/// Deliver the release that is not coming.
///
/// The app's drags — a scrollbar thumb, a text selection — end on `MouseUp` and
/// on nothing else, so the platform has to produce that event on every path
/// where the real one cannot arrive: capture taken away mid-drag, or the window
/// deactivated (Alt+Tab) with the button still down. Without this the drag
/// survives the release and keeps following the pointer, which is exactly what
/// a bar does when the pointer is let go outside the client area and Windows
/// hands the `WM_LBUTTONUP` to whatever is under it.
///
/// Idempotent, and only ever fires when a press is still on the books, so the
/// normal path (release with the button down) must clear `left_down` first —
/// `release_capture` re-enters this window proc with `WM_CAPTURECHANGED`.
fn end_gesture(st: &mut State, hwnd: Hwnd) {
    if !st.left_down {
        return;
    }
    st.left_down = false;
    let (x, y) = st.last_mouse;
    dispatch(
        st,
        hwnd,
        Event::MouseUp {
            x,
            y,
            button: MouseButton::Left,
        },
    );
}

/// Tell the app where the window is and whether it is maximized.
///
/// The app remembers only the normal rect (the place to reopen at), so this
/// fires for every OS-driven change — `WM_MOVE`, `WM_SIZE`, and the maximize
/// the window manager does on its own (double-clicking the title bar, Aero
/// Snap, a monitor coming back) — and not just for the ones the app asked for.
unsafe fn report_geometry(st: &mut State, hwnd: Hwnd) {
    let mut r: Rect = mem::zeroed();
    if get_window_rect(hwnd, &mut r) == 0 {
        return;
    }
    dispatch(
        st,
        hwnd,
        Event::WindowGeometry(WindowGeometry {
            x: r.left,
            y: r.top,
            width: (r.right - r.left).max(0) as u32,
            height: (r.bottom - r.top).max(0) as u32,
            maximized: is_zoomed(hwnd) != 0,
        }),
    );
}

unsafe fn paint(hwnd: Hwnd, st: &mut State) {
    let mut ps: PaintStruct = mem::zeroed();
    let hdc = begin_paint(hwnd, &mut ps);
    let (w, h) = (st.canvas.width, st.canvas.height);
    if w > 0 && h > 0 {
        st.handler.paint(&mut st.canvas);
        let mut bmi: BitmapInfo = mem::zeroed();
        bmi.header.size = mem::size_of::<BitmapInfoHeader>() as u32;
        bmi.header.width = w as i32;
        bmi.header.height = -(h as i32); // negative = top-down rows
        bmi.header.planes = 1;
        bmi.header.bit_count = 32;
        bmi.header.compression = BI_RGB;
        stretch_dibits(
            hdc,
            0,
            0,
            w as i32,
            h as i32,
            0,
            0,
            w as i32,
            h as i32,
            st.canvas.pixels.as_ptr() as *const c_void,
            &bmi,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
    end_paint(hwnd, &ps);
}

unsafe extern "system" fn wndproc(hwnd: Hwnd, msg: u32, wp: Wparam, lp: Lparam) -> Lresult {
    if msg == WM_NCCREATE {
        let cs = lp as *const CreateStructW;
        set_window_long_ptr_w(hwnd, GWLP_USERDATA, (*cs).create_params as isize);
        return def_window_proc_w(hwnd, msg, wp, lp);
    }
    let state = get_window_long_ptr_w(hwnd, GWLP_USERDATA) as *mut State;
    if state.is_null() {
        return def_window_proc_w(hwnd, msg, wp, lp);
    }
    let st = &mut *state;

    // While a touch contact is live the finger owns the pointer. Windows
    // promotes touch to mouse messages; letting those through would start a
    // text selection instead of panning the document. Swallow them, and note
    // that promotion happened so the WM_TOUCH handler knows a tap already
    // produced a real click.
    if st.touch.is_some()
        && matches!(
            msg,
            WM_MOUSEMOVE
                | WM_LBUTTONDOWN
                | WM_RBUTTONDOWN
                | WM_MBUTTONDOWN
                | WM_LBUTTONUP
                | WM_RBUTTONUP
                | WM_MBUTTONUP
        )
    {
        st.touch_promoted = true;
        return 0;
    }

    match msg {
        WM_NCACTIVATE => {
            // Borderless: DefWindowProc paints the legacy inactive caption/
            // frame whenever activation changes (focus loss, minimize/
            // restore) — the grey band that appears when the app is not
            // focused. Handle the message ourselves and let the app repaint;
            // wParam == 0 (deactivate) with a -1 lParam keeps visual styles.
            1
        }
        WM_GETMINMAXINFO => {
            // A borderless WS_POPUP window maximizes to the whole monitor,
            // burying the taskbar (and any docked app bar) — the work area is
            // ignored. Clamp maximize to this window's monitor work area, then
            // add the frame back on each side so the maximized WM_NCCALCSIZE
            // inset below lands the client rect exactly on the work area.
            let mmi = &mut *(lp as *mut MinMaxInfo);
            let monitor = monitor_from_window(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info: MonitorInfo = mem::zeroed();
            info.size = mem::size_of::<MonitorInfo>() as u32;
            if !monitor.is_null() && get_monitor_info_w(monitor, &mut info) != 0 {
                let (fx, fy) = (frame_inset(), frame_inset_y());
                let (position, size) = maximize_metrics(info.monitor, info.work, fx, fy);
                mmi.max_position = position;
                mmi.max_size = size;
            }
            0
        }
        WM_NCCALCSIZE => {
            // Borderless: the client area is the whole window. When maximized a
            // WS_THICKFRAME window extends past the monitor by the frame, so
            // pull the client rect back in — the insets must match the frame
            // added back in WM_GETMINMAXINFO or maximize drifts off the work
            // area (and clips under the taskbar).
            if wp != 0 {
                let p = lp as *mut NcCalcSizeParams;
                if is_zoomed(hwnd) != 0 {
                    let (fx, fy) = (frame_inset(), frame_inset_y());
                    (*p).rgrc[0].left += fx;
                    (*p).rgrc[0].top += fy;
                    (*p).rgrc[0].right -= fx;
                    (*p).rgrc[0].bottom -= fy;
                }
                0
            } else {
                def_window_proc_w(hwnd, msg, wp, lp)
            }
        }
        WM_NCHITTEST => {
            // Screen -> client, then: app-drawn controls, resize edges, then
            // the title bar (Drag -> caption so the OS moves the window).
            // Controls win over the resize band: the window buttons and tabs
            // are flush with the top/right border, so a band-first test ate
            // the corner of the button the user aimed at.
            let mut pt = Point {
                x: loword(lp),
                y: hiword(lp),
            };
            screen_to_client(hwnd, &mut pt);
            let hit = st.handler.title_hit(pt.x as f32, pt.y as f32);
            if !matches!(hit, TitleHit::Drag | TitleHit::Client) {
                return HTCLIENT;
            }
            if is_zoomed(hwnd) == 0 {
                let (x, y) = (pt.x, pt.y);
                let (w, h) = (st.canvas.width, st.canvas.height);
                // Self-tuning: the app gives the points it draws on back to
                // itself (see `Handler::grip_at`), the rest of the edge keeps
                // the full band. Evaluated per point, so a bar can hold the
                // strip at its own pixels while the band widens around it.
                let band = resize_band(hwnd) as f32;
                let g = st.handler.grip_at(x as f32, y as f32, band);
                let (px, py) = (x as f32, y as f32);
                let (w, h) = (w as f32, h as f32);
                let edge = match (px < g, px >= w - g, py < g, py >= h - g) {
                    (true, _, true, _) => HTTOPLEFT,
                    (true, _, _, true) => HTBOTTOMLEFT,
                    (_, true, true, _) => HTTOPRIGHT,
                    (_, true, _, true) => HTBOTTOMRIGHT,
                    (true, ..) => HTLEFT,
                    (_, true, ..) => HTRIGHT,
                    (_, _, true, _) => HTTOP,
                    (_, _, _, true) => HTBOTTOM,
                    _ => 0,
                };
                if edge != 0 {
                    return edge;
                }
            }
            match hit {
                TitleHit::Drag => HTCAPTION,
                _ => HTCLIENT,
            }
        }
        WM_SETCURSOR => {
            // Client area cursors come from the app; the OS handles the rest
            // (resize arrows etc.).
            if loword(lp) == HTCLIENT as i32 {
                let mut pt = Point { x: 0, y: 0 };
                get_cursor_pos(&mut pt);
                screen_to_client(hwnd, &mut pt);
                let id = match st.handler.cursor_at(pt.x as f32, pt.y as f32) {
                    CursorShape::Arrow => IDC_ARROW,
                    CursorShape::IBeam => IDC_IBEAM,
                    CursorShape::Hand => IDC_HAND,
                };
                set_cursor(load_cursor_w(null_mut(), id as *const u16));
                1
            } else {
                def_window_proc_w(hwnd, msg, wp, lp)
            }
        }
        WM_TOUCH => {
            // Touch drags pan the document; a tap (no travel) becomes a click.
            let count = (wp & 0xFFFF) as u32;
            let mut inputs: Vec<TouchInput> = Vec::new();
            inputs.resize_with(count as usize, || TouchInput {
                x: 0,
                y: 0,
                source: null_mut(),
                id: 0,
                flags: 0,
                mask: 0,
                time: 0,
                extra_info: 0,
                contact_x: 0,
                contact_y: 0,
            });
            let hinput = lp as Handle;
            if get_touch_input_info(
                hinput,
                count,
                inputs.as_mut_ptr(),
                mem::size_of::<TouchInput>() as u32,
            ) != 0
            {
                // A two-finger gesture reports both contacts: follow the
                // primary one, or the pan would jump between fingers. A
                // digitizer that marks none still pans.
                let marked = inputs.iter().any(|t| t.flags & TOUCHEVENTF_PRIMARY != 0);
                for t in &inputs {
                    if marked && t.flags & TOUCHEVENTF_PRIMARY == 0 {
                        continue;
                    }
                    // TOUCHINPUT coordinates are screen px × 100; the app
                    // speaks client px.
                    let mut pt = Point {
                        x: t.x / 100,
                        y: t.y / 100,
                    };
                    screen_to_client(hwnd, &mut pt);
                    let (x, y) = (pt.x as f32, pt.y as f32);
                    if touch_action(t.flags) == TouchAction::Down && st.touch.is_none() {
                        st.touch = Some((x, y));
                        st.touch_travel = 0.0;
                        st.touch_promoted = false;
                    } else if touch_action(t.flags) == TouchAction::Move {
                        if let Some((lx, ly)) = st.touch {
                            let (dx, dy) = (x - lx, y - ly);
                            st.touch_travel += dx.abs() + dy.abs();
                            if dx != 0.0 || dy != 0.0 {
                                // A finger is direct manipulation: `Pan`, not a
                                // wheel-like step the app may glide to.
                                dispatch(st, hwnd, Event::Pan { dx, dy });
                            }
                        }
                        st.touch = Some((x, y));
                    } else if touch_action(t.flags) == TouchAction::Up {
                        // A tap's promoted click was swallowed above, so issue
                        // one here. (When the OS did promote, it already
                        // delivered the click and we must not double it.)
                        if let Some((lx, ly)) = st.touch {
                            if !st.touch_promoted && st.touch_travel <= TAP_SLOP {
                                dispatch(
                                    st,
                                    hwnd,
                                    Event::MouseDown {
                                        x: lx,
                                        y: ly,
                                        button: MouseButton::Left,
                                    },
                                );
                                dispatch(
                                    st,
                                    hwnd,
                                    Event::MouseUp {
                                        x: lx,
                                        y: ly,
                                        button: MouseButton::Left,
                                    },
                                );
                            }
                        }
                        st.touch = None;
                    }
                }
            }
            close_touch_input_handle(hinput);
            0
        }
        WM_SETTINGCHANGE => {
            // Theme flips arrive as "ImmersiveColorSet"; re-read the registry
            // and let the app decide (it ignores manual Ctrl+T overrides).
            if system_prefers_dark() != st.handler_prefers_dark() {
                let dark = system_prefers_dark();
                st.handler_dark = dark;
                dispatch(st, hwnd, Event::ThemeChanged { dark });
            }
            def_window_proc_w(hwnd, msg, wp, lp)
        }
        WM_TIMER => {
            dispatch(st, hwnd, timer_event(wp));
            0
        }
        WM_DROPFILES => {
            // Open every dropped file as a tab (last one ends up active).
            let drop = wp as Handle;
            let count = drag_query_file_w(drop, 0xFFFF_FFFF, null_mut(), 0);
            for i in 0..count {
                let len = drag_query_file_w(drop, i, null_mut(), 0);
                if len == 0 {
                    continue;
                }
                let mut buf = vec![0u16; len as usize + 1];
                drag_query_file_w(drop, i, buf.as_mut_ptr(), buf.len() as u32);
                let path = String::from_utf16_lossy(&buf[..len as usize]);
                dispatch(st, hwnd, Event::FileOpen(std::path::PathBuf::from(path)));
            }
            drag_finish(drop);
            0
        }
        WM_PAINT => {
            paint(hwnd, st);
            0
        }
        WM_ERASEBKGND => 1, // we paint every pixel; avoids flicker
        WM_SIZE => {
            // Anything but a minimize is a real geometry change — including a
            // maximize the app never asked for — so `report_geometry` reads the
            // rect and the maximized state back off the window.
            if wp != SIZE_MINIMIZED {
                let (w, h) = (loword(lp) as usize, hiword(lp) as usize);
                if w > 0 && h > 0 {
                    st.canvas.resize(w, h);
                    let scale = dpi_scale(hwnd);
                    dispatch(
                        st,
                        hwnd,
                        Event::Resized {
                            width: w,
                            height: h,
                            scale,
                        },
                    );
                    invalidate_rect(hwnd, null(), 0);
                }
                report_geometry(st, hwnd);
            }
            0
        }
        WM_MOVE => {
            report_geometry(st, hwnd);
            0
        }
        WM_DPICHANGED => {
            // lParam points to the OS-suggested new window rect; adopt it (triggers WM_SIZE).
            let r = *(lp as *const Rect);
            set_window_pos(
                hwnd,
                null_mut(),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            0
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let mods = modifiers();
            if let Some(key) = map_key(wp as u32, mods) {
                dispatch(st, hwnd, Event::KeyDown { key, mods });
                // Let Alt+key reach DefWindowProc (system menu, Alt+F4) unless we consumed a chord.
                if msg == WM_KEYDOWN {
                    return 0;
                }
            }
            def_window_proc_w(hwnd, msg, wp, lp)
        }
        WM_CHAR => {
            let unit = wp as u16;
            let ch = if (0xD800..0xDC00).contains(&unit) {
                st.high_surrogate = Some(unit);
                None
            } else if (0xDC00..0xE000).contains(&unit) {
                st.high_surrogate
                    .take()
                    .and_then(|hi| char::decode_utf16([hi, unit]).next().and_then(|r| r.ok()))
            } else {
                char::from_u32(unit as u32)
            };
            if let Some(c) = ch {
                // Skip control codes (Enter/Backspace/Tab/Esc come through KeyDown) and DEL.
                if !c.is_control() {
                    dispatch(st, hwnd, Event::Text(c.to_string()));
                }
            }
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lp);
            st.last_mouse = (x, y);
            dispatch(st, hwnd, Event::MouseMove { x, y });
            0
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let (x, y) = xy(lp);
            let button = match msg {
                WM_LBUTTONDOWN => MouseButton::Left,
                WM_RBUTTONDOWN => MouseButton::Right,
                _ => MouseButton::Middle,
            };
            st.last_mouse = (x, y);
            if button == MouseButton::Left {
                // Hold the mouse for the gesture. A borderless window gets no
                // implicit capture, so without this a drag that leaves the
                // client area loses its release and the app's thumb stays
                // glued to the pointer (see `end_gesture`).
                st.left_down = true;
                set_capture(hwnd);
            }
            dispatch(st, hwnd, Event::MouseDown { x, y, button });
            0
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            let (x, y) = xy(lp);
            let button = match msg {
                WM_LBUTTONUP => MouseButton::Left,
                WM_RBUTTONUP => MouseButton::Right,
                _ => MouseButton::Middle,
            };
            st.last_mouse = (x, y);
            if button == MouseButton::Left {
                // The real release: off the books before capture goes back, or
                // the `WM_CAPTURECHANGED` that follows would fulfil the
                // gesture a second time.
                st.left_down = false;
            }
            dispatch(st, hwnd, Event::MouseUp { x, y, button });
            if button == MouseButton::Left {
                release_capture();
            }
            0
        }
        WM_CAPTURECHANGED => {
            // The release will be delivered elsewhere: fulfil it now so the
            // app is not left holding a thumb.
            end_gesture(st, hwnd);
            def_window_proc_w(hwnd, msg, wp, lp)
        }
        WM_KILLFOCUS => {
            // A touch stream that never saw its `TOUCHEVENTF_UP` (contact lost,
            // window deactivated mid-touch) would hold `st.touch` forever, and
            // with it swallow every mouse message — the mouse is dead until the
            // app is restarted. Losing focus cancels the stream, so the same
            // applies to the button it is holding.
            st.touch = None;
            st.touch_promoted = false;
            end_gesture(st, hwnd);
            def_window_proc_w(hwnd, msg, wp, lp)
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            // HIWORD(wParam) is a signed delta in multiples of 120 (WHEEL_DELTA).
            let delta = ((wp >> 16) & 0xFFFF) as u16 as i16 as f32 / 120.0;
            let dpi = get_dpi_for_window(hwnd);
            let px = 48.0 * if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }; // ~3 lines per notch
            let (dx, dy) = if msg == WM_MOUSEWHEEL {
                (0.0, delta * px)
            } else {
                (delta * px, 0.0)
            };
            dispatch(st, hwnd, Event::Scroll { dx, dy });
            0
        }
        WM_CLOSE => {
            // App decides (e.g. unsaved-changes prompt later); it calls host.quit() -> destroy_window.
            dispatch(st, hwnd, Event::CloseRequested);
            0
        }
        WM_DESTROY => {
            post_quit_message(0);
            0
        }
        _ => def_window_proc_w(hwnd, msg, wp, lp),
    }
}

// ---------------------------------------------------------------- entry

/// Build the window icon from the portable atlas-rendered pixels: a 32bpp
/// top-down DIB for color plus an empty mask, wrapped by `CreateIconIndirect`.
/// The system copies the bitmaps, so they are freed immediately after.
unsafe fn make_icon() -> Handle {
    let px = crate::gfx::icon::argb32();
    let mut bmi: BitmapInfo = mem::zeroed();
    bmi.header.size = mem::size_of::<BitmapInfoHeader>() as u32;
    bmi.header.width = 32;
    bmi.header.height = -32; // top-down rows
    bmi.header.planes = 1;
    bmi.header.bit_count = 32;
    bmi.header.compression = BI_RGB;
    let mut bits: *mut c_void = null_mut();
    let hdc = get_dc(null_mut());
    let color = create_dib_section(hdc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    release_dc(null_mut(), hdc);
    if color.is_null() || bits.is_null() {
        return null_mut();
    }
    std::ptr::copy_nonoverlapping(px.as_ptr() as *const c_void, bits, px.len() * 4);
    let mask = create_bitmap(32, 32, 1, 1, null());
    let icon = create_icon_indirect(&IconInfo {
        f_icon: 1,
        x_hotspot: 0,
        y_hotspot: 0,
        hbm_mask: mask,
        hbm_color: color,
    });
    delete_object(color);
    delete_object(mask);
    icon
}

pub fn run(handler: Box<dyn Handler>) -> Result<()> {
    unsafe {
        // Best effort: fails harmlessly on pre-1703 Windows 10.
        set_process_dpi_awareness_context(DPI_PER_MONITOR_V2);

        let hinst = get_module_handle_w(null());
        let class = wide("ReaditorWindow");
        let icon = make_icon();
        let wc = WndClassExW {
            cb_size: mem::size_of::<WndClassExW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            wnd_proc: Some(wndproc),
            cls_extra: 0,
            wnd_extra: 0,
            instance: hinst,
            icon,
            cursor: load_cursor_w(null_mut(), IDC_ARROW as *const u16),
            background: null_mut(),
            menu_name: null(),
            class_name: class.as_ptr(),
            icon_sm: icon,
        };
        if register_class_ex_w(&wc) == 0 {
            return Err(Error::Platform("RegisterClassExW failed".into()));
        }

        // Where to open: the rect the last session was closed at when the
        // monitors we have now can still show it, else centered on the primary
        // screen (CW_USEDEFAULT parks a WS_POPUP window in the top-left
        // corner). Read before the handler moves into `State`.
        let asked = handler.initial_window();
        let maximized = asked.is_some_and(|g| g.maximized);
        let (win_w, win_h) = (1100, 800);
        let (sx, sy) = (
            get_system_metrics(SM_CXSCREEN),
            get_system_metrics(SM_CYSCREEN),
        );
        let centered = if sx > 0 && sy > 0 {
            ((sx - win_w) / 2, (sy - win_h) / 2, win_w, win_h)
        } else {
            (CW_USEDEFAULT, CW_USEDEFAULT, win_w, win_h)
        };
        let desktop = (
            get_system_metrics(SM_XVIRTUALSCREEN),
            get_system_metrics(SM_YVIRTUALSCREEN),
            get_system_metrics(SM_CXVIRTUALSCREEN),
            get_system_metrics(SM_CYVIRTUALSCREEN),
        );
        let (x, y, win_w, win_h) = super::open_at(asked, desktop, centered);

        let title = handler.title();
        let title_w = wide(&title);
        let state = Box::into_raw(Box::new(State {
            handler,
            canvas: Canvas::new(0, 0),
            last_title: title,
            high_surrogate: None,
            touch: None,
            touch_travel: 0.0,
            touch_promoted: false,
            left_down: false,
            last_mouse: (0.0, 0.0),
            handler_dark: system_prefers_dark(),
        }));

        let hwnd = create_window_ex_w(
            0,
            class.as_ptr(),
            title_w.as_ptr(),
            // Borderless with a custom-drawn title bar: resize frame + min/max
            // + system menu, no OS caption.
            WS_POPUP | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX,
            x,
            y,
            win_w,
            win_h,
            null_mut(),
            null_mut(),
            hinst,
            state as *mut c_void,
        );
        if hwnd.is_null() {
            drop(Box::from_raw(state));
            return Err(Error::Platform("CreateWindowExW failed".into()));
        }
        drag_accept_files(hwnd, 1);
        register_touch_window(hwnd, 0);
        set_timer(hwnd, TIMER_TICK, 40, null());
        if !icon.is_null() {
            // The class icon covers most of the shell; WM_SETICON is what the
            // taskbar and Alt-Tab actually read.
            send_message_w(hwnd, WM_SETICON, ICON_SMALL, icon as isize);
            send_message_w(hwnd, WM_SETICON, ICON_BIG, icon as isize);
        }
        // A session that ended maximized opens maximized — at the normal rect,
        // so un-maximizing lands where the user left it (and the rect the app
        // keeps for the next restore is never the work area).
        show_window(hwnd, if maximized { SW_MAXIMIZE } else { SW_SHOW });
        update_window(hwnd);

        let mut msg: Msg = mem::zeroed();
        while get_message_w(&mut msg, null_mut(), 0, 0) > 0 {
            translate_message(&msg);
            dispatch_message_w(&msg);
        }
        drop(Box::from_raw(state));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ffi::*;
    use super::{
        end_gesture, maximize_metrics, timer_event, touch_action, State, TouchAction, TIMER_FRAME,
        TIMER_TICK,
    };
    use crate::platform::{Handler, Host};
    use crate::ui::event::{CursorShape, Event, MouseButton};
    use crate::ui::titlebar::TitleHit;
    use std::ptr::null_mut;

    /// Stands in for `App`: records what the platform hands over, and never
    /// touches a window (the tests below pass a null handle, which a real
    /// `WinHost` call would choke on).
    #[derive(Clone, Default)]
    struct Recorder(std::rc::Rc<std::cell::RefCell<Vec<Event>>>);

    impl Handler for Recorder {
        fn on_event(&mut self, ev: Event, _host: &mut dyn Host) {
            self.0.borrow_mut().push(ev);
        }
        fn paint(&mut self, _canvas: &mut crate::gfx::canvas::Canvas) {}
        fn title(&self) -> String {
            // Matches `State::last_title`, so `dispatch` never calls
            // `SetWindowTextW` on the null handle.
            String::new()
        }
        fn title_hit(&self, _x: f32, _y: f32) -> TitleHit {
            TitleHit::Client
        }
        fn cursor_at(&self, _x: f32, _y: f32) -> CursorShape {
            CursorShape::Arrow
        }
    }

    #[test]
    fn touch_contacts_decode_to_the_phase_they_are() {
        // The flags a real touchscreen sends, captured from a live window: a
        // contact arrives as `0x1A` (PRIMARY|INRANGE|DOWN), each move as
        // `0x19` (PRIMARY|INRANGE|MOVE) and lift-off as `0x14` (PRIMARY|UP).
        // With DOWN and MOVE swapped a finger drag dispatched nothing and
        // ended in a click, which is how this was found.
        assert_eq!(touch_action(0x1A), TouchAction::Down);
        assert_eq!(touch_action(0x19), TouchAction::Move);
        assert_eq!(touch_action(0x14), TouchAction::Up);
        // Contact geometry without a phase (a hover, a palm) is not a gesture.
        assert_eq!(touch_action(0x8), TouchAction::Ignore);
        assert_eq!(touch_action(0x10), TouchAction::Ignore);
        assert_eq!(touch_action(0), TouchAction::Ignore);
        // A down that also carries other bits is still the start of a gesture.
        assert_eq!(touch_action(0x1A | 0x8), TouchAction::Down);
        // The values are Win32's, not the order the names read in.
        assert_eq!(TOUCHEVENTF_MOVE, 0x1);
        assert_eq!(TOUCHEVENTF_DOWN, 0x2);
        assert_eq!(TOUCHEVENTF_UP, 0x4);
        assert_eq!(TOUCHEVENTF_PRIMARY, 0x10);
    }

    fn state() -> (State, Recorder) {
        let rec = Recorder::default();
        let st = State {
            handler: Box::new(rec.clone()),
            canvas: crate::gfx::canvas::Canvas::new(8, 8),
            last_title: String::new(),
            high_surrogate: None,
            touch: None,
            touch_travel: 0.0,
            touch_promoted: false,
            left_down: false,
            last_mouse: (0.0, 0.0),
            handler_dark: false,
        };
        (st, rec)
    }

    #[test]
    fn a_lost_release_is_delivered_exactly_once() {
        // A press on the scrollbar holds the mouse, so a `WM_LBUTTONUP` that
        // changes hands (another window grabs capture) or never comes (the
        // window is deactivated mid-drag) has to be fulfilled here: the app's
        // drags end on `MouseUp` and on nothing else, and a stranded thumb
        // then follows the pointer for the rest of the session.
        let (mut st, log) = state();
        st.left_down = true;
        st.last_mouse = (12.0, 34.0);
        end_gesture(&mut st, null_mut());
        let seen = log.0.borrow().clone();
        assert_eq!(seen.len(), 1, "one release for one press: {seen:?}");
        match seen[0] {
            Event::MouseUp { x, y, button } => {
                assert_eq!(
                    (x, y, button),
                    (12.0, 34.0, MouseButton::Left),
                    "the release lands where the pointer was last seen"
                );
            }
            ref other => panic!("expected the release, got {other:?}"),
        }
        // Both WM_KILLFOCUS and WM_CAPTURECHANGED fire for one Alt+Tab, and
        // `release_capture` re-enters the window proc: the app must not have to
        // defend against a second release for the same press.
        end_gesture(&mut st, null_mut());
        assert_eq!(log.0.borrow().len(), 1, "the release is owed only once");
        // Nothing is owed when no press is on the books — an ordinary click
        // ends on its own `WM_LBUTTONUP`.
        end_gesture(&mut st, null_mut());
        assert_eq!(log.0.borrow().len(), 1, "idle: nothing to deliver");
    }

    #[test]
    fn the_two_timers_stay_told_apart() {
        // Two clocks share `WM_TIMER`: the app's 40 ms heartbeat and the fast
        // frame clock a glide runs on. Routing them by id is the whole of it,
        // and crossing them would either stop a glide dead (frames arriving as
        // ticks, which the app only advances at 25 Hz) or run the caret blink
        // and drag auto-scroll at 64 Hz.
        assert!(matches!(timer_event(TIMER_TICK), Event::Tick));
        assert!(matches!(timer_event(TIMER_FRAME), Event::Frame));
        assert_ne!(
            TIMER_TICK, TIMER_FRAME,
            "one id for both means one timer cancels the other"
        );
    }

    #[test]
    fn predefined_cursors_load() {
        // WM_SETCURSOR hands these to SetCursor; a NULL here would make the
        // mouse pointer disappear over the app.
        unsafe {
            for id in [IDC_ARROW, IDC_IBEAM, IDC_HAND] {
                let cur = load_cursor_w(null_mut(), id as *const u16);
                assert!(!cur.is_null(), "cursor {id} failed to load");
            }
        }
    }

    #[test]
    fn maximize_lands_the_client_on_the_work_area() {
        // Regression: a borderless WS_POPUP window maximized to the whole
        // monitor, burying the taskbar. Maximize must clamp to the monitor
        // work area, expressed so that our maximized WM_NCCALCSIZE inset
        // (frame on every side) leaves the client exactly on the work area.
        let monitor = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let work = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040, // 40 px taskbar along the bottom
        };
        let (fx, fy) = (8, 8);
        let (position, size) = maximize_metrics(monitor, work, fx, fy);
        // Client origin = max position + one frame, measured from the monitor.
        assert_eq!(position.x + fx, work.left - monitor.left);
        assert_eq!(position.y + fy, work.top - monitor.top);
        // Client size = window size minus a frame on each side: the work area,
        // so the bottom frame never reaches into the taskbar.
        assert_eq!(size.x - 2 * fx, work.right - work.left);
        assert_eq!(size.y - 2 * fy, work.bottom - work.top);
        assert!(position.y + size.y - fy <= work.bottom - monitor.top);
    }

    #[test]
    fn maximize_is_monitor_relative_on_a_secondary_screen() {
        // A second monitor to the left of the primary has a negative origin;
        // ptMaxPosition is relative to that monitor, not the virtual screen.
        let monitor = Rect {
            left: -1920,
            top: -200,
            right: 0,
            bottom: 880,
        };
        let work = Rect {
            left: -1920,
            top: -200,
            right: 0,
            bottom: 840,
        };
        let (position, size) = maximize_metrics(monitor, work, 8, 8);
        assert_eq!(position.x, -8);
        assert_eq!(position.y, -8);
        assert_eq!(size.x, 1920 + 16);
        assert_eq!(size.y, 1040 + 16);
    }

    #[test]
    fn touch_input_layout_matches_win32() {
        // Regression: a TOUCHINPUT that drops a member (dwID) has the wrong
        // cbSize, and GetTouchInputInfo then fails outright — every touch
        // silently fell through to promoted mouse input, so a finger drag
        // selected text instead of panning.
        let (size, flags_off) = if std::mem::size_of::<usize>() == 8 {
            (48, 20)
        } else {
            (40, 16)
        };
        assert_eq!(std::mem::size_of::<TouchInput>(), size);
        // `flags` must be dwFlags — i.e. sit after hSource *and* dwID.
        assert_eq!(std::mem::offset_of!(TouchInput, flags), flags_off);
    }
}
