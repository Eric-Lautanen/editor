//! Test-only temp directory resolution.
//!
//! The suites write their fixtures through `std::env::temp_dir()`, and on a
//! machine where that directory is not writable from a fresh process (a
//! sanitizer, a locked-down build agent, a hung cleanup tool) every test that
//! does so fails with `PermissionDenied` — which reads as a code regression
//! when it is an environment one. This module answers "a writable scratch
//! directory" once per process: the OS temp dir when a probe file lands
//! there, the build's own `target/.tmp` otherwise (git-ignored by
//! construction, already writable — cargo put the binary there).
//!
//! Shipped code keeps `std::env::temp_dir()`: only the tests get the
//! fallback, so `--mem-report`/`--pretty`-class paths still use the real
//! temp dir the OS gives them.

use std::path::PathBuf;
use std::sync::OnceLock;

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// A scratch directory the tests can write into. Determinstic per process;
/// the first call probes and every later call reuses the answer.
pub fn temp_dir() -> PathBuf {
    DIR.get_or_init(decide).clone()
}

fn decide() -> PathBuf {
    let os = std::env::temp_dir();
    if writable(&os) {
        return os;
    }
    // Fall back beside the build output. `create_dir_all` on the one the
    // harness already made is a no-op; if *that* fails too there is no
    // writable home left — hand back the build dir and let the test's own
    // write report the error it would have reported anyway.
    let fallback = std::env::current_dir()
        .map(|d| d.join("target").join(".tmp"))
        .unwrap_or_else(|_| PathBuf::from("target.tmp"));
    let _ = std::fs::create_dir_all(&fallback);
    fallback
}

/// One create-write-read-delete cycle: a directory that answers `metadata`
/// nicely can still refuse `CreateFileW` (the failure this module exists
/// for), so only a real round trip counts.
fn writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(format!("editor-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_is_writable_when_anything_is() {
        let dir = temp_dir();
        let probe = dir.join(format!("editor-probe-{}", std::process::id()));
        std::fs::write(&probe, b"x").expect("the resolved dir takes a write");
        std::fs::remove_file(&probe).unwrap();
    }

    #[test]
    fn it_is_stable_across_calls() {
        assert_eq!(temp_dir(), temp_dir());
    }
}
