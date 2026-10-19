//! Markdown front end (M4): `block` + `inline` produce the `ast::Document` that
//! `layout::markdown` renders.

pub mod ast;
pub mod block;
pub mod inline;

use ast::Document;
use std::borrow::Cow;

/// Parse Markdown (CommonMark subset + GFM tables/task lists) into an AST.
/// Total: unrecognized input degrades to paragraphs, never errors.
#[cfg(test)]
pub fn parse(src: &str) -> Document {
    block::parse_blocks(src)
}

/// Parse straight from a [`crate::core::buffer::Buffer`], without assembling
/// the whole document into a temporary string first. Markdown's parser is
/// already line-based, so the renderer can hand it the lines it has.
pub fn parse_buffer(buf: &crate::core::buffer::Buffer) -> Document {
    block::parse_buffer(buf)
}

/// Decode HTML character references in text: `&amp;`, `&lt;`, `&#39;`,
/// `&#x2014;` and the handful of named entities a document is likely to use.
///
/// CommonMark resolves references in text and link destinations; leaving them
/// raw put `&amp;` and `&mdash;` on screen as punctuation soup. An unknown or
/// malformed reference is left exactly as written (a lone `&` is just a `&`),
/// so the function never invents text. Code spans and fenced code are handled
/// by their own scanners and are never passed through here.
pub(crate) fn decode_entities(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let mut changed = false;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        match entity_at(tail) {
            Some((c, len)) => {
                out.push(c);
                rest = &tail[len..];
                changed = true;
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(s)
    }
}

/// Parse one character reference at the start of `s` (which begins with `&`).
/// Returns the decoded char and how many source bytes it consumed.
fn entity_at(s: &str) -> Option<(char, usize)> {
    let semi = s.find(';')?;
    // A named reference is short; a numeric one shorter still. Bounding the
    // body keeps a stray `&` followed much later by `;` from swallowing prose.
    let body = &s[1..semi];
    if body.is_empty() || body.len() > 32 {
        return None;
    }
    let c = if let Some(hex) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) {
        u32::from_str_radix(hex, 16)
            .ok()
            .filter(|&v| v != 0)
            .and_then(char::from_u32)?
    } else if let Some(dec) = body.strip_prefix('#') {
        dec.parse::<u32>()
            .ok()
            .filter(|&v| v != 0)
            .and_then(char::from_u32)?
    } else {
        named_entity(body)?
    };
    Some((c, semi + 1))
}

/// The named references worth carrying. Deliberately small: every entry is a
/// character the embedded faces cover, and anything outside it stays literal
/// rather than risking a wrong glyph.
fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{00A0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "bull" => '•',
        "middot" => '·',
        "times" => '×',
        "divide" => '÷',
        "deg" => '°',
        "plusmn" => '±',
        "frac12" => '½',
        "frac14" => '¼',
        "frac34" => '¾',
        "sup2" => '²',
        "sup3" => '³',
        "micro" => 'µ',
        "para" => '¶',
        "sect" => '§',
        "dagger" => '†',
        "Dagger" => '‡',
        "permil" => '‰',
        "prime" => '′',
        "Prime" => '″',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "larr" => '←',
        "uarr" => '↑',
        "rarr" => '→',
        "darr" => '↓',
        "harr" => '↔',
        "le" => '≤',
        "ge" => '≥',
        "ne" => '≠',
        "asymp" => '≈',
        "infin" => '∞',
        "sum" => '∑',
        "radic" => '√',
        "minus" => '−',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities_decode_and_unknown_ones_stay_literal() {
        assert_eq!(decode_entities("a &amp; b &lt; c &gt; d"), "a & b < c > d");
        assert_eq!(decode_entities("&#65;&#x42;&#x1F600;"), "AB😀");
        assert_eq!(decode_entities("&mdash; &hellip; &copy;"), "— … ©");
        // Unknown, unterminated and absurd references are left alone.
        assert_eq!(decode_entities("&bogus; &amp &;"), "&bogus; &amp &;");
        assert_eq!(decode_entities("no references"), "no references");
        // A lone ampersand costs no allocation.
        assert!(matches!(decode_entities("a & b"), Cow::Borrowed(_)));
    }
}
