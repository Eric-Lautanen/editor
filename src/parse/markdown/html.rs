//! HTML tag scanning shared by the inline and block parsers.
//!
//! GitHub renders a curated subset of HTML in Markdown: structural containers,
//! headings, tables, images and a short list of inline formatting tags. The
//! attributes it honours are a similarly short list, and everything else
//! (`class`, `id`, `style`, event handlers) is dropped. This module reads one
//! tag at a time and keeps exactly those attributes; the callers decide which
//! tag names mean anything.

use super::ast::Align;
use super::decode_entities;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TagKind {
    Open,
    Close,
}

/// The attributes the renderer can act on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Attrs {
    pub href: Option<String>,
    pub src: Option<String>,
    pub alt: Option<String>,
    /// Both dimensions are physical pixels in the source; callers clamp them
    /// to the column.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub align: Option<Align>,
    pub open: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Tag {
    /// Lowercased tag name.
    pub name: String,
    pub kind: TagKind,
    pub self_closing: bool,
    pub attrs: Attrs,
    /// Index just past the closing `>` in the scanned character slice.
    pub end: usize,
}

/// Tags whose content is never rendered: GitHub's sanitizer removes the
/// element and everything inside it, because none of it is a document.
pub(super) fn is_dangerous(name: &str) -> bool {
    matches!(
        name,
        "script"
            | "style"
            | "iframe"
            | "object"
            | "embed"
            | "applet"
            | "form"
            | "input"
            | "button"
            | "textarea"
            | "select"
            | "option"
            | "svg"
            | "math"
            | "template"
            | "noscript"
            | "video"
            | "audio"
            | "canvas"
            | "link"
            | "meta"
            | "base"
    )
}

/// Parse the tag starting at `chars[i]` (which must be `<`), or `None` when
/// the angle bracket holds prose instead of markup (`<not@an>`, `2 < 3`).
pub(super) fn tag_at(chars: &[char], i: usize) -> Option<Tag> {
    if chars.get(i) != Some(&'<') {
        return None;
    }
    let mut j = i + 1;
    let kind = if chars.get(j) == Some(&'/') {
        j += 1;
        TagKind::Close
    } else {
        TagKind::Open
    };
    if !chars.get(j)?.is_ascii_alphabetic() {
        return None;
    }
    let start = j;
    while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '-') {
        j += 1;
    }
    let name: String = chars[start..j]
        .iter()
        .collect::<String>()
        .to_ascii_lowercase();
    if kind == TagKind::Close {
        while matches!(chars.get(j), Some(c) if c.is_whitespace()) {
            j += 1;
        }
        if chars.get(j) != Some(&'>') {
            return None;
        }
        return Some(Tag {
            name,
            kind,
            self_closing: false,
            attrs: Attrs::default(),
            end: j + 1,
        });
    }
    let mut attrs = Attrs::default();
    let mut self_closing = false;
    loop {
        while matches!(chars.get(j), Some(c) if c.is_whitespace()) {
            j += 1;
        }
        match chars.get(j) {
            Some('>') => {
                j += 1;
                break;
            }
            Some('/') => {
                self_closing = true;
                j += 1;
                continue;
            }
            // An attribute name starts with a letter (or a data/aria prefix's
            // colon/underscore after one). Anything else — a stray quote, a
            // control character — means this was not a tag after all.
            Some(c) if c.is_ascii_alphabetic() || *c == '_' || *c == ':' => {}
            _ => return None,
        }
        let ns = j;
        while j < chars.len()
            && (chars[j].is_ascii_alphanumeric() || matches!(chars[j], '-' | '_' | ':'))
        {
            j += 1;
        }
        let attr: String = chars[ns..j].iter().collect::<String>().to_ascii_lowercase();
        while matches!(chars.get(j), Some(c) if c.is_whitespace()) {
            j += 1;
        }
        let mut value = String::new();
        if chars.get(j) == Some(&'=') {
            j += 1;
            while matches!(chars.get(j), Some(c) if c.is_whitespace()) {
                j += 1;
            }
            match chars.get(j) {
                Some(&q) if q == '"' || q == '\'' => {
                    j += 1;
                    while j < chars.len() && chars[j] != q {
                        value.push(chars[j]);
                        j += 1;
                    }
                    if chars.get(j) != Some(&q) {
                        return None;
                    }
                    j += 1;
                }
                Some(_) => {
                    while j < chars.len()
                        && !chars[j].is_whitespace()
                        && chars[j] != '>'
                        && chars[j] != '/'
                    {
                        value.push(chars[j]);
                        j += 1;
                    }
                }
                None => return None,
            }
        }
        set_attr(&mut attrs, &attr, &value);
    }
    Some(Tag {
        name,
        kind: TagKind::Open,
        self_closing,
        attrs,
        end: j,
    })
}

fn set_attr(attrs: &mut Attrs, name: &str, value: &str) {
    let decoded = || decode_entities(value).into_owned();
    match name {
        "href" => attrs.href = Some(decoded()),
        "src" => attrs.src = Some(decoded()),
        "alt" => attrs.alt = Some(decoded()),
        "width" => attrs.width = dimension(value),
        "height" => attrs.height = dimension(value),
        "align" => attrs.align = align_of(value),
        "open" => attrs.open = true,
        _ => {}
    }
}

/// `width="120"`, `width='120px'`, `width=120`: the leading integer is the
/// value; units and a trailing `%` are ignored because the preview clamps to
/// the column anyway.
fn dimension(value: &str) -> Option<u32> {
    let digits: String = value
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|&n| n > 0)
}

fn align_of(value: &str) -> Option<Align> {
    match value.trim().to_ascii_lowercase().as_str() {
        "left" => Some(Align::Left),
        "center" | "middle" => Some(Align::Center),
        "right" => Some(Align::Right),
        _ => None,
    }
}

/// Every tag in `s`, with the byte offset of its `<` and the byte offset just
/// past its `>`. Used by the HTML table and `<details>` readers, which need
/// source positions to slice cell/summary text out.
pub(super) fn tags_in(s: &str) -> Vec<(usize, Tag, usize)> {
    let chars: Vec<char> = s.chars().collect();
    let mut byte_at = Vec::with_capacity(chars.len() + 1);
    let mut b = 0;
    for c in &chars {
        byte_at.push(b);
        b += c.len_utf8();
    }
    byte_at.push(b);
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            if let Some(tag) = tag_at(&chars, i) {
                out.push((byte_at[i], tag.clone(), byte_at[tag.end]));
                i = tag.end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Find the close tag matching an opener named `name`, starting just past the
/// opener (`chars[from..]`). Nested same-name tags are balanced. Returns the
/// inner range and the index just past the close tag.
pub(super) fn find_close(chars: &[char], from: usize, name: &str) -> Option<(usize, usize, usize)> {
    let mut depth = 0usize;
    let mut j = from;
    while j < chars.len() {
        if chars[j] == '<' {
            if let Some(tag) = tag_at(chars, j) {
                if tag.name == name && !tag.self_closing {
                    match tag.kind {
                        TagKind::Close if depth == 0 => return Some((from, j, tag.end)),
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
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(s: &str) -> Option<Tag> {
        tag_at(&s.chars().collect::<Vec<_>>(), 0)
    }

    #[test]
    fn tags_carry_only_the_attributes_the_renderer_uses() {
        let t = tag(r#"<img src="a.png" alt="A &amp; B" width="120" class="x">"#).expect("tag");
        assert_eq!(t.name, "img");
        assert!(!t.self_closing);
        assert_eq!(t.attrs.src.as_deref(), Some("a.png"));
        assert_eq!(t.attrs.alt.as_deref(), Some("A & B"));
        assert_eq!(t.attrs.width, Some(120));
        let t = tag("<div align=center style='x'>").expect("tag");
        assert_eq!(t.attrs.align, Some(Align::Center));
        assert_eq!(t.attrs.width, None);
    }

    #[test]
    fn prose_in_angle_brackets_is_not_a_tag() {
        assert!(tag("<not@an>").is_none());
        assert!(tag("<https://e.com>").is_none());
        assert!(tag("<3 cups>").is_none());
        assert_eq!(tag("</b>").map(|t| t.kind), Some(TagKind::Close));
    }

    #[test]
    fn close_matching_balances_nested_tags() {
        let chars: Vec<char> = "<b>one <b>two</b> three</b>".chars().collect();
        let (s, e, after) = find_close(&chars, 3, "b").expect("close");
        assert_eq!(
            chars[s..e].iter().collect::<String>(),
            "one <b>two</b> three"
        );
        assert_eq!(after, chars.len());
    }

    #[test]
    fn block_tags_are_located_by_byte_offset() {
        let tags = tags_in("<tr><td>a</td><td>b</td></tr>");
        let names: Vec<&str> = tags.iter().map(|(_, t, _)| t.name.as_str()).collect();
        assert_eq!(names, vec!["tr", "td", "td", "td", "td", "tr"]);
        let (start, _, end) = &tags[1];
        assert_eq!(&"<tr><td>a</td><td>b</td></tr>"[*start..*end], "<td>");
    }
}
