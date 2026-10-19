//! Crash reports.
//!
//! A release build has nowhere to put a panic: the binary is stripped, the
//! Windows build is GUI-subsystem (no console to print to), and
//! `panic = "abort"` kills the process on the spot. The window simply
//! disappears, and whatever the user was doing is gone with no trace of why.
//!
//! So the hook installed by [`install`] writes one small text file per crash
//! into the config dir, next to the session file. The next launch notices the
//! newest one and names it in the status bar; the user can paste it into a bug
//! report, and it carries everything needed to start: version, OS, the panic
//! message, the source location, and a backtrace when `RUST_BACKTRACE` is set
//! (worth setting in a dev build — a release binary is stripped, so its frames
//! have no names, but the message and location still pin the site).
//!
//! Nothing here may itself panic: a hook that panics aborts immediately, which
//! would lose the very report it was writing.

use std::path::{Path, PathBuf};

/// Report file names: `crash-<epoch seconds>.txt`. The epoch sorts the
/// directory by age and names the file uniquely without a clock API.
const PREFIX: &str = "crash-";
const SUFFIX: &str = ".txt";
/// How many reports to keep. One crash is a bug; ten are a pattern, and a
/// rogue crash loop should not fill the disk.
const KEEP: usize = 10;

/// Install the reporting panic hook. Called once, first thing in `main`, so it
/// is in place before the app builds anything that can panic.
pub fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Under `panic = "abort"` this cannot unwind, but in a dev build it
        // stops a failure inside the report from replacing the original panic
        // with its own.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let location = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
            let text = report_text(
                env!("CARGO_PKG_VERSION"),
                &format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH),
                &info.to_string(),
                location.as_deref(),
                backtrace(),
                stamp(now_secs()),
            );
            if let Some(dir) = dir() {
                write_report(&dir, now_secs(), &text);
            }
        }));
        // Keep the default behaviour (the message on stderr) for dev runs.
        previous(info);
    }));
}

/// A backtrace, but only when it was asked for: capturing one is a stack walk
/// with symbol lookups, which is wasted work in the stripped build that ships.
fn backtrace() -> Option<String> {
    std::env::var_os("RUST_BACKTRACE")
        .is_some_and(|v| v != "0")
        .then(|| std::backtrace::Backtrace::force_capture().to_string())
}

/// The whole report, as bytes to write. Pure so the format is testable.
pub fn report_text(
    version: &str,
    os: &str,
    message: &str,
    location: Option<&str>,
    backtrace: Option<String>,
    when: String,
) -> String {
    let mut out = format!(
        "editor {version} crashed\nwhen: {when}\nos: {os}\npanic: {}\n",
        message.trim()
    );
    match location {
        Some(l) => out.push_str(&format!("at: {l}\n")),
        None => out.push_str("at: (unknown)\n"),
    }
    match backtrace {
        Some(b) => out.push_str(&format!("backtrace:\n{b}\n")),
        None => out.push_str("backtrace: (not captured — set RUST_BACKTRACE=1 and reproduce)\n"),
    }
    out.push_str(
        "\nThis file is written by the panic hook; the next launch names it in the\n\
         status bar. Delete it once it has been reported.\n",
    );
    out
}

/// Where reports live: the same per-user config dir as the session file.
pub fn dir() -> Option<PathBuf> {
    crate::settings::config_dir()
}

/// Write one report, pruning the oldest. `None` when the directory cannot be
/// created or written — a crash report is best effort, never a second failure.
pub fn write_report(dir: &Path, now_secs: u64, text: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!("{PREFIX}{now_secs}{SUFFIX}"));
    std::fs::write(&path, text).ok()?;
    prune(dir);
    Some(path)
}

/// The newest report in `dir`, if any (what the next launch tells the user
/// about).
pub fn latest_in(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_report(p))
        .max_by_key(|p| epoch_of(p).unwrap_or(0))
}

/// The newest report in the config dir.
pub fn latest() -> Option<PathBuf> {
    latest_in(&dir()?)
}

/// Keep the newest [`KEEP`] reports (and drop everything older). Deleting is
/// best effort: a file another instance is writing is simply left alone.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut reports: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_report(p))
        .collect();
    if reports.len() <= KEEP {
        return;
    }
    reports.sort_by_key(|p| epoch_of(p).unwrap_or(0));
    for old in &reports[..reports.len() - KEEP] {
        let _ = std::fs::remove_file(old);
    }
}

fn is_report(p: &Path) -> bool {
    epoch_of(p).is_some()
}

/// The epoch seconds encoded in a report's name, if it is one of ours.
fn epoch_of(p: &Path) -> Option<u64> {
    let name = p.file_name()?.to_str()?;
    let digits = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    digits.parse().ok()
}

/// Seconds since the Unix epoch. A clock before 1970 is not worth handling;
/// `0` keeps the name sorted and parseable.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `2026-10-17T00:12:34Z` from epoch seconds — a readable timestamp without a
/// date library. Days are converted with Howard Hinnant's `civil_from_days`
/// (the same one C++20's `year_month_day` uses), which is exact over the whole
/// range we care about and needs no leap-second table.
pub fn stamp(epoch_secs: u64) -> String {
    let days = (epoch_secs / 86_400) as i64;
    let rem = epoch_secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Days since 1970-01-01 → `(year, month, day)`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Shift the epoch to 0000-03-01 so leap days land at the end of the year.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March = 0
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = crate::testing::temp_dir().join(format!("editor_test_crash_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn epoch_stamps_are_readable_utc() {
        assert_eq!(stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(stamp(1_700_000_000), "2023-11-14T22:13:20Z");
        // A leap day, and the last second of a leap year.
        assert_eq!(stamp(1_582_934_400), "2020-02-29T00:00:00Z");
        assert_eq!(stamp(1_609_459_199), "2020-12-31T23:59:59Z");
    }

    #[test]
    fn the_report_carries_everything_a_bug_report_needs() {
        let text = report_text(
            "9.9.9",
            "linux (x86_64)",
            "index out of bounds: the len is 3 but the index is 9",
            Some("src/app.rs:1234:9"),
            None,
            stamp(1_700_000_000),
        );
        assert!(text.contains("editor 9.9.9 crashed"));
        assert!(text.contains("2023-11-14T22:13:20Z"));
        assert!(text.contains("linux (x86_64)"));
        assert!(text.contains("index out of bounds"));
        assert!(text.contains("src/app.rs:1234:9"));
        assert!(text.contains("RUST_BACKTRACE=1"));
        // A captured backtrace replaces the hint rather than both appearing.
        let with = report_text(
            "1",
            "os",
            "boom",
            None,
            Some("frame 1".into()),
            String::new(),
        );
        assert!(with.contains("backtrace:\nframe 1"));
        assert!(!with.contains("RUST_BACKTRACE=1"));
        assert!(with.contains("at: (unknown)"));
    }

    #[test]
    fn reports_are_written_under_a_sortable_name() {
        let dir = tmp("write");
        let path = write_report(&dir, 1_700_000_000, "boom").expect("report written");
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "crash-1700000000.txt"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "boom");
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_newest_report_is_the_one_surfaced() {
        let dir = tmp("latest");
        std::fs::create_dir_all(&dir).unwrap();
        // Out-of-order names, plus noise the app must ignore.
        write_report(&dir, 100, "old").unwrap();
        write_report(&dir, 300, "new").unwrap();
        write_report(&dir, 200, "middle").unwrap();
        std::fs::write(dir.join("session.txt"), "not a report").unwrap();
        std::fs::write(dir.join("crash-nope.txt"), "no epoch").unwrap();
        let newest = latest_in(&dir).expect("a report");
        assert_eq!(epoch_of(&newest), Some(300));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_reports_do_not_pile_up() {
        let dir = tmp("prune");
        for i in 0..(KEEP + 5) {
            write_report(&dir, 1_000 + i as u64, "boom").unwrap();
        }
        let left = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(left, KEEP, "a crash loop must not fill the disk");
        // The survivors are the newest ones.
        assert_eq!(
            epoch_of(&latest_in(&dir).unwrap()),
            Some(1_000 + KEEP as u64 + 4)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_or_missing_directory_reports_nothing() {
        assert!(latest_in(&tmp("missing")).is_none());
    }

    /// The hook end to end: install it, panic for real, and read what landed.
    ///
    /// This is the only test that touches the process-wide hook, and it leaves
    /// the default reporting in place (the hook chain calls the previous one),
    /// so a genuine failure elsewhere still prints as usual. The file is
    /// removed afterwards — the config dir is shared with the session tests.
    #[test]
    fn the_hook_writes_a_report_for_a_real_panic() {
        let dir = dir().expect("test config dir");
        install();
        let before = latest_in(&dir);
        let _ = std::panic::catch_unwind(|| panic!("boom for the crash-report test"));
        let after = latest_in(&dir).expect("the hook wrote a report");
        assert_ne!(after, before.unwrap_or_default(), "no new report appeared");
        let text = std::fs::read_to_string(&after).expect("readable report");
        assert!(text.contains("boom for the crash-report test"), "{text}");
        assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
        // The location carries the platform's own path separators.
        assert!(
            text.lines()
                .any(|l| l.starts_with("at: ") && l.contains("crash.rs")),
            "no source location: {text}"
        );
        let _ = std::fs::remove_file(&after);
    }
}
