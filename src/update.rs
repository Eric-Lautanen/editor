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
//!
//! Consent and integrity: the background check asks permission (native
//! yes/no) BEFORE anything is downloaded; `--update` needs no ask because the
//! user typed it. What always runs is the integrity gate: the release ships
//! `SHA256SUMS` (see release.yml) and the downloaded installer's SHA-256 is
//! checked against it before anything executes — a truncated or tampered
//! download is refused, never run.
use crate::error::{Error, Result};
use crate::platform::print_line;

const REPO: &str = "Eric-Lautanen/editor";
const API_LATEST: &str = "https://api.github.com/repos/Eric-Lautanen/editor/releases/latest";
/// The latest-release endpoint is unauthenticated, so repeated rebuild-and-run
/// dev loops (which would each hit it at GUI launch) burn the IP's rate
/// budget. Skip the silent on-launch check when this is set; `--update` still
/// works.
const NO_CHECK_ENV: &str = "EDITOR_NO_UPDATE_CHECK";

/// The silent on-launch check runs at most this often: a timestamp file in
/// the config dir records the last attempt (not the last success — a machine
/// offline for a week tries again on its first online launch, which is the
/// point). `--update` ignores it; only the *automatic* check is throttled.
pub const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);
const LAST_CHECK_FILE: &str = "last_update_check";

/// Should this launch's background check run? Reads/creates the timestamp;
/// any IO problems (no config dir, read failure) resolve to "check" — the
/// throttle is a courtesy, not a correctness guarantee.
pub fn due_for_check() -> bool {
    let Some(dir) = crate::settings::config_dir() else {
        return true;
    };
    let stamp = dir.join(LAST_CHECK_FILE);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
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
    // On Windows a GUI-subsystem parent has no console to inherit, so a child
    // curl would get one allocated for it — a black window that flashes for
    // the length of the call. CREATE_NO_WINDOW suppresses that; every other
    // platform runs curl as an ordinary child.
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    #[cfg(target_os = "windows")]
    use std::os::windows::process::CommandExt;
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

// ---------------------------------------------------------------- integrity

fn install(r: &Release) -> Result<()> {
    let file = r.download()?;
    verify(&file, r)?;
    match std::env::consts::OS {
        "windows" => install_windows(&file, &r.tag),
        "linux" => install_linux(&file, &r.tag),
        "macos" => install_macos(&file, &r.tag),
        other => Err(Error::Platform(format!(
            "no automated update for {other}; download from github.com/{REPO}/releases/tag/{}",
            r.tag
        ))),
    }
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
    let sums = curl(&["-sS", "-m", "30", "-L", &r.sums_url])?;
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
            "Editor {tag} is available. Download and install it now?\n\nEditor will close and restart with the new version."
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
    const MB_SYSTEMMODAL: u32 = 0x1000;
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

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
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
            let p = std::env::temp_dir().join(format!("editor-sha-test-{i}"));
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
        let p = std::env::temp_dir().join("editor-sha-test-chunk");
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
}
