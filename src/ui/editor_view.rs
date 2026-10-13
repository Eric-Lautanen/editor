//! Editor state and command execution (M5): caret movement, selection,
//! typing, clipboard-friendly copy/cut, and undo/redo on top of [`Buffer`].
//!
//! Every mutation goes through [`EditorView`] so the undo stack sees one edit
//! per gesture; movement is pure caret math with no buffer side effects.

use crate::core::{
    buffer::{Buffer, Pos},
    undo::{Edit, UndoStack},
};
use crate::gfx::geom::Rect;
use crate::layout::{
    display_list::DrawOp,
    text::{caret_point, hit_test, line_height, selection_rects},
    Ctx,
};
use crate::ui::event::{Key, Modifiers};

#[derive(Default)]
pub struct EditorView {
    pub caret: Pos,
    pub anchor: Option<Pos>,
    undo: UndoStack,
    /// Target column (bytes) for vertical navigation; cleared by horizontal
    /// moves and edits so long lines don't drag the caret sideways.
    goal: Option<usize>,
}

/// One character to the left (wrapping up a line at column 0), or `p` itself
/// at the start of the buffer.
fn left(buf: &Buffer, p: Pos) -> Pos {
    if p.col == 0 {
        if p.line == 0 {
            return p;
        }
        return Pos {
            line: p.line - 1,
            col: buf.line(p.line - 1).len(),
        };
    }
    let prev = buf.line(p.line)[..p.col]
        .chars()
        .next_back()
        .map_or(0, |c| c.len_utf8());
    Pos {
        line: p.line,
        col: p.col - prev,
    }
}

/// Character class for word selection: whitespace, word, punctuation.
fn word_class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// One character to the right (wrapping down at end of line), or `p` itself
/// at the end of the buffer.
fn right(buf: &Buffer, p: Pos) -> Pos {
    let line = buf.line(p.line);
    if p.col >= line.len() {
        if p.line + 1 >= buf.line_count() {
            return p;
        }
        return Pos {
            line: p.line + 1,
            col: 0,
        };
    }
    let c = line[p.col..].chars().next().unwrap_or('\0');
    Pos {
        line: p.line,
        col: p.col + c.len_utf8(),
    }
}

/// One line up/down at the same column (clamped later), or `None` at the edge.
fn vertical(buf: &Buffer, p: Pos, down: bool) -> Option<Pos> {
    let line = if down {
        p.line + 1
    } else {
        p.line.checked_sub(1)?
    };
    if line >= buf.line_count() {
        return None;
    }
    Some(Pos { line, col: p.col })
}

impl EditorView {
    /// Selection in reading order; `None` when collapsed.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        self.anchor.filter(|&a| a != self.caret).map(|a| {
            if a <= self.caret {
                (a, self.caret)
            } else {
                (self.caret, a)
            }
        })
    }

    fn apply_caret(&mut self, at: Pos, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        self.caret = at;
    }

    /// Pure caret movement. Returns `true` when caret or selection changed.
    pub fn move_key(&mut self, buf: &Buffer, key: Key, mods: Modifiers) -> bool {
        let extend = mods.shift;
        let before = (self.caret, self.anchor);
        match key {
            Key::Left | Key::Right => {
                self.goal = None;
                let next = if extend {
                    if key == Key::Left {
                        left(buf, self.caret)
                    } else {
                        right(buf, self.caret)
                    }
                } else {
                    // Out of a selection, the first press lands on the edge in
                    // the direction of travel instead of stepping past it.
                    match self.selection() {
                        Some((from, to)) => {
                            if key == Key::Left {
                                from
                            } else {
                                to
                            }
                        }
                        None => {
                            if key == Key::Left {
                                left(buf, self.caret)
                            } else {
                                right(buf, self.caret)
                            }
                        }
                    }
                };
                self.apply_caret(next, extend);
            }
            Key::Up | Key::Down => {
                let down = key == Key::Down;
                // Track the wanted column from the moment navigation starts.
                if self.goal.is_none() {
                    self.goal = Some(self.caret.col);
                }
                if let Some(target) = vertical(buf, self.caret, down) {
                    let col = self.goal.unwrap_or(target.col);
                    let next = buf.clamp(Pos {
                        line: target.line,
                        col: col.min(buf.line(target.line).len()),
                    });
                    self.apply_caret(next, extend);
                }
            }
            Key::Home | Key::End => {
                self.goal = None;
                let col = if key == Key::Home {
                    0
                } else {
                    buf.line(self.caret.line).len()
                };
                self.apply_caret(
                    buf.clamp(Pos {
                        line: self.caret.line,
                        col,
                    }),
                    extend,
                );
            }
            _ => return false,
        }
        (self.caret, self.anchor) != before
    }

    pub fn select_all(&mut self, buf: &Buffer) {
        self.anchor = Some(Pos { line: 0, col: 0 });
        self.caret = buf.end();
        self.goal = None;
    }

    /// Select the run of same-class characters around `at` (double-click).
    /// Classes: whitespace, word (`alphanumeric` + `_`), punctuation.
    pub fn select_word(&mut self, buf: &Buffer, at: Pos) {
        let line = buf.line(at.line);
        let chars: Vec<(usize, char)> = line.char_indices().collect();
        if chars.is_empty() {
            self.anchor = None;
            self.goal = None;
            return;
        }
        // Map the click column to the char it lands in. Exact-boundary clicks
        // pick that char; a click in the middle of a multibyte char or in the
        // gap between two chars picks the char whose byte range contains the
        // column (or the preceding one if we're before any char).
        let k = match chars.binary_search_by_key(&at.col, |(i, _)| *i) {
            Ok(k) => k,
            Err(k) => k.saturating_sub(1),
        };
        // Expand outward to the full same-class run around `k`.
        let cls = word_class(chars[k].1);
        let mut a = k;
        let mut b = k;
        while a > 0 && word_class(chars[a - 1].1) == cls {
            a -= 1;
        }
        while b + 1 < chars.len() && word_class(chars[b + 1].1) == cls {
            b += 1;
        }
        let (start, end) = (chars[a].0, chars[b].0 + chars[b].1.len_utf8());
        self.anchor = Some(Pos {
            line: at.line,
            col: start,
        });
        self.caret = Pos {
            line: at.line,
            col: end,
        };
        self.goal = None;
    }

    /// Select the whole line at `at`, column 0 through the line end
    /// (triple-click).
    pub fn select_line(&mut self, buf: &Buffer, at: Pos) {
        let line = at.line.min(buf.line_count().saturating_sub(1));
        self.anchor = Some(Pos { line, col: 0 });
        self.caret = Pos {
            line,
            col: buf.line(line).len(),
        };
        self.goal = None;
    }

    /// Insert `text` at the caret (or over the selection). Newlines split.
    pub fn insert_text(&mut self, buf: &mut Buffer, text: &str) {
        if let Some((from, to)) = self.selection() {
            let removed = buf.delete(from, to);
            self.undo.push(Edit::Delete {
                from,
                text: removed,
            });
            self.caret = from;
        }
        let end = buf.insert(self.caret, text);
        self.undo.push(Edit::Insert {
            at: self.caret,
            text: text.to_string(),
        });
        self.caret = end;
        self.anchor = None;
        self.goal = None;
    }

    /// Delete one character (or the whole selection) forward or backward.
    /// Backspace runs merge into a single undo step via `UndoStack`.
    pub fn delete_key(&mut self, buf: &mut Buffer, forward: bool) {
        self.goal = None;
        if let Some((from, to)) = self.selection() {
            let removed = buf.delete(from, to);
            self.caret = from;
            self.anchor = None;
            self.undo.push(Edit::Delete {
                from,
                text: removed,
            });
            return;
        }
        let target = if forward {
            right(buf, self.caret)
        } else {
            left(buf, self.caret)
        };
        if target == self.caret {
            return;
        }
        let (from, to) = if forward {
            (self.caret, target)
        } else {
            (target, self.caret)
        };
        let removed = buf.delete(from, to);
        self.caret = from;
        self.undo.push(Edit::Delete {
            from,
            text: removed,
        });
    }

    // ------------------------------------------------------------- clipboard

    /// Selected text; empty when the selection is collapsed.
    pub fn sel_text(&self, buf: &Buffer) -> String {
        match self.selection() {
            Some((from, to)) => buf.range_text(from, to),
            None => String::new(),
        }
    }

    /// Copy selection; `None` when there is nothing to put on the clipboard.
    pub fn copy(&self, buf: &Buffer) -> Option<String> {
        let text = self.sel_text(buf);
        (!text.is_empty()).then_some(text)
    }

    /// Cut the selection as one undo step; `None` when nothing is selected.
    pub fn cut(&mut self, buf: &mut Buffer) -> Option<String> {
        let (from, to) = self.selection()?;
        let removed = buf.delete(from, to);
        self.caret = from;
        self.anchor = None;
        self.goal = None;
        self.undo.push(Edit::Delete {
            from,
            text: removed.clone(),
        });
        Some(removed)
    }

    /// Paste, replacing any selection. Each half is an ordinary undo step.
    pub fn paste(&mut self, buf: &mut Buffer, text: &str) {
        self.insert_text(buf, text);
    }

    // ---------------------------------------------------------------- history

    /// Undo the newest edit. Returns `false` at the start of history.
    pub fn undo(&mut self, buf: &mut Buffer) -> bool {
        let Some(edit) = self.undo.pop_undo() else {
            return false;
        };
        // Deleting restores text by re-inserting it; the caret should land
        // back at the deletion point (where typing would resume).
        self.caret = edit.invert(buf);
        self.anchor = None;
        self.goal = None;
        true
    }

    /// Redo the most recently undone edit. Returns `false` at the end.
    pub fn redo(&mut self, buf: &mut Buffer) -> bool {
        let Some(edit) = self.undo.pop_redo() else {
            return false;
        };
        self.caret = edit.apply(buf);
        self.anchor = None;
        self.goal = None;
        true
    }

    // ------------------------------------------------------------- painting

    /// Document-space caret (x, row top). Panes offset by their scroll at paint.
    /// The editor pane never wraps: one row per line, horizontal scroll.
    pub fn caret_xy(&self, buf: &Buffer, viewport_w: f32, ctx: &Ctx) -> (f32, f32) {
        caret_point(buf, self.caret, viewport_w, ctx, false)
    }

    /// Line height of the editor pane grid, for caret size and scrolling.
    pub fn line_height(&self, viewport_w: f32, ctx: &Ctx) -> f32 {
        line_height(viewport_w, ctx)
    }

    /// Append selection + caret draw ops on top of a laid-out editor pane.
    /// `show_caret` carries the blink phase (hidden half of the cycle).
    pub fn overlay_ops(
        &self,
        buf: &Buffer,
        viewport_w: f32,
        ctx: &Ctx,
        show_caret: bool,
    ) -> Vec<DrawOp> {
        let mut ops = Vec::new();
        if let Some((from, to)) = self.selection() {
            for rect in selection_rects(buf, from, to, viewport_w, ctx, false) {
                ops.push(DrawOp::Rect {
                    rect,
                    color: ctx.theme.sel,
                });
            }
        }
        if show_caret {
            let (x, y) = self.caret_xy(buf, viewport_w, ctx);
            let line_h = self.line_height(viewport_w, ctx);
            ops.push(DrawOp::Rect {
                rect: Rect::new(x, y, 1.5 * ctx.scale, line_h),
                color: ctx.theme.fg,
            });
        }
        ops
    }

    /// Mouse-down placement: document-space point -> buffer caret position.
    pub fn caret_for_point(
        &self,
        buf: &Buffer,
        x: f32,
        doc_y: f32,
        viewport_w: f32,
        ctx: &Ctx,
    ) -> Pos {
        hit_test(buf, x, doc_y, viewport_w, ctx, false)
    }

    // --------------------------------------------------------- scroll upkeep

    /// Scroll offset that keeps the caret's row visible in a pane of height
    /// `view_h`, given the document-space caret top and the pane line grid.
    pub fn ensure_visible(&self, doc_y: f32, line_h: f32, view_h: f32, scroll: f32) -> f32 {
        if doc_y < scroll {
            doc_y
        } else if doc_y + line_h > scroll + view_h {
            (doc_y + line_h - view_h).max(0.0)
        } else {
            scroll
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::event::Modifiers;

    const CTRL_SHIFT: Modifiers = Modifiers {
        ctrl: true,
        alt: false,
        shift: true,
    };

    fn buf() -> Buffer {
        Buffer::from_text("alpha beta\ngamma\n")
    }

    fn shift_moves(e: &mut EditorView, buf: &Buffer, key: Key, times: usize) {
        for _ in 0..times {
            e.move_key(buf, key, CTRL_SHIFT);
        }
    }

    #[test]
    fn movement_without_selection() {
        let b = buf();
        let mut e = EditorView::default();
        assert!(e.move_key(&b, Key::Right, Modifiers::default()));
        assert_eq!(e.caret.col, 1);
        // Right at end of line wraps to the next line; Left at col 0 wraps up.
        e.caret = Pos {
            line: 0,
            col: b.line(0).len(),
        };
        assert!(e.move_key(&b, Key::Right, Modifiers::default()));
        assert_eq!(e.caret, Pos { line: 1, col: 0 });
        assert!(e.move_key(&b, Key::Left, Modifiers::default()));
        assert_eq!(
            e.caret,
            Pos {
                line: 0,
                col: b.line(0).len()
            }
        );
        // Moving off the top of the buffer: the first Left still steps back
        // within line 0; only at col 0 does it stop being a no-op.
        e.caret = Pos { line: 0, col: 1 };
        assert!(e.move_key(&b, Key::Left, Modifiers::default()));
        assert_eq!(e.caret.col, 0);
        assert!(!e.move_key(&b, Key::Left, Modifiers::default()));
    }

    #[test]
    fn shift_selects_and_typing_replaces_selection() {
        let mut b = buf();
        let mut e = EditorView::default();
        shift_moves(&mut e, &b, Key::Right, 5);
        assert_eq!(
            e.selection(),
            Some((Pos { line: 0, col: 0 }, Pos { line: 0, col: 5 }))
        );
        e.insert_text(&mut b, "ALPHA");
        assert_eq!(b.text(), "ALPHA beta\ngamma\n");
        assert!(b.dirty);
        assert_eq!(e.caret, Pos { line: 0, col: 5 });
        assert!(e.selection().is_none());
    }

    #[test]
    fn undo_typing_runs_and_redo() {
        let mut b = buf();
        let mut e = EditorView::default();
        e.insert_text(&mut b, "X");
        e.insert_text(&mut b, "Y");
        e.insert_text(&mut b, "Z");
        assert_eq!(b.text(), "XYZalpha beta\ngamma\n");
        // Consecutive single chars merge into one undo step.
        assert!(e.undo(&mut b));
        assert_eq!(b.text(), "alpha beta\ngamma\n");
        assert!(e.redo(&mut b));
        assert_eq!(b.text(), "XYZalpha beta\ngamma\n");
    }

    #[test]
    fn undo_of_selection_replacement_takes_two_steps() {
        let mut b = Buffer::from_text("hello world");
        let mut e = EditorView::default();
        shift_moves(&mut e, &b, Key::Right, 5);
        e.insert_text(&mut b, "howdy");
        assert_eq!(b.text(), "howdy world");
        assert!(e.undo(&mut b)); // removes "howdy"
        assert!(e.undo(&mut b)); // restores "hello"
        assert_eq!(b.text(), "hello world");
        assert!(e.redo(&mut b));
        assert_eq!(b.text(), " world");
        assert!(e.redo(&mut b));
        assert_eq!(b.text(), "howdy world");
    }

    #[test]
    fn delete_and_backspace_join_lines() {
        let mut b = buf();
        let eol = b.line(0).len();
        let mut e = EditorView {
            caret: Pos { line: 0, col: eol },
            ..Default::default()
        };
        e.delete_key(&mut b, true); // Delete joins line 0 and 1 at EOL
        assert_eq!(b.text(), "alpha betagamma\n");
        e.delete_key(&mut b, false); // Backspace removes the joined 'a'
        assert_eq!(b.text(), "alpha betgamma\n");
        // The forward join was its own step; the trailing backspace merged
        // into it (contiguous single-char delete run), so one undo restores
        // both.
        assert!(e.undo(&mut b));
        assert_eq!(b.text(), "alpha beta\ngamma\n");
        assert!(!e.undo(&mut b));
        // At the very start backward delete is a no-op.
        e.caret = Pos { line: 0, col: 0 };
        e.delete_key(&mut b, false);
        assert_eq!(b.text(), "alpha beta\ngamma\n");
    }

    #[test]
    fn backspace_run_merges_into_one_undo_step() {
        let mut b = Buffer::from_text("abc");
        let mut e = EditorView {
            caret: Pos { line: 0, col: 3 },
            ..Default::default()
        };
        e.delete_key(&mut b, false);
        e.delete_key(&mut b, false);
        e.delete_key(&mut b, false);
        assert_eq!(b.text(), "");
        assert!(e.undo(&mut b));
        assert_eq!(b.text(), "abc");
        assert_eq!(e.caret, Pos { line: 0, col: 3 });
    }

    #[test]
    fn cut_paste_round_trip() {
        let mut b = buf();
        let mut e = EditorView::default();
        shift_moves(&mut e, &b, Key::Right, 5);
        assert_eq!(e.cut(&mut b).as_deref(), Some("alpha"));
        assert_eq!(b.text(), " beta\ngamma\n");
        e.paste(&mut b, "goodbye");
        assert_eq!(b.text(), "goodbye beta\ngamma\n");
        assert!(e.undo(&mut b) && e.undo(&mut b));
        assert_eq!(b.text(), "alpha beta\ngamma\n");
        assert!(e.redo(&mut b) && e.redo(&mut b));
        assert_eq!(b.text(), "goodbye beta\ngamma\n");
    }

    #[test]
    fn home_end_and_select_all() {
        let b = buf();
        let mut e = EditorView {
            caret: Pos { line: 0, col: 6 },
            ..Default::default()
        };
        assert!(e.move_key(&b, Key::End, Modifiers::default()));
        assert_eq!(e.caret.col, b.line(0).len());
        assert!(e.move_key(&b, Key::Home, Modifiers::default()));
        assert_eq!(e.caret.col, 0);
        e.select_all(&b);
        // The buffer ends with \n, so select_all reaches line 2 col 0.
        assert_eq!(
            e.selection(),
            Some((Pos { line: 0, col: 0 }, Pos { line: 2, col: 0 }))
        );
        assert_eq!(e.copy(&b).as_deref(), Some("alpha beta\ngamma\n"));
    }

    #[test]
    fn vertical_keys_use_goal_column() {
        let b = Buffer::from_text("long line of text here\nshort\nanother long one");
        let mut e = EditorView {
            caret: Pos { line: 0, col: 6 },
            ..Default::default()
        };
        assert!(e.move_key(&b, Key::Down, Modifiers::default()));
        assert_eq!(e.caret, Pos { line: 1, col: 5 }); // clamped to "short" length
        assert!(e.move_key(&b, Key::Down, Modifiers::default()));
        assert_eq!(e.caret, Pos { line: 2, col: 6 }); // goal preserved
        assert!(e.move_key(&b, Key::Up, Modifiers::default()));
        // Goal is long-line col 6 now (updated on each vertical move).
        assert_eq!(e.caret, Pos { line: 1, col: 5 });
        // A fresh goal starts from wherever we are: clamped again to 5.
        e.goal = None;
        assert!(e.move_key(&b, Key::Down, Modifiers::default()));
        assert_eq!(e.caret.col, 5);
    }

    #[test]
    fn arrow_out_of_selection_jumps_to_edge() {
        let b = buf();
        let mut e = EditorView::default();
        e.select_all(&b);
        // Caret sits at the far end; Left collapses to the start.
        assert!(e.move_key(&b, Key::Left, Modifiers::default()));
        assert_eq!(e.caret, Pos { line: 0, col: 0 });
        assert!(e.selection().is_none());
        e.select_all(&b);
        assert!(e.move_key(&b, Key::Right, Modifiers::default()));
        assert_eq!(e.caret, b.end());
        assert!(e.selection().is_none());
    }

    #[test]
    fn double_click_selects_word_by_class() {
        let b = Buffer::from_text("fn word_here(x: i32) {}");
        let mut e = EditorView::default();
        // Middle of an identifier: the whole identifier.
        e.select_word(&b, Pos { line: 0, col: 5 });
        assert_eq!(e.sel_text(&b), "word_here");
        // A punctuation char selects the punctuation run.
        e.select_word(&b, Pos { line: 0, col: 14 });
        assert_eq!(e.sel_text(&b), ":");
        // Whitespace selects the whitespace run.
        e.select_word(&b, Pos { line: 0, col: 15 });
        assert_eq!(e.sel_text(&b), " ");
        // Clicking at end of line selects the last char's run.
        e.select_word(
            &b,
            Pos {
                line: 0,
                col: b.line(0).len(),
            },
        );
        assert_eq!(e.sel_text(&b), "{}");
        // Empty line collapses instead of panicking.
        let empty = Buffer::from_text("");
        e.select_word(&empty, Pos { line: 0, col: 0 });
        assert!(e.selection().is_none());
    }

    #[test]
    fn triple_click_selects_whole_line() {
        let b = Buffer::from_text("alpha beta\ngamma\n");
        let mut e = EditorView::default();
        e.select_line(&b, Pos { line: 1, col: 3 });
        assert_eq!(e.sel_text(&b), "gamma");
        assert_eq!(e.caret, Pos { line: 1, col: 5 });
    }
}
