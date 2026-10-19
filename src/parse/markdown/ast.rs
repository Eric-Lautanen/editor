//! Markdown document model: CommonMark subset + GFM tables/task lists (M4).
//!
//! Parsing is total — anything unrecognized degrades to paragraphs, so there
//! is no error type. Both fenced and indented code blocks are carried as
//! [`Block::Code`].

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Document {
    pub blocks: Vec<Block>,
    /// The reference definitions the inlines resolved against, kept so
    /// the patch path can reuse the map an edit provably did not
    /// change instead of rescanning the whole text to rebuild it.
    pub defs: std::collections::HashMap<String, String>,
    /// T2 · Each block's source line range, half-open, in *document*
    /// coordinates — the buffer's lines minus the frontmatter lead below.
    /// Parallel to `blocks`, in the same order. Blank lines, reference
    /// definitions and skipped HTML blocks belong to no block and appear in
    /// no span. The patch path uses these to find the block run an edit
    /// landed in without re-parsing the document; equal `blocks` with
    /// different `spans` are unequal, which is what makes the splice tests
    /// check the bookkeeping along with the content.
    pub spans: Vec<(u32, u32)>,
    /// T2 · Frontmatter lines stripped from the top before parsing: block
    /// line numbers are buffer lines minus this. An empty lead is the doctor
    /// that lets the patch path treat the two spaces as the same one. (The
    /// BOM strip mutates `line(0)`'s content but never the line count.)
    pub lead: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    /// Code block: fenced (with an optional info string) or indented.
    Code {
        lang: Option<String>,
        text: String,
    },
    Quote(Vec<Block>),
    List {
        ordered: bool,
        start: u32,
        items: Vec<ListItem>,
    },
    Table {
        align: Vec<Align>,
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    /// Task-list state: `Some(true)` = `[x]`, `Some(false)` = `[ ]`.
    pub checked: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Code(String),
    Emph(Vec<Inline>),
    Strong(Vec<Inline>),
    Strike(Vec<Inline>),
    Link { href: String, content: Vec<Inline> },
    Image { src: String, alt: String },
    SoftBreak,
    HardBreak,
}
