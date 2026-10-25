use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Markdown,
    Json,
    JsonLines,
    Csv,
    Yaml,
    Toml,
    Ini,
    Log,
    Code,
    PlainText,
}

/// Suffixes claimed by the app (see `platform::install` and packaging).
/// All ride the same zero-dependency viewers; `Open With` entries, never a
/// default handler grab.
///
/// A leading dot is not an "extension" to `Path::extension`, but it is to
/// Windows and to the shell: `.gitignore` is registered as
/// `Software\Classes\.gitignore`, so the entry here is written without its
/// dot. [`suffix`] is what makes the parse side agree.
pub const EXTENSIONS: &[&str] = &[
    "md",
    "markdown",
    "json",
    "jsonl",
    "ndjson",
    "txt",
    "log",
    "csv",
    "tsv",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "rs",
    "py",
    "js",
    "ts",
    "c",
    "h",
    "cpp",
    "hpp",
    "cs",
    "java",
    "go",
    "rb",
    "php",
    "css",
    "html",
    "xml",
    "sh",
    "gitignore",
    "gitattributes",
    "gitmodules",
    "editorconfig",
    "dockerignore",
    "npmrc",
    "nvmrc",
    "prettierrc",
];

/// The dotfile suffixes: names whose whole point is that they have no
/// extension (`Path::extension()` returns `None` for them). Kept separate from
/// [`EXTENSIONS`] only so the packaging tests and the docs can say "dotfile"
/// instead of making a reader infer it from the missing dot.
#[cfg(test)]
pub const DOTFILES: &[&str] = &[
    "gitignore",
    "gitattributes",
    "gitmodules",
    "editorconfig",
    "dockerignore",
    "npmrc",
    "nvmrc",
    "prettierrc",
];

/// Files whose name *is* the type: no dot at all, so no registry can claim
/// them as an extension. The app still opens them (a preview needs to know
/// what it is looking at), and the Linux MIME package matches them by exact
/// glob; Windows and macOS have no per-filename association mechanism, so
/// there they stay "Open with" choices only.
pub const NAMED_FILES: &[&str] = &[
    "dockerfile",
    "makefile",
    "rakefile",
    "gemfile",
    "procfile",
    "justfile",
    "codeowners",
    "license",
];

/// The suffix [`FileKind`] classifies by: a normal extension, or the name
/// after the dot for a dotfile. `None` for a name that has neither.
fn suffix(p: &Path) -> Option<String> {
    if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
        return Some(ext.to_ascii_lowercase());
    }
    let name = p.file_name()?.to_str()?.to_ascii_lowercase();
    let stem = name.strip_prefix('.')?;
    // `.env.local` has a real extension (`local`); only the single-segment
    // form (`.gitignore`) belongs here.
    (!stem.is_empty() && !stem.contains('.')).then(|| stem.to_string())
}

impl FileKind {
    pub fn from_path(p: &Path) -> Self {
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            let name = name.to_ascii_lowercase();
            if NAMED_FILES.contains(&name.as_str()) {
                return match name.as_str() {
                    "license" | "codeowners" => Self::PlainText,
                    _ => Self::Code,
                };
            }
        }
        match suffix(p).as_deref() {
            Some("md" | "markdown" | "mdown") => Self::Markdown,
            Some("json") => Self::Json,
            Some("jsonl" | "ndjson") => Self::JsonLines,
            Some("csv" | "tsv" | "tab") => Self::Csv,
            Some("yaml" | "yml") => Self::Yaml,
            Some("toml") => Self::Toml,
            Some("ini" | "cfg" | "conf" | "editorconfig" | "npmrc" | "nvmrc" | "prettierrc") => {
                Self::Ini
            }
            Some("log") => Self::Log,
            Some("gitignore" | "gitattributes" | "gitmodules" | "dockerignore") => Self::PlainText,
            Some(
                "rs" | "py" | "js" | "ts" | "c" | "h" | "cpp" | "hpp" | "cs" | "java" | "go" | "rb"
                | "php" | "css" | "html" | "xml" | "sh",
            ) => Self::Code,
            _ => Self::PlainText,
        }
    }
}

/// `*.ext` globs for every claimed extension, joined by `sep`: the single
/// source for open-dialog filters on all three backends (`;` on Windows,
/// spaces for zenity), so the dialogs never drift from what the app opens
/// and the installer registers.
///
/// Present only where a dialog consumes it (the macOS panel enumerates
/// [`EXTENSIONS`] directly) plus test builds, so the shared-API dead-code
/// lint stays quiet on every native target.
#[cfg(any(
    target_os = "linux",
    target_os = "windows",
    editor_check_linux,
    editor_check_win,
    test
))]
pub fn globs(sep: &str) -> String {
    EXTENSIONS
        .iter()
        .map(|e| format!("*.{e}"))
        .collect::<Vec<_>>()
        .join(sep)
}

/// Save-dialog groups: `(label, extensions)`, mirroring [`FileKind`].
/// Every [`EXTENSIONS`] entry appears in exactly one group (pinned by test).
/// Windows-only by the same rule as [`globs`]: the save dialog that reads it
/// is Windows code.
#[cfg(any(target_os = "windows", editor_check_win, test))]
pub const SAVE_GROUPS: &[(&str, &[&str])] = &[
    ("Markdown", &["md", "markdown"]),
    ("JSON", &["json"]),
    ("JSON Lines", &["jsonl", "ndjson"]),
    ("CSV", &["csv", "tsv"]),
    ("Config", &["yaml", "yml", "toml", "ini", "cfg", "conf"]),
    (
        "Dotfiles",
        &[
            "gitignore",
            "gitattributes",
            "gitmodules",
            "editorconfig",
            "dockerignore",
            "npmrc",
            "nvmrc",
            "prettierrc",
        ],
    ),
    (
        "Code",
        &[
            "rs", "py", "js", "ts", "c", "h", "cpp", "hpp", "cs", "java", "go", "rb", "php", "css",
            "html", "xml", "sh",
        ],
    ),
    ("Text", &["txt", "log"]),
];

impl FileKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::Json => "JSON",
            Self::JsonLines => "JSON Lines",
            Self::Csv => "CSV",
            Self::Yaml => "YAML",
            Self::Toml => "TOML",
            Self::Ini => "INI",
            Self::Log => "Log",
            Self::Code => "Code",
            Self::PlainText => "Plain text",
        }
    }

    /// Extension used when saving a new buffer of this kind (Save-As default).
    pub fn default_ext(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json => "json",
            Self::JsonLines => "jsonl",
            Self::Csv => "csv",
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Ini => "ini",
            // Logs, code, and text save as plain text by default; the open
            // path's suffix already decides the viewer for existing files.
            Self::Log | Self::Code | Self::PlainText => "txt",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects() {
        assert_eq!(FileKind::from_path(Path::new("a.MD")), FileKind::Markdown);
        assert_eq!(
            FileKind::from_path(Path::new("a.ndjson")),
            FileKind::JsonLines
        );
        assert_eq!(FileKind::from_path(Path::new("a")), FileKind::PlainText);
    }

    /// `.gitignore` has no extension as far as `Path` is concerned, which is
    /// why it opened as plain text and, before [`suffix`], could not be
    /// associated at all: the installer wrote `Software\Classes\.gitignore`
    /// from this list while the parse side asked for an extension that does
    /// not exist.
    #[test]
    fn dotfiles_are_classified_by_their_whole_name() {
        assert_eq!(
            FileKind::from_path(Path::new(".gitignore")),
            FileKind::PlainText
        );
        assert_eq!(
            FileKind::from_path(Path::new(".gitattributes")),
            FileKind::PlainText
        );
        assert_eq!(
            FileKind::from_path(Path::new(".editorconfig")),
            FileKind::Ini
        );
        assert_eq!(FileKind::from_path(Path::new(".npmrc")), FileKind::Ini);
        assert_eq!(
            FileKind::from_path(Path::new(r"C:\repo\.prettierrc")),
            FileKind::Ini
        );
        for dot in DOTFILES {
            assert_eq!(
                FileKind::from_path(Path::new(&format!(".{dot}"))),
                FileKind::from_path(Path::new(&format!("x.{dot}"))),
                ".{dot} and x.{dot} must classify the same"
            );
        }
        // A dotfile with a real second extension is not a dotfile.
        assert_eq!(
            FileKind::from_path(Path::new(".env.local")),
            FileKind::PlainText
        );
        assert_eq!(suffix(Path::new(".env.local")).as_deref(), Some("local"));
        assert_eq!(
            suffix(Path::new(".gitignore")).as_deref(),
            Some("gitignore")
        );
    }

    /// Bare names have no suffix to register, but a preview still has to pick
    /// a viewer for them.
    #[test]
    fn named_files_pick_a_viewer() {
        assert_eq!(FileKind::from_path(Path::new("Dockerfile")), FileKind::Code);
        assert_eq!(FileKind::from_path(Path::new("Makefile")), FileKind::Code);
        assert_eq!(
            FileKind::from_path(Path::new("LICENSE")),
            FileKind::PlainText
        );
        assert_eq!(
            FileKind::from_path(Path::new("CODEOWNERS")),
            FileKind::PlainText
        );
        // Casing is not significant, and a directory is not a name.
        assert_eq!(FileKind::from_path(Path::new("dockerfile")), FileKind::Code);
        assert_eq!(
            FileKind::from_path(Path::new("vendor/License.md")),
            FileKind::Markdown
        );
    }

    #[test]
    fn claimed_extensions_resolve() {
        for e in EXTENSIONS {
            let kind = FileKind::from_path(Path::new(&format!("x.{e}")));
            assert!(!kind.label().is_empty());
        }
        // Spot-check the mapping the installer relies on.
        assert_eq!(FileKind::from_path(Path::new("x.md")), FileKind::Markdown);
        assert_eq!(FileKind::from_path(Path::new("x.json")), FileKind::Json);
        assert_eq!(
            FileKind::from_path(Path::new("x.jsonl")),
            FileKind::JsonLines
        );
        assert_eq!(FileKind::from_path(Path::new("x.log")), FileKind::Log);
        assert_eq!(FileKind::from_path(Path::new("x.csv")), FileKind::Csv);
        assert_eq!(FileKind::from_path(Path::new("x.tsv")), FileKind::Csv);
        assert_eq!(FileKind::from_path(Path::new("x.yaml")), FileKind::Yaml);
        assert_eq!(FileKind::from_path(Path::new("x.yml")), FileKind::Yaml);
        assert_eq!(FileKind::from_path(Path::new("x.toml")), FileKind::Toml);
        assert_eq!(FileKind::from_path(Path::new("x.ini")), FileKind::Ini);
        assert_eq!(FileKind::from_path(Path::new("x.cfg")), FileKind::Ini);
        assert_eq!(FileKind::from_path(Path::new("x.rs")), FileKind::Code);
    }

    /// The packaging manifests carry hand-written copies of [`EXTENSIONS`],
    /// so an extension added here quietly stops being associated until
    /// someone remembers all three files. Read them at compile time and fail
    /// here instead — this is the test `packaging/windows/editor.wxs` used to
    /// point at as missing.
    #[test]
    fn packaging_manifests_claim_exactly_the_supported_extensions() {
        const WXS: &str = include_str!("../../packaging/windows/editor.wxs");
        const PLIST: &str = include_str!("../../packaging/macos/Info.plist");

        let mut want: Vec<&str> = EXTENSIONS.to_vec();
        want.sort_unstable();
        want.dedup();

        // Windows: one `Software\Classes\.<ext>\OpenWithProgids` key per
        // extension, shared by the MSI and the portable `--install` path.
        // Anchor on the attribute, not the bare path: the file's own comments
        // name a key when explaining why it exists.
        const KEY: &str = "Key=\"Software\\Classes\\.";
        let mut win: Vec<&str> = WXS
            .match_indices(KEY)
            .filter_map(|(i, key)| WXS[i + key.len()..].split('\\').next())
            .collect();
        win.sort_unstable();
        win.dedup();
        assert_eq!(win, want, "editor.wxs and EXTENSIONS disagree");

        // macOS: the bundle's document types, which is what Finder offers.
        let array = PLIST
            .split("<key>CFBundleTypeExtensions</key>")
            .nth(1)
            .expect("no CFBundleTypeExtensions in Info.plist")
            .split("</array>")
            .next()
            .unwrap();
        let mut mac: Vec<&str> = array
            .split("<string>")
            .skip(1)
            .filter_map(|s| s.split("</string>").next())
            .collect();
        mac.sort_unstable();
        mac.dedup();
        assert_eq!(mac, want, "Info.plist and EXTENSIONS disagree");
    }

    /// The desktop entry declares the MIME types the app handles; install.sh
    /// loops over the list it makes default. Two copies of one list: a
    /// mismatch leaves a type declared but undefaulted, or a default for a
    /// type the entry does not even claim.
    #[test]
    fn linux_desktop_entry_and_installer_agree_on_mime_types() {
        const DESKTOP: &str = include_str!("../../packaging/linux/editor.desktop");
        const INSTALL: &str = include_str!("../../packaging/linux/install.sh");

        let declared: Vec<&str> = DESKTOP
            .lines()
            .find_map(|l| l.strip_prefix("MimeType="))
            .expect("no MimeType line in editor.desktop")
            .split(';')
            .filter(|t| !t.is_empty())
            .collect();
        let installed: Vec<&str> = INSTALL
            .split("for t in ")
            .nth(1)
            .expect("no `for t in` loop in install.sh")
            .split("; do")
            .next()
            .unwrap()
            .split_whitespace()
            .collect();
        assert_eq!(
            declared, installed,
            "editor.desktop and install.sh disagree on the MIME types"
        );
    }

    /// The dotfile MIME package is the only place Linux can say "this exact
    /// name is Editor's": unlike Windows there is no extension key for
    /// `.gitignore`, and unlike a filename argument there is no shell to
    /// invoke. The globs and [`DOTFILES`]/[`NAMED_FILES`] are two copies of
    /// one promise, so they are compared here rather than in a manual install.
    #[test]
    fn linux_mime_package_matches_the_claimed_dotfiles_and_names() {
        const XML: &str = include_str!("../../packaging/linux/editor-mime.xml");
        let mut globs: Vec<String> = XML
            .match_indices("<glob pattern=\"")
            .filter_map(|(i, key)| {
                let rest = &XML[i + key.len()..];
                rest.split('"').next().map(|s| s.to_string())
            })
            .collect();
        globs.sort();
        globs.dedup();

        for dot in DOTFILES {
            let want = format!("*.{dot}");
            assert!(globs.contains(&want), "editor-mime.xml lacks {want}");
        }
        for name in NAMED_FILES {
            assert!(
                globs.iter().any(|g| g.eq_ignore_ascii_case(name)),
                "editor-mime.xml lacks {name}"
            );
        }
    }

    #[test]
    fn dialog_filters_cover_every_claimed_extension() {
        // Open filter: one glob per claimed extension.
        let pattern = globs(";");
        assert_eq!(pattern.matches("*.").count(), EXTENSIONS.len());
        for ext in ["csv", "tsv", "yaml", "toml", "ini", "log", "rs", "py", "sh"] {
            assert!(pattern.contains(&format!("*.{ext}")), "missing {ext}");
        }
        // Save groups: every claimed extension in exactly one group.
        let mut seen = std::collections::HashSet::new();
        for (_, exts) in SAVE_GROUPS {
            for e in *exts {
                assert!(seen.insert(*e), "duplicate group entry: {e}");
            }
        }
        for ext in EXTENSIONS {
            assert!(seen.contains(ext), "no save group claims .{ext}");
        }
    }
}
