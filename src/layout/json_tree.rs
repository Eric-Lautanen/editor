//! Collapsible JSON tree layout: parsed values -> tinted, indented rows.
//!
//! Every row is a run of single-style text segments (keys, strings, numbers,
//! punctuation each get their theme color). Rows are wrapped with a hanging
//! indent via [`wrap_ranges`]; the tree is all monospace so char-index math
//! holds. Containers carry `▶`/`▼` expanders backed by [`HitAction`]
//! regions; collapsed subtrees keep stable node ids by walking children
//! silently instead of skipping them.

use super::{
    display_list::{DisplayList, DrawOp, FontStyle, HitAction, HitRegion},
    text::wrap_ranges,
    theme::Theme,
    Ctx,
};
use crate::{
    core::buffer::{Buffer, EditSpan},
    error::Error,
    gfx::{color::Color, font::FontSystem, geom::Rect},
    parse::{
        json::{parser, pretty, Value},
        jsonl,
        jsonl::Record,
    },
};
use std::collections::HashSet;
use std::fmt::Write as _;

struct Seg {
    text: String,
    color: Color,
}

impl Seg {
    fn new(text: impl Into<String>, color: Color) -> Self {
        Self {
            text: text.into(),
            color,
        }
    }
}

struct Tree<'a> {
    fonts: &'a FontSystem,
    theme: &'a Theme,
    size: f32,
    line_h: f32,
    ascent: f32,
    height: f32,
    adv: f32,
    indent_w: f32,
    x0: f32,
    col_w: f32,
    pad: f32,
    collapsed: &'a HashSet<usize>,
    next_id: usize,
    /// False while walking a collapsed subtree (ids still assigned, rows not).
    emit: bool,
    ops: Vec<DrawOp>,
    hits: Vec<HitRegion>,
    y: f32,
}

impl Tree<'_> {
    fn baseline(&self, top: f32) -> f32 {
        (top + (self.line_h - self.height) / 2.0 + self.ascent).round()
    }

    fn text_op(&mut self, x: f32, baseline: f32, text: String, color: Color) {
        self.ops.push(DrawOp::Text {
            x,
            y: baseline,
            text,
            size: self.size,
            style: FontStyle::Mono,
            color,
        });
    }

    /// Emit one logical row (wrapped with a hanging indent). The toggle, if
    /// any, lives at the start of the first visual row only. `toggle` carries
    /// the node id and the char offset of the `▶`/`▼` expander within the row
    /// row — the hit area spans from the row start through the marker, so both
    /// the key text and the glyph itself are clickable.
    fn emit_row(&mut self, indent: usize, segs: Vec<Seg>, toggle: Option<(usize, usize)>) {
        if !self.emit {
            return;
        }
        let x_indent = self.x0 + indent as f32 * self.indent_w;
        // Folded rows hang one indent step in, so the wrap width reserves
        // that step: a continuation row then ends exactly on the column's
        // right edge instead of running one indent past it.
        let avail = (self.col_w - (x_indent - self.x0) - self.indent_w).max(self.adv * 8.0);
        // The tree is monospace, so an ASCII row can place every segment by
        // multiplying the cell advance — the old path did that for all text.
        // Wide glyphs are the exception: measure the prefix only when the row
        // actually contains one, instead of on every segment of every row.
        //
        // The count is also the row's width test: a row whose cells fit the
        // column is emitted segment by segment, moving each segment's text
        // straight into its op. Almost every row in a tree fits, and the
        // general path — a joined row string, a char-offset table, the wrap
        // call, and a fresh `String` per segment per visual row — was six
        // allocations and two scans just to decide it was one row wide.
        let mut cells = 0usize;
        let mut one_cell = true;
        for s in &segs {
            if s.text.is_ascii() {
                cells += s.text.len();
                continue;
            }
            for c in s.text.chars() {
                if !matches!(c, '▼' | '▶' | '…') {
                    one_cell = false;
                    break;
                }
                cells += 1;
            }
            if !one_cell {
                break;
            }
        }
        if one_cell && cells as f32 * self.adv <= avail {
            let baseline = self.baseline(self.y);
            let mut cell = 0usize;
            for seg in segs {
                if seg.text.is_empty() {
                    continue;
                }
                let n = if seg.text.is_ascii() {
                    seg.text.len()
                } else {
                    seg.text.chars().count()
                };
                self.text_op(
                    x_indent + cell as f32 * self.adv,
                    baseline,
                    seg.text,
                    seg.color,
                );
                cell += n;
            }
            if let Some((id, off)) = toggle {
                self.hits.push(HitRegion {
                    rect: Rect::new(
                        x_indent,
                        self.y,
                        off as f32 * self.adv + self.adv * 2.0,
                        self.line_h,
                    ),
                    action: HitAction::ToggleNode(id),
                });
            }
            self.y += self.line_h;
            return;
        }
        let full: String = segs.iter().map(|s| s.text.as_str()).collect();
        // The expander and ellipsis are one cell but not ASCII. Give the
        // wrapper an ASCII stand-in of the same char count so it can take the
        // byte fast path; the returned ranges still index `full`.
        let width_text = (one_cell && !full.is_ascii()).then(|| {
            full.chars()
                .map(|c| if c.is_ascii() { c } else { 'x' })
                .collect::<String>()
        });
        // Cumulative char offsets per segment for slicing wrapped pieces back.
        let mut starts = Vec::with_capacity(segs.len());
        let mut off = 0;
        for s in &segs {
            starts.push(off);
            off += s.text.chars().count();
        }
        let rows = wrap_ranges(
            self.fonts,
            FontStyle::Mono,
            self.size,
            width_text.as_deref().unwrap_or(&full),
            avail,
        );
        for (ri, &(s, e)) in rows.iter().enumerate() {
            let row_x = if ri == 0 {
                x_indent
            } else {
                x_indent + self.indent_w
            };
            let baseline = self.baseline(self.y);
            for (seg, &seg_start) in segs.iter().zip(starts.iter()) {
                let seg_len = seg.text.chars().count();
                let (a, b) = (s.max(seg_start), e.min(seg_start + seg_len));
                if a < b {
                    let piece: String = seg.text.chars().skip(a - seg_start).take(b - a).collect();
                    let dx = if one_cell {
                        (a - s) as f32 * self.adv
                    } else {
                        self.fonts
                            .measure_range(FontStyle::Mono, &full, s, a, self.size)
                    };
                    self.text_op(row_x + dx, baseline, piece, seg.color);
                }
            }
            // Toggle hit area covers the marker on the first visual row. A
            // wide key puts the expander farther right, so measure there; an
            // ASCII prefix is `off` cells.
            if ri == 0 {
                if let Some((id, off)) = toggle {
                    let prefix = if one_cell {
                        off as f32 * self.adv
                    } else {
                        self.fonts
                            .measure_range(FontStyle::Mono, &full, 0, off, self.size)
                    };
                    self.hits.push(HitRegion {
                        rect: Rect::new(x_indent, self.y, prefix + self.adv * 2.0, self.line_h),
                        action: HitAction::ToggleNode(id),
                    });
                }
            }
            self.y += self.line_h;
        }
    }

    fn scalar_segs(&self, v: &Value) -> Vec<Seg> {
        let t = self.theme;
        match v {
            Value::Null => vec![Seg::new("null", t.json_literal)],
            Value::Bool(true) => vec![Seg::new("true", t.json_literal)],
            Value::Bool(false) => vec![Seg::new("false", t.json_literal)],
            Value::Number(n) => vec![Seg::new(n.clone(), t.json_number)],
            Value::String(s) => vec![Seg::new(pretty::escape(s), t.json_string)],
            // The only row that is not JSON: the text here does not parse, so
            // the row says why instead of pretending to be a value.
            Value::Broken(msg) => vec![Seg::new(format!("! {msg}"), t.error)],
            Value::Array(_) | Value::Object(_) => vec![],
        }
    }

    fn render_value(&mut self, v: &Value, indent: usize, mut head: Vec<Seg>, trailing_comma: bool) {
        let t = self.theme;
        let comma = |segs: &mut Vec<Seg>| {
            if trailing_comma {
                segs.push(Seg::new(",", t.muted));
            }
        };
        let (is_obj, len) = match v {
            Value::Object(pairs) => (true, pairs.len()),
            Value::Array(items) => (false, items.len()),
            scalar => {
                head.extend(self.scalar_segs(scalar));
                comma(&mut head);
                self.emit_row(indent, head, None);
                return;
            }
        };
        let (open, close) = if is_obj { ("{", "}") } else { ("[", "]") };
        if len == 0 {
            head.push(Seg::new(format!("{open}{close}"), t.fg));
            comma(&mut head);
            self.emit_row(indent, head, None);
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        // Char offset of the expander glyph within the row: the hit area spans
        // from the row start through the marker so the key text and the glyph
        // itself are both clickable.
        let off: usize = head.iter().map(|s| s.text.chars().count()).sum();
        if self.collapsed.contains(&id) {
            head.push(Seg::new("▶", t.accent));
            head.push(Seg::new(" ", t.fg));
            head.push(Seg::new(format!("{open}…{close}"), t.fg));
            comma(&mut head);
            let what = if is_obj {
                if len == 1 {
                    "field"
                } else {
                    "fields"
                }
            } else if len == 1 {
                "item"
            } else {
                "items"
            };
            head.push(Seg::new(format!("  {len} {what}"), t.muted));
            self.emit_row(indent, head, Some((id, off)));
            // Walk silently so toggling never renumbers later nodes.
            let emit = std::mem::replace(&mut self.emit, false);
            self.render_children(v, indent + 1);
            self.emit = emit;
            return;
        }
        head.push(Seg::new("▼", t.accent));
        head.push(Seg::new(" ", t.fg));
        head.push(Seg::new(open, t.fg));
        self.emit_row(indent, head, Some((id, off)));
        self.render_children(v, indent + 1);
        if self.emit {
            // The comma belongs after the closing bracket, where JSON puts it.
            let mut close_segs = vec![Seg::new(close, t.fg)];
            comma(&mut close_segs);
            self.emit_row(indent, close_segs, None);
        }
    }

    fn render_children(&mut self, v: &Value, indent: usize) {
        let t = self.theme;
        match v {
            Value::Object(pairs) => {
                let last = pairs.len().saturating_sub(1);
                for (i, (k, child)) in pairs.iter().enumerate() {
                    let head = vec![
                        Seg::new(pretty::escape(k), t.json_key),
                        Seg::new(": ", t.fg),
                    ];
                    // Objects render as JSON, so every field but the last
                    // carries the comma the source would have had.
                    self.render_value(child, indent, head, i != last);
                }
            }
            Value::Array(items) => {
                let last = items.len().saturating_sub(1);
                for (i, child) in items.iter().enumerate() {
                    let head = vec![Seg::new(i.to_string(), t.muted), Seg::new(": ", t.fg)];
                    self.render_value(child, indent, head, i != last);
                }
            }
            _ => {}
        }
    }

    /// Error banner: tinted panel, `line:col: message`, and the offending
    /// source line with a caret when the caller knows it.
    fn render_error(&mut self, line: usize, col: usize, msg: &str, snippet: Option<&str>) {
        if !self.emit {
            return;
        }
        let t = self.theme;
        self.y += self.line_h * 0.5;
        let top = self.y;
        let title = format!("! {line}:{col}: {msg}");
        let title_chars: Vec<char> = title.chars().collect();
        let avail = (self.col_w - self.adv * 2.0).max(self.adv * 8.0);
        let rows = wrap_ranges(self.fonts, FontStyle::Mono, self.size, &title, avail);
        let mut body_h = rows.len() as f32 * self.line_h;
        if snippet.is_some() {
            body_h += 2.0 * self.line_h;
        }
        let banner_h = body_h + self.line_h * 0.75;
        self.ops.push(DrawOp::RoundedRect {
            rect: Rect::new(self.x0, top, self.col_w, banner_h),
            radius: 6.0,
            color: t.code_bg,
        });
        self.ops.push(DrawOp::Rect {
            rect: Rect::new(self.x0, top, 3.0, banner_h),
            color: t.error,
        });
        self.y = top + self.line_h * 0.375;
        for (s, e) in &rows {
            let piece: String = title_chars[*s..*e].iter().collect();
            self.text_op(self.x0 + self.adv, self.baseline(self.y), piece, t.error);
            self.y += self.line_h;
        }
        if let Some(src) = snippet {
            let src_rows = wrap_ranges(self.fonts, FontStyle::Mono, self.size, src, avail);
            // First visual row only; long lines are clipped, not wrapped, here.
            if let Some((s, e)) = src_rows.first() {
                let piece: String = src.chars().skip(*s).take(e - s).collect();
                let baseline = self.baseline(self.y);
                self.text_op(self.x0 + self.adv, baseline, piece, t.muted);
                let prefix: String = src.chars().take(col.saturating_sub(1)).collect();
                let caret_x =
                    self.x0 + self.adv + self.fonts.measure(FontStyle::Mono, &prefix, self.size);
                self.text_op(caret_x, baseline + self.line_h, "^".to_string(), t.error);
            }
            self.y += 2.0 * self.line_h;
        }
        self.y = top + banner_h + self.line_h * 0.5;
    }
}

fn tree<'a>(width: f32, ctx: &'a Ctx, collapsed: &'a HashSet<usize>) -> Tree<'a> {
    let t = ctx.theme;
    let size = ctx.body_size() * 0.9;
    let m = ctx.fonts.metrics(FontStyle::Mono, size);
    let adv = ctx.fonts.advance(FontStyle::Mono, '0', size).max(1.0);
    let pad = ctx.px(32.0);
    // The reading column reflows with the window (see `layout::column`).
    let (col_w, x0) = super::column(width, ctx);
    Tree {
        fonts: ctx.fonts,
        theme: t,
        size,
        line_h: (size * 1.55).round(),
        ascent: m.ascent,
        height: m.height(),
        adv,
        indent_w: adv * 2.0,
        x0,
        col_w,
        pad,
        collapsed,
        next_id: 0,
        emit: true,
        ops: Vec::new(),
        hits: Vec::new(),
        y: pad,
    }
}

fn finish(tr: Tree<'_>) -> DisplayList {
    DisplayList::new(tr.ops, tr.y + tr.pad, tr.hits)
}

// ---------------------------------------------------------------------------
// Virtualized layout for very large JSONL files.
//
// A display list for every record costs millions of ops and a multi-second
// build; almost none of it is on screen. The approach:
//
//   plan   — one cheap pass computes each record's row height *without* any
//            ops (pure shape counting over the Value, no font access) and
//            prefix-sums heights and node-id offsets. Parallel across records
//            because workers only read the Value tree.
//   band   — given a scroll range, lay out only the records overlapping it,
//            with each record's `next_id` seeded from the plan so toggle ids
//            match what a full layout would have assigned.
//
// The scroll range stays the virtual one (the plan's total height); the pane
// paints the band shifted to the position its records carry in that space.
// ---------------------------------------------------------------------------

/// A record's measured shape, in the geometry of one plan.
#[derive(Clone, Copy)]
struct Measured {
    /// Head row + body (or banner) height, excluding pads and separators.
    h: f32,
    /// How many toggle nodes the record's walk assigns.
    nodes: usize,
}

/// The shared tree scalars, derived once per (width, zoom) on the main
/// thread. Probes read these — never `FontSystem`, whose glyph caches are not
/// thread-safe; row counting is pure arithmetic over the mono advance.
#[derive(Clone, Copy)]
struct Geom {
    size: f32,
    line_h: f32,
    adv: f32,
    indent_w: f32,
    head_avail: f32,
    banner_avail: f32,
    pad: f32,
}

impl Geom {
    fn of(width: f32, ctx: &Ctx) -> Self {
        let size = ctx.body_size() * 0.9;
        let line_h = (size * 1.55).round();
        let adv = ctx.fonts.advance(FontStyle::Mono, '0', size).max(1.0);
        let indent_w = adv * 2.0;
        let (col_w, _x0) = super::column(width, ctx);
        Self {
            size,
            line_h,
            adv,
            indent_w,
            // emit_row's width at indent 0 (it subtracts the hanging indent).
            head_avail: col_w.max(adv * 8.0),
            // render_error's title bound: JSONL record bodies are indented one
            // level and the banner keeps two further cells of padding.
            banner_avail: (col_w - indent_w - adv * 2.0).max(adv * 8.0),
            pad: ctx.px(32.0),
        }
    }
}

/// Wrapped-row count for one row's real text, without touching the font
/// system.
///
/// Heights have to be identical to what [`wrap_ranges`] produces, or the
/// virtual list and the painted band disagree about where records start. The
/// greedy rule is the one that function uses: fill to the width, then break
/// after the last space in the window; hard-break a token wider than the row.
/// ASCII is one advance per character; anything else is charged two cells,
/// which over-measures a narrow fallback glyph rather than under-measuring a
/// wide one (under-measuring would clip the last row of a record).
fn rows_for_text(text: &str, avail: f32, adv: f32) -> usize {
    if text.is_empty() {
        return 1; // wrap_ranges returns one (empty) row
    }
    // ASCII rows are counted on bytes: the plan visits every row of every
    // record, and building a `Vec<char>` only to count advances was most of
    // its allocations. The arithmetic mirrors `text::wrap_ascii`.
    if text.is_ascii() && !text.bytes().any(|b| b.is_ascii_control()) {
        let per_row = ((avail / adv).max(1.0)) as usize;
        let bytes = text.as_bytes();
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
        return rows;
    }
    let chars: Vec<char> = text.chars().collect();
    let width_of = |c: char| {
        if c.is_ascii() && !c.is_control() {
            adv
        } else {
            adv * 2.0
        }
    };
    let mut rows = 0usize;
    let mut start = 0usize;
    while start < chars.len() {
        let (mut w, mut i, mut last_space) = (0.0f32, start, None);
        while i < chars.len() {
            let cw = width_of(chars[i]);
            if w + cw > avail && i > start {
                break;
            }
            w += cw;
            i += 1;
            if chars[i - 1] == ' ' {
                last_space = Some(i);
            }
        }
        if i >= chars.len() {
            rows += 1;
            break;
        }
        let cut = match last_space {
            Some(b) if b > start => b,
            _ => i,
        };
        rows += 1;
        start = cut;
    }
    rows
}

/// Lay out one parsed JSON document as a collapsible tree.
pub fn layout_value(v: &Value, collapsed: &HashSet<usize>, width: f32, ctx: &Ctx) -> DisplayList {
    let mut tr = tree(width, ctx, collapsed);
    tr.render_value(v, 0, Vec::new(), false);
    finish(tr)
}

/// Lay out a parse failure: banner with position, message, and source context.
pub fn layout_error(
    line: usize,
    col: usize,
    msg: &str,
    snippet: Option<&str>,
    width: f32,
    ctx: &Ctx,
) -> DisplayList {
    let collapsed = HashSet::new();
    let mut tr = tree(width, ctx, &collapsed);
    tr.render_error(line, col, msg, snippet);
    finish(tr)
}

/// Lay out JSON Lines: one headed tree (or error banner) per record with
/// rule separators. Node ids continue across records so `collapsed` is global.
pub fn layout_records(
    records: &[Record],
    buf: &Buffer,
    collapsed: &HashSet<usize>,
    width: f32,
    ctx: &Ctx,
) -> DisplayList {
    let mut tr = tree(width, ctx, collapsed);
    let t = tr.theme;
    for (ri, rec) in records.iter().enumerate() {
        if ri > 0 {
            tr.y += tr.line_h * 0.5;
            if tr.emit {
                tr.ops.push(DrawOp::Line {
                    x0: tr.x0,
                    y0: tr.y,
                    x1: tr.x0 + tr.col_w,
                    y1: tr.y,
                    color: t.rule,
                });
            }
            tr.y += tr.line_h * 0.5;
        }
        let head = vec![
            Seg::new(format!("#{} ", rec.line), t.muted),
            Seg::new(
                match &rec.value {
                    Ok(_) => "record",
                    Err(_) => "record — error",
                },
                if rec.value.is_ok() { t.fg } else { t.error },
            ),
        ];
        tr.emit_row(0, head, None);
        match &rec.value {
            Ok(v) => tr.render_value(v, 1, Vec::new(), false),
            Err(Error::Parse { line, col, msg }) => {
                // The error position is within the raw line; show it anchored
                // to the physical file line, not as "1:col" under record #N.
                let snippet = buf.line(
                    rec.line
                        .saturating_sub(1)
                        .min(buf.line_count().saturating_sub(1)),
                );
                // Indent the banner with the record body.
                let saved_x0 = tr.x0;
                tr.x0 += tr.indent_w;
                tr.col_w -= tr.indent_w;
                tr.render_error(rec.line + line.saturating_sub(1), *col, msg, Some(snippet));
                tr.x0 = saved_x0;
                tr.col_w += tr.indent_w;
            }
            Err(other) => {
                tr.render_error(1, 1, &other.to_string(), None);
            }
        }
    }
    if records.is_empty() {
        tr.emit_row(
            0,
            vec![Seg::new("empty file — no records", tr.theme.muted)],
            None,
        );
    }
    finish(tr)
}

/// Count one record's emitted rows and toggle nodes without building them.
///
/// This mirrors `layout_records` row for row, but advances counters instead
/// of pushing ops — so a measurement cannot drift from a painted layout
/// except by a real bug in one of the two, which the band test below pins.
/// Font access is not needed: every row is monospace, so the wrapped-row
/// count is `rows_for_chars` with the shared advance — the same arithmetic
/// the fast path inside `wrap_ranges` runs.
fn measure_record(g: &Geom, rec: &Record, collapsed: &HashSet<usize>, id_base: usize) -> Measured {
    let mut h = 0.0f32;
    let mut nodes = 0usize;

    // The walk shares a small frame so the recursion is one struct deref, not
    // five arguments per level; `next_in_record` mirrors the real walk's node
    // numbering so a collapsed subtree's summary row is what the measurement
    // counts — heights then agree with a painted band byte for byte while
    // the set is unchanged.
    struct Walk<'c> {
        g: &'c Geom,
        collapsed: &'c HashSet<usize>,
        id_base: usize,
        h: f32,
        nodes: usize,
    }

    impl Walk<'_> {
        /// Advance the walk over `v`; `head` is the text already on the row
        /// (`"0: "`, a key + `": "`), and `trailing_comma` is whether the
        /// painted row ends with one. Collapsed containers count a summary row
        /// instead of their children, exactly as `render_value` paints them.
        fn count_value(&mut self, v: &Value, indent: usize, head: &str, trailing_comma: bool) {
            let g = self.g;
            let width = indent_avail(g, indent);
            let comma = if trailing_comma { "," } else { "" };
            let add = |h: &mut f32, text: &str| {
                *h += g.line_h * rows_for_text(text, width, g.adv) as f32;
            };
            match v {
                Value::Object(pairs) => {
                    let len = pairs.len();
                    if len == 0 {
                        add(&mut self.h, &format!("{head}{{}}{comma}"));
                        return;
                    }
                    let id = self.id_base + self.nodes;
                    self.nodes += 1;
                    // ASCII stand-ins for the one-cell expander/summary glyphs,
                    // so the row-count fast path can stay on bytes.
                    add(&mut self.h, &format!("{head}v {{"));
                    if self.collapsed.contains(&id) {
                        let word = if len == 1 { "field" } else { "fields" };
                        add(&mut self.h, &format!("{head}> {{.}}{comma}  {len} {word}"));
                        return;
                    }
                    let last = len.saturating_sub(1);
                    for (i, (k, child)) in pairs.iter().enumerate() {
                        let hc = format!("{}: ", pretty::escape(k));
                        self.count_value(child, indent + 1, &hc, i != last);
                    }
                    add(&mut self.h, &format!("}}{comma}"));
                }
                Value::Array(items) => {
                    let len = items.len();
                    if len == 0 {
                        add(&mut self.h, &format!("{head}[]{comma}"));
                        return;
                    }
                    let id = self.id_base + self.nodes;
                    self.nodes += 1;
                    add(&mut self.h, &format!("{head}v ["));
                    if self.collapsed.contains(&id) {
                        let word = if len == 1 { "item" } else { "items" };
                        add(&mut self.h, &format!("{head}> [.]{comma}  {len} {word}"));
                        return;
                    }
                    let last = len.saturating_sub(1);
                    for (i, child) in items.iter().enumerate() {
                        let hc = format!("{i}: ");
                        self.count_value(child, indent + 1, &hc, i != last);
                    }
                    add(&mut self.h, &format!("]{comma}"));
                }
                scalar => {
                    let mut row = String::with_capacity(head.len() + 24);
                    row.push_str(head);
                    match scalar {
                        Value::Null => row.push_str("null"),
                        Value::Bool(true) => row.push_str("true"),
                        Value::Bool(false) => row.push_str("false"),
                        Value::Number(n) => row.push_str(n),
                        Value::String(s) => row.push_str(&pretty::escape(s)),
                        Value::Broken(msg) => {
                            row.push_str("! ");
                            row.push_str(msg);
                        }
                        _ => unreachable!(),
                    }
                    row.push_str(comma);
                    add(&mut self.h, &row);
                }
            }
        }
    }

    // Head row (`#N record`) at indent 0.
    let head_row = format!("#{} record", rec.line);
    h += g.line_h * rows_for_text(&head_row, indent_avail(g, 0), g.adv) as f32;
    match &rec.value {
        Ok(v) => {
            let mut walk = Walk {
                g,
                collapsed,
                id_base,
                h,
                nodes,
            };
            walk.count_value(v, 1, "", false);
            h = walk.h;
            nodes = walk.nodes;
        }
        Err(Error::Parse { line, col, msg }) => {
            // Same physical-line anchor as `render_error` (see layout_records).
            let shown = rec.line + line.saturating_sub(1);
            let title = format!("! {shown}:{col}: {msg}");
            let rows = rows_for_text(&title, g.banner_avail, g.adv);
            let body_h = rows as f32 * g.line_h + 2.0 * g.line_h;
            let banner_h = body_h + g.line_h * 0.75;
            // The record's head row is already counted above; the banner sits
            // under it with half a line of air on each side.
            h += g.line_h * 0.5 + banner_h + g.line_h * 0.5;
        }
        Err(other) => {
            let title = format!("1:1: {other}");
            let rows = rows_for_text(&title, g.banner_avail, g.adv);
            let body_h = rows as f32 * g.line_h;
            let banner_h = body_h + g.line_h * 0.75;
            h += g.line_h * 0.5 + banner_h + g.line_h * 0.5;
        }
    }
    Measured { h, nodes }
}

/// `emit_row`'s available width at an indent within the tree: the column,
/// less the row's indent and the one-step hanging indent a folded row uses.
fn indent_avail(g: &Geom, indent: usize) -> f32 {
    (g.head_avail - (indent + 1) as f32 * g.indent_w).max(g.adv * 8.0)
}

/// Where a record sits in the virtual list.
#[derive(Clone, Copy, Debug)]
pub struct RecordSlot {
    /// Record index into the records slice. Always the slot's own index.
    pub record: usize,
    /// y of the record block's top (its separator half-line for i>0) in the
    /// virtual list — the coordinate scroll offsets compare against.
    pub top: f32,
    /// Height of the record's own rows (no pads, no separators).
    pub h: f32,
    /// First node id the record's walk assigns.
    pub id_base: usize,
    /// How many toggle nodes the record's walk assigns. Kept so a patch can
    /// renumber the ids below an edit without re-measuring those records.
    pub nodes: usize,
}

/// The layout plan: prefix sums of record heights and id offsets, plus the
/// total virtual height. Rebuilt on width/zoom/collapsed-set changes; reused
/// by every paint in between.
///
/// An edit patches it in place ([`Plan::patch`]): records are one line each and
/// independent, so only the lines a span replaced are parsed and measured.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub slots: Vec<RecordSlot>,
    pub height: f32,
    /// Geometry the slots were measured in: the separator between records and
    /// the pads above and below the list. A patch prices new slots with them.
    line_h: f32,
    pad: f32,
}

impl Plan {
    /// Node id just past the last record: where a record appended at the end
    /// starts numbering.
    fn nodes(&self) -> usize {
        self.slots.last().map_or(0, |s| s.id_base + s.nodes)
    }

    /// Patch the records and the plan for the edits since it was built.
    ///
    /// A record is one physical line, so an edit can only change the records
    /// on the lines it replaced: those lines are parsed again, their blocks
    /// measured, and the index below them shifted by the height and node-count
    /// difference. Every other record keeps the measurement it already had,
    /// which is what makes a keystroke cost the edited record instead of the
    /// file. Returns whether the plan describes the buffer: `false` means the
    /// caller must rebuild, exactly as before.
    /// Patch the records and the plan for the edits since it was built.
    ///
    /// A record is one physical line, so an edit can only change the records
    /// on the lines it replaced: those lines are parsed again, their blocks
    /// measured, and the index below them shifted by the height and node-count
    /// difference. Every other record keeps the measurement it already had,
    /// which is what makes a keystroke cost the edited record instead of the
    /// file. Returns whether the plan describes the buffer — `false` means
    /// the caller must rebuild, exactly as before — plus the net change in
    /// errored records the patch made, which is all the status line's count
    /// needs to trail the text (T4 · the running count prices O(edited
    /// records) instead of a whole-list recount).
    pub fn patch(
        &mut self,
        records: &mut Vec<Record>,
        buf: &Buffer,
        collapsed: &HashSet<usize>,
        width: f32,
        ctx: &Ctx,
        edits: &[EditSpan],
    ) -> (bool, isize) {
        // A non-virtual plan has no slots to patch, and a plan whose slots and
        // records disagree is already inconsistent: both rebuild.
        if self.slots.is_empty() || self.slots.len() != records.len() {
            return (false, 0);
        }
        let g = Geom::of(width, ctx);
        let mut measured: Vec<Measured> = Vec::new();
        let mut error_delta: isize = 0;
        for e in edits {
            if e.start + e.inserted > buf.line_count() {
                return (false, 0);
            }
            // The records whose physical lines the span replaced. Blank lines
            // carry no record, so either side may be the same index.
            let lo = records.partition_point(|r| r.line - 1 < e.start);
            let hi = lo + records[lo..].partition_point(|r| r.line - 1 < e.start + e.removed);
            let fresh = jsonl::parse_lines(buf, e.start, e.start + e.inserted);
            let k = fresh.len();
            let mut id = match self.slots.get(lo) {
                Some(s) => s.id_base,
                None => self.nodes(),
            };
            measured.clear();
            for r in &fresh {
                let m = measure_record(&g, r, collapsed, id);
                id += m.nodes;
                measured.push(m);
            }
            // The status counter trails the text: this replace's error
            // arithmetic is exact — old records drop, fresh ones count.
            error_delta -= records[lo..hi].iter().filter(|r| r.value.is_err()).count() as isize;
            error_delta += fresh.iter().filter(|r| r.value.is_err()).count() as isize;
            if !self.splice(lo, hi - lo, &measured) {
                return (false, 0);
            }
            records.splice(lo..hi, fresh);
            // Everything below moved by the span's line delta; the fresh
            // records already carry their post-edit numbers.
            let delta = e.inserted as isize - e.removed as isize;
            if delta != 0 {
                for r in &mut records[lo + k..] {
                    r.line = (r.line as isize + delta) as usize;
                }
            }
        }
        (true, error_delta)
    }

    /// Replace `removed` slots at `start` with measured records, shifting the
    /// tops and node ids below.
    ///
    /// The layout rule is [`build_plan`]'s: a slot starts where the one above
    /// ended, plus one separator line for every record after the first. So a
    /// patched index is the index a fresh build of the edited records gives —
    /// the test pins that, tops and ids included.
    fn splice(&mut self, start: usize, removed: usize, measured: &[Measured]) -> bool {
        if start > self.slots.len()
            || start + removed > self.slots.len()
            || self.slots.is_empty()
            // Removing the last record leaves no virtual list to patch: the
            // caller rebuilds, and an empty document is cheap.
            || (removed == self.slots.len() && measured.is_empty())
        {
            return false;
        }
        let last = self.slots.last().expect("non-empty");
        let base = match self.slots.get(start) {
            Some(s) => s.top,
            None => last.top + last.h + self.line_h,
        };
        let mut id = match self.slots.get(start) {
            Some(s) => s.id_base,
            None => self.nodes(),
        };
        let mut tops = Vec::with_capacity(measured.len());
        let mut y = base;
        for (j, m) in measured.iter().enumerate() {
            if j > 0 {
                y += self.line_h;
            }
            tops.push((y, id));
            y += m.h;
            id += m.nodes;
        }
        if !measured.is_empty() {
            // `y` is where the record *after* the run starts: one separator
            // below the run's own last row, which is the stride every record
            // past the first is placed by.
            y += self.line_h;
        }
        // Where the replaced run ended in the old index: the next slot's top,
        // or — when it ran to the end — one separator past the last record,
        // which is the stride a following record would start at.
        let end = match self.slots.get(start + removed) {
            Some(s) => s.top,
            None => self.height - self.pad + self.line_h,
        };
        let dy = y - end;
        let fresh: Vec<RecordSlot> = measured
            .iter()
            .zip(tops)
            .enumerate()
            .map(|(j, (m, (top, id_base)))| RecordSlot {
                record: start + j,
                top,
                h: m.h,
                id_base,
                nodes: m.nodes,
            })
            .collect();
        self.slots.splice(start..start + removed, fresh);
        if dy != 0.0 {
            for s in &mut self.slots[start + measured.len()..] {
                s.top += dy;
            }
            self.height += dy;
        }
        // `record` is the slot's index and `id_base` the running node count:
        // one pass from the splice keeps both true below it.
        let mut nodes = id;
        for (i, s) in self
            .slots
            .iter_mut()
            .enumerate()
            .skip(start + measured.len())
        {
            s.record = i;
            s.id_base = nodes;
            nodes += s.nodes;
        }
        true
    }
}

/// Build the plan for the whole records slice.
///
/// Probes are pure shape counting: workers read only the parsed `Value`
/// trees, never `FontSystem` (its glyph caches are interior-mutable and not
/// shareable). Split across threads once the record list is large enough to
/// pay for it, one contiguous chunk per worker.
pub fn build_plan(
    records: &[Record],
    collapsed: &HashSet<usize>,
    width: f32,
    ctx: &Ctx,
    virtualize: bool,
) -> Plan {
    let g = Geom::of(width, ctx);
    if !virtualize {
        // Small file: no virtualization — the caller paints the full layout;
        // the plan only reports the height the pane scrolls through.
        let list = layout_records(records, &Buffer::from_text(""), collapsed, width, ctx);
        return Plan {
            slots: Vec::new(),
            height: list.height,
            line_h: g.line_h,
            pad: g.pad,
        };
    }
    let n = records.len();
    // The probe is pure shape counting — no font access, no ops pushed — so it
    // is a few hundred nanoseconds per record and even half a million costs
    // well under a second sequentially. A parallel fan-out is possible later
    // (workers would read only Values) but the profile says the band layout,
    // not the probe, is what to keep cheap.
    let mut measured: Vec<Measured> = Vec::with_capacity(n);
    let mut id_base = 0usize;
    for r in records {
        measured.push(measure_record(&g, r, collapsed, id_base));
        id_base += measured.last().expect("just pushed").nodes;
    }
    let line_h = g.line_h;
    let mut slots = Vec::with_capacity(n);
    let mut top = g.pad;
    let mut id_base = 0usize;
    for (i, m) in measured.iter().enumerate() {
        if i > 0 {
            top += line_h; // half above + half below the rule row
        }
        slots.push(RecordSlot {
            record: i,
            top,
            h: m.h,
            id_base,
            nodes: m.nodes,
        });
        id_base += m.nodes;
        top += m.h;
    }
    Plan {
        slots,
        height: top + g.pad,
        line_h,
        pad: g.pad,
    }
}

/// What one band paint needs: the plan, its records and buffer, the layout
/// environment, and the scroll range to cover.
pub struct Band<'a> {
    pub plan: &'a Plan,
    pub records: &'a [Record],
    pub buf: &'a Buffer,
    pub collapsed: &'a HashSet<usize>,
    pub width: f32,
    pub ctx: &'a Ctx<'a>,
    /// Virtual-list y range to lay out.
    pub top: f32,
    pub bottom: f32,
}

/// Lay out the records whose virtual blocks overlap the band request's range,
/// wrapped with the standard pads. Node ids are seeded per record from the
/// plan so a band's toggles carry exactly the ids a full layout would have
/// given them.
pub fn layout_band(band: Band<'_>) -> DisplayList {
    let Band {
        plan,
        records,
        buf,
        collapsed,
        width,
        ctx,
        top,
        bottom,
    } = band;
    if plan.slots.is_empty() {
        return layout_records(records, buf, collapsed, width, ctx);
    }
    // Records whose block starts before the view but extends into it still
    // paint; take one extra record on each side so band-edge separators and
    // the tops of partially visible records are present.
    let first = plan
        .slots
        .partition_point(|s| s.top + s.h < top)
        .saturating_sub(1)
        .min(plan.slots.len().saturating_sub(1));
    let last = plan.slots.partition_point(|s| s.top < bottom);
    let first_slot = plan.slots[first];
    let mut tr = tree(width, ctx, collapsed);
    tr.y = first_slot.top;
    for (ri, slot) in plan.slots[first..last].iter().enumerate() {
        let rec = &records[slot.record];
        if ri > 0 {
            tr.y += tr.line_h * 0.5;
            tr.ops.push(DrawOp::Line {
                x0: tr.x0,
                y0: tr.y,
                x1: tr.x0 + tr.col_w,
                y1: tr.y,
                color: tr.theme.rule,
            });
            tr.y += tr.line_h * 0.5;
        }
        let head = vec![
            Seg::new(format!("#{} ", rec.line), tr.theme.muted),
            Seg::new(
                match &rec.value {
                    Ok(_) => "record",
                    Err(_) => "record — error",
                },
                if rec.value.is_ok() {
                    tr.theme.fg
                } else {
                    tr.theme.error
                },
            ),
        ];
        tr.emit_row(0, head, None);
        tr.next_id = slot.id_base;
        match &rec.value {
            Ok(v) => tr.render_value(v, 1, Vec::new(), false),
            Err(Error::Parse { line, col, msg }) => {
                let snippet = buf.line(
                    rec.line
                        .saturating_sub(1)
                        .min(buf.line_count().saturating_sub(1)),
                );
                let saved_x0 = tr.x0;
                tr.x0 += tr.indent_w;
                tr.col_w -= tr.indent_w;
                tr.render_error(rec.line + line.saturating_sub(1), *col, msg, Some(snippet));
                tr.x0 = saved_x0;
                tr.col_w += tr.indent_w;
            }
            Err(other) => tr.render_error(1, 1, &other.to_string(), None),
        }
    }
    DisplayList::new(tr.ops, plan.height, tr.hits)
}

// ---------------------------------------------------------------------------
// Virtualized layout for one very large JSON document.
//
// A single tree has no record boundaries to hang a plan on, so the plan is per
// node: every node's block — its own row, and for a container everything under
// it — gets a top, a height and the toggle id a full walk would give it.
// Painting seeks to the deepest node that can be on screen and then lays out
// only the blocks that overlap the viewport, each row placed at the y the plan
// decided rather than accumulated from the top of the document. A document
// with a million nodes therefore holds one fixed-size slot per node instead of
// a display list of millions of ops, and a frame costs what is visible.
// ---------------------------------------------------------------------------

/// Above this many nodes a single JSON document is laid out virtually.
///
/// A tree is measured in nodes, not lines: a minified document is one line.
/// Twenty thousand nodes is already tens of thousands of rows — far more than
/// a window shows and more ops than a frame should carry — while a document
/// below it lays out whole in a few milliseconds, which keeps the everyday
/// file on the single path everything else is pinned against.
pub const VIRTUAL_JSON_NODES: usize = 20_000;

/// Slot fields with no value: the root's parent, a scalar's toggle id.
const NONE: u32 = u32::MAX;

/// Where one node sits in a virtual tree, in the geometry of one plan.
///
/// The slots are in pre-order, which is also y order: a node's block is either
/// disjoint from or nested inside the blocks around it, and `top` never
/// decreases with the index. That is what lets a paint binary-search the slot
/// under the scroll offset and binary-search from one sibling to the next.
#[derive(Clone, Copy, Debug)]
pub struct NodeSlot {
    /// y of the node's first row: its opener, its scalar row, or the summary
    /// row a collapsed container shows in place of its children.
    pub top: f32,
    /// Height of the node's whole block, its closing row included. Zero for a
    /// node inside a collapsed subtree: it keeps its slot — and its id, so
    /// toggling never renumbers anything — but paints nothing.
    pub h: f32,
    /// Slot index of the node's parent; [`NONE`] for the root.
    pub parent: u32,
    /// Slot index just past this node's block — the next sibling, or whatever
    /// follows the enclosing container. Recorded rather than derived from
    /// `top + h`, because the slots inside a collapsed subtree share that y:
    /// only the walk knows where the next *visible* node is.
    pub next: u32,
    /// Position of this node within its parent (an array's displayed index).
    pub ord: u32,
    /// The toggle id a full layout gives this node; [`NONE`] for a scalar.
    pub id: u32,
    /// The id the walk was about to hand out when it reached this node: for a
    /// container this is its own `id`, and for a scalar it is the id the next
    /// container gets. A patch planning a replacement subtree starts numbering
    /// there, so ids keep meaning what they meant without a whole re-walk.
    pub first_id: u32,
}

/// One JSON document's layout plan: a slot per node, plus the total height the
/// pane scrolls through, and how many toggle ids it handed out. Rebuilt on
/// width/zoom/collapse changes; every paint in between reads it.
#[derive(Clone, Debug, Default)]
pub struct ValuePlan {
    pub slots: Vec<NodeSlot>,
    pub height: f32,
    /// Containers in the tree: the id the next one would get.
    pub ids: u32,
}

/// The one node [`ValuePlan::patch_node`] replaces: its slot, its new value,
/// and the three things the walk that built the plan was handed for it (its
/// key or index prefix, its depth, and whether a comma follows it) — see
/// [`head_for`].
pub struct NodePatch<'a> {
    pub slot: usize,
    pub value: &'a Value,
    pub head: &'a str,
    pub indent: usize,
    pub trailing_comma: bool,
}

impl ValuePlan {
    /// Whether this plan is worth virtualizing: a tree this deep has more rows
    /// than any window shows, so laying it all out to paint one screenful is
    /// the cost being avoided. A smaller tree lays out whole.
    pub fn is_virtual(&self) -> bool {
        self.slots.len() > VIRTUAL_JSON_NODES
    }

    /// Replace one node's block — its slot and everything under it — with a
    /// plan of that node as it now parses, and move everything below it.
    ///
    /// Only the node is measured. Its ancestors keep their slots and their
    /// subtree keeps its slots, so the work is one subtree's rows, and the
    /// index a patched plan holds is the index a full walk of the edited tree
    /// would give: positions, spans, node counts and toggle ids all included.
    /// Returns false when the node cannot be located, which the caller answers
    /// by rebuilding.
    pub fn patch_node(
        &mut self,
        node: NodePatch<'_>,
        collapsed: &HashSet<usize>,
        width: f32,
        ctx: &Ctx,
    ) -> bool {
        let NodePatch {
            slot: start,
            value: v,
            head,
            indent,
            trailing_comma,
        } = node;
        let Some(old) = self.slots.get(start).copied() else {
            return false;
        };
        // A node's block is the slots up to the one it recorded as the next
        // node outside it: a leaf is its own slot, a container is everything
        // under its opening row.
        let end = old.next as usize;
        if old.next == NONE || end <= start || end > self.slots.len() {
            return false;
        }
        let old_len = end - start;
        // A node inside a collapsed subtree paints nothing, so its block has no
        // height and the replacement must be walked the same way — quietly.
        let mut emit = true;
        let mut anc = old.parent;
        while anc != NONE {
            let s = self.slots[anc as usize];
            if s.id != NONE && collapsed.contains(&(s.id as usize)) {
                emit = false;
                break;
            }
            anc = s.parent;
        }
        let g = Geom::of(width, ctx);
        let mut p = Planner {
            y: old.top,
            fonts: ctx.fonts,
            g,
            collapsed,
            slots: Vec::new(),
            heads: Vec::new(),
            row: String::new(),
            tail: String::new(),
            next_id: old.first_id as usize,
            parent: old.parent,
            emit,
        };
        p.walk(v, indent, head, trailing_comma, old.ord);
        let mut fresh = p.slots;
        let new_len = fresh.len();
        let new_end_y = p.y;
        // Ids handed out by the replacement, against the ones the node's old
        // block had: every id below the block moves by the difference.
        let new_ids = p.next_id - old.first_id as usize;
        let ids_after_block = match self.slots.get(end) {
            Some(s) => s.first_id as usize,
            None => self.ids as usize,
        };
        let old_ids = ids_after_block - old.first_id as usize;
        let dy = new_end_y - (old.top + old.h);
        let delta_nodes = new_len as isize - old_len as isize;
        let delta_ids = new_ids as isize - old_ids as isize;
        // A fresh walk numbers its own slots and ids from zero, so its inner
        // references are shifted onto the plan's numbering here.
        for (i, s) in fresh.iter_mut().enumerate() {
            if i == 0 {
                s.parent = old.parent;
            } else if s.parent != NONE {
                s.parent += start as u32;
            }
            s.next += start as u32;
        }
        self.slots.splice(start..end, fresh);
        self.ids = (self.ids as isize + delta_ids).max(0) as u32;
        let shift_ref = |v: u32, by: isize| (v as isize + by) as u32;
        for s in &mut self.slots[start + new_len..] {
            s.top += dy;
            // A reference into or past the replaced block moves with it; one
            // to an ancestor above it does not, which is why this is a
            // comparison and not a blanket shift.
            if s.parent != NONE && s.parent as usize >= end {
                s.parent = shift_ref(s.parent, delta_nodes);
            }
            if s.next != NONE && s.next as usize >= end {
                s.next = shift_ref(s.next, delta_nodes);
            }
            s.first_id = shift_ref(s.first_id, delta_ids);
            if s.id != NONE {
                s.id = shift_ref(s.id, delta_ids);
            }
        }
        // The ancestors' blocks contain the replaced one, so their heights move
        // by the same difference; a block that ended exactly at the old node's
        // end now ends at the new one.
        let mut anc = old.parent;
        while anc != NONE {
            let s = &mut self.slots[anc as usize];
            s.h += dy;
            if s.next != NONE && s.next as usize >= end {
                s.next = shift_ref(s.next, delta_nodes);
            }
            anc = s.parent;
        }
        self.height += dy;
        true
    }
}

/// The prefix a node's rows carry: an object member's escaped key and `": "`,
/// or an array item's index and `": "`. Anything without a parent row to hang
/// off (the root) has none.
///
/// The planner, the painter and a patch that re-measures one node all have to
/// agree on this text, or the node they measure is not the node they paint.
pub fn head_for(parent: &Value, ord: usize) -> String {
    match parent {
        Value::Object(pairs) => match pairs.get(ord) {
            Some((k, _)) => format!("{}: ", pretty::escape(k)),
            None => String::new(),
        },
        Value::Array(_) => format!("{ord}: "),
        _ => String::new(),
    }
}

/// Cells a run of text takes in the tree's monospace grid, and whether the
/// whole run is on that grid.
///
/// The three marker glyphs the tree draws are one cell each (`emit_row` counts
/// them that way, and the wrapper is handed an ASCII stand-in of the same
/// width); everything else ASCII is one cell, and anything else is two — the
/// same upward-looking approximation the JSONL plan makes. `false` means the
/// row holds a glyph whose real advance only the font knows, and the caller
/// has to measure it rather than trust the cell count. Escapes are counted as
/// the source they render as (`"` is two cells, a control character six).
fn cells_of(text: &str) -> (usize, bool) {
    let mut cells = 0usize;
    let mut one_cell = true;
    for c in text.chars() {
        match c {
            '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => cells += 2,
            c if (c as u32) < 0x20 => cells += 6,
            '▼' | '▶' | '…' => cells += 1,
            c if c.is_ascii() => cells += 1,
            _ => {
                cells += 2;
                one_cell = false;
            }
        }
    }
    (cells, one_cell)
}

/// Builds a [`ValuePlan`] in one walk of the tree, counting rows and assigning
/// node ids exactly as a full layout emits them.
///
/// Almost nothing is measured: every row is monospace, so a row on the grid
/// that fits the column is one line by arithmetic — the same test `emit_row`
/// makes — and is never written out at all. Only a row that overflows is built
/// and wrapped, and only a row holding a glyph the grid cannot size reaches the
/// font (see [`Planner::row`]). On a tree of ordinary JSON that leaves the walk
/// as a count of cells and one slot per node.
struct Planner<'a> {
    fonts: &'a FontSystem,
    g: Geom,
    collapsed: &'a HashSet<usize>,
    slots: Vec<NodeSlot>,
    /// One reusable head buffer per indent level: a child's key prefix is built
    /// into the level's buffer and handed down, so a document with a million
    /// keys allocates one string per depth rather than one per node.
    heads: Vec<String>,
    /// Scratch for a row's text, and for a tail that has to be escaped or
    /// formatted before it can be counted.
    row: String,
    tail: String,
    y: f32,
    next_id: usize,
    parent: u32,
    /// False while walking a collapsed subtree: rows are not counted, but
    /// slots and ids still are, so a toggle never renumbers a later node.
    emit: bool,
}

/// A container's own rows — its opener, its empty form and its closer — and
/// whether it is an object with `len` fields or an array with `len` items.
struct Shape<'a> {
    is_obj: bool,
    len: usize,
    open: &'a str,
    empty: &'a str,
    close: &'a str,
}

impl Planner<'_> {
    /// Plan the node `v` — whose `head` already carries its key or index —
    /// into a fresh slot, then everything under it.
    fn walk(&mut self, v: &Value, indent: usize, head: &str, trailing_comma: bool, ord: u32) {
        let slot = self.slots.len();
        let top = self.y;
        self.slots.push(NodeSlot {
            top,
            h: 0.0,
            parent: self.parent,
            next: NONE,
            ord,
            id: NONE,
            first_id: self.next_id as u32,
        });
        let saved_parent = std::mem::replace(&mut self.parent, slot as u32);
        match v {
            Value::Object(pairs) => self.container(
                v,
                slot,
                indent,
                head,
                trailing_comma,
                Shape {
                    is_obj: true,
                    len: pairs.len(),
                    open: "v {",
                    empty: "{}",
                    close: "}",
                },
            ),
            Value::Array(items) => self.container(
                v,
                slot,
                indent,
                head,
                trailing_comma,
                Shape {
                    is_obj: false,
                    len: items.len(),
                    open: "v [",
                    empty: "[]",
                    close: "]",
                },
            ),
            Value::Null => self.row(indent, head, "null", trailing_comma),
            Value::Bool(true) => self.row(indent, head, "true", trailing_comma),
            Value::Bool(false) => self.row(indent, head, "false", trailing_comma),
            Value::Number(n) => self.row(indent, head, n, trailing_comma),
            Value::String(s) => {
                let mut tail = std::mem::take(&mut self.tail);
                tail.clear();
                pretty::escape_into(&mut tail, s);
                self.row(indent, head, &tail, trailing_comma);
                self.tail = tail;
            }
            // One row, exactly as the painter draws it: the message is the
            // whole of what this node has to say.
            Value::Broken(msg) => {
                let mut tail = std::mem::take(&mut self.tail);
                tail.clear();
                tail.push_str("! ");
                tail.push_str(msg);
                self.row(indent, head, &tail, trailing_comma);
                self.tail = tail;
            }
        }
        self.parent = saved_parent;
        self.slots[slot].h = self.y - top;
        // Everything under this node is planned, so the next slot is the next
        // node outside its block.
        self.slots[slot].next = self.slots.len() as u32;
    }

    /// Plan a container: an empty one is a single row, a collapsed one shows
    /// its summary row and walks its children silently, an open one is an
    /// opener row, its children, and a closing row.
    fn container(
        &mut self,
        v: &Value,
        slot: usize,
        indent: usize,
        head: &str,
        trailing_comma: bool,
        shape: Shape<'_>,
    ) {
        let Shape {
            is_obj,
            len,
            open,
            empty,
            close,
        } = shape;
        if len == 0 {
            self.row(indent, head, empty, trailing_comma);
            return;
        }
        // Ids are handed out in walk order, so they match a full layout's ids
        // — including inside a collapsed subtree, which is still walked.
        let id = self.next_id;
        self.next_id += 1;
        self.slots[slot].id = id as u32;
        if self.collapsed.contains(&id) {
            let what = match (is_obj, len) {
                (true, 1) => "field",
                (true, _) => "fields",
                (false, 1) => "item",
                (false, _) => "items",
            };
            let comma = if trailing_comma { "," } else { "" };
            // The summary's `{…}` is three cells, so the stand-in is too.
            let mark = if is_obj { "{.}" } else { "[.]" };
            let mut tail = std::mem::take(&mut self.tail);
            tail.clear();
            let _ = write!(tail, "> {mark}{comma}  {len} {what}");
            self.row(indent, head, &tail, false);
            self.tail = tail;
            let emit = std::mem::replace(&mut self.emit, false);
            self.children(v, indent);
            self.emit = emit;
            return;
        }
        self.row(indent, head, open, false);
        self.children(v, indent);
        self.row(indent, "", close, trailing_comma);
    }

    /// Plan a container's children, building each one's key prefix into the
    /// level's reused buffer.
    fn children(&mut self, v: &Value, indent: usize) {
        let level = indent + 1;
        while self.heads.len() <= level {
            self.heads.push(String::new());
        }
        let mut buf = std::mem::take(&mut self.heads[level]);
        match v {
            Value::Object(pairs) => {
                let last = pairs.len().saturating_sub(1);
                for (i, (k, child)) in pairs.iter().enumerate() {
                    buf.clear();
                    pretty::escape_into(&mut buf, k);
                    buf.push_str(": ");
                    self.walk(child, level, &buf, i != last, i as u32);
                }
            }
            Value::Array(items) => {
                let last = items.len().saturating_sub(1);
                for (i, child) in items.iter().enumerate() {
                    buf.clear();
                    let _ = write!(buf, "{i}: ");
                    self.walk(child, level, &buf, i != last, i as u32);
                }
            }
            _ => {}
        }
        self.heads[level] = buf;
    }

    /// Count one logical row — its head, its tail and its comma — and advance
    /// the running y by its height.
    ///
    /// A row on the grid takes the same arithmetic `emit_row` uses to decide it
    /// fits, and only a row that overflows is written out and wrapped. A row
    /// holding a glyph the grid cannot size is always measured for real, through
    /// the same `wrap_ranges` the painter calls — an approximation there would
    /// put every row below it at the wrong y.
    fn row(&mut self, indent: usize, head: &str, tail: &str, trailing_comma: bool) {
        if !self.emit {
            return;
        }
        let g = self.g;
        let avail = indent_avail(&g, indent);
        let (head_cells, head_on_grid) = cells_of(head);
        let (tail_cells, tail_on_grid) = cells_of(tail);
        let cells = head_cells + tail_cells + usize::from(trailing_comma);
        let on_grid = head_on_grid && tail_on_grid;
        if on_grid && cells as f32 * g.adv <= avail {
            self.y += g.line_h;
            return;
        }
        self.row.clear();
        self.row.push_str(head);
        self.row.push_str(tail);
        if trailing_comma {
            self.row.push(',');
        }
        let rows = if on_grid {
            // One cell each and no font measurement: the marker glyphs come out
            // as the stand-ins `emit_row` wraps with, so the counting path's
            // byte fast path applies.
            if self.row.is_ascii() {
                rows_for_text(&self.row, avail, g.adv)
            } else {
                let stand_in: String = self
                    .row
                    .chars()
                    .map(|c| if c.is_ascii() { c } else { 'x' })
                    .collect();
                rows_for_text(&stand_in, avail, g.adv)
            }
        } else {
            wrap_ranges(self.fonts, FontStyle::Mono, g.size, &self.row, avail).len()
        };
        self.y += g.line_h * rows as f32;
    }
}

/// Build the plan for one parsed JSON document.
///
/// A single walk of the tree, no ops and no font access (see [`Planner`]), so
/// it costs a fraction of the display list it replaces — and, being per node
/// rather than per row, a fraction of a per-row plan would too.
///
/// The collapsed set only changes which blocks have content: every node keeps
/// its slot and its id either way, so a toggle cannot renumber a later node,
/// and a plan rebuilt after one lands where a full layout says it should.
pub fn build_value_plan(v: &Value, collapsed: &HashSet<usize>, width: f32, ctx: &Ctx) -> ValuePlan {
    let g = Geom::of(width, ctx);
    let mut p = Planner {
        y: g.pad,
        fonts: ctx.fonts,
        g,
        collapsed,
        slots: Vec::new(),
        heads: Vec::new(),
        row: String::new(),
        tail: String::new(),
        next_id: 0,
        parent: NONE,
        emit: true,
    };
    p.walk(v, 0, "", false, 0);
    let height = p.y + p.g.pad;
    ValuePlan {
        slots: p.slots,
        height,
        ids: p.next_id as u32,
    }
}

/// A parsed JSON document an edit can patch: the tree, every node's source
/// range, and the plan measured from them.
///
/// A JSONL is one record per line, so an edit is localized by *line*. A JSON
/// document is one value, so it is localized by *byte*: `spans[i]` is the text
/// of the node that plan slot `i` measures, in the same pre-order, which lets a
/// keystroke re-parse the one node it landed in and re-measure that node's rows
/// instead of the document's. The other nodes — the ones above, beside and below
/// it — keep their measurement, so the cost of a keystroke follows the size of
/// the value being typed into, not the size of the file.
pub struct Doc {
    pub value: Value,
    /// Byte ranges in the document, pre-order — one per node, parallel to
    /// `plan.slots`. A node's range runs from its first token to just past its
    /// last, so it holds no leading or trailing whitespace.
    pub spans: Vec<parser::Span>,
    pub plan: ValuePlan,
    /// The first node the tree cannot describe, or `None` while every node is a
    /// value the parser produced. A patch sets it when an edit leaves text that
    /// does not parse; the pane keeps the tree either way, and the status line
    /// is what turns this into words.
    pub broken: Option<String>,
}

impl Doc {
    /// Parse a document and plan it at `width`.
    pub fn parse(
        src: &str,
        collapsed: &HashSet<usize>,
        width: f32,
        ctx: &Ctx,
    ) -> Result<Doc, Error> {
        let (value, spans) = parser::parse_spanned(src)?;
        Ok(Self::from_parts(value, spans, collapsed, width, ctx))
    }

    /// Plan a tree that was parsed elsewhere (the worker parses its own copy
    /// of the text) together with the ranges it came with.
    pub fn from_parts(
        value: Value,
        spans: Vec<parser::Span>,
        collapsed: &HashSet<usize>,
        width: f32,
        ctx: &Ctx,
    ) -> Doc {
        let plan = build_value_plan(&value, collapsed, width, ctx);
        Doc {
            value,
            spans,
            plan,
            broken: None,
        }
    }

    /// The plan slot of the first node this tree cannot describe, in pre-order.
    ///
    /// Slots are one per node in the same pre-order, so the count of nodes
    /// before a broken one is its slot. Only a press asks this — the bar's
    /// per-frame check is `broken`, which is a field.
    pub fn first_broken_slot(&self) -> Option<usize> {
        fn walk(v: &Value, at: &mut usize) -> Option<usize> {
            let me = *at;
            *at += 1;
            match v {
                Value::Broken(_) => Some(me),
                Value::Array(items) => items.iter().find_map(|c| walk(c, at)),
                Value::Object(pairs) => pairs.iter().find_map(|(_, c)| walk(c, at)),
                _ => None,
            }
        }
        let mut at = 0usize;
        walk(&self.value, &mut at)
    }

    /// Re-measure every row at a new width, zoom or collapse set from the tree
    /// already parsed: a resize must not re-parse a minified document, and a
    /// toggle must not either.
    pub fn replan(&mut self, collapsed: &HashSet<usize>, width: f32, ctx: &Ctx) {
        self.plan = build_value_plan(&self.value, collapsed, width, ctx);
    }

    /// Patch the tree, the ranges and the plan for the edits since they were
    /// built, in the order the buffer logged them.
    ///
    /// Returns whether the document now describes the buffer: `false` means the
    /// caller must re-parse, which is now only when an edit cannot be localized
    /// at all (trailing content past the root, a plan and ranges that disagree).
    /// Text that does not parse is not a reason to give up: the node it landed
    /// in becomes a [`Value::Broken`] leaf carrying the parser's complaint, and
    /// the rest of the tree stays exactly as it was. A document being typed into
    /// is invalid between keystrokes far more often than it is valid, and each
    /// of those states used to cost a whole re-parse.
    pub fn patch(
        &mut self,
        buf: &Buffer,
        edits: &[EditSpan],
        collapsed: &HashSet<usize>,
        width: f32,
        ctx: &Ctx,
    ) -> bool {
        if self.spans.is_empty() || self.spans.len() != self.plan.slots.len() {
            return false;
        }
        for e in edits {
            if !self.patch_one(buf, e, collapsed, width, ctx) {
                return false;
            }
        }
        if self.spans.len() != self.plan.slots.len() {
            return false;
        }
        // A node may have been repaired by the edit that replaced it, or a break
        // may have been left behind by one that did not touch it; the tree is
        // the only thing that knows, so read it back while it has an error to
        // report (never on the healthy path).
        if self.broken.is_some() {
            self.broken = first_broken(&self.value);
        }
        true
    }

    fn patch_one(
        &mut self,
        buf: &Buffer,
        e: &EditSpan,
        collapsed: &HashSet<usize>,
        width: f32,
        ctx: &Ctx,
    ) -> bool {
        let removed = e.removed_bytes();
        let inserted = e.inserted_bytes();
        let Some(slot) = self.deepest_at(buf, e.at, removed, inserted) else {
            return false;
        };
        let path = self.ord_path(slot);
        let next = self.plan.slots[slot].next;
        if next == NONE || next as usize <= slot {
            return false;
        }
        // Whitespace between two tokens renders nothing, so the tree and every
        // measurement are untouched and only the offsets below the edit move.
        // Inside a string those same bytes are content, so a string node takes
        // the re-parse below like any other.
        if removed == 0
            && inserted > 0
            && self
                .value
                .get_at(&path)
                .is_some_and(|v| !matches!(v, Value::String(_)))
            && buf
                .byte_slice(e.at, e.at + inserted)
                .chars()
                .all(char::is_whitespace)
        {
            self.shift_spans(0, 0, e.at, removed, e.delta);
            return true;
        }
        let old_block = next as usize - slot;
        let start = self.spans[slot].start;
        let end = self.spans[slot].end as isize + e.delta;
        if end < start as isize {
            return false;
        }
        // The site the node is walked at: the head its parent's row hangs on
        // it, how deep it sits, and whether a comma follows it.
        let (head, indent, comma) = match path.split_last() {
            None => (String::new(), 0, false),
            Some((&ord, rest)) => {
                let Some(parent) = self.value.get_at(rest) else {
                    return false;
                };
                (
                    head_for(parent, ord),
                    path.len(),
                    ord + 1 < parent.child_count(),
                )
            }
        };
        let text = buf.byte_slice(start, end as usize);
        // The node's own text does not parse (yet): it becomes one error row
        // covering that text, and every other node keeps its place, its ranges
        // and its measurement. The row is exactly as wide as the region the
        // parser could not describe, because that region is what the node was.
        let (value, rel) = match parser::parse_spanned(&text) {
            Ok(parsed) => parsed,
            Err(err) => {
                let msg = match err {
                    Error::Parse { msg, .. } => msg,
                    other => other.to_string(),
                };
                let broken = Value::Broken(msg.clone());
                if !self.plan.patch_node(
                    NodePatch {
                        slot,
                        value: &broken,
                        head: &head,
                        indent,
                        trailing_comma: comma,
                    },
                    collapsed,
                    width,
                    ctx,
                ) {
                    return false;
                }
                if self.plan.slots.len() != self.spans.len() - old_block + 1 {
                    return false;
                }
                let Some(node) = self.value.get_mut_at(&path) else {
                    return false;
                };
                *node = broken;
                self.spans.splice(
                    slot..slot + old_block,
                    [parser::Span {
                        start,
                        end: end as usize,
                    }],
                );
                self.shift_spans(slot, slot + 1, e.at, removed, e.delta);
                self.broken = Some(msg);
                return true;
            }
        };
        if !self.plan.patch_node(
            NodePatch {
                slot,
                value: &value,
                head: &head,
                indent,
                trailing_comma: comma,
            },
            collapsed,
            width,
            ctx,
        ) {
            return false;
        }
        // The re-parse's own ranges, moved onto the document's offsets. A
        // node's range is exactly the text it was cut from, so the root of the
        // replacement has to cover the whole slice.
        let mut fresh: Vec<parser::Span> = rel
            .iter()
            .map(|s| parser::Span {
                start: s.start + start,
                end: s.end + start,
            })
            .collect();
        if fresh
            .first()
            .is_none_or(|s| s.start != start || s.end != start + text.len())
        {
            return false;
        }
        let Some(node) = self.value.get_mut_at(&path) else {
            return false;
        };
        *node = value;
        // A slot per node, and a range per slot, is what ties the plan to the
        // spans; a replacement that breaks the count is not usable.
        let new_block = fresh.len();
        if self.plan.slots.len() != self.spans.len() - old_block + new_block {
            return false;
        }
        self.spans.splice(slot..slot + old_block, fresh.drain(..));
        self.shift_spans(slot, slot + new_block, e.at, removed, e.delta);
        true
    }

    /// The innermost node whose source range contains the edit.
    ///
    /// Ranges are pre-order and sorted by start, so the innermost node starting
    /// at or before the edit is one binary search away; when it ends before the
    /// edit, the nodes that can still contain it are exactly its ancestors, so
    /// the parent chain finds them without touching the rest of the tree.
    fn deepest_at(
        &self,
        buf: &Buffer,
        at: usize,
        removed: usize,
        inserted: usize,
    ) -> Option<usize> {
        let mut i = self
            .spans
            .partition_point(|s| s.start <= at)
            .checked_sub(1)?;
        if self.contains(i, at, removed) {
            return Some(i);
        }
        // A token typed at the end of a scalar extends it: `1` becomes `12`.
        // The node itself is the thing that changed, not the container around
        // it — this is what keeps typing a digit in a huge flat object cheap.
        if removed == 0 && self.extends_leaf(buf, i, at, inserted) {
            return Some(i);
        }
        loop {
            let parent = self.plan.slots[i].parent;
            if parent == NONE {
                return None;
            }
            i = parent as usize;
            if self.contains(i, at, removed) {
                return Some(i);
            }
        }
    }

    fn contains(&self, i: usize, at: usize, removed: usize) -> bool {
        let s = self.spans[i];
        s.start <= at && at + removed <= s.end && (removed > 0 || at < s.end)
    }

    /// Whether the inserted bytes continue the token of the leaf at `i`.
    fn extends_leaf(&self, buf: &Buffer, i: usize, at: usize, inserted: usize) -> bool {
        if inserted == 0 || self.spans[i].end != at {
            return false;
        }
        // Only a scalar is one token; a container's closing brace is not
        // extended by typing after it.
        if self.plan.slots[i].next as usize != i + 1 {
            return false;
        }
        buf.byte_slice(at, at + inserted)
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
    }

    /// The child ordinals from the root down to a slot: the path `get_at`
    /// follows to reach the node's value.
    fn ord_path(&self, slot: usize) -> Vec<usize> {
        let mut path = Vec::new();
        let mut i = slot;
        loop {
            let parent = self.plan.slots[i].parent;
            if parent == NONE {
                break;
            }
            path.push(self.plan.slots[i].ord as usize);
            i = parent as usize;
        }
        path.reverse();
        path
    }

    /// Move every range outside `[lo, hi)` by the edit: the nodes that start at
    /// or after it move whole, and the ancestors above it — which start before
    /// it and end past it — grow or shrink at the end.
    fn shift_spans(&mut self, lo: usize, hi: usize, at: usize, removed: usize, delta: isize) {
        if delta == 0 {
            return;
        }
        let shift = |v: usize| (v as isize + delta).max(0) as usize;
        for (i, s) in self.spans.iter_mut().enumerate() {
            if i >= lo && i < hi {
                continue;
            }
            if s.start >= at + removed {
                s.start = shift(s.start);
                s.end = shift(s.end);
            } else if s.end > at {
                s.end = shift(s.end);
            }
        }
    }
}

/// The first node in a tree that is not a value the parser produced.
///
/// Pre-order, so the message a document reports is the one nearest its top —
/// which is the one a reader scrolling from the start meets first.
fn first_broken(v: &Value) -> Option<String> {
    match v {
        Value::Broken(msg) => Some(msg.clone()),
        Value::Array(items) => items.iter().find_map(first_broken),
        Value::Object(pairs) => pairs.iter().find_map(|(_, c)| first_broken(c)),
        _ => None,
    }
}

/// What one band paint of a virtualized tree needs.
pub struct ValueBand<'a> {
    pub value: &'a Value,
    pub plan: &'a ValuePlan,
    pub collapsed: &'a HashSet<usize>,
    pub width: f32,
    pub ctx: &'a Ctx<'a>,
    /// Virtual-list y range to lay out.
    pub top: f32,
    pub bottom: f32,
}

/// Lay out the part of a virtualized tree a viewport can see.
///
/// The pane scrolls through the plan's total height; this produces ops only for
/// the blocks overlapping `[top, bottom]`, plus the openers and closing rows on
/// the path down to them, each row at the y the plan gave its node.
pub fn layout_value_band(band: ValueBand<'_>) -> DisplayList {
    let ValueBand {
        value,
        plan,
        collapsed,
        width,
        ctx,
        top,
        bottom,
    } = band;
    if plan.slots.is_empty() {
        // Unplanned (a small document): whole, as it always was.
        return layout_value(value, collapsed, width, ctx);
    }
    // The chain of ancestors from the root to the deepest node that can be on
    // screen. Each level of it knows which child to start at, which is how a
    // container with a million children above the viewport is skipped in one
    // step instead of a million.
    let mut at = plan
        .slots
        .partition_point(|s| s.top <= top)
        .saturating_sub(1);
    let mut path = vec![at];
    while plan.slots[at].parent != NONE {
        at = plan.slots[at].parent as usize;
        path.push(at);
    }
    path.reverse();
    let mut painter = Painter {
        tr: tree(width, ctx, collapsed),
        plan,
        band: (top, bottom),
    };
    painter.node(value, 0, 0, Vec::new(), false, &path[1..]);
    let tr = painter.tr;
    DisplayList::new(tr.ops, plan.height, tr.hits)
}

/// The band painter: the display list being built, the plan it reads every row
/// position from, and the range it is filling.
struct Painter<'a, 'b> {
    tr: Tree<'b>,
    plan: &'a ValuePlan,
    band: (f32, f32),
}

impl Painter<'_, '_> {
    /// Lay out the node at `slot` — `head` carries its key or index, `path` the
    /// remaining chain of descendants that can be on screen — then as many of
    /// its siblings' blocks as the band reaches.
    fn node(
        &mut self,
        v: &Value,
        slot: usize,
        indent: usize,
        head: Vec<Seg>,
        trailing_comma: bool,
        path: &[usize],
    ) {
        let s = self.plan.slots[slot];
        // A block entirely above or below the band paints nothing, and neither
        // can anything under it: blocks nest, so a descendant is inside this
        // one's y range.
        if s.top > self.band.1 || s.top + s.h < self.band.0 {
            return;
        }
        let t = self.tr.theme;
        // The plan owns this node's y. Nothing is accumulated down from the top
        // of the document, which is the whole point: a row's position is known
        // without laying out anything above it.
        self.tr.y = s.top;
        let mut head = head;
        let comma = |segs: &mut Vec<Seg>| {
            if trailing_comma {
                segs.push(Seg::new(",", t.muted));
            }
        };
        let (is_obj, len) = match v {
            Value::Object(pairs) => (true, pairs.len()),
            Value::Array(items) => (false, items.len()),
            scalar => {
                head.extend(self.tr.scalar_segs(scalar));
                comma(&mut head);
                self.tr.emit_row(indent, head, None);
                return;
            }
        };
        let (open, close) = if is_obj { ("{", "}") } else { ("[", "]") };
        if len == 0 {
            head.push(Seg::new(format!("{open}{close}"), t.fg));
            comma(&mut head);
            self.tr.emit_row(indent, head, None);
            return;
        }
        let id = s.id as usize;
        // Char offset of the expander within the row: the hit area spans from
        // the row start through the marker, exactly as a full layout's does.
        let off: usize = head.iter().map(|x| x.text.chars().count()).sum();
        if self.tr.collapsed.contains(&id) {
            head.push(Seg::new("▶", t.accent));
            head.push(Seg::new(" ", t.fg));
            head.push(Seg::new(format!("{open}…{close}"), t.fg));
            comma(&mut head);
            let what = match (is_obj, len) {
                (true, 1) => "field",
                (true, _) => "fields",
                (false, 1) => "item",
                (false, _) => "items",
            };
            head.push(Seg::new(format!("  {len} {what}"), t.muted));
            self.tr.emit_row(indent, head, Some((id, off)));
            // The children keep their slots (and their ids) but paint nothing.
            return;
        }
        head.push(Seg::new("▼", t.accent));
        head.push(Seg::new(" ", t.fg));
        head.push(Seg::new(open, t.fg));
        self.tr.emit_row(indent, head, Some((id, off)));
        // Start at the child the path points at, so the children above the band
        // are skipped in one step however many of them there are; everything
        // from there down is walked in order.
        let start = path.first().map_or(0, |&c| self.plan.slots[c].ord as usize);
        let rest = path.get(1..).unwrap_or(&[]);
        let mut child_slot = path.first().copied().unwrap_or(slot + 1);
        match v {
            Value::Object(pairs) => {
                let last = pairs.len().saturating_sub(1);
                for (o, (k, child)) in pairs.iter().enumerate().skip(start) {
                    if self.past_band(child_slot) {
                        break;
                    }
                    let head = vec![
                        Seg::new(pretty::escape(k), t.json_key),
                        Seg::new(": ", t.fg),
                    ];
                    let path = if o == start { rest } else { &[] };
                    self.node(child, child_slot, indent + 1, head, o != last, path);
                    child_slot = self.next_slot(child_slot);
                }
            }
            Value::Array(items) => {
                let last = items.len().saturating_sub(1);
                for (o, child) in items.iter().enumerate().skip(start) {
                    if self.past_band(child_slot) {
                        break;
                    }
                    let head = vec![Seg::new(o.to_string(), t.muted), Seg::new(": ", t.fg)];
                    let path = if o == start { rest } else { &[] };
                    self.node(child, child_slot, indent + 1, head, o != last, path);
                    child_slot = self.next_slot(child_slot);
                }
            }
            _ => {}
        }
        // The closing row sits where the block ends. It is one row (`}` or `]`
        // and a comma at most) and `emit_row`'s floor keeps it from wrapping, so
        // its height is a line. A container that opens above the band still
        // paints its closer if the band reaches it, which the row's own y
        // decides.
        let mut close_segs = vec![Seg::new(close, t.fg)];
        comma(&mut close_segs);
        self.tr.y = s.top + s.h - self.tr.line_h;
        self.tr.emit_row(indent, close_segs, None);
    }

    /// Whether the child at `child_slot` starts below the band: blocks nest in
    /// index order, so that child and every sibling after it are off screen.
    fn past_band(&self, child_slot: usize) -> bool {
        match self.plan.slots.get(child_slot) {
            Some(s) => s.top > self.band.1,
            None => true,
        }
    }

    /// The slot of the sibling following the block at `child_slot`. The first
    /// child of a node is the next slot in pre-order; every later one is where
    /// the plan recorded that its predecessor's block ends.
    fn next_slot(&self, child_slot: usize) -> usize {
        (self.plan.slots[child_slot].next as usize).min(self.plan.slots.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core::buffer::Pos, gfx::font::FontSystem, layout::theme::Theme};

    fn ctx<'a>(f: &'a FontSystem, t: &'a Theme) -> Ctx<'a> {
        Ctx {
            fonts: f,
            theme: t,
            scale: 1.0,
            zoom: 1.0,
        }
    }

    #[test]
    fn tree_tints_and_toggles() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let v = crate::parse::json::parser::parse(r#"{"b": [1, true], "s": "x"}"#).unwrap();
        let open = layout_value(&v, &HashSet::new(), 800.0, &c);
        assert!(open.height > 100.0);
        assert!(!open.hits.is_empty(), "containers need toggles");
        // Toggle colors: key tint differs from punctuation.
        let colors: Vec<u32> = open
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { color, .. } => Some(color.pack()),
                _ => None,
            })
            .collect();
        assert!(colors.contains(&t.json_key.pack()));
        assert!(colors.contains(&t.json_number.pack()));
        // Collapsing the inner array shrinks the list; its summary row (and
        // every toggle) stays drawn, so hit count is unchanged and ids stable.
        let array_id = match &open.hits[1].action {
            HitAction::ToggleNode(id) => *id,
            HitAction::Link(href) => panic!("expected toggle, got link {href}"),
        };
        let mut collapsed = HashSet::new();
        collapsed.insert(array_id);
        let shut = layout_value(&v, &collapsed, 800.0, &c);
        assert!(shut.height < open.height);
        assert_eq!(shut.hits.len(), open.hits.len(), "ids must stay stable");
        // Collapsing the root hides the whole subtree including inner toggles.
        let root_id = match &open.hits[0].action {
            HitAction::ToggleNode(id) => *id,
            HitAction::Link(href) => panic!("expected toggle, got link {href}"),
        };
        let mut shut_root = HashSet::new();
        shut_root.insert(root_id);
        let shut = layout_value(&v, &shut_root, 800.0, &c);
        assert_eq!(shut.hits.len(), 1);
    }

    #[test]
    fn error_banner_shows_position() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout_error(2, 5, "expected a value", Some("  tru"), 800.0, &c);
        assert!(list.height > 0.0);
        assert!(
            list.ops
                .iter()
                .any(|op| matches!(op, DrawOp::RoundedRect { .. })),
            "banner panel missing"
        );
        assert!(
            list.ops
                .iter()
                .any(|op| matches!(op, DrawOp::Text { text, .. } if text.contains("2:5"))),
            "position missing"
        );
    }

    #[test]
    fn records_separate_and_mark_errors() {
        let f = FontSystem::load_embedded();
        let t = Theme::dark();
        let c = ctx(&f, &t);
        let src = "{\"a\": 1}\nbroken\n[2]";
        let buf = Buffer::from_text(src);
        let recs = crate::parse::jsonl::parse(src);
        let list = layout_records(&recs, &buf, &HashSet::new(), 800.0, &c);
        assert!(
            list.ops.iter().any(|op| matches!(op, DrawOp::Line { .. })),
            "record separator missing"
        );
        assert!(
            list.ops
                .iter()
                .any(|op| matches!(op, DrawOp::RoundedRect { .. })),
            "error banner missing"
        );
        let text: String = list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            text.contains("! 2:"),
            "JSONL error should be anchored to the physical record line: {text}"
        );
    }

    #[test]
    fn fields_and_items_are_comma_separated_like_json() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let v =
            crate::parse::json::parser::parse(r#"{"a": 1, "b": [true, null], "c": {}}"#).unwrap();
        let list = layout_value(&v, &HashSet::new(), 800.0, &c);
        let commas = list
            .ops
            .iter()
            .filter(|op| matches!(op, DrawOp::Text { text, .. } if text == ","))
            .count();
        // a's comma, b's comma, true's comma — but not after the last field,
        // the last item, or the empty object.
        assert_eq!(commas, 3, "commas: {commas}");
    }

    #[test]
    fn the_expander_says_whether_a_node_is_open() {
        let f = FontSystem::load_embedded();
        let t = Theme::dark();
        let c = ctx(&f, &t);
        let v = crate::parse::json::parser::parse(r#"{"a": {"b": 1}}"#).unwrap();
        let open = layout_value(&v, &HashSet::new(), 800.0, &c);
        let text_of = |list: &DisplayList| -> String {
            list.ops
                .iter()
                .filter_map(|op| match op {
                    DrawOp::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        };
        let shown = text_of(&open);
        assert!(shown.contains('▼'), "open expander missing: {shown}");
        assert!(!shown.contains('▶'), "collapsed glyph while open: {shown}");
        let root = match &open.hits[0].action {
            HitAction::ToggleNode(id) => *id,
            HitAction::Link(href) => panic!("expected a toggle, got link {href}"),
        };
        let mut collapsed = HashSet::new();
        collapsed.insert(root);
        let shut = layout_value(&v, &collapsed, 800.0, &c);
        let shown = text_of(&shut);
        assert!(shown.contains('▶'), "collapsed expander missing: {shown}");
        assert!(shown.contains('…'), "collapsed summary missing: {shown}");
        assert!(shown.contains("1 field"), "field count missing: {shown}");
    }

    #[test]
    fn wide_characters_do_not_shift_the_next_segment() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let v = crate::parse::json::parser::parse(r#"{"日本語": {"x": 1}, "next": 2}"#).unwrap();
        let list = layout_value(&v, &HashSet::new(), 800.0, &c);
        // Group every painted run by its baseline, then check left to right
        // that no run starts before the previous one ends. A segment placed by
        // `chars * mono_advance` instead of its measured width overlaps the
        // wide key on the very first row.
        let mut rows: Vec<(i32, Vec<(f32, f32)>)> = Vec::new();
        for op in &list.ops {
            if let DrawOp::Text {
                x,
                y,
                text,
                size,
                style,
                ..
            } = op
            {
                let right = x + f.measure(*style, text, *size);
                let key = y.round() as i32;
                match rows.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, spans)) => spans.push((*x, right)),
                    None => rows.push((key, vec![(*x, right)])),
                }
            }
        }
        for (baseline, mut spans) in rows {
            spans.sort_by(|a, b| a.0.total_cmp(&b.0));
            for pair in spans.windows(2) {
                assert!(
                    pair[1].0 + 0.5 >= pair[0].1,
                    "runs overlap on baseline {baseline}: {:?}",
                    pair
                );
            }
        }
        // The nested toggle's hit rect has to reach past the wide key to the
        // expander glyph, or clicking the glyph misses.
        let key = "\"日本語\": ";
        let prefix = f.measure(FontStyle::Mono, key, c.body_size() * 0.9);
        let nested = &list.hits[1];
        assert!(
            nested.rect.w >= prefix + 1.0,
            "toggle rect {} does not cover a {prefix} px key",
            nested.rect.w
        );
    }

    #[test]
    fn the_virtual_plan_matches_a_full_layout() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let src = "{\"a\": 1}\n\
                   {\"b\": [1, 2, {\"c\": \"a value long enough to wrap in a narrow column\"}]}\n\
                   broken\n\
                   {\"d\": {\"e\": {\"f\": true}}}\n";
        let buf = Buffer::from_text(src);
        let recs = crate::parse::jsonl::parse(src);
        let collapsed = HashSet::new();
        let full = layout_records(&recs, &buf, &collapsed, 800.0, &c);
        let plan = build_plan(&recs, &collapsed, 800.0, &c, true);
        assert!(
            (plan.height - full.height).abs() < 0.5,
            "plan height {} != full layout {}",
            plan.height,
            full.height
        );
        let band = layout_band(Band {
            plan: &plan,
            records: &recs,
            buf: &buf,
            collapsed: &collapsed,
            width: 800.0,
            ctx: &c,
            top: 0.0,
            bottom: f32::MAX,
        });
        assert!(
            (band.height - full.height).abs() < 0.5,
            "band height {} != full layout {}",
            band.height,
            full.height
        );
    }

    /// Order the slots of a patched plan against a rebuilt one: ids have to be
    /// exactly equal, and the heights and tops equal to within a rounding step
    /// (a patch reaches a top by shifting it, a rebuild by accumulating, and
    /// the two float orders can differ in the last bit).
    fn assert_same_index(what: &str, plan: &Plan, fresh: &Plan) {
        assert_eq!(plan.slots.len(), fresh.slots.len(), "{what}: slot count");
        for (a, b) in plan.slots.iter().zip(fresh.slots.iter()) {
            assert_eq!(
                (a.record, a.id_base, a.nodes),
                (b.record, b.id_base, b.nodes),
                "{what}: slot ids"
            );
            assert!(
                (a.top - b.top).abs() < 0.01,
                "{what}: top {} != {}",
                a.top,
                b.top
            );
            assert!(
                (a.h - b.h).abs() < 0.01,
                "{what}: height {} != {}",
                a.h,
                b.h
            );
        }
        assert!(
            (plan.height - fresh.height).abs() < 0.01,
            "{what}: index height {} != rebuild {}",
            plan.height,
            fresh.height
        );
    }

    /// A patched value plan has to equal a rebuild field for field: positions
    /// and heights (to a rounding step — a patch shifts a top, a rebuild
    /// accumulates one), and every reference, span and toggle id exactly.
    fn assert_same_tree(what: &str, plan: &ValuePlan, fresh: &ValuePlan) {
        assert_eq!(plan.slots.len(), fresh.slots.len(), "{what}: slot count");
        assert_eq!(plan.ids, fresh.ids, "{what}: container count");
        for (a, b) in plan.slots.iter().zip(fresh.slots.iter()) {
            assert_eq!(
                (a.parent, a.next, a.ord, a.id, a.first_id),
                (b.parent, b.next, b.ord, b.id, b.first_id),
                "{what}: slot refs"
            );
            assert!(
                (a.top - b.top).abs() < 0.01,
                "{what}: top {} != {}",
                a.top,
                b.top
            );
            assert!(
                (a.h - b.h).abs() < 0.01,
                "{what}: height {} != {}",
                a.h,
                b.h
            );
        }
        assert!(
            (plan.height - fresh.height).abs() < 0.01,
            "{what}: index height {} != rebuild {}",
            plan.height,
            fresh.height
        );
    }

    /// The slot a path of child ordinals lands on, and the site a walk is
    /// handed for it (head, indent, whether a comma follows) — derived from the
    /// plan's own links and the document, the way an edit derives them.
    fn site(value: &Value, plan: &ValuePlan, path: &[usize]) -> (usize, String, usize, bool) {
        let mut slot = 0usize;
        for &ord in path {
            let mut child = slot + 1;
            for _ in 0..ord {
                child = plan.slots[child].next as usize;
            }
            slot = child;
        }
        let Some((&ord, rest)) = path.split_last() else {
            return (slot, String::new(), 0, false);
        };
        let parent = value.get_at(rest).expect("the parent exists");
        (
            slot,
            head_for(parent, ord),
            path.len(),
            ord + 1 < parent.child_count(),
        )
    }

    /// Replacing one node's plan must land on the plan a full walk of the
    /// edited tree gives — for a leaf whose value grows a subtree, a container
    /// that empties, a wrapped string that shrinks, a node deep enough that
    /// every ancestor's block moves, and a node inside a collapsed subtree.
    #[test]
    fn patching_one_node_matches_a_rebuild() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let width = 800.0;
        let src = "{\"a\": [1, 2, {\"b\": [\"deep\", {\"c\": true}]}], \"d\": \"x\", \
                   \"e\": [], \"f\": \"a value long enough to wrap in a narrow column for sure\", \
                   \"g\": {\"h\": [1, 2, 3]}}";

        // Each case: the node to replace, what to replace it with, and whether
        // the collapsed set hides the node being patched.
        let cases: [(&[usize], &str, bool); 8] = [
            (&[0, 0], "{\"n\": [true, false]}", false),
            (
                &[0, 2, 0, 1],
                "{\"c\": true, \"d\": [0, 1, 2, 3], \"e\": {\"f\": {\"g\": \"x\"}}}",
                false,
            ),
            (&[3], "\"short\"", false),
            (&[2], "[1, 2, 3]", false),
            (&[4, 0], "[]", false),
            (&[1], "[{\"deep\": [1, 2]}]", false),
            (
                &[0, 2, 0, 0],
                "\"a much longer string than the one that was here, wide enough to wrap\"",
                true,
            ),
            (&[0, 2, 0], "[1]", true),
        ];
        for (path, replacement, hide) in cases {
            let collapsed: HashSet<usize> = if hide {
                // The `b` array, whose members the patch is inside.
                HashSet::from([3])
            } else {
                HashSet::new()
            };
            let what = format!("path {path:?} -> {replacement}");
            let (mut value, _) = crate::parse::json::parser::parse_spanned(src).unwrap();
            let mut plan = build_value_plan(&value, &collapsed, width, &c);
            let (slot, head, indent, comma) = site(&value, &plan, path);
            let new = crate::parse::json::parser::parse(replacement).unwrap();
            assert!(
                plan.patch_node(
                    NodePatch {
                        slot,
                        value: &new,
                        head: &head,
                        indent,
                        trailing_comma: comma,
                    },
                    &collapsed,
                    width,
                    &c,
                ),
                "{what}: the patch was refused"
            );
            *value.get_mut_at(path).expect("the node exists") = new;
            let fresh = build_value_plan(&value, &collapsed, width, &c);
            assert_same_tree(&what, &plan, &fresh);
            // And the pane a patched plan paints is the pane a full layout
            // paints: the band reads every position and id out of the plan.
            let band = layout_value_band(ValueBand {
                value: &value,
                plan: &plan,
                collapsed: &collapsed,
                width,
                ctx: &c,
                top: 0.0,
                bottom: f32::MAX,
            });
            let full = layout_value(&value, &collapsed, width, &c);
            assert_eq!(
                runs(&band),
                runs(&full),
                "{what}: the patched band is not the full layout"
            );
            assert!(
                (band.height - full.height).abs() < 0.5,
                "{what}: band height {} != full layout {}",
                band.height,
                full.height
            );
        }
    }

    /// A patch names a slot; a slot that is not a node in this plan is refused
    /// rather than patched onto something else.
    #[test]
    fn a_patch_outside_the_tree_is_refused() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let collapsed = HashSet::new();
        let (value, _) = crate::parse::json::parser::parse_spanned("[1, 2, 3]").unwrap();
        let mut plan = build_value_plan(&value, &collapsed, 800.0, &c);
        let one = crate::parse::json::parser::parse("9").unwrap();
        assert!(!plan.patch_node(
            NodePatch {
                slot: 99,
                value: &one,
                head: "",
                indent: 1,
                trailing_comma: false,
            },
            &collapsed,
            800.0,
            &c
        ));
        // The last slot's `next` is the slot past it, so its own block is one
        // slot: patching it is still fine.
        let last = plan.slots.len() - 1;
        assert!(plan.patch_node(
            NodePatch {
                slot: last,
                value: &one,
                head: "3: ",
                indent: 1,
                trailing_comma: false,
            },
            &collapsed,
            800.0,
            &c
        ));
        assert_eq!(plan.slots.len(), 4);
    }

    /// A document to edit: a few records the patch can localize inside, a
    /// container wide enough that re-walking it is visibly not the same work
    /// as re-walking one string, and a value long enough to wrap.
    const DOC: &str = "{\n  \"items\": [\n    {\"id\": 1, \"name\": \"one\"},\n    {\"id\": 2, \"name\": \"two\"},\n    {\"id\": 3, \"name\": \"three\"}\n  ],\n  \"ok\": true,\n  \"note\": \"a value long enough to wrap in a narrow column for sure\"\n}";

    /// An edit applied through [`Doc::patch`] must leave exactly the tree, the
    /// ranges and the plan a full parse-and-plan of the edited text gives.
    /// That is the whole point of re-parsing one node: it is not an
    /// approximation of the document, it is the document.
    #[test]
    fn patching_a_document_matches_a_rebuild() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let collapsed = HashSet::new();
        let width = 800.0;
        let mut buf = Buffer::from_text(DOC);
        let mut doc = Doc::parse(&buf.text(), &collapsed, width, &c).unwrap();

        let check = |what: &str, buf: &Buffer, doc: &Doc| {
            let text = buf.text();
            let (value, spans) = parser::parse_spanned(&text).unwrap();
            assert_eq!(doc.value, value, "{what}: tree");
            assert_eq!(doc.spans, spans, "{what}: source ranges");
            let fresh = build_value_plan(&doc.value, &collapsed, width, &c);
            assert_same_tree(what, &doc.plan, &fresh);
            // And the pane a patched plan paints is the pane a full layout
            // paints — the artifact on screen, not just the index behind it.
            let band = layout_value_band(ValueBand {
                value: &doc.value,
                plan: &doc.plan,
                collapsed: &collapsed,
                width,
                ctx: &c,
                top: 0.0,
                bottom: f32::MAX,
            });
            assert_eq!(
                runs(&band),
                runs(&layout_value(&doc.value, &collapsed, width, &c)),
                "{what}: the patched band is not the full layout"
            );
        };

        macro_rules! edit {
            ($what:expr, $body:expr) => {{
                $body;
                let edits = buf.take_edits().expect("the edit was logged");
                assert!(
                    doc.patch(&buf, &edits, &collapsed, width, &c),
                    "{}: the patch was refused",
                    $what
                );
                check($what, &buf, &doc);
            }};
        }

        fn line_with(buf: &Buffer, needle: &str) -> usize {
            (0..buf.line_count())
                .find(|&i| buf.line(i).contains(needle))
                .unwrap_or_else(|| panic!("no line holds {needle:?}"))
        }

        // A character inside one string: one node is re-parsed and re-measured,
        // and every other node keeps the measurement it had.
        let line = line_with(&buf, "one");
        let col = buf.line(line).find("one").unwrap() + 1;
        edit!(
            "a character inside a string",
            buf.insert(Pos { line, col }, "X")
        );
        // A digit typed at the end of a number extends it, rather than making
        // the container around it the thing that changed.
        let line = line_with(&buf, "\"id\": 1");
        let col = buf.line(line).find(" 1,").unwrap() + 2;
        edit!(
            "a digit extending a number",
            buf.insert(Pos { line, col }, "7")
        );
        // Whitespace between tokens renders nothing: the tree does not change,
        // only the offsets below it do.
        let line = line_with(&buf, "\"id\": 2");
        edit!(
            "a blank line inside the array",
            buf.insert(Pos { line, col: 0 }, "\n")
        );
        // An item appended to the array: the node grows a whole subtree, so
        // the slot block grows and every node below it moves.
        let line = line_with(&buf, "three");
        let col = buf.line(line).len();
        edit!("an item appended to the array", {
            buf.insert(
                Pos { line, col },
                ",\n    {\"id\": 4, \"deep\": {\"a\": [1, 2]}}",
            )
        });
        // A whole member removed: the document's structure changes at the
        // root, which is described exactly all the same.
        let line = line_with(&buf, "\"id\": 1");
        edit!(
            "a member's line deleted",
            buf.delete(
                Pos { line, col: 0 },
                Pos {
                    line: line + 1,
                    col: 0
                }
            )
        );
        // And a member appended to the root object, before its `}`.
        let line = line_with(&buf, "note");
        let col = buf.line(line).len();
        edit!("a member appended at the end", {
            buf.insert(Pos { line, col }, ",\n  \"late\": [true, false]")
        });
    }

    /// A key typed at the root stays valid, and the node the edit belongs to is
    /// the root — which the patch can still describe exactly.
    #[test]
    fn a_key_renamed_at_the_root_is_patched() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let collapsed = HashSet::new();
        let width = 800.0;
        let mut buf = Buffer::from_text(DOC);
        let mut doc = Doc::parse(&buf.text(), &collapsed, width, &c).unwrap();
        let col = buf.line(6).find("ok").unwrap();
        buf.insert(Pos { line: 6, col }, "o");
        let edits = buf.take_edits().expect("the edit was logged");
        assert!(doc.patch(&buf, &edits, &collapsed, width, &c));
        assert_eq!(doc.broken, None);
        let (value, spans) = parser::parse_spanned(&buf.text()).unwrap();
        assert_eq!(doc.value, value);
        assert_eq!(doc.spans, spans);
        assert_same_tree(
            "a key renamed",
            &doc.plan,
            &build_value_plan(&doc.value, &collapsed, width, &c),
        );
    }

    /// A keystroke that leaves the text unparseable does not cost the tree: the
    /// node the edit landed in becomes one error row and everything else keeps
    /// its place. The document is invalid between keystrokes far more often
    /// than not while it is being typed into, so a re-parse there is a re-parse
    /// per keystroke for as long as the thought takes.
    #[test]
    fn an_edit_that_breaks_the_document_keeps_the_tree() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let collapsed = HashSet::new();
        let width = 800.0;
        let mut buf = Buffer::from_text(DOC);
        let mut doc = Doc::parse(&buf.text(), &collapsed, width, &c).unwrap();
        let nodes = doc.plan.slots.len();

        // A comma typed before the array's `]`: a dangling comma. The array is
        // the smallest node whose text no longer parses, so the array is the
        // row that reports it — and the object around it keeps its other
        // members, their ranges and their measurements.
        let line = (0..buf.line_count())
            .find(|&i| buf.line(i).contains(']'))
            .unwrap();
        let col = buf.line(line).find(']').unwrap();
        buf.insert(Pos { line, col }, ",");
        let edits = buf.take_edits().expect("the edit was logged");
        assert!(doc.patch(&buf, &edits, &collapsed, width, &c));
        assert!(
            doc.broken.is_some(),
            "a document that does not parse has to say so"
        );
        assert!(
            matches!(doc.value.get_at(&[0]), Some(Value::Broken(_))),
            "the array should be the broken node: {:?}",
            doc.value
        );
        // Four nodes: the root, the broken array, `ok` and `note` — the items
        // it held are gone with the text that described them, and nothing else
        // was re-measured.
        assert_eq!(doc.plan.slots.len(), 4);
        assert_eq!(doc.spans.len(), 4);
        assert_same_tree(
            "a dangling comma",
            &doc.plan,
            &build_value_plan(&doc.value, &collapsed, width, &c),
        );
        // The pane says why, where the text is.
        let band = layout_value_band(ValueBand {
            value: &doc.value,
            plan: &doc.plan,
            collapsed: &collapsed,
            width,
            ctx: &c,
            top: 0.0,
            bottom: f32::MAX,
        });
        assert!(
            band.ops.iter().any(|op| matches!(
                op,
                DrawOp::Text { text, .. } if text.starts_with("! ")
            )),
            "the band did not paint the error row"
        );

        // Typing a digit at the end of a number that is half written (`1.`) is
        // the same story one node further in, and finishing it repairs the
        // node in place — the document is whole again, and equal to a parse of
        // its own text.
        let mut buf = Buffer::from_text(DOC);
        let mut doc = Doc::parse(&buf.text(), &collapsed, width, &c).unwrap();
        let line = (0..buf.line_count())
            .find(|&i| buf.line(i).contains("\"id\": 1"))
            .unwrap();
        let col = buf.line(line).find(" 1,").unwrap() + 2;
        buf.insert(Pos { line, col }, ".");
        let edits = buf.take_edits().expect("the edit was logged");
        assert!(doc.patch(&buf, &edits, &collapsed, width, &c));
        assert!(doc.broken.is_some());
        assert!(
            matches!(doc.value.get_at(&[0, 0, 0]), Some(Value::Broken(_))),
            "the half-written number should be the broken node: {:?}",
            doc.value.get_at(&[0, 0, 0])
        );
        // A leaf replaced a leaf: the rest of the document is untouched.
        assert_eq!(doc.plan.slots.len(), nodes);
        buf.insert(Pos { line, col: col + 1 }, "5");
        let edits = buf.take_edits().expect("the edit was logged");
        assert!(doc.patch(&buf, &edits, &collapsed, width, &c));
        assert_eq!(doc.broken, None, "the repaired number is a value again");
        assert_eq!(
            doc.value.get_at(&[0, 0, 0]),
            Some(&Value::Number("1.5".into()))
        );
        let (value, spans) = parser::parse_spanned(&buf.text()).unwrap();
        assert_eq!(doc.value, value);
        assert_eq!(doc.spans, spans);
        assert_same_tree(
            "a repaired number",
            &doc.plan,
            &build_value_plan(&doc.value, &collapsed, width, &c),
        );

        // Two breaks in two places: fixing one leaves the other reported, and
        // the tree is the one a parse of the once-broken document gives.
        let mut buf = Buffer::from_text(DOC);
        let mut doc = Doc::parse(&buf.text(), &collapsed, width, &c).unwrap();
        let line = (0..buf.line_count())
            .find(|&i| buf.line(i).contains("true"))
            .unwrap();
        let col = buf.line(line).find("true").unwrap() + 2;
        buf.insert(Pos { line, col }, "x");
        let edits = buf.take_edits().expect("the edit was logged");
        assert!(doc.patch(&buf, &edits, &collapsed, width, &c));
        let first = doc.broken.clone();
        let line = (0..buf.line_count())
            .find(|&i| buf.line(i).contains(']'))
            .unwrap();
        let col = buf.line(line).find(']').unwrap();
        buf.insert(Pos { line, col }, ",");
        let edits = buf.take_edits().expect("the edit was logged");
        assert!(doc.patch(&buf, &edits, &collapsed, width, &c));
        assert!(doc.broken.is_some());
        assert_ne!(doc.broken, first, "the first break is the one reported");
    }

    /// An edit outside every node — trailing content past the root — has no
    /// node to hang on, so the patch says so instead of guessing.
    #[test]
    fn an_edit_outside_the_document_is_refused() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let collapsed = HashSet::new();
        let width = 800.0;
        let mut buf = Buffer::from_text(DOC);
        let mut doc = Doc::parse(&buf.text(), &collapsed, width, &c).unwrap();
        let at = buf.end();
        buf.insert(at, " x");
        let edits = buf.take_edits().expect("the edit was logged");
        assert!(!doc.patch(&buf, &edits, &collapsed, width, &c));
        assert_eq!(doc.broken, None, "a refusal describes nothing");
    }

    /// A source with the shapes a record's measured height depends on: nested
    /// containers, a value long enough to wrap, blank lines, a broken line,
    /// wide glyphs, and a record carrying an error banner.
    fn patch_source(n: usize) -> String {
        let mut s = String::new();
        for i in 0..n {
            match i % 8 {
                0 => s.push_str(&format!("{{\"id\": {i}, \"tags\": [\"a\", \"b\"]}}\n")),
                1 => s.push_str(&format!(
                    "{{\"id\": {i}, \"note\": \"{}\"}}\n",
                    "x".repeat(240)
                )),
                2 => s.push('\n'),
                3 => s.push_str(&format!(
                    "{{\"id\": {i}, \"deep\": {{\"a\": {{\"b\": [1, 2, 3]}}}}}}\n"
                )),
                4 => s.push_str(&format!("[{{\"id\": {i}}}, 2, null]\n")),
                5 => s.push_str(&format!("{{\"id\": {i}}} trailing junk\n")),
                6 => s.push_str(&format!(
                    "{{\"id\": {i}, \"s\": \"\u{65e5}\u{672c}\u{8a9e}\"}}\n"
                )),
                _ => s.push_str(&format!("{{\"id\": {i}, \"ok\": {}}}\n", i % 2 == 0)),
            }
        }
        s
    }

    /// An edit applied through [`Plan::patch`] must leave exactly the records
    /// and the index a full rebuild of the edited text gives — the whole point
    /// of parsing and measuring only the touched line is that it is not an
    /// approximation of the whole-file result.
    #[test]
    fn patching_records_matches_a_rebuild() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let collapsed = HashSet::new();
        let width = 800.0;
        let src = patch_source(600);
        let mut buf = Buffer::from_text(&src);
        let mut records = jsonl::parse(&src);
        let mut plan = build_plan(&records, &collapsed, width, &c, true);
        assert!(
            plan.slots.len() > 300,
            "the fixture should be mostly records"
        );

        let check = |what: &str, buf: &Buffer, records: &[Record], plan: &Plan| {
            // The records are what a whole-file parse of the edited text has:
            // the same lines, the same ok/error shape.
            let whole = jsonl::parse(&buf.text());
            let numbers = |rs: &[Record]| rs.iter().map(|r| r.line).collect::<Vec<_>>();
            let oks = |rs: &[Record]| rs.iter().map(|r| r.value.is_ok()).collect::<Vec<_>>();
            assert_eq!(numbers(records), numbers(&whole), "{what}: record lines");
            assert_eq!(oks(records), oks(&whole), "{what}: record errors");
            // And the index is the one those records get built whole.
            let fresh = build_plan(records, &collapsed, width, &c, true);
            assert_same_index(what, plan, &fresh);
        };

        macro_rules! edit {
            ($what:expr, $body:expr) => {{
                $body;
                let edits = buf.take_edits().expect("the edit was logged");
                assert!(
                    Plan::patch(&mut plan, &mut records, &buf, &collapsed, width, &c, &edits).0,
                    "{}: the patch was refused",
                    $what
                );
                check($what, &buf, &records, &plan);
            }};
        }

        // One character into a record: its height changes, everything below
        // shifts.
        edit!(
            "a character inside a record",
            buf.insert(Pos { line: 1, col: 12 }, "z")
        );
        // A newline inside a record: one record becomes two, one of them an
        // error, at a line count the index has to grow into.
        edit!(
            "a newline splitting a record",
            buf.insert(Pos { line: 9, col: 4 }, "\n")
        );
        // A blank line before a record: no record in the replaced line range,
        // but every record below moves one physical line.
        edit!(
            "a blank line inserted",
            buf.insert(Pos { line: 30, col: 0 }, "\n")
        );
        // A pasted run of records: the line it lands on keeps its tail, which
        // is no longer valid JSON — the error banner a patch has to measure.
        edit!(
            "a pasted run of records",
            buf.insert(Pos { line: 60, col: 0 }, "{\"p\": 1}\n{\"p\": [2, 3]}\n")
        );
        // A record deleted: the line goes, the records below it shift up.
        edit!(
            "a record deleted",
            buf.delete(Pos { line: 49, col: 0 }, Pos { line: 50, col: 0 })
        );
        // Two records joined into one, which is what deleting the newline
        // between them does — two records become one invalid one.
        edit!(
            "two records joined",
            buf.delete(Pos { line: 20, col: 0 }, Pos { line: 22, col: 0 })
        );
        // A record appended at the end: the index grows past its last slot.
        edit!("a record appended at the end", {
            let at = buf.end();
            buf.insert(
                at,
                "\n{\"id\": 9999, \"tags\": [\"z\"], \"note\": \"tail\"}",
            )
        });
        // And a whole run removed from the middle, with nothing parsed in its
        // place.
        edit!(
            "a run of records deleted",
            buf.delete(Pos { line: 100, col: 0 }, Pos { line: 130, col: 0 })
        );
    }

    /// A patch that cannot describe the buffer says so and leaves the caller to
    /// rebuild, rather than half-updating an index that then paints.
    #[test]
    fn a_patch_that_does_not_fit_is_refused() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let collapsed = HashSet::new();
        let src = patch_source(40);

        // A non-virtual plan (a small file's) has no slots to patch.
        let mut small = build_plan(&jsonl::parse(&src), &collapsed, 800.0, &c, false);
        let mut records = jsonl::parse(&src);
        let mut buf = Buffer::from_text(&src);
        buf.insert(Pos { line: 0, col: 0 }, "x");
        let edits = buf.take_edits().expect("logged");
        assert!(
            !Plan::patch(
                &mut small,
                &mut records,
                &buf,
                &collapsed,
                800.0,
                &c,
                &edits
            )
            .0
        );

        // Slots and records that no longer match each other are inconsistent
        // already: a patch would index records that are not there.
        let mut plan = build_plan(&jsonl::parse(&src), &collapsed, 800.0, &c, true);
        let mut short = jsonl::parse(&src);
        short.truncate(3);
        let mut buf = Buffer::from_text(&src);
        buf.insert(Pos { line: 0, col: 0 }, "x");
        let edits = buf.take_edits().expect("logged");
        assert!(!Plan::patch(&mut plan, &mut short, &buf, &collapsed, 800.0, &c, &edits).0);

        // An edit naming lines the buffer does not have cannot be replayed.
        let mut plan = build_plan(&jsonl::parse(&src), &collapsed, 800.0, &c, true);
        let mut records = jsonl::parse(&src);
        let far = [EditSpan {
            start: 10_000,
            removed: 1,
            inserted: 1,
            at: 1 << 40,
            delta: 0,
        }];
        assert!(!Plan::patch(&mut plan, &mut records, &buf, &collapsed, 800.0, &c, &far).0);

        // Removing every record leaves no virtual list to patch — and an empty
        // document is cheap to build, so that is what the caller does.
        let text = "\n{\"a\": 1}\n{\"b\": 2}\n\n";
        let mut buf = Buffer::from_text(text);
        let mut records = jsonl::parse(text);
        let mut plan = build_plan(&records, &collapsed, 800.0, &c, true);
        assert_eq!(plan.slots.len(), 2);
        buf.delete(Pos { line: 0, col: 0 }, Pos { line: 4, col: 0 });
        let edits = buf.take_edits().expect("logged");
        assert!(!Plan::patch(&mut plan, &mut records, &buf, &collapsed, 800.0, &c, &edits).0);
    }

    /// Every text run of a list, as (x, y, text): the pane's ink in a form two
    /// layouts of one document can be compared in. Op order is an
    /// implementation detail (a band emits a node's closing row where the block
    /// ends, not where the walk reaches it), so the comparison sorts.
    fn runs(list: &DisplayList) -> Vec<(i32, i32, String)> {
        let mut out: Vec<(i32, i32, String)> = list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { x, y, text, .. } => {
                    Some((x.round() as i32, y.round() as i32, text.clone()))
                }
                _ => None,
            })
            .collect();
        out.sort();
        out
    }

    /// A document for the virtual-tree tests: a wide array, so a band has to
    /// skip hundreds of siblings in one step; nesting; a value long enough to
    /// wrap; a run of wide glyphs that wraps too; an empty container; and depth
    /// enough that the path down to a band runs through a dozen levels.
    fn virtual_doc_rows(n: usize) -> Value {
        let mut s = String::from("{\"rows\": [\n");
        for i in 0..n {
            let note = if i == n / 2 {
                "z".repeat(240)
            } else if i == n / 2 + 1 {
                "日".repeat(120)
            } else {
                "n".into()
            };
            s.push_str(&format!(
                "  {{\"id\": {i}, \"name\": \"row-{i}\", \"tags\": [\"a\", \"b\"], \
                 \"meta\": {{\"ok\": {}, \"note\": \"{note}\"}}}}{}\n",
                i % 2 == 0,
                if i + 1 == n { "" } else { "," },
            ));
        }
        s.push_str("],\n\"wide\": {\"日本語\": [1, 2, {\"deeper\": \"☃☃☃\"}], \"empty\": {}},\n");
        s.push_str("\"deep\": ");
        for _ in 0..24 {
            s.push_str("{\"d\": ");
        }
        s.push_str("\"bottom\"");
        s.push_str(&"}".repeat(24));
        s.push_str("}\n");
        crate::parse::json::parser::parse(&s).expect("the test document parses")
    }

    fn virtual_doc() -> Value {
        virtual_doc_rows(300)
    }

    fn band(
        v: &Value,
        plan: &ValuePlan,
        collapsed: &HashSet<usize>,
        w: f32,
        ctx: &Ctx,
        top: f32,
        bottom: f32,
    ) -> DisplayList {
        layout_value_band(ValueBand {
            value: v,
            plan,
            collapsed,
            width: w,
            ctx,
            top,
            bottom,
        })
    }

    /// A tree's plan has to agree with the layout it replaces. The pane scrolls
    /// through the plan's total height and paints every row at the y the plan
    /// gave its node, so a disagreement is scroll drift at best and a missing
    /// row at worst — and the whole-document band pins both in one check.
    #[test]
    fn the_virtual_tree_plan_matches_a_full_layout() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let v = virtual_doc();
        let none = HashSet::new();
        for w in [400.0f32, 820.0, 1440.0, 2560.0] {
            let full = layout_value(&v, &none, w, &c);
            let plan = build_value_plan(&v, &none, w, &c);
            assert_eq!(
                plan.slots.len(),
                v_node_count(&v),
                "w {w}: the plan has one slot per node"
            );
            assert!(
                (plan.height - full.height).abs() < 0.5,
                "w {w}: plan height {} != full layout {}",
                plan.height,
                full.height
            );
            let whole = band(&v, &plan, &none, w, &c, f32::MIN, f32::MAX);
            assert_eq!(
                runs(&whole),
                runs(&full),
                "w {w}: a band over the whole document is not the layout"
            );
            assert_eq!(whole.height, plan.height, "the band scrolls the plan");
        }
    }

    /// The index has to be *seekable*: whatever the scroll offset, the band
    /// lays out the blocks that overlap it and nothing else — no row invented
    /// from a stale position, no visible row skipped because its node sits
    /// behind a subtree the walk skipped.
    #[test]
    fn a_tree_band_paints_exactly_the_rows_it_covers() {
        let f = FontSystem::load_embedded();
        let t = Theme::dark();
        let c = ctx(&f, &t);
        let v = virtual_doc();
        let none = HashSet::new();
        for w in [400.0f32, 900.0] {
            let full = layout_value(&v, &none, w, &c);
            let plan = build_value_plan(&v, &none, w, &c);
            let all = runs(&full);
            let known: HashSet<(i32, i32, String)> = all.iter().cloned().collect();
            // Offsets that are not multiples of the row height, so blocks are
            // entered mid-row and mid-subtree, plus both ends of the document.
            let view = 520.0f32;
            let last = (full.height - view).max(0.0);
            for k in 0..=14 {
                let top = last * k as f32 / 14.0;
                let got = band(&v, &plan, &none, w, &c, top, top + view);
                let got: HashSet<(i32, i32, String)> = runs(&got).into_iter().collect();
                for r in &got {
                    assert!(known.contains(r), "w {w} at {top}: the band invented {r:?}");
                }
                for r in &all {
                    if (r.1 as f32) > top && (r.1 as f32) < top + view && !got.contains(r) {
                        panic!("w {w} at {top}: the band missed {r:?}");
                    }
                }
            }
        }
    }

    /// Collapsing is what makes a tree's ids load-bearing: the summary a
    /// collapsed node shows must be the row the full layout shows, at the y the
    /// plan resizes every block after it to, for every node in the document.
    /// The document is small because this walks *every* node — at
    /// [`virtual_doc`]'s size it is a thousand layouts of a thousand rows.
    #[test]
    fn a_collapsed_node_moves_the_tree_the_way_a_full_layout_does() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let v = virtual_doc_rows(20);
        let w = 820.0;
        let none = HashSet::new();
        let open = layout_value(&v, &none, w, &c);
        let mut ids: Vec<usize> = open
            .hits
            .iter()
            .filter_map(|h| match &h.action {
                HitAction::ToggleNode(id) => Some(*id),
                HitAction::Link(_) => None,
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert!(
            ids.len() > 8,
            "the test document needs toggles: {}",
            ids.len()
        );
        let plan_open = build_value_plan(&v, &none, w, &c);
        assert_eq!(
            plan_open.slots.iter().filter(|s| s.id != NONE).count(),
            ids.len(),
            "the plan assigns exactly the ids a full layout does"
        );
        for &id in &ids {
            let mut collapsed = HashSet::new();
            collapsed.insert(id);
            let full = layout_value(&v, &collapsed, w, &c);
            let plan = build_value_plan(&v, &collapsed, w, &c);
            assert!(
                (plan.height - full.height).abs() < 0.5,
                "collapsed {id}: plan {} != full {}",
                plan.height,
                full.height
            );
            let whole = band(&v, &plan, &collapsed, w, &c, f32::MIN, f32::MAX);
            assert_eq!(runs(&whole), runs(&full), "collapsed {id}: rows differ");
            // The painted summary also has to be clickable where the plan put
            // it: a toggle whose hit rect drifts would expand the wrong node.
            let hit = whole
                .hits
                .iter()
                .find(|h| matches!(h.action, HitAction::ToggleNode(i) if i == id))
                .unwrap_or_else(|| panic!("collapsed {id}: its toggle lost its hit area"));
            let want = full
                .hits
                .iter()
                .find(|h| matches!(h.action, HitAction::ToggleNode(i) if i == id))
                .expect("the full layout has the same toggle")
                .rect;
            assert_eq!(hit.rect, want, "collapsed {id}: the toggle moved");
        }
    }

    /// Below the threshold a document is laid out whole, as it always was: no
    /// plan is kept, and a band with no plan is the full layout rather than an
    /// empty pane.
    #[test]
    fn a_tree_below_the_threshold_lays_out_whole() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let v = crate::parse::json::parser::parse(r#"{"a": [1, 2], "b": {"c": null}}"#).unwrap();
        let none = HashSet::new();
        let plan = build_value_plan(&v, &none, 800.0, &c);
        assert!(!plan.is_virtual(), "a four-node tree is not virtualized");
        assert_eq!(plan.slots.len(), v_node_count(&v));
        let whole = band(&v, &plan, &none, 800.0, &c, 0.0, 500.0);
        assert_eq!(
            runs(&whole),
            runs(&layout_value(&v, &none, 800.0, &c)),
            "an unplanned band must fall back to the whole layout"
        );
        // The threshold itself is a count of nodes, not bytes or lines: a
        // minified document is one line and as many nodes as it has values.
        let big = ValuePlan {
            slots: vec![
                NodeSlot {
                    top: 0.0,
                    h: 1.0,
                    parent: NONE,
                    next: NONE,
                    ord: 0,
                    id: 0,
                    first_id: 0,
                };
                VIRTUAL_JSON_NODES + 1
            ],
            height: 0.0,
            ids: 0,
        };
        assert!(big.is_virtual());
    }

    /// The nodes in a parsed value, to check a plan really has one slot each.
    fn v_node_count(v: &Value) -> usize {
        match v {
            Value::Object(pairs) => 1 + pairs.iter().map(|(_, c)| v_node_count(c)).sum::<usize>(),
            Value::Array(items) => 1 + items.iter().map(v_node_count).sum::<usize>(),
            _ => 1,
        }
    }

    /// The one-row fast path and the wrapping path have to agree on the two
    /// sides of the column's edge: a row that fits is emitted once, a row one
    /// cell wider folds, and neither paints past the column.
    #[test]
    fn rows_fold_at_the_column_edge_and_never_past_it() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let (col_w, x0) = super::super::column(900.0, &c);
        let size = c.body_size() * 0.9;
        let adv = f.advance(FontStyle::Mono, '0', size).max(1.0);
        let baselines = |list: &DisplayList| -> usize {
            let mut ys: Vec<i32> = list
                .ops
                .iter()
                .filter_map(|op| match op {
                    DrawOp::Text { y, .. } => Some(y.round() as i32),
                    _ => None,
                })
                .collect();
            ys.sort_unstable();
            ys.dedup();
            ys.len()
        };
        let widest = |list: &DisplayList| -> f32 {
            list.ops
                .iter()
                .filter_map(|op| match op {
                    DrawOp::Text {
                        x,
                        text,
                        size,
                        style,
                        ..
                    } => Some(x + f.measure(*style, text, *size)),
                    _ => None,
                })
                .fold(0.0f32, f32::max)
        };
        // `"short": "…"` at indent 1: root opener + one value row.
        let short = crate::parse::json::parser::parse(r#"{"short": "value"}"#).unwrap();
        let list = layout_value(&short, &HashSet::new(), 900.0, &c);
        // Root opener, the one field, and the root's closing brace.
        assert_eq!(baselines(&list), 3, "a fitting row must not fold");
        assert!(widest(&list) <= x0 + col_w + 0.5);
        // Just over a column's worth of cells: the same row folds, and every
        // visual row stays inside the column.
        let cells = (col_w / adv) as usize;
        let long = crate::parse::json::parser::parse(&format!(
            "{{\"k\": \"{}\"}}",
            "x".repeat(cells + 10)
        ))
        .unwrap();
        let list = layout_value(&long, &HashSet::new(), 900.0, &c);
        assert!(baselines(&list) > 3, "an over-wide row must fold");
        assert!(
            widest(&list) <= x0 + col_w + 0.5,
            "row past column: {}",
            widest(&list)
        );
    }
}
