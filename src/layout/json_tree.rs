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
    let col_w = (width - 2.0 * pad)
        .min(ctx.px(t.content_max_width))
        .max(ctx.px(120.0));
    let x0 = ((width - col_w) / 2.0).max(pad.min(width / 4.0)).round();
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
