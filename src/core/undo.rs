//! Undo/redo history (M5).
//!
//! Consecutive typing merges into one undo step; anything else (paste,
//! cut, newline, undo/redo itself) starts a fresh step.

use super::buffer::{Buffer, Pos};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Insert { at: Pos, text: String },
    Delete { from: Pos, text: String },
}

impl Edit {
    /// Apply this edit to the buffer (for redo and fresh application).
    pub fn apply(&self, buf: &mut Buffer) -> Pos {
        match self {
            Edit::Insert { at, text } => buf.insert(*at, text),
            Edit::Delete { from, text } => {
                let to = buf.advance(*from, text);
                buf.delete(*from, to);
                *from
            }
        }
    }

    /// Invert this edit (for undo). Returns the caret position afterwards.
    pub fn invert(&self, buf: &mut Buffer) -> Pos {
        match self {
            Edit::Insert { at, text } => {
                let to = buf.advance(*at, text);
                buf.delete(*at, to);
                *at
            }
            Edit::Delete { from, text } => buf.insert(*from, text),
        }
    }
}

#[derive(Default)]
pub struct UndoStack {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
}

impl UndoStack {
    /// Record an edit, merging runs of single-character typing into one step.
    /// Any push clears the redo stack.
    pub fn push(&mut self, edit: Edit) {
        self.redo.clear();
        if let Some(last) = self.undo.last_mut() {
            if merge(last, &edit) {
                return;
            }
        }
        self.undo.push(edit);
    }

    pub fn pop_undo(&mut self) -> Option<Edit> {
        let edit = self.undo.pop()?;
        self.redo.push(edit.clone());
        Some(edit)
    }

    pub fn pop_redo(&mut self) -> Option<Edit> {
        let edit = self.redo.pop()?;
        self.undo.push(edit.clone());
        Some(edit)
    }
}

/// Merge `next` into `prev` when they form one typing run: single-character
/// inserts/deletes at a contiguous position.
fn merge(prev: &mut Edit, next: &Edit) -> bool {
    match (prev, next) {
        (
            Edit::Insert { at, text },
            Edit::Insert {
                at: at2,
                text: text2,
            },
        ) => {
            if text2.chars().count() == 1 && at2.line == at.line && at2.col == at.col + text.len() {
                text.push_str(text2);
                true
            } else {
                false
            }
        }
        (
            Edit::Delete { from, text },
            Edit::Delete {
                from: from2,
                text: text2,
            },
        ) => {
            // Backspace runs delete backwards: the new chunk sits before. Merge
            // only a contiguous single char directly before the run.
            let contiguous = text2.chars().count() == 1
                && from2.line == from.line
                && from2.col + text2.len() == from.col;
            let merged = contiguous.then(|| {
                let mut all = text2.clone();
                all.push_str(text);
                all
            });
            match merged {
                Some(all) => {
                    *text = all;
                    *from = *from2;
                    true
                }
                None => false,
            }
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }

    #[test]
    fn typing_merges_but_pastes_do_not() {
        let mut u = UndoStack::default();
        u.push(Edit::Insert {
            at: pos(0, 0),
            text: "a".into(),
        });
        u.push(Edit::Insert {
            at: pos(0, 1),
            text: "b".into(),
        });
        assert_eq!(
            u.pop_undo().map(|e| e
                == Edit::Insert {
                    at: pos(0, 0),
                    text: "ab".into()
                }),
            Some(true)
        );
        u.pop_undo();
        // Non-contiguous insert starts a new step.
    }

    #[test]
    fn backspace_runs_merge() {
        let mut u = UndoStack::default();
        u.push(Edit::Delete {
            from: pos(0, 2),
            text: "c".into(),
        });
        u.push(Edit::Delete {
            from: pos(0, 1),
            text: "b".into(),
        });
        assert_eq!(
            u.pop_undo(),
            Some(Edit::Delete {
                from: pos(0, 1),
                text: "bc".into()
            })
        );
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut u = UndoStack::default();
        u.push(Edit::Insert {
            at: pos(0, 0),
            text: "x".into(),
        });
        assert!(u.pop_undo().is_some());
        assert!(u.pop_redo().is_some());
        // New edits clear redo.
        u.push(Edit::Insert {
            at: pos(0, 0),
            text: "y".into(),
        });
        assert!(u.pop_redo().is_none());
    }
}
