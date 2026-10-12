use super::{
    display_list::{DisplayList, DrawOp, FontStyle},
    Ctx,
};
use crate::{
    core::buffer::{Buffer, Pos},
    gfx::{font::FontSystem, geom::Rect},
};

/// Shared geometry for the plain-text pane: layout, hit-testing, and caret
/// math must agree exactly, so they all derive from here.
pub struct PlainParams {
    pub x0: f32,
    pub col_w: f32,
    pub line_h: f32,
    pub pad: f32,
    pub size: f32,
}

/// Line height of the plain-text grid for `viewport_w` (wrap-independent).
pub fn line_height(viewport_w: f32, ctx: &Ctx) -> f32 {
    plain_params(viewport_w, ctx, true).line_h
}

/// Geometry of the plain-text pane. `wrap`: word-wrap into a centered column
/// (preview panes); otherwise one row per line, left-aligned, for the editor
/// pane — long lines scroll horizontally instead of folding.
pub fn plain_params(viewport_w: f32, ctx: &Ctx, wrap: bool) -> PlainParams {
    let t = ctx.theme;
    let size = ctx.body_size() * 0.9;
    let pad = ctx.px(32.0);
    let (col_w, x0) = if wrap {
        let col_w = (viewport_w - 2.0 * pad)
            .min(ctx.px(t.content_max_width))
            .max(ctx.px(120.0));
        let x0 = ((viewport_w - col_w) / 2.0)
            .max(pad.min(viewport_w / 4.0))
            .round();
        (col_w, x0)
    } else {
        (f32::INFINITY, pad)
    };
    PlainParams {
        x0,
        col_w,
        line_h: (size * 1.55).round(),
        pad,
        size,
    }
}

/// Greedy word-wrap of one logical line to `max_w` pixels. Breaks after spaces; hard-breaks tokens that
/// are wider than the column. Always makes progress (at least one char per row). Empty input -> one empty row.
///
/// Returns char-index ranges into `text`; trailing spaces are trimmed from
/// wrapped rows (never from the final row), exactly as [`wrap_line`] renders.
pub fn wrap_ranges(
    fonts: &FontSystem,
    style: FontStyle,
    size: f32,
    text: &str,
    max_w: f32,
) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![(0, 0)];
    }
    let mut rows = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let (mut w, mut i, mut last_break) = (0.0f32, start, None);
        while i < chars.len() {
            let a = fonts.advance(style, chars[i], size);
            if w + a > max_w && i > start {
                break;
            }
            w += a;
            i += 1;
            if chars[i - 1] == ' ' {
                last_break = Some(i);
            }
        }
        if i >= chars.len() {
            rows.push((start, chars.len()));
            break;
        }
        let cut = match last_break {
            Some(b) if b > start => b,
            _ => i,
        };
        let mut end = cut;
        while end > start && chars[end - 1] == ' ' {
            end -= 1;
        }
        rows.push((start, end));
        start = cut;
    }
    rows
}

/// Same as [`wrap_ranges`] but rendering each row to a `String`.
pub fn wrap_line(
    fonts: &FontSystem,
    style: FontStyle,
    size: f32,
    text: &str,
    max_w: f32,
) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    wrap_ranges(fonts, style, size, text, max_w)
        .into_iter()
        .map(|(s, e)| chars[s..e].iter().collect())
        .collect()
}

/// Plain text / logs / anything without a richer renderer yet: monospace,
/// wrapped into a centered column (preview) or one row per line (editor).
pub fn layout_plain(buf: &Buffer, viewport_w: f32, ctx: &Ctx, wrap: bool) -> DisplayList {
    let t = ctx.theme;
    let style = FontStyle::Mono;
    let p = plain_params(viewport_w, ctx, wrap);
    let m = ctx.fonts.metrics(style, p.size);

    let mut ops = Vec::new();
    let mut y = p.pad;
    for i in 0..buf.line_count() {
        let line = buf.line(i).replace('\t', "    ");
        for row in wrap_line(ctx.fonts, style, p.size, &line, p.col_w) {
            if !row.is_empty() {
                let baseline = (y + (p.line_h - m.height()) / 2.0 + m.ascent).round();
                ops.push(DrawOp::Text {
                    x: p.x0,
                    y: baseline,
                    text: row,
                    size: p.size,
                    style,
                    color: t.fg,
                });
            }
            y += p.line_h;
        }
    }
    DisplayList {
        ops,
        height: y + p.pad,
        hits: Vec::new(),
    }
}

/// One buffer line with tabs expanded for display, plus byte maps between
/// original and expanded coordinates (a tab is 1 original byte, 4 display
/// bytes). Caret columns are always original bytes on char boundaries.
struct ExpLine {
    chars: Vec<char>,
    /// Expanded byte offset of each char start, plus the total at the end.
    byte_at: Vec<usize>,
    /// Original byte for each expanded byte, plus the line length at the end.
    exp_to_orig: Vec<usize>,
}

fn expand_line(line: &str) -> ExpLine {
    let mut expanded = String::new();
    let mut exp_to_orig = Vec::new();
    let mut ob = 0;
    for ch in line.chars() {
        let s = expanded.len();
        if ch == '\t' {
            expanded.push_str("    ");
        } else {
            expanded.push(ch);
        }
        for _ in s..expanded.len() {
            exp_to_orig.push(ob);
        }
        ob += ch.len_utf8();
    }
    exp_to_orig.push(ob);
    let chars: Vec<char> = expanded.chars().collect();
    let mut byte_at = vec![0usize; chars.len() + 1];
    let mut b = 0;
    for (k, ch) in chars.iter().enumerate() {
        byte_at[k] = b;
        b += ch.len_utf8();
    }
    byte_at[chars.len()] = b;
    ExpLine {
        chars,
        byte_at,
        exp_to_orig,
    }
}

fn orig_to_exp(line: &str, ob: usize) -> usize {
    let mut eb = 0;
    let mut o = 0;
    for ch in line.chars() {
        if o >= ob {
            break;
        }
        eb += if ch == '\t' { 4 } else { ch.len_utf8() };
        o += ch.len_utf8();
    }
    eb
}

fn advance_of(ctx: &Ctx, ch: char, size: f32) -> f32 {
    ctx.fonts.advance(FontStyle::Mono, ch, size)
}

/// Map a document-space point to the nearest buffer position.
pub fn hit_test(buf: &Buffer, x: f32, y: f32, viewport_w: f32, ctx: &Ctx, wrap: bool) -> Pos {
    let p = plain_params(viewport_w, ctx, wrap);
    let mut row_top = p.pad;
    for li in 0..buf.line_count() {
        let exp = expand_line(buf.line(li));
        for (s, e) in wrap_ranges(
            ctx.fonts,
            FontStyle::Mono,
            p.size,
            &exp.chars.iter().collect::<String>(),
            p.col_w,
        ) {
            if y < row_top + p.line_h {
                let mut w = 0.0;
                let mut col = s;
                for k in s..e {
                    let cw = advance_of(ctx, exp.chars[k], p.size);
                    if x < p.x0 + w + cw / 2.0 {
                        break;
                    }
                    w += cw;
                    col = k + 1;
                }
                let eb = exp.byte_at[col.min(exp.chars.len())];
                let ob = exp.exp_to_orig[eb.min(exp.exp_to_orig.len() - 1)];
                return buf.clamp(Pos { line: li, col: ob });
            }
            row_top += p.line_h;
        }
    }
    buf.end()
}

/// Map a caret to its document-space (x, row top) for painting and scrolling.
pub fn caret_point(buf: &Buffer, caret: Pos, viewport_w: f32, ctx: &Ctx, wrap: bool) -> (f32, f32) {
    let p = plain_params(viewport_w, ctx, wrap);
    let caret = buf.clamp(caret);
    let mut row_top = p.pad;
    for li in 0..buf.line_count() {
        let exp = expand_line(buf.line(li));
        let text: String = exp.chars.iter().collect();
        for (s, e) in wrap_ranges(ctx.fonts, FontStyle::Mono, p.size, &text, p.col_w) {
            if li == caret.line {
                let eb = orig_to_exp(buf.line(li), caret.col);
                let row_end = exp.byte_at[e.min(exp.chars.len())];
                if eb <= row_end {
                    let mut w = 0.0;
                    for k in s..e {
                        if exp.byte_at[k] >= eb {
                            break;
                        }
                        w += advance_of(ctx, exp.chars[k], p.size);
                    }
                    return (p.x0 + w, row_top);
                }
            }
            row_top += p.line_h;
        }
    }
    // Unreachable after clamping, but never lie about a position.
    (p.x0, row_top)
}

/// Highlight rectangles for `[from, to)` in document space. Empty when equal.
pub fn selection_rects(
    buf: &Buffer,
    from: Pos,
    to: Pos,
    viewport_w: f32,
    ctx: &Ctx,
    wrap: bool,
) -> Vec<Rect> {
    let (from, to) = if from <= to { (from, to) } else { (to, from) };
    if from == to {
        return Vec::new();
    }
    let p = plain_params(viewport_w, ctx, wrap);
    let mut rects = Vec::new();
    let mut row_top = p.pad;
    for li in 0..buf.line_count() {
        let line = buf.line(li);
        let exp = expand_line(line);
        let text: String = exp.chars.iter().collect();
        let sel: Option<(usize, usize)> = if li < from.line || li > to.line {
            None
        } else {
            let a = if li == from.line {
                orig_to_exp(line, from.col)
            } else {
                0
            };
            let z = if li == to.line {
                orig_to_exp(line, to.col)
            } else {
                exp.byte_at[exp.chars.len()]
            };
            if a >= z {
                None
            } else {
                Some((a, z))
            }
        };
        for (s, e) in wrap_ranges(ctx.fonts, FontStyle::Mono, p.size, &text, p.col_w) {
            if let Some((a, z)) = sel {
                let mut x0 = 0.0;
                let mut x1 = 0.0;
                for k in s..e {
                    let cs = exp.byte_at[k];
                    let ce = exp.byte_at[k + 1];
                    let cw = advance_of(ctx, exp.chars[k], p.size);
                    if ce <= a {
                        x0 += cw;
                        x1 += cw;
                    } else if cs < z {
                        x1 += cw;
                    } else {
                        break;
                    }
                }
                if x1 > x0 {
                    rects.push(Rect::new(p.x0 + x0, row_top, (x1 - x0).max(2.0), p.line_h));
                }
            }
            row_top += p.line_h;
        }
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::theme::Theme;

    #[test]
    fn wraps_within_width() {
        let f = FontSystem::load_embedded();
        let text = "the quick brown fox jumps over the lazy dog and keeps running far away";
        let rows = wrap_line(&f, FontStyle::Regular, 16.0, text, 200.0);
        assert!(rows.len() > 1);
        for r in &rows {
            assert!(
                f.measure(FontStyle::Regular, r, 16.0) <= 200.5,
                "row too wide: {r:?}"
            );
        }
        assert_eq!(rows.join(" "), text);
    }

    #[test]
    fn hard_breaks_long_tokens_and_terminates() {
        let f = FontSystem::load_embedded();
        let rows = wrap_line(&f, FontStyle::Regular, 16.0, &"W".repeat(100), 100.0);
        assert!(rows.len() > 5);
        assert_eq!(rows.concat().len(), 100);
        assert_eq!(
            wrap_line(&f, FontStyle::Regular, 16.0, "", 100.0),
            vec![String::new()]
        );
        // pathological: column narrower than one glyph still makes progress
        assert_eq!(wrap_line(&f, FontStyle::Regular, 16.0, "abc", 1.0).len(), 3);
    }

    #[test]
    fn plain_layout_height_grows_with_lines() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let ctx = Ctx {
            fonts: &f,
            theme: &t,
            scale: 1.0,
            zoom: 1.0,
        };
        let a = layout_plain(&Buffer::from_text("one\ntwo"), 800.0, &ctx, true);
        let b = layout_plain(
            &Buffer::from_text("one\ntwo\nthree\nfour"),
            800.0,
            &ctx,
            true,
        );
        assert!(b.height > a.height);
        assert_eq!(a.ops.len(), 2);
        // No-wrap mode: one row per line, left-aligned at the padding.
        let long = "a very long line that would surely wrap in a narrow column";
        let nw = layout_plain(&Buffer::from_text(long), 200.0, &ctx, false);
        assert_eq!(nw.ops.len(), 1, "no-wrap must not fold the line");
        let p = plain_params(200.0, &ctx, false);
        match &nw.ops[0] {
            DrawOp::Text { x, .. } => assert_eq!(*x, p.x0),
            _ => panic!("expected a text op"),
        }
    }

    #[test]
    fn caret_round_trips_through_hit_test() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let ctx = Ctx {
            fonts: &f,
            theme: &t,
            scale: 1.0,
            zoom: 1.0,
        };
        let buf = Buffer::from_text("hello world\nsecond line here\n\tindented");
        for wrap in [true, false] {
            for line in 0..buf.line_count() {
                let len = buf.line(line).len();
                for col in [0, 1, len / 2, len] {
                    let caret = buf.clamp(Pos { line, col });
                    let (x, y) = caret_point(&buf, caret, 800.0, &ctx, wrap);
                    let back = hit_test(&buf, x + 1.0, y + 1.0, 800.0, &ctx, wrap);
                    assert_eq!(back, caret, "wrap {wrap} line {line} col {col}");
                }
            }
        }
    }

    #[test]
    fn selection_rects_cover_ranges() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let ctx = Ctx {
            fonts: &f,
            theme: &t,
            scale: 1.0,
            zoom: 1.0,
        };
        let buf = Buffer::from_text("aaa\nbbb\nccc");
        assert!(selection_rects(
            &buf,
            Pos { line: 0, col: 0 },
            Pos { line: 0, col: 0 },
            800.0,
            &ctx,
            true
        )
        .is_empty());
        let one = selection_rects(
            &buf,
            Pos { line: 1, col: 0 },
            Pos { line: 1, col: 3 },
            800.0,
            &ctx,
            true,
        );
        assert_eq!(one.len(), 1);
        let multi = selection_rects(
            &buf,
            Pos { line: 0, col: 1 },
            Pos { line: 2, col: 2 },
            800.0,
            &ctx,
            true,
        );
        assert_eq!(multi.len(), 3);
        // Reversed ranges normalize.
        let rev = selection_rects(
            &buf,
            Pos { line: 2, col: 2 },
            Pos { line: 0, col: 1 },
            800.0,
            &ctx,
            true,
        );
        assert_eq!(rev.len(), multi.len());
    }
}
