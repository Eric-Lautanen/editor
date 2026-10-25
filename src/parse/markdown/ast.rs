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
    /// Whether the text carried any footnote definition. Definitions are
    /// lifted out before the block parse and their section depends on the
    /// whole document, so the splice path must not re-parse a slice of a
    /// text that has them (an unused definition still counts).
    pub has_footnotes: bool,
}

impl Document {
    /// The top-level block index of the note numbered `number`, when the
    /// document prints one. The app turns that index into a scroll position
    /// through the plan, so a reference marker can jump to its note.
    pub fn footnote_block(&self, number: usize) -> Option<usize> {
        self.blocks
            .iter()
            .position(|b| matches!(b, Block::Footnote { number: n, .. } if *n == number))
    }

    /// The top-level block index holding the first reference to `number` —
    /// where that note's return arrow goes back to.
    pub fn footnote_reference_block(&self, number: usize) -> Option<usize> {
        self.blocks
            .iter()
            .position(|b| block_refers_to_footnote(b, number))
    }
}

fn block_refers_to_footnote(block: &Block, number: usize) -> bool {
    match block {
        Block::Heading { content, .. } | Block::Paragraph(content) => {
            inlines_refer_to_footnote(content, number)
        }
        Block::Quote(inner)
        | Block::Html { blocks: inner, .. }
        | Block::Alert { blocks: inner, .. } => {
            inner.iter().any(|b| block_refers_to_footnote(b, number))
        }
        Block::Details { summary, blocks } => {
            inlines_refer_to_footnote(summary, number)
                || blocks.iter().any(|b| block_refers_to_footnote(b, number))
        }
        Block::List { items, .. } => items.iter().any(|item| {
            item.blocks
                .iter()
                .any(|b| block_refers_to_footnote(b, number))
        }),
        Block::Table { head, rows, .. } => head
            .iter()
            .chain(rows.iter().flatten())
            .any(|cell| inlines_refer_to_footnote(cell, number)),
        // A reference inside a note points at another note; the block that
        // holds it is still this note's block.
        Block::Footnote { blocks, .. } => {
            blocks.iter().any(|b| block_refers_to_footnote(b, number))
        }
        Block::FootnoteRule | Block::Code { .. } | Block::Rule => false,
    }
}

fn inlines_refer_to_footnote(inlines: &[Inline], number: usize) -> bool {
    inlines.iter().any(|i| match i {
        Inline::FootnoteRef { number: n, .. } => *n == number,
        Inline::Emph(v)
        | Inline::Strong(v)
        | Inline::Strike(v)
        | Inline::Underline(v)
        | Inline::Highlight(v)
        | Inline::Sup(v)
        | Inline::Sub(v) => inlines_refer_to_footnote(v, number),
        Inline::Link { content, .. } => inlines_refer_to_footnote(content, number),
        _ => false,
    })
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
    /// GitHub alert (`> [!NOTE]`, `> [!WARNING]`): a callout with a label
    /// instead of an anonymous quote bar.
    Alert {
        kind: AlertKind,
        blocks: Vec<Block>,
    },
    /// An HTML container GitHub renders structurally: `<div align="center">`,
    /// `<center>`, `<p align="…">`, `<section>`, `<figure>`. The tags
    /// themselves are not painted; `align` positions the children and the
    /// children are ordinary blocks, so Markdown inside the container parses
    /// exactly as it does outside one.
    Html {
        align: Option<Align>,
        blocks: Vec<Block>,
    },
    /// `<details><summary>…</summary> … </details>`. Previews show the body
    /// expanded: hiding it behind a click in a checker people use to see what
    /// they are about to push would defeat the point.
    Details {
        summary: Vec<Inline>,
        blocks: Vec<Block>,
    },
    /// An HTML table (`<table><tr><th>…`) mapped onto the Markdown table
    /// shape, which is what makes side-by-side image READMEs render.
    Table {
        align: Vec<Align>,
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    /// The rule GitHub prints between a document and its footnote list. A
    /// synthetic block the parser appends when at least one note prints.
    FootnoteRule,
    /// One entry in that list: the marker it prints and the definition body.
    /// Numbers follow first-reference order, and each note is its own block so
    /// the plan knows where it starts (a marker can jump to it) and a band
    /// paints only the notes it shows.
    Footnote {
        number: usize,
        blocks: Vec<Block>,
    },
    List {
        ordered: bool,
        start: u32,
        items: Vec<ListItem>,
    },
    Rule,
}

/// The GitHub alert flavours. `[!NOTE]`, `[!TIP]` and `[!IMPORTANT]` share
/// the accent colour; warning and caution read as warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Note => "Note",
            Self::Tip => "Tip",
            Self::Important => "Important",
            Self::Warning => "Warning",
            Self::Caution => "Caution",
        }
    }
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
    /// `<ins>` / `<u>`: underlined text.
    Underline(Vec<Inline>),
    /// `<mark>`: highlighted text.
    Highlight(Vec<Inline>),
    /// `<sup>` / `<sub>`: raised or lowered text.
    Sup(Vec<Inline>),
    Sub(Vec<Inline>),
    /// `[^label]`: a footnote reference. `number` is filled in once the
    /// whole document has been parsed (0 means the label has no definition,
    /// which renders the reference as written).
    FootnoteRef {
        label: String,
        number: usize,
    },
    Link {
        href: String,
        content: Vec<Inline>,
    },
    Image {
        src: String,
        alt: String,
        /// `<img width="120">`; the preview sizes its placeholder from this.
        width: Option<u32>,
        height: Option<u32>,
        align: Option<Align>,
    },
    SoftBreak,
    HardBreak,
}
