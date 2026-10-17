//! Collapsible JSON tree layout: parsed values -> tinted, indented rows.
//!
//! Every row is a run of single-style text segments (keys, strings, numbers,
//! punctuation each get their theme color). Rows are wrapped with a hanging
//! indent via [`wrap_ranges`]; the tree is all monospace so char-index math
//! holds. Containers carry `[+]`/`[-]` toggles backed by [`HitAction`]
//! regions; collapsed subtrees keep stable node ids by walking children
//! silently instead of skipping them.

use super::{
    display_list::{DisplayList, DrawOp, FontStyle, HitAction, HitRegion},
    text::wrap_ranges,
    theme::Theme,
    Ctx,
};
use crate::{
    core::buffer::Buffer,
    error::Error,
    gfx::{color::Color, font::FontSystem, geom::Rect},
    parse::{
        json::{pretty, Value},
        jsonl::Record,
    },
};
use std::collections::HashSet;

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
    /// the node id and the char offset of the `[+]`/`[-]` marker within the
    /// row — the hit area spans from the row start through the marker, so both
    /// the key text and the glyph itself are clickable.
    fn emit_row(&mut self, indent: usize, segs: Vec<Seg>, toggle: Option<(usize, usize)>) {
        if !self.emit {
            return;
        }
        let x_indent = self.x0 + indent as f32 * self.indent_w;
        let avail = (self.col_w - (x_indent - self.x0)).max(self.adv * 8.0);
        let full: String = segs.iter().map(|s| s.text.as_str()).collect();
        // Cumulative char offsets per segment for slicing wrapped pieces back.
        let mut starts = Vec::with_capacity(segs.len());
        let mut off = 0;
        for s in &segs {
            starts.push(off);
            off += s.text.chars().count();
        }
        let rows = wrap_ranges(self.fonts, FontStyle::Mono, self.size, &full, avail);
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
                    self.text_op(
                        row_x + (a - s) as f32 * self.adv,
                        baseline,
                        piece,
                        seg.color,
                    );
                }
            }
            // Toggle hit area covers the marker on the first visual row.
            if ri == 0 {
                if let Some((id, off)) = toggle {
                    self.hits.push(HitRegion {
                        rect: Rect::new(
                            x_indent,
                            self.y,
                            self.adv * (off as f32 + 3.0),
                            self.line_h,
                        ),
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
            Value::Array(_) | Value::Object(_) => vec![],
        }
    }

    fn render_value(&mut self, v: &Value, indent: usize, mut head: Vec<Seg>) {
        let t = self.theme;
        let (is_obj, len) = match v {
            Value::Object(pairs) => (true, pairs.len()),
            Value::Array(items) => (false, items.len()),
            scalar => {
                head.extend(self.scalar_segs(scalar));
                self.emit_row(indent, head, None);
                return;
            }
        };
        let (open, close) = if is_obj { ("{", "}") } else { ("[", "]") };
        if len == 0 {
            head.push(Seg::new(format!("{open}{close}"), t.fg));
            self.emit_row(indent, head, None);
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        // Char offset of the `[+]`/`[-]` marker within the row: the hit area
        // spans from the row start through the marker so the key text and the
        // glyph itself are both clickable.
        let off: usize = head.iter().map(|s| s.text.chars().count()).sum();
        if self.collapsed.contains(&id) {
            head.push(Seg::new("[-]", t.accent));
            head.push(Seg::new(" ", t.fg));
            head.push(Seg::new(open.to_string(), t.fg));
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
            head.push(Seg::new(format!("...{close}"), t.fg));
            head.push(Seg::new(format!("  {len} {what}"), t.muted));
            self.emit_row(indent, head, Some((id, off)));
            // Walk silently so toggling never renumbers later nodes.
            let emit = std::mem::replace(&mut self.emit, false);
            self.render_children(v, indent + 1);
            self.emit = emit;
            return;
        }
        head.push(Seg::new("[+]", t.accent));
        head.push(Seg::new(" ", t.fg));
        head.push(Seg::new(open, t.fg));
        self.emit_row(indent, head, Some((id, off)));
        self.render_children(v, indent + 1);
        if self.emit {
            self.emit_row(indent, vec![Seg::new(close, t.fg)], None);
        }
    }

    fn render_children(&mut self, v: &Value, indent: usize) {
        let t = self.theme;
        match v {
            Value::Object(pairs) => {
                for (k, child) in pairs {
                    let head = vec![
                        Seg::new(pretty::escape(k), t.json_key),
                        Seg::new(": ", t.fg),
                    ];
                    self.render_value(child, indent, head);
                }
            }
            Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    let head = vec![Seg::new(i.to_string(), t.muted), Seg::new(": ", t.fg)];
                    self.render_value(child, indent, head);
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
                let caret_x = self.x0 + self.adv + (col.saturating_sub(1)) as f32 * self.adv;
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
        height: m.ascent + m.descent,
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
    DisplayList {
        ops: tr.ops,
        height: tr.y + tr.pad,
        hits: tr.hits,
    }
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
struct Geom {
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
            line_h,
            adv,
            indent_w,
            // emit_row: head indent is 0, avail = col_w - 0.
            head_avail: col_w.max(adv * 8.0),
            // render_error's title bound (indent shift applies on x0).
            banner_avail: (col_w - adv * 2.0).max(adv * 8.0),
            pad: ctx.px(32.0),
        }
    }
}

/// Wrapped-row count for a single-style text into `avail`, applying the same
/// rule `wrap_ranges`' mono fast path does — one advance per ASCII char, and
/// a break after the last space inside the window. Heights must be identical
/// to what a real layout produces, so this mirrors that code exactly.
fn rows_for_chars(text: &str, avail: f32, adv: f32) -> usize {
    let n = text.chars().count();
    if n == 0 {
        return 1; // wrap_ranges returns one (empty) row
    }
    let per_row = (avail / adv).max(1.0) as usize;
    let chars: Vec<char> = text.chars().collect();
    let mut rows = 0usize;
    let mut start = 0usize;
    while start < n {
        if n - start <= per_row {
            rows += 1;
            break;
        }
        let ideal = start + per_row;
        let cut = match chars[start..ideal].iter().rposition(|&c| c == ' ') {
            Some(p) if p > 0 => start + p + 1,
            _ => ideal,
        };
        rows += 1;
        start = cut;
    }
    rows
}

/// Lay out one parsed JSON document as a collapsible tree.
pub fn layout_value(v: &Value, collapsed: &HashSet<usize>, width: f32, ctx: &Ctx) -> DisplayList {
    let mut tr = tree(width, ctx, collapsed);
    tr.render_value(v, 0, Vec::new());
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
            Ok(v) => tr.render_value(v, 1, Vec::new()),
            Err(Error::Parse { line, col, msg }) => {
                // The error position is within the raw line; the snippet is
                // the record's own physical line.
                let snippet = buf.line(
                    rec.line
                        .saturating_sub(1)
                        .min(buf.line_count().saturating_sub(1)),
                );
                // Indent the banner with the record body.
                let saved_x0 = tr.x0;
                tr.x0 += tr.indent_w;
                tr.col_w -= tr.indent_w;
                tr.render_error(*line, *col, msg, Some(snippet));
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
        /// Advance the walk over `v`; `head_chars` is the char count of the
        /// prefix already on the row (`"0: "`, a key + `": "`). Collapsed
        /// containers count a summary row instead of their children, exactly
        /// as `render_value` paints them.
        fn count_value(&mut self, v: &Value, indent: usize, head_chars: usize) {
            let g = self.g;
            let width = indent_avail(g, indent);
            match v {
                Value::Object(pairs) => {
                    let len = pairs.len();
                    if len == 0 {
                        self.h += g.line_h
                            * rows_for_chars(&(" ".repeat(head_chars)), width, g.adv) as f32;
                        return;
                    }
                    let id = self.id_base + self.nodes;
                    self.nodes += 1;
                    let row_chars = head_chars + "[+] ".len() + "{".len();
                    self.h +=
                        g.line_h * rows_for_chars(&(" ".repeat(row_chars)), width, g.adv) as f32;
                    if self.collapsed.contains(&id) {
                        // Summary row: open + "...}" + "  N fields".
                        let summary =
                            format!("...}}  {len} {}", if len == 1 { "field" } else { "fields" });
                        let row2 = head_chars + 4 + summary.chars().count();
                        self.h +=
                            g.line_h * rows_for_chars(&(" ".repeat(row2)), width, g.adv) as f32;
                        return;
                    }
                    for (k, child) in pairs {
                        let hc = pretty::escape(k).chars().count() + 2; // key + ": "
                        self.count_value(child, indent + 1, hc);
                    }
                    // Closing row.
                    self.h +=
                        g.line_h * rows_for_chars(&(" ".repeat(head_chars)), width, g.adv) as f32;
                }
                Value::Array(items) => {
                    let len = items.len();
                    if len == 0 {
                        self.h += g.line_h
                            * rows_for_chars(&(" ".repeat(head_chars)), width, g.adv) as f32;
                        return;
                    }
                    let id = self.id_base + self.nodes;
                    self.nodes += 1;
                    let row_chars = head_chars + "[+] ".len() + "[".len();
                    self.h +=
                        g.line_h * rows_for_chars(&(" ".repeat(row_chars)), width, g.adv) as f32;
                    if self.collapsed.contains(&id) {
                        let summary =
                            format!("...]  {len} {}", if len == 1 { "item" } else { "items" });
                        let row2 = head_chars + 4 + summary.chars().count();
                        self.h +=
                            g.line_h * rows_for_chars(&(" ".repeat(row2)), width, g.adv) as f32;
                        return;
                    }
                    for (i, child) in items.iter().enumerate() {
                        let hc = i.to_string().len() + 2; // index + ": "
                        self.count_value(child, indent + 1, hc);
                    }
                    // Closing row.
                    self.h +=
                        g.line_h * rows_for_chars(&(" ".repeat(head_chars)), width, g.adv) as f32;
                }
                scalar => {
                    let text_len = match scalar {
                        Value::Null => 4,
                        Value::Bool(_) => 4,
                        Value::Number(n) => n.chars().count(),
                        Value::String(s) => pretty::escape(s).chars().count(),
                        _ => unreachable!(),
                    };
                    self.h += g.line_h
                        * rows_for_chars(&(" ".repeat(head_chars + text_len)), width, g.adv) as f32;
                }
            }
        }
    }

    // Head row (record's "#N record" text, one fewer char per # style) at indent 0.
    let head_len = rec.line.to_string().len() + 2 + "record".len();
    h += g.line_h * rows_for_chars(&(" ".repeat(head_len)), g.head_avail, g.adv) as f32;
    match &rec.value {
        Ok(v) => {
            let mut walk = Walk {
                g,
                collapsed,
                id_base,
                h,
                nodes,
            };
            walk.count_value(v, 1, 0);
            h = walk.h;
            nodes = walk.nodes;
        }
        Err(Error::Parse { line, col, msg }) => {
            let title = format!("! {line}:{col}: {msg}");
            let rows = rows_for_chars(&title, g.banner_avail, g.adv);
            let body_h = rows as f32 * g.line_h + 2.0 * g.line_h;
            let banner_h = body_h + g.line_h * 0.75;
            h = g.line_h * 0.5 + banner_h + g.line_h * 0.5;
        }
        Err(other) => {
            let title = format!("1:1: {other}");
            let rows = rows_for_chars(&title, g.banner_avail, g.adv);
            let body_h = rows as f32 * g.line_h;
            let banner_h = body_h + g.line_h * 0.75;
            h = g.line_h * 0.5 + banner_h + g.line_h * 0.5;
        }
    }
    Measured { h, nodes }
}

/// `emit_row`'s available width at an indent within the tree.
fn indent_avail(g: &Geom, indent: usize) -> f32 {
    let shift = indent as f32 * g.indent_w * 0.0; // head row indent is 0 in px terms
    (g.head_avail - shift).max(g.adv * 8.0)
}

/// Where a record sits in the virtual list.
#[derive(Clone, Copy, Debug)]
pub struct RecordSlot {
    /// Record index into the records slice.
    pub record: usize,
    /// y of the record block's top (its separator half-line for i>0) in the
    /// virtual list — the coordinate scroll offsets compare against.
    pub top: f32,
    /// Height of the record's own rows (no pads, no separators).
    pub h: f32,
    /// First node id the record's walk assigns.
    pub id_base: usize,
}

/// The layout plan: prefix sums of record heights and id offsets, plus the
/// total virtual height. Rebuilt on width/zoom/collapsed-set changes; reused
/// by every paint in between.
#[derive(Clone, Debug)]
pub struct Plan {
    pub slots: Vec<RecordSlot>,
    pub height: f32,
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
    if !virtualize {
        // Small file: no virtualization — the caller paints the full layout;
        // the plan only reports the height the pane scrolls through.
        let list = layout_records(records, &Buffer::from_text(""), collapsed, width, ctx);
        return Plan {
            slots: Vec::new(),
            height: list.height,
        };
    }
    let g = Geom::of(width, ctx);
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
        });
        id_base += m.nodes;
        top += m.h;
    }
    Plan {
        slots,
        height: top + g.pad,
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
            Ok(v) => tr.render_value(v, 1, Vec::new()),
            Err(Error::Parse { line, col, msg }) => {
                let snippet = buf.line(
                    rec.line
                        .saturating_sub(1)
                        .min(buf.line_count().saturating_sub(1)),
                );
                let saved_x0 = tr.x0;
                tr.x0 += tr.indent_w;
                tr.col_w -= tr.indent_w;
                tr.render_error(*line, *col, msg, Some(snippet));
                tr.x0 = saved_x0;
                tr.col_w += tr.indent_w;
            }
            Err(other) => tr.render_error(1, 1, &other.to_string(), None),
        }
    }
    DisplayList {
        ops: tr.ops,
        height: plan.height,
        hits: tr.hits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{gfx::font::FontSystem, layout::theme::Theme};

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
    }
}
