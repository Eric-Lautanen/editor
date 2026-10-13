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
        event::{CursorShape, Event, Key, Modifiers, MouseButton},
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

    /// `TOUCHINPUT` for `WM_TOUCH` (coordinates are screen px × 100).
    #[repr(C)]
    pub struct TouchInput {
        pub x: i32,
        pub y: i32,
        pub handle: Handle,
        pub flags: u32,
        pub mask: u32,
        pub time: u32,
        pub extra_info: usize,
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
    pub const WM_SIZE: u32 = 0x0005;
    pub const WM_PAINT: u32 = 0x000F;
    pub const WM_CLOSE: u32 = 0x0010;
    pub const WM_ERASEBKGND: u32 = 0x0014;
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
    pub const WM_DROPFILES: u32 = 0x0233;
    pub const WM_TIMER: u32 = 0x0113;
    pub const WM_TOUCH: u32 = 0x0240;
    pub const WM_DPICHANGED: u32 = 0x02E0;
    pub const WM_SETICON: u32 = 0x0080;
    /// `ICON_SMALL` wParam for `WM_SETICON`.
    pub const ICON_SMALL: usize = 0;
    /// `ICON_BIG` wParam for `WM_SETICON`.
    pub const ICON_BIG: usize = 1;

    // Touch input flags (TOUCHEVENTF_*)
    pub const TOUCHEVENTF_DOWN: u32 = 0x0000_0001;
    pub const TOUCHEVENTF_MOVE: u32 = 0x0000_0002;
    pub const TOUCHEVENTF_UP: u32 = 0x0000_0004;

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
    pub const SM_CXFRAME: i32 = 32;
    pub const SM_CXPADDEDBORDER: i32 = 92;

    // GetOpenFileNameW flags
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
        #[link_name = "ScreenToClient"]
        pub fn screen_to_client(hwnd: Hwnd, pt: *mut Point) -> i32;
        #[link_name = "IsZoomed"]
        pub fn is_zoomed(hwnd: Hwnd) -> i32;
        #[link_name = "GetSystemMetrics"]
        pub fn get_system_metrics(index: i32) -> i32;
        #[link_name = "GetCursorPos"]
        pub fn get_cursor_pos(pt: *mut Point) -> i32;
        #[link_name = "SetTimer"]
        pub fn set_timer(hwnd: Hwnd, id: usize, ms: u32, proc: *const c_void) -> usize;
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

// ---------------------------------------------------------------- state & host

struct State {
    handler: Box<dyn Handler>,
    canvas: Canvas,
    last_title: String,
    /// Pending UTF-16 high surrogate from WM_CHAR (emoji etc. arrive as two messages).
    high_surrogate: Option<u16>,
    /// Last touch contact (screen px × 100) while a touch drag pans the view.
    touch: Option<(f32, f32)>,
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

/// Resize border thickness in px (frame + padded border), min 4.
fn resize_frame() -> i32 {
    unsafe { (get_system_metrics(SM_CXFRAME) + get_system_metrics(SM_CXPADDEDBORDER)).max(4) }
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

    match msg {
        WM_NCCALCSIZE => {
            // Borderless: the client area is the whole window. When maximized a
            // WS_THICKFRAME window extends past the monitor by the frame, so
            // pull the client rect back in.
            if wp != 0 {
                let p = lp as *mut NcCalcSizeParams;
                if is_zoomed(hwnd) != 0 {
                    let f = resize_frame();
                    (*p).rgrc[0].left += f;
                    (*p).rgrc[0].top += f;
                    (*p).rgrc[0].right -= f;
                    (*p).rgrc[0].bottom -= f;
                }
                0
            } else {
                def_window_proc_w(hwnd, msg, wp, lp)
            }
        }
        WM_NCHITTEST => {
            // Screen -> client, then: resize edges, else ask the app about the
            // title bar (Drag -> caption so the OS moves the window).
            let mut pt = Point {
                x: loword(lp),
                y: hiword(lp),
            };
            screen_to_client(hwnd, &mut pt);
            if is_zoomed(hwnd) == 0 {
                let f = resize_frame();
                let (x, y) = (pt.x, pt.y);
                let (w, h) = (st.canvas.width as i32, st.canvas.height as i32);
                let hit = match (x < f, x >= w - f, y < f, y >= h - f) {
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
                if hit != 0 {
                    return hit;
                }
            }
            match st.handler.title_hit(pt.x as f32, pt.y as f32) {
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
            // Touch drags pan the document (taps still arrive as mouse clicks).
            let count = (wp & 0xFFFF) as u32;
            let mut inputs: Vec<TouchInput> = Vec::new();
            inputs.resize_with(count as usize, || TouchInput {
                x: 0,
                y: 0,
                handle: null_mut(),
                flags: 0,
                mask: 0,
                time: 0,
                extra_info: 0,
            });
            let hinput = lp as Handle;
            if get_touch_input_info(
                hinput,
                count,
                inputs.as_mut_ptr(),
                mem::size_of::<TouchInput>() as u32,
            ) != 0
            {
                for t in &inputs {
                    let (x, y) = (t.x as f32 / 100.0, t.y as f32 / 100.0);
                    if t.flags & TOUCHEVENTF_DOWN != 0 {
                        st.touch = Some((x, y));
                    } else if t.flags & TOUCHEVENTF_MOVE != 0 {
                        if let Some((lx, ly)) = st.touch {
                            let (dx, dy) = (x - lx, y - ly);
                            if dx != 0.0 || dy != 0.0 {
                                dispatch(st, hwnd, Event::Scroll { dx, dy });
                            }
                        }
                        st.touch = Some((x, y));
                    } else if t.flags & TOUCHEVENTF_UP != 0 {
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
            // Heartbeat for drag auto-scroll at pane edges.
            dispatch(st, hwnd, Event::Tick);
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
            if wp != SIZE_MINIMIZED {
                let (w, h) = (loword(lp) as usize, hiword(lp) as usize);
                if w > 0 && h > 0 {
                    st.canvas.resize(w, h);
                    let dpi = get_dpi_for_window(hwnd);
                    let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
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
            }
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
            dispatch(st, hwnd, Event::MouseUp { x, y, button });
            0
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

        let title = handler.title();
        let title_w = wide(&title);
        let state = Box::into_raw(Box::new(State {
            handler,
            canvas: Canvas::new(0, 0),
            last_title: title,
            high_surrogate: None,
            touch: None,
            handler_dark: system_prefers_dark(),
        }));

        // Center the window on the primary monitor (CW_USEDEFAULT parks a
        // WS_POPUP window in the top-left corner).
        let (win_w, win_h) = (1100, 800);
        let (sx, sy) = (
            get_system_metrics(SM_CXSCREEN),
            get_system_metrics(SM_CYSCREEN),
        );
        let (x, y) = if sx > 0 && sy > 0 {
            ((sx - win_w) / 2, (sy - win_h) / 2)
        } else {
            (CW_USEDEFAULT, CW_USEDEFAULT)
        };
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
        set_timer(hwnd, 1, 40, null());
        if !icon.is_null() {
            // The class icon covers most of the shell; WM_SETICON is what the
            // taskbar and Alt-Tab actually read.
            send_message_w(hwnd, WM_SETICON, ICON_SMALL, icon as isize);
            send_message_w(hwnd, WM_SETICON, ICON_BIG, icon as isize);
        }
        show_window(hwnd, SW_SHOW);
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
    use std::ptr::null_mut;

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
}
