use super::{
    display_list::{DisplayList, DrawOp, FontStyle},
    plan, Ctx,
};
use crate::{
    core::buffer::{Buffer, EditSpan, Pos},
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
    let size = ctx.body_size() * 0.9;
    let pad = ctx.px(32.0);
    let (col_w, x0) = if wrap {
        // The reading column reflows with the window (see `layout::column`).
        super::column(viewport_w, ctx)
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
    // ASCII mono lines (the overwhelming majority of a code, log or config
    // pane) wrap on bytes: char index == byte index there, so the `Vec<char>`
    // this used to collect first — one allocation and one pass over every
    // character of every line, per relayout — is not needed at all.
    if style == FontStyle::Mono && text.is_ascii() && !text.bytes().any(|b| b.is_ascii_control()) {
        return wrap_ranges_ascii(fonts, size, text.as_bytes(), max_w);
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![(0, 0)];
    }
    let mut rows = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let (mut w, mut i, mut last_break) = (0.0f32, start, None);
        while i < chars.len() {
            // Look ahead: a variation selector changes the width of the
            // codepoint in front of it, so wrapping has to know about it too.
            let a = fonts.advance_at(style, chars[i], chars.get(i + 1).copied(), size);
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

/// [`wrap_ranges`] for a mono face on an ASCII, control-free line: the same
/// break rule on bytes, with no char collection at all.
fn wrap_ranges_ascii(
    fonts: &FontSystem,
    size: f32,
    bytes: &[u8],
    max_w: f32,
) -> Vec<(usize, usize)> {
    let n = bytes.len();
    if n == 0 {
        return vec![(0, 0)];
    }
    // Monospace rows wrap on one fixed advance: asking the font per character
    // is the same answer every time, and at a display list of a few hundred
    // thousand characters that is a per-char cache walk dominating the layout.
    let adv = fonts.advance(FontStyle::Mono, '0', size).max(1.0);
    let per_row = ((max_w / adv).max(1.0)) as usize;
    let mut rows = Vec::new();
    let mut start = 0;
    while start < n {
        // A row that fits whole is never wrapped or trimmed, matching the
        // general path's final row.
        if n - start <= per_row {
            rows.push((start, n));
            break;
        }
        let ideal = start + per_row;
        // Prefer an ASCII space break inside the window; a break after a
        // space at position 0 is no progress, so hard-break instead (a window
        // narrower than a word must still advance).
        let cut = match bytes[start..ideal].iter().rposition(|&b| b == b' ') {
            Some(p) if p > 0 => start + p + 1,
            _ => ideal,
        };
        // Trailing spaces are dropped from the break — the same rule the
        // general path uses: this row ends before them, and the next one
        // starts after them, so rejoining with a single space holds.
        let mut end = cut;
        while end > start && bytes[end - 1] == b' ' {
            end -= 1;
        }
        rows.push((start, end));
        start = cut;
    }
    rows
}

/// One buffer line expanded for display: tabs become four spaces.
///
/// Borrowed when the line has no tab, which is the overwhelming majority of
/// them — `str::replace` always allocates, and layout runs this over every
/// line of the document, so on a 754k-line file that difference is three
/// quarters of a million allocations per relayout.
pub fn expand_tabs(line: &str) -> std::borrow::Cow<'_, str> {
    if line.contains('\t') {
        std::borrow::Cow::Owned(line.replace('\t', "    "))
    } else {
        std::borrow::Cow::Borrowed(line)
    }
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
/// T1 · The editor pane's band: the same rows `layout_plain(buf, w, ctx,
/// false)` emits, restricted to the lines `[first_line, next_line)` — but at
/// the *absolute* y those lines have in the full list, and with the full
/// list's height. Every scroll range, scrollbar and overlay arithmetic the
/// pane already does keeps working; only the ops are screenful-sized. This
/// is the arithmetic twin of `layout_plain`'s no-wrap loop (same baseline
/// rule, same tab expansion, same skip-empty-lines rule), scoped to a row
/// range; they must stay in step exactly, which R1's equality test
/// (`t1_editor_band_equals_layout_plain_per_pixel`) pins.
pub fn editor_band(
    buf: &Buffer,
    viewport_w: f32,
    ctx: &Ctx,
    first_line: usize,
    next_line: usize,
) -> DisplayList {
    let p = plain_params(viewport_w, ctx, false);
    let m = ctx.fonts.metrics(FontStyle::Mono, p.size);
    let n = buf.line_count();
    let first = first_line.min(n);
    let next = next_line.min(n).max(first);
    let mut ops = Vec::new();
    for i in first..next {
        let line = expand_tabs(buf.line(i));
        if line.is_empty() {
            continue;
        }
        let baseline = (p.pad + i as f32 * p.line_h + (p.line_h - m.height()) / 2.0 + m.ascent,)
            .0
            .round();
        ops.push(DrawOp::Text {
            x: p.x0,
            y: baseline,
            text: line.into_owned(),
            size: p.size,
            style: FontStyle::Mono,
            color: ctx.theme.fg,
        });
    }
    DisplayList::new(ops, p.pad * 2.0 + n as f32 * p.line_h, Vec::new())
}

/// T1 · h_max's per-keystroke helper: the widest extent among the rows an
/// edit log touched, including one merge-boundary row either side of each
/// span (a deletion may join fragments into a longer line at `e.start`).
/// ASCII rows score arithmetically, non-ASCII rows measure whole — the
/// same two-part rule `widest_width` runs, scoped to the touched rows so a
/// keystroke prices its own lines, not the document.
pub fn touched_extent(
    buf: &Buffer,
    edits: &[crate::core::buffer::EditSpan],
    viewport_w: f32,
    ctx: &Ctx,
) -> f32 {
    let p = plain_params(viewport_w, ctx, false);
    let adv = ctx.fonts.advance(FontStyle::Mono, '0', p.size).max(1.0);
    let n = buf.line_count();
    let mut extent = 0.0f32;
    for e in edits {
        let from = e.start.saturating_sub(1);
        let to = (e.start + e.inserted + 1).min(n);
        for i in from..to {
            let line = expand_tabs(buf.line(i));
            extent = extent.max(if line.is_ascii() {
                line.len() as f32 * adv
            } else {
                ctx.fonts.measure(FontStyle::Mono, &line, p.size)
            });
        }
    }
    extent
}

pub fn layout_plain(buf: &Buffer, viewport_w: f32, ctx: &Ctx, wrap: bool) -> DisplayList {
    // The editor pane (wrap=false) never folds a line into rows, and its
    // per-line output is exactly `buf.line(i)` tab-expanded into one Text op.
    // Running that through the ordinary wrap path walked every line's chars
    // with `advance_at` to produce the same single row — most of a huge
    // no-wrap pane's layout cost for no difference on screen. wrap=true (the
    // wrapped preview pane) keeps the full path below.
    if !wrap {
        let p = plain_params(viewport_w, ctx, false);
        let m = ctx.fonts.metrics(FontStyle::Mono, p.size);
        let mut ops = Vec::new();
        let mut y = p.pad;
        for i in 0..buf.line_count() {
            let line = expand_tabs(buf.line(i));
            if !line.is_empty() {
                let baseline = (y + (p.line_h - m.height()) / 2.0 + m.ascent).round();
                ops.push(DrawOp::Text {
                    x: p.x0,
                    y: baseline,
                    text: line.into_owned(),
                    size: p.size,
                    style: FontStyle::Mono,
                    color: ctx.theme.fg,
                });
            }
            y += p.line_h;
        }
        return DisplayList::new(ops, y + p.pad, Vec::new());
    }
    layout_plain_measured(buf, viewport_w, ctx, wrap, false).0
}

/// The no-wrap editor pane plus the pixel width of its widest line, from
/// one walk. T1 · The panes are bands now, so no installer pays this whole
/// walk any more; it stays for the perf gate's before/after timings (see
/// src/perf.rs's G3 probe) and as the arithmetic reference the band's
/// scattered pieces (`editor_band` + `widest_width`) must sum to.
#[cfg(test)]
pub fn layout_plain_edit(buf: &Buffer, viewport_w: f32, ctx: &Ctx) -> (DisplayList, f32) {
    layout_plain_measured(buf, viewport_w, ctx, false, true)
}

/// The widest display row's width, over the whole buffer, from **one rule**
/// (R1). ASCII on the mono face is exactly `char_count × advance` — zero
/// glyph calls — so those lines are scored arithmetically; at most one
/// non-ASCII line (the longest by bytes) is shaped and measured whole. That
/// is `install_preview`'s semantics, which are the more complete answer: a
/// short-ASCII/wide-render line next to a long-bytes/narrow-render one is
/// caught, which a longest-by-bytes scan loses.
///
/// Callers: `install_preview`, `relayout`'s Edit branch and
/// `relayout_band`'s editor half — each behind their `tab.widest` cache,
/// so a keystroke grows it through `touched_extent` instead of a
/// whole-document re-scan.
pub fn widest_width(buf: &Buffer, viewport_w: f32, ctx: &Ctx, wrap: bool) -> f32 {
    let p = plain_params(viewport_w, ctx, wrap);
    let adv = ctx.fonts.advance(FontStyle::Mono, '0', p.size).max(1.0);
    let mut extent = 0.0f32;
    let mut wide = String::new();
    let mut wide_len = 0usize;
    for i in 0..buf.line_count() {
        let line = expand_tabs(buf.line(i));
        if line.is_ascii() {
            extent = extent.max(line.len() as f32 * adv);
        } else if line.len() > wide_len {
            wide_len = line.len();
            wide.clear();
            wide.push_str(&line);
        }
    }
    extent.max(ctx.fonts.measure(FontStyle::Mono, &wide, p.size))
}

/// O(1) per line: a row count needs the wrap engine, but a *height-only*
/// pass over mostly-normal lines can use the mono fast path's own break
/// arithmetic, with no Vec allocation per line (a 300k-line file wrapped
/// through `wrap_line` spent the whole pass in allocator traffic — 450 ms
/// on the pump's deadline). Only the mono-ASCII fast path takes the
/// arithmetic; anything else falls back to `wrap_ranges`, which is the same
/// engine the fills use, so the total still matches a real layout.
fn line_row_count(
    fonts: &FontSystem,
    style: FontStyle,
    size: f32,
    line: &str,
    max_w: f32,
) -> usize {
    // The ASCII-mono short-circuit scans the str in place — no Vec<char>
    // collect (that collect alone was ~half the height pass on a 300k-line
    // stream: 450 ms of allocator traffic per stagger).
    if style == FontStyle::Mono && line.chars().all(|c| c.is_ascii() && !c.is_control()) {
        let adv = fonts.advance(style, '0', size).max(1.0);
        let per_row = ((max_w / adv).max(1.0)) as usize;
        let bytes = line.as_bytes();
        let n = bytes.len();
        if n == 0 {
            return 1;
        }
        // Same break rule as `wrap_ranges`'s fast path: an ideal cut at a
        // per-row window that falls after a space, trailing spaces dropped,
        // a window narrower than a word hard-breaks, the final short row
        // taken whole.
        let mut rows = 0usize;
        let mut start = 0usize;
        while start < n {
            if n - start <= per_row {
                rows += 1;
                break;
            }
            let ideal = start + per_row;
            let cut = match bytes[start..ideal].iter().rposition(|&b| b == b' ') {
                Some(p) if p > 0 => start + p + 1,
                _ => ideal,
            };
            rows += 1;
            start = cut;
        }
        rows.max(1)
    } else {
        wrap_ranges(fonts, style, size, line, max_w).len().max(1)
    }
}

/// The wrapped preview's incremental row-count: `cached` holds the
/// row counts for the buffer's first lines *as wrapped at the caller's last
/// width*; if the width did not change (`same_width`)
/// and the cached prefix is a prefix of the buffer, only the newly appended
/// tail is wrapped — the O(new lines) that keeps a streaming pump off the
/// full-document deadline. A width change recounts from the top (rare).
pub fn plain_wrapped_height_incremental(
    buf: &Buffer,
    viewport_w: f32,
    ctx: &Ctx,
    mut cached: Vec<u32>,
    same_width: bool,
) -> (f32, Vec<u32>) {
    let p = plain_params(viewport_w, ctx, true);
    let n = buf.line_count();
    let from = if same_width && cached.len() <= n {
        cached.len()
    } else {
        cached.clear();
        cached.reserve(n);
        0
    };
    let mut total: usize = cached.iter().map(|&r| r as usize).sum();
    // The mono fast path's per-row width, hoisted out of the loop: one
    // advance() lookup per pass, not per line (the per-line lookup was
    // ~half the count pass on a 300k-line tail).
    let per_row = if let Some(pr) = mono_per_row(ctx, viewport_w) {
        pr
    } else {
        // Non-mono geometry (proportional face): the per-line fallback path
        // below stays correct by construction.
        usize::MAX
    };
    for i in from..n {
        let raw = buf.line(i);
        let line = expand_tabs(raw);
        let rows = line_rows(&line, ctx, &p, per_row);
        cached.push(rows as u32);
        total += rows;
    }
    let height = p.pad * 2.0 + total as f32 * p.line_h;
    (height, cached)
}

/// Rows one buffer line wraps to in the plain pane. Planning a document and
/// patching one line of it both come through here, so a patched plan cannot
/// drift from a freshly built one.
fn line_rows(line: &str, ctx: &Ctx, p: &PlainParams, per_row: usize) -> usize {
    if line.is_empty() {
        1
    } else if per_row != usize::MAX && line.as_bytes().iter().all(|&b| (0x20..0x7F).contains(&b)) {
        mono_ascii_rows(line.as_bytes(), per_row)
    } else {
        line_row_count(ctx.fonts, FontStyle::Mono, p.size, line, p.col_w)
    }
}

/// Rows per row-window for one ASCII mono line, given the hoisted per-row
/// character count. Same break rule as `wrap_ranges`'s fast path.
fn mono_ascii_rows(bytes: &[u8], per_row: usize) -> usize {
    let n = bytes.len();
    let mut rows = 0usize;
    let mut start = 0usize;
    while start < n {
        if n - start <= per_row {
            rows += 1;
            break;
        }
        let ideal = start + per_row;
        let cut = match bytes[start..ideal].iter().rposition(|&b| b == b' ') {
            Some(p) if p > 0 => start + p + 1,
            _ => ideal,
        };
        rows += 1;
        start = cut;
    }
    rows.max(1)
}

/// The mono fast path's per-row char count at this width, or `None` when the
/// plain pane is not on the mono face at the expected size (the caller then
/// takes the general per-line path).
fn mono_per_row(ctx: &Ctx, viewport_w: f32) -> Option<usize> {
    let p = plain_params(viewport_w, ctx, true);
    let adv = ctx.fonts.advance(FontStyle::Mono, '0', p.size).max(1.0);
    Some(((p.col_w / adv).max(1.0)) as usize)
}

/// The wrapped preview rows a viewport [top, bottom) shows: a *band* of rows
/// at real scroll coordinates. This is what a streaming tab's relayout
/// builds instead of the whole document — the pump repaints every stagger,
/// and a big file's full wrap pass is hundreds of ms while a band is
/// microseconds.
///
/// The row geometry matches `layout_plain(.., true)` exactly (same params,
/// same wrap engine, same baseline rounding), so the shape of the text does
/// not shift when the seal's full layout replaces the band. `cached` carries
/// the tab's per-line row counts between staggers (see
/// [`plain_wrapped_height_incremental`]), so the pre-band lines cost only a
/// prefix sum, no wrap work.
pub fn layout_plain_band(
    buf: &Buffer,
    viewport_w: f32,
    ctx: &Ctx,
    top: f32,
    bottom: f32,
    cached: Vec<u32>,
    same_width: bool,
) -> (DisplayList, Vec<u32>) {
    let p = plain_params(viewport_w, ctx, true);
    let style = FontStyle::Mono;
    let m = ctx.fonts.metrics(style, p.size);
    // The per-line row counts are the index: line i's first row top is
    // pad + (sum of rows before i) * line_h; the incremental count reuses
    // the cached prefix and wraps only what the pump appended.
    let (height, rows_per_line) =
        plain_wrapped_height_incremental(buf, viewport_w, ctx, cached, same_width);
    let mut ops = Vec::new();
    let mut y = p.pad;
    for (i, &rows) in rows_per_line.iter().enumerate() {
        let line_h0 = y;
        let line_end = line_h0 + rows as f32 * p.line_h;
        if line_end >= top && line_h0 <= bottom {
            let line = expand_tabs(buf.line(i));
            if line.is_empty() {
                // An empty row: nothing visible, but the cursor advanced.
            } else {
                for (k, row) in wrap_line(ctx.fonts, style, p.size, &line, p.col_w)
                    .into_iter()
                    .enumerate()
                {
                    let row_top = line_h0 + k as f32 * p.line_h;
                    if row_top + p.line_h >= top && row_top <= bottom && !row.is_empty() {
                        let baseline = (row_top + (p.line_h - m.height()) / 2.0 + m.ascent).round();
                        ops.push(DrawOp::Text {
                            x: p.x0,
                            y: baseline,
                            text: row,
                            size: p.size,
                            style,
                            color: ctx.theme.fg,
                        });
                    }
                }
            }
        }
        y = line_end;
        if y > bottom {
            break;
        }
    }
    (
        // The band reports the *document* height, not the band height: the
        // pane's scrollbar (v_geom) sizes from this and must page honestly.
        DisplayList::new(ops, height, Vec::new()),
        rows_per_line,
    )
}

/// The wrapped preview as a plan: one entry per buffer line, at the y a full
/// layout would place it. The row counts come from the same counting pass the
/// streaming pump uses (mono-ASCII arithmetic, `wrap_ranges` for anything
/// else), so the height and every line's top match [`layout_plain`] exactly.
///
/// This is what makes a big text pane a band: a document with a million lines
/// keeps one `f32` per line instead of a `String` per wrapped row.
pub fn plan_lines(buf: &Buffer, viewport_w: f32, ctx: &Ctx) -> plan::Rows {
    let p = plain_params(viewport_w, ctx, true);
    let (height, rows) = plain_wrapped_height_incremental(buf, viewport_w, ctx, Vec::new(), false);
    let mut tops = Vec::with_capacity(rows.len());
    let mut y = p.pad;
    for r in rows {
        tops.push(y);
        y += r as f32 * p.line_h;
    }
    plan::Rows::new(tops, height, p.pad)
}

/// Patch a plain pane's plan for the lines an edit touched.
///
/// Only `inserted` lines per span are counted again; everything below shifts by
/// the height difference. The count is the same function a fresh plan uses, so
/// the result is the plan the document would have been given whole.
pub fn patch_lines(
    rows: &mut plan::Rows,
    buf: &Buffer,
    viewport_w: f32,
    ctx: &Ctx,
    edits: &[EditSpan],
) -> bool {
    let p = plain_params(viewport_w, ctx, true);
    let per_row = mono_per_row(ctx, viewport_w).unwrap_or(usize::MAX);
    let mut heights: Vec<f32> = Vec::new();
    for e in edits {
        if e.start + e.inserted > buf.line_count() {
            return false;
        }
        heights.clear();
        for i in e.start..e.start + e.inserted {
            let line = expand_tabs(buf.line(i));
            heights.push(line_rows(&line, ctx, &p, per_row) as f32 * p.line_h);
        }
        if !rows.splice(e.start, e.removed, &heights) {
            return false;
        }
    }
    true
}

/// Lay out only the lines whose wrapped rows overlap `[top, bottom)`, at the
/// y the plan carries. Same wrap engine and baseline rounding as
/// [`layout_plain`], so the band is the full list with everything else left
/// out — not an approximation of it.
pub fn layout_lines_band(
    buf: &Buffer,
    rows_at: &plan::Rows,
    viewport_w: f32,
    ctx: &Ctx,
    top: f32,
    bottom: f32,
) -> DisplayList {
    let p = plain_params(viewport_w, ctx, true);
    let style = FontStyle::Mono;
    let m = ctx.fonts.metrics(style, p.size);
    let mut ops = Vec::new();
    for i in rows_at.band(top, bottom) {
        let line = expand_tabs(buf.line(i));
        let mut y = rows_at.top(i);
        for row in wrap_line(ctx.fonts, style, p.size, &line, p.col_w) {
            if !row.is_empty() && y + p.line_h >= top && y <= bottom {
                let baseline = (y + (p.line_h - m.height()) / 2.0 + m.ascent).round();
                ops.push(DrawOp::Text {
                    x: p.x0,
                    y: baseline,
                    text: row,
                    size: p.size,
                    style,
                    color: ctx.theme.fg,
                });
            }
            y += p.line_h;
        }
    }
    DisplayList::new(ops, rows_at.height(), Vec::new())
}

fn layout_plain_measured(
    buf: &Buffer,
    viewport_w: f32,
    ctx: &Ctx,
    wrap: bool,
    want_width: bool,
) -> (DisplayList, f32) {
    if !wrap {
        let p = plain_params(viewport_w, ctx, false);
        let m = ctx.fonts.metrics(FontStyle::Mono, p.size);
        let mut ops = Vec::new();
        let mut y = p.pad;
        // R1 · the `widest_width` rule, inline so the walk stays single-
        // pass: ASCII is `len × advance`, at most one non-ASCII line (the
        // longest by bytes) is measured whole. Same answer as `widest_width`
        // on the same buffer by construction.
        let adv = ctx.fonts.advance(FontStyle::Mono, '0', p.size).max(1.0);
        let mut extent = 0.0f32;
        let mut longest = String::new();
        let mut longest_w = 0usize;
        for i in 0..buf.line_count() {
            let line = expand_tabs(buf.line(i));
            if want_width {
                if line.is_ascii() {
                    extent = extent.max(line.len() as f32 * adv);
                } else if line.len() > longest_w {
                    longest_w = line.len();
                    longest.clear();
                    longest.push_str(&line);
                }
            }
            if !line.is_empty() {
                let baseline = (y + (p.line_h - m.height()) / 2.0 + m.ascent).round();
                ops.push(DrawOp::Text {
                    x: p.x0,
                    y: baseline,
                    text: line.into_owned(),
                    size: p.size,
                    style: FontStyle::Mono,
                    color: ctx.theme.fg,
                });
            }
            y += p.line_h;
        }
        let width = if want_width {
            extent.max(ctx.fonts.measure(FontStyle::Mono, &longest, p.size))
        } else {
            0.0
        };
        let out = DisplayList::new(ops, y + p.pad, Vec::new());
        return (out, width);
    }
    #[cfg(any(test, feature = "mem-census"))]
    crate::mem::begin(crate::mem::Component::DisplayList);
    let t = ctx.theme;
    let style = FontStyle::Mono;
    let p = plain_params(viewport_w, ctx, wrap);
    let m = ctx.fonts.metrics(style, p.size);

    let mut ops = Vec::new();
    let mut y = p.pad;
    for i in 0..buf.line_count() {
        let line = expand_tabs(buf.line(i));
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
    let out = DisplayList::new(ops, y + p.pad, Vec::new());
    #[cfg(any(test, feature = "mem-census"))]
    crate::mem::end();
    (out, 0.0)
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
    // The editor pane never wraps: one row per line, so the row index *is*
    // the line index — no walking the document to find the row.
    if !wrap {
        if y >= p.pad + buf.line_count() as f32 * p.line_h {
            return buf.end();
        }
        let li = (((y - p.pad) / p.line_h).floor().max(0.0) as usize)
            .min(buf.line_count().saturating_sub(1));
        let exp = expand_line(buf.line(li));
        let mut w = 0.0;
        let mut col = 0usize;
        for k in 0..exp.chars.len() {
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
    // The editor pane never wraps: the caret's row is its line, at
    // pad + line*line_h — no walking the document to find it.
    if !wrap {
        let line = buf.line(caret.line);
        let exp = expand_line(line);
        let eb = orig_to_exp(line, caret.col);
        let mut w = 0.0;
        for k in 0..exp.chars.len() {
            if exp.byte_at[k] >= eb {
                break;
            }
            w += advance_of(ctx, exp.chars[k], p.size);
        }
        return (p.x0 + w, p.pad + caret.line as f32 * p.line_h);
    }
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
    // The editor pane never wraps: one row per line at pad + line*line_h, so
    // only the selected lines are worth visiting. Wrapped panes must walk
    // every line to keep the row_top cursor honest.
    let (lo, hi, row0) = if wrap {
        (0, buf.line_count(), p.pad)
    } else {
        let hi = (to.line + 1).min(buf.line_count());
        (from.line.min(hi), hi, p.pad + from.line as f32 * p.line_h)
    };
    let mut row_top = row0;
    for li in lo..hi {
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

/// Highlight rectangles for many ranges at once (the find bar's matches) on
/// the no-wrap editor grid: one row per line, so each range is a single
/// horizontal band and only the matched characters are measured — the cost
/// follows the match count, not the document size. `ranges` must be sorted
/// in reading order and non-overlapping (find's matches are both).
pub fn match_rects(buf: &Buffer, ranges: &[(Pos, Pos)], viewport_w: f32, ctx: &Ctx) -> Vec<Rect> {
    let p = plain_params(viewport_w, ctx, false);
    let mut rects = Vec::new();
    for &(from, to) in ranges {
        let (from, to) = if from <= to { (from, to) } else { (to, from) };
        if from == to || from.line != to.line {
            continue;
        }
        let line = buf.line(from.line);
        let exp = expand_line(line);
        let y = p.pad + from.line as f32 * p.line_h;
        let x0 = exp_x(&exp, line, from.col, ctx, p.size);
        let x1 = exp_x(&exp, line, to.col, ctx, p.size);
        if x1 > x0 {
            rects.push(Rect::new(p.x0 + x0, y, (x1 - x0).max(2.0), p.line_h));
        }
    }
    rects
}

/// Document-space x of a byte column on an expanded (tabs -> 4 spaces) line.
fn exp_x(exp: &ExpLine, line: &str, ob: usize, ctx: &Ctx, size: f32) -> f32 {
    let eb = orig_to_exp(line, ob);
    let mut w = 0.0;
    for k in 0..exp.chars.len() {
        if exp.byte_at[k] >= eb {
            break;
        }
        w += advance_of(ctx, exp.chars[k], size);
    }
    w
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

    #[test]
    fn no_wrap_geometry_jumps_straight_to_the_line() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let ctx = Ctx {
            fonts: &f,
            theme: &t,
            scale: 1.0,
            zoom: 1.0,
        };
        let buf = Buffer::from_text("aaa\nbbb\nccc");
        let p = plain_params(800.0, &ctx, false);
        // One row per line at pad + line*line_h: a multi-line selection's
        // rects sit on exactly those rows.
        let rects = selection_rects(
            &buf,
            Pos { line: 0, col: 1 },
            Pos { line: 2, col: 2 },
            800.0,
            &ctx,
            false,
        );
        assert_eq!(rects.len(), 3);
        for (i, r) in rects.iter().enumerate() {
            assert!(
                (r.y - (p.pad + i as f32 * p.line_h)).abs() < 0.5,
                "row {i} drawn at {}",
                r.y
            );
        }
        // A click far below the text is the buffer end, not a phantom row.
        assert_eq!(
            hit_test(&buf, 10.0, p.pad + 99.0 * p.line_h, 800.0, &ctx, false),
            buf.end()
        );
        // The caret of a late line is found without walking the early ones.
        let (x, y) = caret_point(&buf, Pos { line: 2, col: 2 }, 800.0, &ctx, false);
        assert!((y - (p.pad + 2.0 * p.line_h)).abs() < 0.5);
        assert!(x > p.x0, "caret x is inside the pane");
    }
}

#[cfg(test)]
mod band_tests {
    use super::*;
    use crate::layout::theme::Theme;

    fn ctx<'a>(f: &'a FontSystem, t: &'a Theme) -> Ctx<'a> {
        Ctx {
            fonts: f,
            theme: t,
            scale: 1.0,
            zoom: 1.0,
        }
    }

    fn doc() -> Buffer {
        let mut s = String::new();
        for i in 0..400 {
            // A mix of short lines, long wrapping lines and blank lines.
            match i % 7 {
                0 => s.push('\n'),
                1 => s.push_str(&format!("line {i} {}\n", "wrap ".repeat(60))),
                _ => s.push_str(&format!("line {i} alpha bravo charlie\n")),
            }
        }
        Buffer::from_text(&s)
    }

    fn text_of(op: &DrawOp) -> String {
        format!("{op:?}")
    }

    /// A band over everything is the full layout. That is the strongest form
    /// of parity: every row's y, baseline and text came out of the same
    /// emitter, so a plan can never drift from the pane it replaces.
    #[test]
    fn a_whole_document_band_is_the_full_preview() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let buf = doc();
        let full = layout_plain(&buf, 900.0, &c, true);
        let plan = plan_lines(&buf, 900.0, &c);
        let band = layout_lines_band(&buf, &plan, 900.0, &c, f32::NEG_INFINITY, f32::INFINITY);
        assert_eq!(band.ops.len(), full.ops.len(), "row count");
        assert!((band.height - full.height).abs() < 0.5);
        assert_eq!(
            band.ops.iter().map(text_of).collect::<Vec<_>>(),
            full.ops.iter().map(text_of).collect::<Vec<_>>()
        );
    }

    /// A band at a scroll offset claims nothing the full layout does not have
    /// and misses nothing visible. The interior check matters: the edges are
    /// allowed a row of overlap, which is what makes a half-scrolled wrapped
    /// row paint instead of leaving a gap.
    #[test]
    fn a_band_claims_nothing_and_misses_nothing_visible() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let buf = doc();
        let full = layout_plain(&buf, 900.0, &c, true);
        let plan = plan_lines(&buf, 900.0, &c);
        let lh = line_height(900.0, &c);
        let all: Vec<String> = full.ops.iter().map(text_of).collect();
        for top in [0.0, 200.0, 1_500.0, plan.height() - 400.0] {
            let bottom = top + 700.0;
            let band = layout_lines_band(&buf, &plan, 900.0, &c, top, bottom);
            let mut seen = 0;
            for op in &band.ops {
                let s = text_of(op);
                let at = all[seen..]
                    .iter()
                    .position(|a| *a == s)
                    .unwrap_or_else(|| panic!("at {top}: the band invented {s}"));
                seen += at + 1;
            }
            // Every op whose row is inside the viewport is there.
            let inside = full
                .ops
                .iter()
                .filter(|op| matches!(op, DrawOp::Text { y, .. } if *y >= top + lh && *y <= bottom - lh))
                .map(text_of)
                .collect::<Vec<_>>();
            let got = band.ops.iter().map(text_of).collect::<Vec<_>>();
            for s in inside {
                assert!(got.contains(&s), "at {top}: the band missed {s}");
            }
        }
    }

    /// Growth in the middle, a deletion near the end and a split at the top,
    /// each patched as the edit arrives (the app's order), leave the index a
    /// fresh plan gives the edited text. Then a whole-document band over it is
    /// the full layout op for op, so a patched row's baseline and text are
    /// right, not just its height.
    #[test]
    fn edits_patch_the_plan_to_match_a_fresh_one() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let mut buf = doc();
        let mut plan = plan_lines(&buf, 900.0, &c);
        let patch = |buf: &mut Buffer, plan: &mut plan::Rows| {
            let edits = buf.take_edits().expect("one edit");
            assert_eq!(edits.len(), 1, "{edits:?}");
            assert!(patch_lines(plan, buf, 900.0, &c, &edits));
        };
        buf.insert(
            Pos { line: 200, col: 0 },
            &format!("{}\nnew line\n", "wrap ".repeat(80)),
        );
        patch(&mut buf, &mut plan);
        buf.delete(Pos { line: 350, col: 0 }, Pos { line: 352, col: 0 });
        patch(&mut buf, &mut plan);
        buf.insert(Pos { line: 3, col: 2 }, "\n");
        patch(&mut buf, &mut plan);

        let fresh = plan_lines(&buf, 900.0, &c);
        assert_eq!(plan.len(), fresh.len(), "unit count");
        for i in 0..fresh.len() {
            assert_eq!(plan.top(i), fresh.top(i), "top {i}");
        }
        assert_eq!(plan.height(), fresh.height());
        let full = layout_plain(&buf, 900.0, &c, true);
        let band = layout_lines_band(&buf, &plan, 900.0, &c, f32::NEG_INFINITY, f32::INFINITY);
        assert_eq!(all_ops(&band), all_ops(&full), "ops after the patch");
    }

    /// A row-count change shifts every unit below it by exactly the rows that
    /// came or went. That is the whole point of the splice: it costs a pass
    /// over the tops, not a measure of the document.
    #[test]
    fn patching_shifts_the_units_below_by_the_row_delta() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let lh = line_height(900.0, &c);
        let mut buf = Buffer::from_text(&"one word per line\n".repeat(50));
        let mut plan = plan_lines(&buf, 900.0, &c);
        let below = plan.top(40);
        // A line that wraps to several rows replaces a one-row line.
        buf.insert(
            Pos { line: 10, col: 0 },
            &format!("{}\n", "wrap ".repeat(120)),
        );
        let edits = buf.take_edits().unwrap();
        assert!(patch_lines(&mut plan, &buf, 900.0, &c, &edits));
        // Unit 10 is now the inserted line; unit 11 is the old line 10.
        let added = plan.top(11) - plan.top(10) - lh;
        assert!(added > 0.0, "the inserted line wrapped");
        // The old line 40 is unit 41 now, and it moved down by exactly the
        // height of the inserted line.
        assert!((plan.top(41) - below - (added + lh)).abs() < 0.5);
    }

    /// T1's verify: the band restricted to `[first, next)` carries exactly
    /// the rows the full no-wrap list has in that window — same texts, same
    /// baselines, same order — and keeps the full list's height.
    #[test]
    fn t1_editor_band_equals_layout_plain_per_pixel() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let ctx = Ctx {
            fonts: &f,
            theme: &t,
            scale: 1.0,
            zoom: 1.0,
        };
        let mut text = String::new();
        text.push_str(&"x".repeat(200));
        text.push('\n');
        text.push_str("\texpands\twide\n");
        text.push_str(&"é".repeat(180));
        text.push('\n');
        text.push_str(&"日本".repeat(90));
        text.push('\n');
        for i in 0..30 {
            text.push_str(&format!("line {i}\n"));
        }
        let buf = Buffer::from_text(&text);
        let p = plain_params(720.0, &ctx, false);
        let full = layout_plain(&buf, 720.0, &ctx, false);
        let (first, next) = (2usize, 9usize);
        let band = editor_band(&buf, 720.0, &ctx, first, next);
        assert_eq!(
            band.height, full.height,
            "the band keeps the full list's height"
        );
        assert!(
            band.ops.len() < full.ops.len(),
            "the band is screenful-sized: {} vs {}",
            band.ops.len(),
            full.ops.len()
        );
        // A row's baseline sits inside its own row band, so filtering the
        // full list's ops by baseline is exact.
        let (lo, hi) = (
            p.pad + first as f32 * p.line_h,
            p.pad + next as f32 * p.line_h,
        );
        let rows = |ops: &[DrawOp]| {
            ops.iter()
                .filter_map(|op| match op {
                    DrawOp::Text { y, text, .. } => Some((*y, text.clone())),
                    _ => None,
                })
                .filter(|(y, _)| *y >= lo && *y < hi)
                .collect::<Vec<_>>()
        };
        let want = rows(&full.ops);
        let got = rows(&band.ops);
        assert!(
            !want.is_empty(),
            "the window must carry rows (fixture has {first}..{next} of {})",
            buf.line_count()
        );
        assert_eq!(
            want, got,
            "band rows equal the full list's rows in the window"
        );
    }

    fn all_ops(list: &DisplayList) -> Vec<String> {
        list.ops.iter().map(text_of).collect()
    }
}
