//! Every key binding the app has, in one place.
//!
//! Three things read this table and nothing keeps its own copy: the F1 overlay
//! ([`super::help`]) paints it, the README's shortcut table is pasted from
//! [`markdown_table`] (a test in this file refuses a stale paste), and
//! `every_probed_chord_changes_something` fires each chord at a real [`App`]
//! to prove the dispatcher in `app::on_key` still does what the row claims.
//!
//! So a binding that works but is missing here is undocumented, and a row
//! whose chord stops working fails `cargo test` instead of quietly lying.
//!
//! The probe test lives in `app::tests` rather than here: it reads the live
//! window state (`zoom`, `help_open`, the find bar), which is private to
//! `app`.

use super::event::{Key, Modifiers};

const NONE: Modifiers = Modifiers {
    ctrl: false,
    alt: false,
    shift: false,
};
const CTRL: Modifiers = Modifiers {
    ctrl: true,
    alt: false,
    shift: false,
};

/// One row of the legend: a chord (or a gesture, written as prose) and what it
/// does.
pub struct Binding {
    /// What the user presses. Gestures that are not keys ("drag & drop",
    /// "click a link") are prose here and carry no probe.
    pub keys: &'static str,
    /// What it does, one line, lowercase like the rest of the legend.
    pub what: &'static str,
    /// The key event a test fires to exercise this row, when a fresh window
    /// can exercise it. `None` for gestures, and for chords that need context
    /// to do anything — a selection, a clipboard, an undo history, a dirty
    /// buffer, or a file dialog — since a probe there could only prove the
    /// no-op path.
    ///
    /// Read by the test suite and nothing else, but it lives here on purpose:
    /// a probe kept next to the row it exercises is a probe that gets updated
    /// when the row is, and a probe in a separate test file is one that
    /// quietly stops matching.
    #[cfg_attr(not(test), allow(dead_code))]
    pub probe: Option<(Key, Modifiers)>,
}

impl Binding {
    const fn key(keys: &'static str, what: &'static str, probe: Option<(Key, Modifiers)>) -> Self {
        Binding { keys, what, probe }
    }

    /// A gesture or a context-dependent chord: documented, not probed.
    const fn prose(keys: &'static str, what: &'static str) -> Self {
        Binding::key(keys, what, None)
    }
}

const fn ctrl(c: char) -> Option<(Key, Modifiers)> {
    Some((Key::Char(c), CTRL))
}

const fn plain(key: Key) -> Option<(Key, Modifiers)> {
    Some((key, NONE))
}

pub struct Section {
    pub title: &'static str,
    pub items: &'static [Binding],
}

/// Sections in paint order. The overlay numbers its rows by position in this
/// array, so reordering is free but inserting a section is what the help
/// layout tests count.
pub const SECTIONS: &[Section] = &[
    Section {
        title: "Modes",
        items: &[Binding::key(
            "Ctrl+E",
            "cycle Preview → Edit → Split",
            ctrl('e'),
        )],
    },
    Section {
        title: "Files",
        items: &[
            Binding::key("Ctrl+O", "open a file in a new tab", None),
            Binding::prose(
                "drag & drop",
                "drop files onto the window to open them (Windows, macOS)",
            ),
            Binding::key("Ctrl+Tab", "next tab (Shift = previous)", plain(Key::Tab)),
            Binding::key("Ctrl+R", "reload the active file from disk", ctrl('r')),
            Binding::key("Ctrl+W", "close tab (press twice if unsaved)", ctrl('w')),
            Binding::key("Ctrl+S", "save", None),
        ],
    },
    Section {
        title: "Editing  (Edit / Split)",
        items: &[
            Binding::prose("type", "edit the document"),
            Binding::prose("arrows", "move the caret (Shift = select)"),
            Binding::prose("mouse drag", "select text"),
            Binding::key("Ctrl+A", "select all", ctrl('a')),
            Binding::prose("Ctrl+C / X / V", "copy / cut / paste"),
            Binding::prose("Ctrl+Z / Y", "undo / redo"),
            Binding::key("Tab", "insert four spaces", plain(Key::Tab)),
        ],
    },
    Section {
        title: "View",
        items: &[
            Binding::prose("wheel, arrows", "scroll"),
            Binding::prose(
                "double-click",
                "select a word in the preview; a third click takes the row",
            ),
            Binding::key("Ctrl+F", "find bar (Enter/F3 next, Shift back)", ctrl('f')),
            // One row, three chords: the '+' variant is the probed one, since
            // a reset is a no-op on a fresh window.
            Binding::key("Ctrl + / - / 0", "zoom in / out / reset", ctrl('=')),
            Binding::key("Ctrl+T", "light / dark theme", ctrl('t')),
            Binding::prose("click [+]", "fold / unfold JSON nodes"),
            Binding::prose(
                "hover a link",
                "show its address in the status bar; click to open it",
            ),
        ],
    },
    Section {
        title: "App",
        items: &[
            Binding::key("F1", "this help", plain(Key::F(1))),
            Binding::key("F2", "about editor", plain(Key::F(2))),
            Binding::prose("right-click", "context menu"),
            Binding::key("Ctrl+Q", "quit", ctrl('q')),
        ],
    },
];

/// Marker lines around the generated table in `README.md`.
#[cfg(test)]
pub const README_START: &str = "<!-- keymap:start -->";
#[cfg(test)]
pub const README_END: &str = "<!-- keymap:end -->";

/// The README table, as the text that sits between the markers above.
#[cfg(test)]
pub fn markdown_table() -> String {
    let mut out = String::from("| Keys | Action |\n|---|---|\n");
    for section in SECTIONS {
        for item in section.items {
            out.push_str(&format!("| `{}` | {} |\n", item.keys, item.what));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The README table is a paste of [`markdown_table`], between the markers.
    #[test]
    fn readme_table_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
        let readme = std::fs::read_to_string(&path).expect("README.md");
        let start = readme
            .find(README_START)
            .expect("README lacks the keymap start marker");
        let rest = &readme[start + README_START.len()..];
        let end = rest
            .find(README_END)
            .expect("README lacks the keymap end marker");
        // A Windows checkout is CRLF; the table is generated with bare
        // newlines, and that difference is not drift.
        let pasted = rest[..end].replace("\r\n", "\n");
        let pasted = pasted.trim_matches('\n');
        assert_eq!(
            pasted,
            markdown_table().trim_matches('\n'),
            "README's shortcut table is stale; it is a paste of keymap::markdown_table()"
        );
    }

    /// Same keys doing two different things is how a legend starts lying.
    #[test]
    fn no_chord_is_documented_twice() {
        let mut seen = std::collections::BTreeSet::new();
        for section in SECTIONS {
            assert!(!section.items.is_empty(), "{} is empty", section.title);
            for item in section.items {
                assert!(
                    seen.insert(item.keys),
                    "\"{}\" is documented twice",
                    item.keys
                );
            }
        }
    }

    /// A gesture cannot be fired as one key event, so it must not claim a
    /// probe: the test would fail on the no-op path and the table would get
    /// blamed for a key that is fine.
    #[test]
    fn gestures_are_not_probed() {
        for section in SECTIONS {
            for item in section.items {
                let gesture = item.keys.starts_with("click")
                    || item.keys.starts_with("drag")
                    || item.keys.starts_with("right-click")
                    || item.keys.starts_with("wheel")
                    || item.keys.starts_with("mouse")
                    || item.keys == "type";
                if gesture {
                    assert!(
                        item.probe.is_none(),
                        "\"{}\" is a gesture but claims a probe",
                        item.keys
                    );
                }
            }
        }
    }
}
