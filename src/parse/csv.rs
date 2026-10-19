//! CSV/TSV and friends: delimiter-sniffed, quote-aware, one record per line.
//!
//! Deliberately small: quotes cannot span lines (an unclosed quote runs to
//! end of line), and only the first [`MAX_ROWS`] records are kept — the rest
//! are counted for the status line. A quick viewer must never turn a 10 MB
//! export into 100k heap strings; total parse cost stays one line at a time.

/// Records kept for layout; the rest are counted, not stored.
pub const MAX_ROWS: usize = 2000;

pub struct Sheet {
    pub header: Vec<String>,
    /// First [`MAX_ROWS`] data records (header excluded), ragged rows padded.
    pub rows: Vec<Vec<String>>,
    /// Data records in the file (header excluded, blanks skipped).
    pub total_rows: usize,
    pub cols: usize,
    /// True when rows were dropped by [`MAX_ROWS`].
    pub capped: bool,
}

fn count_outside_quotes(line: &str, delim: char) -> usize {
    let mut n = 0;
    let mut quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            if quotes && chars.peek() == Some(&'"') {
                chars.next();
            } else {
                quotes = !quotes;
            }
        } else if c == delim && !quotes {
            n += 1;
        }
    }
    n
}

/// Pick `,`/`\t`/`;`/`|` by outside-quote occurrences over the first few
/// lines; ties prefer `,` then `\t`.
pub fn sniff(text: &str) -> char {
    let mut counts = [0usize; 4];
    let delims = [',', '\t', ';', '|'];
    for line in text.lines().filter(|l| !l.trim().is_empty()).take(5) {
        for (i, d) in delims.iter().enumerate() {
            counts[i] += count_outside_quotes(line, *d);
        }
    }
    let best = counts
        .iter()
        .enumerate()
        .max_by_key(|&(i, &n)| (n, -(i as i64)))
        .map(|(i, _)| i)
        .unwrap_or(0);
    if counts[best] == 0 {
        ','
    } else {
        delims[best]
    }
}

/// One line into fields. `""` inside quotes is an escaped quote; an
/// unclosed quote runs to end of line. Surrounding whitespace is trimmed.
pub fn parse_line(line: &str, delim: char) -> Vec<String> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            if quotes && chars.peek() == Some(&'"') {
                cur.push('"');
                chars.next();
            } else {
                quotes = !quotes;
            }
        } else if c == delim && !quotes {
            fields.push(cur.trim().to_string());
            cur = String::new();
        } else {
            cur.push(c);
        }
    }
    fields.push(cur.trim().to_string());
    fields
}

/// The delimiter the file's own extension names, if one does.
///
/// Only `.tsv`/`.tab` are unambiguous enough to pin; `.csv` names a comma in
/// most locales but some tools write semicolons there (the Excel dialect),
/// so the sniffer keeps the deciding vote for it.
/// The delimiter the file's own extension names, if one does.
///
/// A `.tsv`/`.tab` file is tab-delimited by name, and sniffing can disagree:
/// one prose row (a note pasted in) carries commas that outrank the tab
/// count, and the sheet then widens with a ragged no-delimiter row — every
/// later data cell slides one field over. When the extension names the
/// delimiter, it wins. `.csv` is *not* pinned: some locales write it with
/// semicolons, so the sniffer keeps the deciding vote there.
pub fn hint_delimiter(ext: Option<&str>) -> Option<char> {
    match ext.unwrap_or("") {
        "tsv" | "tab" => Some('\t'),
        _ => None,
    }
}
/// Parse a sheet from `text`, with the file's extension when the caller
/// knows it (`.tsv`/`.tab` pins the tab; anything else is sniffed).
pub fn parse_with_ext(text: &str, ext: Option<&str>) -> Sheet {
    let delimiter = hint_delimiter(ext).unwrap_or_else(|| sniff(text));
    let mut header = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut total_rows = 0usize;
    let mut cols = 0usize;
    let mut first = true;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // Past the cap the line is only *counted*: splitting it would allocate
        // a String per field for a row nothing ever shows (a 41 MB export
        // spent most of its open time on four million throwaway Strings).
        // Field count is still exact — one more than the outside-quote
        // delimiters, which is what `parse_line` produces.
        if !first && rows.len() >= MAX_ROWS {
            total_rows += 1;
            cols = cols.max(count_outside_quotes(line, delimiter) + 1);
            continue;
        }
        let fields = parse_line(line, delimiter);
        cols = cols.max(fields.len());
        if first {
            header = fields;
            first = false;
        } else {
            total_rows += 1;
            rows.push(fields);
        }
    }
    cols = cols.max(header.len());
    for row in &mut rows {
        row.resize_with(cols, Default::default);
    }
    header.resize_with(cols, Default::default);
    let capped = total_rows > rows.len();
    Sheet {
        header,
        rows,
        total_rows,
        cols,
        capped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_commas_tabs_and_pipes() {
        assert_eq!(sniff("a,b\n1,2\n"), ',');
        assert_eq!(sniff("a\tb\n1\t2\n"), '\t');
        assert_eq!(sniff("a;b\n1;2\n"), ';');
        assert_eq!(sniff("a|b\n1|2\n"), '|');
        assert_eq!(sniff("no delimiters here\n"), ',');
    }

    #[test]
    fn quoted_delimiters_do_not_split() {
        assert_eq!(parse_line("\"a,b\",c", ','), vec!["a,b", "c"]);
        assert_eq!(parse_line("\"a\"\"b\",c", ','), vec!["a\"b", "c"]);
    }

    #[test]
    fn header_plus_ragged_rows_pad() {
        let s = parse_with_ext("a,b,c\n1,2\n3,4,5,6\n", None);
        assert_eq!(s.header, vec!["a", "b", "c", ""]);
        assert_eq!(s.rows.len(), 2);
        assert_eq!(s.rows[0], vec!["1", "2", "", ""]);
        assert_eq!(s.cols, 4);
        assert_eq!(s.total_rows, 2);
        assert!(!s.capped);
    }

    #[test]
    fn blanks_are_skipped_and_empty_is_empty() {
        let s = parse_with_ext("\n  \n", None);
        assert_eq!(s.total_rows, 0);
        assert!(s.rows.is_empty());
        assert_eq!(s.cols, 0);
    }

    #[test]
    fn a_tsv_extension_pins_the_tab_even_when_prose_says_comma() {
        // The reproduction of the real report: a tab-delimited sheet where one
        // row was typed with spaces, and a later comma in a note field. The
        // sniffer counts commas; the extension must still win.
        let text = "Step\tAction\tResult\n1\topen\ta tab appears\n6 hello yuppers\n";
        let s = parse_with_ext(text, Some("tsv"));
        assert_eq!(s.header, vec!["Step", "Action", "Result"]);
        assert_eq!(s.rows[0], vec!["1", "open", "a tab appears"]);
        // The space row is one field, padded to the sheet's shape — its own
        // widened row, not a delimiter shift.
        assert_eq!(s.rows[1], vec!["6 hello yuppers", "", ""]);
    }

    #[test]
    fn a_csv_extension_keeps_the_sniffer_in_charge() {
        // Semicolon dialect: a pin must not land here — the sniffer decides,
        // and for semicolon text it reads ';' (a false tab pin would split on
        // the comma and turn that row into one wide cell).
        let s = parse_with_ext("a;b\n1;2\n", Some("csv"));
        assert_eq!(s.header, vec!["a", "b"]);
    }
    #[test]
    fn a_plain_sniff_without_a_kind_still_reads_tabs() {
        // No extension given: the sniffer reads the tabs because the tab count
        // over the first lines is the highest.
        let s = parse_with_ext("a\tb\n1\t2\n", None);
        assert_eq!(s.header, vec!["a", "b"]);
    }

    /// Rows past the cap are not split, but the column count they imply is
    /// still honoured: the sheet's shape must not depend on how long the file
    /// is, only on what it contains.
    #[test]
    fn a_wide_row_past_the_cap_still_widens_the_sheet() {
        let mut text = String::from("h1,h2\n");
        for i in 0..MAX_ROWS + 10 {
            if i == MAX_ROWS + 5 {
                text.push_str("a,b,c,d\n");
            } else {
                text.push_str("a,b\n");
            }
        }
        let s = parse_with_ext(&text, None);
        assert_eq!(s.rows.len(), MAX_ROWS);
        assert_eq!(s.total_rows, MAX_ROWS + 10);
        assert_eq!(s.cols, 4);
        assert_eq!(s.rows[0].len(), 4, "kept rows are padded to the shape");
        assert!(s.capped);
    }

    #[test]
    fn huge_files_keep_first_max_rows_and_count_the_rest() {
        let mut text = String::from("h1,h2\n");
        for i in 0..MAX_ROWS + 50 {
            text.push_str(&format!("{i},x\n"));
        }
        let s = parse_with_ext(&text, None);
        assert_eq!(s.rows.len(), MAX_ROWS);
        assert_eq!(s.total_rows, MAX_ROWS + 50);
        assert!(s.capped);
    }
}
