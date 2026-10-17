//! HTTPS without a TLS implementation: hand the URL to whatever the OS
//! already ships, in order, and stop at the first rung that works.
//!
//! The updater needs three requests (release JSON, installer, `SHA256SUMS`)
//! and nothing else in the app touches the network. Bundling a TLS stack to
//! do that is the one dependency this project refuses on principle (see the
//! covenant in Cargo.toml), and implementing TLS by hand is worse, so the
//! fetch is delegated:
//!
//! | OS | order |
//! |---|---|
//! | Windows | `urlmon`'s `URLDownloadToFileW` (in-process WinINet) → `curl.exe` (System32 since 10 1803) |
//! | macOS | `curl` (at `/usr/bin/curl` on every install) → `python3` |
//! | other Unix | `curl` → `wget` (GNU, then busybox) → `python3` |
//!
//! Each rung is either part of the OS or absent, which is the point: a
//! fallback is only ever a different *existing* route to the network, never a
//! download of one. Enterprise machines with `curl.exe` blocked by AppLocker
//! and Alpine containers with no curl each land on a different rung, and none
//! of them on an error nobody can act on: when every rung fails the message
//! names each one and why.
//!
//! **Nothing on Windows spawns a script host any more.** The chain used to be
//! `curl` → PowerShell's `Invoke-WebRequest` → urlmon → `certutil -urlcache`,
//! and every rung after the first was a process creating malware-detection
//! noise on the user's machine: a text editor running
//! `powershell -ExecutionPolicy Bypass -Command …` twice a day, or
//! `certutil -urlcache` (a malware favourite that EDR blocks outright — this
//! machine refuses to start it at all, `ERROR_ACCESS_DENIED` at process
//! creation). A routine update check now runs entirely inside this process:
//! `urlmon` is one FFI call into a DLL that shipped with Windows, the OS's own
//! HTTP stack does the TLS, and `curl.exe` remains only as the fallback for an
//! image where WinINet is broken. Neither can be mistaken for a dropper, and
//! neither puts a child process on the desktop.
//!
//! Bare `Command` argv everywhere (never a shell string), single quotes
//! escaped for the one rung that is a shell, and [`trusted`] refuses any URL
//! that is not HTTPS on GitHub's release hosts — so a compromised or spoofed
//! API response cannot turn a fetch into a local file read or a plaintext
//! download.

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `https://api.github.com` answers 403 to a request with no User-Agent.
pub fn user_agent() -> String {
    format!("editor/{}", env!("CARGO_PKG_VERSION"))
}

/// Hosts a release download can legitimately come from: the API, the release
/// asset URLs it returns (`github.com/.../releases/download/...`), and the
/// object stores those redirect to.
const HOSTS: [&str; 5] = [
    "github.com",
    "api.github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github-releases.githubusercontent.com",
];

/// Is this a URL the updater may fetch? HTTPS on one of [`HOSTS`], or plain
/// HTTP to the loopback address (which is how the tests exercise the real
/// rungs against a server they started).
pub fn trusted(url: &str) -> bool {
    let (scheme, rest) = match url.split_once("://") {
        Some(p) => p,
        None => return false,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Strip credentials and port: `user@host:443`.
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, h)| h)
        .trim_start_matches('[')
        .split([']', ':'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match scheme.to_ascii_lowercase().as_str() {
        "https" => HOSTS.contains(&host.as_str()),
        "http" => matches!(host.as_str(), "127.0.0.1" | "::1" | "localhost"),
        _ => false,
    }
}

/// One way to obtain a URL into a file.
enum Rung {
    /// A helper program and its argv.
    Helper(Helper),
    /// `urlmon.dll`'s `URLDownloadToFileW`, in-process (Windows only).
    #[cfg(target_os = "windows")]
    UrlMon,
}

struct Helper {
    /// What to call it in an error message.
    name: &'static str,
    program: &'static str,
    args: Vec<String>,
}

impl Rung {
    fn name(&self) -> &'static str {
        match self {
            Rung::Helper(h) => h.name,
            #[cfg(target_os = "windows")]
            Rung::UrlMon => "urlmon",
        }
    }
}

/// The rungs to try, best first, for this OS. `out` is the file the rung must
/// leave the bytes in; `timeout` is a whole-request cap in seconds, for the
/// rungs that take one (the in-process ones bound themselves instead).
fn rungs(url: &str, out: &Path, timeout: u32) -> Vec<Rung> {
    let mut v: Vec<Rung> = helpers(url, out, timeout)
        .into_iter()
        .map(Rung::Helper)
        .collect();
    #[cfg(target_os = "windows")]
    // First, and usually only: it is not a child process, so a check leaves no
    // spawned executables behind for endpoint protection to comment on.
    v.insert(0, Rung::UrlMon);
    v
}

fn helpers(url: &str, out: &Path, timeout: u32) -> Vec<Helper> {
    let out = out.display().to_string();
    let ua = user_agent();
    let mut v = Vec::with_capacity(4);
    if cfg!(target_os = "windows") {
        // `curl.exe` by bare name resolves through System32 before PATH, so
        // this is the system curl and not a git-for-windows / msys2 copy.
        v.push(Helper {
            name: "curl",
            program: "curl.exe",
            args: vec![
                "-sS".into(),
                "-m".into(),
                timeout.to_string(),
                "-L".into(),
                "-H".into(),
                format!("User-Agent: {ua}"),
                "-o".into(),
                out.clone(),
                url.into(),
            ],
        });
        // The only Windows helper, and the fallback for an image where WinINet
        // is unavailable. Deliberately not PowerShell (a script host asked to
        // download and execute is the shape EDR is built to interrupt) and not
        // certutil (blocked outright on machines like this one).
    } else {
        v.push(Helper {
            name: "curl",
            program: "curl",
            args: vec![
                "-sS".into(),
                "-m".into(),
                timeout.to_string(),
                "-L".into(),
                "-H".into(),
                format!("User-Agent: {ua}"),
                "-o".into(),
                out.clone(),
                url.into(),
            ],
        });
        // GNU wget takes the header; busybox's does not, hence the second
        // spelling (busybox images are the ones with no curl).
        v.push(Helper {
            name: "wget",
            program: "wget",
            args: vec![
                "-q".into(),
                "-T".into(),
                timeout.to_string(),
                "-O".into(),
                out.clone(),
                format!("--header=User-Agent: {ua}"),
                url.into(),
            ],
        });
        v.push(Helper {
            name: "busybox wget",
            program: "busybox",
            args: vec![
                "wget".into(),
                "-q".into(),
                "-O".into(),
                out.clone(),
                url.into(),
            ],
        });
        // Python verifies certificates through the system store, so this is
        // still the OS's TLS, not ours.
        v.push(Helper {
            name: "python3",
            program: "python3",
            args: vec![
                "-c".into(),
                "import sys,urllib.request;req=urllib.request.Request(sys.argv[1],\
                 headers={'User-Agent':sys.argv[3]});\
                 open(sys.argv[2],'wb').write(\
                 urllib.request.urlopen(req,timeout=float(sys.argv[4])).read())"
                    .into(),
                url.into(),
                out,
                ua,
                timeout.to_string(),
            ],
        });
    }
    v
}

/// Obtain `url` into `out` via one rung. Success means a non-empty file is
/// there afterwards — `certutil` in particular reports some failures in its
/// output while exiting 0, and an empty download must never be mistaken for an
/// install asset.
fn run(rung: &Rung, url: &str, out: &Path) -> Result<()> {
    let _ = std::fs::remove_file(out);
    match rung {
        Rung::Helper(h) => run_helper(h, out)?,
        #[cfg(target_os = "windows")]
        Rung::UrlMon => urlmon_download(url, out)?,
    }
    match std::fs::metadata(out) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => Err(Error::Platform(format!("{} wrote no data", rung.name()))),
    }
}

fn run_helper(h: &Helper, _out: &Path) -> Result<()> {
    let mut cmd = Command::new(h.program);
    cmd.args(&h.args);
    // A GUI-subsystem parent has no console for the child to inherit, so a
    // console helper would get one allocated: a black window flashing for the
    // length of the download. Other platforms run it as an ordinary child.
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let got = cmd.output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Error::Platform(format!("{} is not installed", h.name)),
        // Security software refusing to start the process is the same outcome
        // as it not being there: the next rung runs. (Observed for certutil.)
        std::io::ErrorKind::PermissionDenied => {
            Error::Platform(format!("{} is blocked here ({e})", h.name))
        }
        _ => Error::Platform(format!("{} could not start: {e}", h.name)),
    })?;
    if !got.status.success() {
        // Helpers disagree about which stream carries the reason; take
        // whichever said something.
        let err = String::from_utf8_lossy(&got.stderr).trim().to_string();
        let err = if err.is_empty() {
            String::from_utf8_lossy(&got.stdout).trim().to_string()
        } else {
            err
        };
        return Err(Error::Platform(format!(
            "{} exited {}: {err}",
            h.name, got.status
        )));
    }
    Ok(())
}

/// `urlmon!URLDownloadToFileW`: the OS's HTTP stack (WinINet) writes the file
/// in one call. No helper to install, none to block, and the TLS is
/// Microsoft's. Returns an HRESULT, negative on failure.
#[cfg(target_os = "windows")]
fn urlmon_download(url: &str, out: &Path) -> Result<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "urlmon")]
    extern "system" {
        fn URLDownloadToFileW(
            caller: *mut c_void,
            url: *const u16,
            file: *const u16,
            reserved: u32,
            progress: *mut c_void,
        ) -> i32;
    }
    let wide =
        |s: &std::ffi::OsStr| -> Vec<u16> { s.encode_wide().chain(std::iter::once(0)).collect() };
    let url = wide(std::ffi::OsStr::new(url));
    let file = wide(out.as_os_str());
    let hr = unsafe {
        URLDownloadToFileW(
            std::ptr::null_mut(),
            url.as_ptr(),
            file.as_ptr(),
            0,
            std::ptr::null_mut(),
        )
    };
    if hr < 0 {
        return Err(Error::Platform(format!(
            "urlmon could not download (HRESULT 0x{:08x})",
            hr as u32
        )));
    }
    Ok(())
}

/// Try each rung in order and return after the first success. On total
/// failure the error lists every rung, so a bug report says whether the
/// machine has no route at all or all of them were blocked.
fn fetch(url: &str, out: &Path, timeout: u32) -> Result<()> {
    if !trusted(url) {
        return Err(Error::Platform(format!(
            "refusing to download {url}: not an https GitHub release URL"
        )));
    }
    let mut failures = Vec::new();
    for rung in rungs(url, out, timeout) {
        match run(&rung, url, out) {
            Ok(()) => return Ok(()),
            Err(e) => failures.push(e.to_string()),
        }
    }
    let _ = std::fs::remove_file(out);
    Err(Error::Platform(format!(
        "could not download {url} ({}); download it by hand from github.com/Eric-Lautanen/editor/releases",
        failures.join("; ")
    )))
}

/// Fetch `url` into memory as text. Used for the release JSON and its
/// `SHA256SUMS`, both small; the scratch file is removed on every path,
/// success or not.
pub fn get_text(url: &str, timeout: u32) -> Result<String> {
    let path = scratch_path();
    let res = fetch(url, &path, timeout).and_then(|()| {
        std::fs::read(&path)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .map_err(|e| Error::Platform(format!("downloaded {url} but could not read it: {e}")))
    });
    let _ = std::fs::remove_file(&path);
    res
}

/// Fetch `url` into `path` (the install asset: megabytes, so it goes straight
/// to disk in whatever rung runs and is never held whole here).
pub fn get_file(url: &str, path: &Path, timeout: u32) -> Result<()> {
    fetch(url, path, timeout)
}

/// A unique scratch name in the temp dir: pid + counter, so two checks in one
/// process (or two processes) never collide.
fn scratch_path() -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "editor-fetch-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn trusted_allows_github_and_loopback_only() {
        assert!(trusted(
            "https://api.github.com/repos/Eric-Lautanen/editor/releases/latest"
        ));
        assert!(trusted(
            "https://github.com/Eric-Lautanen/editor/releases/download/v0.2.1/editor-v0.2.1-windows-x86_64.msi"
        ));
        assert!(trusted(
            "https://objects.githubusercontent.com/github-production-release-asset-2e65be/x?X-Amz-Signature=y"
        ));
        assert!(trusted("http://127.0.0.1:8080/x"));
        assert!(trusted("http://localhost/x"));
        // Not GitHub, not https, not a URL at all.
        assert!(!trusted("https://evil.example/x"));
        assert!(!trusted("https://github.com.evil.example/x"));
        assert!(!trusted("http://github.com/x"));
        assert!(!trusted("file:///C:/Windows/win.ini"));
        assert!(!trusted("https://github.com@evil.example/x"));
        assert!(!trusted("C:\\Windows\\win.ini"));
        assert!(!trusted(""));
    }

    #[test]
    fn the_chain_carries_the_url_and_never_runs_a_script_host() {
        let out = Path::new("/tmp/out.bin");
        let url = "https://github.com/x/y.msi";
        let names: Vec<_> = rungs(url, out, 30).iter().map(|r| r.name()).collect();
        if cfg!(target_os = "windows") {
            // In-process first: a routine check must not create a child
            // process for Defender to look at. `powershell` and `certutil`
            // used to sit here and were exactly the noise the user reported.
            assert_eq!(names, ["urlmon", "curl"]);
        } else {
            assert_eq!(names, ["curl", "wget", "busybox wget", "python3"]);
        }
        // Nothing in the chain is a shell: every helper gets the URL as its
        // own argv entry, never interpolated into a command string.
        for helper in helpers(url, out, 30) {
            assert!(
                helper.args.iter().any(|s| s == url),
                "{} lost the url",
                helper.name
            );
            for banned in ["powershell", "certutil", "cmd", "wscript", "cscript"] {
                assert_ne!(helper.program, format!("{banned}.exe"), "{banned} is back");
            }
        }
    }

    #[test]
    fn untrusted_urls_never_reach_a_rung() {
        let e = get_text("https://evil.example/x", 5)
            .unwrap_err()
            .to_string();
        assert!(e.contains("refusing to download"), "{e}");
    }

    /// Serves `body` over up to `n` sequential HTTP connections. The thread is
    /// detached on purpose: a rung that never connects (blocked helper) would
    /// otherwise leave a `join` waiting on an `accept` that never comes. It
    /// dies with the test binary like any other stray thread.
    fn serve(body: &'static str, n: usize) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..n {
                let Ok((mut s, _)) = l.accept() else { break };
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(body.as_bytes());
            }
        });
        format!("http://{addr}/asset")
    }

    /// The whole point of the module: on this machine, the real rungs download
    /// over the real network stack. Whichever rung is available must work —
    /// the chain is only as good as its first working link.
    #[test]
    fn an_available_rung_actually_downloads() {
        let url = serve("hello from loopback\n", 1);
        let got = get_text(&url, 10);
        assert_eq!(got.unwrap(), "hello from loopback\n");
    }

    /// Live, `#[ignore]`d: every rung against the real API over real TLS, so
    /// a fallback is never *assumed* to work because its argv looks right.
    /// `cargo test net::tests::live -- --ignored --nocapture` prints one line
    /// per rung. Not in the default run: it needs the network, and GitHub
    /// rate-limits by IP.
    #[test]
    #[ignore]
    fn live_rungs_reach_github() {
        let out = std::env::temp_dir().join("editor_fetch_live_test");
        for rung in rungs("https://api.github.com/", &out, 15) {
            let got = run(&rung, "https://api.github.com/", &out);
            eprintln!(
                "live {}: {}",
                rung.name(),
                match &got {
                    Ok(()) => "ok".to_string(),
                    Err(e) => e.to_string(),
                }
            );
            if got.is_ok() {
                // `ok` only means a file was written: an HTTP error page is a
                // file too. api.github.com answers JSON, so anything else is
                // this rung quietly returning a 4xx body (WinINet does that
                // for urlmon, which cannot see the status) and must not count.
                let body = std::fs::read(&out).unwrap_or_default();
                assert_eq!(
                    body.first(),
                    Some(&b'{'),
                    "{} fetched something that is not the API's JSON: {}",
                    rung.name(),
                    String::from_utf8_lossy(&body[..body.len().min(80)])
                );
            }
            let _ = std::fs::remove_file(&out);
        }
    }

    /// Each rung, run alone, so a broken second or third rung (bad flags,
    /// wrong quoting, an FFI signature that writes nothing) shows up here
    /// instead of on a user's machine that has no curl. A rung whose program
    /// is missing — or blocked by security software, which is how certutil
    /// fails on this machine — is skipped, not failed; at least one must work.
    #[test]
    fn every_available_rung_works_on_its_own() {
        let out = std::env::temp_dir().join("editor_fetch_rung_test");
        let names: Vec<&str> = rungs("http://127.0.0.1:1/asset", &out, 5)
            .iter()
            .map(|r| r.name())
            .collect();
        let url = serve("rung ok", names.len());
        let mut worked = 0;
        for rung in rungs(&url, &out, 5) {
            let outcome = run(&rung, &url, &out);
            eprintln!(
                "rung {}: {:?}",
                rung.name(),
                outcome.as_ref().err().map(|e| e.to_string())
            );
            match outcome {
                Ok(()) => {
                    assert_eq!(
                        std::fs::read_to_string(&out).unwrap(),
                        "rung ok",
                        "{} produced the wrong bytes",
                        rung.name()
                    );
                    worked += 1;
                }
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("is not installed") || msg.contains("is blocked here"),
                        "{} failed: {msg}",
                        rung.name()
                    );
                }
            }
            let _ = std::fs::remove_file(&out);
        }
        assert!(worked >= 1, "no fetch rung worked at all");
    }
}
