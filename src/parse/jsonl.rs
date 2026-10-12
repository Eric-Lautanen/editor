//! JSON Lines / NDJSON: one JSON value per line, parsed independently.
//!
//! A bad line never fails the file: it becomes a record carrying the parse
//! error, so the viewer can show per-line markers. Blank lines are skipped
//! but keep their physical numbers on the records around them.

use super::json::Value;
use crate::error::Result;

pub struct Record {
    /// 1-based physical line number in the file (blank lines skipped).
    pub line: usize,
    pub value: Result<Value>,
}

pub fn parse(src: &str) -> Vec<Record> {
    let mut records = Vec::new();
    for (i, raw) in src.lines().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        records.push(Record {
            line: i + 1,
            value: super::json::parser::parse(raw),
        });
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_independent() {
        let recs = parse("{\"a\": 1}\n\nbroken\n[1, 2]\n");
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[0].line, 1);
        assert!(recs[0].value.is_ok());
        // The blank line is skipped but numbering is physical.
        assert_eq!(recs[1].line, 3);
        assert!(recs[1].value.is_err());
        assert_eq!(recs[2].line, 4);
        assert!(matches!(recs[2].value, Ok(Value::Array(_))));
    }

    #[test]
    fn empty_input_has_no_records() {
        assert!(parse("").is_empty());
        assert!(parse("\n  \n").is_empty());
    }
}
