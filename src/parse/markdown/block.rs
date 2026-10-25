//! Block-level Markdown: ATX/setext headings, paragraphs, fenced code,
//! blockquotes, ordered/unordered/task lists (nested), rules, GFM tables.
//!
//! Line-based, single pass with local lookahead. Anything unrecognized
//! degrades to paragraph text, so parsing is total.

use std::collections::HashMap;

use super::ast::*;
use super::decode_entities;
use super::html::{self, Tag, TagKind};
use super::inline::parse_inlines_with;

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

/// Strip YAML/TOML frontmatter: a leading `---`/`+++` line closed by the same
/// marker. Only at the very top, and only when the block actually looks like
/// frontmatter — otherwise a document that happens to *start* with a
/// horizontal rule and has another rule within the next fifty lines would
/// silently lose everything between them.
fn strip_frontmatter(lines: &mut Vec<String>) -> usize {
    let Some(first) = lines.first() else {
        return 0;
    };
    let marker = first.trim();
    if marker != "---" && marker != "+++" {
        return 0;
    }
    let marker = marker.to_string();
    // Bound the search so a 10k-line doc with no closing marker stays fast.
    let limit = lines.len().min(51);
    let Some(close) = (2..limit).find(|&i| lines[i].trim() == marker) else {
        return 0;
    };
    // Frontmatter opens with a key/value line (`title: …`, `port = 8080`), not
    // prose or a blank line. No such line means the `---` was a rule.
    let opens_like_mapping = lines[1..close]
        .iter()
        .map(|l| l.trim())
        .find(|t| !t.is_empty() && !t.starts_with('#'))
        .is_some_and(|t| {
            t.split_once([':', '=']).is_some_and(|(key, _)| {
                let key = key.trim();
                !key.is_empty() && !key.chars().any(char::is_whitespace)
            })
        });
    if !opens_like_mapping {
        return 0;
    }
    let lead = close + 1;
    lines.drain(0..=close);
    lead
}

/// Normalize a reference label: trim, collapse inner whitespace, lowercase.
fn norm_label(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Parse a link-reference definition line (`[label]: dest "optional title"`).
/// Returns (label, dest). Up to 3 leading spaces; titles are ignored.
fn parse_ref_def(line: &str) -> Option<(String, String)> {
    let t = line.trim_start();
    if indent_of(line) > 3 || !t.starts_with('[') {
        return None;
    }
    let close = t.find(']')?;
    let label = t[1..close].trim();
    // `[^x]: …` is a footnote definition, not a link target.
    if label.is_empty() || label.starts_with('^') || label.contains('[') || label.contains(']') {
        return None;
    }
    let rest = t[close + 1..].trim_start();
    if !rest.starts_with(':') {
        return None;
    }
    let mut dest = rest[1..].trim_start();
    if dest.is_empty() {
        return None;
    }
    let href: String;
    if dest.starts_with('<') {
        let end = dest.find('>')?;
        href = dest[1..end].to_string();
        dest = dest[end + 1..].trim_start();
        let _ = dest; // optional title follows; ignored.
    } else {
        let end = dest.find(|c: char| c.is_whitespace()).unwrap_or(dest.len());
        href = dest[..end].to_string();
    }
    if href.is_empty() || href.chars().any(|c| c.is_control()) {
        return None;
    }
    Some((norm_label(label), decode_entities(&href).into_owned()))
}

/// True when the line is a reference definition (used to break paragraphs).
fn is_ref_def(line: &str) -> bool {
    parse_ref_def(line).is_some()
}

/// Collect all reference definitions in the document.
///
/// Definitions are block-level constructs: a `[x]: url` line inside a fenced
/// or indented code block is code, not a link target, and an HTML block is
/// skipped as a whole. Scanning every line blindly let a code sample define
/// links for the rest of the file.
fn collect_ref_defs(lines: &[String]) -> HashMap<String, String> {
    let mut defs = HashMap::new();
    let mut fence: Option<Fence> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        if let Some(open) = &fence {
            if is_close_fence(line, open) {
                fence = None;
            }
            i += 1;
            continue;
        }
        if let Some(open) = open_fence(line) {
            fence = Some(open);
            i += 1;
            continue;
        }
        if starts_html_block(line) {
            i = skip_html_block(lines, i);
            continue;
        }
        if indent_of(line) >= 4 {
            i += 1;
            continue;
        }
        if let Some((label, href)) = parse_ref_def(line) {
            defs.entry(label).or_insert(href);
        }
        i += 1;
    }
    defs
}

/// Parse a footnote definition line (`[^label]: first line of the body`).
/// Returns (label, body remainder). Up to 3 leading spaces, like a link
/// definition; a deeper indent is code, not a definition.
fn footnote_def(line: &str) -> Option<(String, String)> {
    if indent_of(line) > 3 {
        return None;
    }
    let t = line.trim_start();
    let rest = t.strip_prefix("[^")?;
    let close = rest.find(']')?;
    let label = &rest[..close];
    if label.is_empty() || label.contains('[') || label.contains('\n') {
        return None;
    }
    let after = rest[close + 1..].strip_prefix(':')?;
    Some((label.to_string(), after.trim_start().to_string()))
}

/// One collected definition, before numbering decides whether it prints.
struct FootnoteItem {
    label: String,
    blocks: Vec<Block>,
}

/// Lift footnote definitions out of the text before the block parse.
///
/// GitHub collects these and prints the referenced ones as a numbered list at
/// the end, so the definition lines themselves must not render where they
/// were written. Definitions are taken at the top level, which is where a
/// README writes them. Each definition runs until the next
/// non-blank line that is not indented four spaces; the consumed lines are
/// blanked rather than removed, which keeps every later block's line numbers
/// (and therefore the splice bookkeeping) intact.
fn collect_footnotes(lines: &mut [String], defs: &HashMap<String, String>) -> Vec<FootnoteItem> {
    let mut found: Vec<(usize, usize, String, String)> = Vec::new();
    let mut fence: Option<Fence> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        if let Some(open) = &fence {
            if is_close_fence(line, open) {
                fence = None;
            }
            i += 1;
            continue;
        }
        if let Some(open) = open_fence(line) {
            fence = Some(open);
            i += 1;
            continue;
        }
        if starts_html_block(line) {
            i = skip_html_block(lines, i);
            continue;
        }
        if indent_of(line) >= 4 {
            i += 1;
            continue;
        }
        let Some((label, first)) = footnote_def(line) else {
            i += 1;
            continue;
        };
        let mut body = vec![first];
        let mut j = i + 1;
        while j < lines.len() {
            let body_line = &lines[j];
            if is_blank(body_line) {
                // A blank belongs to the body only when indented text follows
                // it; otherwise it separates the definition from the next
                // block.
                let mut k = j + 1;
                while k < lines.len() && is_blank(&lines[k]) {
                    k += 1;
                }
                if k < lines.len() && indent_of(&lines[k]) >= 4 {
                    body.push(String::new());
                    j += 1;
                    continue;
                }
                break;
            }
            if indent_of(body_line) >= 4 {
                body.push(body_line[4.min(body_line.len())..].to_string());
                j += 1;
                continue;
            }
            break;
        }
        found.push((i, j, label, body.join("\n")));
        i = j;
    }
    for (start, end, ..) in &found {
        for line in &mut lines[*start..*end] {
            line.clear();
        }
    }
    // First definition of a label wins; a duplicate is still consumed.
    let mut items: Vec<FootnoteItem> = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();
    for (_, _, label, body) in found {
        if seen.insert(norm_label(&label), ()).is_some() {
            continue;
        }
        items.push(FootnoteItem {
            label,
            blocks: parse_blocks_with_defs(&body, defs).blocks,
        });
    }
    items
}

/// Number the references and hand back the notes that print, in order.
///
/// A reference takes the next number the first time its label is seen, so the
/// numbers follow the text, not the order the definitions were written in.
/// References inside a definition are numbered after the reference that led
/// to it. Definitions nobody references are dropped, as are their numbers.
fn resolve_footnotes(
    blocks: &mut [Block],
    mut items: Vec<FootnoteItem>,
) -> Vec<(usize, Vec<Block>)> {
    let mut def_index: HashMap<String, usize> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        def_index.entry(norm_label(&item.label)).or_insert(i);
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut order: Vec<usize> = Vec::new();
    fn record(
        inline: &mut Inline,
        def_index: &HashMap<String, usize>,
        seen: &mut HashMap<String, usize>,
        order: &mut Vec<usize>,
    ) {
        let Inline::FootnoteRef { label, .. } = inline else {
            return;
        };
        let key = norm_label(label);
        let Some(&idx) = def_index.get(&key) else {
            return;
        };
        if let std::collections::hash_map::Entry::Vacant(e) = seen.entry(key) {
            e.insert(order.len());
            order.push(idx);
        }
    }
    walk_blocks(blocks, &mut |inline| {
        record(inline, &def_index, &mut seen, &mut order)
    });
    // Notes can reference each other: walk the bodies in the order they were
    // numbered, so a note discovered this way takes the next number.
    let mut k = 0;
    while k < order.len() {
        let idx = order[k];
        walk_blocks(&mut items[idx].blocks, &mut |inline| {
            record(inline, &def_index, &mut seen, &mut order)
        });
        k += 1;
    }
    walk_blocks(blocks, &mut |inline| {
        if let Inline::FootnoteRef { label, number } = inline {
            *number = seen.get(&norm_label(label)).map_or(0, |p| p + 1);
        }
    });
    for item in items.iter_mut() {
        walk_blocks(&mut item.blocks, &mut |inline| {
            if let Inline::FootnoteRef { label, number } = inline {
                *number = seen.get(&norm_label(label)).map_or(0, |p| p + 1);
            }
        });
    }
    let mut numbered: Vec<(usize, Vec<Block>)> = items
        .into_iter()
        .filter_map(|item| {
            let number = seen.get(&norm_label(&item.label))? + 1;
            Some((number, item.blocks))
        })
        .collect();
    numbered.sort_by_key(|(number, _)| *number);
    numbered
}

/// Visit every inline in the block tree, in reading order.
fn walk_blocks(blocks: &mut [Block], f: &mut impl FnMut(&mut Inline)) {
    for block in blocks {
        match block {
            Block::Heading { content, .. } | Block::Paragraph(content) => walk_inlines(content, f),
            Block::Quote(inner)
            | Block::Html { blocks: inner, .. }
            | Block::Alert { blocks: inner, .. } => walk_blocks(inner, f),
            Block::Details { summary, blocks } => {
                walk_inlines(summary, f);
                walk_blocks(blocks, f);
            }
            Block::List { items, .. } => {
                for item in items {
                    walk_blocks(&mut item.blocks, f);
                }
            }
            Block::Table { head, rows, .. } => {
                for cell in head {
                    walk_inlines(cell, f);
                }
                for row in rows {
                    for cell in row {
                        walk_inlines(cell, f);
                    }
                }
            }
            Block::Footnote { blocks, .. } => walk_blocks(blocks, f),
            Block::FootnoteRule | Block::Code { .. } | Block::Rule => {}
        }
    }
}

fn walk_inlines(inlines: &mut [Inline], f: &mut impl FnMut(&mut Inline)) {
    for inline in inlines {
        f(inline);
        match inline {
            Inline::Emph(v)
            | Inline::Strong(v)
            | Inline::Strike(v)
            | Inline::Underline(v)
            | Inline::Highlight(v)
            | Inline::Sup(v)
            | Inline::Sub(v) => walk_inlines(v, f),
            Inline::Link { content, .. } => walk_inlines(content, f),
            _ => {}
        }
    }
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

/// GitHub's alert marker (`[!NOTE]`, `[!WARNING]`, …) as the quote's first
/// non-blank line. The marker line is removed; the rest of the quote becomes
/// the callout body.
fn take_alert(lines: &mut Vec<String>) -> Option<AlertKind> {
    let idx = lines.iter().position(|l| !l.trim().is_empty())?;
    let t = lines[idx].trim();
    let name = t.strip_prefix("[!")?.strip_suffix(']')?;
    let kind = match name.to_ascii_uppercase().as_str() {
        "NOTE" => AlertKind::Note,
        "TIP" => AlertKind::Tip,
        "IMPORTANT" => AlertKind::Important,
        "WARNING" => AlertKind::Warning,
        "CAUTION" => AlertKind::Caution,
        _ => return None,
    };
    lines.remove(idx);
    Some(kind)
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

/// An HTML construct at the start of a line that the block parser handles.
/// Unknown tags never reach here: a line of `<span>text</span>` is a
/// paragraph, and the inline parser drops the tags inside it.
enum HtmlStart {
    /// `<!-- … -->` or a `<!doctype …>` declaration; dropped with contents.
    Skip,
    /// A block-level tag, with its attribute set already parsed.
    Tag { tag: Tag },
}

/// Tags whose interior is a block of its own rather than inline text.
fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "div"
            | "center"
            | "p"
            | "section"
            | "figure"
            | "figcaption"
            | "picture"
            | "article"
            | "main"
            | "header"
            | "footer"
            | "nav"
            | "aside"
            | "blockquote"
            | "details"
            | "summary"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "pre"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "hr"
    )
}

fn html_start(line: &str) -> Option<HtmlStart> {
    if indent_of(line) > 3 {
        return None;
    }
    let chars: Vec<char> = line.chars().collect();
    let at = chars.iter().position(|c| *c == '<')?;
    if at > 3 || chars.get(at + 1) == Some(&'!') {
        if at > 3 {
            return None;
        }
        return Some(HtmlStart::Skip);
    }
    let tag = html::tag_at(&chars, at)?;
    if tag.kind != TagKind::Open || !(is_block_tag(&tag.name) || html::is_dangerous(&tag.name)) {
        return None;
    }
    Some(HtmlStart::Tag { tag })
}

/// A line that ends a paragraph: a comment, a dangerous element, or a
/// structural tag. Inline-only tags do not.
fn starts_html_block(line: &str) -> bool {
    match html_start(line) {
        Some(HtmlStart::Skip) => true,
        Some(HtmlStart::Tag { tag, .. }) => {
            html::is_dangerous(&tag.name) || is_block_tag(&tag.name)
        }
        None => false,
    }
}

/// The interior of a block element opened at `line` just past `after`, and
/// where its matching close tag sits. `close` is `None` when the element is
/// never closed; the caller then consumes only the opener line.
struct HtmlRegion {
    inner: Vec<String>,
    close: Option<usize>,
}

fn html_region(lines: &[String], line: usize, after: usize, name: &str) -> HtmlRegion {
    let mut inner = Vec::new();
    let mut depth = 0usize;
    let mut li = line;
    let mut ci = after;
    loop {
        let Some(raw) = lines.get(li) else {
            return HtmlRegion { inner, close: None };
        };
        let chars: Vec<char> = raw.chars().collect();
        let mut j = ci;
        let mut found = None;
        while j < chars.len() {
            if chars[j] == '<' {
                if let Some(tag) = html::tag_at(&chars, j) {
                    if tag.name == name && !tag.self_closing {
                        match tag.kind {
                            TagKind::Close if depth == 0 => {
                                found = Some(j);
                                break;
                            }
                            TagKind::Close => depth -= 1,
                            TagKind::Open => depth += 1,
                        }
                    }
                    j = tag.end;
                    continue;
                }
            }
            j += 1;
        }
        match found {
            Some(j) => {
                let tail: String = chars[ci.min(j)..j].iter().collect();
                if !tail.trim().is_empty() || inner.is_empty() {
                    inner.push(tail);
                }
                return HtmlRegion {
                    inner,
                    close: Some(li),
                };
            }
            None => {
                inner.push(chars[ci..].iter().collect());
            }
        }
        li += 1;
        ci = 0;
    }
}

fn trim_blank_edges(lines: &mut Vec<String>) {
    while lines.first().is_some_and(|l| is_blank(l)) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| is_blank(l)) {
        lines.pop();
    }
}

/// Remove the indentation an author used to lay out nested HTML tags. Inside
/// an HTML container that indentation is formatting, not Markdown: without
/// this, the four-space-indented `<img>` of the `<picture>` idiom parsed as an
/// indented code block and the logo appeared as source text.
fn dedent(lines: &mut [String]) {
    let min = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.chars().take_while(|c| *c == ' ').count())
        .min()
        .unwrap_or(0);
    if min == 0 {
        return;
    }
    for line in lines.iter_mut() {
        let cut = line.chars().take_while(|c| *c == ' ').count().min(min);
        line.drain(..cut);
    }
}

/// The line after the HTML construct starting at `i`. Comments run to `-->`;
/// an element runs to its close tag, or to the end of the opener line when it
/// never closes (the parser does not claim content it cannot delimit).
fn skip_html_block(lines: &[String], i: usize) -> usize {
    match html_start(&lines[i]) {
        Some(HtmlStart::Skip) => {
            if !lines[i].trim_start().starts_with("<!--") {
                return i + 1;
            }
            let mut j = i;
            while j < lines.len() {
                if lines[j].contains("-->") {
                    return j + 1;
                }
                j += 1;
            }
            lines.len()
        }
        Some(HtmlStart::Tag { tag }) => {
            if tag.self_closing || tag.name == "hr" {
                return i + 1;
            }
            let region = html_region(lines, i, tag.end, &tag.name);
            region.close.map_or(i + 1, |line| line + 1)
        }
        None => i + 1,
    }
}

/// Split a table row on unescaped pipes; strips one outer pipe pair.
fn split_row(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cur = String::new();
    // Strip one leading/trailing pipe (they only frame the row).
    let trimmed = line.trim();
    let body = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let body = body.strip_suffix('|').unwrap_or(body);
    let mut it = body.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                // An escaped pipe is cell content, not a cell boundary.
                Some('|') => cur.push('|'),
                // Every other escape belongs to the inline parser, which
                // knows which punctuation a backslash actually escapes. Eating
                // it here made `\*\*` bold and `\n` lose its backslash.
                Some(n) => {
                    cur.push('\\');
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
    defs: HashMap<String, String>,
}

/// What a line that looks like HTML produced.
enum HtmlOut {
    /// A rendered block (container, heading, table, details, rule, code).
    One(Block),
    /// A comment, dangerous element, or an empty tag: consumed, not painted.
    Skipped,
    /// Not block HTML at all; the paragraph path owns the line.
    NotHtml,
}

fn is_empty_block(block: &Block) -> bool {
    match block {
        Block::Paragraph(content) => content.is_empty(),
        Block::Html { blocks, .. } => blocks.iter().all(is_empty_block),
        Block::Details { blocks, .. } => blocks.iter().all(is_empty_block),
        Block::Footnote { blocks, .. } => blocks.iter().all(is_empty_block),
        _ => false,
    }
}

impl Parser {
    fn inlines(&self, s: &str) -> Vec<super::ast::Inline> {
        parse_inlines_with(s, &self.defs)
    }
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
        if quote_prefix(line).is_some() || starts_html_block(line) {
            return true;
        }
        if is_rule(line) {
            return true;
        }
        // Reference definitions and indented code both end a paragraph.
        if is_ref_def(line) || indent_of(line) >= 4 {
            return true;
        }
        if let Some(m) = list_marker(line) {
            return !in_para || !m.ordered || m.start == 1;
        }
        false
    }

    fn parse_top(&mut self) -> (Vec<Block>, Vec<(u32, u32)>) {
        let mut blocks = Vec::new();
        let mut spans: Vec<(u32, u32)> = Vec::new();
        while self.pos < self.lines.len() {
            if is_blank(self.peek().unwrap_or("")) {
                self.pos += 1;
                continue;
            }
            // Link-reference definitions never render.
            if self.peek().is_some_and(is_ref_def) {
                self.pos += 1;
                continue;
            }
            // T2 · Record where each block sits in source lines: the run an
            // edit lands in is found through these, so it can be re-parsed
            // alone. A branch that pushes several blocks shares the range
            // (the paragraph/table pair reads the same lines); blank lines,
            // ref defs and skipped HTML blocks belong to no block.
            let start = self.pos as u32;
            let before = blocks.len();
            let block = if let Some(b) = self.parse_fence() {
                Some(b)
            } else if let Some(b) = self.parse_atx() {
                Some(b)
            } else if let Some(b) = self.parse_quote() {
                Some(b)
            } else if let Some(b) = self.parse_list() {
                Some(b)
            } else {
                match self.parse_html() {
                    HtmlOut::One(b) => Some(b),
                    HtmlOut::Skipped => None,
                    HtmlOut::NotHtml => {
                        if let Some(b) = self.parse_indented_code() {
                            Some(b)
                        } else {
                            blocks.extend(self.parse_paragraph_or_table());
                            None
                        }
                    }
                }
            };
            if let Some(b) = block {
                blocks.push(b);
            }
            let pushed = blocks.len() - before;
            if pushed > 0 {
                let end = self.pos as u32;
                spans.extend(std::iter::repeat_n((start, end), pushed));
            }
        }
        blocks.shrink_to_fit();
        spans.shrink_to_fit();
        (blocks, spans)
    }

    /// Indented code: consecutive lines indented 4+. Blank lines ride along
    /// only when followed by more code; trailing blanks are dropped.
    fn parse_indented_code(&mut self) -> Option<Block> {
        if indent_of(self.peek()?) < 4 {
            return None;
        }
        let mut text: Vec<String> = Vec::new();
        while self.pos < self.lines.len() {
            let line = &self.lines[self.pos];
            if is_blank(line) {
                // Look ahead: keep the blank only if code continues.
                let mut j = self.pos + 1;
                while j < self.lines.len() && is_blank(&self.lines[j]) {
                    j += 1;
                }
                if j < self.lines.len() && indent_of(&self.lines[j]) >= 4 {
                    text.push(String::new());
                    self.pos += 1;
                    continue;
                }
                break;
            }
            if indent_of(line) < 4 {
                break;
            }
            text.push(line[4.min(line.len())..].to_string());
            self.pos += 1;
        }
        while text.last().is_some_and(|l| l.is_empty()) {
            text.pop();
        }
        if text.is_empty() {
            return None;
        }
        Some(Block::Code {
            lang: None,
            text: text.join("\n"),
        })
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
            content: self.inlines(&content),
        })
    }

    fn parse_quote(&mut self) -> Option<Block> {
        quote_prefix(self.peek()?)?;
        let mut inner = Vec::new();
        while self.pos < self.lines.len() {
            let line = self.lines[self.pos].clone();
            if is_blank(&line) {
                // A blank line ends the quote, as CommonMark and GitHub have
                // it. Continuing across one merged `> first` and `> second`
                // into a single block, which turned a second alert
                // (`> [!WARNING]`) into literal text inside the first. A
                // quoted blank line (`>` alone) is not blank, and keeps the
                // quote going.
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
        let kind = take_alert(&mut inner);
        let blocks = parse_blocks_with_defs(&inner.join("\n"), &self.defs).blocks;
        Some(match kind {
            Some(kind) => Block::Alert { kind, blocks },
            None => Block::Quote(blocks),
        })
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
                blocks: parse_blocks_with_defs(&content.join("\n"), &self.defs).blocks,
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

    fn parse_html(&mut self) -> HtmlOut {
        let Some(line) = self.peek().map(str::to_string) else {
            return HtmlOut::NotHtml;
        };
        let Some(start) = html_start(&line) else {
            return HtmlOut::NotHtml;
        };
        let tag = match start {
            HtmlStart::Skip => {
                self.pos = skip_html_block(&self.lines, self.pos);
                return HtmlOut::Skipped;
            }
            HtmlStart::Tag { tag } => tag,
        };
        if html::is_dangerous(&tag.name) {
            self.pos = skip_html_block(&self.lines, self.pos);
            return HtmlOut::Skipped;
        }
        if tag.name == "hr" {
            self.pos += 1;
            return HtmlOut::One(Block::Rule);
        }
        let region = html_region(&self.lines, self.pos, tag.end, &tag.name);
        let Some(close_line) = region.close else {
            // Never closed: consume the opener line and render what it holds
            // as inline content (`<img>` on its own line, stray `<div>` tags).
            self.pos += 1;
            let content = self.inlines(&line);
            return if content.is_empty() {
                HtmlOut::Skipped
            } else {
                HtmlOut::One(Block::Paragraph(content))
            };
        };
        let mut inner = region.inner;
        trim_blank_edges(&mut inner);
        self.pos = close_line + 1;
        let joined = inner.join("\n");
        let block = match tag.name.as_str() {
            "details" => self.details_block(&joined),
            "table" => self.table_block(&joined),
            "ul" | "ol" => self.list_block(tag.name == "ol", &joined),
            "pre" => Some(Block::Code {
                lang: None,
                text: decode_entities(&joined).into_owned(),
            }),
            name if name.len() == 2
                && name.starts_with('h')
                && name.ends_with(['1', '2', '3', '4', '5', '6']) =>
            {
                let level = name.as_bytes()[1] - b'0';
                let content = self.inlines(&inner.join(" "));
                let heading = Block::Heading { level, content };
                Some(match tag.attrs.align {
                    Some(align) => Block::Html {
                        align: Some(align),
                        blocks: vec![heading],
                    },
                    None => heading,
                })
            }
            _ => {
                let align = if tag.name == "center" {
                    Some(Align::Center)
                } else {
                    tag.attrs.align
                };
                dedent(&mut inner);
                let blocks = parse_blocks_with_defs(&inner.join("\n"), &self.defs).blocks;
                Some(Block::Html { align, blocks })
            }
        };
        match block {
            Some(b) if !is_empty_block(&b) => HtmlOut::One(b),
            _ => HtmlOut::Skipped,
        }
    }

    fn details_block(&self, inner: &str) -> Option<Block> {
        let tags = html::tags_in(inner);
        // (open tag start, content start)
        let mut summary: Option<(usize, usize)> = None;
        let mut depth = 0usize;
        for (start, tag, end) in &tags {
            if tag.name != "summary" {
                continue;
            }
            match tag.kind {
                TagKind::Open => {
                    if depth == 0 {
                        summary = Some((*start, *end));
                    }
                    depth += 1;
                }
                TagKind::Close if depth > 0 => {
                    depth -= 1;
                    if depth == 0 {
                        if let Some((open_start, body_start)) = summary {
                            let text = &inner[body_start..*start];
                            let body = format!("{}\n{}", &inner[..open_start], &inner[*end..]);
                            let mut body: Vec<String> = body.lines().map(str::to_string).collect();
                            dedent(&mut body);
                            let content = self.inlines(text.trim());
                            return Some(Block::Details {
                                summary: if content.is_empty() {
                                    vec![Inline::Text("Details".into())]
                                } else {
                                    content
                                },
                                blocks: parse_blocks_with_defs(&body.join("\n"), &self.defs).blocks,
                            });
                        }
                    }
                }
                TagKind::Close => {}
            }
        }
        // No `<summary>`: the body is still worth showing.
        let mut lines: Vec<String> = inner.lines().map(str::to_string).collect();
        dedent(&mut lines);
        Some(Block::Details {
            summary: vec![Inline::Text("Details".into())],
            blocks: parse_blocks_with_defs(&lines.join("\n"), &self.defs).blocks,
        })
    }

    fn table_block(&self, inner: &str) -> Option<Block> {
        let tags = html::tags_in(inner);
        let mut rows: Vec<Vec<(bool, String, Option<Align>)>> = Vec::new();
        let mut row: Option<Vec<(bool, String, Option<Align>)>> = None;
        let mut cell: Option<(bool, usize, Option<Align>)> = None;
        for (start, tag, end) in &tags {
            match tag.name.as_str() {
                "tr" => match tag.kind {
                    TagKind::Open => row = Some(Vec::new()),
                    TagKind::Close => {
                        if let Some(r) = row.take() {
                            rows.push(r);
                        }
                    }
                },
                "td" | "th" => match tag.kind {
                    TagKind::Open => cell = Some((tag.name == "th", *end, tag.attrs.align)),
                    TagKind::Close => {
                        if let (Some((head, s, align)), Some(r)) = (cell.take(), row.as_mut()) {
                            r.push((head, inner[s..*start].to_string(), align));
                        }
                    }
                },
                _ => {}
            }
        }
        if let Some(r) = row.take() {
            if !r.is_empty() {
                rows.push(r);
            }
        }
        if rows.is_empty() {
            return None;
        }
        let n = rows.iter().map(Vec::len).max().unwrap_or(0);
        if n == 0 {
            return None;
        }
        let mut align = vec![Align::None; n];
        for row in &rows {
            for (ci, (_, _, a)) in row.iter().enumerate() {
                if let (Some(a), true) = (a, ci < n) {
                    align[ci] = *a;
                }
            }
        }
        // The header is the first row that names its cells `<th>`. A layout
        // table (the side-by-side screenshots README idiom) has none, and
        // GitHub draws it without a header band — so an empty head here is
        // meaningful, not a missing first row.
        let mut head: Vec<Vec<Inline>> = Vec::new();
        if let Some(head_idx) = rows.iter().position(|r| r.iter().any(|(th, _, _)| *th)) {
            let head_row = rows.remove(head_idx);
            head = head_row
                .iter()
                .map(|(_, text, _)| self.inlines(text.trim()))
                .collect();
            head.resize_with(n, Vec::new);
        }
        let body = rows
            .iter()
            .map(|r| {
                let mut cells: Vec<Vec<Inline>> = r
                    .iter()
                    .map(|(_, text, _)| self.inlines(text.trim()))
                    .collect();
                cells.resize_with(n, Vec::new);
                cells
            })
            .collect();
        Some(Block::Table {
            align,
            head,
            rows: body,
        })
    }

    fn list_block(&self, ordered: bool, inner: &str) -> Option<Block> {
        let tags = html::tags_in(inner);
        let mut items = Vec::new();
        let mut depth = 0usize;
        let mut start = 0usize;
        for (byte, tag, end) in &tags {
            if tag.name != "li" {
                continue;
            }
            match tag.kind {
                TagKind::Open => {
                    if depth == 0 {
                        start = *end;
                    }
                    depth += 1;
                }
                TagKind::Close if depth > 0 => {
                    depth -= 1;
                    if depth == 0 {
                        items.push(ListItem {
                            checked: None,
                            blocks: parse_blocks_with_defs(&inner[start..*byte], &self.defs).blocks,
                        });
                    }
                }
                TagKind::Close => {}
            }
        }
        if items.is_empty() {
            return None;
        }
        Some(Block::List {
            ordered,
            start: 1,
            items,
        })
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
                        let text = para.join("\n");
                        out.push(Block::Paragraph(self.inlines(&text)));
                    }
                    let mut head_cells = split_row(&head_line);
                    head_cells.resize_with(align.len(), Default::default);
                    let head = head_cells
                        .into_iter()
                        .take(align.len())
                        .map(|c| self.inlines(&c))
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
                                .map(|c| self.inlines(&c))
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
                    let text = para.join("\n");
                    return vec![Block::Heading {
                        level,
                        content: self.inlines(&text),
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
        let text = para.join("\n");
        vec![Block::Paragraph(self.inlines(&text))]
    }
}

#[cfg(test)]
pub fn parse_blocks(src: &str) -> Document {
    // A Windows editor may leave a UTF-8 BOM on the first line; it is not
    // content, and without this a document that opens with a heading or fence
    // parsed it as a paragraph ("\u{FEFF}# Title").
    let src = src.strip_prefix('\u{FEFF}').unwrap_or(src);
    let lines: Vec<String> = src.lines().map(expand_leading_tabs).collect();
    let (doc, lead) = parse_lines(lines);
    Document { lead, ..doc }
}

/// Parse a buffer without joining it into one string first. The renderer
/// already has the lines; [`Buffer::text`] would copy the whole document just
/// for the parser to split it again.
pub fn parse_buffer(buf: &crate::core::buffer::Buffer) -> Document {
    let mut lines: Vec<String> = Vec::with_capacity(buf.line_count());
    for i in 0..buf.line_count() {
        lines.push(expand_leading_tabs(buf.line(i)));
    }
    let (doc, lead) = parse_lines(lines);
    Document { lead, ..doc }
}

fn parse_lines(mut lines: Vec<String>) -> (Document, u32) {
    // A buffer can carry the BOM inside its first line.
    if let Some(first) = lines.first_mut() {
        if let Some(rest) = first.strip_prefix('\u{FEFF}') {
            *first = rest.to_string();
        }
    }
    let lead = strip_frontmatter(&mut lines);
    let defs = collect_ref_defs(&lines);
    // Footnote definitions are lifted out before the block parse and printed
    // as one section at the end; the blanked lines keep the line count.
    let items = collect_footnotes(&mut lines, &defs);
    let has_footnotes = !items.is_empty();
    let end = u32::try_from(lines.len()).unwrap_or(u32::MAX);
    // The lines are already split and tab-expanded; joining them only to split
    // them again (and rescanning for definitions) was two full copies of the
    // document on every parse.
    let mut doc = parse_top_lines(lines, defs);
    doc.has_footnotes = has_footnotes;
    if has_footnotes {
        let notes = resolve_footnotes(&mut doc.blocks, items);
        // A definition nobody references prints nothing at all — not even
        // the section rule.
        if !notes.is_empty() {
            doc.blocks.push(Block::FootnoteRule);
            doc.spans.push((end, end));
            for (number, blocks) in notes {
                doc.blocks.push(Block::Footnote { number, blocks });
                doc.spans.push((end, end));
            }
        }
    }
    let lead = u32::try_from(lead).unwrap_or(u32::MAX);
    (doc, lead)
}

fn parse_blocks_with_defs(src: &str, outer: &HashMap<String, String>) -> Document {
    let lines: Vec<String> = src.lines().map(expand_leading_tabs).collect();
    // Nested content (quotes/lists) inherits the outer definitions; local
    // definitions win and never render.
    let mut defs = outer.clone();
    for (label, href) in collect_ref_defs(&lines) {
        defs.entry(label).or_insert(href);
    }
    parse_top_lines(lines, defs)
}

fn parse_top_lines(lines: Vec<String>, defs: HashMap<String, String>) -> Document {
    let mut p = Parser {
        lines,
        pos: 0,
        defs,
    };
    let (blocks, spans) = p.parse_top();
    Document {
        blocks,
        spans,
        lead: 0,
        has_footnotes: false,
        defs: p.defs,
    }
}

/// T2 · The edited-block splice. `old` is the document the pane was planned
/// from; `buf` is the text after the edits, and `edits` is the buffer's edit
/// log in the coordinates of that text (see [`EditSpan`] for the exact
/// shape). Returns the document a whole re-parse would produce, assembled
/// from the old blocks wherever the edit left them verbatim — or `None`
/// when the edit cannot be localized safely, which the caller answers with
/// a full re-parse.
///
/// Only the bookkeeping needs the block list: the line ranges in
/// [`Document::spans`] name the run the edit landed in, the run is widened
/// to blank-line boundaries (the one rule that lets a slice re-parse stand
/// in for a whole-document one — every block association the parser makes
/// across a non-blank line is inside the widened run), the slice is
/// re-parsed with the full document's reference definitions so inline links
/// resolve exactly as a whole parse would resolve them, and blocks and
/// spans are spliced back. An unclosed fence, a list, a table: each is one
/// block with one span, so the run widens through/around them without
/// special cases.
///
/// Divergence from a whole re-parse is possible in one corner: an edit that
/// removes lines which carried reference definitions used by links in the
/// *kept* blocks. The guard below sends such edits (any removal with a
/// kept-region definition or a resolved link in play) to the full parse
/// rather than splice stale inline content — the honesty the JSON patch
/// already has.
/// T2 · The block-run a splice replaced: (blocks kept above, old run
/// length, new run length). The plan patch uses it to skip re-deriving the
/// run by comparing whole documents block by block.
pub type MdSpliceRun = (usize, usize, usize);

pub fn patch_document(
    mut old: Document,
    buf: &crate::core::buffer::Buffer,
    edits: &[crate::core::buffer::EditSpan],
) -> (Document, Option<MdSpliceRun>) {
    let n = buf.line_count();
    // The affected lines, with one merged-boundary row either side of each
    // edit — the same ±1 rule `touched_extent` prices: a deletion joins two
    // lines at `e.start`, and that joined line is a line that changed.
    let lo_b = edits
        .iter()
        .map(|e| e.start.saturating_sub(1))
        .min()
        .unwrap_or(0);
    let hi_b = edits
        .iter()
        .map(|e| e.start.saturating_add(e.inserted).saturating_add(1))
        .max()
        .unwrap_or(0)
        .min(n);
    if lo_b >= hi_b {
        return (old, None);
    }
    // Footnotes are whole-document state: definitions are lifted out before
    // the parse and the numbering follows the text's reference order, so a
    // re-parse of one slice can rebuild neither. Any text that carries a
    // definition goes whole, and an edit that could introduce the syntax is
    // refused by the same check on the lines it touched.
    if old.has_footnotes || (lo_b..hi_b).any(|i| buf.line(i).contains("[^")) {
        return (old, None);
    }
    let lead = old.lead as usize;
    // Frontmatter lines belong to the lead and no block does; an edit
    // touching them does not localize.
    if lo_b < lead {
        return (old, None);
    }
    // Rebase every block's span into the current text's coordinates: the
    // splice decides kept / re-parsed on these, so a deletion that pulled
    // `para two` up beside `para one` is seen where it now sits. The spans
    // vec is the caller's document's own, so this is an in-place pass and
    // the document always comes back, whatever the splice decided.
    // Each edit replaces old lines `[start, start+removed)` (in the text as
    // the edits before it left it) with `inserted` lines; a boundary landing
    // inside a removed run clamps to the replacement's edge.
    let mut cur = std::mem::take(&mut old.spans);
    for e in edits {
        let st = (e.start.saturating_sub(lead)) as i64;
        let r_end = st + e.removed as i64;
        let r_ins = st + e.inserted as i64;
        for span in cur.iter_mut() {
            let b0 = span.0 as i64;
            let b1 = span.1 as i64;
            let n0 = if b0 > r_end {
                (r_ins + (b0 - r_end)) as u32
            } else if b0 > st {
                st as u32
            } else {
                b0 as u32
            };
            let n1 = if b1 >= r_end {
                (r_ins + (b1 - r_end)) as u32
            } else if b1 > st {
                r_ins as u32
            } else {
                b1 as u32
            };
            *span = (n0, n1);
        }
    }
    let lo_d = lo_b - lead;
    let hi_d = hi_b - lead;
    // The neighborhood: the affected lines widened across non-blank lines
    // until a blank or the document's edge. Every block association the
    // parser makes across a non-blank line (a paragraph's continuation, a
    // list item's lazy line) lies inside it, and the lines just outside are
    // blank - the one rule that lets a slice re-parse stand in for a whole
    // parse of the same middle. Document coordinates throughout; the buffer
    // lines are one `lead` below the doc line index.
    let mut b_lo = lo_d;
    while b_lo > 0 && !is_blank(buf.line(b_lo + lead - 1)) {
        b_lo -= 1;
    }
    let mut b_hi = hi_d;
    while b_hi < n - lead && !is_blank(buf.line(b_hi + lead)) {
        b_hi += 1;
    }
    // The run: every block whose span meets the neighborhood. An unclosed
    // fence's giant block and a list's span have whole-parse ends that reach
    // past the blank the neighborhood stops at, so meeting it puts their
    // whole extent in the run.
    let mut first = None;
    let mut last = 0usize;
    for (i, &(s0, s1)) in cur.iter().enumerate() {
        if (s1 as usize) > b_lo && (s0 as usize) < b_hi {
            if first.is_none() {
                first = Some(i);
            }
            last = i;
        }
    }
    // The slice sits between `s_lo` and `s_hi`, in document coordinates.
    // With a run, its bounds are the neighborhood's widened to the first /
    // last run block's own span edges - a block's span may cross blank
    // lines (a fenced or indented code block's interior), so a run block
    // that reaches outside the neighborhood pulls the slice with it. With
    // no run, the neighborhood itself: the splice resamples the blank gap,
    // where blocks may now appear that no span ever numbered.
    let s_lo = match first {
        Some(_) => b_lo.min(cur[first.unwrap_or(0)].0 as usize),
        None => b_lo,
    };
    let s_hi = match first {
        Some(_) => b_hi.max(cur[last].1 as usize),
        None => b_hi,
    };
    // A fence standing open at the slice's lower boundary runs the whole
    // parse's block past the blank behind it: stretch the slice on to its
    // close line (or to the text's end when it never closes), then look
    // again. The close is found within one pass, so this bound never trips.
    let mut s_hi = s_hi;
    for _ in 0..4 {
        let mut open: Option<Fence> = None;
        for i in 0..(s_hi + lead).min(n) {
            let line = expand_leading_tabs(buf.line(i));
            match &open {
                Some(f) => {
                    if is_close_fence(&line, f) {
                        open = None;
                    }
                }
                None => {
                    if let Some(f) = open_fence(&line) {
                        open = Some(f);
                    }
                }
            }
        }
        let Some(carried) = open else {
            break;
        };
        let mut i = (s_hi + lead).min(n);
        while i < n && !is_close_fence(&expand_leading_tabs(buf.line(i)), &carried) {
            i += 1;
        }
        // The close line is the block's own: a whole parse consumes it, so
        // the slice ends one line past it.
        s_hi = (i + 1).min(n) - lead;
    }
    // Which old blocks the splice keeps: everything above the slice, and
    // everything that starts at or after its end. With a run these are the
    // head and the tail; with none, they split at the first block that was
    // numbered below the blank gap.
    let below_start = cur
        .iter()
        .position(|&(s0, _)| (s0 as usize) >= s_hi)
        .unwrap_or(cur.len());
    let above_end = first.unwrap_or(below_start);
    // A definition line inside the affected range: the splice would re-parse
    // it in isolation and the kept blocks' labels may resolve differently.
    for i in lo_b..hi_b {
        if is_ref_def(&expand_leading_tabs(buf.line(i))) {
            old.spans = cur;
            return (old, None);
        }
    }
    // Whether the edit could change the reference map at all. Line-structural
    // edits (a removal shuffles def lines and blank gaps) can, and so can a
    // byte deletion (its removed bytes may have been the head of a def). A
    // pure insertion inside an unrewritten line count cannot: the pre-edit
    // line is the current one with the inserted run spliced out (`at` is the
    // same offset before and after), and only a def line being rewritten
    // matters — the current shape was refused in range above, the pre-edit
    // shape here.
    let mut map_can_change = edits
        .iter()
        .any(|e| e.removed != e.inserted || e.removed_bytes() > 0);
    if !map_can_change {
        for e in edits.iter() {
            if e.inserted_bytes() == 0 {
                continue;
            }
            let i = e.start;
            if i >= buf.line_count() {
                map_can_change = true;
                break;
            }
            let line = buf.line(i);
            let cut = e.at.saturating_sub(buf.line_start_byte(i));
            if cut > line.len() {
                map_can_change = true;
                break;
            }
            let mut old_line = String::with_capacity(line.len());
            old_line.push_str(&line[..cut]);
            if cut + e.inserted_bytes() <= line.len() {
                old_line.push_str(&line[cut + e.inserted_bytes()..]);
            }
            if is_ref_def(&expand_leading_tabs(&old_line)) {
                map_can_change = true;
                break;
            }
        }
    }
    // A removal next to kept definitions or kept resolved links: the removed
    // lines are inside the slice, so a re-parse of it sees the same text -
    // but the kept blocks were resolved against the old map, and a label the
    // removed lines defined (or changed) would now render differently in a
    // whole parse. Ref defs live in blank gaps, so either kept-region
    // definitions or a resolved link in a kept block is enough to go whole.
    if map_can_change {
        let kept_defs = (0..s_lo + lead)
            .any(|i| i < n && is_ref_def(&expand_leading_tabs(buf.line(i))))
            || ((s_hi + lead)..n).any(|i| is_ref_def(&expand_leading_tabs(buf.line(i))));
        let kept_links = old
            .blocks
            .iter()
            .enumerate()
            .any(|(i, b)| (i < above_end || i >= below_start) && block_carries_link(b));
        if kept_defs || kept_links {
            old.spans = cur;
            return (old, None);
        }
    }
    // The slice, from the current text, tab-expanded like every parse path.
    let slice: Vec<String> = (s_lo + lead..(s_hi + lead).min(n))
        .map(|i| expand_leading_tabs(buf.line(i)))
        .collect();
    // Full-document definitions: collected over the whole current text, in
    // order, so a slice label resolves to the same href a whole parse gives
    // it (first one wins there and here alike). The scan is a whole pass
    // over the text, so an edit that provably kept the map reuses the old
    // parse's instead: a keystroke prices no document walk.
    let defs = if !map_can_change {
        old.defs.clone()
    } else {
        let mut all_lines: Vec<String> = (0..n).map(|i| expand_leading_tabs(buf.line(i))).collect();
        if let Some(first_line) = all_lines.first_mut() {
            if let Some(rest) = first_line.strip_prefix('\u{FEFF}') {
                *first_line = rest.to_string();
            }
        }
        collect_ref_defs(&all_lines)
    };
    let mut p = Parser {
        lines: slice,
        pos: 0,
        defs,
    };
    let (blocks, spans) = p.parse_top();
    // Splice in place: kept head, re-parsed middle, kept tail. No copy of
    // the document's blocks on either side — the splices move the kept ones
    // inside their own `Vec`s, and the tail's spans arrive rebased already.
    old.blocks.splice(above_end..below_start, blocks);
    cur.splice(
        above_end..below_start,
        spans
            .iter()
            .map(|&(s0, s1)| (s0 + s_lo as u32, s1 + s_lo as u32)),
    );
    old.spans = cur;
    if map_can_change {
        old.defs = p.defs;
    }
    (old, Some((above_end, below_start - above_end, spans.len())))
}

/// T2 · Whether a block's inline content carries any resolved link or
/// image — the shape the guard above keys on. Only asked when a removal is
/// in play.
fn block_carries_link(b: &Block) -> bool {
    match b {
        Block::Heading { content, .. } | Block::Paragraph(content) => {
            content.iter().any(inline_carries_link)
        }
        Block::Code { .. } | Block::Rule => false,
        Block::Quote(blocks) | Block::Html { blocks, .. } | Block::Alert { blocks, .. } => {
            blocks.iter().any(block_carries_link)
        }
        Block::Details { summary, blocks } => {
            summary.iter().any(inline_carries_link) || blocks.iter().any(block_carries_link)
        }
        Block::List { items, .. } => items
            .iter()
            .any(|item| item.blocks.iter().any(block_carries_link)),
        Block::Table { head, rows, .. } => head
            .iter()
            .chain(rows.iter().flatten())
            .flatten()
            .any(inline_carries_link),
        Block::Footnote { blocks, .. } => blocks.iter().any(block_carries_link),
        Block::FootnoteRule => false,
    }
}

fn inline_carries_link(i: &Inline) -> bool {
    match i {
        Inline::Link { .. } | Inline::Image { .. } => true,
        Inline::Text(_)
        | Inline::Code(_)
        | Inline::FootnoteRef { .. }
        | Inline::SoftBreak
        | Inline::HardBreak => false,
        Inline::Emph(v)
        | Inline::Strong(v)
        | Inline::Strike(v)
        | Inline::Underline(v)
        | Inline::Highlight(v)
        | Inline::Sup(v)
        | Inline::Sub(v) => v.iter().any(inline_carries_link),
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
    fn a_bom_does_not_hide_the_first_block() {
        let d = parse_blocks("\u{FEFF}# Title\n\n```rust\nlet x = 1;\n```");
        assert!(
            matches!(d.blocks[0], Block::Heading { level: 1, .. }),
            "BOM broke the heading: {d:?}"
        );
        assert!(
            matches!(d.blocks[1], Block::Code { .. }),
            "BOM broke the fence: {d:?}"
        );
    }

    #[test]
    fn parsing_a_buffer_matches_parsing_its_text() {
        let src =
            "\u{FEFF}# Title\n\nA **para** with `code`.\n\n- [x] task\n\n```rs\nfn f() {}\n```";
        let buf = crate::core::buffer::Buffer::from_text(src);
        assert_eq!(parse_blocks(src), parse_buffer(&buf));
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
    fn table_escapes_reach_the_inline_parser_intact() {
        // `\|` is cell content; every other escape is the inline parser's job.
        // Eating the backslash here used to turn `\*\*x\*\*` into bold text.
        let d = parse_blocks("| a \\*\\*x\\*\\* | b \\| c |\n|---|---|");
        match &d.blocks[0] {
            Block::Table { head, .. } => {
                assert_eq!(head[0], vec![Inline::Text("a **x**".into())]);
                assert_eq!(head[1], vec![Inline::Text("b | c".into())]);
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn html_containers_render_their_content() {
        // A container tag is structure, not a reason to lose the paragraph
        // inside it: GitHub renders the text and drops the tags.
        let d = parse_blocks("<div>\nkept\n</div>\n\nafter");
        assert_eq!(d.blocks.len(), 2, "content lost: {d:?}");
        assert!(
            matches!(&d.blocks[0], Block::Html { blocks, .. }
                if matches!(&blocks[0], Block::Paragraph(v)
                    if v == &[Inline::Text("kept".into())])),
            "the container's content is missing: {d:?}"
        );
        assert!(matches!(&d.blocks[1], Block::Paragraph(_)));
    }

    #[test]
    fn alignment_survives_as_a_container_property() {
        let d = parse_blocks("<div align=center>\n![a](a.png)\n</div>");
        match &d.blocks[0] {
            Block::Html { align, blocks } => {
                assert_eq!(*align, Some(Align::Center));
                assert!(matches!(&blocks[0], Block::Paragraph(_)));
            }
            other => panic!("expected an aligned container, got {other:?}"),
        }
        // `<center>` is the old spelling of the same thing.
        let d = parse_blocks("<center>hi</center>");
        assert!(matches!(
            &d.blocks[0],
            Block::Html {
                align: Some(Align::Center),
                ..
            }
        ));
        // A heading keeps its size and gains the alignment.
        let d = parse_blocks("<h1 align=\"center\">Title</h1>");
        match &d.blocks[0] {
            Block::Html { align, blocks } => {
                assert_eq!(*align, Some(Align::Center));
                assert!(matches!(
                    &blocks[0],
                    Block::Heading { level: 1, content } if content == &[Inline::Text("Title".into())]
                ));
            }
            other => panic!("expected an aligned heading, got {other:?}"),
        }
    }

    #[test]
    fn details_render_expanded_with_their_summary() {
        let d = parse_blocks(
            "<details>\n<summary>More</summary>\n\nHidden **body**\n\n</details>\n\nafter",
        );
        match &d.blocks[0] {
            Block::Details { summary, blocks } => {
                assert_eq!(summary, &[Inline::Text("More".into())]);
                assert!(
                    matches!(&blocks[0], Block::Paragraph(v)
                        if v.iter().any(|n| matches!(n, Inline::Strong(_)))),
                    "body lost: {blocks:?}"
                );
            }
            other => panic!("expected details, got {other:?}"),
        }
        assert_eq!(d.blocks.len(), 2);
        // No summary: a default label, not a blank bar.
        let d = parse_blocks("<details>\nbody\n</details>");
        assert!(matches!(
            &d.blocks[0],
            Block::Details { summary, .. } if !summary.is_empty()
        ));
    }

    #[test]
    fn html_tables_map_onto_the_markdown_table_shape() {
        let d = parse_blocks(
            "<table>\n<tr><th align=\"left\">Name</th><th align=\"right\">Size</th></tr>\n\
             <tr><td>a</td><td>1</td></tr>\n</table>",
        );
        match &d.blocks[0] {
            Block::Table { align, head, rows } => {
                assert_eq!(align, &[Align::Left, Align::Right]);
                assert_eq!(head[0], vec![Inline::Text("Name".into())]);
                assert_eq!(rows[0][1], vec![Inline::Text("1".into())]);
            }
            other => panic!("expected a table, got {other:?}"),
        }
    }

    #[test]
    fn html_lists_become_real_lists() {
        let d = parse_blocks("<ul>\n<li>one</li>\n<li>two</li>\n</ul>");
        match &d.blocks[0] {
            Block::List {
                ordered: false,
                items,
                ..
            } => {
                assert_eq!(items.len(), 2);
                assert!(matches!(&items[0].blocks[0], Block::Paragraph(v)
                    if v == &[Inline::Text("one".into())]));
            }
            other => panic!("expected a list, got {other:?}"),
        }
    }

    #[test]
    fn alerts_replace_the_anonymous_quote_bar() {
        let d = parse_blocks("> [!WARNING]\n> hot\n\nplain\n\n> quote");
        match &d.blocks[0] {
            Block::Alert { kind, blocks } => {
                assert_eq!(*kind, crate::parse::markdown::ast::AlertKind::Warning);
                assert!(matches!(&blocks[0], Block::Paragraph(_)));
            }
            other => panic!("expected an alert, got {other:?}"),
        }
        assert!(matches!(&d.blocks[2], Block::Quote(_)));
    }

    #[test]
    fn a_blank_line_keeps_two_quotes_apart() {
        // The blank line between the quotes used to be folded into the first,
        // so a second alert rendered as literal `[!WARNING]` text inside the
        // first instead of its own callout.
        let d = parse_blocks("> [!NOTE]\n> first\n\n> [!WARNING]\n> second");
        assert_eq!(d.blocks.len(), 2, "quotes merged: {d:?}");
        assert!(matches!(
            &d.blocks[0],
            Block::Alert {
                kind: AlertKind::Note,
                ..
            }
        ));
        assert!(matches!(
            &d.blocks[1],
            Block::Alert {
                kind: AlertKind::Warning,
                ..
            }
        ));
    }

    #[test]
    fn an_html_layout_table_has_no_header_row() {
        // A screenshot table names no `<th>`; inventing a header out of its
        // first image row styled the images as a heading band and dropped
        // them, so the header is empty instead.
        let d = parse_blocks("<table>\n<tr><td>a</td><td>b</td></tr>\n</table>");
        match &d.blocks[0] {
            Block::Table { head, rows, .. } => {
                assert!(head.is_empty(), "invented a header row: {d:?}");
                assert_eq!(rows.len(), 1);
            }
            other => panic!("expected a table, got {other:?}"),
        }
        // A `<th>` row is still the header.
        let d = parse_blocks("<table>\n<tr><th>H</th></tr>\n<tr><td>a</td></tr>\n</table>");
        assert!(
            matches!(&d.blocks[0], Block::Table { head, .. } if head.len() == 1),
            "the <th> row stopped being the header: {d:?}"
        );
    }

    #[test]
    fn a_picture_renders_its_fallback_image() {
        // `<picture>` with a dark-mode `<source>` is how READMEs do a theme
        // logo. Its four-space-indented `<img>` used to parse as an indented
        // code block, so the preview showed the tag as source text.
        let d = parse_blocks(
            "<p align=\"center\">\n  <picture>\n    <source media=\"(prefers-color-scheme: dark)\" srcset=\"d.png\">\n    <img src=\"l.png\" width=\"180\" alt=\"Logo\">\n  </picture>\n</p>",
        );
        let text = format!("{d:?}");
        assert!(!text.contains("Block::Code"), "picture became code: {text}");
        assert!(
            text.contains("l.png"),
            "the fallback image is missing: {text}"
        );
        assert!(!text.contains("d.png"), "the dark source leaked in: {text}");
    }

    #[test]
    fn dangerous_html_and_comments_are_dropped_with_their_content() {
        let d = parse_blocks("a\n\n<script>\nalert(1)\n</script>\n\n<!-- secret -->\n\nafter");
        let text = format!("{d:?}");
        assert!(!text.contains("alert"), "script body rendered: {text}");
        assert!(!text.contains("secret"), "comment rendered: {text}");
        assert_eq!(d.blocks.len(), 2, "blocks: {d:?}");
    }

    /// The body blocks of the note numbered `number`.
    fn note(d: &Document, number: usize) -> &[Block] {
        let i = d
            .footnote_block(number)
            .unwrap_or_else(|| panic!("note {number} is missing: {:#?}", d.blocks));
        match &d.blocks[i] {
            Block::Footnote { blocks, .. } => blocks,
            other => panic!("block {i} is not a note: {other:?}"),
        }
    }

    #[test]
    fn footnotes_collect_into_one_numbered_section() {
        let d = parse_blocks(
            "First[^b] then[^a], and again[^b].\n\n[^a]: The A note.\n[^b]: The B note.\n",
        );
        // The definitions do not render where they were written: the prose,
        // the section rule, then two notes.
        assert_eq!(d.blocks.len(), 4, "{:#?}", d.blocks);
        assert!(matches!(d.blocks[1], Block::FootnoteRule));
        // The list follows the text's reference order, not the order the
        // definitions were written in.
        assert!(matches!(
            &note(&d, 1)[0],
            Block::Paragraph(v) if v == &vec![Inline::Text("The B note.".into())]
        ));
        assert!(matches!(
            &note(&d, 2)[0],
            Block::Paragraph(v) if v == &vec![Inline::Text("The A note.".into())]
        ));
        // The same reference twice keeps its number, and the next new label
        // takes the next one.
        let numbers: Vec<usize> = match &d.blocks[0] {
            Block::Paragraph(inlines) => inlines
                .iter()
                .filter_map(|i| match i {
                    Inline::FootnoteRef { number, .. } => Some(*number),
                    _ => None,
                })
                .collect(),
            other => panic!("expected a paragraph, got {other:?}"),
        };
        assert_eq!(numbers, vec![1, 2, 1]);
        // The jump targets the pane uses: the first reference's block, and
        // each note's own block.
        assert_eq!(d.footnote_reference_block(1), Some(0));
        assert_eq!(d.footnote_reference_block(2), Some(0));
    }

    #[test]
    fn a_footnote_can_hold_several_blocks() {
        let d = parse_blocks(
            "Text[^n].\n\n[^n]: First paragraph.\n\n    Second paragraph.\n\n    - and a list\n",
        );
        let blocks = note(&d, 1);
        assert_eq!(blocks.len(), 3, "{blocks:#?}");
        assert!(matches!(blocks[0], Block::Paragraph(_)));
        assert!(matches!(blocks[1], Block::Paragraph(_)));
        assert!(matches!(blocks[2], Block::List { .. }));
    }

    #[test]
    fn a_note_can_reference_another_note() {
        let d = parse_blocks("Text[^a].\n\n[^a]: See also[^b].\n[^b]: The second note.\n");
        assert!(d.footnote_block(1).is_some() && d.footnote_block(2).is_some());
        // The reference inside the first note takes the number after it.
        assert!(matches!(
            &note(&d, 1)[0],
            Block::Paragraph(v)
                if matches!(&v[1], Inline::FootnoteRef { number: 2, .. })
        ));
        // The note carrying that reference is a top-level block in its own
        // right: the arrow in note 2 goes back to note 1's block.
        assert_eq!(d.footnote_reference_block(2), d.footnote_block(1));
    }

    #[test]
    fn unused_definitions_vanish_and_undefined_references_stay_literal() {
        let d = parse_blocks("Literal[^missing].\n\n[^unused]: No reference.\n");
        assert_eq!(d.blocks.len(), 1, "{:#?}", d.blocks);
        assert!(
            d.has_footnotes,
            "the definition still has to bar the splice"
        );
        match &d.blocks[0] {
            Block::Paragraph(inlines) => assert!(matches!(
                &inlines[1],
                Inline::FootnoteRef { label, number: 0 } if label == "missing"
            )),
            other => panic!("expected a paragraph, got {other:?}"),
        }
        // A definition is not a link target either, so `[^x]` never resolved
        // to the definition's dest the way `[x]` would.
        assert!(!d.defs.contains_key("^unused"));
    }

    #[test]
    fn footnote_syntax_inside_code_is_code() {
        let d = parse_blocks("See[^x].\n\n```\n[^x]: code\n```\n");
        assert_eq!(d.blocks.len(), 2, "{:#?}", d.blocks);
        assert!(!d.has_footnotes);
        assert!(matches!(
            &d.blocks[1],
            Block::Code { text, .. } if text.contains("[^x]:")
        ));
    }

    #[test]
    fn the_first_definition_of_a_label_wins() {
        let d = parse_blocks("See[^d].\n\n[^d]: first\n[^d]: second\n");
        assert_eq!(d.footnote_block(2), None, "a duplicate printed twice");
        assert!(matches!(
            &note(&d, 1)[0],
            Block::Paragraph(inlines) if inlines == &vec![Inline::Text("first".into())]
        ));
        // Labels match case-insensitively, like link references.
        let d = parse_blocks("Note[^Mark]\n\n[^mark]: matched.\n");
        assert!(d.footnote_block(1).is_some());
    }

    #[test]
    fn a_footnote_edit_is_never_spliced() {
        use crate::core::buffer::Buffer;
        // The section is built from the whole document, so a slice re-parse
        // could not rebuild it: the splice must decline.
        let mut buf = Buffer::from_text("text[^1]\n\n[^1]: note\n\ntail\n");
        let old = parse_buffer(&buf);
        edit_line(&mut buf, 4, " changed");
        let edits = buf.take_edits().expect("the edit logged itself");
        let (_, splice) = patch_document(old, &buf, &edits);
        assert!(splice.is_none(), "a footnote document must go whole");
        // Introducing the syntax into a document that had none is refused on
        // the lines the edit touched.
        let mut buf = Buffer::from_text("plain text\n");
        let old = parse_buffer(&buf);
        edit_line(&mut buf, 0, " [^1]");
        let edits = buf.take_edits().expect("the edit logged itself");
        let (_, splice) = patch_document(old, &buf, &edits);
        assert!(splice.is_none(), "a new reference must go whole");
    }

    #[test]
    fn frontmatter_is_stripped() {
        let d = parse_blocks("---\ntitle: hi\n---\n\n# Real\n");
        assert_eq!(d.blocks.len(), 1, "frontmatter must not render: {d:?}");
        assert!(matches!(d.blocks[0], Block::Heading { level: 1, .. }));
        // TOML frontmatter too.
        let d = parse_blocks("+++\ntitle = \"hi\"\n+++\n\n# Real\n");
        assert_eq!(d.blocks.len(), 1, "frontmatter must not render: {d:?}");
        // A leading comment inside the block does not stop it from looking
        // like frontmatter.
        let d = parse_blocks("---\n# a comment\ntitle: hi\n---\n\n# Real\n");
        assert_eq!(d.blocks.len(), 1, "frontmatter must not render: {d:?}");
        // No closing marker: a lone --- still parses as a rule.
        let d = parse_blocks("---\n\ntext");
        assert!(matches!(d.blocks[0], Block::Rule));
    }

    #[test]
    fn a_document_that_starts_with_a_rule_keeps_its_content() {
        // `---` … `---` with prose between them is two thematic breaks, not
        // frontmatter. Stripping it would silently delete the paragraph.
        let d = parse_blocks("---\n\nA paragraph.\n\n---\n\nAfter");
        assert_eq!(d.blocks.len(), 4, "content lost: {d:?}");
        assert!(matches!(d.blocks[0], Block::Rule));
        assert!(matches!(&d.blocks[1], Block::Paragraph(v) if !v.is_empty()));
        assert!(matches!(d.blocks[2], Block::Rule));
        assert!(matches!(&d.blocks[3], Block::Paragraph(v) if !v.is_empty()));
    }

    #[test]
    fn indented_code_renders() {
        let d = parse_blocks("para\n\n    code line\n    second\n\ntext");
        assert_eq!(d.blocks.len(), 3);
        match &d.blocks[1] {
            Block::Code { lang, text } => {
                assert_eq!(lang, &None);
                assert_eq!(text, "code line\nsecond");
            }
            other => panic!("expected indented code, got {other:?}"),
        }
    }

    #[test]
    fn reference_links_resolve_and_defs_do_not_render() {
        let d = parse_blocks("[hi][a] and [bye][]\n\n[a]: https://a.com\n[bye]: https://b.com\n");
        assert_eq!(d.blocks.len(), 1, "defs must not render: {d:?}");
        match &d.blocks[0] {
            Block::Paragraph(inlines) => {
                let hrefs: Vec<&str> = inlines
                    .iter()
                    .filter_map(|n| match n {
                        Inline::Link { href, .. } => Some(href.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(hrefs, vec!["https://a.com", "https://b.com"]);
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
        // Shortcut form + nested inheritance (definition after a quote).
        let d = parse_blocks("> [q]\n\n[q]: https://q.com\n");
        match &d.blocks[0] {
            Block::Quote(inner) => assert!(
                matches!(&inner[0], Block::Paragraph(v) if v.iter().any(|n| matches!(n, Inline::Link { href, .. } if href == "https://q.com"))),
                "quote must inherit outer defs: {inner:?}"
            ),
            other => panic!("expected quote, got {other:?}"),
        }
        // Undefined references stay literal text.
        let d = parse_blocks("[nope]\n");
        match &d.blocks[0] {
            Block::Paragraph(v) => assert!(
                v.iter().all(|n| !matches!(n, Inline::Link { .. })),
                "undefined ref must not link: {v:?}"
            ),
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn a_definition_inside_code_or_html_does_not_resolve() {
        // The code sample must stay code, and `[x]` after it must stay text.
        let d = parse_blocks("```\n[x]: https://example.com\n```\n\n[x]\n");
        assert!(matches!(d.blocks[0], Block::Code { .. }));
        match &d.blocks[1] {
            Block::Paragraph(v) => assert!(
                v.iter().all(|n| !matches!(n, Inline::Link { .. })),
                "a code sample defined a link: {v:?}"
            ),
            other => panic!("expected paragraph, got {other:?}"),
        }
        let d = parse_blocks("<div>\n[y]: https://example.com\n</div>\n\n[y]\n");
        assert!(
            d.blocks.iter().all(|b| match b {
                Block::Paragraph(v) => v.iter().all(|n| !matches!(n, Inline::Link { .. })),
                _ => true,
            }),
            "an HTML block defined a link: {d:?}"
        );
    }

    // ---- T2 · The edited-block splice -----------------------------------

    use crate::core::buffer::{Buffer, Pos};

    /// The splice must equal a whole re-parse — blocks *and* the span
    /// bookkeeping, since `Document`'s equality compares both.
    fn assert_splice_equals_whole(before: &str, edit: impl FnOnce(&mut Buffer)) {
        let mut buf = Buffer::from_text(before);
        let old = parse_buffer(&buf);
        edit(&mut buf);
        let edits = buf.take_edits().expect("the edit logged itself");
        let whole = parse_buffer(&buf);
        let (spliced, splice) = patch_document(old, &buf, &edits);
        assert!(splice.is_some(), "the edit must localize: {before:?}");
        assert_eq!(spliced, whole, "splice diverged from a whole re-parse");
    }

    fn edit_line(buf: &mut Buffer, line: usize, suffix: &str) {
        let len = buf.line(line).len();
        buf.insert(Pos { line, col: len }, suffix);
    }

    #[test]
    fn t2_an_edit_inside_a_paragraph_splices_to_the_whole_parse() {
        assert_splice_equals_whole(
            "# Title\n\nfirst paragraph line\nsecond paragraph line\n\n## Next\n\nother words\n",
            |buf| edit_line(buf, 2, " plus a tail"),
        );
    }

    #[test]
    fn t2_a_fence_opened_to_eof_localizes_through_its_end() {
        // The whole-parse gives the unclosed fence one block that swallows
        // the rest of the document; the splice must reproduce it.
        assert_splice_equals_whole("para one\n\npara two\n\ntail body\n", |buf| {
            buf.insert(Pos { line: 4, col: 0 }, "```rs\nfn f() {}\n");
        });
    }

    #[test]
    fn t2_deleting_the_blank_line_between_two_paragraphs_merges_exactly() {
        assert_splice_equals_whole("para one\n\npara two\n\nlast\n", |buf| {
            buf.delete(Pos { line: 1, col: 0 }, Pos { line: 2, col: 0 });
        });
    }

    #[test]
    fn t2_a_table_row_edit_stays_whole() {
        assert_splice_equals_whole(
            "heading\n\n| a | b |\n| --- | ---: |\n| 1 | 2 |\n| 3 | 4 |\n\ntail\n",
            |buf| edit_line(buf, 4, " | 5"),
        );
    }

    #[test]
    fn t2_a_nested_list_item_edit_stays_whole() {
        assert_splice_equals_whole(
            "intro\n\n- one\n- two\n  - deeper\n- three\n\nover\n",
            |buf| edit_line(buf, 4, " words"),
        );
    }

    #[test]
    fn t2_a_lazy_quote_continuation_stays_whole() {
        assert_splice_equals_whole(" > quoted\ncontinuation\n\nafter\n", |buf| {
            edit_line(buf, 1, " more")
        });
    }

    #[test]
    fn t2_the_run_widens_across_a_non_blank_boundary() {
        // Two adjacent paragraphs (no blank between) that a heading
        // interrupts? A paragraph continues until structure: editing inside
        // the created run must widen past the paragraph close, which the
        // ATX heading above provides. Adjacent blocks of different kinds.
        assert_splice_equals_whole("# Head\npara directly under it\n\nnext para\n", |buf| {
            edit_line(buf, 1, " tail")
        });
    }

    #[test]
    fn t2_an_edit_on_the_first_and_last_line_stays_whole() {
        assert_splice_equals_whole("first\n\nlast", |buf| {
            edit_line(buf, 0, " start");
            let n = buf.line_count() - 1;
            let len = buf.line(n).len();
            buf.insert(Pos { line: n, col: len }, " end");
        });
    }

    #[test]
    fn t2_a_blank_gap_edit_localizes_as_a_no_op() {
        // Pure blank-line edits never change which blocks exist in the text
        // around them; the splice re-numbers everything below and equals a
        // whole re-parse.
        assert_splice_equals_whole("para\n\n\nother\n", |buf| {
            buf.insert(Pos { line: 1, col: 0 }, "\n");
        });
    }

    #[test]
    fn t2_edits_a_splice_must_not_claim() {
        // A reference definition inside the affected range.
        let mut buf = Buffer::from_text("para\n\nother\n");
        let old = parse_buffer(&buf);
        buf.insert(Pos { line: 2, col: 0 }, "[x]: https://example.com\n");
        let edits = buf.take_edits().unwrap();
        let (_, spliced) = patch_document(old, &buf, &edits);
        assert!(!spliced.is_some(), "a ref-def edit must not splice");
        // Frontmatter is outside every block and inside the lead.
        let mut buf2 = Buffer::from_text("---\ntitle: x\n---\n\nbody\n");
        let old2 = parse_buffer(&buf2);
        assert_eq!(old2.lead, 3, "frontmatter lifts the block lines");
        edit_line(&mut buf2, 0, "y");
        let edits2 = buf2.take_edits().unwrap();
        let (_, spliced2) = patch_document(old2, &buf2, &edits2);
        assert!(!spliced2.is_some(), "a frontmatter edit must not splice");
    }
}
