//! Ctrl+F find bar: the query state, the live match search, and the floating
//! bar itself.
//!
//! The search is the source of truth: every match is a buffer position pair,
//! so the count is honest in every view mode. Edit/Split shows the current
//! match as the editor selection and tints the rest over the pane; Preview
//! has no buffer-offset geometry, so it gets the count plus a jump to the
//! row containing the current occurrence.

use crate::{
    core::buffer::{Buffer, Pos},
    gfx::{
        canvas::{Canvas, TextSpec},
        font::{FontStyle, FontSystem},
        geom::Rect,
    },
    layout::theme::Theme,
};

/// Live find state, owned by `App`; `None` while the bar is closed.
#[derive(Default)]
pub struct Find {
    pub query: String,
    /// Every match in the active buffer, in reading order.
    pub matches: Vec<(Pos, Pos)>,
    /// Index into `matches` of the current match.
    pub current: usize,
}

impl Find {
    /// Recompute the matches for `buf` and land on the first one at or after
    /// `from` (the caret), so extending the query keeps the current match
    /// instead of teleporting back to the top of the document.
    pub fn refresh(&mut self, buf: &Buffer, from: Pos) {
        self.matches = search(buf, &self.query);
        self.current = self
            .matches
            .iter()
            .position(|&(a, _)| a >= from)
            .unwrap_or(0);
    }

    /// Step to the next (Shift: previous) match, wrapping at either end.
    pub fn step(&mut self, back: bool) {
        let n = self.matches.len();
        if n == 0 {
            self.current = 0;
            return;
        }
        self.current = if back {
            (self.current + n - 1) % n
        } else {
            (self.current + 1) % n
        };
    }

    /// The current match, if there is one.
    pub fn current_match(&self) -> Option<(Pos, Pos)> {
        self.matches.get(self.current).copied()
    }

    /// Bar status: `n/m`, or a plain word when there is nothing to count.
    pub fn status(&self) -> String {
        if self.query.is_empty() {
            String::new()
        } else if self.matches.is_empty() {
            "0 matches".into()
        } else {
            format!("{}/{}", self.current + 1, self.matches.len())
        }
    }
}

/// Case-insensitive substring matches of `query` in `buf`, in reading order.
/// A match never spans a newline: find is line-oriented for v1.
pub fn search(buf: &Buffer, query: &str) -> Vec<(Pos, Pos)> {
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in 0..buf.line_count() {
        for (start, end) in search_line(buf.line(line), &needle) {
            out.push((Pos { line, col: start }, Pos { line, col: end }));
        }
    }
    out
}

/// Byte ranges of the non-overlapping matches of `needle` (lowercased chars)
/// in `line`. Each haystack char is tagged with the byte offset of the char
/// it was folded from, so columns stay on char boundaries even when case
/// folding expands (İ -> "i" + combining dot).
fn search_line(line: &str, needle: &[char]) -> Vec<(usize, usize)> {
    let mut hay: Vec<(usize, char)> = Vec::with_capacity(line.len());
    for (i, ch) in line.char_indices() {
        for lc in ch.to_lowercase() {
            hay.push((i, lc));
        }
    }
    let mut out = Vec::new();
    let mut k = 0;
    while k + needle.len() <= hay.len() {
        if hay[k..k + needle.len()]
            .iter()
            .zip(needle)
            .all(|((_, h), n)| h == n)
        {
            let start = hay[k].0;
            // End: just past the last matched source char.
            let last = hay[k + needle.len() - 1].0;
            let end = line[last..]
                .chars()
                .next()
                .map_or(line.len(), |c| last + c.len_utf8());
            out.push((start, end));
            k += needle.len();
        } else {
            k += 1;
        }
    }
    out
}

// ------------------------------------------------------------- the bar

/// Bar height and insets, logical px before `scale`.
const BAR_H: f32 = 30.0;
const BAR_PAD: f32 = 10.0;
const TEXT_SIZE: f32 = 12.5;

/// The bar rect for the current query/status: docked at the viewport's
/// top-right. Pure, so hit-testing and painting always agree.
pub fn bar_rect(fonts: &FontSystem, view: Rect, scale: f32, query: &str, status: &str) -> Rect {
    let size = TEXT_SIZE * scale;
    let pad = BAR_PAD * scale;
    let gap = 8.0 * scale;
    let label_w = fonts.measure(FontStyle::Regular, "Find", size);
    let query_w = fonts
        .measure(FontStyle::Mono, query, size)
        .max(120.0 * scale);
    let status_w = fonts.measure(FontStyle::Regular, status, size);
    let w = (pad + label_w + gap + query_w + gap + status_w + pad)
        .min((view.w - 16.0 * scale).max(0.0));
    Rect::new(
        view.right() - w - 8.0 * scale,
        view.y + 8.0 * scale,
        w,
        BAR_H * scale,
    )
}

/// Paint the bar over the viewport's top-right corner; returns its rect
/// (identical to [`bar_rect`], which the caller hit-tests with).
pub fn paint(
    canvas: &mut Canvas,
    fonts: &FontSystem,
    theme: &Theme,
    view: Rect,
    scale: f32,
    find: &Find,
    caret: bool,
) -> Rect {
    let status = find.status();
    let r = bar_rect(fonts, view, scale, &find.query, &status);
    let size = TEXT_SIZE * scale;
    let pad = BAR_PAD * scale;
    let gap = 8.0 * scale;
    canvas.set_clip(Some(view));
    canvas.fill_rounded_rect(r, 6.0 * scale, theme.code_bg);
    canvas.fill_rect(
        Rect::new(r.x, r.bottom() - scale.max(1.0), r.w, scale.max(1.0)),
        theme.rule,
    );
    let m = fonts.metrics(FontStyle::Regular, size);
    let baseline = (r.y + (r.h - m.height()) / 2.0 + m.ascent).round();
    let label_w = fonts.measure(FontStyle::Regular, "Find", size);
    canvas.draw_text(
        fonts,
        "Find",
        r.x + pad,
        baseline,
        TextSpec {
            size,
            style: FontStyle::Regular,
            color: theme.muted,
        },
    );
    let qx = r.x + pad + label_w + gap;
    canvas.draw_text(
        fonts,
        &find.query,
        qx,
        baseline,
        TextSpec {
            size,
            style: FontStyle::Mono,
            color: theme.fg,
        },
    );
    if caret {
        let qw = fonts.measure(FontStyle::Mono, &find.query, size);
        canvas.fill_rect(
            Rect::new(qx + qw, r.y + 6.0 * scale, 1.5 * scale, r.h - 12.0 * scale),
            theme.accent,
        );
    }
    let sw = fonts.measure(FontStyle::Regular, &status, size);
    canvas.draw_text(
        fonts,
        &status,
        r.right() - pad - sw,
        baseline,
        TextSpec {
            size,
            style: FontStyle::Regular,
            color: theme.muted,
        },
    );
    canvas.set_clip(None);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_is_case_insensitive_and_counts_every_line() {
        let buf = Buffer::from_text("Alpha beta\nGAMMA alpha\nalpha");
        let m = search(&buf, "alpha");
        assert_eq!(m.len(), 3);
        assert_eq!(m[0], (Pos { line: 0, col: 0 }, Pos { line: 0, col: 5 }));
        assert_eq!(m[1], (Pos { line: 1, col: 6 }, Pos { line: 1, col: 11 }));
        assert_eq!(m[2], (Pos { line: 2, col: 0 }, Pos { line: 2, col: 5 }));
        // An empty query matches nothing rather than everything.
        assert!(search(&buf, "").is_empty());
    }

    #[test]
    fn matches_do_not_overlap_and_stay_on_char_boundaries() {
        // "aaaa" at width 2 finds two matches, not three.
        let buf = Buffer::from_text("aaaa");
        assert_eq!(search(&buf, "aa").len(), 2);
        // Multibyte: É is 2 bytes, so "Éa" is 3 bytes and the match covers
        // bytes 0..3.
        let buf = Buffer::from_text("Éa Éa");
        let m = search(&buf, "éa");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0], (Pos { line: 0, col: 0 }, Pos { line: 0, col: 3 }));
        assert_eq!(m[1], (Pos { line: 0, col: 4 }, Pos { line: 0, col: 7 }));
        // Case folding that expands: İ (U+0130) lowercases to "i" plus a
        // combining dot, and the match still maps back to whole-char columns.
        let buf = Buffer::from_text("İx");
        let m = search(&buf, "i");
        assert_eq!(m, vec![(Pos { line: 0, col: 0 }, Pos { line: 0, col: 2 })]);
    }

    #[test]
    fn refresh_lands_on_the_first_match_at_or_after_the_caret() {
        let buf = Buffer::from_text("one two\nthree one\none");
        let mut f = Find {
            query: "one".into(),
            ..Default::default()
        };
        f.refresh(&buf, Pos { line: 1, col: 0 });
        assert_eq!(f.current, 1, "skips the match on line 0");
        f.refresh(&buf, buf.end());
        assert_eq!(f.current, 0, "past the last match wraps to the first");
    }

    #[test]
    fn step_wraps_in_both_directions_and_survives_no_matches() {
        let buf = Buffer::from_text("a a a");
        let mut f = Find {
            query: "a".into(),
            ..Default::default()
        };
        f.refresh(&buf, Pos::default());
        assert_eq!(f.status(), "1/3");
        f.step(false);
        assert_eq!(f.status(), "2/3");
        f.step(false);
        f.step(false);
        assert_eq!(f.status(), "1/3", "wraps forward");
        f.step(true);
        assert_eq!(f.status(), "3/3", "wraps backward");
        f.matches.clear();
        f.step(false);
        assert_eq!(f.current, 0);
        assert_eq!(f.status(), "0 matches");
    }

    #[test]
    fn the_bar_paints_inside_the_viewport() {
        let fonts = FontSystem::load_embedded();
        let theme = Theme::light();
        let view = Rect::new(0.0, 40.0, 800.0, 500.0);
        let mut c = Canvas::new(800, 600);
        let find = Find {
            query: "query".into(),
            matches: vec![(Pos::default(), Pos::default()); 7],
            current: 1,
        };
        let r = paint(&mut c, &fonts, &theme, view, 1.0, &find, true);
        assert!(
            r.x >= view.x && r.right() <= view.right(),
            "inside the pane"
        );
        assert!(
            r.y >= view.y && r.bottom() <= view.bottom(),
            "inside the pane"
        );
        // A very long query clamps to the pane width instead of overflowing.
        let long = "q".repeat(400);
        let r = bar_rect(&fonts, view, 1.0, &long, "1/1");
        assert!(r.right() <= view.right() + 0.5);
        assert!(r.w <= view.w);
    }
}
