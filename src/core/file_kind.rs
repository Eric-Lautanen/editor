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

/// Extensions claimed by the app (see `platform::install` and packaging).
/// All ride the same zero-dependency viewers; `Open With` entries, never a
/// default handler grab.
pub const EXTENSIONS: &[&str] = &[
    "md", "markdown", "json", "jsonl", "ndjson", "txt", "log", "csv", "tsv", "yaml", "yml", "toml",
    "ini", "cfg", "conf", "rs", "py", "js", "ts", "c", "h", "cpp", "hpp", "cs", "java", "go", "rb",
    "php", "css", "html", "xml", "sh",
];

impl FileKind {
    pub fn from_path(p: &Path) -> Self {
        match p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("md" | "markdown" | "mdown") => Self::Markdown,
            Some("json") => Self::Json,
            Some("jsonl" | "ndjson") => Self::JsonLines,
            Some("csv" | "tsv" | "tab") => Self::Csv,
            Some("yaml" | "yml") => Self::Yaml,
            Some("toml") => Self::Toml,
            Some("ini" | "cfg" | "conf") => Self::Ini,
            Some("log") => Self::Log,
            Some(
                "rs" | "py" | "js" | "ts" | "c" | "h" | "cpp" | "hpp" | "cs" | "java" | "go" | "rb"
                | "php" | "css" | "html" | "xml" | "sh",
            ) => Self::Code,
            _ => Self::PlainText,
        }
    }
}

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
}
