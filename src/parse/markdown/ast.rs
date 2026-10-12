//! Markdown document model: CommonMark subset + GFM tables/task lists (M4).
//!
//! Parsing is total — anything unrecognized degrades to paragraphs, so there
//! is no error type. Indented code blocks are the one deliberate omission:
//! indented lines render as plain text (v1 limitation, noted in the plan).

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Document {
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    /// Fenced code block. (Indented code is not recognized; see module docs.)
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
