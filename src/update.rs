//! Self-update: find the newest Editor release on GitHub, download it with
//! the OS's built-in `curl` (no crates — see the Cargo.toml covenant), and
//! install it. Two entry points:
//!
//! * [`run`] — the `editor --update` CLI command: check, download, install,
//!   narrated for the terminal.
//! * [`background_check`] — spawned on GUI launch (see `cli::run`): the same
//!   flow done silently, asking first via a native dialog, then closing the
//!   app and installing.
//!
//! Per-OS install story:
//!
//! | OS | Asset | Install |
//! |---|---|---|
//! | Windows | `editor-<tag>-windows-x86_64.msi` | `msiexec /i <msi> /passive` — the per-user MSI needs no UAC, replaces the old build, and Windows Installer's restart manager relaunches the app |
//! | Linux | `editor-<tag>-linux-x86_64.tar.gz` | unpack, rename the fresh binary over the running one (legal while it runs on Linux), relaunch |
//! | macOS | `editor-<tag>-macos.zip` | unpack to a staging dir, remove the old `Editor.app`, move the new one into place, relaunch |
//!
//! When no official asset exists for the running platform/arch (macOS on
//! Intel, Linux on arm…) the check still works and says there is nothing to
//! auto-install. Asset names are owned by `release.yml`; a drift between the
//! two ends in a clear "download it manually" message, not a wrong download.
use crate::error::{Error, Result};
use crate::platform::print_line;

const REPO: &str = "Eric-Lautanen/editor";
const API_LATEST: &str = "https://api.github.com/repos/Eric-Lautanen/editor/releases/latest";
/// The latest-release endpoint is unauthenticated, so repeated rebuild-and-run
/// dev loops (which would each hit it at GUI launch) burn the IP's rate
/// budget. Skip the silent on-launch check when this is set; `--update` still
/// works.
const NO_CHECK_ENV: &str = "EDITOR_NO_UPDATE_CHECK";

pub fn run() -> Result<()> {
    print_line("editor self-update:");
    let Some(r) = newest_release()? else {
        print_line(&format!(
            "already on the latest version ({})",
            env!("CARGO_PKG_VERSION")
        ));
        return Ok(());
    };
    print_line(&format!(
        "{} -> {} found; downloading…",
        env!("CARGO_PKG_VERSION"),
        r.tag
    ));
    install(&r)
}

/// Spawned detached on GUI launch and fully asynchronous with it — the
/// window opens in parallel with the check, which never holds the launch:
/// nothing in the app waits on the result, the whole sequence (HTTP, dialog,
/// install) lives in its own thread, and every failure path is silent. No
/// network → `curl` fails fast (`-m 10` hard cap) → the thread returns
/// without a trace. Firewalled/blocked endpoints → same: an error exit, no
/// dialog, no retry loop, no crash. A blocked port can only ever cost the
/// app ≤10s of an idle background thread, never a paint frame.
pub fn background_check() {
    if std::env::var_os(NO_CHECK_ENV).is_some() {
        return;
    }
    std::thread::Builder::new()
        .name("update-check".into())
        .spawn(|| {
            let Ok(Some(r)) = newest_release() else {
                return;
            };
            if !ask_install(&r.tag) {
                return;
            }
            let _ = install(&r);
        })
        // A refused spawn skips the check for that one launch; not something
        // a GUI app can surface.
        .ok();
}

// ---------------------------------------------------------------- discovery

struct Release {
    tag: String,
    /// `browser_download_url` of this platform's install asset.
    asset_url: String,
}

/// `None` when the newest release is not newer than this build.
fn newest_release() -> Result<Option<Release>> {
    let stage = match asset_name() {
        Some(a) => a,
        None => {
            return Err(Error::Platform(format!(
                "no official build for {} {}; download from github.com/{REPO}/releases",
                std::env::consts::OS,
                std::env::consts::ARCH
            )));
        }
    };
    let json = curl(&["-sS", "-m", "10", "-L", API_LATEST])?;
    let tag = extract_field(&json, "tag_name").ok_or_else(|| {
        Error::Platform("GitHub's response had no tag_name (rate-limited?)".into())
    })?;
    if !newer(&tag, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let asset_url = find_asset(&json, &format!("editor-{tag}"), stage).ok_or_else(|| {
        Error::Platform(format!(
            "release {tag} exists but ships no {stage}; download one from github.com/{REPO}/releases/tag/{tag}"
        ))
    })?;
    Ok(Some(Release { tag, asset_url }))
}

/// The release asset for the running build per `release.yml`'s artifact
/// names, or `None` where no asset is published today.
fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("windows-x86_64.msi"),
        ("linux", "x86_64") => Some("linux-x86_64.tar.gz"),
        ("macos", _) => Some("macos.zip"),
        _ => None,
    }
}

// ---------------------------------------------------------------- scanning

/// Scans the release API response for `"browser_download_url":"<url>"` and
/// returns the one whose filename starts with `name_prefix` (the tag) and
/// ends with `stage` (the platform piece). A hand-rolled scan, not a JSON
/// parser: the release body's quoted text could fool a fully general one
/// anyway (see `extract_field`), while this anchor appears exactly once per
/// asset and always at a real asset boundary.
fn find_asset(json: &str, name_prefix: &str, stage: &str) -> Option<String> {
    const KEY: &str = "\"browser_download_url\":\"";
    let mut rest = json;
    while let Some(i) = rest.find(KEY) {
        rest = &rest[i + KEY.len()..];
        let end = rest.find('"')?;
        let url = &rest[..end];
        let name = url.rsplit('/').next()?;
        if name.starts_with(name_prefix) && name.ends_with(stage) {
            return Some(url.into());
        }
    }
    None
}

/// Extracts the flat top-level string field `"tag_name":"v0.1.0"` from the
/// release API response. Deliberately not `parse::json`: the value is one of
/// two fields the updater needs, always unique per response, and the
/// renderer's parser targets the documents this app edits, not the GitHub
/// API dialect.
fn extract_field(json: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\":\"");
    let i = json.find(&key)? + key.len();
    let rest = &json[i..];
    let end = rest.find('"')?;
    Some(rest[..end].into())
}

/// `newer("v0.2.1", "0.2.0")` → true. Segment-by-segment numerically, so
/// `0.10.0` stays newer than `0.2.0`; a tag may be shorter than the running
/// version (`v1` > `v0.1.0`, since a missing segment reads as 0); a segment
/// that will not parse on either side is a tie and the earlier segments
/// decide. Only ever sees the tags this repo publishes.
fn newer(tag: &str, current: &str) -> bool {
    let tag = tag.trim_start_matches('v');
    let mut cur = current.split('.');
    for t in tag.split('.') {
        match (t.parse::<u64>(), cur.next()) {
            (Ok(t), Some(c)) => match c.parse::<u64>() {
                Ok(c) => {
                    if t != c {
                        return t > c;
                    }
                }
                // A non-numeric segment in the *running* version is a dev
                // build suffix; let the next segment decide.
                Err(_) => {}
            },
            // The tag names something past our (numerically exhausted)
            // version: `v1` vs `0.9.9`.
            (Ok(_), None) => return true,
            (Err(_), _) => {}
        }
    }
    false
}

// ---------------------------------------------------------------- fetching

/// Runs the OS `curl` and captures stdout. Windows has shipped `curl.exe` in
/// System32 since 1803; macOS since 10.9 (but `.curl` sits there either way);
/// mainstream Linux distros all have it. No crates — the alternative is a
/// TLS hand-rolled client, which is exactly the non-starter this covenant
/// exists to avoid.
fn curl(args: &[&str]) -> Result<String> {
    let exe = if cfg!(target_os = "windows") {
        "curl.exe" // The System32 one, not an msys2/babun PATH shadow.
    } else {
        "curl"
    };
    let mut cmd = std::process::Command::new(exe);
    // GitHub's API 403s anything without a User-Agent.
    cmd.arg("-H")
        .arg(format!("User-Agent: editor/{}", env!("CARGO_PKG_VERSION")));
    for a in args {
        cmd.arg(a);
    }
    let out = cmd
        .output()
        .map_err(|e| Error::Platform(format!("could not run {exe}: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(Error::Platform(format!("{exe} failed: {}", stderr.trim())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Release {
    /// Downloads the asset to `<temp>/editor-update-<tag>/`, each call on a
    /// clean slate (a half-written file from a crashed earlier install would
    /// otherwise be silently reused).
    fn download(&self) -> Result<std::path::PathBuf> {
        let dir = std::env::temp_dir().join(format!("editor-update-{}", self.tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let file = dir
            .join(format!(
                "editor-{}-{}",
                self.tag,
                asset_name().unwrap_or_default()
            ))
            .display()
            .to_string();
        curl(&["-sS", "-m", "300", "-L", "-o", &file, &self.asset_url])?;
        if std::fs::metadata(&file).map_or(true, |m| m.len() == 0) {
            return Err(Error::Platform(format!(
                "download of {} produced nothing; github may be down",
                self.asset_url
            )));
        }
        Ok(std::path::PathBuf::from(file))
    }
}

// ---------------------------------------------------------------- installing

fn install(r: &Release) -> Result<()> {
    match std::env::consts::OS {
        "windows" => install_windows(&r.download()?, &r.tag),
        "linux" => install_linux(&r.download()?, &r.tag),
        "macos" => install_macos(&r.download()?, &r.tag),
        other => Err(Error::Platform(format!(
            "no automated update for {other}; download from github.com/{REPO}/releases/tag/{}",
            r.tag
        ))),
    }
}

/// Windows: run the per-user MSI. `/passive` shows a progress bar but asks
/// no questions; Windows Installer's restart manager closes the running
/// editor.exe and relaunches it, and a *newer* version installs over the old
/// via the MSI's major-upgrade, all without a UAC prompt (per-user MSI).
/// This process ends here: the relaunch after install comes from msiexec.
fn install_windows(file: &std::path::Path, tag: &str) -> Result<()> {
    let file = file
        .to_str()
        .ok_or_else(|| Error::Platform("temp path is not UTF-8".into()))?;
    print_line(&format!("launching the installer for {tag}…"));
    std::process::Command::new("msiexec")
        .args(["/i", file, "/passive"])
        .spawn()
        .map_err(|e| Error::Platform(format!("msiexec could not start: {e}")))?;
    // Hold nothing open the installer would want exclusive; `msiexec` was
    // spawned detached above and survives this exit.
    std::process::exit(0);
}

/// Linux: unpack and rename the fresh binary over the running one. POSIX
/// `rename` replaces an open executable in place, so this instance finishes
/// serving files on the old image while the next launch (desktop entry,
/// terminal) runs the new build.
fn install_linux(file: &std::path::Path, tag: &str) -> Result<()> {
    let dir = file.parent().unwrap().to_path_buf();
    run_unpack("tar", &["-xzf".into()], file, &dir)?;
    // `release.yml` tarballs the staging dir, so the binary sits at its root.
    let fresh = dir.join("editor");
    if !fresh.is_file() {
        return Err(Error::Platform(format!(
            "unpacked {tag} but found no `editor` binary in {}",
            dir.display()
        )));
    }
    let me = std::env::current_exe()
        .map_err(|e| Error::Platform(format!("could not resolve the running exe path: {e}")))?;
    // `copy` spans filesystems (temp is often tmpfs, $HOME rarely) and the
    // sidecar `rename` then swaps atomically in place. Deleting the sidecar
    // leftover is the install's cleanup job on a rerun.
    let sidecar = me.with_extension("update-new");
    let _ = std::fs::remove_file(&sidecar);
    std::fs::copy(&fresh, &sidecar)?;
    match std::fs::rename(&sidecar, &me) {
        Ok(()) => {
            // `relaunch` ends the process on success; reaching the `Ok(())`
            // only happens when it stayed alive on a failed heroics path.
            relaunch(&me, tag);
            Ok(())
        }
        Err(e) => Err(Error::Platform(format!(
            "replacing the installed binary failed ({e}); the fresh one is at {}",
            fresh.display()
        ))),
    }
}

/// macOS: `ditto -x -k` (the exact reciprocal of `release.yml`'s
/// `ditto -c -k`) into a staging dir, then move the packed `Editor.app` over
/// the one this process runs from. Unlinking a running bundle's files is
/// legal on macOS — the image stays mapped — and the move is what the next
/// Finder double-click or Dock `instance` launches.
fn install_macos(file: &std::path::Path, tag: &str) -> Result<()> {
    let dir = file.parent().unwrap().to_path_buf();
    run_unpack("ditto", &["-x".into(), "-k".into()], file, &dir)?;
    let fresh = dir.join("Editor.app");
    let me = std::env::current_exe()
        .map_err(|e| Error::Platform(format!("could not resolve the running exe path: {e}")))?;
    // Find the `.app` root my exe is nested inside. A bare cargo-run has
    // none: report the bundle path instead of guessing at ~half-replaced.
    let Some(bundle) = me
        .ancestors()
        .find(|p| p.extension().map_or(false, |e| e == "app"))
        .map(|p| p.to_path_buf())
    else {
        return Err(Error::Platform(format!(
            "{tag} unpacked to {} but this editor is not running from an Editor.app; replace the bundle by hand",
            dir.display()
        )));
    };
    std::fs::remove_dir_all(&bundle)?;
    std::fs::rename(&fresh, &bundle)?;
    relaunch(&bundle.join("Contents/MacOS/editor"), tag);
    Ok(())
}

/// `Command::status` waiting on the unpacker, mapping its exit to this
/// module's error type. `tar` and `ditto` both speak this two-flag pattern;
/// the lifetimes keep the arg values alive into the `Command` spawning.
fn run_unpack(
    unpacker: &str,
    unpack_flags: &[std::borrow::Cow<'_, str>],
    archive: &std::path::Path,
    dir: &std::path::Path,
) -> Result<()> {
    let mut cmd = std::process::Command::new(unpacker);
    for f in unpack_flags {
        cmd.arg(f.as_ref());
    }
    cmd.args([archive.as_os_str(), dir.as_os_str()]);
    let status = cmd
        .status()
        .map_err(|e| Error::Platform(format!("{unpacker} could not start: {e}")))?;
    if !status.success() {
        return Err(Error::Platform(format!(
            "{unpacker} could not unpack {}; output above",
            archive.display()
        )));
    }
    Ok(())
}

/// Spawn the fresh binary with no args (the session restore in `cli::run`
/// puts the window where it belongs) and exit this instance. If the spawn
/// fails the flow reports and stays alive — the user still has a working
/// (old) app, the install just shows its results by hand next time.
fn relaunch(exe: &std::path::Path, tag: &str) {
    match std::process::Command::new(exe).spawn() {
        Ok(_) => std::process::exit(0),
        Err(e) => print_line(&format!(
            "{tag} installed; start the new one from your launcher (auto-restart failed: {e})"
        )),
    }
}

// ---------------------------------------------------------------- consent

/// Native yes/no per OS, shown only by the silent background check —
/// `--update` from a terminal needs no permission (the user already asked
/// for an update by typing it).
#[cfg(target_os = "windows")]
fn ask_install(tag: &str) -> bool {
    win_dialog(
        &format!(
            "Editor {tag} is available. Install it now?\n\nEditor will close and restart with the new version."
        ),
        "Editor update",
    )
}

#[cfg(target_os = "windows")]
fn win_dialog(text: &str, caption: &str) -> bool {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    // user32 + kernel32's wide-string charset — the rest of the backend uses
    // the same two-symbol-per-purpose pattern (see `install.rs`/`windows.rs`).
    #[link(name = "user32")]
    extern "C" {
        fn MessageBoxW(hwnd: *mut c_void, text: *const u16, caption: *const u16, ty: u32) -> i32;
    }
    const MB_ICONINFORMATION: u32 = 0x0040;
    const MB_SETFOREGROUND: u32 = 0x1_0000;
    const MB_TOPMOST: u32 = 0x4_0000;
    const MB_YESNO: u32 = 0x0004;
    const MB_SYSTEMMODAL: u32 = 0_1000;
    const IDYES: i32 = 6;
    let to_wide = |s: &str| {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<std::vec::Vec<u16>>()
    };
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            to_wide(text).as_ptr(),
            to_wide(caption).as_ptr(),
            MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST | MB_SYSTEMMODAL | MB_YESNO,
        ) == IDYES
    }
}

#[cfg(target_os = "macos")]
fn ask_install(tag: &str) -> bool {
    // `display dialog` is a stock system surface the way `open` (which routes
    // links through it above) is — using it asks nothing and installs
    // nothing by itself; the answer decides. `--question` and friends run a
    // non-admin path every macOS has.
    let script = format!(
        "display dialog \"Editor {tag} is available. Install it now?\" with title \"Editor update\" buttons {{\"Not now\",\"Install\"}} default button \"Install\""
    );
    match std::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
    {
        Ok(out) => String::from_utf8_lossy(&out.stdout).contains("button returned:\"Install\""),
        // No osascript (broken install): skip the update this launch.
        Err(_) => false,
    }
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn ask_install(tag: &str) -> bool {
    // The same zenity/kdialog pair the file dialogs use (see
    // `platform::linux`), so on a kit where the user picked dialogs with,
    // update consent appears with the same look. The first present wins:
    // if zenity answered at all, do not re-ask with kdialog.
    let text = format!("Editor {tag} is available. Install it now?");
    let question = format!("--text={text}");
    if let Some(yes) = ask_with("zenity", &["--question", &question]) {
        return yes;
    }
    ask_with("kdialog", &["--yesno", &question]).unwrap_or(false)
}

/// Runs a dialog helper and maps its exit code; `None` = helper not installed
/// (its name is not on `PATH`), the caller tries the next.
#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn ask_with(helper: &str, args: &[&str]) -> Option<bool> {
    match std::process::Command::new(helper).args(args).status() {
        Ok(s) => Some(s.success()),
        Err(_) => None,
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_numerics_and_short_tags() {
        assert!(!newer("v0.1.0", "0.1.0"));
        assert!(newer("v0.2.0", "0.1.0"));
        assert!(newer("v0.10.0", "0.2.0")); // numeric, not lexical
        assert!(newer("v1.0.0", "0.9.9"));
        assert!(!newer("v0.1.0", "0.1.1"));
        assert!(newer("v1", "0.1.0")); // missing segments read as 0
        assert!(!newer("v0.1", "0.1.0")); // 0.1 == 0.1.0
        assert!(!newer("0.9.9", "1.0.0")); // without the v, same rules
    }

    #[test]
    fn field_extraction_grabs_the_kv_shape() {
        // Body text containing a lookalike `tag_name` inside a *string* does
        // not match the key shape (no leading quote before the field name),
        // so the find lands on the real top-level pair.
        let doc = r#"{"body":"mentions tag_name:\"X\" as prose", "tag_name":"v0.2.0"}"#;
        let tag = extract_field(doc, "tag_name");
        assert_eq!(tag.as_deref(), Some("v0.2.0"));

        assert_eq!(extract_field("{}", "tag_name"), None);
        assert_eq!(extract_field("{\"nothing\":1}", "tag_name"), None);
    }

    #[test]
    fn asset_scan_finds_each_platforms_asset() {
        let json = r#"{"assets":[
            {"name":"editor-v0.1.0-macos.zip","browser_download_url":"https://gh/dl/v0.1.0/editor-v0.1.0-macos.zip"},
            {"name":"editor-v0.1.0-windows-x86_64.msi","browser_download_url":"https://gh/dl/v0.1.0/editor-v0.1.0-windows-x86_64.msi"},
            {"name":"editor-v0.1.0-windows-x86_64.zip","browser_download_url":"https://gh/dl/v0.1.0/editor-v0.1.0-windows-x86_64.zip"},
            {"name":"editor-v0.1.0-linux-x86_64.tar.gz","browser_download_url":"https://gh/dl/v0.1.0/editor-v0.1.0-linux-x86_64.tar.gz"}
        ]}"#;
        // Each platform's updater finds its own asset despite the coexisting
        // zip variant (the portable zip must not shadow the msi: the scan
        // anchors on the full suffix, not a prefix).
        for stage in ["windows-x86_64.msi", "linux-x86_64.tar.gz", "macos.zip"] {
            let url = find_asset(json, "editor-v0.1.0", stage).unwrap_or_default();
            assert!(url.ends_with(stage), "{stage} not found");
        }
    }

    #[test]
    fn asset_scan_misses_are_none() {
        assert_eq!(
            find_asset("{\"assets\":[]}", "editor-v9", "windows-x86_64.msi"),
            None
        );
        assert_eq!(find_asset("{}", "editor-v9", "macos.zip"), None);
    }

    #[test]
    fn updater_contract_matches_the_release_workflow() {
        // Pin the updater's expected artifact layout here so a future rename
        // in `.github/workflows/release.yml` fails a test instead of quietly
        // breaking every install's update check.
        let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
        let expected = match (os, arch) {
            ("windows", "x86_64") => Some("windows-x86_64.msi"),
            ("linux", "x86_64") => Some("linux-x86_64.tar.gz"),
            ("macos", _) => Some("macos.zip"),
            _ => None,
        };
        assert_eq!(asset_name(), expected);
    }
}
