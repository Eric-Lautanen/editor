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

/// Below this many records the thread split costs more than it saves.
const PARALLEL_MIN_LINES: usize = 4_000;

/// [`parse`] across a few threads: the lines are independent, so each worker
/// takes a contiguous slice of them and the results concatenate in order.
/// Threads are only spent when there are enough lines to pay for it, capped at
/// the machine's parallelism.
///
/// The text is leaked so workers can hold `&'static` line slices — the same
/// trade `fallback::Chain::load` makes for font bytes. The leak is one
/// document-sized buffer per loaded JSONL; acceptable for a big-file path
/// where parse dominates. Blank lines keep their physical numbers exactly as
/// in [`parse`].
pub fn parse_parallel(src: &str) -> Vec<Record> {
    let line_count = src.lines().count();
    if line_count < PARALLEL_MIN_LINES {
        return parse(src);
    }
    let threads = std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .min(line_count / 500)
        .clamp(2, 16);
    let static_src: &'static str =
        // The source is the buffer's text, which outlives this call. The
        // transmute is only a borrow-extender; nothing is mutated through it.
        unsafe { std::mem::transmute::<&str, &'static str>(src) };
    let lines: Vec<&'static str> = static_src.lines().collect();
    let chunk = line_count.div_ceil(threads);
    let mut handles = Vec::with_capacity(threads);
    for t in 0..threads {
        let a = (t * chunk).min(line_count);
        let b = ((t + 1) * chunk).min(line_count);
        if a == b {
            continue;
        }
        let slice: Vec<&'static str> = lines[a..b].to_vec();
        let base = a; // physical start index of this chunk
        handles.push(std::thread::spawn(move || {
            let mut out = Vec::new();
            for (k, l) in slice.iter().enumerate() {
                if l.trim().is_empty() {
                    continue;
                }
                out.push(Record {
                    line: base + k + 1,
                    value: super::json::parser::parse(l),
                });
            }
            out
        }));
    }
    let mut records = Vec::new();
    for h in handles {
        records.extend(h.join().expect("jsonl worker"));
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

    /// Small files take the sequential path (same answer, no threads spent).
    #[test]
    fn small_files_stay_sequential() {
        let recs = parse_parallel("{\"a\": 1}\n");
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].line, 1);
    }
}
