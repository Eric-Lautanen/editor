//! The status bar's app half (R4): the text it shows, its buttons, the
//! Copy affordance, and the walk to the JSON node its error names.
//! Extracted from app.rs; no logic changed.

use super::*;

impl App {
    /// Whether the bar's detail is a place to go: a JSON tree on screen with a
    /// node it cannot describe, and the tree's own message is the one the bar
    /// is showing. O(1) — the bar asks this on every paint, so the walk to find
    /// the node waits for the press. A transient that has taken the message's
    /// place (`reloaded`, `save failed`) is a fact again: a word that does not
    /// name the node must not offer to lead to it.
    pub(super) fn status_names_broken_json(&self) -> bool {
        if self.kind != FileKind::Json || self.mode == Mode::Edit {
            return false;
        }
        let Some((_, doc)) = &self.json_doc else {
            return false;
        };
        doc.broken
            .as_deref()
            .is_some_and(|msg| self.status_extra.strip_prefix("error: ") == Some(msg))
    }

    /// Bring the first node the active JSON tree cannot describe into view:
    /// the status bar's error is the way there. The row lands in the middle of
    /// the pane rather than at its top — a jump is for finding the thing, and a
    /// row at the very top reads like the start of the document.
    pub(super) fn jump_to_broken_node(&mut self, host: &mut dyn Host) {
        let Some(slot) = self.json_error_slot() else {
            return;
        };
        let (top, h, height) = match &self.json_doc {
            Some((_, doc)) => match doc.plan.slots.get(slot) {
                Some(s) => (s.top, s.h, doc.plan.height),
                None => return,
            },
            None => return,
        };
        let pane_h = match self.mode {
            // The pane the tree is painted in: the whole window in Preview,
            // the left half in Split. (Edit has no reader pane, and the bar
            // does not offer the target there.)
            Mode::Split => self.split_panes().0.h,
            Mode::Edit => return,
            Mode::Preview => self.viewport().h,
        };
        let target = (top - ((pane_h - h).max(0.0)) / 2.0).clamp(0.0, (height - pane_h).max(0.0));
        // Split shares one progress between its panes, so a preview pixel has
        // to be handed over as a fraction of the preview's own travel: writing
        // it into `scroll.offset` there would move the pane by that many pixels
        // of whichever document is taller.
        if self.mode == Mode::Split {
            let max = self.preview_max();
            self.set_scroll_ratio(if max > 0.0 { target / max } else { 0.0 });
        } else {
            self.scroll.offset = target;
            self.scroll.scroll_by(0.0);
        }
        host.request_redraw();
    }

    /// Status-bar buttons. Ids: 0=Preview, 1=Edit, 2=Split, 3=Help, 4=Theme,
    /// 5=Open, 6=About, 7=Find. The vec is in left-to-right order (paint
    /// walks it in reverse), giving `Find Open | Preview Edit Split | Help
    /// About | Dark/Light`. Rects are filled in by `statusbar::paint`.
    pub(super) fn status_buttons(&self) -> Vec<statusbar::StatusButton> {
        let active = match self.mode {
            Mode::Preview => 0,
            Mode::Edit => 1,
            Mode::Split => 2,
        };
        let rows: [(u8, &'static str, bool, bool); 8] = [
            // id, label, active, starts-a-group
            (5, "Open", false, false),
            (7, "Find", self.find.is_some(), false),
            (0, "Preview", active == 0, true),
            (1, "Edit", active == 1, false),
            (2, "Split", active == 2, false),
            (3, "Help", self.help_open, true),
            (6, "About", self.about_open, false),
            // Names the theme a click switches to.
            (4, if self.dark { "Light" } else { "Dark" }, false, true),
        ];
        rows.iter()
            .map(
                |&(id, label, active, group_start)| statusbar::StatusButton {
                    id,
                    label,
                    active,
                    group_start,
                    rect: Rect::default(),
                },
            )
            .collect()
    }

    /// The status bar's Copy button: present only while there is a
    /// selection to copy (a preview word or rows, or an editor selection in
    /// Edit/Split), and reading "Copied" for as long as that same selection is
    /// what was last copied. Rect is filled in by `statusbar::paint`.
    pub(super) fn copy_button(&self) -> Option<statusbar::StatusButton> {
        let selected = self.preview_selection_text().is_some() || self.editor.selection().is_some();
        selected.then(|| statusbar::StatusButton {
            id: 8,
            label: if self.copy_done { "Copied" } else { "Copy" },
            active: false,
            group_start: false,
            rect: Rect::default(),
        })
    }

    /// The bar's left-hand text: `(info, detail)`.
    ///
    /// `info` is the document's own facts and always sits at the far left, in
    /// every mode — the reader should never have to hunt for the line count
    /// because a selection moved the bar around. `detail` is contextual and
    /// follows it: the caret in Edit, a parse error, `reloaded`, a failed
    /// open. The Copy button and a hovered link come after both.
    pub(super) fn status_bar_text(&self) -> (String, String) {
        let lines = self.buffer.line_count();
        let info = format!(
            "{}  ·  {} line{}  ·  {}%",
            self.kind.label(),
            lines,
            if lines == 1 { "" } else { "s" },
            (self.zoom * 100.0).round() as i32
        );
        // The tab already names the file, so the bar leaves it out.
        let mut detail = if self.mode == Mode::Edit {
            let sel = if self.editor.selection().is_some() {
                " [sel]"
            } else {
                ""
            };
            format!(
                "Ln {}, Col {}{sel}",
                self.editor.caret.line + 1,
                char_col(&self.buffer, self.editor.caret),
            )
        } else {
            String::new()
        };
        if !self.status_extra.is_empty() {
            if !detail.is_empty() {
                detail.push_str("  ·  ");
            }
            detail.push_str(&self.status_extra);
        }
        (info, detail)
    }
}
