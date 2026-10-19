//! JSON Lines / NDJSON: one JSON value per line, parsed independently.
//!
//! A bad line never fails the file: it becomes a record carrying the parse
//! error, so the viewer can show per-line markers. Blank lines are skipped
//! but keep their physical numbers on the records around them.

use super::json::Value;
use crate::core::buffer::Buffer;
use crate::error::Result;

pub struct Record {
    /// 1-based physical line number in the file (blank lines skipped).
    pub line: usize,
    pub value: Result<Value>,
}

/// One record from the physical line at `index` (0-based): `None` for a blank
/// line, which the format skips. That rule lives here so the whole-file and
/// incremental paths cannot disagree about which lines are records.
fn record_of(index: usize, raw: &str) -> Option<Record> {
    if raw.trim().is_empty() {
        return None;
    }
    Some(Record {
        line: index + 1,
        value: super::json::parser::parse(raw),
    })
}

pub fn parse(src: &str) -> Vec<Record> {
    src.lines()
        .enumerate()
        .filter_map(|(i, raw)| record_of(i, raw))
        .collect()
}

/// Parse the buffer lines `[start, end)` as records, physically numbered.
///
/// This is what an edit replays: a record is one line, so the lines a span
/// replaced are the only ones whose parse can have changed. Same blank-line
/// rule and same numbering as [`parse`] — for any buffer, the concatenation of
/// `parse_lines` over a line partition is exactly `parse` of the text.
pub fn parse_lines(buf: &Buffer, start: usize, end: usize) -> Vec<Record> {
    (start..end.min(buf.line_count()))
        .filter_map(|i| record_of(i, buf.line(i)))
        .collect()
}

/// Below this many records the thread split costs more than it saves.
const PARALLEL_MIN_LINES: usize = 4_000;

/// [`parse`] across a few threads: the lines are independent, so each worker
/// takes a contiguous slice of them and the results concatenate in order.
/// Threads are only spent when there are enough lines to pay for it, capped at
/// the machine's parallelism.
///
/// The workers borrow the source through [`std::thread::scope`], so no buffer
/// is leaked and no per-chunk line vector is copied. Blank lines keep their
/// physical numbers exactly as in [`parse`].
pub fn parse_parallel(src: &str) -> Vec<Record> {
    let lines: Vec<&str> = src.lines().collect();
    if lines.len() < PARALLEL_MIN_LINES {
        return parse(src);
    }
    let line_count = lines.len();
    let threads = std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .min(line_count / 500)
        .clamp(2, 16);
    let chunk = line_count.div_ceil(threads);
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(threads);
        for t in 0..threads {
            let a = (t * chunk).min(line_count);
            let b = ((t + 1) * chunk).min(line_count);
            if a == b {
                continue;
            }
            let slice = &lines[a..b];
            let base = a; // physical start index of this chunk
            handles.push(scope.spawn(move || {
                slice
                    .iter()
                    .enumerate()
                    .filter_map(|(k, l)| record_of(base + k, l))
                    .collect::<Vec<_>>()
            }));
        }
        let mut records = Vec::new();
        for h in handles {
            records.extend(h.join().expect("jsonl worker"));
        }
        records
    })
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

    /// The parallel path produces the same records as the sequential one —
    /// including which physical lines are skipped and each error's position.
    #[test]
    fn parallel_matches_sequential() {
        let mut text = String::new();
        for i in 0..9_000 {
            text.push_str(&format!(r#"{{"i": {i}, "s": "row{i}}}"#));
            text.push('\n');
            if i % 97 == 0 {
                text.push_str("\nbroken line\n");
            }
        }
        let seq = parse(&text);
        let par = parse_parallel(&text);
        assert_eq!(seq.len(), par.len());
        for (a, b) in seq.iter().zip(par.iter()) {
            assert_eq!(a.line, b.line, "line mismatch at {}", a.line);
            match (&a.value, &b.value) {
                (Ok(x), Ok(y)) => {
                    // Value has no PartialEq; the Debug shape carries structure.
                    assert_eq!(format!("{x:?}"), format!("{y:?}"));
                }
                (Err(_), Err(_)) => {}
                _ => panic!("diverged at line {}", a.line),
            }
        }
    }

    /// The incremental path parses a line range exactly as the whole-file path
    /// parses it: the same records, at the same physical lines, with the same
    /// errors. A plan patch depends on that — it parses the edited lines this
    /// way and keeps the rest.
    #[test]
    fn a_line_range_parses_like_the_whole_file() {
        let text = "{\"a\": 1}\n\nbroken\n[1, 2]\n  \n{\"b\": {\"c\": [3]}}\n";
        let buf = Buffer::from_text(text);
        let whole = parse(text);
        for split in [1usize, 3, 4, 5, buf.line_count()] {
            let mut parts = parse_lines(&buf, 0, split);
            parts.extend(parse_lines(&buf, split, buf.line_count()));
            assert_eq!(parts.len(), whole.len(), "split at {split}");
            for (a, b) in parts.iter().zip(whole.iter()) {
                assert_eq!(a.line, b.line, "split at {split}");
                assert_eq!(a.value.is_ok(), b.value.is_ok(), "split at {split}");
            }
        }
    }

    /// Small files take the sequential path (same answer, no threads spent).
    #[test]
    fn small_files_stay_sequential() {
        let recs = parse_parallel("{\"a\": 1}\n");
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].line, 1);
    }
}
