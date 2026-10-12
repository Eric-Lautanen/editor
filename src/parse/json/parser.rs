//! JSON parser: token stream -> [`Value`] with 1-based line/col errors.
//!
//! Recursive descent with an explicit depth limit so hostile nesting fails
//! with a position instead of a stack overflow. Object keys stay in file
//! order (duplicates are kept, not merged) so the preview matches the file.

use super::{lexer, Value};
use crate::error::{Error, Result};

/// Deeper than any realistic document; hostile input gets an error, not a crash.
const MAX_DEPTH: usize = 128;

fn err(line: usize, col: usize, msg: impl Into<String>) -> Error {
    Error::Parse {
        line,
        col,
        msg: msg.into(),
    }
}

struct Parser<'a> {
    toks: &'a [lexer::Lexeme],
    eof: lexer::Span,
    pos: usize,
}

impl Parser<'_> {
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

    fn bump(&mut self) -> Result<lexer::Lexeme> {
        match self.toks.get(self.pos) {
            Some(l) => {
                self.pos += 1;
                Ok(l.clone())
            }
            None => Err(self.eof_err("expected more input")),
        }
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
        let l = self.bump()?;
        match l.tok {
            lexer::Token::Str(s) => Ok(Value::String(s)),
            lexer::Token::Num(n) => Ok(Value::Number(n)),
            lexer::Token::True => Ok(Value::Bool(true)),
            lexer::Token::False => Ok(Value::Bool(false)),
            lexer::Token::Null => Ok(Value::Null),
            lexer::Token::LBrace => self.object(depth),
            lexer::Token::LBracket => self.array(depth),
            _ => Err(err(l.line, l.col, "expected a value")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value> {
        let mut pairs = Vec::new();
        if matches!(self.peek().map(|l| &l.tok), Some(lexer::Token::RBrace)) {
            self.pos += 1;
            return Ok(Value::Object(pairs));
        }
        loop {
            let k = self.bump()?;
            let lexer::Token::Str(key) = k.tok else {
                return Err(err(k.line, k.col, "expected a string key"));
            };
            self.expect(&lexer::Token::Colon, "':'")?;
            pairs.push((key, self.value(depth + 1)?));
            let s = self.bump()?;
            match s.tok {
                lexer::Token::Comma => {}
                lexer::Token::RBrace => break,
                _ => return Err(err(s.line, s.col, "expected ',' or '}'")),
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
            let s = self.bump()?;
            match s.tok {
                lexer::Token::Comma => {}
                lexer::Token::RBracket => break,
                _ => return Err(err(s.line, s.col, "expected ',' or ']'")),
            }
        }
        Ok(Value::Array(items))
    }
}

/// Parse one JSON document. Any top-level value is accepted; trailing content
/// after the value is an error. Feed JSONL through `crate::parse::jsonl`.
pub fn parse(src: &str) -> Result<Value> {
    let lexed = lexer::lex(src)?;
    if lexed.toks.is_empty() {
        return Err(err(lexed.eof.line, lexed.eof.col, "empty document"));
    }
    let mut p = Parser {
        toks: &lexed.toks,
        eof: lexed.eof,
        pos: 0,
    };
    let v = p.value(0)?;
    if let Some(l) = p.peek() {
        return Err(err(l.line, l.col, "unexpected trailing content"));
    }
    Ok(v)
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
