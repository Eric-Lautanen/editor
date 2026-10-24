//! Self-update: find the newest Editor release on GitHub, download it through
//! whatever the OS already ships (see `src/net.rs` — `urlmon` then `curl` on
//! Windows, `curl`/`wget`/`python3` elsewhere; no crates, no bundled TLS, and
//! no script host anywhere), and install it. Two entry points:
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
//! | Windows | `editor-<tag>-windows-x86_64.msi` | a staged `install.bat`: wait for this process to exit, `msiexec /i <msi> /passive REBOOT=ReallySuppress`, wait for the installed exe's file stamp to change, then reopen it through Explorer — verified, retried, `start /max` as the last resort. The per-user MSI replaces the old build; the package itself launches nothing (see `packaging/windows/editor.wxs` for why that matters) |
//! | Linux | `editor-<tag>-linux-x86_64.tar.gz` | unpack, rename the fresh binary over the running one (legal while it runs on Linux), relaunch |
//! | macOS | `editor-<tag>-macos.zip` | unpack to a staging dir, remove the old `Editor.app`, move the new one into place, relaunch |
//!
//! When no official asset exists for the running platform/arch (macOS on
//! Intel, Linux on arm…) the check still works and says there is nothing to
//! auto-install. Asset names are owned by `release.yml`; a drift between the
//! two ends in a clear "download it manually" message, not a wrong download.
//!
//! Everything this module writes lives under `<temp>/editor-update-<tag>/`
//! and nowhere else, so the whole footprint is one directory that [the launch
//! sweep](sweep_stale_payloads), the installer's uninstall step
//! ([`clean_payloads`]) and a user with a file manager can each delete without
//! knowing anything about the update flow.
//!
//! Consent and integrity: the background check asks permission (native
//! yes/no) BEFORE anything is downloaded; `--update` needs no ask because the
//! user typed it. What always runs is the integrity gate: the release ships
//! `SHA256SUMS` (see release.yml) and the downloaded installer's SHA-256 is
//! checked against it before anything executes — a truncated or tampered
//! download is refused, never run.
use crate::error::{Error, Result};
use crate::net;
use crate::platform::print_line;

const REPO: &str = "Eric-Lautanen/editor";
const API_LATEST: &str = "https://api.github.com/repos/Eric-Lautanen/editor/releases/latest";
/// The latest-release endpoint is unauthenticated, so repeated rebuild-and-run
/// dev loops (which would each hit it at GUI launch) burn the IP's rate
/// budget. Skip the silent on-launch check when this is set; `--update` still
/// works.
const NO_CHECK_ENV: &str = "EDITOR_NO_UPDATE_CHECK";
/// Where the release index comes from. Overridable so the whole flow —
/// download, checksum, install, restart — can be exercised against a locally
/// built package instead of a published release (which is the only way to test
/// the install half before the release exists): point it at a loopback server
/// serving the release JSON. That is the whole reach of the knob: the asset
/// URLs *inside* that JSON still have to pass [`net::trusted`], so it can only
/// redirect the updater to GitHub or to the local machine, never to some
/// third-party host.
const API_ENV: &str = "EDITOR_UPDATE_API";

/// The release index URL for this run.
fn api_latest() -> String {
    std::env::var(API_ENV).unwrap_or_else(|_| API_LATEST.to_string())
}

/// The silent on-launch check runs at most this often: a timestamp file in
/// the config dir records the last attempt (not the last success — a machine
/// offline for a week tries again on its first online launch, which is the
/// point). `--update` ignores it; only the *automatic* check is throttled.
pub const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);
const LAST_CHECK_FILE: &str = "last_update_check";

/// Seconds since the epoch, or 0 if the clock is before it (a machine with a
/// dead CMOS battery still gets an update check, it just is not throttled).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Record that a check just happened, so the launch check does not repeat one
/// the user has already seen the result of — a manual check counts.
fn note_check_time() {
    if let Some(dir) = crate::settings::config_dir() {
        let _ = std::fs::write(dir.join(LAST_CHECK_FILE), format!("{}\n", now_secs()));
    }
}

/// Should this launch's background check run? Reads/creates the timestamp;
/// any IO problems (no config dir, read failure) resolve to "check" — the
/// throttle is a courtesy, not a correctness guarantee.
pub fn due_for_check() -> bool {
    let Some(dir) = crate::settings::config_dir() else {
        return true;
    };
    let stamp = dir.join(LAST_CHECK_FILE);
    let now = now_secs();
    let last = std::fs::read_to_string(&stamp)
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok());
    if last.is_none_or(|l| now.saturating_sub(l) >= CHECK_INTERVAL.as_secs()) {
        // Write through the attempt, success or not: a firewalled machine
        // must not retry every launch within the interval either, or the
        // black flash it cannot see turns into silent traffic we deny by
        // design. The next window opens whether this write happens or not.
        let _ = std::fs::write(&stamp, format!("{now}\n"));
        true
    } else {
        false
    }
}

pub fn run() -> Result<()> {
    print_line("editor self-update:");
    let _ = log_step("self-update: the console check started");
    // Leftovers from an update that already installed and exited before it
    // could tidy up (Windows: its own msi). The current tag's directory, if it
    // exists, is about to be replaced by `download` anyway.
    let swept = sweep_stale_payloads();
    if swept > 0 {
        print_line(&format!(
            "removed {swept} staged installer{} from a previous update",
            if swept == 1 { "" } else { "s" }
        ));
    }
    let Some(r) = (match newest_release() {
        Ok(any) => any,
        Err(e) => {
            let _ = log_step(&format!("self-update: the check failed: {e}"));
            return Err(e);
        }
    }) else {
        let _ = log_step(&format!(
            "self-update: already on the latest ({})",
            env!("CARGO_PKG_VERSION")
        ));
        print_line(&format!(
            "already on the latest version ({})",
            env!("CARGO_PKG_VERSION")
        ));
        return Ok(());
    };
    let _ = log_step(&format!(
        "self-update: {} -> {} found",
        env!("CARGO_PKG_VERSION"),
        r.tag
    ));
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
            let _ = log_step("background check started");
            let Ok(Some(r)) = newest_release() else {
                let _ = log_step("background check: already on the latest");
                return;
            };
            let _ = log_step(&format!("background check: {} available", r.tag));
            if !ask_install(&r.tag) {
                let _ = log_step("background check: the offer was declined");
                return;
            }
            let _ = install(&r);
            let _ = log_step("background check: the install sequence ended");
        })
        // A refused spawn skips the check for that one launch; not something
        // a GUI app can surface.
        .ok();
}

// ------------------------------------------------------------ manual check

/// What the About box has to say about the last manual check. The box paints
/// this *inline*, under its button: "you are up to date" is not news, and a
/// native message box for it made Windows play the alert sound for a
/// non-event. The only dialog left in this path is the offer to install a
/// version that actually exists, which has to ask before it downloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// Nothing asked yet; the box shows no status line.
    Idle,
    /// In flight.
    Checking,
    /// The newest release is this build.
    UpToDate,
    /// A newer release exists (its tag).
    Available(String),
    /// The check could not complete, with the reason.
    Failed(String),
}

/// Shared between the worker running the check and the UI painting it. A
/// `Mutex` rather than a channel: the app already ticks every 40 ms, so it can
/// read the current value and repaint when it changes, and nothing has to
/// wake a window from a background thread.
pub type Status = std::sync::Arc<std::sync::Mutex<Check>>;

pub fn new_status() -> Status {
    std::sync::Arc::new(std::sync::Mutex::new(Check::Idle))
}

/// The status right now. A poisoned lock reads as `Idle` rather than taking a
/// paint frame down with it.
pub fn current(status: &Status) -> Check {
    status.lock().map(|c| c.clone()).unwrap_or(Check::Idle)
}

fn report(status: &Status, c: Check) {
    if let Ok(mut slot) = status.lock() {
        *slot = c;
    }
}

/// The About box's "Check for updates now": the same flow as the launch check,
/// off the clock (no [`CHECK_INTERVAL`] throttle) and off the automatic-check
/// setting — the user asked, which is what `--update` means too. Runs on its
/// own thread so the window never blocks on the network, and it answers in
/// every case through [`Status`]: up to date, what went wrong, or an offer to
/// install. A button that silently does nothing when there is nothing new is
/// indistinguishable from a broken one.
pub fn check_now(status: Status) {
    // Before the spawn, so the box says "checking" the moment it is clicked
    // rather than after the thread's first instruction.
    report(&status, Check::Checking);
    let worker = status.clone();
    let spawned = std::thread::Builder::new()
        .name("update-check-manual".into())
        .spawn(move || {
            note_check_time();
            let _ = log_step("manual check started");
            match newest_release() {
                Ok(None) => report(&worker, Check::UpToDate),
                Ok(Some(r)) => {
                    report(&worker, Check::Available(r.tag.clone()));
                    let _ = log_step(&format!("manual check: {} available", r.tag));
                    if ask_install(&r.tag) {
                        if let Err(e) = install(&r) {
                            let _ = log_step(&format!(
                                "manual check: the offer was declined; install error: {e}"
                            ));
                            report(
                                &worker,
                                Check::Failed(format!("The update could not be installed: {e}")),
                            );
                        }
                    } else {
                        let _ = log_step("manual check: the offer was declined");
                    }
                }
                Err(e) => {
                    let _ = log_step(&format!("manual check failed: {e}"));
                    report(
                        &worker,
                        Check::Failed(format!("Could not check for updates: {e}")),
                    )
                }
            }
        });
    if spawned.is_err() {
        report(&status, Check::Failed("The check could not start.".into()));
    }
}

// ---------------------------------------------------------------- discovery

struct Release {
    tag: String,
    /// `browser_download_url` of this platform's install asset.
    asset_url: String,
    /// `browser_download_url` of the same release's `SHA256SUMS`.
    sums_url: String,
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
    let json = net::get_text(&api_latest(), 10)?;
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
    // The sums file is the integrity gate below; a release without one is a
    // packaging drift, and the updater refuses to install rather than trust
    // an unverified download.
    let sums_url = find_named_asset(&json, "SHA256SUMS").ok_or_else(|| {
        Error::Platform(format!(
            "release {tag} ships no SHA256SUMS; refusing to install an unverified binary - download by hand from github.com/{REPO}/releases/tag/{tag}"
        ))
    })?;
    Ok(Some(Release {
        tag,
        asset_url,
        sums_url,
    }))
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

/// Exact-filename sibling of `find_asset`, for the sums file (which does not
/// carry the tag in its name).
fn find_named_asset(json: &str, name: &str) -> Option<String> {
    const KEY: &str = "\"browser_download_url\":\"";
    let mut rest = json;
    while let Some(i) = rest.find(KEY) {
        rest = &rest[i + KEY.len()..];
        let end = rest.find('"')?;
        let url = &rest[..end];
        let fname = url.rsplit('/').next()?;
        if fname == name {
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
            (Ok(t), Some(c)) => {
                if let Ok(c) = c.parse::<u64>() {
                    if t != c {
                        return t > c;
                    }
                }
                // A non-numeric segment in the *running* version is a dev
                // build suffix; let the next segment decide.
            }
            // The tag names something past our (numerically exhausted)
            // version: `v1` vs `0.9.9`.
            (Ok(_), None) => return true,
            (Err(_), _) => {}
        }
    }
    false
}

// ---------------------------------------------------------------- footprint

/// Every staged download is one directory named with this prefix. The name is
/// stable across versions on purpose: the sweep and the uninstaller find old
/// payloads by it, without having to know which build wrote them.
const PAYLOAD_PREFIX: &str = "editor-update-";
/// A payload directory younger than this is left alone by the sweep: a
/// concurrent `--update` (or another instance between download and install)
/// may still be filling it. Older than this and nobody can be using it.
const PAYLOAD_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(120);

fn payload_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("{PAYLOAD_PREFIX}{tag}"))
}

/// Delete payload directories under `root` older than `min_age`; returns how
/// many went away. Only directories this app names are touched — a shared
/// temp dir holds plenty that is not ours.
fn sweep_in(root: &std::path::Path, min_age: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut gone = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(PAYLOAD_PREFIX) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_dir() {
            continue;
        }
        let fresh = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age < min_age);
        // A locked directory (Windows Installer still reading its own msi)
        // just fails to delete and is retried on the next launch.
        if !fresh && std::fs::remove_dir_all(entry.path()).is_ok() {
            gone += 1;
        }
    }
    gone
}

/// Launch cleanup: whatever an earlier version left staged. The install that
/// replaced that version cannot delete its own payload (Windows Installer is
/// reading the file), so the first launch of the *new* build does it — see
/// `cli::run`, which calls this on the GUI path rather than from the update
/// check, so a build that is not due to check for 12 hours still tidies up.
pub fn sweep_stale_payloads() -> usize {
    sweep_in(&std::env::temp_dir(), PAYLOAD_MIN_AGE)
}

/// Remove every staged payload, whatever its age, plus the sidecar a failed
/// Linux swap can leave next to the binary. `--uninstall` and the installer's
/// uninstall step both call this: removing the app has to mean the megabytes
/// it downloaded, not just its exe.
///
/// Settings, session state and crash reports live in the config dir and are
/// deliberately *not* touched — they are the user's, and a reinstall should
/// find the window where it was left.
pub fn clean_payloads() -> usize {
    let gone = sweep_in(&std::env::temp_dir(), std::time::Duration::ZERO);
    if let Ok(me) = std::env::current_exe() {
        let _ = std::fs::remove_file(me.with_extension("update-new"));
    }
    gone
}

// ---------------------------------------------------------------- fetching

impl Release {
    /// Downloads the asset to `<temp>/editor-update-<tag>/`, each call on a
    /// clean slate (a half-written file from a crashed earlier install would
    /// otherwise be silently reused), and removes the directory again if the
    /// download fails so a failed attempt costs no disk.
    fn download(&self) -> Result<std::path::PathBuf> {
        let dir = payload_dir(&self.tag);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(format!(
            "editor-{}-{}",
            self.tag,
            asset_name().unwrap_or_default()
        ));
        if let Err(e) = net::get_file(&self.asset_url, &file, 300) {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        Ok(file)
    }
}

// ---------------------------------------------------------------- integrity

fn install(r: &Release) -> Result<()> {
    rotate_update_log();
    let _ = log_step(&format!("install requested for {}", r.tag));
    let file = match r.download() {
        Ok(f) => {
            let _ = log_step(&format!("downloaded {}", f.display()));
            f
        }
        Err(e) => {
            let _ = log_step(&format!("download failed: {e}"));
            return Err(e);
        }
    };
    match verify(&file, r) {
        Ok(()) => {
            let _ = log_step("the checksum matches the release's SHA256SUMS");
        }
        Err(e) => {
            let _ = log_step(&format!("the checksum refused the download: {e}"));
            return Err(e);
        }
    }
    let _ = log_step("handing over to the platform installer");
    let outcome = match std::env::consts::OS {
        "windows" => install_windows(&file, &r.tag),
        "linux" => install_linux(&file, &r.tag),
        "macos" => install_macos(&file, &r.tag),
        other => Err(Error::Platform(format!(
            "no automated update for {other}; download from github.com/{REPO}/releases/tag/{}",
            r.tag
        ))),
    };
    if let Err(e) = &outcome {
        let _ = log_step(&format!("install failed: {e}"));
    }
    outcome
}

/// Compares the downloaded asset's SHA-256 with the one the release's
/// `SHA256SUMS` publishes for its exact filename. Mismatch, sums without our
/// entry, or unfetchable sums: hard refusal — a truncated download trips this
/// exactly like a substituted one, and there is no "install anyway". The
/// error text points at the manual download either way; in the silent
/// background path the thread just returns, leaving the running (trusted)
/// install untouched.
fn verify(file: &std::path::Path, r: &Release) -> Result<()> {
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::Platform("download path is not UTF-8".into()))?;
    let sums = net::get_text(&r.sums_url, 30)?;
    let want = parse_sums(&sums)
        .into_iter()
        .find(|(_, f)| f == name)
        .map(|(h, _)| h)
        .ok_or_else(|| {
            Error::Platform(format!(
                "SHA256SUMS lists no entry for {name}; download by hand from github.com/{REPO}/releases/tag/{}",
                r.tag
            ))
        })?;
    let got = sha256_file(file)?;
    if got != want {
        return Err(Error::Platform(format!(
            "checksum mismatch for {name}: the release published {want}, the download hashes to {got}. Refusing to run a file that is not the one published; if you trust the release, install it by hand from github.com/{REPO}/releases/tag/{}",
            r.tag
        )));
    }
    Ok(())
}

/// Parses `sha256sum`'s output format: `<64 hex>␣␣[*]<filename>` per line.
/// Unknown lines (release notes, blanks) are skipped, so a hand-edited sums
/// file cannot smuggle a parse error past the gate — its entries just have
/// to match, or our asset will not be found.
fn parse_sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.len() < 67 {
                return None; // 64 hex + separator + at least a name
            }
            let (hex, rest) = line.split_at(64);
            if !hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return None;
            }
            // `sha256sum` writes two spaces for text mode, one space plus `*`
            // for binary (`*` is meaningless here — the digest is over bytes).
            let name = rest.trim_start_matches([' ', '*']);
            if name.is_empty() {
                return None;
            }
            Some((hex.into(), name.into()))
        })
        .collect()
}

/// Hex digest of a file, chunked (installers are megabytes, never held whole).
fn sha256_file(path: &std::path::Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut hasher = sha256::Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finish();
    let mut hex = String::with_capacity(64);
    for b in digest {
        hex.push_str(&format!("{b:02x}"));
    }
    Ok(hex)
}

/// Pure-std SHA-256 (FIPS 180-4). No crates (the Cargo.toml covenant), and
/// shelling out to `certutil`/`shasum` would put the integrity check for a
/// download at the mercy of the same environment an attacker could shadow —
/// the algorithm is 60 lines, so it is inline.
mod sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    pub struct Sha256 {
        h: [u32; 8],
        buf: [u8; 64],
        used: usize,
        /// Bytes fed so far (padding excluded) — set into the final block.
        total: u64,
    }

    impl Sha256 {
        pub fn new() -> Self {
            Sha256 {
                h: [
                    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c,
                    0x1f83d9ab, 0x5be0cd19,
                ],
                buf: [0; 64],
                used: 0,
                total: 0,
            }
        }

        pub fn update(&mut self, mut data: &[u8]) {
            self.total = self.total.wrapping_add(data.len() as u64);
            if self.used > 0 {
                let take = (64 - self.used).min(data.len());
                self.buf[self.used..self.used + take].copy_from_slice(&data[..take]);
                self.used += take;
                data = &data[take..];
                if self.used == 64 {
                    let block = self.buf;
                    self.compress(&block);
                    self.used = 0;
                }
            }
            while data.len() >= 64 {
                let mut block = [0u8; 64];
                block.copy_from_slice(&data[..64]);
                self.compress(&block);
                data = &data[64..];
            }
            if !data.is_empty() {
                self.buf[..data.len()].copy_from_slice(data);
                self.used = data.len();
            }
        }

        pub fn finish(mut self) -> [u8; 32] {
            let bits = self.total.wrapping_mul(8); // before padding inflates total
            self.update(&[0x80]);
            // Cross the block boundary first (57..64 would leave no room for
            // the length), then pad up to byte 56, then the 8 length bytes
            // close that block — every case ends with exactly one compress
            // for the final block, even when the message is exactly 56 bytes
            // (padding lands alone in the next block there).
            while self.used > 56 {
                self.update(&[0]);
            }
            while self.used < 56 {
                self.update(&[0]);
            }
            self.update(&bits.to_be_bytes());
            let mut out = [0u8; 32];
            for (i, w) in self.h.iter().enumerate() {
                out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
            }
            out
        }

        fn compress(&mut self, block: &[u8; 64]) {
            let mut w = [0u32; 64];
            for (i, wi) in w.iter_mut().take(16).enumerate() {
                *wi = u32::from_be_bytes([
                    block[i * 4],
                    block[i * 4 + 1],
                    block[i * 4 + 2],
                    block[i * 4 + 3],
                ]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = self.h;
            for (i, wi) in w.iter().enumerate() {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ (!e & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(*wi);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            let add = [a, b, c, d, e, f, g, hh];
            for (hi, xi) in self.h.iter_mut().zip(add) {
                *hi = hi.wrapping_add(xi);
            }
        }
    }
}

// ---------------------------------------------------------------- installing
//
// Every step also appends a timestamped line to the config dir's
// `last-update.log`, one attempt per file with the previous attempt kept as
// `last-update.1.log`, and the install batch script keeps logging after this
// process exits (msiexec's exit code, the wait timeout, the relaunch). An
// update that silently died somewhere between "closed the app" and "editor
// back on screen" is diagnosable from those files afterwards.

/// Windows: run the per-user MSI. `/passive` shows a progress bar but asks no
/// questions, and a *newer* version installs over the old via the MSI's
/// major-upgrade, all without a UAC prompt (per-user MSI).
///
/// Restart Manager closes the running app when the exe has to be replaced, but
/// it does not reopen it, so the relaunch is ours: the staged batch waits for
/// this process to leave, installs, waits for the installed exe to be the new
/// one, and only then opens it (see [`windows_install_script`]) — so the window
/// comes back *after* the install has finished, and exactly once. Neither part
/// is optional. Launching from inside the package (a deferred custom action,
/// which this used to do) fires on every pass of the upgrade's execute
/// sequence: two Editors came up, the second hung on the first one's
/// single-instance mutex, and a process started mid-transaction can still hold
/// a file the install wants to replace — which is how Windows Installer ends
/// up scheduling a restart, i.e. rebooting the machine.
/// `REBOOT=ReallySuppress` closes that door for good: an update may close and
/// reopen the app, never restart the computer.
///
/// The relaunched app is argument-less on purpose: it restores the session it
/// saved on the way out, so the last file and the window place come back.
/// This process ends here: the `cmd` was spawned detached and survives it.
fn install_windows(file: &std::path::Path, tag: &str) -> Result<()> {
    let dir = parent_dir(file)?;
    // Where the installer's own exit code is left for the next launch: the
    // About box is the only place an update the user asked for can report
    // back, and a refused install used to be indistinguishable from one that
    // worked — both ended with the old build on screen.
    let result = install_result_path(&dir);
    if let Some(parent) = result.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(&result);
    let log_path = update_log_path(&dir);
    let script = windows_install_script(
        file,
        &installed_exe(),
        std::process::id(),
        &result,
        &log_path,
    );
    let bat = dir.join("install.bat");
    std::fs::write(&bat, script)?;
    let _ = log_step(&format!(
        "install.bat staged in {}; this process exits so the msiexec inside it can replace the exe",
        dir.display()
    ));
    print_line(&format!("launching the installer for {tag}…"));
    // The script waits for this process to leave the process table before it
    // runs msiexec, so nothing here holds a handle the upgrade wants.
    windows_hidden_cmd(&format!("\"{}\"", bat.display()))
        .map_err(|e| Error::Platform(format!("msiexec could not start: {e}")))?;
    std::process::exit(0);
}

/// The file the installer's exit code is written to (see
/// [`windows_install_script`]). The config directory survives the payload
/// sweep that deletes the staged MSI on the next launch; a payload directory
/// is only the fallback for an account with no config directory at all.
const INSTALL_RESULT: &str = "last-install.txt";

fn install_result_path(payload_dir: &std::path::Path) -> std::path::PathBuf {
    crate::settings::config_dir()
        .map(|d| d.join(INSTALL_RESULT))
        .unwrap_or_else(|| payload_dir.join(INSTALL_RESULT))
}

/// msiexec's exit code from the last Windows install attempt, read *and
/// cleared* so the notice appears once: an install that failed last week
/// must not greet every launch. `None` when there has been no attempt or the
/// last one succeeded (0, or 3010/1641 — installed, a restart is wanted).
pub fn take_install_result() -> Option<u32> {
    let path = crate::settings::config_dir()?.join(INSTALL_RESULT);
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let _ = log_step(&format!(
        "the next launch read the install result: {}",
        text.trim()
    ));
    text.trim()
        .parse::<u32>()
        .ok()
        .filter(|code| !matches!(code, 0 | 3010 | 1641))
}

// ---------------------------------------------------------------- evidence

/// What an update the user asked for left behind, after the fact: every step
/// of the check and the install appends a timestamped line to the config
/// dir's `last-update.log`. `last-install.txt` is conversational — removed
/// once its notice shows — while the log is historical: "closed and never
/// came back" is answered by reading that file, not by guessing which stage
/// died. Best effort everywhere; a log write never fails an update.
const UPDATE_LOG: &str = "last-update.log";
/// One previous attempt sits beside the current one, so a second attempt
/// cannot destroy the evidence of the first.
const UPDATE_LOG_OLD: &str = "last-update.1.log";

fn log_step(step: &str) -> std::io::Result<()> {
    use std::io::Write;
    let Some(dir) = crate::settings::config_dir() else {
        return Err(std::io::Error::other("no config directory"));
    };
    let line = format!("{} {step}\r\n", now_secs());
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(UPDATE_LOG))?
        .write_all(line.as_bytes())
}

/// Each batch script line the trail needs lives in the one place the batch
/// can find it. The script runs after this process has exited, so no
/// Rust-side logging can see the msiexec outcome — the batch writes it.
fn update_log_path(payload_dir: &std::path::Path) -> std::path::PathBuf {
    crate::settings::config_dir()
        .map(|d| d.join(UPDATE_LOG))
        .unwrap_or_else(|| payload_dir.join(UPDATE_LOG))
}

/// A new attempt owns the current log; the previous one moves aside.
fn rotate_update_log() {
    let Some(dir) = crate::settings::config_dir() else {
        return;
    };
    let path = dir.join(UPDATE_LOG);
    let keep = dir.join(UPDATE_LOG_OLD);
    let _ = std::fs::remove_file(&keep);
    let _ = std::fs::rename(&path, &keep);
}

/// Put a failed update in front of the user, in the About box's status line —
/// the same place a manual check answers. Called once at startup by `cli`.
pub fn note_install_result(status: &Status) {
    let Some(code) = take_install_result() else {
        return;
    };
    report(
        status,
        Check::Failed(format!(
            "The last update did not install (msiexec exit {code}). Download it from github.com/{REPO}/releases/latest and run the installer."
        )),
    );
}

/// How many one-second ticks the install script waits for this process to
/// leave the process table before installing anyway.
const WAIT_TICKS: u32 = 120;

/// How many one-second ticks the install script waits for the installer to put
/// the new exe on disk before reopening whatever is there.
const SETTLE_TICKS: u32 = 120;

/// How many times the relaunch asks Explorer to start the app before falling
/// back to `start`.
const LAUNCH_TRIES: u32 = 3;

/// The batch file a Windows update runs. A file, not a `cmd /c` one-liner:
/// the waits need `goto`, and the exit code has to be read *after* msiexec
/// runs — a one-liner expands `%errorlevel%` before anything starts.
///
/// Four things it has to get right, each one a failed update:
///
/// * **Wait for this process to exit.** An upgrade removes the old files, and
///   `editor.exe` is one of them; deleting an executable a live process still
///   maps is how an update ended in msiexec 1603 with no window and no new
///   version.
/// * **Record the installer's exit code**, where the next launch reads it. A
///   refused install and a successful one used to look identical.
/// * **Reopen the new build, not the old one.** msiexec returning is not the
///   same thing as the new exe being on disk: the install can still be
///   finishing in a pass the batch did not start (elevation is one way that
///   happens), and reopening too early starts the old build, which Windows
///   Installer then closes when it replaces the file. The update ends with the
///   app gone and nothing on screen: no window, no new version in front of the
///   user, and the exit code saying success. So the script takes the installed
///   exe's file stamp before the install and waits for it to change before
///   opening anything. The wait is bounded, and it gives up as soon as no
///   msiexec is left, so a refused install reopens what is there instead of
///   hanging.
/// * **Open it visibly, and check that it opened.** This console is created
///   hidden (`CREATE_NO_WINDOW`), and `start` passes its show state on, which
///   opened the new build invisibly — then its single-instance lock kept the
///   next double-click from showing a window either. Explorer does not, so
///   Explorer is the first choice; but a shell hand-off reports nothing when
///   it fails, so each attempt looks for the process and retries, and
///   `start /max` — an explicit show state, so it cannot come up hidden — is
///   the last resort. Every attempt reaches the log.
///
/// Two details are load-bearing and easy to lose in a tidy-up:
///
/// * `find.exe` is named by full path. Git for Windows ships a GNU `find` and
///   puts its `bin` on `PATH`; a bare `find` in that environment resolves to
///   it, which fails on a bare number, so the loop decided on its first tick
///   that this process was already gone — the wait was a no-op. The same path
///   is used for the process check after the relaunch.
/// * Both waits are bounded. A PID that never leaves the table (the number was
///   reused by something else) must not leave the update spinning for ever
///   with no window, no new version and nothing to report.
///
/// Kept as a plain string so all of it can be asserted without an installer.
fn windows_install_script(
    msi: &std::path::Path,
    exe: &std::path::Path,
    pid: u32,
    result: &std::path::Path,
    log: &std::path::Path,
) -> String {
    // Split by hand rather than with `Path::file_name`: the script is built on
    // every host the tests run on, and `file_name` knows only that host's
    // separators — on Linux the whole `C:\...\editor.exe` is one file name,
    // so the process check below asked for an image nothing is called.
    let image = exe
        .to_string_lossy()
        .rsplit(['\\', '/'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("editor.exe")
        .to_string();
    let mut s = String::new();
    s.push_str("@echo off\r\n");
    s.push_str("set /a waited=0\r\n");
    s.push_str(":wait\r\n");
    s.push_str(&format!(
        "tasklist /FI \"PID eq {pid}\" 2>nul | \"%SystemRoot%\\System32\\find.exe\" \"{pid}\" >nul\r\n"
    ));
    s.push_str("if errorlevel 1 goto install\r\n");
    // The timeout install-anyway is worth knowing about later: it is how a
    // wait race becomes a 1603 with no window (see the comment above).
    s.push_str(&format!(
        "if %waited% geq {WAIT_TICKS} >>\"{}\" echo the app was still running after {WAIT_TICKS}s, installing anyway\r\n",
        log.display()
    ));
    s.push_str(&format!("if %waited% geq {WAIT_TICKS} goto install\r\n"));
    s.push_str("set /a waited+=1\r\n");
    s.push_str("ping -n 2 127.0.0.1 >nul\r\n");
    s.push_str("goto wait\r\n");
    s.push_str(":install\r\n");
    // The before picture the relaunch waits on: the installed exe's own file
    // stamp. Nothing else on this side of the install is trustworthy evidence
    // that the new build has landed (see the comment above).
    s.push_str(&format!(
        "set \"stamp=\"\r\nfor %%f in (\"{}\") do set \"stamp=%%~tf\"\r\n",
        exe.display()
    ));
    s.push_str(&format!(
        "msiexec /i \"{}\" /passive REBOOT=ReallySuppress\r\n",
        msi.display()
    ));
    // The redirection comes first on purpose: `echo %errorlevel%>file` becomes
    // `echo 1603>file`, and cmd reads the digit before `>` as a stream number,
    // so a `0` exit wrote the code nowhere at all.
    s.push_str(&format!(">\"{}\" echo %errorlevel%\r\n", result.display()));
    // Same code into the log: last-install.txt is removed when its notice
    // shows; the log is the historical copy.
    s.push_str(&format!(
        ">>\"{}\" echo msiexec exit %errorlevel%\r\n",
        log.display()
    ));
    s.push_str("set /a settled=0\r\n");
    s.push_str(":settle\r\n");
    s.push_str(&format!(
        "for %%f in (\"{}\") do if not \"%%~tf\"==\"%stamp%\" goto launch\r\n",
        exe.display()
    ));
    // An installer that is gone without touching the exe was refused: reopen
    // what is installed (the About box names the exit code next launch) rather
    // than sit out the timeout with no window on screen.
    //
    // Scoped to this session on purpose. `msiexec.exe` is not only the install
    // pass: the Windows Installer *service* is a permanent session-0 msiexec
    // that outlives every install, so an unfiltered image-name match answers
    // "an installer is still running" for the whole 120-tick timeout even
    // after the msi came back and changed nothing — two minutes of no window
    // for an install that already knows it failed. The pass this wait is about
    // runs in the session that started it, so match that.
    s.push_str(
        "tasklist /FI \"IMAGENAME eq msiexec.exe\" /FI \"SESSIONNAME eq %SESSIONNAME%\" 2>nul | \"%SystemRoot%\\System32\\find.exe\" /i \"msiexec.exe\" >nul\r\n",
    );
    s.push_str(&format!(
        "if errorlevel 1 >>\"{}\" echo the installer left the exe alone; opening what is installed\r\n",
        log.display()
    ));
    s.push_str("if errorlevel 1 goto launch\r\n");
    s.push_str(&format!(
        "if %settled% geq {SETTLE_TICKS} >>\"{}\" echo the installer was still running after {SETTLE_TICKS}s; opening the app anyway\r\n",
        log.display()
    ));
    s.push_str(&format!("if %settled% geq {SETTLE_TICKS} goto launch\r\n"));
    s.push_str("set /a settled+=1\r\n");
    s.push_str("ping -n 2 127.0.0.1 >nul\r\n");
    s.push_str("goto settle\r\n");
    s.push_str(":launch\r\n");
    // One beat first: the stamp changes when the file starts landing, and a
    // launch that beats the copy earns an Explorer error instead of a window.
    s.push_str("ping -n 3 127.0.0.1 >nul\r\n");
    s.push_str("set /a tried=0\r\n");
    s.push_str(":attempt\r\n");
    s.push_str(&format!("explorer.exe \"{}\"\r\n", exe.display()));
    s.push_str("set /a tried+=1\r\n");
    s.push_str(&format!(
        ">>\"{}\" echo relaunch attempt %tried%: explorer {}\r\n",
        log.display(),
        exe.display()
    ));
    // Long enough for a cold start to show up in the process table; the check
    // is what turns "explorer was asked" into "the app is running".
    s.push_str("ping -n 4 127.0.0.1 >nul\r\n");
    s.push_str(&format!(
        "tasklist /FI \"IMAGENAME eq {image}\" 2>nul | \"%SystemRoot%\\System32\\find.exe\" /i \"{image}\" >nul\r\n"
    ));
    s.push_str(&format!(
        "if not errorlevel 1 >>\"{}\" echo relaunch: {image} is running\r\n",
        log.display()
    ));
    s.push_str("if not errorlevel 1 goto end\r\n");
    s.push_str(&format!(
        ">>\"{}\" echo relaunch attempt %tried% started nothing\r\n",
        log.display()
    ));
    s.push_str(&format!("if %tried% geq {LAUNCH_TRIES} goto fallback\r\n"));
    s.push_str("goto attempt\r\n");
    s.push_str(":fallback\r\n");
    // The last resort: `start` would pass this hidden console's show state on,
    // but with `/max` the app is opened with an explicit one, so a maximized
    // window beats no window. The session's own place is not consulted here,
    // and the log says the fallback was the route that worked.
    s.push_str(&format!("start \"\" /max \"{}\"\r\n", exe.display()));
    s.push_str(&format!(
        ">>\"{}\" echo relaunch fallback: start /max {}\r\n",
        log.display(),
        exe.display()
    ));
    s.push_str(":end\r\n");
    s
}

/// `%LOCALAPPDATA%\Programs\Editor\editor.exe` — where `editor.wxs` puts the
/// per-user install (`PerUserProgramFilesFolder` + the `Editor` folder). Falls
/// back to this exe in an environment with no `LOCALAPPDATA`, which only a
/// stripped account has.
fn installed_exe() -> std::path::PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(local) => std::path::PathBuf::from(local)
            .join("Programs")
            .join("Editor")
            .join("editor.exe"),
        None => std::env::current_exe().unwrap_or_default(),
    }
}

/// Run the install script with no console of its own: this is a GUI process, so
/// a `cmd` window would flash in front of the user for the length of an install.
#[cfg(windows)]
fn windows_hidden_cmd(script: &str) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("cmd.exe")
        .raw_arg(format!("/c {script}"))
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn()
}

#[cfg(not(windows))]
fn windows_hidden_cmd(_script: &str) -> std::io::Result<std::process::Child> {
    Err(std::io::Error::other(
        "the MSI install path is Windows-only",
    ))
}

/// The directory a staged download lives in. Every caller unpacks *next to*
/// the archive (that is what makes the final move a same-volume rename), and
/// an update is not a place to abort the process because a path had no
/// parent: report it and let the caller's consent/retry path handle it.
fn parent_dir(file: &std::path::Path) -> Result<std::path::PathBuf> {
    file.parent().map(|p| p.to_path_buf()).ok_or_else(|| {
        Error::Platform(format!("the download has no directory: {}", file.display()))
    })
}

/// Linux: unpack and rename the fresh binary over the running one. POSIX
/// `rename` replaces an open executable in place, so this instance finishes
/// serving files on the old image while the next launch (desktop entry,
/// terminal) runs the new build.
fn install_linux(file: &std::path::Path, tag: &str) -> Result<()> {
    let dir = parent_dir(file)?;
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
            // The tarball and its unpacked twin have done their job; delete
            // them before the handoff (nothing else will: this is the only
            // process that knows the directory).
            let _ = std::fs::remove_dir_all(&dir);
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
    let dir = parent_dir(file)?;
    run_unpack("ditto", &["-x".into(), "-k".into()], file, &dir)?;
    let fresh = dir.join("Editor.app");
    let me = std::env::current_exe()
        .map_err(|e| Error::Platform(format!("could not resolve the running exe path: {e}")))?;
    // Find the `.app` root my exe is nested inside. A bare cargo-run has
    // none: report the bundle path instead of guessing at ~half-replaced.
    let Some(bundle) = me
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(|p| p.to_path_buf())
    else {
        return Err(Error::Platform(format!(
            "{tag} unpacked to {} but this editor is not running from an Editor.app; replace the bundle by hand",
            dir.display()
        )));
    };
    std::fs::remove_dir_all(&bundle)?;
    std::fs::rename(&fresh, &bundle)?;
    // The zip and any unpacking leftovers: nothing else removes them.
    let _ = std::fs::remove_dir_all(&dir);
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
///
/// Each surface is gated on the *backend* the build carries, not on the host
/// OS: this box's owner comes from `platform::main_window`, which only the
/// Win32 backend has, and the `editor_check_*` builds compile one backend at
/// a time to lint it from another host. The three gates below are mutually
/// exclusive and cover every build, so exactly one surface exists.
#[cfg(all(
    any(target_os = "windows", editor_check_win),
    not(editor_check_linux),
    not(editor_check_macos)
))]
fn ask_install(tag: &str) -> bool {
    win_dialog(
        &format!(
            "Editor {tag} is available. Download and install it now?\n\nEditor will close and restart with the new version."
        ),
        "Editor update",
        true,
    )
}

#[cfg(all(
    any(target_os = "windows", editor_check_win),
    not(editor_check_linux),
    not(editor_check_macos)
))]
fn win_dialog(text: &str, caption: &str, yes_no: bool) -> bool {
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
    const MB_OK: u32 = 0x0000;
    const MB_SYSTEMMODAL: u32 = 0x1000;
    const IDYES: i32 = 6;
    let to_wide = |s: &str| {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<std::vec::Vec<u16>>()
    };
    // Own the box to the app's window, and wait for that window to exist: this
    // runs on the check's own thread, which on the launch path can get here
    // before the window is created. An ownerless box mixes with the app's own
    // window in z-order and can end up behind it — the shape of a user
    // reporting "no update prompt at all". Capped: a headless or still-broken
    // window must not hold the check forever.
    let mut owner = crate::platform::main_window();
    for _ in 0..20 {
        if !owner.is_null() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        owner = crate::platform::main_window();
    }
    unsafe {
        MessageBoxW(
            owner,
            to_wide(text).as_ptr(),
            to_wide(caption).as_ptr(),
            MB_ICONINFORMATION
                | MB_SETFOREGROUND
                | MB_TOPMOST
                | MB_SYSTEMMODAL
                | if yes_no { MB_YESNO } else { MB_OK },
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
        "display dialog \"Editor {tag} is available. Download and install it now?\" with title \"Editor update\" buttons {{\"Not now\",\"Install\"}} default button \"Install\""
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

#[cfg(all(
    any(all(unix, not(target_os = "macos")), editor_check_linux),
    not(editor_check_win),
    not(editor_check_macos)
))]
fn ask_install(tag: &str) -> bool {
    // The same zenity/kdialog pair the file dialogs use (see
    // `platform::linux`), so on a kit where the user picked dialogs with,
    // update consent appears with the same look. The first present wins:
    // if zenity answered at all, do not re-ask with kdialog.
    let text = format!("Editor {tag} is available. Download and install it now?");
    let question = format!("--text={text}");
    if let Some(yes) = ask_with("zenity", &["--question", &question]) {
        return yes;
    }
    ask_with("kdialog", &["--yesno", &question]).unwrap_or(false)
}

/// Runs a dialog helper and maps its exit code; `None` = helper not installed
/// (its name is not on `PATH`), the caller tries the next.
#[cfg(all(
    any(all(unix, not(target_os = "macos")), editor_check_linux),
    not(editor_check_win),
    not(editor_check_macos)
))]
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
    fn sha256_matches_the_published_test_vectors() {
        // FIPS 180-4 vectors, via temp files so the real file path is what is
        // hashed.
        let cases: &[(&str, &str)] = &[
            (
                "",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                "abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
        ];
        for (i, (input, want)) in cases.iter().enumerate() {
            let p = crate::testing::temp_dir().join(format!("editor-sha-test-{i}"));
            std::fs::write(&p, input).unwrap();
            assert_eq!(sha256_file(&p).unwrap(), *want, "input {i}");
            let _ = std::fs::remove_file(&p);
        }
    }

    #[test]
    fn sha256_streams_across_chunk_boundaries() {
        // A buffer larger than one block, built byte by byte, must hash the
        // same as fed all at once (this is where a broken `update` shows).
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let p = crate::testing::temp_dir().join("editor-sha-test-chunk");
        std::fs::write(&p, &data).unwrap();
        let streamed = sha256_file(&p).unwrap();

        let mut one = sha256::Sha256::new();
        one.update(&data);
        let once = one.finish();
        let mut hex = String::with_capacity(64);
        for b in once {
            hex.push_str(&format!("{b:02x}"));
        }
        assert_eq!(streamed, hex);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn sums_parsing_tolerates_the_shasum_dialect_and_skips_junk() {
        let sums = join_lines(&[
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  editor-v0.2.0-windows-x86_64.msi",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad *editor-v0.2.0-macos.zip",
            "# a comment line from release notes",
            "",
            "short line",
            "NOTHEXNOTHEXNOTHEXNOTHEXNOTHEXNOTHEXNOTHEXNOTHEXNOTHEXNOT  some-file",
        ]);
        let parsed = parse_sums(&sums);
        assert_eq!(parsed.len(), 2, "junk lines must be skipped");
        assert_eq!(
            parsed[0],
            (
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
                "editor-v0.2.0-windows-x86_64.msi".to_string()
            )
        );
        assert_eq!(
            parsed[1].1, "editor-v0.2.0-macos.zip",
            "binary-marker star must be stripped"
        );
    }

    /// The launched sweep is the only thing that deletes a staged installer
    /// after it has been consumed, and the uninstall hook is the only
    /// guarantee — so both directions are pinned here: old payload dirs go,
    /// fresh ones and everything else in a shared temp dir stay.
    #[test]
    fn sweep_removes_old_payloads_and_nothing_else() {
        let root = crate::testing::temp_dir().join("editor_sweep_test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("editor-update-v0.0.1")).unwrap();
        std::fs::write(
            root.join("editor-update-v0.0.1")
                .join("editor-v0.0.1-windows-x86_64.msi"),
            b"payload",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("editor-update-v0.0.2")).unwrap();
        std::fs::create_dir_all(root.join("someone-elses-dir")).unwrap();
        std::fs::write(root.join("editor-update-file"), b"not a directory").unwrap();

        assert_eq!(sweep_in(&root, std::time::Duration::ZERO), 2);
        assert!(!root.join("editor-update-v0.0.1").exists());
        assert!(!root.join("editor-update-v0.0.2").exists());
        assert!(root.join("someone-elses-dir").exists());
        assert!(root.join("editor-update-file").exists());

        // A directory that is still being filled (younger than the guard) is
        // left for the next launch.
        std::fs::create_dir_all(root.join("editor-update-v0.0.3")).unwrap();
        assert_eq!(sweep_in(&root, PAYLOAD_MIN_AGE), 0);
        assert!(root.join("editor-update-v0.0.3").exists());

        // `clean_payloads` (the uninstall hook) ignores age by construction.
        assert_eq!(sweep_in(&root, std::time::Duration::ZERO), 1);
        assert!(!root.join("editor-update-v0.0.3").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn staged_payloads_live_in_one_named_temp_directory() {
        let dir = payload_dir("v0.2.1");
        // The runtime answer is the OS temp dir, not the test fallback: this
        // test compares paths only and never writes, so it pins the real
        // staging shape on every machine, writable temp or not.
        assert_eq!(dir.parent().unwrap(), std::env::temp_dir());
        assert_eq!(dir.file_name().unwrap(), "editor-update-v0.2.1");
        assert!(dir
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with(PAYLOAD_PREFIX));
    }

    fn join_lines(lines: &[&str]) -> String {
        let mut out = String::new();
        for l in lines {
            out.push_str(l);
            out.push('\n');
        }
        out
    }

    #[test]
    fn named_asset_finds_the_sums_file() {
        let json = r#"{"assets":[
            {"name":"SHA256SUMS","browser_download_url":"https://gh/dl/v0.2.0/SHA256SUMS"},
            {"name":"editor-v0.2.0-macos.zip","browser_download_url":"https://gh/dl/v0.2.0/editor-v0.2.0-macos.zip"}
        ]}"#;
        assert_eq!(
            find_named_asset(json, "SHA256SUMS").unwrap(),
            "https://gh/dl/v0.2.0/SHA256SUMS"
        );
        assert_eq!(find_named_asset(json, "editor-v0.2.0-linux.tar.gz"), None);
    }

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
    fn launch_check_respects_the_twelve_hour_throttle() {
        // The tests' config dir is a throwaway (see settings::config_base),
        // but delete the stamp anyway so the sequence below starts fresh.
        let dir = crate::settings::config_dir().expect("test config dir");
        let stamp = dir.join(LAST_CHECK_FILE);
        let _ = std::fs::remove_file(&stamp);

        // First launch checks (and records the attempt); a second one within
        // the interval must not. Both calls are "attempts", so a flaky
        // network between them would still be throttled — by design.
        assert!(due_for_check());
        assert!(stamp.is_file(), "throttle must persist its attempt");
        assert!(!due_for_check());

        // An old timestamp (13h ago) reopens the window.
        let old = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            - CHECK_INTERVAL.as_secs()
            - 3600;
        std::fs::write(&stamp, format!("{old}\n")).unwrap();
        assert!(due_for_check());
        let _ = std::fs::remove_file(&stamp);
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

    #[test]
    fn windows_install_waits_then_installs_then_relaunches() {
        let msi =
            r"C:\Users\me\AppData\Local\Temp\editor-update-v0.2.0\editor-v0.2.0-windows-x86_64.msi";
        let exe = r"C:\Users\me\AppData\Local\Programs\Editor\editor.exe";
        let result = r"C:\Users\me\AppData\Roaming\Editor\last-install.txt";
        let log = r"C:\Users\me\AppData\Roaming\Editor\last-update.log";
        let script = windows_install_script(
            std::path::Path::new(msi),
            std::path::Path::new(exe),
            4321,
            std::path::Path::new(result),
            std::path::Path::new(log),
        );
        // The installer must install, then open the app once it is the new
        // build — never before, or the window it opens is the one the upgrade
        // is about to close.
        let install = script.find("msiexec /i \"").expect("installer line");
        let launch = script.find(":launch\r\n").expect("the launch stage");
        let explorer_line = script
            .find(&format!("explorer.exe \"{exe}\""))
            .expect("relaunch line");
        assert!(install < launch, "both present, install first");
        assert!(launch < explorer_line, "and the launch stage opens the app");
        assert_eq!(script.matches("msiexec /i ").count(), 1);
        // An update may close and reopen the app; it may never restart the box.
        assert!(script.contains("/passive"));
        assert!(script.contains("REBOOT=ReallySuppress"));
        assert!(!script.contains("EDITOR_RESTART"));

        // The upgrade removes the old exe, so it must wait for this process to
        // leave the process table first — that race is what produced 1603.
        assert!(script.contains("PID eq 4321"), "waits on this process");
        assert!(script.contains("goto wait"), "and keeps waiting");
        assert!(
            script.find("goto wait").unwrap() < install,
            "the wait comes before the install"
        );
        // A bare `find` is whatever `PATH` says it is — on a machine with Git
        // for Windows that is GNU find, which rejects a bare number and made
        // the wait end on its first tick.
        assert!(
            script.contains(r#""%SystemRoot%\System32\find.exe""#),
            "the Windows find is named by full path"
        );
        // A timed-out wait says so instead of installing silently: that is
        // how a wait race becomes a no-window 1603, and the log is the only
        // way to see it after the fact.
        assert!(
            script.contains("still running after 120s, installing anyway"),
            "the timeout is logged, not just tolerated"
        );
        // Bounded, so a reused PID cannot spin here for ever.
        let cap = script.find("if %waited% geq").expect("a bound on the wait");
        assert!(cap < install, "and it is checked before the install");
        assert!(script.contains("set /a waited=0"), "ticked from zero");

        // The exit code has to be captured *after* msiexec, which a batch file
        // does and a `/c` one-liner does not (it expands at parse time).
        let echo = script
            .find("echo %errorlevel%")
            .expect("records the result");
        assert!(echo > install, "after the installer runs");
        // `echo %errorlevel%>file` turns into `echo 0>file` on success, and cmd
        // reads that `0` as a stream number — the result went missing.
        assert!(
            script.contains(&format!(">\"{result}\" echo")),
            "redirection first, so the code cannot be read as a stream number"
        );

        // msiexec exiting is not the new build being on disk: the script takes
        // the installed exe's stamp before the install and waits for it to
        // change, which is what an early-returning (elevated, handed-off)
        // install leaves behind.
        let stamp = script
            .find(&format!("for %%f in (\"{exe}\") do set \"stamp=%%~tf\""))
            .expect("captures the installed exe's stamp");
        assert!(stamp < install, "the stamp is the before picture");
        assert!(
            script.contains(&format!(
                "for %%f in (\"{exe}\") do if not \"%%~tf\"==\"%stamp%\" goto launch"
            )),
            "the new build is waited for, not assumed"
        );
        // And the wait cannot outlive an installer that is already gone: a
        // refused install opens what is installed and leaves the code to the
        // next launch's notice instead of leaving the screen empty.
        assert!(script.contains("IMAGENAME eq msiexec.exe"));
        // Matching the image name alone would find the Windows Installer
        // service, which runs in session 0 and never goes away, so the escape
        // below would never fire.
        assert!(script.contains("/FI \"SESSIONNAME eq %SESSIONNAME%\""));
        assert!(script.contains("left the exe alone; opening what is installed"));
        assert!(script.contains("still running after 120s; opening the app anyway"));

        // A shell hand-off reports nothing when it fails, so each attempt is
        // verified against the process table and retried; `start` is the last
        // resort, and `/max` gives it an explicit show state.
        assert!(script.contains("IMAGENAME eq editor.exe"));
        assert!(script.contains("if not errorlevel 1 goto end"));
        assert!(script.contains(&format!("if %tried% geq {LAUNCH_TRIES} goto fallback")));
        assert!(script.contains(&format!("start \"\" /max \"{exe}\"")));
        assert_eq!(
            script.matches("start \"\"").count(),
            1,
            "start is the last resort, never the first choice"
        );

        // The batch outlives this process, so it writes the trail itself, and
        // every path through the tail of the update says what it did.
        for line in [
            "echo msiexec exit %errorlevel%",
            "echo relaunch attempt %tried%: explorer",
            "echo relaunch: editor.exe is running",
            "echo relaunch attempt %tried% started nothing",
            "echo relaunch fallback: start /max",
        ] {
            assert!(
                script.contains(&format!(">>\"{log}\" {line}")),
                "missing from the trail: {line}"
            );
        }
        let relaunch_log = script
            .find("echo relaunch attempt")
            .expect("logs the attempt");
        assert!(
            explorer_line < relaunch_log,
            "the relaunch is logged after it is attempted"
        );
    }

    /// The process check after the relaunch names the exe whatever separators
    /// the path came with. The script is built on every host the suite runs
    /// on, and `Path::file_name` knows only the host's own separators — on
    /// Linux a `C:\...\editor.exe` was one file name, so the check asked for
    /// an image that nothing is called.
    #[test]
    fn the_relaunch_check_names_the_exe_on_any_host() {
        for exe in [
            r"C:\Users\me\AppData\Local\Programs\Editor\editor.exe",
            "/home/me/.local/bin/editor",
        ] {
            let script = windows_install_script(
                std::path::Path::new(
                    r"C:\Users\me\AppData\Local\Temp\x\editor-v0.2.0-windows-x86_64.msi",
                ),
                std::path::Path::new(exe),
                4321,
                std::path::Path::new(r"C:\Users\me\AppData\Roaming\Editor\last-install.txt"),
                std::path::Path::new(r"C:\Users\me\AppData\Roaming\Editor\last-update.log"),
            );
            let image = exe.rsplit(['\\', '/']).next().unwrap();
            assert!(
                script.contains(&format!("IMAGENAME eq {image}")),
                "{exe}: the process check does not name {image}"
            );
        }
    }

    #[test]
    fn a_failed_install_is_reported_once_and_success_is_not() {
        let dir = crate::settings::config_dir().expect("test config dir");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("last-install.txt");

        std::fs::write(&path, "1603\r\n").unwrap();
        assert_eq!(take_install_result(), Some(1603));
        assert!(!path.exists(), "taken, so the notice shows once");
        assert_eq!(take_install_result(), None);

        for ok in ["0", "3010", "1641"] {
            std::fs::write(&path, ok).unwrap();
            assert_eq!(take_install_result(), None, "{ok} is success");
            assert!(!path.exists());
        }
    }

    /// The trail an update leaves: one attempt's lines stay behind it, and a
    /// new attempt moves the previous one aside rather than destroying it.
    #[test]
    fn the_update_log_rotates_and_keeps_the_previous_attempt() {
        let dir = crate::settings::config_dir().expect("test config dir");
        let path = dir.join(UPDATE_LOG);
        let older = dir.join(UPDATE_LOG_OLD);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&older);

        log_step("attempt one, step one").unwrap();
        log_step("attempt one, step two").unwrap();
        let current = std::fs::read_to_string(&path).unwrap();
        assert!(current.contains("attempt one, step one"));
        assert!(current.contains("attempt one, step two"));

        rotate_update_log();
        assert!(!path.exists(), "a new attempt starts with a fresh log");
        let first = std::fs::read_to_string(&older).unwrap();
        assert!(first.contains("attempt one, step one"));

        log_step("attempt two").unwrap();
        let again = std::fs::read_to_string(&path).unwrap();
        assert!(again.contains("attempt two"));
        assert!(
            !again.contains("attempt one"),
            "the old attempt moved aside"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&older);
    }

    #[test]
    fn the_package_launches_only_from_its_finish_page() {
        // Regression, and the reason this file owns an *update's* relaunch: a
        // deferred `asyncNoWait` action in the MSI fired on both passes of a
        // major upgrade, so a second Editor came up and hung on the first one's
        // single-instance mutex, and the process it left behind mid-transaction
        // pushed Windows Installer into scheduling a restart.
        //
        // A fresh install still has to end with the app open, so the package
        // does launch — from the completion page, after `InstallFinalize`, once,
        // and only when the user's checked box says so. That is the shape this
        // pins; anything scheduled in the execute sequence is the old failure.
        let wxs = std::fs::read_to_string("packaging/windows/editor.wxs").expect("editor.wxs");
        assert!(!wxs.contains("Id=\"RestartEditor\""));
        assert!(
            !wxs.contains("<Custom Action=\"LaunchEditor\""),
            "the launch must not be in the execute sequence"
        );
        let start = wxs
            .find("Id=\"LaunchEditor\"")
            .expect("the finish page's launch action");
        let end = wxs[start..].find("/>").expect("the action element ends");
        let action = &wxs[start..start + end];
        assert!(
            action.contains("Execute=\"immediate\""),
            "only an immediate action runs after the transaction: {action}"
        );
        assert!(
            action.contains("Return=\"asyncNoWait\""),
            "the installer must not wait on the editor it just opened: {action}"
        );
        assert!(
            wxs.contains(
                "<Publish Event=\"DoAction\" Value=\"LaunchEditor\" Condition=\"LAUNCHAPP\""
            ),
            "Finish publishes the launch, gated on the checkbox"
        );
        assert!(
            wxs.contains("Property=\"LAUNCHAPP\""),
            "the completion page has the box that gates it"
        );
    }
}
