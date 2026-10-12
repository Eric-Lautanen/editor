//! Text buffer. v0: Vec<String> lines. Replace with gap buffer/piece table if needed.
#[derive(Default, Clone)]
pub struct Buffer {
    lines: Vec<String>,
    pub dirty: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    /// Byte offset within the line (always on a char boundary).
    pub col: usize,
}

impl Buffer {
    pub fn from_text(s: &str) -> Self {
        let mut lines: Vec<String> = s
            .split('\n')
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self {
            lines,
            dirty: false,
        }
    }
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    pub fn line(&self, i: usize) -> &str {
        &self.lines[i]
    }
    pub fn text(&self) -> String {
        self.lines.join("\n")
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
        let removed = self.range_text(from, to);
        let tail = self.lines[to.line][to.col..].to_string();
        self.lines[from.line].truncate(from.col);
        self.lines[from.line].push_str(&tail);
        self.lines.drain(from.line + 1..=to.line);
        self.dirty = true;
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
}
