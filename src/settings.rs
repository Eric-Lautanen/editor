//! Session persistence: theme, view mode, the open tabs and the window's size
//! and place survive app restarts in one tiny text file under the user config
//! dir. Written only when something changes, and replaced in a single step (see
//! [`save_in`]) so a crash mid-write cannot leave a half session behind; any
//! read error falls back to defaults.

use std::path::{Path, PathBuf};

/// Persisted session state. `mode` mirrors `app::Mode`'s discriminants
/// (0 = Preview, 1 = Edit, 2 = Split) to keep this module app-agnostic, and the
/// window is kept as bare numbers for the same reason — this file knows nothing
/// about `App`, the event types, or the window system.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub dark: bool,
    /// The user picked a theme manually (stop following the OS).
    pub manual: bool,
    pub mode: u8,
    pub files: Vec<PathBuf>,
    pub active: usize,
    /// Last window rect `(x, y, width, height)` in physical px **as it was
    /// before being maximized** — the size and place a restore should reopen
    /// at, which is not what a maximized window reports. `None` until the app
    /// has been told one (a first run, or a window that never reported).
    pub window: Option<(i32, i32, u32, u32)>,
    /// The window was maximized when the session ended, so the next one opens
    /// the same way (at `window`, then maximized).
    pub maximized: bool,
    /// Background update checks on launch (About box toggle). On by default;
    /// off means `--update` is still there but nothing pings GitHub silently.
    /// The consent + checksum gates around *installing* are separate from and
    /// stricter than this switch: it only controls whether the check runs.
    pub auto_update: bool,
    /// Recently opened files, most recent first, capped at [`RECENT_MAX`].
    /// Independent of `files` (the tabs this session reopens): a file can be
    /// recent long after it stopped being open.
    pub recent: Vec<PathBuf>,
}

/// How many recent files the session keeps.
pub const RECENT_MAX: usize = 10;

/// Move `path` to the front of the recent list, dropping the duplicate and
/// keeping [`RECENT_MAX`] entries.
pub fn note_recent(recent: &mut Vec<PathBuf>, path: &Path) {
    recent.retain(|p| p.as_path() != path);
    recent.insert(0, path.to_path_buf());
    recent.truncate(RECENT_MAX);
}

/// Per-user config directory (`%APPDATA%\\Editor`, `~/.config/editor`,
/// `~/Library/Application Support/Editor`), if the OS provides one.
pub fn config_dir() -> Option<PathBuf> {
    config_base()
}

/// The real per-user config directory.
#[cfg(not(test))]
fn config_base() -> Option<PathBuf> {
    let base = if let Some(a) = std::env::var_os("APPDATA") {
        Some(PathBuf::from(a).join("Editor"))
    } else if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(x).join("editor"))
    } else if let Some(h) = std::env::var_os("HOME") {
        if cfg!(target_os = "macos") {
            Some(
                PathBuf::from(h)
                    .join("Library")
                    .join("Application Support")
                    .join("Editor"),
            )
        } else {
            Some(PathBuf::from(h).join(".config").join("editor"))
        }
    } else {
        None
    }?;
    Some(base)
}

/// A throwaway directory, so the suite never touches the real one.
///
/// `App::open_path` persists the session, and the tests open temp files in a
/// dozen places, so without this redirect a plain `cargo test` run would
/// overwrite the developer's own theme, view mode and open tabs with whatever
/// the last test happened to be looking at.
#[cfg(test)]
fn config_base() -> Option<PathBuf> {
    Some(std::env::temp_dir().join("editor_test_config"))
}

fn file_in(dir: &Path) -> PathBuf {
    dir.join("session.txt")
}

/// The file a session is written to *before* it replaces the real one. Same
/// directory, so the rename that commits it is a same-volume move.
fn temp_in(dir: &Path) -> PathBuf {
    dir.join("session.tmp")
}

/// Render the settings as the tiny text file format.
fn render(s: &Settings) -> String {
    let mut out = format!(
        "theme {}\nmanual {}\nmode {}\nactive {}\nmaximized {}\nauto_update {}\n",
        if s.dark { "dark" } else { "light" },
        u8::from(s.manual),
        s.mode,
        s.active,
        u8::from(s.maximized),
        u8::from(s.auto_update)
    );
    if let Some((x, y, w, h)) = s.window {
        out.push_str(&format!("window {x} {y} {w} {h}\n"));
    }
    for f in &s.files {
        // Paths cannot contain newlines, so one `open <path>` per line is
        // unambiguous even with spaces in the name.
        out.push_str(&format!("open {}\n", f.display()));
    }
    for f in &s.recent {
        out.push_str(&format!("recent {}\n", f.display()));
    }
    out
}

fn parse(text: &str) -> Settings {
    let mut s = Settings::default();
    for line in text.lines() {
        let Some((key, val)) = line.split_once(' ') else {
            continue;
        };
        match key {
            "theme" => s.dark = val == "dark",
            "manual" => s.manual = val == "1",
            "mode" => s.mode = val.parse().unwrap_or(0),
            "active" => s.active = val.parse().unwrap_or(0),
            "maximized" => s.maximized = val == "1",
            "auto_update" => s.auto_update = val == "1",
            "window" => s.window = parse_window(val),
            // A path with an embedded NUL can never open (Win32 rejects it),
            // so such a line is dropped rather than carried forever. Builds
            // before the handoff fix wrote exactly that into the recent list.
            "open" if !val.contains('\0') => s.files.push(PathBuf::from(val)),
            "recent" if !val.contains('\0') => s.recent.push(PathBuf::from(val)),
            _ => {}
        }
    }
    s
}

/// `x y w h`, all four required: a half-written or hand-edited line is dropped
/// rather than applied as a partially-zeroed rect (which would place a window
/// at the origin with no size).
fn parse_window(val: &str) -> Option<(i32, i32, u32, u32)> {
    let mut it = val.split_whitespace();
    let x = it.next()?.parse().ok()?;
    let y = it.next()?.parse().ok()?;
    let w = it.next()?.parse().ok()?;
    let h = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((x, y, w, h))
}

/// Load the persisted session; defaults when missing or unreadable.
pub fn load() -> Settings {
    config_dir().map(|d| load_in(&d)).unwrap_or_default()
}

/// Read the session out of `dir`. Only the real file is read: a leftover temp
/// file (a write that a crash interrupted before the rename) is ignored, which
/// is the point of committing with a rename — the last complete session is what
/// the next launch sees.
fn load_in(dir: &Path) -> Settings {
    std::fs::read_to_string(file_in(dir))
        .map(|t| parse(&t))
        .unwrap_or_default()
}

/// Persist the session. Best effort: a failure keeps the app running.
pub fn save(s: &Settings) {
    let Some(dir) = config_dir() else {
        return;
    };
    save_in(&dir, s);
}

/// Write the session into `dir` in one step: the text goes to a temp file in
/// the same directory, is flushed to the disk, and only then replaces the
/// session file with a rename.
///
/// This matters most for the writes that happen *during* a session (the window
/// geometry the heartbeat persists): a power cut or a kill landing inside a
/// plain in-place write would leave a truncated file, and the next launch would
/// come up with no session at all. With the rename it either sees the whole new
/// session or the whole previous one.
///
/// The fallback is for the target that refuses to be replaced (a locked file, an
/// unusual filesystem, another instance winning the race for the single temp
/// name) — a session written in place is still better than none, and it is
/// complete by then, so the only thing given up is the atomicity of that step.
///
/// Every failure is swallowed: persisting must never take the app down.
fn save_in(dir: &Path, s: &Settings) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let path = file_in(dir);
    let tmp = temp_in(dir);
    let text = render(s);
    use std::io::Write;
    let written = std::fs::File::create(&tmp)
        .and_then(|mut f| {
            f.write_all(text.as_bytes())?;
            // A few hundred bytes, and the whole point: the new session is on
            // the disk before the old one is replaced.
            f.sync_all()
        })
        .is_ok();
    if !written || std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::write(&path, &text);
    }
    // Gone after a successful rename; removes the leftovers when it was not.
    let _ = std::fs::remove_file(&tmp);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Settings {
            dark: true,
            manual: true,
            mode: 2,
            files: vec![
                PathBuf::from("C:\\my docs\\a file.md"),
                PathBuf::from("/home/u/notes.txt"),
            ],
            active: 1,
            window: Some((-1920, 40, 1100, 800)),
            maximized: true,
            auto_update: false,
            recent: vec![PathBuf::from("/home/u/notes.txt")],
        };
        let text = render(&s);
        assert_eq!(parse(&text), s);
    }

    #[test]
    fn nul_tainted_paths_are_dropped_on_load() {
        // The single-instance handoff once delivered its path with the UTF-16
        // NUL terminator still attached, so those lines landed in the recent
        // list. They can never open; loading must discard them.
        let s = parse("open C:\\ok\\a.md\nopen C:\\bad\\b.md\0\nrecent C:\\bad\\b.md\0\nrecent C:\\ok\\c.md\n");
        assert_eq!(s.files, vec![PathBuf::from("C:\\ok\\a.md")]);
        assert_eq!(s.recent, vec![PathBuf::from("C:\\ok\\c.md")]);
    }

    #[test]
    fn recents_move_to_the_front_and_cap() {
        let mut recent: Vec<PathBuf> = (0..RECENT_MAX)
            .map(|i| PathBuf::from(format!("/old/{i}.md")))
            .collect();
        // Re-opening an existing entry moves it to the front, not a duplicate.
        note_recent(&mut recent, Path::new("/old/3.md"));
        assert_eq!(recent[0], PathBuf::from("/old/3.md"));
        assert_eq!(recent.len(), RECENT_MAX);
        // A new file pushes the oldest out.
        note_recent(&mut recent, Path::new("/new.md"));
        assert_eq!(recent.len(), RECENT_MAX);
        assert_eq!(recent[0], PathBuf::from("/new.md"));
        assert!(!recent.contains(&PathBuf::from("/old/9.md")));
        // And the list survives the file format.
        let s = Settings {
            recent: recent.clone(),
            ..Settings::default()
        };
        assert_eq!(parse(&render(&s)).recent, recent);
    }

    #[test]
    fn defaults_on_garbage() {
        let s = parse("theme maybe\nmode x\nactive y\nopen\n\njunk");
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn the_window_line_carries_the_whole_rect() {
        // Negative origins are ordinary: a monitor left of or above the
        // primary one, which is exactly the case a restore must not lose.
        let s = parse("window -1920 -40 1100 800\nmaximized 1\n");
        assert_eq!(s.window, Some((-1920, -40, 1100, 800)));
        assert!(s.maximized);
        assert_eq!(parse(&render(&s)).window, s.window);
    }

    #[test]
    fn a_partial_window_line_is_dropped() {
        // Apply only a complete rect: half a line would otherwise restore a
        // window at the origin with no size.
        for line in [
            "window 120 80 1100",
            "window 120 80",
            "window",
            "window a b c d",
            "window 120 80 1100 800 3",
        ] {
            assert_eq!(parse(line).window, None, "{line:?} must be dropped");
        }
        // A missing window line means "never seen one", not the origin.
        assert_eq!(parse("theme dark\n").window, None);
    }

    #[test]
    fn a_save_replaces_the_session_in_one_step() {
        // The file is written elsewhere and renamed over the old one, so the
        // session on disk is always a complete one, and the scratch file it was
        // staged in is gone afterwards.
        let dir = std::env::temp_dir().join("editor_settings_atomic");
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = Settings {
            dark: true,
            mode: 1,
            window: Some((260, 170, 900, 640)),
            ..Settings::default()
        };
        save_in(&dir, &s);
        assert_eq!(load_in(&dir), s);
        assert!(!temp_in(&dir).exists(), "no scratch file is left behind");
        // A later save updates the same file in place, minus the scratch.
        s.window = Some((300, 200, 880, 620));
        s.maximized = true;
        save_in(&dir, &s);
        assert_eq!(load_in(&dir), s);
        assert!(!temp_in(&dir).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_interrupted_before_the_rename_keeps_the_previous_session() {
        let dir = std::env::temp_dir().join("editor_settings_interrupted");
        let _ = std::fs::remove_dir_all(&dir);
        let s = Settings {
            dark: false,
            manual: false,
            mode: 2,
            files: vec![PathBuf::from("/home/u/notes.md")],
            active: 0,
            window: Some((120, 80, 1100, 800)),
            maximized: false,
            recent: Vec::new(),
            auto_update: true,
        };
        save_in(&dir, &s);
        // What a crash between the write and the rename leaves: a temp file with
        // the new text that was never committed.
        std::fs::write(temp_in(&dir), "window 999\ntheme dark\njunk").unwrap();
        assert_eq!(load_in(&dir), s, "the committed session is the one read");
        // And the next save cleans the leftover up.
        save_in(&dir, &s);
        assert!(!temp_in(&dir).exists());
        assert_eq!(load_in(&dir), s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_session_dir_that_cannot_be_replaced_does_not_take_the_app_down() {
        // Neither the rename nor the fallback can work when the session path is
        // a directory, which stands in for a locked file or an unwritable spot:
        // persisting is best effort, and reading falls back to defaults.
        let dir = std::env::temp_dir().join("editor_settings_unwritable");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(file_in(&dir)).unwrap();
        save_in(&dir, &Settings::default());
        assert_eq!(load_in(&dir), Settings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_then_load() {
        let dir = std::env::temp_dir().join("editor_settings_test");
        let _ = std::fs::remove_dir_all(&dir);
        let s = Settings {
            dark: true,
            manual: false,
            mode: 1,
            files: vec![dir.join("x.json")],
            active: 0,
            window: Some((10, 20, 900, 700)),
            maximized: false,
            recent: Vec::new(),
            auto_update: true,
        };
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(file_in(&dir), render(&s)).unwrap();
        assert_eq!(parse(&std::fs::read_to_string(file_in(&dir)).unwrap()), s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The suite must not write the real session file: `App::open_path`
    /// persists, so a test run would otherwise clobber the developer's theme
    /// and open tabs.
    #[test]
    fn tests_never_reach_the_real_config_dir() {
        let dir = config_dir().expect("a config dir under test");
        assert!(
            dir.starts_with(std::env::temp_dir()),
            "tests would write {dir:?}"
        );
    }
}
