//! JSON lexer: source text -> tokens with 1-based line/col positions.
//!
//! Strict superset-free JSON: `"`-quoted strings with `\uXXXX` escapes,
//! spec-valid numbers (kept as source text to avoid precision loss), and the
//! `true`/`false`/`null` literals. A leading UTF-8 BOM is skipped.

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Comma,
    Str(String),
    Num(String),
    True,
    False,
    Null,
}

/// A token plus the 1-based position where it starts.
#[derive(Debug, Clone, PartialEq)]
pub struct Lexeme {
    pub tok: Token,
    pub line: usize,
    pub col: usize,
}

/// Where the input ended (for "unexpected end of file" errors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub col: usize,
}

#[derive(Debug)]
pub struct Lexed {
    pub toks: Vec<Lexeme>,
    pub eof: Span,
}

struct Lexer<'a> {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
    _src: &'a str,
}

fn err(line: usize, col: usize, msg: impl Into<String>) -> Error {
    Error::Parse {
        line,
        col,
        msg: msg.into(),
    }
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn lex_string(&mut self, line: usize, col: usize) -> Result<Lexeme> {
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(err(line, col, "unterminated string")),
                Some('"') => break,
                Some('\\') => out.push(self.escape(line, col)?),
                Some(c) if (c as u32) < 0x20 => {
                    return Err(err(
                        self.line,
                        self.col.saturating_sub(1),
                        "unescaped control character in string",
                    ));
                }
                Some(c) => out.push(c),
            }
        }
        Ok(Lexeme {
            tok: Token::Str(out),
            line,
            col,
        })
    }

    fn escape(&mut self, line: usize, col: usize) -> Result<char> {
        match self.bump() {
            Some('"') => Ok('"'),
            Some('\\') => Ok('\\'),
            Some('/') => Ok('/'),
            Some('b') => Ok('\u{0008}'),
            Some('f') => Ok('\u{000C}'),
            Some('n') => Ok('\n'),
            Some('r') => Ok('\r'),
            Some('t') => Ok('\t'),
            Some('u') => {
                let hi = self.hex4()?;
                // UTF-16 surrogate pair.
                if (0xD800..0xDC00).contains(&hi) {
                    match (self.bump(), self.bump()) {
                        (Some('\\'), Some('u')) => {
                            let lo = self.hex4()?;
                            if !(0xDC00..0xE000).contains(&lo) {
                                return Err(err(line, col, "lone high surrogate"));
                            }
                            let cp =
                                0x10000 + ((hi - 0xD800) as u32) * 0x400 + (lo - 0xDC00) as u32;
                            char::from_u32(cp).ok_or_else(|| err(self.line, self.col, "bad escape"))
                        }
                        _ => Err(err(line, col, "lone high surrogate")),
                    }
                } else if (0xDC00..0xE000).contains(&hi) {
                    Err(err(line, col, "lone low surrogate"))
                } else {
                    char::from_u32(hi as u32).ok_or_else(|| err(self.line, self.col, "bad escape"))
                }
            }
            _ => Err(err(self.line, self.col, "bad escape")),
        }
    }

    fn hex4(&mut self) -> Result<u16> {
        let mut v: u16 = 0;
        for _ in 0..4 {
            let d = match self.bump() {
                Some(c) if c.is_ascii_hexdigit() => c.to_digit(16).unwrap() as u16,
                _ => return Err(err(self.line, self.col, "bad \\u escape")),
            };
            v = v * 16 + d;
        }
        Ok(v)
    }

    /// Strict JSON number: `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`.
    fn lex_number(&mut self, line: usize, col: usize) -> Result<Lexeme> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.bump();
        }
        let int_start = self.pos;
        match self.peek() {
            Some(c) if c.is_ascii_digit() => {
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    self.bump();
                }
            }
            _ => return Err(err(line, col, "bad number")),
        }
        if self.chars[int_start] == '0' && self.pos - int_start > 1 {
            return Err(err(line, col, "leading zeros are not allowed"));
        }
        if self.peek() == Some('.') {
            self.bump();
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return Err(err(line, col, "bad number"));
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                self.bump();
            }
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return Err(err(line, col, "bad number"));
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
        }
        Ok(Lexeme {
            tok: Token::Num(self.chars[start..self.pos].iter().collect()),
            line,
            col,
        })
    }

    /// `true` / `false` / `null`: the next char must not extend the word.
    fn lex_word(&mut self, line: usize, col: usize) -> Result<Lexeme> {
        let tail_ok =
            |lx: &Lexer<'_>| !matches!(lx.peek(), Some(c) if c.is_alphanumeric() || c == '_');
        let rest = |lx: &mut Lexer<'_>, s: &str| {
            for expect in s.chars() {
                if lx.bump() != Some(expect) {
                    return false;
                }
            }
            true
        };
        match self.peek() {
            Some('t') if rest(self, "true") && tail_ok(self) => Ok(Lexeme {
                tok: Token::True,
                line,
                col,
            }),
            Some('f') if rest(self, "false") && tail_ok(self) => Ok(Lexeme {
                tok: Token::False,
                line,
                col,
            }),
            Some('n') if rest(self, "null") && tail_ok(self) => Ok(Lexeme {
                tok: Token::Null,
                line,
                col,
            }),
            _ => Err(err(line, col, "unexpected character")),
        }
    }
}

pub fn lex(src: &str) -> Result<Lexed> {
    let mut lx = Lexer {
        chars: src.chars().collect(),
        pos: 0,
        line: 1,
        col: 1,
        _src: src,
    };
    // A Windows-editor BOM is not JSON content.
    if lx.peek() == Some('\u{FEFF}') {
        lx.bump();
    }
    let mut toks = Vec::new();
    while let Some(c) = lx.peek() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                lx.bump();
            }
            '{' => {
                toks.push(Lexeme {
                    tok: Token::LBrace,
                    line: lx.line,
                    col: lx.col,
                });
                lx.bump();
            }
            '}' => {
                toks.push(Lexeme {
                    tok: Token::RBrace,
                    line: lx.line,
                    col: lx.col,
                });
                lx.bump();
            }
            '[' => {
                toks.push(Lexeme {
                    tok: Token::LBracket,
                    line: lx.line,
                    col: lx.col,
                });
                lx.bump();
            }
            ']' => {
                toks.push(Lexeme {
                    tok: Token::RBracket,
                    line: lx.line,
                    col: lx.col,
                });
                lx.bump();
            }
            ':' => {
                toks.push(Lexeme {
                    tok: Token::Colon,
                    line: lx.line,
                    col: lx.col,
                });
                lx.bump();
            }
            ',' => {
                toks.push(Lexeme {
                    tok: Token::Comma,
                    line: lx.line,
                    col: lx.col,
                });
                lx.bump();
            }
            '"' => {
                let (line, col) = (lx.line, lx.col);
                lx.bump();
                toks.push(lx.lex_string(line, col)?);
            }
            '-' | '0'..='9' => {
                let (line, col) = (lx.line, lx.col);
                toks.push(lx.lex_number(line, col)?);
            }
            _ => {
                let (line, col) = (lx.line, lx.col);
                toks.push(lx.lex_word(line, col)?);
            }
        }
    }
    Ok(Lexed {
        toks,
        eof: Span {
            line: lx.line,
            col: lx.col,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Token> {
        lex(src).unwrap().toks.into_iter().map(|l| l.tok).collect()
    }

    #[test]
    fn punctuation_and_scalars() {
        assert_eq!(
            toks(r#"{"a": [1, -2.5e3, true, false, null]}"#),
            vec![
                Token::LBrace,
                Token::Str("a".into()),
                Token::Colon,
                Token::LBracket,
                Token::Num("1".into()),
                Token::Comma,
                Token::Num("-2.5e3".into()),
                Token::Comma,
                Token::True,
                Token::Comma,
                Token::False,
                Token::Comma,
                Token::Null,
                Token::RBracket,
                Token::RBrace,
            ]
        );
    }

    #[test]
    fn string_escapes() {
        let t = toks(r#""a\"b\\c\/d\b\f\n\r\t\u00e9𝄞""#);
        assert_eq!(t.len(), 1);
        match &t[0] {
            Token::Str(s) => {
                assert!(s.contains('é'), "missing \\u00e9: {s:?}");
                assert!(s.contains('𝄞'), "missing raw non-BMP char: {s:?}");
                assert!(s.contains('\n') && s.contains('\t'));
            }
            other => panic!("expected string, got {other:?}"),
        }
        // Surrogate pair decodes to one astral char.
        assert_eq!(toks(r#""\uD834\uDD1E""#), vec![Token::Str("𝄞".into())]);
    }

    #[test]
    fn positions_point_at_the_problem() {
        let e = lex("{\n  \"a\": truX\n}").unwrap_err();
        assert!(
            matches!(
                e,
                Error::Parse {
                    line: 2,
                    col: 8,
                    ..
                }
            ),
            "got {e}"
        );
        let e = lex("\"unterminated").unwrap_err();
        assert!(
            matches!(
                e,
                Error::Parse {
                    line: 1,
                    col: 1,
                    ..
                }
            ),
            "got {e}"
        );
        let e = lex("[01]").unwrap_err();
        assert!(matches!(e, Error::Parse { .. }), "got {e}");
        let e = lex("\"bad \x01 escape\"").unwrap_err();
        assert!(matches!(e, Error::Parse { .. }), "got {e}");
        let e = lex("\"\\uD834\"").unwrap_err();
        assert!(
            matches!(e, Error::Parse { .. }),
            "lone surrogate accepted: {e}"
        );
    }

    #[test]
    fn bom_is_skipped() {
        assert_eq!(toks("\u{FEFF}1"), vec![Token::Num("1".into())]);
    }
}
