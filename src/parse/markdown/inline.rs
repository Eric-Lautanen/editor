//! Inline Markdown: code spans, emphasis/strong, links/images, strike,
//! autolinks, hard breaks. Reference-style links and footnotes are v2; HTML tags
//! are skipped (ignored) per the plan.

use super::ast::Inline;

/// Scan a code span opening at `i` (chars[i] == '`'). Returns the decoded
/// content and the index after the closer, or `None` when unclosed.
fn scan_code(chars: &[char], i: usize) -> Option<(String, usize)> {
    let mut run = 0;
    while chars.get(i + run) == Some(&'`') {
        run += 1;
    }
    let mut j = i + run;
    while j < chars.len() {
        if chars[j] == '`' {
            let mut k = 0;
            while chars.get(j + k) == Some(&'`') {
                k += 1;
            }
            if k == run {
                let mut content: String = chars[i + run..j].iter().collect();
                // Line endings become spaces; one surrounding space pair is stripped.
                content = content.replace('\n', " ");
                if content.len() >= 2
                    && content.starts_with(' ')
                    && content.ends_with(' ')
                    && !content.trim().is_empty()
                {
                    content = content[1..content.len() - 1].to_string();
                }
                return Some((content, j + run));
            }
            j += k;
        } else {
            j += 1;
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkKind {
    Star,
    Under,
    Tilde,
}

enum Out {
    Text(String),
    Node(Inline),
    Mark(MarkKind, usize),
}

struct Opener {
    out_idx: usize,
    kind: MarkKind,
    len: usize,
    can_open: bool,
    can_close: bool,
}

fn is_ws(c: Option<char>) -> bool {
    c.is_none_or(|c| c.is_whitespace())
}

fn is_punct(c: Option<char>) -> bool {
    match c {
        None => false,
        Some(c) => c.is_ascii_punctuation() || (!c.is_alphanumeric() && !c.is_whitespace()),
    }
}

fn push_text(out: &mut Vec<Out>, s: &str) {
    if s.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(Out::Text(t)) => t.push_str(s),
        _ => out.push(Out::Text(s.to_string())),
    }
}

/// Parse one raw (code-span-free) fragment.
fn parse_raw(s: &str) -> Vec<Inline> {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<Out> = Vec::new();
    let mut openers: Vec<Opener> = Vec::new();
    let mut i = 0;

    // Try to match a closer run against the opener stack.
    // Returns leftover closer length that found no opener.
    fn close_run(
        out: &mut Vec<Out>,
        openers: &mut Vec<Opener>,
        kind: MarkKind,
        mut len: usize,
        can_open: bool,
    ) -> usize {
        while len > 0 {
            let pos = openers.iter().rposition(|o| o.kind == kind && o.can_open);
            let Some(p) = pos else { break };
            let (olen, oopen, oclose, oidx) = {
                let o = &openers[p];
                (o.len, o.can_open, o.can_close, o.out_idx)
            };
            // Odd-match rule: a run that can both open and close cannot match
            // an opener it would leave half-consumed.
            if (oclose || can_open) && (olen + len) % 2 == 1 {
                break;
            }
            // Empty emphasis (`****`) is literal text.
            if out.len() == oidx + 1 {
                break;
            }
            let use_len = if olen >= 2 && len >= 2 { 2 } else { 1 };
            let strong = use_len == 2 && kind != MarkKind::Tilde;
            // Remove the opener mark; everything after it is the content.
            out.remove(oidx);
            let children: Vec<Inline> = out
                .drain(oidx..)
                .map(|n| match n {
                    Out::Text(t) => Inline::Text(t),
                    Out::Node(n) => n,
                    Out::Mark(k, l) => Inline::Text(mark_text(k, l)),
                })
                .collect();
            let node = if kind == MarkKind::Tilde {
                Inline::Strike(children)
            } else if strong {
                Inline::Strong(children)
            } else {
                Inline::Emph(children)
            };
            // Interior unmatched openers were drained into literal text above;
            // they cannot participate further (matches CommonMark).
            openers.truncate(p);
            let left = olen - use_len;
            if left > 0 {
                out.push(Out::Mark(kind, left));
                openers.push(Opener {
                    out_idx: out.len() - 1,
                    kind,
                    len: left,
                    can_open: oopen,
                    can_close: oclose,
                });
            }
            out.push(Out::Node(node));
            len -= use_len;
        }
        len
    }

    while i < chars.len() {
        let c = chars[i];
        // Backslash: punctuation escapes and hard breaks.
        if c == '\\' {
            match chars.get(i + 1) {
                Some('\n') => {
                    out.push(Out::Node(Inline::HardBreak));
                    i += 2;
                }
                Some(n) if n.is_ascii_punctuation() => {
                    push_text(&mut out, &n.to_string());
                    i += 2;
                }
                _ => {
                    push_text(&mut out, "\\");
                    i += 1;
                }
            }
            continue;
        }
        // Code spans are opaque nodes: their content never parses, but
        // emphasis around them still matches (unlike pre-split chunks).
        if c == '`' {
            match scan_code(&chars, i) {
                Some((code, next)) => {
                    out.push(Out::Node(Inline::Code(code)));
                    i = next;
                }
                None => {
                    let mut run = 0;
                    while chars.get(i + run) == Some(&'`') {
                        run += 1;
                    }
                    push_text(&mut out, &"`".repeat(run));
                    i += run;
                }
            }
            continue;
        }
        // Potential link / image.
        if c == '[' || (c == '!' && chars.get(i + 1) == Some(&'[')) {
            let img = c == '!';
            let lb = if img { i + 1 } else { i };
            if let Some((inner, after)) = scan_bracket(&chars, lb) {
                if let Some((href, next)) = scan_link_target(&chars, after) {
                    let content = if img {
                        Vec::new()
                    } else {
                        parse_inlines(&inner)
                    };
                    let node = if img {
                        Inline::Image {
                            src: href,
                            alt: inner,
                        }
                    } else {
                        Inline::Link { href, content }
                    };
                    out.push(Out::Node(node));
                    i = next;
                    continue;
                }
            }
            push_text(&mut out, if img { "!" } else { "[" });
            i += 1;
            continue;
        }
        // Autolink / inline HTML / literal `<`.
        if c == '<' {
            if let Some(end) = chars[i..].iter().position(|&x| x == '>') {
                let inner: String = chars[i + 1..i + end].iter().collect();
                if is_autolink(&inner) {
                    let href = inner.clone();
                    out.push(Out::Node(Inline::Link {
                        href: href.clone(),
                        content: vec![Inline::Text(href)],
                    }));
                    i += end + 1;
                    continue;
                }
                if is_html_tag(&inner) {
                    i += end + 1; // Ignored per plan.
                    continue;
                }
            }
            push_text(&mut out, "<");
            i += 1;
            continue;
        }
        // Emphasis runs.
        if c == '*' || c == '_' || c == '~' {
            let kind = if c == '*' {
                MarkKind::Star
            } else if c == '_' {
                MarkKind::Under
            } else {
                MarkKind::Tilde
            };
            let mut len = 0;
            while chars.get(i + len) == Some(&c) {
                len += 1;
            }
            // `~` only strikes in exact pairs; anything else is literal.
            if kind == MarkKind::Tilde && len != 2 {
                push_text(&mut out, &"~".repeat(len));
                i += len;
                continue;
            }
            let before = if i == 0 { None } else { Some(chars[i - 1]) };
            let after = chars.get(i + len).copied();
            let (bw, bp) = (is_ws(before), is_punct(before));
            let (aw, ap) = (is_ws(after), is_punct(after));
            let left = !aw && (!ap || bw || bp);
            let right = !bw && (!bp || aw || ap);
            let (can_open, can_close) = match kind {
                MarkKind::Star | MarkKind::Tilde => (left, right),
                MarkKind::Under => (left && (!right || bp), right && (!left || ap)),
            };
            if can_close {
                let left_over = close_run(&mut out, &mut openers, kind, len, can_open);
                if left_over == 0 {
                    i += len;
                    continue;
                }
                // Partially unmatched: fall through and treat the rest below.
                len = left_over;
            }
            if can_open {
                out.push(Out::Mark(kind, len));
                openers.push(Opener {
                    out_idx: out.len() - 1,
                    kind,
                    len,
                    can_open,
                    can_close,
                });
            } else {
                push_text(&mut out, &c.to_string().repeat(len));
            }
            i += len;
            continue;
        }
        // Newlines: hard break on two trailing spaces, else soft.
        if c == '\n' {
            let trailing = match out.last() {
                Some(Out::Text(t)) => t.len() - t.trim_end_matches(' ').len(),
                _ => 0,
            };
            if trailing >= 2 {
                // Strip the signaling spaces from the tail.
                let mut need = 2;
                while need > 0 {
                    match out.last_mut() {
                        Some(Out::Text(t)) if t.ends_with(' ') => {
                            t.pop();
                            need -= 1;
                            if t.is_empty() {
                                out.pop();
                            }
                        }
                        _ => break,
                    }
                }
                out.push(Out::Node(Inline::HardBreak));
            } else {
                out.push(Out::Node(Inline::SoftBreak));
            }
            i += 1;
            continue;
        }
        // Plain text until the next special char.
        let mut j = i;
        while j < chars.len() && !"\\[<*~_\n`".contains(chars[j]) {
            j += 1;
        }
        push_text(&mut out, &chars[i..j].iter().collect::<String>());
        i = j;
    }

    let mut inlines: Vec<Inline> = out
        .into_iter()
        .map(|n| match n {
            Out::Text(t) => Inline::Text(t),
            Out::Node(n) => n,
            Out::Mark(k, l) => Inline::Text(mark_text(k, l)),
        })
        .collect();
    // Adjacent text (e.g. literal markers beside words) merges into one node.
    let mut merged: Vec<Inline> = Vec::with_capacity(inlines.len());
    for node in inlines.drain(..) {
        match (merged.last_mut(), node) {
            (Some(Inline::Text(t)), Inline::Text(n)) => t.push_str(&n),
            (_, node) => merged.push(node),
        }
    }
    merged
}

fn mark_text(kind: MarkKind, len: usize) -> String {
    let c = match kind {
        MarkKind::Star => '*',
        MarkKind::Under => '_',
        MarkKind::Tilde => '~',
    };
    c.to_string().repeat(len)
}

/// Scan `[` at `lb`: returns (inner text, index after `]`), nesting-aware.
/// (Code spans never reach this scanner — they are split out beforehand.)
fn scan_bracket(chars: &[char], lb: usize) -> Option<(String, usize)> {
    let mut depth = 0;
    let mut j = lb;
    while j < chars.len() {
        match chars[j] {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some((chars[lb + 1..j].iter().collect(), j + 1));
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// Scan `(href "optional title")` at `open` (which must be `(`).
/// Returns (href, index after `)`).
fn scan_link_target(chars: &[char], open: usize) -> Option<(String, usize)> {
    if chars.get(open) != Some(&'(') {
        return None;
    }
    let mut j = open + 1;
    while chars.get(j) == Some(&' ') || chars.get(j) == Some(&'\n') {
        j += 1;
    }
    let href: String;
    if chars.get(j) == Some(&'<') {
        let end = chars[j..].iter().position(|&x| x == '>')?;
        href = chars[j + 1..j + end].iter().collect();
        if href.chars().any(|c| c == '<' || c == '\n') {
            return None;
        }
        j += end + 1;
    } else {
        let start = j;
        let mut depth = 0;
        while j < chars.len() {
            match chars[j] {
                '(' => depth += 1,
                ')' if depth == 0 => break,
                ')' => depth -= 1,
                ' ' | '\n' => break,
                c if c.is_control() => return None,
                _ => {}
            }
            j += 1;
        }
        href = chars[start..j].iter().collect();
        if href.is_empty() {
            return None;
        }
    }
    while chars.get(j) == Some(&' ') || chars.get(j) == Some(&'\n') {
        j += 1;
    }
    // Optional title, ignored.
    if matches!(chars.get(j), Some('"') | Some('\'') | Some('(')) {
        let q = chars[j];
        let close = if q == '(' { ')' } else { q };
        j += 1;
        while j < chars.len() && chars[j] != close {
            j += 1;
        }
        if chars.get(j) != Some(&close) {
            return None;
        }
        j += 1;
        while chars.get(j) == Some(&' ') || chars.get(j) == Some(&'\n') {
            j += 1;
        }
    }
    if chars.get(j) != Some(&')') {
        return None;
    }
    Some((href, j + 1))
}

fn is_autolink(s: &str) -> bool {
    if s.contains([' ', '<', '\n']) {
        return false;
    }
    match s.split_once(':') {
        Some((scheme, _)) => {
            let mut sc = scheme.chars();
            matches!(sc.next(), Some(c) if c.is_ascii_alphabetic())
                && scheme.len() >= 2
                && scheme.len() <= 32
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        }
        None => false,
    }
}

fn is_html_tag(s: &str) -> bool {
    let s = s.trim();
    let s = s.strip_prefix('/').unwrap_or(s);
    if s.starts_with('!') || s.starts_with('?') {
        return true;
    }
    match s.chars().next() {
        Some(c) if c.is_ascii_alphabetic() => !s.contains(['<', '>']),
        _ => false,
    }
}

pub fn parse_inlines(s: &str) -> Vec<Inline> {
    let mut out = parse_raw(s.trim_end_matches([' ', '\t']));
    // A trailing hard-break signal with no following line is just whitespace.
    while matches!(out.last(), Some(Inline::HardBreak)) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emphasis_nests() {
        assert_eq!(
            parse_inlines("**bold** and *it*"),
            vec![
                Inline::Strong(vec![Inline::Text("bold".into())]),
                Inline::Text(" and ".into()),
                Inline::Emph(vec![Inline::Text("it".into())]),
            ]
        );
        // Triple star: strong inside emph.
        assert_eq!(
            parse_inlines("***both***"),
            vec![Inline::Emph(vec![Inline::Strong(vec![Inline::Text(
                "both".into()
            )])])]
        );
        // Intra-word underscores stay literal.
        assert_eq!(
            parse_inlines("foo_bar_baz"),
            vec![Inline::Text("foo_bar_baz".into())]
        );
        // Unmatched markers are literal.
        assert_eq!(parse_inlines("*solo"), vec![Inline::Text("*solo".into())]);
    }

    #[test]
    fn code_links_strike() {
        assert_eq!(parse_inlines("`a*b`"), vec![Inline::Code("a*b".into())]);
        // Emphasis reaches across code spans.
        assert_eq!(
            parse_inlines("*a `c` b*"),
            vec![Inline::Emph(vec![
                Inline::Text("a ".into()),
                Inline::Code("c".into()),
                Inline::Text(" b".into()),
            ])]
        );
        assert_eq!(
            parse_inlines("[t](https://e.com)"),
            vec![Inline::Link {
                href: "https://e.com".into(),
                content: vec![Inline::Text("t".into())],
            }]
        );
        assert_eq!(
            parse_inlines("![alt](i.png)"),
            vec![Inline::Image {
                src: "i.png".into(),
                alt: "alt".into(),
            }]
        );
        assert_eq!(
            parse_inlines("~~gone~~"),
            vec![Inline::Strike(vec![Inline::Text("gone".into())])]
        );
        assert_eq!(
            parse_inlines("<https://e.com>"),
            vec![Inline::Link {
                href: "https://e.com".into(),
                content: vec![Inline::Text("https://e.com".into())],
            }]
        );
        // Inline HTML tags are stripped but their content is kept.
        assert_eq!(
            parse_inlines("a<b>x</b>c"),
            vec![Inline::Text("axc".into())]
        );
    }

    #[test]
    fn breaks_and_escapes() {
        assert_eq!(
            parse_inlines("a  \nb"),
            vec![
                Inline::Text("a".into()),
                Inline::HardBreak,
                Inline::Text("b".into())
            ]
        );
        assert_eq!(
            parse_inlines("a\nb"),
            vec![
                Inline::Text("a".into()),
                Inline::SoftBreak,
                Inline::Text("b".into())
            ]
        );
        assert_eq!(parse_inlines("\\*x\\*"), vec![Inline::Text("*x*".into())]);
    }
}
