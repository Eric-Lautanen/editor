//! Markdown layout: parsed document -> styled, wrapped rows.
//!
//! One size per block (headings scale it), mixed styles via text spans.
//! Wrapping measures every character in its own face, so proportional serif
//! and monospace code can share a row. Links register [`HitAction`] regions;
//! images render as labeled placeholders (PNG decoding is v2).

use super::{
    display_list::{DisplayList, DrawOp, FontStyle, HitAction, HitRegion},
    theme::Theme,
    Ctx,
};
use crate::{
    gfx::{color::Color, font::FontSystem, geom::Rect},
    parse::markdown::ast::{Align, Block, Document, Inline, ListItem},
};

/// A run of uniformly styled text inside a paragraph or cell.
#[derive(Clone)]
struct Span {
    text: String,
    style: FontStyle,
    color: Color,
    link: Option<String>,
    strike: bool,
    code: bool,
    /// Hard line break (zero width, forces a row end).
    brk: bool,
    /// Image placeholder: (src, alt). Always stands alone in its row.
    image: Option<(String, String)>,
}

impl Span {
    fn text(text: String, style: FontStyle, color: Color) -> Self {
        Self {
            text,
            style,
            color,
            link: None,
            strike: false,
            code: false,
            brk: false,
            image: None,
        }
    }
}

/// Flatten inlines to spans, tracking emphasis nesting for face selection.
fn spans_of(
    inlines: &[Inline],
    style: FontStyle,
    color: Color,
    link: Option<&str>,
    theme: &Theme,
    out: &mut Vec<Span>,
) {
    for inline in inlines {
        match inline {
            Inline::Text(t) => {
                let mut s = Span::text(t.clone(), style, color);
                s.link = link.map(|l| l.to_string());
                out.push(s);
            }
            Inline::Code(c) => {
                let mut s = Span::text(c.clone(), FontStyle::Mono, theme.fg);
                s.code = true;
                s.link = link.map(|l| l.to_string());
                out.push(s);
            }
            Inline::Emph(inner) => spans_of(
                inner,
                if style == FontStyle::Bold {
                    FontStyle::BoldItalic
                } else {
                    FontStyle::Italic
                },
                color,
                link,
                theme,
                out,
            ),
            Inline::Strong(inner) => spans_of(
                inner,
                if style == FontStyle::Italic {
                    FontStyle::BoldItalic
                } else {
                    FontStyle::Bold
                },
                color,
                link,
                theme,
                out,
            ),
            Inline::Strike(inner) => {
                let before = out.len();
                spans_of(inner, style, color, link, theme, out);
                for s in &mut out[before..] {
                    s.strike = true;
                }
            }
            Inline::Link { href, content } => {
                spans_of(content, style, theme.accent, Some(href), theme, out)
            }
            Inline::Image { src, alt } => out.push(Span {
                text: String::new(),
                style,
                color,
                link: None,
                strike: false,
                code: false,
                brk: false,
                image: Some((src.clone(), alt.clone())),
            }),
            Inline::SoftBreak => {
                let mut s = Span::text(" ".into(), style, color);
                s.link = link.map(|l| l.to_string());
                out.push(s);
            }
            Inline::HardBreak => out.push(Span {
                text: String::new(),
                style,
                color,
                link: None,
                strike: false,
                code: false,
                brk: true,
                image: None,
            }),
        }
    }
}

/// One character of a wrapped run: which span, which char offset, its width.
struct Atom {
    span: usize,
    off: usize,
    ch: char,
    w: f32,
}

/// Greedy word-wrap over mixed-style spans. Returns visual rows of
/// (span index, first char, one-past-last char). Breaks after spaces;
/// hard-break atoms end their row. Always makes progress.
fn wrap_spans(
    fonts: &FontSystem,
    spans: &[Span],
    size: f32,
    max_w: f32,
) -> Vec<Vec<(usize, usize, usize)>> {
    let mut atoms = Vec::new();
    for (si, s) in spans.iter().enumerate() {
        if s.image.is_some() {
            continue;
        }
        if s.brk {
            atoms.push(Atom {
                span: si,
                off: 0,
                ch: '\n',
                w: 0.0,
            });
            continue;
        }
        for (oi, ch) in s.text.chars().enumerate() {
            atoms.push(Atom {
                span: si,
                off: oi,
                ch,
                w: fonts.advance(s.style, ch, size),
            });
        }
    }
    let mut rows = Vec::new();
    let mut start = 0;
    while start < atoms.len() {
        // A leading newline is an empty row (hard break).
        if atoms[start].ch == '\n' {
            rows.push(Vec::new());
            start += 1;
            continue;
        }
        let (mut w, mut i, mut last_break) = (0.0f32, start, None);
        while i < atoms.len() {
            if atoms[i].ch == '\n' {
                break;
            }
            if w + atoms[i].w > max_w && i > start {
                break;
            }
            w += atoms[i].w;
            i += 1;
            if atoms[i - 1].ch == ' ' {
                last_break = Some(i);
            }
        }
        // Stopped at a newline: it ends the row and is consumed, not drawn.
        if i < atoms.len() && atoms[i].ch == '\n' {
            rows.push(ranges_of(&atoms, start, i));
            start = i + 1;
            continue;
        }
        // Consumed everything: final row, kept as-is.
        if i >= atoms.len() {
            rows.push(ranges_of(&atoms, start, i));
            break;
        }
        // Stopped at width: cut at the last space, trim trailing spaces.
        let cut = match last_break {
            Some(b) if b > start => b,
            _ => i,
        };
        let mut end = cut;
        while end > start && atoms[end - 1].ch == ' ' {
            end -= 1;
        }
        rows.push(ranges_of(&atoms, start, end));
        start = cut;
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows
}

/// Convert an atom range to per-span (span, start, end) triples.
fn ranges_of(atoms: &[Atom], start: usize, end: usize) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        let si = atoms[i].span;
        let s = atoms[i].off;
        let mut j = i + 1;
        while j < end && atoms[j].span == si && atoms[j].off == atoms[j - 1].off + 1 {
            j += 1;
        }
        out.push((si, s, atoms[j - 1].off + 1));
        i = j;
    }
    out
}

struct Md<'a> {
    fonts: &'a FontSystem,
    theme: &'a Theme,
    scale: f32,
    body: f32,
    ops: Vec<DrawOp>,
    hits: Vec<HitRegion>,
    y: f32,
}

impl Md<'_> {
    fn line_h(&self, size: f32) -> f32 {
        (size * 1.55).round()
    }

    fn gap(&mut self, size: f32, factor: f32) {
        self.y += (self.line_h(size) * factor).round();
    }

    fn baseline(&self, top: f32, style: FontStyle, size: f32) -> f32 {
        let m = self.fonts.metrics(style, size);
        (top + (self.line_h(size) - m.ascent - m.descent) / 2.0 + m.ascent).round()
    }

    fn rule(&mut self, x: f32, w: f32) {
        let thick = self.scale.max(1.0).round();
        self.ops.push(DrawOp::Line {
            x0: x,
            y0: self.y,
            x1: x + w,
            y1: self.y,
            color: self.theme.rule,
        });
        self.y += thick;
    }

    fn text_op(
        &mut self,
        x: f32,
        baseline: f32,
        text: String,
        style: FontStyle,
        size: f32,
        color: Color,
    ) {
        self.ops.push(DrawOp::Text {
            x,
            y: baseline,
            text,
            size,
            style,
            color,
        });
    }

    /// Emit pre-wrapped rows of spans. `first_x` offsets only the first visual
    /// row (list markers); continuations start at `x`.
    fn emit_rows(
        &mut self,
        rows: &[Vec<(usize, usize, usize)>],
        spans: &[Span],
        x: f32,
        first_x: f32,
        size: f32,
        style: FontStyle,
    ) {
        let lh = self.line_h(size);
        for (ri, row) in rows.iter().enumerate() {
            let rx = if ri == 0 { first_x } else { x };
            let baseline = self.baseline(self.y, style, size);
            let mut pen = rx;
            for &(si, s, e) in row {
                let sp = &spans[si];
                let piece: String = sp.text.chars().skip(s).take(e - s).collect();
                if piece.is_empty() {
                    continue;
                }
                let w: f32 = piece
                    .chars()
                    .map(|c| self.fonts.advance(sp.style, c, size))
                    .sum();
                if sp.code {
                    self.ops.push(DrawOp::RoundedRect {
                        rect: Rect::new(pen - 3.0, self.y + 2.0, w + 6.0, lh - 3.0),
                        radius: 4.0,
                        color: self.theme.code_bg,
                    });
                }
                self.text_op(pen, baseline, piece, sp.style, size, sp.color);
                if sp.strike {
                    let mid = (self.y + lh / 2.0).round();
                    self.ops.push(DrawOp::Line {
                        x0: pen,
                        y0: mid,
                        x1: pen + w,
                        y1: mid,
                        color: sp.color,
                    });
                }
                if let Some(href) = &sp.link {
                    self.hits.push(HitRegion {
                        rect: Rect::new(pen, self.y, w.max(4.0), lh),
                        action: HitAction::Link(href.clone()),
                    });
                }
                pen += w;
            }
            self.y += lh;
        }
    }

    /// A paragraph that may contain image placeholders: split at images,
    /// laying out text runs and panels in order.
    fn render_inlines(
        &mut self,
        inlines: &[Inline],
        x: f32,
        max_w: f32,
        size: f32,
        style: FontStyle,
    ) {
        let mut spans = Vec::new();
        spans_of(inlines, style, self.theme.fg, None, self.theme, &mut spans);
        let mut run: Vec<Span> = Vec::new();
        for span in spans {
            if let Some((src, alt)) = span.image.clone() {
                if !run.is_empty() {
                    let rows = wrap_spans(self.fonts, &run, size, max_w);
                    self.emit_rows(&rows, &run, x, x, size, style);
                    run.clear();
                }
                self.render_image(x, max_w, &src, &alt);
            } else {
                run.push(span);
            }
        }
        if !run.is_empty() {
            let rows = wrap_spans(self.fonts, &run, size, max_w);
            self.emit_rows(&rows, &run, x, x, size, style);
        }
    }

    fn render_image(&mut self, x: f32, max_w: f32, src: &str, alt: &str) {
        let lh = self.line_h(self.body);
        let h = lh * 2.0;
        self.ops.push(DrawOp::RoundedRect {
            rect: Rect::new(x, self.y, max_w, h),
            radius: 6.0,
            color: self.theme.code_bg,
        });
        let label = if alt.is_empty() { src } else { alt };
        let size = self.body * 0.85;
        let baseline = self.baseline(self.y + (h - lh) / 2.0, FontStyle::Regular, size);
        self.text_op(
            x + 12.0,
            baseline,
            format!("image: {label}"),
            FontStyle::Regular,
            size,
            self.theme.muted,
        );
        self.y += h + lh * 0.4;
    }

    fn render_blocks(&mut self, blocks: &[Block], x: f32, max_w: f32) {
        for b in blocks {
            match b {
                Block::Paragraph(inlines) => {
                    self.render_inlines(inlines, x, max_w, self.body, FontStyle::Regular);
                    self.gap(self.body, 0.55);
                }
                Block::Heading { level, content } => {
                    let factor =
                        [1.55, 1.3, 1.15, 1.05, 1.0, 0.9][(*level).clamp(1, 6) as usize - 1];
                    let size = self.body * factor;
                    self.gap(size, 0.5);
                    self.render_inlines(content, x, max_w, size, FontStyle::Bold);
                    if *level <= 2 {
                        self.gap(size, 0.2);
                        self.rule(x, max_w);
                        self.gap(size, 0.25);
                    } else {
                        self.gap(size, 0.35);
                    }
                }
                Block::Code { lang: _, text } => {
                    let size = self.body * 0.9;
                    let lh = self.line_h(size);
                    let style = FontStyle::Mono;
                    let rows: Vec<&str> = text.split('\n').collect();
                    let h = rows.len() as f32 * lh + 16.0;
                    self.ops.push(DrawOp::RoundedRect {
                        rect: Rect::new(x, self.y, max_w, h),
                        radius: 6.0,
                        color: self.theme.code_bg,
                    });
                    let mut cy = self.y + 8.0;
                    for row in rows {
                        let baseline = self.baseline(cy, style, size);
                        self.text_op(
                            x + 12.0,
                            baseline,
                            row.to_string(),
                            style,
                            size,
                            self.theme.fg,
                        );
                        cy += lh;
                    }
                    self.y += h;
                    self.gap(self.body, 0.55);
                }
                Block::Quote(inner) => {
                    let bar_x = x;
                    let content_x = x + 14.0;
                    let content_w = (max_w - 14.0).max(40.0);
                    let top = self.y;
                    let top_op = self.ops.len();
                    self.render_blocks(inner, content_x, content_w);
                    // The children end with a block gap; the bar stops above it.
                    let bottom = self.y - self.gap_size();
                    self.ops.insert(
                        top_op,
                        DrawOp::Rect {
                            rect: Rect::new(bar_x, top, 3.0, (bottom - top).max(4.0)),
                            color: self.theme.quote_bar,
                        },
                    );
                    self.gap(self.body, 0.55);
                }
                Block::List {
                    ordered,
                    start,
                    items,
                } => {
                    let indent = self
                        .fonts
                        .advance(FontStyle::Regular, '0', self.body)
                        .max(1.0)
                        * 2.0;
                    for (idx, item) in items.iter().enumerate() {
                        self.render_list_item(item, *ordered, start + idx as u32, x, max_w, indent);
                    }
                    self.gap(self.body, 0.35);
                }
                Block::Table { align, head, rows } => {
                    self.render_table(align, head, rows, x, max_w);
                    self.gap(self.body, 0.55);
                }
                Block::Rule => {
                    self.gap(self.body, 0.3);
                    self.rule(x, max_w);
                    self.gap(self.body, 0.55);
                }
            }
        }
    }

    /// Trailing gap left by the last rendered block (excluded from quote bars).
    fn gap_size(&self) -> f32 {
        (self.line_h(self.body) * 0.55).round()
    }

    fn render_list_item(
        &mut self,
        item: &ListItem,
        ordered: bool,
        num: u32,
        x: f32,
        max_w: f32,
        indent: f32,
    ) {
        let size = self.body;
        let marker = if let Some(checked) = item.checked {
            if checked { "[x] " } else { "[ ] " }.to_string()
        } else if ordered {
            format!("{num}. ")
        } else {
            "• ".to_string()
        };
        let mw = self.fonts.measure(FontStyle::Regular, &marker, size);
        let content_x = x + indent;
        let content_w = (max_w - indent).max(40.0);
        let marker_color = if item.checked == Some(true) {
            self.theme.accent
        } else {
            self.theme.fg
        };
        // Marker aligns with the first row's baseline.
        let baseline = self.baseline(self.y, FontStyle::Regular, size);
        self.text_op(
            x + indent - mw,
            baseline,
            marker,
            FontStyle::Regular,
            size,
            marker_color,
        );
        self.render_blocks(&item.blocks, content_x, content_w);
    }

    fn render_table(
        &mut self,
        align: &[Align],
        head: &[Vec<Inline>],
        rows: &[Vec<Vec<Inline>>],
        x: f32,
        max_w: f32,
    ) {
        let n = align.len().max(1);
        let size = self.body * 0.92;
        let lh = self.line_h(size);
        let adv = self.fonts.advance(FontStyle::Mono, '0', size).max(1.0);
        let cell_pad = adv * 1.5;
        // Natural single-line widths.
        let mut widths = vec![0.0f32; n];
        let cell_width = |cell: &[Inline], style: FontStyle| -> f32 {
            let mut spans = Vec::new();
            spans_of(cell, style, self.theme.fg, None, self.theme, &mut spans);
            spans
                .iter()
                .filter(|s| s.image.is_none() && !s.brk)
                .map(|s| self.fonts.measure(s.style, &s.text, size))
                .sum()
        };
        for (ci, cell) in head.iter().enumerate().take(n) {
            widths[ci] = widths[ci].max(cell_width(cell, FontStyle::Bold));
        }
        for row in rows {
            for (ci, cell) in row.iter().enumerate().take(n) {
                widths[ci] = widths[ci].max(cell_width(cell, FontStyle::Regular));
            }
        }
        let natural: f32 = widths.iter().sum::<f32>() + cell_pad * 2.0 * n as f32;
        if natural > max_w {
            // Equal shares; cells truncate with an ellipsis downstream.
            let share = ((max_w - cell_pad * 2.0 * n as f32) / n as f32).max(adv * 4.0);
            widths = vec![share; n];
        }
        // Column origins.
        let mut col_x = Vec::with_capacity(n);
        let mut cx = x;
        for w in &widths {
            col_x.push(cx);
            cx += w + cell_pad * 2.0;
        }
        let render_row = |md: &mut Md, cells: &[Vec<Inline>], style: FontStyle| {
            for (ci, cell) in cells.iter().enumerate().take(n) {
                let mut spans = Vec::new();
                spans_of(cell, style, md.theme.fg, None, md.theme, &mut spans);
                let budget = widths[ci];
                let text_w: f32 = spans
                    .iter()
                    .filter(|s| s.image.is_none() && !s.brk)
                    .map(|s| md.fonts.measure(s.style, &s.text, size))
                    .sum();
                let truncated = text_w > budget;
                let avail = if truncated {
                    budget - md.fonts.measure(style, "…", size)
                } else {
                    budget
                };
                // Lay out one visual row, then clip/truncate pieces to budget.
                let wrapped = wrap_spans(md.fonts, &spans, size, avail);
                let first = wrapped.into_iter().next().unwrap_or_default();
                let used: f32 = first
                    .iter()
                    .map(|&(si, s, e)| {
                        spans[si]
                            .text
                            .chars()
                            .skip(s)
                            .take(e - s)
                            .map(|c| md.fonts.advance(spans[si].style, c, size))
                            .sum::<f32>()
                    })
                    .sum();
                let mut ox = match align.get(ci).copied().unwrap_or(Align::None) {
                    Align::Right => col_x[ci] + cell_pad + (budget - used).max(0.0),
                    Align::Center => col_x[ci] + cell_pad + ((budget - used).max(0.0) / 2.0),
                    Align::None | Align::Left => col_x[ci] + cell_pad,
                };
                let baseline = md.baseline(md.y, style, size);
                for &(si, s, e) in &first {
                    let sp = &spans[si];
                    let piece: String = sp.text.chars().skip(s).take(e - s).collect();
                    if piece.is_empty() {
                        continue;
                    }
                    let w: f32 = piece
                        .chars()
                        .map(|c| md.fonts.advance(sp.style, c, size))
                        .sum();
                    md.text_op(ox, baseline, piece, sp.style, size, sp.color);
                    if let Some(href) = &sp.link {
                        md.hits.push(HitRegion {
                            rect: Rect::new(ox, md.y, w.max(4.0), lh),
                            action: HitAction::Link(href.clone()),
                        });
                    }
                    ox += w;
                }
                if truncated {
                    md.text_op(ox, baseline, "…".into(), style, size, md.theme.muted);
                }
            }
            md.y += lh;
        };
        render_row(self, head, FontStyle::Bold);
        self.rule(x, max_w.min(natural));
        for row in rows {
            // Pad short rows with empty cells.
            let mut full = row.clone();
            full.resize_with(n, Default::default);
            render_row(self, &full, FontStyle::Regular);
        }
    }
}

/// Lay out a parsed Markdown document.
pub fn layout(doc: &Document, width: f32, ctx: &Ctx) -> DisplayList {
    let t = ctx.theme;
    let body = ctx.body_size();
    let pad = ctx.px(32.0);
    let col_w = (width - 2.0 * pad)
        .min(ctx.px(t.content_max_width))
        .max(ctx.px(120.0));
    let x0 = ((width - col_w) / 2.0).max(pad.min(width / 4.0)).round();
    let mut md = Md {
        fonts: ctx.fonts,
        theme: t,
        scale: ctx.scale,
        body,
        ops: Vec::new(),
        hits: Vec::new(),
        y: pad,
    };
    md.render_blocks(&doc.blocks, x0, col_w);
    let height = md.y + pad;
    DisplayList {
        ops: md.ops,
        height,
        hits: md.hits,
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

    fn doc(src: &str) -> Document {
        crate::parse::markdown::parse(src)
    }

    #[test]
    fn headings_scale_and_rule() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(&doc("# Big\n\ntext"), 800.0, &c);
        let sizes: Vec<f32> = list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { size, .. } => Some(*size),
                _ => None,
            })
            .collect();
        assert!(sizes[0] > sizes[sizes.len() - 1], "heading not larger");
        assert!(
            list.ops.iter().any(|op| matches!(op, DrawOp::Line { .. })),
            "h1 rule missing"
        );
    }

    #[test]
    fn links_code_strike_quote() {
        let f = FontSystem::load_embedded();
        let t = Theme::dark();
        let c = ctx(&f, &t);
        let list = layout(
            &doc("[t](https://e.com) `c` ~~s~~\n\n> q\n\n![a](i.png)"),
            800.0,
            &c,
        );
        assert!(
            list.hits
                .iter()
                .any(|h| matches!(&h.action, HitAction::Link(h) if h == "https://e.com")),
            "link hit missing"
        );
        assert!(
            list.ops
                .iter()
                .any(|op| matches!(op, DrawOp::RoundedRect { .. })),
            "code/quote/image panel missing"
        );
        // Strike-through draws a line through text (plus quote has none).
        assert!(
            list.ops.iter().any(|op| matches!(op, DrawOp::Line { .. })),
            "strike line missing"
        );
    }

    #[test]
    fn lists_and_tables() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(
            &doc("- a\n  - b\n- [x] done\n\n| h1 | h2 |\n|---|---:|\n| x | 1 |"),
            800.0,
            &c,
        );
        // Bullets, task marker, table header + separator line.
        let text: String = list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("|");
        assert!(text.contains('•'), "bullet missing: {text}");
        assert!(text.contains("[x]"), "task marker missing: {text}");
        assert!(text.contains("h1"), "table header missing");
        assert!(
            list.ops.iter().any(|op| matches!(op, DrawOp::Line { .. })),
            "table rule missing"
        );
        assert!(list.height > 200.0);
    }

    #[test]
    fn short_text_stays_on_one_row() {
        // Regression: rows must not break at the last space when everything fits.
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(&doc("# Readitor sample\n"), 1100.0, &c);
        let rows: Vec<String> = list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(rows, vec!["Readitor sample".to_string()]);
    }

    #[test]
    fn sample_renders_tall() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let src = include_str!("../../samples/sample.md");
        let list = layout(&doc(src), 900.0, &c);
        assert!(!list.ops.is_empty());
        assert!(!list.hits.is_empty(), "sample link has no hit region");
        assert!(list.height > 400.0);
    }
}
