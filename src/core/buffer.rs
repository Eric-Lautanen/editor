//! Text buffer. v0: Vec<String> lines. Replace with gap buffer/piece table if needed.
use std::sync::atomic::{AtomicU64, Ordering};

/// Content revision source. Caches (parsed records, sheets, log counts) key on
/// this; a global counter rather than a per-buffer one means replacing a
/// buffer (reload) or creating one from text still produces a new revision.
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

fn next_revision() -> u64 {
    NEXT_REVISION.fetch_add(1, Ordering::Relaxed)
}

/// How many line-range edits a buffer remembers before it stops trusting the
/// log. One keystroke is one span, and a paint consumes them, so a frame
/// normally sees one or two; the cap only bounds a tab edited while nothing
/// consumes its log (Edit mode) or a burst between two frames.
const EDIT_LOG_CAP: usize = 64;

/// What one edit did to the buffer's line list, in the coordinates of the
/// text *after* it was applied: the lines `[start, start + removed)` were
/// replaced by the `inserted` lines `[start, start + inserted)`.
///
/// This is what lets a plan patch itself instead of re-measuring the whole
/// document: only the inserted range's units need their heights counted
/// again, and everything below shifts by the difference.
///
/// Line spans are all a line-indexed plan needs; `at` and `delta` are for the
/// documents indexed by *source offset* (a JSON tree's node ranges). Both are
/// recorded where the mutation happens, where they are exact: a plan that
/// searched for them afterwards would have to reconstruct text that is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditSpan {
    pub start: usize,
    pub removed: usize,
    pub inserted: usize,
    /// Byte offset of the edit's first byte — the same offset in the text
    /// before it and the text after, since every byte above it is untouched.
    pub at: usize,
    /// How much the document's byte length changed. Every mutation is a pure
    /// insertion or a pure deletion of one run, so the removed run's length is
    /// the negative part of this and the inserted run's the positive one.
    pub delta: isize,
}

impl EditSpan {
    /// Byte length of the run this edit removed (0 for a pure insertion).
    pub fn removed_bytes(&self) -> usize {
        (-self.delta).max(0) as usize
    }

    /// Byte length of the run this edit inserted (0 for a pure deletion).
    pub fn inserted_bytes(&self) -> usize {
        self.delta.max(0) as usize
    }
}

#[derive(Default, Clone)]
pub struct Buffer {
    lines: Vec<String>,
    pub dirty: bool,
    /// Changes on every mutation and on construction, so a caller can cheaply
    /// tell whether parsed data derived from this buffer is still current.
    revision: u64,
    /// True while the buffer is a freshly built one whose text ended in a
    /// newline: `split('\n')` leaves a trailing empty line there, and a
    /// streaming load's first appended line belongs *in* that slot, not after
    /// it. Without this the stream shows one phantom blank line where the head
    /// meets the tail (and the sealed rebuild then silently removes it).
    trailing_slot: bool,
    /// Every edit since a caller last took them, oldest first.
    edits: Vec<EditSpan>,
    /// The log overflowed and no longer describes the text: a consumer must
    /// rebuild from the document rather than replay it.
    edits_lost: bool,
    /// [`Buffer::text`]'s length, kept current by every mutation. The join's
    /// length is asked for on every append and every keystroke, and summing
    /// the lines there made a streamed load quadratic — 400 000 appended lines
    /// re-walked the whole document each time.
    bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    /// Byte offset within the line (always on a char boundary).
    pub col: usize,
}

impl Buffer {
    pub fn from_text(s: &str) -> Self {
        #[cfg(any(test, feature = "mem-census"))]
        crate::mem::begin(crate::mem::Component::Buffer);
        let mut lines: Vec<String> = s
            .split('\n')
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        #[cfg(any(test, feature = "mem-census"))]
        crate::mem::end();
        let bytes = lines.iter().map(|l| l.len()).sum::<usize>() + lines.len().saturating_sub(1);
        Self {
            lines,
            dirty: false,
            revision: next_revision(),
            trailing_slot: s.ends_with('\n'),
            edits: Vec::new(),
            edits_lost: false,
            bytes,
        }
    }

    /// Append one more line while a streaming load is filling the buffer
    /// (`src/load.rs`). The loader appends *complete* lines to a buffer
    /// whose head was built by `from_text`, so the head's own trailing
    /// placeholder (an empty last element when the text ends with a
    /// newline) is consumed by the first append: the first streamed line
    /// takes its slot instead of stacking an empty line above it.
    fn note_edit(
        &mut self,
        start: usize,
        removed: usize,
        inserted: usize,
        at: usize,
        delta: isize,
    ) {
        if self.edits_lost {
            return;
        }
        if self.edits.len() >= EDIT_LOG_CAP {
            self.edits.clear();
            self.edits_lost = true;
            return;
        }
        self.edits.push(EditSpan {
            start,
            removed,
            inserted,
            at,
            delta,
        });
    }

    /// Take the edits since the last call, or `None` when the log overflowed
    /// and the caller must rebuild from the document. Taking clears the log
    /// either way.
    pub fn take_edits(&mut self) -> Option<Vec<EditSpan>> {
        let lost = std::mem::take(&mut self.edits_lost);
        let edits = std::mem::take(&mut self.edits);
        if lost || edits.is_empty() {
            None
        } else {
            Some(edits)
        }
    }

    /// Discard the log without using it: for a caller that is about to rebuild
    /// its caches from the whole document anyway, so the next edit starts a
    /// fresh log instead of hitting the cap.
    pub fn forget_edits(&mut self) {
        self.edits.clear();
        self.edits_lost = false;
    }

    /// An append is one unit added at the end, or the head's placeholder slot
    /// filled. Consecutive appends coalesce: a streaming load adds thousands
    /// of lines between two paints, and one span per line would hit the cap
    /// and turn every stream into a rebuild.
    fn note_append(&mut self, start: usize, removed: usize, at: usize, delta: isize) {
        if self.edits_lost {
            return;
        }
        if let Some(last) = self.edits.last_mut() {
            if last.start + last.inserted == start && last.removed <= 1 && removed <= 1 {
                // The run grows at `last.at`, which stays the earliest byte of
                // the coalesced span.
                last.inserted += 1;
                last.delta += delta;
                return;
            }
        }
        self.note_edit(start, removed, 1, at, delta);
    }

    pub fn append_line(&mut self, line: &str) {
        // The loader hands over complete lines, so a trailing `\r` is a CRLF
        // artefact: `from_text` trims it, and a streamed buffer has to match
        // that text exactly or the band (and every row count on it) disagrees
        // with the sealed pane — a `\r` even fails the mono fast path, taking
        // the per-character font measurement for every line.
        let line = line.trim_end_matches('\r');
        // The head's trailing placeholder (the empty last element of a text
        // that ends in a newline, or the one empty line of an empty head) is
        // where the first streamed line goes: it takes that slot instead of
        // stacking a blank line above the tail.
        let placeholder = self.trailing_slot
            || (self.lines.len() == 1 && self.lines[0].is_empty())
            || self.lines.is_empty();
        // Appending is an insertion at the document's end, whether the line
        // takes the head's placeholder slot (an empty last line, already
        // there) or is pushed after a separator.
        let at_byte = self.byte_len();
        let delta = (line.len() + usize::from(!placeholder)) as isize;
        let at = self.lines.len();
        match self.lines.last_mut() {
            Some(slot) if placeholder => {
                slot.clear();
                slot.push_str(line);
                self.note_append(at.saturating_sub(1), 1, at_byte, delta);
            }
            _ => {
                self.lines.push(line.to_string());
                self.note_append(at, 0, at_byte, delta);
            }
        }
        self.bytes = (self.bytes as isize + delta) as usize;
        self.trailing_slot = false;
        self.revision = next_revision();
    }
    /// Close a stream-filled buffer: content is final, nothing is unsaved,
    /// and the next append is a plain push (the head's slot was long used).
    /// Mints a revision like any other change, so caches keyed on the old one
    /// rebuild against the sealed text.
    pub fn sealed(&mut self) {
        self.trailing_slot = false;
        self.dirty = false;
        self.revision = next_revision();
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    /// Changes on every content mutation and on construction. Two calls with
    /// the same value can only describe the same text.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn line(&self, i: usize) -> &str {
        &self.lines[i]
    }
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Byte length of [`Buffer::text`]: the document's size in bytes.
    ///
    /// The join's own cost, without building it — an edit's byte delta is a
    /// difference of this against the length the last parse saw; and, being a
    /// field rather than a sum of the lines, it does not turn a streamed load
    /// into a quadratic walk.
    pub fn byte_len(&self) -> usize {
        self.bytes
    }

    /// Byte offset [`Buffer::line`]`(i)` starts at in [`Buffer::text`]. One
    /// past the last line is the text's length, not one more: only the lines
    /// above the last are followed by a separator.
    pub fn line_start_byte(&self, i: usize) -> usize {
        if i >= self.lines.len() {
            return self.byte_len();
        }
        self.lines[..i].iter().map(|l| l.len() + 1).sum()
    }

    /// The document's bytes in `[start, end)` — exactly the text
    /// [`Buffer::text`] has there, separators included.
    ///
    /// A node's source range is a byte range, so re-parsing that node means
    /// cutting its bytes back out of the document; the offsets that come back
    /// from the re-parse are only usable if the slice is the document's own
    /// text and nothing else.
    pub fn byte_slice(&self, start: usize, end: usize) -> String {
        let end = end.min(self.byte_len());
        let mut out = String::with_capacity(end.saturating_sub(start));
        let last = self.lines.len().saturating_sub(1);
        let mut at = 0usize;
        for (i, line) in self.lines.iter().enumerate() {
            // Past the range: the lines below it are not part of the slice.
            if at >= end {
                break;
            }
            let line_end = at + line.len();
            let a = start.max(at);
            let b = end.min(line_end);
            if b > a {
                out.push_str(&line[a - at..b - at]);
            }
            // The '\n' the join puts after every line but the last.
            if i < last && (start..end).contains(&line_end) {
                out.push('\n');
            }
            at = line_end + usize::from(i < last);
        }
        out
    }

    /// Clamp a position into the buffer, snapping the column to a char boundary.
    pub fn clamp(&self, at: Pos) -> Pos {
        let line = at.line.min(self.line_count().saturating_sub(1));
        let text = &self.lines[line];
        let mut col = at.col.min(text.len());
        while !text.is_char_boundary(col) {
            col -= 1;
        }
        Pos { line, col }
    }

    /// End of the buffer (after the last character).
    pub fn end(&self) -> Pos {
        let line = self.line_count().saturating_sub(1);
        Pos {
            line,
            col: self.lines[line].len(),
        }
    }

    /// Advance a position forward over `text` (used to invert edits).
    pub fn advance(&self, at: Pos, text: &str) -> Pos {
        let at = self.clamp(at);
        let mut line = at.line;
        let mut col = at.col;
        let mut first = true;
        for chunk in text.split('\n') {
            if first {
                col += chunk.len();
                first = false;
            } else {
                line += 1;
                col = chunk.len();
            }
        }
        Pos { line, col }
    }

    /// Insert text at `at` (which may contain newlines). Returns the position
    /// after the inserted text. Marks the buffer dirty.
    pub fn insert(&mut self, at: Pos, text: &str) -> Pos {
        let at = self.clamp(at);
        let at_byte = self.line_start_byte(at.line) + at.col;
        let tail = self.lines[at.line][at.col..].to_string();
        self.lines[at.line].truncate(at.col);
        let mut parts = text.split('\n');
        self.lines[at.line].push_str(parts.next().unwrap_or(""));
        let mut end = Pos {
            line: at.line,
            col: self.lines[at.line].len(),
        };
        for part in parts {
            end.line += 1;
            self.lines.insert(end.line, part.to_string());
            end.col = part.len();
        }
        // The original tail reattaches after the caret.
        self.lines[end.line].push_str(&tail);
        self.dirty = true;
        self.trailing_slot = false;
        self.revision = next_revision();
        self.bytes += text.len();
        // The insertion replaced the one line it started in with that line's
        // head plus one line per newline plus the reattached tail.
        let inserted = 1 + text.bytes().filter(|&b| b == b'\n').count();
        // Inserting text is a pure insertion of bytes: the touched line's
        // original content is reattached after them.
        self.note_edit(at.line, 1, inserted, at_byte, text.len() as isize);
        end
    }

    /// Delete the half-open range `[from, to)` and return the removed text.
    /// Empty ranges remove nothing. Marks the buffer dirty when text goes.
    pub fn delete(&mut self, from: Pos, to: Pos) -> String {
        let (from, to) = (self.clamp(from), self.clamp(to));
        let (from, to) = if from <= to { (from, to) } else { (to, from) };
        if from == to {
            return String::new();
        }
        let at_byte = self.line_start_byte(from.line) + from.col;
        let removed = self.range_text(from, to);
        let tail = self.lines[to.line][to.col..].to_string();
        self.lines[from.line].truncate(from.col);
        self.lines[from.line].push_str(&tail);
        self.lines.drain(from.line + 1..=to.line);
        self.dirty = true;
        self.trailing_slot = false;
        self.revision = next_revision();
        self.bytes -= removed.len();
        // The removed range collapsed into the one merged line it left.
        self.note_edit(
            from.line,
            to.line - from.line + 1,
            1,
            at_byte,
            -(removed.len() as isize),
        );
        removed
    }

    /// Read the half-open range `[from, to)` without mutating.
    pub fn range_text(&self, from: Pos, to: Pos) -> String {
        let (from, to) = (self.clamp(from), self.clamp(to));
        let (from, to) = if from <= to { (from, to) } else { (to, from) };
        if from.line == to.line {
            return self.lines[from.line][from.col..to.col].to_string();
        }
        let mut out = String::from(&self.lines[from.line][from.col..]);
        for line in from.line + 1..to.line {
            out.push('\n');
            out.push_str(&self.lines[line]);
        }
        out.push('\n');
        out.push_str(&self.lines[to.line][..to.col]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let b = Buffer::from_text("a\r\nb\nc");
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.text(), "a\nb\nc");
    }

    /// A streamed load appends complete lines to a head built by `from_text`:
    /// the result has to be the text a single `from_text` of the whole file
    /// would have produced, CRLF endings included.
    #[test]
    fn streamed_appends_land_in_the_head_placeholder() {
        let mut streamed = Buffer::from_text("one\ntwo\n");
        for line in ["three", "four\r", "five"] {
            streamed.append_line(line);
        }
        let whole = Buffer::from_text("one\ntwo\nthree\nfour\nfive");
        assert_eq!(streamed.text(), whole.text());
        assert_eq!(streamed.line_count(), whole.line_count());
        // An empty head is one empty line, not a blank above the first row.
        let mut empty = Buffer::from_text("");
        empty.append_line("first");
        empty.append_line("second");
        assert_eq!(empty.text(), "first\nsecond");
    }

    #[test]
    fn insert_split_and_join() {
        let mut b = Buffer::from_text("ab\ncd");
        let end = b.insert(Pos { line: 0, col: 1 }, "X\nY");
        assert_eq!(end, Pos { line: 1, col: 1 });
        assert_eq!(b.text(), "aX\nYb\ncd");
        assert!(b.dirty);
        let end = b.insert(Pos { line: 2, col: 2 }, "!");
        assert_eq!(b.text(), "aX\nYb\ncd!");
        assert_eq!(end, Pos { line: 2, col: 3 });
    }

    #[test]
    fn delete_ranges() {
        let mut b = Buffer::from_text("ab\ncd\nef");
        assert_eq!(
            b.delete(Pos { line: 0, col: 1 }, Pos { line: 0, col: 1 }),
            ""
        );
        assert_eq!(
            b.delete(Pos { line: 0, col: 1 }, Pos { line: 2, col: 1 }),
            "b\ncd\ne"
        );
        assert_eq!(b.text(), "af");
        // Reversed ranges normalize.
        let mut b = Buffer::from_text("hello");
        assert_eq!(
            b.delete(Pos { line: 0, col: 5 }, Pos { line: 0, col: 2 }),
            "llo"
        );
        assert_eq!(b.text(), "he");
    }

    #[test]
    fn positions_clamp_to_boundaries() {
        let mut b = Buffer::from_text("é\nx");
        // "é" is 2 bytes; col 1 is mid-character.
        assert_eq!(b.clamp(Pos { line: 0, col: 1 }), Pos { line: 0, col: 0 });
        assert_eq!(b.clamp(Pos { line: 9, col: 99 }), Pos { line: 1, col: 1 });
        // Insert/delete round-trip through advance().
        let at = Pos { line: 0, col: 0 };
        let end = b.insert(at, "a\nb");
        assert_eq!(b.advance(at, "a\nb"), end);
        assert_eq!(b.delete(at, end), "a\nb");
        assert_eq!(b.text(), "é\nx");
    }

    /// The log is what a plan patches itself with, so its spans have to be in
    /// the coordinates of the text after each edit and replay in order.
    #[test]
    fn edits_are_logged_in_the_coordinates_of_the_text_after_them() {
        let mut b = Buffer::from_text("one\ntwo\nthree");
        b.insert(Pos { line: 1, col: 3 }, "\nnew");
        assert_eq!(
            b.take_edits().unwrap(),
            vec![EditSpan {
                start: 1,
                removed: 1,
                inserted: 2,
                at: 7,
                delta: 4,
            }]
        );
        // Taking clears: nothing is replayed twice.
        assert!(b.take_edits().is_none());
        b.delete(Pos { line: 0, col: 0 }, Pos { line: 2, col: 0 });
        assert_eq!(
            b.take_edits().unwrap(),
            vec![EditSpan {
                start: 0,
                removed: 3,
                inserted: 1,
                at: 0,
                delta: -8,
            }]
        );
        // An insertion with no newline still replaces the line it landed on.
        b.insert(Pos { line: 0, col: 0 }, "x");
        assert_eq!(
            b.take_edits().unwrap(),
            vec![EditSpan {
                start: 0,
                removed: 1,
                inserted: 1,
                at: 0,
                delta: 1,
            }]
        );
    }

    /// Apply one logged edit's byte fields to `before` and require the text
    /// after to be exactly that splice of it. `at`/`delta` are what a
    /// source-indexed cache patches with, so the bytes above the edit must be
    /// identical in both texts and so must the bytes below it — otherwise a
    /// node's recorded range would move for the wrong reason.
    fn splice_matches(b: &mut Buffer, before: &str) -> String {
        let after = b.text();
        for e in b.take_edits().expect("the edit was logged") {
            let (removed, inserted) = (e.removed_bytes(), e.inserted_bytes());
            let mut spliced = String::with_capacity(before.len() + inserted);
            spliced.push_str(&before[..e.at]);
            spliced.push_str(&after[e.at..e.at + inserted]);
            spliced.push_str(&before[e.at + removed..]);
            assert_eq!(spliced, after, "{e:?} does not describe the splice");
        }
        after
    }

    /// Every mutation is a pure insertion or a pure deletion of one run, so a
    /// span's byte fields reconstruct the text exactly — for typing, splitting
    /// lines, deleting across lines, wide glyphs and streamed appends.
    #[test]
    fn a_spans_byte_fields_describe_the_splice() {
        let mut b = Buffer::from_text("alpha\nbeta\ngamma\ndelta\n");
        let mut before = b.text();
        b.insert(Pos { line: 1, col: 2 }, "XY");
        before = splice_matches(&mut b, &before);
        b.insert(Pos { line: 0, col: 3 }, "\n");
        before = splice_matches(&mut b, &before);
        b.delete(Pos { line: 2, col: 0 }, Pos { line: 3, col: 4 });
        before = splice_matches(&mut b, &before);
        b.insert(Pos { line: 0, col: 0 }, "é𝄞\n");
        before = splice_matches(&mut b, &before);
        b.delete(Pos { line: 0, col: 2 }, Pos { line: 1, col: 1 });
        before = splice_matches(&mut b, &before);
        b.append_line("tail");
        before = splice_matches(&mut b, &before);
        // And a streamed burst is one span whose bytes cover all of it.
        for i in 0..50 {
            b.append_line(&format!("row {i}"));
        }
        splice_matches(&mut b, &before);
    }

    /// Past the cap the log stops describing the text: the consumer has to be
    /// told to rebuild, not handed a partial replay.
    #[test]
    fn an_overflowing_log_reports_that_it_was_lost() {
        let mut b = Buffer::from_text("x");
        for _ in 0..100 {
            b.insert(Pos { line: 0, col: 0 }, "y");
        }
        assert!(b.take_edits().is_none(), "a full log must not replay");
        // And the next edit starts a fresh log.
        b.insert(Pos { line: 0, col: 0 }, "z");
        assert!(b.take_edits().is_some());
    }

    /// A streamed load appends thousands of lines between paints; that is one
    /// growing span, not thousands of entries that overflow the cap.
    #[test]
    fn a_streamed_load_logs_one_span() {
        let mut b = Buffer::from_text("one\n");
        let before = b.byte_len();
        for i in 0..10_000 {
            b.append_line(&format!("line {i}"));
        }
        let edits = b.take_edits().expect("a coalesced span");
        assert_eq!(edits.len(), 1, "{edits:?}");
        assert_eq!(edits[0].start, 1);
        assert_eq!(edits[0].removed, 1);
        assert_eq!(edits[0].inserted, 10_000);
        // The coalesced run starts where the document ended and accounts for
        // every byte the appends added.
        assert_eq!(edits[0].at, before);
        assert_eq!(edits[0].delta, (b.byte_len() - before) as isize);
    }

    /// The byte helpers have to agree with `text()` exactly — a node's slice
    /// and the offsets a re-parse reports are read back against each other.
    #[test]
    fn byte_offsets_match_the_joined_text() {
        let src = "é{\n  \"a\": 1,\n\t\"b\": \"𝄞\"\n}\n";
        let buf = Buffer::from_text(src);
        let text = buf.text();
        assert_eq!(buf.byte_len(), text.len());
        for i in 0..=buf.line_count() {
            let at = buf.line_start_byte(i);
            assert!(text.is_char_boundary(at), "line {i} starts mid-character");
            if i < buf.line_count() {
                assert!(text[at..].starts_with(buf.line(i)), "line {i}");
            }
        }
        // Every boundary-aligned range comes back byte for byte.
        let bounds: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .collect();
        for &a in &bounds {
            for &b in bounds.iter().filter(|&&b| b >= a) {
                assert_eq!(buf.byte_slice(a, b), text[a..b], "[{a}, {b})");
            }
        }
        // An empty document, and one whose text ends in a newline.
        assert_eq!(Buffer::from_text("").byte_len(), 0);
        assert_eq!(Buffer::from_text("\n").byte_len(), 1);
        assert_eq!(Buffer::from_text("a\n").byte_slice(0, 2), "a\n");
        // Out-of-range ends clamp to the document.
        assert_eq!(buf.byte_slice(0, usize::MAX), text);
    }

    /// Caches key on this: every construction and mutation must mint a new
    /// revision, including a reload that replaces the whole buffer.
    #[test]
    fn revisions_are_unique_across_loads_and_edits() {
        let a = Buffer::from_text("one");
        let b = Buffer::from_text("one");
        assert_ne!(a.revision(), b.revision(), "two loads shared a revision");
        let mut c = a.clone();
        assert_eq!(c.revision(), a.revision(), "a clone is the same content");
        c.insert(Pos { line: 0, col: 3 }, " two");
        assert_ne!(c.revision(), a.revision(), "an edit kept the revision");
        let before = c.revision();
        c.delete(Pos { line: 0, col: 0 }, Pos { line: 0, col: 4 });
        assert_ne!(c.revision(), before, "a delete kept the revision");
    }
}
