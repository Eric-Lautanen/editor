//! Session persistence: theme, view mode and the open tabs survive app
//! restarts in one tiny text file under the user config dir. Written only
//! when something changes; any read error falls back to defaults.

use std::path::{Path, PathBuf};

/// Persisted session state. `mode` mirrors `app::Mode`'s discriminants
/// (0 = Preview, 1 = Edit, 2 = Split) to keep this module app-agnostic.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub dark: bool,
    /// The user picked a theme manually (stop following the OS).
    pub manual: bool,
    pub mode: u8,
    pub files: Vec<PathBuf>,
    pub active: usize,
}

/// Per-user config directory (`%APPDATA%\\Readitor`, `~/.config/readitor`,
/// `~/Library/Application Support/Readitor`), if the OS provides one.
pub fn config_dir() -> Option<PathBuf> {
    config_base()
}

/// The real per-user config directory.
#[cfg(not(test))]
fn config_base() -> Option<PathBuf> {
    let base = if let Some(a) = std::env::var_os("APPDATA") {
        Some(PathBuf::from(a).join("Readitor"))
    } else if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(x).join("readitor"))
    } else if let Some(h) = std::env::var_os("HOME") {
        if cfg!(target_os = "macos") {
            Some(
                PathBuf::from(h)
                    .join("Library")
                    .join("Application Support")
                    .join("Readitor"),
            )
        } else {
            Some(PathBuf::from(h).join(".config").join("readitor"))
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
    Some(std::env::temp_dir().join("readitor_test_config"))
}

fn file_in(dir: &Path) -> PathBuf {
    dir.join("session.txt")
}

/// Render the settings as the tiny text file format.
fn render(s: &Settings) -> String {
    let mut out = format!(
        "theme {}\nmanual {}\nmode {}\nactive {}\n",
        if s.dark { "dark" } else { "light" },
        u8::from(s.manual),
        s.mode,
        s.active
    );
    for f in &s.files {
        // Paths cannot contain newlines, so one `open <path>` per line is
        // unambiguous even with spaces in the name.
        out.push_str(&format!("open {}\n", f.display()));
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
            "open" => s.files.push(PathBuf::from(val)),
            _ => {}
        }
    }
    s
}

/// Load the persisted session; defaults when missing or unreadable.
pub fn load() -> Settings {
    config_dir()
        .and_then(|d| std::fs::read_to_string(file_in(&d)).ok())
        .map(|t| parse(&t))
        .unwrap_or_default()
}

/// Persist the session. Best effort: a failure keeps the app running.
pub fn save(s: &Settings) {
    let Some(dir) = config_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(file_in(&dir), render(s));
    }
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
        };
        let text = render(&s);
        assert_eq!(parse(&text), s);
    }

    #[test]
    fn defaults_on_garbage() {
        let s = parse("theme maybe\nmode x\nactive y\nopen\n\njunk");
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn save_then_load() {
        let dir = std::env::temp_dir().join("readitor_settings_test");
        let _ = std::fs::remove_dir_all(&dir);
        let s = Settings {
            dark: true,
            manual: false,
            mode: 1,
            files: vec![dir.join("x.json")],
            active: 0,
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
