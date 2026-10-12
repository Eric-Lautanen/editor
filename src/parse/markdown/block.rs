//! Block-level Markdown: ATX/setext headings, paragraphs, fenced code,
//! blockquotes, ordered/unordered/task lists (nested), rules, GFM tables.
//!
//! Line-based, single pass with local lookahead. Anything unrecognized
//! degrades to paragraph text, so parsing is total.

use super::ast::*;
use super::inline::parse_inlines;

/// Leading tabs count as 4 columns for indent measurement; the tab itself is
/// replaced so downstream code works in spaces.
fn expand_leading_tabs(line: &str) -> String {
    let mut out = String::new();
    let mut col = 0;
    let mut chars = line.chars();
    loop {
        match chars.clone().next() {
            Some('\t') => {
                chars.next();
                let fill = 4 - col % 4;
                for _ in 0..fill {
                    out.push(' ');
                }
                col += fill;
            }
            Some(' ') => {
                chars.next();
                out.push(' ');
                col += 1;
            }
            _ => break,
        }
    }
    out.push_str(chars.as_str());
    out
}

fn indent_of(line: &str) -> usize {
    line.bytes().take_while(|&b| b == b' ').count()
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// `---`/`***`/`___` thematic break (spaces allowed between markers).
fn is_rule(line: &str) -> bool {
    let t = line.trim();
    if t.len() < 3 {
        return false;
    }
    let c = t.as_bytes()[0];
    if c != b'-' && c != b'*' && c != b'_' {
        return false;
    }
    t.bytes().all(|b| b == c || b == b' ')
}

/// Setext underline: `===` → h1, `---` → h2.
fn setext_level(line: &str) -> Option<u8> {
    let t = line.trim();
    if !t.is_empty() && t.bytes().all(|b| b == b'=') {
        Some(1)
    } else if !t.is_empty() && t.bytes().all(|b| b == b'-') {
        Some(2)
    } else {
        None
    }
}

/// ATX heading: up to 3 leading spaces, 1–6 `#`, then space or end of line.
fn atx_heading(line: &str) -> Option<(u8, &str)> {
    let t = line.trim_start();
    if indent_of(line) > 3 {
        return None;
    }
    let hashes = t.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &t[hashes..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let mut content = rest.trim();
    // Optional closing hash run.
    if let Some(pos) = content.rfind(" #") {
        let tail = &content[pos + 1..];
        if !tail.is_empty() && tail.bytes().all(|b| b == b'#') {
            content = content[..pos].trim_end();
        }
    } else if content.starts_with('#') && content.bytes().all(|b| b == b'#') {
        content = "";
    }
    Some((hashes as u8, content))
}

struct Fence {
    char: char,
    len: usize,
    lang: Option<String>,
}

/// Opening fence: up to 3 spaces, 3+ backticks (info without backticks) or tildes.
fn open_fence(line: &str) -> Option<Fence> {
    let t = line.trim_start();
    if indent_of(line) > 3 {
        return None;
    }
    let c = t.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let len = t.chars().take_while(|&x| x == c).count();
    if len < 3 {
        return None;
    }
    let info = t[len..].trim();
    if c == '`' && info.contains('`') {
        return None;
    }
    let lang = info.split_whitespace().next().map(|s| s.to_string());
    Some(Fence { char: c, len, lang })
}

fn is_close_fence(line: &str, f: &Fence) -> bool {
    let t = line.trim_start();
    indent_of(line) <= 3
        && t.chars().take_while(|&x| x == f.char).count() >= f.len
        && t[f.len.min(t.len())..].trim().is_empty()
        && t.starts_with(f.char)
}

/// Quote prefix length (`>`, ` >`, `  > ` …), or `None`.
fn quote_prefix(line: &str) -> Option<usize> {
    let mut i = 0;
    let b = line.as_bytes();
    while i < b.len() && b[i] == b' ' && i < 3 {
        i += 1;
    }
    if b.get(i) == Some(&b'>') {
        i += 1;
        if b.get(i) == Some(&b' ') {
            i += 1;
        }
        Some(i)
    } else {
        None
    }
}

struct ListMarker {
    ordered: bool,
    delim: char,
    start: u32,
    /// Column where the item content starts (marker + following spaces).
    content_indent: usize,
    indent: usize,
    task: Option<bool>,
}

fn list_marker(line: &str) -> Option<ListMarker> {
    let indent = indent_of(line);
    if indent > 3 {
        return None;
    }
    let t = &line[indent..];
    let mut it = t.chars();
    let first = it.next()?;
    let (ordered, delim, num_len, start) = if first == '-' || first == '+' || first == '*' {
        (false, first, 1, 1)
    } else if first.is_ascii_digit() {
        let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.len() > 9 {
            return None;
        }
        let d = t.chars().nth(digits.len())?;
        if d != '.' && d != ')' {
            return None;
        }
        let start = digits.parse().unwrap_or(1);
        (true, d, digits.len() + 1, start)
    } else {
        return None;
    };
    let mut rest = &t[num_len..];
    if rest.is_empty() {
        // Bare marker: an empty item; continuations indent past the marker.
        return Some(ListMarker {
            ordered,
            delim,
            start,
            content_indent: indent + num_len + 1,
            indent,
            task: None,
        });
    }
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    // Content starts after the marker, following spaces, and task box.
    // (5+ spaces collapse to one column per CommonMark.)
    let spaces = rest
        .bytes()
        .take_while(|&b| b == b' ' || b == b'\t')
        .count();
    rest = &rest[spaces..];
    let mut task = None;
    if rest.starts_with("[ ] ") || rest.starts_with("[ ]\t") || rest == "[ ]" {
        task = Some(false);
        rest = rest[3..].trim_start_matches([' ', '\t']);
    } else if rest.starts_with("[x] ") || rest.starts_with("[X] ") || rest == "[x]" || rest == "[X]"
    {
        task = Some(true);
        rest = rest[3..].trim_start_matches([' ', '\t']);
    }
    let content_indent = if rest.is_empty() {
        indent + num_len + 1
    } else if spaces >= 5 {
        // 5+ spaces after the marker collapse to a single column.
        indent + num_len + 1
    } else {
        indent + t.len() - rest.len()
    };
    Some(ListMarker {
        ordered,
        delim,
        start,
        content_indent,
        indent,
        task,
    })
}

/// Block HTML that we skip wholesale (until a blank line): a `<` tag opener.
fn is_html_block_start(line: &str) -> bool {
    let t = line.trim_start();
    if indent_of(line) > 3 || !t.starts_with('<') {
        return false;
    }
    let inner = &t[1..];
    matches!(
        inner.chars().next(),
        Some(c) if c.is_ascii_alphabetic() || c == '/' || c == '!' || c == '?'
    )
}

/// Split a table row on unescaped pipes; strips one outer pipe pair.
fn split_row(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = line.trim().chars().peekable();
    // Strip one leading/trailing pipe (they only frame the row).
    let mut body: String = chars.collect();
    if body.starts_with('|') {
        body = body[1..].to_string();
    }
    if body.ends_with('|') {
        body.pop();
    }
    let mut it = body.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some(n) => {
                    cur.push(n);
                }
                None => cur.push('\\'),
            }
        } else if c == '|' {
            cells.push(cur.trim().to_string());
            cur = String::new();
        } else {
            cur.push(c);
        }
    }
    cells.push(cur.trim().to_string());
    chars = body.chars().peekable();
    let _ = chars;
    cells
}

/// Delimiter row: every cell is `:?-+:?`. Returns alignments on success.
fn delimiter_row(line: &str) -> Option<Vec<Align>> {
    if !line.contains('|') && !line.contains('-') {
        return None;
    }
    let cells = split_row(line);
    if cells.is_empty() {
        return None;
    }
    let mut align = Vec::new();
    for cell in &cells {
        let c = cell.trim();
        let (l, r) = (c.starts_with(':'), c.ends_with(':'));
        let dashes = c.trim_matches(':');
        if dashes.is_empty() || !dashes.bytes().all(|b| b == b'-') {
            return None;
        }
        align.push(match (l, r) {
            (true, true) => Align::Center,
            (true, false) => Align::Left,
            (false, true) => Align::Right,
            (false, false) => Align::None,
        });
    }
    Some(align)
}

struct Parser {
    lines: Vec<String>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&str> {
        self.lines.get(self.pos).map(|s| s.as_str())
    }

    /// A line starts a new block construct (used to terminate paragraphs and
    /// table bodies). With `in_para`, the paragraph-interrupt rules apply: a
    /// list breaks a paragraph only for bullets or `1`-started ordered lists.
    fn structural(&self, in_para: bool) -> bool {
        let Some(line) = self.peek() else {
            return false;
        };
        if is_blank(line) {
            return true;
        }
        if open_fence(line).is_some() || atx_heading(line).is_some() {
            return true;
        }
        if quote_prefix(line).is_some() || is_html_block_start(line) {
            return true;
        }
        if is_rule(line) {
            return true;
        }
        if let Some(m) = list_marker(line) {
            return !in_para || !m.ordered || m.start == 1;
        }
        false
    }

    fn parse_top(&mut self) -> Vec<Block> {
        let mut blocks = Vec::new();
        while self.pos < self.lines.len() {
            if is_blank(self.peek().unwrap_or("")) {
                self.pos += 1;
                continue;
            }
            if let Some(b) = self.parse_fence() {
                blocks.push(b);
            } else if let Some(b) = self.parse_atx() {
                blocks.push(b);
            } else if let Some(b) = self.parse_quote() {
                blocks.push(b);
            } else if let Some(b) = self.parse_list() {
                blocks.push(b);
            } else if self.parse_html_skip() {
                // Ignored per plan.
            } else {
                blocks.extend(self.parse_paragraph_or_table());
            }
        }
        blocks
    }

    fn parse_fence(&mut self) -> Option<Block> {
        let f = open_fence(self.peek()?)?;
        self.pos += 1;
        let mut text = Vec::new();
        while self.pos < self.lines.len() {
            if is_close_fence(&self.lines[self.pos], &f) {
                self.pos += 1;
                break;
            }
            text.push(self.lines[self.pos].clone());
            self.pos += 1;
        }
        Some(Block::Code {
            lang: f.lang,
            text: text.join("\n"),
        })
    }

    fn parse_atx(&mut self) -> Option<Block> {
        let (level, content) = atx_heading(self.peek()?)?;
        let content = content.to_string();
        self.pos += 1;
        Some(Block::Heading {
            level,
            content: parse_inlines(&content),
        })
    }

    fn parse_quote(&mut self) -> Option<Block> {
        quote_prefix(self.peek()?)?;
        let mut inner = Vec::new();
        while self.pos < self.lines.len() {
            let line = self.lines[self.pos].clone();
            if is_blank(&line) {
                // A blank line ends the quote unless a quote line follows.
                let mut j = self.pos + 1;
                while j < self.lines.len() && is_blank(&self.lines[j]) {
                    j += 1;
                }
                if j < self.lines.len() && quote_prefix(&self.lines[j]).is_some() {
                    inner.push(String::new());
                    self.pos += 1;
                    continue;
                }
                break;
            }
            match quote_prefix(&line) {
                Some(n) => inner.push(line[n..].to_string()),
                // Lazy continuation: plain text joins the quote paragraph.
                None if !self.structural(false) => inner.push(line),
                None => break,
            }
            self.pos += 1;
        }
        Some(Block::Quote(parse_blocks(&inner.join("\n")).blocks))
    }

    fn parse_list(&mut self) -> Option<Block> {
        let first = list_marker(self.peek()?)?;
        let (ordered, delim) = (first.ordered, first.delim);
        let start = first.start;
        let mut items = Vec::new();
        while let Some(m) = self.peek().and_then(list_marker) {
            if m.ordered != ordered || m.delim != delim || m.indent != first.indent {
                break;
            }
            self.pos += 1;
            let content_indent = m.content_indent;
            let mut content = Vec::new();
            // First line: everything after the marker + task box.
            content.push(
                self.lines[self.pos - 1][content_indent.min(self.lines[self.pos - 1].len())..]
                    .to_string(),
            );
            // Continuation lines.
            while let Some(line) = self.peek() {
                if is_blank(line) {
                    // Look ahead: sibling, deeper content, or end?
                    let mut j = self.pos + 1;
                    while j < self.lines.len() && is_blank(&self.lines[j]) {
                        j += 1;
                    }
                    if j < self.lines.len() {
                        let next = &self.lines[j];
                        let next_indent = indent_of(next);
                        if list_marker(next).is_some_and(|nm| {
                            nm.ordered == ordered && nm.delim == delim && nm.indent == first.indent
                        }) || next_indent >= content_indent
                        {
                            content.push(String::new());
                            self.pos += 1;
                            continue;
                        }
                    }
                    break;
                }
                let ind = indent_of(line);
                if ind >= content_indent {
                    content.push(line[content_indent..].to_string());
                    self.pos += 1;
                } else if !self.structural(false) {
                    // Lazy paragraph continuation.
                    content.push(line.trim_start().to_string());
                    self.pos += 1;
                } else {
                    break;
                }
            }
            // Drop trailing blanks from the item.
            while content.last().is_some_and(|l| l.is_empty()) {
                content.pop();
            }
            items.push(ListItem {
                checked: m.task,
                blocks: parse_blocks(&content.join("\n")).blocks,
            });
        }
        if items.is_empty() {
            return None;
        }
        Some(Block::List {
            ordered,
            start,
            items,
        })
    }

    fn parse_html_skip(&mut self) -> bool {
        if !is_html_block_start(self.peek().unwrap_or("")) {
            return false;
        }
        while self.pos < self.lines.len() && !is_blank(&self.lines[self.pos]) {
            self.pos += 1;
        }
        true
    }

    /// Paragraph with setext-heading and GFM-table promotion. Returns a
    /// sequence because a table header can trail a leading paragraph.
    fn parse_paragraph_or_table(&mut self) -> Vec<Block> {
        let mut para: Vec<String> = Vec::new();
        while let Some(line) = self.peek() {
            if is_blank(line) {
                break;
            }
            // Table delimiter directly after a pipe-bearing line?
            if !para.is_empty() && para.last().is_some_and(|l| l.contains('|')) {
                if let Some(align) = delimiter_row(line) {
                    self.pos += 1;
                    let head_line = para.pop().unwrap();
                    let mut out = Vec::new();
                    if !para.is_empty() {
                        out.push(Block::Paragraph(parse_inlines(&para.join("\n"))));
                    }
                    let mut head_cells = split_row(&head_line);
                    head_cells.resize_with(align.len(), Default::default);
                    let head = head_cells
                        .into_iter()
                        .take(align.len())
                        .map(|c| parse_inlines(&c))
                        .collect();
                    let mut rows = Vec::new();
                    while self.pos < self.lines.len() {
                        let body = &self.lines[self.pos];
                        if is_blank(body) || !body.contains('|') || self.structural(false) {
                            break;
                        }
                        let mut cells = split_row(body);
                        cells.resize_with(align.len(), Default::default);
                        rows.push(
                            cells
                                .into_iter()
                                .take(align.len())
                                .map(|c| parse_inlines(&c))
                                .collect(),
                        );
                        self.pos += 1;
                    }
                    out.push(Block::Table { align, head, rows });
                    return out;
                }
            }
            // Setext underline promotes the accumulated paragraph.
            if !para.is_empty() {
                if let Some(level) = setext_level(line) {
                    self.pos += 1;
                    return vec![Block::Heading {
                        level,
                        content: parse_inlines(&para.join("\n")),
                    }];
                }
            }
            // `***`/`___` rules interrupt; `---` was handled as setext above.
            if (para.is_empty() || !line.trim().starts_with('-')) && is_rule(line) {
                if para.is_empty() {
                    self.pos += 1;
                    return vec![Block::Rule];
                }
                break;
            }
            if self.structural(true) {
                break;
            }
            para.push(line.trim().to_string());
            self.pos += 1;
        }
        vec![Block::Paragraph(parse_inlines(&para.join("\n")))]
    }
}

pub fn parse_blocks(src: &str) -> Document {
    let lines: Vec<String> = src.lines().map(expand_leading_tabs).collect();
    let mut p = Parser { lines, pos: 0 };
    Document {
        blocks: p.parse_top(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_rules_code() {
        let d = parse_blocks("# Hi #\n\ntext\n\n---\n\n```rs\nx\n```");
        assert!(matches!(d.blocks[0], Block::Heading { level: 1, .. }));
        assert!(matches!(d.blocks[1], Block::Paragraph(_)));
        assert!(matches!(d.blocks[2], Block::Rule));
        match &d.blocks[3] {
            Block::Code { lang, text } => {
                assert_eq!(lang.as_deref(), Some("rs"));
                assert_eq!(text, "x");
            }
            other => panic!("expected code, got {other:?}"),
        }
        // Setext.
        let d = parse_blocks("Title\n=====\n\nSub\n---");
        assert!(matches!(d.blocks[0], Block::Heading { level: 1, .. }));
        assert!(matches!(d.blocks[1], Block::Heading { level: 2, .. }));
    }

    #[test]
    fn quotes_and_lists_nest() {
        let d = parse_blocks("> a\n> - x\n> - [x] done\n\n1. one\n2. two");
        match &d.blocks[0] {
            Block::Quote(inner) => {
                assert!(matches!(inner[0], Block::Paragraph(_)));
                match &inner[1] {
                    Block::List {
                        ordered: false,
                        items,
                        ..
                    } => {
                        assert_eq!(items.len(), 2);
                        assert_eq!(items[1].checked, Some(true));
                    }
                    other => panic!("expected list, got {other:?}"),
                }
            }
            other => panic!("expected quote, got {other:?}"),
        }
        match &d.blocks[1] {
            Block::List {
                ordered: true,
                start: 1,
                ..
            } => {}
            other => panic!("expected ol, got {other:?}"),
        }
        // Nested unordered.
        let d = parse_blocks("- a\n  - b\n- c");
        match &d.blocks[0] {
            Block::List { items, .. } => {
                assert_eq!(items.len(), 2);
                assert!(matches!(items[0].blocks[1], Block::List { .. }));
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn tables_align() {
        let d = parse_blocks("| a | b |\n|---|--:|\n| 1 | 2 |");
        match &d.blocks[0] {
            Block::Table { align, head, rows } => {
                assert_eq!(align, &[Align::None, Align::Right]);
                assert_eq!(head.len(), 2);
                assert_eq!(rows.len(), 1);
            }
            other => panic!("expected table, got {other:?}"),
        }
        // `---` after text without pipes is setext, not a table.
        let d = parse_blocks("hey\n---");
        assert!(matches!(d.blocks[0], Block::Heading { level: 2, .. }));
    }

    #[test]
    fn html_ignored() {
        let d = parse_blocks("<div>\nskipped\n</div>\n\nkept");
        assert_eq!(d.blocks.len(), 1);
        assert!(matches!(d.blocks[0], Block::Paragraph(_)));
    }
}
