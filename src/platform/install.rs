//! File-association registration (`--install` / `--uninstall`).
//!
//! Windows writes `HKCU\Software\Classes` (ProgID `Editor.Document`, an
//! `OpenWithProgids` value per claimed extension, `shell\open\command`) via
//! advapi32 FFI and then `SHChangeNotify` — no admin needed. Linux/macOS
//! registration is owned by the `packaging/` scripts.

use crate::error::{Error, Result};

#[cfg(all(
    any(target_os = "windows", editor_check_win),
    not(editor_check_linux),
    not(editor_check_macos)
))]
mod win {
    use super::*;
    use crate::core::file_kind::EXTENSIONS;
    use std::ffi::c_void;

    const HKEY_CURRENT_USER: isize = 0x8000_0001;
    const REG_SZ: u32 = 1;
    /// KEY_SET_VALUE | KEY_CREATE_SUB_KEY | STANDARD_RIGHTS_WRITE
    const KEY_WRITE: u32 = 0x0002_0006;
    const SHCNE_ASSOCCHANGED: i32 = 0x0800_0000;
    const SHCNF_IDLIST: u32 = 0x0000;
    const PROGID: &str = "Editor.Document";
    const CLASSES: &str = "Software\\Classes";

    type Hkey = *mut c_void;

    #[link(name = "advapi32")]
    extern "system" {
        #[link_name = "RegCreateKeyExW"]
        fn reg_create_key_ex_w(
            parent: Hkey,
            subkey: *const u16,
            reserved: u32,
            class: *const u16,
            options: u32,
            access: u32,
            sa: *const c_void,
            result: *mut Hkey,
            disposition: *mut u32,
        ) -> i32;
        #[link_name = "RegSetValueExW"]
        fn reg_set_value_ex_w(
            key: Hkey,
            name: *const u16,
            reserved: u32,
            kind: u32,
            data: *const u8,
            size: u32,
        ) -> i32;
        #[link_name = "RegCloseKey"]
        fn reg_close_key(key: Hkey) -> i32;
        #[link_name = "RegDeleteTreeW"]
        fn reg_delete_tree_w(parent: Hkey, subkey: *const u16) -> i32;
        #[link_name = "RegDeleteKeyValueW"]
        fn reg_delete_key_value_w(parent: Hkey, subkey: *const u16, value: *const u16) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        #[link_name = "SHChangeNotify"]
        fn sh_change_notify(event: i32, flags: u32, a: *const c_void, b: *const c_void);
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Open (create) `HKCU\<parent>\<subkey>` for writing.
    fn open_key(parent: Hkey, path: &str) -> Result<Hkey> {
        let sub = wide(&format!("{CLASSES}\\{path}"));
        let mut key: Hkey = std::ptr::null_mut();
        let rc = unsafe {
            reg_create_key_ex_w(
                parent,
                sub.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_WRITE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            return Err(Error::Platform(format!(
                "RegCreateKeyExW({path}) failed: {rc}"
            )));
        }
        Ok(key)
    }

    /// Set a REG_SZ value (default value when `name` is empty).
    fn set_sz(key: Hkey, name: &str, value: &str) -> Result<()> {
        let name = wide(name);
        let data = wide(value);
        let rc = unsafe {
            reg_set_value_ex_w(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr() as *const u8,
                data.len() as u32 * 2,
            )
        };
        if rc != 0 {
            return Err(Error::Platform(format!("RegSetValueExW failed: {rc}")));
        }
        Ok(())
    }

    fn hkcu() -> Hkey {
        HKEY_CURRENT_USER as Hkey
    }

    pub fn install() -> Result<()> {
        let Some(exe) = super::exe_path() else {
            return Err(Error::Platform("cannot resolve the exe path".into()));
        };
        let exe = exe.to_string_lossy().into_owned();

        let key = open_key(hkcu(), PROGID)?;
        set_sz(key, "", "Editor Document")?;
        unsafe { reg_close_key(key) };
        let key = open_key(hkcu(), &format!("{PROGID}\\DefaultIcon"))?;
        set_sz(key, "", &format!("\"{exe}\",0"))?;
        unsafe { reg_close_key(key) };
        let key = open_key(hkcu(), &format!("{PROGID}\\shell\\open\\command"))?;
        set_sz(key, "", &format!("\"{exe}\" \"%1\""))?;
        unsafe { reg_close_key(key) };

        for ext in EXTENSIONS {
            let key = open_key(hkcu(), &format!(".{ext}\\OpenWithProgids"))?;
            set_sz(key, PROGID, "")?;
            unsafe { reg_close_key(key) };
        }
        unsafe {
            sh_change_notify(
                SHCNE_ASSOCCHANGED,
                SHCNF_IDLIST,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        super::super::print_line(&format!("registered Editor for: {}", EXTENSIONS.join(", ")));
        super::super::print_line(
            "(Windows may still ask you to pick Editor once in Settings > Apps > Default apps.)",
        );
        Ok(())
    }

    pub fn uninstall() -> Result<()> {
        let progid = wide(&format!("{CLASSES}\\{PROGID}"));
        unsafe { reg_delete_tree_w(hkcu(), progid.as_ptr()) };
        for ext in EXTENSIONS {
            let sub = wide(&format!("{CLASSES}\\.{ext}\\OpenWithProgids"));
            let value = wide(PROGID);
            unsafe { reg_delete_key_value_w(hkcu(), sub.as_ptr(), value.as_ptr()) };
        }
        unsafe {
            sh_change_notify(
                SHCNE_ASSOCCHANGED,
                SHCNF_IDLIST,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        super::super::print_line("unregistered Editor file associations");
        if let Some(dir) = crate::settings::config_dir() {
            super::super::print_line(&format!(
                "settings and recent files kept in {} (delete it for a clean slate)",
                dir.display()
            ));
        }
        Ok(())
    }
}

/// The exe path on Windows (FFI lives in the backend module).
#[cfg(all(
    any(target_os = "windows", editor_check_win),
    not(editor_check_linux),
    not(editor_check_macos)
))]
use super::windows::exe_path;

pub fn install() -> Result<()> {
    #[cfg(all(
        any(target_os = "windows", editor_check_win),
        not(editor_check_linux),
        not(editor_check_macos)
    ))]
    {
        win::install()
    }
    #[cfg(not(all(
        any(target_os = "windows", editor_check_win),
        not(editor_check_linux),
        not(editor_check_macos)
    )))]
    {
        Err(Error::Platform(
            "use the scripts in packaging/ on this OS".into(),
        ))
    }
}

pub fn uninstall() -> Result<()> {
    // Downloads first, everywhere: the installer leaves its msi/zip/tarball in
    // a temp directory, and "uninstalled" should not mean "except the 1.6 MB
    // still in %TEMP%". Works even where this flag has no registration to
    // undo, so the cleanup is never skipped on a platform that errors below.
    let gone = crate::update::clean_payloads();
    if gone > 0 {
        crate::platform::print_line(&format!(
            "removed {gone} staged update download{}",
            if gone == 1 { "" } else { "s" }
        ));
    }
    #[cfg(all(
        any(target_os = "windows", editor_check_win),
        not(editor_check_linux),
        not(editor_check_macos)
    ))]
    {
        win::uninstall()
    }
    #[cfg(not(all(
        any(target_os = "windows", editor_check_win),
        not(editor_check_linux),
        not(editor_check_macos)
    )))]
    {
        Err(Error::Platform(
            "use the scripts in packaging/ on this OS".into(),
        ))
    }
}
