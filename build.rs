//! Put the app's icon and version stamp inside the Windows executable.
//!
//! The window icon is painted at runtime (`gfx::icon`), but Windows reads a
//! *file type's* icon from the executable named by the shell's `DefaultIcon`
//! value (`editor.exe,0`), not from anything the process draws. An exe with no
//! icon resource therefore made every associated `.md`/`.json`/… file show the
//! generic Windows app icon. The version resource is what Explorer's
//! Properties → Details pane reads, what Add/Remove Programs and signing
//! policies (SignPath enforces product name and version) expect. This script
//! turns `assets/icon.ico` and the package version into a `.res` file and
//! hands it to the MSVC linker, which is the whole fix.
//!
//! No crates: the `.ico` and `.res` formats are both small and fixed, so the
//! conversion is a few dozen lines of byte assembly here rather than a build
//! dependency (the crate is deliberately dependency-free). GNU toolchains use
//! a different linker and do not read `.res`; the release workflow builds MSVC,
//! and a GNU local build simply gets the old iconless, stampless exe back.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");
    let windows_msvc = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !windows_msvc {
        return;
    }
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let ico = fs::read(root.join("assets/icon.ico")).expect("read assets/icon.ico");
    let mut res = res_file(&ico).expect("assets/icon.ico is a well-formed icon");
    entry(&mut res, RT_VERSION, 1, &version_info(&version));
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("editor.res");
    fs::write(&out, res).expect("write editor.res");
    // A bare path is an input file to link.exe, exactly like an .obj: the
    // linker folds the resource section into the image.
    println!("cargo:rustc-link-arg={}", out.display());
}

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;

/// One entry of an `.ico`: the image bytes plus what a group icon has to say
/// about it.
struct Frame<'a> {
    width: u8,
    height: u8,
    colors: u8,
    planes: u16,
    bits: u16,
    data: &'a [u8],
}

/// Build a Win32 `.res` holding every frame as `RT_ICON` (ids 1..=n) and a
/// `RT_GROUP_ICON` (id 1) that names them, which is what Explorer asks for
/// when it loads `editor.exe,0`.
fn res_file(ico: &[u8]) -> Option<Vec<u8>> {
    // ICONDIR: reserved 0, type 1 (icon), count.
    if ico.get(0..4)? != [0, 0, 1, 0] {
        return None;
    }
    let count = u16::from_le_bytes([*ico.get(4)?, *ico.get(5)?]) as usize;
    let mut frames = Vec::with_capacity(count);
    for i in 0..count {
        let e = ico.get(6 + i * 16..6 + (i + 1) * 16)?;
        let bytes = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize;
        let at = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as usize;
        frames.push(Frame {
            width: e[0],
            height: e[1],
            colors: e[2],
            planes: u16::from_le_bytes([e[4], e[5]]),
            bits: u16::from_le_bytes([e[6], e[7]]),
            data: ico.get(at..at + bytes)?,
        });
    }
    if frames.is_empty() {
        return None;
    }

    // GRPICONDIR: header, then a 14-byte entry per frame. `nId` is the
    // RT_ICON id the frame is written under below, not an offset.
    let mut group = Vec::with_capacity(6 + frames.len() * 14);
    group.extend_from_slice(&0u16.to_le_bytes()); // reserved
    group.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    group.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    for (i, f) in frames.iter().enumerate() {
        group.extend_from_slice(&[f.width, f.height, f.colors, 0]);
        group.extend_from_slice(&f.planes.to_le_bytes());
        group.extend_from_slice(&f.bits.to_le_bytes());
        group.extend_from_slice(&(f.data.len() as u32).to_le_bytes());
        group.extend_from_slice(&((i + 1) as u16).to_le_bytes());
    }

    // rc.exe opens a `.res` with one all-zero entry (ordinal type 0, name 0,
    // no data). link.exe reads the first entry's fields before it has any
    // idea what follows, and rejects the file without this placeholder.
    let mut out = Vec::new();
    out_null(&mut out);
    for (i, f) in frames.iter().enumerate() {
        entry(&mut out, RT_ICON, (i + 1) as u16, f.data);
    }
    entry(&mut out, RT_GROUP_ICON, 1, &group);
    Some(out)
}

/// The 32-byte placeholder every `.res` starts with.
fn out_null(out: &mut Vec<u8>) {
    out.extend_from_slice(&0u32.to_le_bytes()); // data size
    out.extend_from_slice(&0x20u32.to_le_bytes()); // header size
    out.extend_from_slice(&[0; 24]);
}

/// Append one resource: the 0x20-byte header (both the type and the name are
/// `0xFFFF` + ordinal here, four bytes each), the data, and 4-byte alignment.
fn entry(out: &mut Vec<u8>, kind: u16, id: u16, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x20u32.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes()); // ordinal type follows
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes()); // ordinal name follows
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // data version
    out.extend_from_slice(&0x1010u16.to_le_bytes()); // what rc.exe writes for icons
    out.extend_from_slice(&0x0409u16.to_le_bytes()); // en-US
    out.extend_from_slice(&0u32.to_le_bytes()); // version
    out.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    out.extend_from_slice(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// The `VS_VERSIONINFO` block Explorer's Properties → Details pane,
/// Add/Remove Programs and signing policies read: the fixed file-info header
/// plus a `StringFileInfo` table and the translation `VarFileInfo`.
fn version_info(version: &str) -> Vec<u8> {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    let ms = (major << 16) | (minor & 0xFFFF);
    let ls = patch << 16;

    // VS_FIXEDFILEINFO, 13 dwords. VOS_NT_WINDOWS32 + VFT_APP: a Windows GUI
    // application, which is what the updater and the shell both expect.
    let mut fixed = Vec::with_capacity(52);
    for field in [
        0xFEEF04BDu32, // signature
        0x0001_0000,   // struct version
        ms,            // file version MS
        ls,            // file version LS
        ms,            // product version MS
        ls,            // product version LS
        0x3F,          // file flags mask
        0,             // file flags
        0x0004_0004,   // file OS
        1,             // file type
        0,             // file subtype
        0,             // file date MS
        0,             // file date LS
    ] {
        fixed.extend_from_slice(&field.to_le_bytes());
    }

    // ProductName and ProductVersion are the two a SignPath artifact
    // configuration pins; the rest is what a Windows user expects to find.
    let mut strings = Vec::new();
    for (name, value) in [
        ("CompanyName", "Eric Lautanen"),
        (
            "FileDescription",
            "Editor - native viewer for Markdown, JSON, CSV and code",
        ),
        ("FileVersion", version),
        ("InternalName", "editor"),
        ("LegalCopyright", "Copyright (C) 2026 Eric Lautanen"),
        ("OriginalFilename", "editor.exe"),
        ("ProductName", "Editor"),
        ("ProductVersion", version),
    ] {
        let value = wstr(value);
        strings.extend_from_slice(&block(name, (value.len() / 2) as u16, 1, &value, &[]));
    }

    let table = block("040904B0", 0, 1, &[], &strings);
    let string_info = block("StringFileInfo", 0, 1, &[], &table);
    // Translation: 0x0409 = en-US, 0x04B0 = Unicode code page, as a DWORD.
    let var = block("Translation", 4, 0, &[0x09, 0x04, 0xB0, 0x04], &[]);
    let var_info = block("VarFileInfo", 0, 1, &[], &var);

    let mut children = Vec::new();
    children.extend_from_slice(&string_info);
    children.extend_from_slice(&var_info);
    block("VS_VERSION_INFO", 52, 0, &fixed, &children)
}

/// One version-info node: `wLength`, `wValueLength`, `wType`, the key
/// (UTF-16, NUL-terminated), 4-byte alignment, the value, and the children.
/// `value_len` is in bytes for binary nodes (52 for `VS_FIXEDFILEINFO`, 4 for
/// `Translation`) and in WCHARs for strings, as Microsoft's format defines.
fn block(key: &str, value_len: u16, value_type: u16, value: &[u8], children: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&0u16.to_le_bytes()); // wLength, filled in at the end
    b.extend_from_slice(&value_len.to_le_bytes());
    b.extend_from_slice(&value_type.to_le_bytes());
    wr(&mut b, key);
    align4(&mut b);
    b.extend_from_slice(value);
    align4(&mut b);
    b.extend_from_slice(children);
    let len = b.len() as u16;
    b[0..2].copy_from_slice(&len.to_le_bytes());
    b
}

/// UTF-16LE with the NUL terminator the resource format requires.
fn wstr(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() * 2 + 2);
    wr(&mut v, s);
    v
}

fn wr(v: &mut Vec<u8>, s: &str) {
    for unit in s.encode_utf16() {
        v.extend_from_slice(&unit.to_le_bytes());
    }
    v.extend_from_slice(&0u16.to_le_bytes());
}

fn align4(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}
