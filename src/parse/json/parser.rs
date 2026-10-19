//! JSON parser: token stream -> [`Value`] with 1-based line/col errors.
//!
//! Recursive descent with an explicit depth limit so hostile nesting fails
//! with a position instead of a stack overflow. Object keys stay in file
//! order (duplicates are kept, not merged) so the preview matches the file.
//!
//! [`parse_spanned`] also reports every node's source range, in pre-order: a
//! node and the text it was parsed from are the same entry of two parallel
//! lists, which is what an edit uses to re-parse one node instead of the
//! document.

use super::{lexer, Value};
use crate::error::{Error, Result};

/// One node's source range, in **bytes**, in the document the node was parsed
/// from. Pre-order: a node's range contains its descendants' and is disjoint
/// from its following siblings'.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    /// Byte offset of the node's first token.
    pub start: usize,
    /// Byte offset just past the node's last token.
    pub end: usize,
}

/// Deeper than any realistic document; hostile input gets an error, not a crash.
const MAX_DEPTH: usize = 128;

fn err(line: usize, col: usize, msg: impl Into<String>) -> Error {
    Error::Parse {
        line,
        col,
        msg: msg.into(),
    }
}

struct Parser {
    toks: Vec<lexer::Lexeme>,
    eof: lexer::Eof,
    pos: usize,
    /// Every node's range, pushed as the node is entered so the order is
    /// pre-order — the order a `ValuePlan` slot is assigned in.
    spans: Vec<Span>,
}

impl Parser {
    fn peek(&self) -> Option<&lexer::Lexeme> {
        self.toks.get(self.pos)
    }

    fn eof_err(&self, what: &str) -> Error {
        err(
            self.eof.line,
            self.eof.col,
            format!("unexpected end of file ({what})"),
        )
    }

    /// Take the next token. The token is moved out of the stream (a `Null`
    /// placeholder is left behind) so a string token is not cloned on its way
    /// into the value tree; only `pos` ever walks backward, so a consumed slot
    /// is never read again.
    fn bump(&mut self) -> Result<(usize, usize, lexer::Token)> {
        let pos = self.pos;
        let Some(l) = self.toks.get_mut(pos) else {
            return Err(self.eof_err("expected more input"));
        };
        let (line, col) = (l.line, l.col);
        let tok = std::mem::replace(&mut l.tok, lexer::Token::Null);
        self.pos = pos + 1;
        Ok((line, col, tok))
    }

    /// Consume `tok`, or fail with what was actually found.
    fn expect(&mut self, tok: &lexer::Token, what: &str) -> Result<()> {
        match self.toks.get(self.pos) {
            Some(l) if &l.tok == tok => {
                self.pos += 1;
                Ok(())
            }
            Some(l) => Err(err(l.line, l.col, format!("expected {what}"))),
            None => Err(self.eof_err(what)),
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value> {
        if depth > MAX_DEPTH {
            let (line, col) = match self.peek() {
                Some(l) => (l.line, l.col),
                None => (self.eof.line, self.eof.col),
            };
            return Err(err(
                line,
                col,
                format!("nesting too deep (limit {MAX_DEPTH})"),
            ));
        }
        // The node's slot in the span list is taken before its children are
        // parsed, and its range is closed over them; that is what makes the
        // list pre-order.
        let at = self.peek().map_or(self.eof.at, |l| l.at);
        let ix = self.spans.len();
        self.spans.push(Span { start: at, end: at });
        let (line, col, tok) = self.bump()?;
        let v = match tok {
            lexer::Token::Str(s) => Ok(Value::String(s)),
            lexer::Token::Num(n) => Ok(Value::Number(n)),
            lexer::Token::True => Ok(Value::Bool(true)),
            lexer::Token::False => Ok(Value::Bool(false)),
            lexer::Token::Null => Ok(Value::Null),
            lexer::Token::LBrace => self.object(depth),
            lexer::Token::LBracket => self.array(depth),
            _ => Err(err(line, col, "expected a value")),
        }?;
        // The last token consumed is the node's last token: a closing brace
        // for a container, the literal itself for a scalar.
        let end = self
            .toks
            .get(self.pos.saturating_sub(1))
            .map_or(at, |l| l.end);
        self.spans[ix] = Span { start: at, end };
        Ok(v)
    }

    fn object(&mut self, depth: usize) -> Result<Value> {
        let mut pairs = Vec::new();
        if matches!(self.peek().map(|l| &l.tok), Some(lexer::Token::RBrace)) {
            self.pos += 1;
            return Ok(Value::Object(pairs));
        }
        loop {
            let (line, col, tok) = self.bump()?;
            let lexer::Token::Str(key) = tok else {
                return Err(err(line, col, "expected a string key"));
            };
            self.expect(&lexer::Token::Colon, "':'")?;
            pairs.push((key, self.value(depth + 1)?));
            let (line, col, tok) = self.bump()?;
            match tok {
                lexer::Token::Comma => {}
                lexer::Token::RBrace => break,
                _ => return Err(err(line, col, "expected ',' or '}'")),
            }
        }
        Ok(Value::Object(pairs))
    }

    fn array(&mut self, depth: usize) -> Result<Value> {
        let mut items = Vec::new();
        if matches!(self.peek().map(|l| &l.tok), Some(lexer::Token::RBracket)) {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            let (line, col, tok) = self.bump()?;
            match tok {
                lexer::Token::Comma => {}
                lexer::Token::RBracket => break,
                _ => return Err(err(line, col, "expected ',' or ']'")),
            }
        }
        Ok(Value::Array(items))
    }
}

/// Parse one JSON document, with the source range of every node. Any top-level
/// value is accepted; trailing content after the value is an error. Feed JSONL
/// through `crate::parse::jsonl`.
///
/// The spans are pre-order, so `spans[i]` is the text of the node that a plan's
/// slot `i` measures — and `&src[spans[i].start..spans[i].end]` parses back to
/// that same node, which is the property a patch relies on.
pub fn parse_spanned(src: &str) -> Result<(Value, Vec<Span>)> {
    let lexed = lexer::lex(src)?;
    let eof = lexed.eof;
    if lexed.toks.is_empty() {
        return Err(err(eof.line, eof.col, "empty document"));
    }
    let mut p = Parser {
        toks: lexed.toks,
        eof,
        pos: 0,
        spans: Vec::new(),
    };
    let v = p.value(0)?;
    if let Some(l) = p.peek() {
        return Err(err(l.line, l.col, "unexpected trailing content"));
    }
    Ok((v, p.spans))
}

/// Parse one JSON document, discarding the node ranges.
pub fn parse(src: &str) -> Result<Value> {
    parse_spanned(src).map(|(v, _)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_object_keeps_order() {
        let v = parse(r#"{"b": [1, {"a": null}], "a": {}}"#).unwrap();
        match v {
            Value::Object(pairs) => {
                assert_eq!(pairs[0].0, "b");
                assert_eq!(pairs[1].0, "a");
                match &pairs[0].1 {
                    Value::Array(items) => {
                        assert_eq!(items[0], Value::Number("1".into()));
                        assert!(matches!(items[1], Value::Object(_)));
                    }
                    other => panic!("expected array, got {other:?}"),
                }
            }
            other => panic!("expected object, got {other:?}"),
        }
    }

    #[test]
    fn scalars_and_empty_containers() {
        assert_eq!(parse("true").unwrap(), Value::Bool(true));
        assert_eq!(parse("  null ").unwrap(), Value::Null);
        assert_eq!(parse("{}").unwrap(), Value::Object(vec![]));
        assert_eq!(parse("[ ]").unwrap(), Value::Array(vec![]));
        assert_eq!(parse("\"x\"").unwrap(), Value::String("x".into()));
    }

    #[test]
    fn errors_carry_positions() {
        let e = parse("{\"a\": }").unwrap_err();
        assert!(
            matches!(
                e,
                Error::Parse {
                    line: 1,
                    col: 7,
                    ..
                }
            ),
            "got {e}"
        );
        let e = parse("{\"a\": 1, }").unwrap_err();
        assert!(
            matches!(e, Error::Parse { .. }),
            "trailing comma accepted: {e}"
        );
        let e = parse("[1 2]").unwrap_err();
        assert!(matches!(e, Error::Parse { .. }), "got {e}");
        let e = parse("").unwrap_err();
        assert!(matches!(e, Error::Parse { .. }), "empty accepted: {e}");
        let e = parse("{} {}").unwrap_err();
        assert!(
            matches!(e, Error::Parse { .. }),
            "trailing value accepted: {e}"
        );
        let e = parse("{unquoted: 1}").unwrap_err();
        assert!(matches!(e, Error::Parse { .. }), "got {e}");
        // Unterminated input reports the end of file.
        let e = parse("{\"a\": [1,").unwrap_err();
        assert!(
            matches!(
                e,
                Error::Parse {
                    line: 1,
                    col: 10,
                    ..
                }
            ),
            "got {e}"
        );
    }

    #[test]
    fn depth_limit_errors_instead_of_crashing() {
        let deep = format!("{{\"a\": {}}}", "[".repeat(300) + &"]".repeat(300));
        let e = parse(&deep).unwrap_err();
        assert!(
            matches!(e, Error::Parse { ref msg, .. } if msg.contains("too deep")),
            "got {e}"
        );
    }

    /// Every node's range is its own source text: slicing the document at the
    /// range parses back to that node, and the list is pre-order, so a parent
    /// comes before its children and contains them.
    #[test]
    fn every_span_slices_back_to_its_node() {
        let src = "\u{FEFF}{\n  \"a\": [1, {\"b\": \"x 𝄞\"}],\n  \"c\": null\n}\n";
        let (value, spans) = parse_spanned(src).unwrap();
        // Walk the tree in the same pre-order, checking each node against its
        // span as we go.
        fn walk(v: &Value, nodes: &mut Vec<Value>) {
            nodes.push(v.clone());
            match v {
                Value::Array(items) => items.iter().for_each(|c| walk(c, nodes)),
                Value::Object(pairs) => pairs.iter().for_each(|(_, c)| walk(c, nodes)),
                _ => {}
            }
        }
        let mut nodes = Vec::new();
        walk(&value, &mut nodes);
        assert_eq!(nodes.len(), spans.len(), "one span per node");
        for (i, (node, span)) in nodes.iter().zip(spans.iter()).enumerate() {
            let text = &src[span.start..span.end];
            let back = parse(text).unwrap_or_else(|e| panic!("span {i} ({text:?}) failed: {e}"));
            assert_eq!(&back, node, "span {i} is not its node: {text:?}");
        }
        // The root covers the document, trim of surrounding whitespace.
        assert_eq!(spans[0].end, src.trim_end().len());
        assert_eq!(spans[0].start, '\u{FEFF}'.len_utf8());
        // The last node in pre-order is the document's last leaf, and its range
        // is exactly its token.
        let leaf = spans.last().unwrap();
        assert_eq!(&src[leaf.start..leaf.end], "null");
        // Consecutive nodes are either nested (the second inside the first) or
        // in document order (the second starting after the first ends) — never
        // overlapping some other way, which is what makes a binary search over
        // the starts a valid "which node is this offset in" lookup.
        for pair in spans.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert!(b.start >= a.start, "pre-order went backwards: {a:?} {b:?}");
            assert!(
                b.end <= a.end || b.start >= a.end,
                "span {b:?} is neither inside nor after {a:?}"
            );
        }
    }

    /// A failed parse has no spans to hand out — the caller re-parses whole.
    #[test]
    fn spans_come_only_from_a_whole_parse() {
        assert!(parse_spanned("{\"a\": }").is_err());
        let (v, spans) = parse_spanned("[1, 2]").unwrap();
        assert_eq!(spans.len(), 3, "array, two items");
        assert!(matches!(v, Value::Array(_)));
    }

    #[test]
    fn duplicate_keys_are_kept_in_order() {
        let v = parse("{\"a\": 1, \"a\": 2}").unwrap();
        match v {
            Value::Object(pairs) => {
                assert_eq!(pairs.len(), 2);
                assert_eq!(pairs[1].1, Value::Number("2".into()));
            }
            other => panic!("expected object, got {other:?}"),
        }
    }
}
