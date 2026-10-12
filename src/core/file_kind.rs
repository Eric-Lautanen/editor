use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Markdown,
    Json,
    JsonLines,
    PlainText,
}

/// Extensions claimed by the app (see `platform::install` and packaging/).
/// `log` rides the plain-text viewer with level/timestamp tinting later.
pub const EXTENSIONS: &[&str] = &["md", "markdown", "json", "jsonl", "ndjson", "txt", "log"];

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
            Self::PlainText => "Plain text",
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
        // `.log` intentionally rides the plain-text viewer for now.
        assert_eq!(FileKind::from_path(Path::new("x.log")), FileKind::PlainText);
    }
}
