//! Markdown layout: parsed document -> styled, wrapped rows.
//!
//! One size per block (headings scale it), mixed styles via text spans.
//! Wrapping measures every character in its own face, so proportional serif
//! and monospace code can share a row. Links register [`HitAction`] regions;
//! images render as labeled placeholders (PNG decoding is v2).

use super::{
    display_list::{DisplayList, DrawOp, FontStyle, HitAction, HitRegion},
    plan, syntax,
    theme::Theme,
    Ctx,
};
use crate::{
    gfx::{color::Color, font::FontSystem, geom::Rect},
    parse::markdown::ast::{AlertKind, Align, Block, Document, Inline, ListItem},
};

/// Side padding (physical px) inside a code panel, and the indent a folded
/// continuation row gets. Both are unscaled: they sit next to padded text.
const CODE_PAD: f32 = 12.0;
const CODE_HANG: &str = "  ";

/// `text` clipped to `budget` px with a trailing ellipsis. Measures once per
/// character instead of re-measuring a shrinking prefix, which matters when a
/// long image path sits in a paragraph.
fn truncate(fonts: &FontSystem, style: FontStyle, size: f32, text: &str, budget: f32) -> String {
    if fonts.measure(style, text, size) <= budget {
        return text.to_string();
    }
    let ell = fonts.measure(style, "…", size);
    let mut out = String::new();
    let mut w = 0.0;
    for ch in text.chars() {
        let cw = fonts.advance(style, ch, size);
        if w + cw + ell > budget {
            break;
        }
        w += cw;
        out.push(ch);
    }
    out.push('…');
    out
}

/// A run of uniformly styled text inside a paragraph or cell.
#[derive(Clone)]
struct ImageSpan {
    src: String,
    alt: String,
    width: Option<u32>,
    height: Option<u32>,
    align: Option<Align>,
}

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
    /// Image placeholder. Always stands alone in its row.
    image: Option<ImageSpan>,
    /// `<ins>` / `<u>`.
    underline: bool,
    /// `<mark>`: an accent wash behind the glyphs.
    highlight: bool,
    /// `<sup>` / `<sub>`: smaller glyphs on a shifted baseline.
    sup: bool,
    sub: bool,
    /// `[^1]`: the note this marker jumps to, so the glyph gets a hit region.
    footnote: Option<usize>,
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
            underline: false,
            highlight: false,
            sup: false,
            sub: false,
            footnote: None,
        }
    }
}

/// `<sup>`/`<sub>` glyphs are 75% of the run's size, like the browser default.
fn span_size(span: &Span, size: f32) -> f32 {
    if span.sup || span.sub {
        (size * 0.75).max(1.0)
    } else {
        size
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
            Inline::Underline(inner) => {
                let before = out.len();
                spans_of(inner, style, color, link, theme, out);
                for s in &mut out[before..] {
                    s.underline = true;
                }
            }
            Inline::Highlight(inner) => {
                let before = out.len();
                spans_of(inner, style, color, link, theme, out);
                for s in &mut out[before..] {
                    s.highlight = true;
                }
            }
            Inline::Sup(inner) => {
                let before = out.len();
                spans_of(inner, style, color, link, theme, out);
                for s in &mut out[before..] {
                    s.sup = true;
                }
            }
            Inline::Sub(inner) => {
                let before = out.len();
                spans_of(inner, style, color, link, theme, out);
                for s in &mut out[before..] {
                    s.sub = true;
                }
            }
            Inline::Link { href, content } => {
                spans_of(content, style, theme.accent, Some(href), theme, out)
            }
            // A footnote marker is a raised number, like GitHub's; a label
            // with no definition has nothing to point at and stays as typed.
            Inline::FootnoteRef { label, number } => {
                if *number == 0 {
                    let mut s = Span::text(format!("[^{label}]"), style, color);
                    s.link = link.map(str::to_string);
                    out.push(s);
                } else {
                    let mut s = Span::text(number.to_string(), style, theme.accent);
                    s.sup = true;
                    s.footnote = Some(*number);
                    out.push(s);
                }
            }
            Inline::Image {
                src,
                alt,
                width,
                height,
                align,
            } => out.push(Span {
                text: String::new(),
                style,
                color,
                // A badge is `<a href><img></a>`: the image keeps the anchor's
                // target so the whole card is clickable.
                link: link.map(str::to_string),
                strike: false,
                code: false,
                brk: false,
                image: Some(ImageSpan {
                    src: src.clone(),
                    alt: alt.clone(),
                    width: *width,
                    height: *height,
                    align: *align,
                }),
                underline: false,
                highlight: false,
                sup: false,
                sub: false,
                footnote: None,
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
                underline: false,
                highlight: false,
                sup: false,
                sub: false,
                footnote: None,
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

/// Where a block of wrapped rows sits: `x` is the left edge, `first_x` the
/// left edge of the first visual row only (list markers indent the rows that
/// follow, not the one they label), and `max_w` the width an aligned container
/// gives the row — see [`Md::emit_rows`].
#[derive(Clone, Copy)]
struct RowBox {
    x: f32,
    first_x: f32,
    max_w: f32,
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
        let mut it = s.text.chars().peekable();
        let mut oi = 0;
        let ssize = span_size(s, size);
        while let Some(ch) = it.next() {
            atoms.push(Atom {
                span: si,
                off: oi,
                ch,
                // Widths need the lookahead too, or a row holding an emoji
                // would be laid out with the narrower text-form advance.
                w: fonts.advance_at(s.style, ch, it.peek().copied(), ssize),
            });
            oi += 1;
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

/// One table row's column geometry: the leaf width of each column, where
/// its text starts, its alignment, and the row's top. Grouped so the row
/// painter takes the row and its geometry, not eight loose numbers.
struct TableRow<'a> {
    widths: &'a [f32],
    col_x: &'a [f32],
    align: &'a [Align],
    top: f32,
}

struct Md<'a> {
    fonts: &'a FontSystem,
    theme: &'a Theme,
    scale: f32,
    body: f32,
    ops: Vec<DrawOp>,
    hits: Vec<HitRegion>,
    y: f32,
    /// Height of the most recent block gap. A list item removes its last
    /// block's trailing gap so items sit close together instead of reading as
    /// one-paragraph-per-bullet.
    last_gap: f32,
    /// Alignment imposed by an enclosing `<div align>`, `<p align>` or
    /// `<center>`: rows inside it are shifted within the column.
    align: Option<Align>,
}

impl Md<'_> {
    fn line_h(&self, size: f32) -> f32 {
        (size * 1.55).round()
    }

    fn gap(&mut self, size: f32, factor: f32) {
        let g = (self.line_h(size) * factor).round();
        self.y += g;
        self.last_gap = g;
    }

    fn baseline(&self, top: f32, style: FontStyle, size: f32) -> f32 {
        let m = self.fonts.metrics(style, size);
        (top + (self.line_h(size) - m.height()) / 2.0 + m.ascent).round()
    }

    /// Where a strike-through crosses the glyphs: the middle of the *x-height*,
    /// not the middle of the line box. The box also carries the ascender and
    /// descender the lowercase letters never reach, so its midpoint sat the
    /// rule roughly 4 px high — across the tops of `struck text` instead of
    /// through them. 0.27 of the ascent above the baseline lands mid-x-height
    /// for the embedded faces (x-height ≈ 0.45 em, ascent ≈ 0.9 em).
    fn strike_y(&self, baseline: f32, style: FontStyle, size: f32) -> f32 {
        let m = self.fonts.metrics(style, size);
        (baseline - m.ascent * 0.27).round()
    }

    /// Where a link's underline sits: below the glyphs' feet, not on the
    /// baseline. `Metrics::descent` is negative (it grows downward), so the
    /// old `baseline + descent * 0.4` put the rule *above* the baseline, on
    /// the bottom third of `n`, `k` and `a`; most of the descent is air the
    /// letters leave free.
    fn underline_y(&self, baseline: f32, style: FontStyle, size: f32) -> f32 {
        let m = self.fonts.metrics(style, size);
        (baseline - m.descent * 0.75).round()
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
    /// row (list markers); continuations start at `x`. `max_w` is the width an
    /// aligned container gives the row: with `self.align` set, each row is
    /// shifted inside it, which is how `<div align="center">` and
    /// `<h1 align="center">` read as centered text rather than left-aligned
    /// words in a centered box.
    fn emit_rows(
        &mut self,
        rows: &[Vec<(usize, usize, usize)>],
        spans: &[Span],
        row_box: RowBox,
        size: f32,
        style: FontStyle,
    ) {
        let RowBox { x, max_w, .. } = row_box;
        let lh = self.line_h(size);
        for (ri, row) in rows.iter().enumerate() {
            let row_w: f32 = row
                .iter()
                .map(|&(si, s, e)| {
                    let sp = &spans[si];
                    self.fonts
                        .measure_range(sp.style, &sp.text, s, e, span_size(sp, size))
                })
                .sum();
            let rx = match self.align {
                Some(Align::Center) => x + ((max_w - row_w) / 2.0).max(0.0),
                Some(Align::Right) => x + (max_w - row_w).max(0.0),
                _ if ri == 0 => row_box.first_x,
                _ => x,
            };
            let baseline = self.baseline(self.y, style, size);
            let mut pen = rx;
            for &(si, s, e) in row {
                let sp = &spans[si];
                let piece: String = sp.text.chars().skip(s).take(e - s).collect();
                if piece.is_empty() {
                    continue;
                }
                let ssize = span_size(sp, size);
                let w = self.fonts.measure(sp.style, &piece, ssize);
                let glyph_baseline = if sp.sup {
                    baseline - size * 0.38
                } else if sp.sub {
                    baseline + size * 0.2
                } else {
                    baseline
                };
                if sp.highlight {
                    let wash = Color::rgba(
                        self.theme.accent.r,
                        self.theme.accent.g,
                        self.theme.accent.b,
                        48,
                    );
                    self.ops.push(DrawOp::Rect {
                        rect: Rect::new(pen - 1.0, self.y + 2.0, w + 2.0, lh - 3.0),
                        color: wash,
                    });
                }
                if sp.code {
                    self.ops.push(DrawOp::RoundedRect {
                        rect: Rect::new(pen - 3.0, self.y + 2.0, w + 6.0, lh - 3.0),
                        radius: 4.0,
                        color: self.theme.code_bg,
                    });
                }
                self.text_op(pen, glyph_baseline, piece, sp.style, ssize, sp.color);
                if sp.strike {
                    let mid = self.strike_y(glyph_baseline, sp.style, ssize);
                    self.ops.push(DrawOp::Line {
                        x0: pen,
                        y0: mid,
                        x1: pen + w,
                        y1: mid,
                        color: sp.color,
                    });
                }
                if sp.underline || sp.link.is_some() {
                    // Underline affordance: links read as clickable, not just
                    // blue, and `<ins>`/`<u>` are underlined in the source.
                    let uy = self.underline_y(glyph_baseline, sp.style, ssize);
                    self.ops.push(DrawOp::Line {
                        x0: pen,
                        y0: uy,
                        x1: pen + w,
                        y1: uy,
                        color: sp.color,
                    });
                }
                if let Some(href) = &sp.link {
                    self.hits.push(HitRegion {
                        rect: Rect::new(pen, self.y, w.max(4.0), lh),
                        action: HitAction::Link(href.clone()),
                    });
                }
                if let Some(note) = sp.footnote {
                    self.hits.push(HitRegion {
                        rect: Rect::new(pen, self.y, w.max(4.0), lh),
                        action: HitAction::FootnoteRef(note),
                    });
                }
                pen += w;
            }
            self.y += lh;
        }
    }

    /// A paragraph that may contain image placeholders: split at images,
    /// laying out text runs and panels in order. Images separated only by
    /// whitespace — `<img>` badges on consecutive lines of a centered `<p>` —
    /// share a row, the way GitHub lays inline images out.
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
        let mut i = 0;
        while i < spans.len() {
            if spans[i].image.is_some() {
                self.flush_run(&mut run, x, max_w, size, style);
                let mut group = vec![i];
                let mut next = i + 1;
                loop {
                    let mut k = next;
                    while k < spans.len() && spans[k].image.is_none() {
                        k += 1;
                    }
                    let whitespace = spans[next..k]
                        .iter()
                        .all(|s| s.image.is_none() && !s.brk && s.text.trim().is_empty());
                    if k < spans.len() && whitespace {
                        group.push(k);
                        next = k + 1;
                    } else {
                        break;
                    }
                }
                if group.len() > 1 {
                    let images: Vec<(ImageSpan, Option<String>)> = group
                        .iter()
                        .map(|&gi| {
                            let span = &spans[gi];
                            (
                                span.image.clone().expect("grouped images"),
                                span.link.clone(),
                            )
                        })
                        .collect();
                    self.render_image_row(&images, x, max_w);
                } else {
                    let image = spans[i].image.clone().expect("an image span");
                    self.render_image(x, max_w, &image, spans[i].link.as_deref());
                }
                i = next;
            } else {
                run.push(spans[i].clone());
                i += 1;
            }
        }
        self.flush_run(&mut run, x, max_w, size, style);
    }

    fn flush_run(&mut self, run: &mut Vec<Span>, x: f32, max_w: f32, size: f32, style: FontStyle) {
        if run.is_empty() {
            return;
        }
        let rows = wrap_spans(self.fonts, run, size, max_w);
        self.emit_rows(
            &rows,
            run,
            RowBox {
                x,
                first_x: x,
                max_w,
            },
            size,
            style,
        );
        run.clear();
    }

    /// A cell or image row's card width: the source's `width` clamped to the
    /// space available, or `auto` when the source states none. `auto` is the
    /// whole column for a standalone image and a thumbnail's width in a table
    /// cell, where the card must share the column with its neighbours.
    fn card_width(image: &ImageSpan, budget: f32, auto: f32) -> f32 {
        image
            .width
            .map_or(auto.min(budget), |w| (w as f32).min(budget).max(80.0))
    }

    /// The height of an image's placeholder card, independent of its width.
    /// `width`/`height` in the source describe the real image: the card grows
    /// to at least the stated height, so a README's side-by-side screenshots
    /// keep the proportions the author chose even though nothing is decoded.
    fn image_card_h(&self, image: &ImageSpan) -> f32 {
        // PNG decoding is a v2 feature, so the placeholder has to earn its
        // space instead: a framed card naming the image and, when the source
        // says one, the alt text.
        let pad = 12.0;
        let small = self.body * 0.8;
        let lh = self.line_h(self.body);
        let lh_small = self.line_h(small);
        let show_src = !image.alt.is_empty();
        let text_h = pad * 2.0 + lh_small + lh + if show_src { lh_small } else { 0.0 };
        text_h.max(image.height.map_or(0.0, |h| (h as f32).min(480.0)))
    }

    /// Paint the card at `top` without moving the flow. A card that came from
    /// an `<a href>` — the badge idiom — is a link in its whole area, because
    /// GitHub's badge is one.
    fn draw_image_card(
        &mut self,
        x: f32,
        top: f32,
        card_w: f32,
        h: f32,
        image: &ImageSpan,
        link: Option<&str>,
    ) {
        self.ops.push(DrawOp::RoundedRect {
            rect: Rect::new(x, top, card_w, h),
            radius: 6.0,
            color: self.theme.rule,
        });
        self.ops.push(DrawOp::RoundedRect {
            rect: Rect::new(x + 1.0, top + 1.0, (card_w - 2.0).max(1.0), h - 2.0),
            radius: 5.0,
            color: self.theme.code_bg,
        });
        let pad = 12.0;
        let small = self.body * 0.8;
        let lh = self.line_h(self.body);
        let lh_small = self.line_h(small);
        let show_src = !image.alt.is_empty();
        let budget = (card_w - 2.0 * pad).max(20.0);
        let mut cy = top + pad;
        let caption = truncate(self.fonts, FontStyle::Regular, small, "image", budget);
        let baseline = self.baseline(cy, FontStyle::Regular, small);
        self.text_op(
            x + pad,
            baseline,
            caption,
            FontStyle::Regular,
            small,
            self.theme.muted,
        );
        cy += lh_small;
        let title = if image.alt.is_empty() {
            &image.src
        } else {
            &image.alt
        };
        let title = truncate(self.fonts, FontStyle::Regular, self.body, title, budget);
        let baseline = self.baseline(cy, FontStyle::Regular, self.body);
        self.text_op(
            x + pad,
            baseline,
            title,
            FontStyle::Regular,
            self.body,
            self.theme.fg,
        );
        cy += lh;
        if show_src {
            let path = truncate(self.fonts, FontStyle::Regular, small, &image.src, budget);
            let baseline = self.baseline(cy, FontStyle::Regular, small);
            self.text_op(
                x + pad,
                baseline,
                path,
                FontStyle::Regular,
                small,
                self.theme.muted,
            );
        }
        if let Some(href) = link {
            self.hits.push(HitRegion {
                rect: Rect::new(x, top, card_w, h),
                action: HitAction::Link(href.to_string()),
            });
        }
    }

    fn render_image(&mut self, x: f32, max_w: f32, image: &ImageSpan, link: Option<&str>) {
        let card_w = Self::card_width(image, max_w, max_w);
        let h = self.image_card_h(image);
        let lh = self.line_h(self.body);
        let x = match image.align.or(self.align) {
            Some(Align::Center) => x + ((max_w - card_w) / 2.0).max(0.0),
            Some(Align::Right) => x + (max_w - card_w).max(0.0),
            _ => x,
        };
        self.draw_image_card(x, self.y, card_w, h, image, link);
        self.y += h + (lh * 0.5).round();
    }

    /// Lay a run of adjacent images side by side — a README's row of badges —
    /// shrinking them to fit the column. The cards stay placeholders; what
    /// changes is that GitHub put them on one line, so this does too.
    fn render_image_row(&mut self, images: &[(ImageSpan, Option<String>)], x: f32, max_w: f32) {
        let gap = 8.0;
        let count = images.len() as f32;
        let mut widths: Vec<f32> = images
            .iter()
            .map(|(image, _)| Self::card_width(image, max_w, 240.0))
            .collect();
        let mut total: f32 = widths.iter().sum::<f32>() + gap * (count - 1.0);
        if total > max_w {
            let room = (max_w - gap * (count - 1.0)).max(40.0 * count);
            let scale = room / widths.iter().sum::<f32>().max(1.0);
            for w in &mut widths {
                *w = (*w * scale).max(40.0);
            }
            total = widths.iter().sum::<f32>() + gap * (count - 1.0);
        }
        let heights: Vec<f32> = images
            .iter()
            .map(|(image, _)| self.image_card_h(image))
            .collect();
        let row_h = heights.iter().copied().fold(0.0, f32::max);
        let mut cx = match self.align {
            Some(Align::Center) => x + ((max_w - total) / 2.0).max(0.0),
            Some(Align::Right) => x + (max_w - total).max(0.0),
            _ => x,
        };
        for (i, (image, link)) in images.iter().enumerate() {
            self.draw_image_card(cx, self.y, widths[i], heights[i], image, link.as_deref());
            cx += widths[i] + gap;
        }
        self.y += row_h + (self.line_h(self.body) * 0.5).round();
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
                Block::Code { lang, text } => {
                    let size = self.body * 0.9;
                    let lh = self.line_h(size);
                    let style = FontStyle::Mono;
                    let lang_opt = lang.as_deref();
                    // Fold a line that is wider than the panel rather than
                    // letting it run out of the column and off the pane: a
                    // command wrapped onto its own row is readable, one that
                    // disappears past the edge is not. A continuation is
                    // indented so it cannot be mistaken for a new line.
                    let inner = (max_w - 2.0 * CODE_PAD).max(40.0);
                    let lang_kind = syntax::Lang::from_hint(lang_opt.unwrap_or(""));
                    let mut highlight = syntax::State::default();
                    // Tint the logical line first, then slice the runs across
                    // its wrapped rows: a string or comment that folds keeps
                    // its color on the continuation instead of restarting as
                    // plain text.
                    let mut rows: Vec<Vec<(String, Color)>> = Vec::new();
                    for line in text.split('\n') {
                        let runs = syntax::runs(line, lang_kind, &mut highlight, self.theme);
                        let mut wrapped = false;
                        for (s, e) in super::text::wrap_ranges(self.fonts, style, size, line, inner)
                        {
                            let mut row = Vec::new();
                            if wrapped {
                                row.push((CODE_HANG.to_string(), self.theme.fg));
                            }
                            row.extend(syntax::slice_runs(&runs, s, e));
                            rows.push(row);
                            wrapped = true;
                        }
                    }
                    // An info string earns a small header strip: the language
                    // named at the top right, over a hairline rule. It frames
                    // the panel as a code block and tells a reader at a glance
                    // what they are looking at.
                    let label = lang_opt.map(str::trim).filter(|s| !s.is_empty());
                    let chip_h = if label.is_some() {
                        (lh * 0.9).round()
                    } else {
                        0.0
                    };
                    let h = chip_h + rows.len() as f32 * lh + 16.0;
                    self.ops.push(DrawOp::RoundedRect {
                        rect: Rect::new(x, self.y, max_w, h),
                        radius: 6.0,
                        color: self.theme.code_bg,
                    });
                    if let Some(label) = label {
                        let chip_size = (size * 0.78).max(1.0);
                        let label = truncate(self.fonts, style, chip_size, label, inner);
                        let lw = self.fonts.measure(style, &label, chip_size);
                        let m = self.fonts.metrics(style, chip_size);
                        let baseline = (self.y + (chip_h - m.height()) / 2.0 + m.ascent).round();
                        self.text_op(
                            x + max_w - CODE_PAD - lw,
                            baseline,
                            label,
                            style,
                            chip_size,
                            self.theme.muted,
                        );
                        let rule_y = (self.y + chip_h).round();
                        self.ops.push(DrawOp::Line {
                            x0: x + CODE_PAD,
                            y0: rule_y,
                            x1: x + max_w - CODE_PAD,
                            y1: rule_y,
                            color: self.theme.rule,
                        });
                    }
                    let mut cy = self.y + chip_h + 8.0;
                    for row in &rows {
                        let baseline = self.baseline(cy, style, size);
                        let mut rx = x + CODE_PAD;
                        for (piece, color) in row {
                            if !piece.is_empty() {
                                self.text_op(rx, baseline, piece.clone(), style, size, *color);
                                rx += self.fonts.measure(style, piece, size);
                            }
                        }
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
                    // The children end with a block gap; the bar stops above
                    // it. `last_gap` is that exact gap, whatever kind of block
                    // the quote ended with.
                    let bottom = self.y - self.last_gap;
                    self.ops.insert(
                        top_op,
                        DrawOp::Rect {
                            rect: Rect::new(bar_x, top, 3.0, (bottom - top).max(4.0)),
                            color: self.theme.quote_bar,
                        },
                    );
                    self.gap(self.body, 0.55);
                }
                Block::Html { align, blocks } => {
                    let previous = self.align;
                    // A container without an `align` of its own — the
                    // `<picture>` inside `<p align="center">` — inherits the
                    // alignment it sits in rather than clearing it.
                    self.align = align.or(previous);
                    self.render_blocks(blocks, x, max_w);
                    self.align = previous;
                }
                Block::Details { summary, blocks } => {
                    let pad = 10.0;
                    let top = self.y;
                    let top_op = self.ops.len();
                    let size = self.body * 0.95;
                    let lh = self.line_h(size);
                    self.y += pad;
                    let baseline = self.baseline(self.y, FontStyle::Regular, size);
                    self.text_op(
                        x + pad,
                        baseline,
                        "▸".into(),
                        FontStyle::Regular,
                        size,
                        self.theme.muted,
                    );
                    self.render_inlines(
                        summary,
                        x + pad * 2.0,
                        (max_w - pad * 3.0).max(40.0),
                        size,
                        FontStyle::Bold,
                    );
                    let bottom = self.y + pad * 0.4;
                    self.ops.insert(
                        top_op,
                        DrawOp::RoundedRect {
                            rect: Rect::new(x, top, max_w, (bottom - top).max(lh)),
                            radius: 6.0,
                            color: self.theme.code_bg,
                        },
                    );
                    self.y = bottom;
                    self.gap(self.body, 0.3);
                    self.render_blocks(blocks, x + 16.0, (max_w - 16.0).max(40.0));
                    self.gap(self.body, 0.55);
                }
                Block::Alert { kind, blocks } => {
                    let (bar, wash) = match kind {
                        AlertKind::Warning | AlertKind::Caution => (
                            self.theme.error,
                            Color::rgba(
                                self.theme.error.r,
                                self.theme.error.g,
                                self.theme.error.b,
                                24,
                            ),
                        ),
                        _ => (
                            self.theme.accent,
                            Color::rgba(
                                self.theme.accent.r,
                                self.theme.accent.g,
                                self.theme.accent.b,
                                24,
                            ),
                        ),
                    };
                    let top = self.y;
                    let top_op = self.ops.len();
                    let size = self.body * 0.9;
                    let lh = self.line_h(size);
                    self.y += 8.0;
                    let baseline = self.baseline(self.y, FontStyle::Bold, size);
                    self.text_op(
                        x + 14.0,
                        baseline,
                        kind.label().to_string(),
                        FontStyle::Bold,
                        size,
                        bar,
                    );
                    self.y += lh + 4.0;
                    self.render_blocks(blocks, x + 14.0, (max_w - 14.0).max(40.0));
                    let bottom = self.y - self.last_gap;
                    self.ops.insert(
                        top_op,
                        DrawOp::Rect {
                            rect: Rect::new(x, top, 3.0, (bottom - top).max(4.0)),
                            color: bar,
                        },
                    );
                    self.ops.insert(
                        top_op,
                        DrawOp::Rect {
                            rect: Rect::new(x, top, max_w, (bottom - top).max(4.0)),
                            color: wash,
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
                Block::FootnoteRule => {
                    self.gap(self.body, 0.55);
                    self.rule(x, max_w);
                    self.gap(self.body, 0.45);
                }
                Block::Footnote { number, blocks } => {
                    self.render_footnote(*number, blocks, x, max_w);
                }
                Block::Rule => {
                    self.gap(self.body, 0.3);
                    self.rule(x, max_w);
                    self.gap(self.body, 0.55);
                }
            }
        }
    }
    /// One numbered note: the marker, the definition's blocks indented past
    /// it, and the return arrow GitHub prints at the end. Both the number and
    /// the arrow jump back to the first reference.
    fn render_footnote(&mut self, number: usize, blocks: &[Block], x: f32, max_w: f32) {
        let size = self.body * 0.92;
        let digits = number.to_string().len() as f32;
        let indent = self.fonts.advance(FontStyle::Regular, '0', size).max(1.0) * (digits + 1.6);
        let marker = format!("{number}.");
        let mw = self.fonts.measure(FontStyle::Regular, &marker, size);
        let lh = self.line_h(size);
        let top = self.y;
        let baseline = self.baseline(top, FontStyle::Regular, size);
        self.text_op(
            x + indent - mw,
            baseline,
            marker,
            FontStyle::Regular,
            size,
            self.theme.muted,
        );
        self.hits.push(HitRegion {
            rect: Rect::new(x + indent - mw, top, mw.max(8.0), lh),
            action: HitAction::FootnoteBack(number),
        });
        let before = self.y;
        let first_op = self.ops.len();
        self.render_blocks(blocks, x + indent, (max_w - indent).max(40.0));
        let arrow = "\u{21A9}";
        let aw = self.fonts.measure(FontStyle::Regular, arrow, size);
        // After the last line of prose when the note ends in a paragraph and
        // the margin has room; a note ending in a panel or a table gets a row
        // of its own, so the arrow never lands inside another block's box.
        let prose = if matches!(blocks.last(), Some(Block::Paragraph(_))) {
            self.last_text_end(first_op)
        } else {
            None
        };
        let (ax, ay) = match prose {
            Some((right, ty, _)) if right + 6.0 + aw <= x + max_w => (right + 6.0, ty),
            _ => {
                // Close the block's trailing gap, then give the arrow a row.
                if self.y > before {
                    self.y -= self.last_gap;
                    self.last_gap = 0.0;
                }
                let row = self.y;
                self.y += lh;
                (x + indent, self.baseline(row, FontStyle::Regular, size))
            }
        };
        self.text_op(
            ax,
            ay,
            arrow.into(),
            FontStyle::Regular,
            size,
            self.theme.accent,
        );
        self.hits.push(HitRegion {
            rect: Rect::new(ax - 2.0, ay - lh * 0.85, aw.max(8.0) + 4.0, lh),
            action: HitAction::FootnoteBack(number),
        });
        // Notes sit as one group, like list items, rather than as a stack of
        // paragraphs.
        if self.y > before {
            let trailing = self.last_gap;
            let tight = (self.line_h(self.body) * 0.16).round();
            self.y = self.y - trailing + tight;
            self.last_gap = tight;
        }
    }

    /// The right edge and baseline of the last text drawn since `from`, the
    /// place a trailing marker can follow without a new row.
    fn last_text_end(&self, from: usize) -> Option<(f32, f32, f32)> {
        self.ops[from..]
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text {
                    x,
                    y,
                    text,
                    size,
                    style,
                    ..
                } => Some((*x + self.fonts.measure(*style, text, *size), *y, *size)),
                _ => None,
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
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
        // Real task boxes, not the source's `[x]` text: the glyph family in
        // `gfx::font::shapes` draws both states, so the marker reads like a
        // checklist at any size and needs no image assets.
        let (marker, marker_color) = match item.checked {
            Some(true) => ("☑ ".to_string(), self.theme.accent),
            Some(false) => ("☐ ".to_string(), self.theme.muted),
            None if ordered => (format!("{num}. "), self.theme.fg),
            None => ("• ".to_string(), self.theme.fg),
        };
        let mw = self.fonts.measure(FontStyle::Regular, &marker, size);
        let content_x = x + indent;
        let content_w = (max_w - indent).max(40.0);
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
        let before = self.y;
        self.render_blocks(&item.blocks, content_x, content_w);
        // A list item is not a paragraph: replace the block gap it ended with
        // by a small inter-item gap, so a list reads as one group instead of a
        // stack of paragraphs.
        if self.y > before {
            let trailing = self.last_gap;
            let tight = (self.line_h(size) * 0.16).round();
            self.y = self.y - trailing + tight;
            self.last_gap = tight;
        }
    }

    fn render_table(
        &mut self,
        align: &[Align],
        head: &[Vec<Inline>],
        rows: &[Vec<Vec<Inline>>],
        x: f32,
        max_w: f32,
    ) {
        // A layout table — the side-by-side screenshots idiom — has no `<th>`
        // and therefore no header row; `align` and the rows still say how
        // many columns there are.
        let n = align
            .len()
            .max(head.len())
            .max(rows.iter().map(Vec::len).max().unwrap_or(0))
            .max(1);
        let size = self.body * 0.92;
        let lh = self.line_h(size);
        let adv = self.fonts.advance(FontStyle::Mono, '0', size).max(1.0);
        let cell_pad = adv * 1.5;
        // Natural single-line widths: the cell's text, or the width of the
        // image it holds. An image column that stated no width gets a
        // thumbnail's share rather than a zero-width column.
        let mut widths = vec![0.0f32; n];
        for (ci, cell) in head.iter().enumerate().take(n) {
            let spans = cell_spans(cell, FontStyle::Bold, self.theme);
            self.measure_cell(&spans, ci, size, max_w, &mut widths);
        }
        for row in rows {
            for (ci, cell) in row.iter().enumerate().take(n) {
                let spans = cell_spans(cell, FontStyle::Regular, self.theme);
                self.measure_cell(&spans, ci, size, max_w, &mut widths);
            }
        }
        let mut natural: f32 = widths.iter().sum::<f32>() + cell_pad * 2.0 * n as f32;
        if natural > max_w {
            // Too wide for the reading column: shrink the columns to fit and
            // let the cells wrap. Clipping every cell to one ellipsized line —
            // what this used to do — quietly dropped the rest of its text.
            let avail = (max_w - cell_pad * 2.0 * n as f32).max(0.0);
            widths = super::fit_columns(&widths, avail, adv * 4.0);
            natural = widths.iter().sum::<f32>() + cell_pad * 2.0 * n as f32;
        }
        // Column origins.
        let mut col_x = Vec::with_capacity(n);
        let mut cx = x;
        for w in &widths {
            col_x.push(cx);
            cx += w + cell_pad * 2.0;
        }
        let table_w = max_w.min(natural);
        let mut top = self.y;
        // Header band: the head row reads as a header, not just bold text.
        if !head.is_empty() {
            let h = self.table_row_h(head, FontStyle::Bold, size, &widths, lh);
            self.ops.push(DrawOp::Rect {
                rect: Rect::new(x, top, table_w, h),
                color: self.theme.code_bg,
            });
            self.render_row(
                head,
                FontStyle::Bold,
                size,
                lh,
                TableRow {
                    widths: &widths,
                    col_x: &col_x,
                    align,
                    top,
                },
            );
            top += h;
            self.y = top;
            self.rule(x, table_w);
        }
        // Alternate body rows get a barely-there tint: with columns measured
        // rather than ruled, a reader's eye otherwise slides sideways across a
        // wide row. The header band stays the strongest fill.
        let zebra = Color::rgba(self.theme.fg.r, self.theme.fg.g, self.theme.fg.b, 8);
        for (ri, row) in rows.iter().enumerate() {
            // Pad short rows with empty cells.
            let mut full = row.clone();
            full.resize_with(n, Default::default);
            let h = self.table_row_h(&full, FontStyle::Regular, size, &widths, lh);
            if ri % 2 == 1 {
                self.ops.push(DrawOp::Rect {
                    rect: Rect::new(x, top, table_w, h),
                    color: zebra,
                });
            }
            self.render_row(
                &full,
                FontStyle::Regular,
                size,
                lh,
                TableRow {
                    widths: &widths,
                    col_x: &col_x,
                    align,
                    top,
                },
            );
            top += h;
        }
        self.y = top;
    }

    /// A cell's contribution to its column's natural width.
    fn measure_cell(&self, spans: &[Span], ci: usize, size: f32, max_w: f32, widths: &mut [f32]) {
        let text: f32 = spans
            .iter()
            .filter(|s| s.image.is_none() && !s.brk)
            .map(|s| self.fonts.measure(s.style, &s.text, size))
            .sum();
        widths[ci] = widths[ci].max(text);
        for image in spans.iter().filter_map(|s| s.image.as_ref()) {
            widths[ci] = widths[ci].max(Self::card_width(image, max_w, 240.0));
        }
    }

    /// A row's height: its tallest cell — the cell's image cards stacked, plus
    /// one line for every visual row its text wraps to at the column's width.
    /// The wrapping [`Self::render_row`] runs, so a row and the one after it
    /// cannot disagree about where the first ends.
    fn table_row_h(
        &self,
        cells: &[Vec<Inline>],
        style: FontStyle,
        size: f32,
        widths: &[f32],
        lh: f32,
    ) -> f32 {
        let mut h = lh;
        for (ci, cell) in cells.iter().enumerate() {
            let spans = cell_spans(cell, style, self.theme);
            let budget = widths.get(ci).copied().unwrap_or(0.0);
            let mut cards = 0.0;
            for image in spans.iter().filter_map(|s| s.image.as_ref()) {
                cards += self.image_card_h(image);
            }
            let text: Vec<Span> = spans.into_iter().filter(|s| s.image.is_none()).collect();
            let lines = if text.iter().all(|s| s.text.trim().is_empty()) {
                0.0
            } else {
                wrap_spans(self.fonts, &text, size, budget).len() as f32
            };
            h = h.max(cards + lines * lh);
        }
        h
    }

    /// Paint one table row with its top at `top`. Images draw as their
    /// placeholder cards — a screenshot cell is an image and nothing else —
    /// and a cell's text follows them on its own line.
    fn render_row(
        &mut self,
        cells: &[Vec<Inline>],
        style: FontStyle,
        size: f32,
        lh: f32,
        row: TableRow<'_>,
    ) {
        let TableRow {
            widths,
            col_x,
            align,
            top,
        } = row;
        let n = widths.len();
        let cell_pad = self.fonts.advance(FontStyle::Mono, '0', size).max(1.0) * 1.5;
        for (ci, cell) in cells.iter().enumerate().take(n) {
            let spans = cell_spans(cell, style, self.theme);
            let budget = widths[ci];
            let cell_align = align.get(ci).copied().unwrap_or(Align::None);
            let mut cy = top;
            for span in spans.iter().filter(|s| s.image.is_some()) {
                let image = span.image.as_ref().expect("an image span");
                let w = Self::card_width(image, budget, budget.min(240.0));
                let h = self.image_card_h(image);
                let ix = match cell_align {
                    Align::Right => col_x[ci] + cell_pad + (budget - w).max(0.0),
                    Align::Center => col_x[ci] + cell_pad + ((budget - w).max(0.0) / 2.0),
                    Align::None | Align::Left => col_x[ci] + cell_pad,
                };
                self.draw_image_card(ix, cy, w, h, image, span.link.as_deref());
                cy += h;
            }
            let text: Vec<Span> = spans.into_iter().filter(|s| s.image.is_none()).collect();
            if text.iter().all(|s| s.text.trim().is_empty()) {
                continue;
            }
            // The cell wraps to its column and the row grows to fit it: a cell
            // used to be laid out as one visual row and cut off at the column
            // edge with an ellipsis, which lost everything past the cut.
            for row in wrap_spans(self.fonts, &text, size, budget) {
                let used: f32 = row
                    .iter()
                    .map(|&(si, s, e)| {
                        let sp = &text[si];
                        self.fonts
                            .measure_range(sp.style, &sp.text, s, e, span_size(sp, size))
                    })
                    .sum();
                let mut ox = match cell_align {
                    Align::Right => col_x[ci] + cell_pad + (budget - used).max(0.0),
                    Align::Center => col_x[ci] + cell_pad + ((budget - used).max(0.0) / 2.0),
                    Align::None | Align::Left => col_x[ci] + cell_pad,
                };
                let baseline = self.baseline(cy, style, size);
                for &(si, s, e) in &row {
                    let sp = &text[si];
                    let piece: String = sp.text.chars().skip(s).take(e - s).collect();
                    if piece.is_empty() {
                        continue;
                    }
                    let ssize = span_size(sp, size);
                    let w = self.fonts.measure(sp.style, &piece, ssize);
                    self.text_op(ox, baseline, piece, sp.style, ssize, sp.color);
                    if sp.link.is_some() {
                        let uy = self.underline_y(baseline, sp.style, ssize);
                        self.ops.push(DrawOp::Line {
                            x0: ox,
                            y0: uy,
                            x1: ox + w,
                            y1: uy,
                            color: sp.color,
                        });
                    }
                    if let Some(href) = &sp.link {
                        self.hits.push(HitRegion {
                            rect: Rect::new(ox, cy, w.max(4.0), lh),
                            action: HitAction::Link(href.clone()),
                        });
                    }
                    ox += w;
                }
                cy += lh;
            }
        }
    }
}

/// One table cell's spans, styled like the rest of the table.
fn cell_spans(cell: &[Inline], style: FontStyle, theme: &Theme) -> Vec<Span> {
    let mut spans = Vec::new();
    spans_of(cell, style, theme.fg, None, theme, &mut spans);
    spans
}

/// A Markdown document as a plan: one entry per top-level block.
///
/// A block's height is measured by rendering it once at y=0 and dropping the
/// ops: unlike a code line, a paragraph's height is whatever its spans wrap
/// to, so it cannot be counted without shaping. What the plan buys is memory
/// and per-frame cost — a 56 000-block document keeps 56 000 `f32`s instead of
/// 787 846 ops — not a cheaper first measure.
pub struct MdPlan {
    pub rows: plan::Rows,
    /// `Md::last_gap` at each block's start. Only an empty quote reads it
    /// before its children overwrite it, and the bar must still be the height
    /// a full layout gives it.
    gaps: Vec<f32>,
}

/// Measure every top-level block, keeping where it starts and nothing else.
pub fn plan(doc: &Document, width: f32, ctx: &Ctx) -> MdPlan {
    let t = ctx.theme;
    let body = ctx.body_size();
    let pad = ctx.px(32.0);
    let (col_w, x0) = super::column(width, ctx);
    let mut tops = Vec::with_capacity(doc.blocks.len());
    let mut gaps = Vec::with_capacity(doc.blocks.len());
    let mut y = pad;
    let mut last_gap = 0.0f32;
    for block in &doc.blocks {
        tops.push(y);
        gaps.push(last_gap);
        let mut md = Md {
            fonts: ctx.fonts,
            theme: t,
            scale: ctx.scale,
            body,
            ops: Vec::new(),
            hits: Vec::new(),
            y: 0.0,
            last_gap,
            align: None,
        };
        md.render_blocks(std::slice::from_ref(block), x0, col_w);
        y += md.y;
        last_gap = md.last_gap;
    }
    MdPlan {
        rows: plan::Rows::new(tops, y + pad, pad),
        gaps,
    }
}

impl MdPlan {
    /// Patch the plan for an edited document: the blocks an edit left alone
    /// keep their measured heights, and only the changed run between the
    /// common prefix and suffix is measured again.
    ///
    /// Markdown still has to be re-parsed (a block list is the parser's
    /// output, not a line index), but not re-measured: shaping every block's
    /// spans is the expensive pass this skips.
    ///
    /// With a run already known from the splice — [`patch_run`] — this walk
    /// is a check, not a derivation: the compare here is for tests that hold
    /// two whole documents, which owe the diff themselves.
    #[cfg(test)]
    pub fn patch(&mut self, old: &Document, new: &Document, width: f32, ctx: &Ctx) -> bool {
        let (o, n) = (&old.blocks, &new.blocks);
        let mut pre = 0;
        while pre < o.len() && pre < n.len() && o[pre] == n[pre] {
            pre += 1;
        }
        let mut suf = 0;
        while suf < o.len() - pre
            && suf < n.len() - pre
            && o[o.len() - 1 - suf] == n[n.len() - 1 - suf]
        {
            suf += 1;
        }
        let run = (pre, o.len() - pre - suf, n.len() - pre - suf);
        self.patch_run(new, width, ctx, run)
    }

    /// The run the splice named: `(prefix, old-run length, new-run length)`.
    /// The splice guarantees the blocks above and below are the old ones
    /// verbatim, so no compare walks the document — the plan re-measures the
    /// re-parsed blocks and splices the row heights.
    pub fn patch_run(
        &mut self,
        new: &Document,
        width: f32,
        ctx: &Ctx,
        (pre, removed, inserted): (usize, usize, usize),
    ) -> bool {
        if removed == 0 && inserted == 0 {
            return true;
        }
        if pre + removed > self.rows.len() || self.gaps.len() != self.rows.len() {
            return false;
        }
        let t = ctx.theme;
        let body = ctx.body_size();
        let (col_w, x0) = super::column(width, ctx);
        // The gap the run starts after; a bar's height in an empty quote reads
        // it, so measuring from the document's own earlier state keeps the
        // patched block the height a full layout gives it.
        let mut last_gap = self.gaps.get(pre).copied().unwrap_or(0.0);
        let mut gaps = Vec::with_capacity(inserted);
        let mut heights = Vec::with_capacity(inserted);
        for block in &new.blocks[pre..pre + inserted] {
            gaps.push(last_gap);
            let mut md = Md {
                fonts: ctx.fonts,
                theme: t,
                scale: ctx.scale,
                body,
                ops: Vec::new(),
                hits: Vec::new(),
                y: 0.0,
                last_gap,
                align: None,
            };
            md.render_blocks(std::slice::from_ref(block), x0, col_w);
            heights.push(md.y);
            last_gap = md.last_gap;
        }
        if !self.rows.splice(pre, removed, &heights) {
            return false;
        }
        self.gaps.splice(pre..pre + removed, gaps);
        // The block after the run starts after the run's trailing gap, which
        // may differ from the one the old block left.
        if pre + inserted < self.gaps.len() {
            self.gaps[pre + inserted] = last_gap;
        }
        true
    }
}

/// Lay out only the blocks that overlap `[top, bottom)`, each at the y the
/// plan carries. Blocks are independent given their start height and gap
/// state, so the band is the full layout with the rest left out.
pub fn layout_band(
    doc: &Document,
    plan: &MdPlan,
    width: f32,
    ctx: &Ctx,
    top: f32,
    bottom: f32,
) -> DisplayList {
    let t = ctx.theme;
    let body = ctx.body_size();
    let (col_w, x0) = super::column(width, ctx);
    let mut md = Md {
        fonts: ctx.fonts,
        theme: t,
        scale: ctx.scale,
        body,
        ops: Vec::new(),
        hits: Vec::new(),
        y: 0.0,
        last_gap: 0.0,
        align: None,
    };
    for i in plan.rows.band(top, bottom) {
        md.y = plan.rows.top(i);
        md.last_gap = plan.gaps.get(i).copied().unwrap_or(0.0);
        md.render_blocks(std::slice::from_ref(&doc.blocks[i]), x0, col_w);
    }
    DisplayList::new(md.ops, plan.rows.height(), md.hits)
}

/// Lay out a parsed Markdown document.
pub fn layout(doc: &Document, width: f32, ctx: &Ctx) -> DisplayList {
    let t = ctx.theme;
    let body = ctx.body_size();
    let pad = ctx.px(32.0);
    // The reading column reflows with the window (see `layout::column`).
    let (col_w, x0) = super::column(width, ctx);
    let mut md = Md {
        fonts: ctx.fonts,
        theme: t,
        scale: ctx.scale,
        body,
        ops: Vec::new(),
        hits: Vec::new(),
        y: pad,
        last_gap: 0.0,
        align: None,
    };
    md.render_blocks(&doc.blocks, x0, col_w);
    let height = md.y + pad;
    DisplayList::new(md.ops, height, md.hits)
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

    /// Every Text op in the list, with its measured right edge, so a test can
    /// ask whether the content actually fits the column.
    fn text_extents(list: &DisplayList, f: &FontSystem) -> Vec<(f32, f32)> {
        list.ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text {
                    x,
                    text,
                    size,
                    style,
                    ..
                } => Some((*x, *x + f.measure(*style, text, *size))),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_long_code_line_folds_inside_the_panel() {
        // A command longer than the panel used to be drawn straight past it and
        // off the pane edge, so the end of the line could not be read at all.
        // It now folds onto continuation rows that stay inside the panel.
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let long = "x".repeat(800);
        let src = format!("```\n{long}\nshort\n```\n");
        let list = layout(&doc(&src), 1920.0, &c);
        let (col_w, x0) = crate::layout::column(1920.0, &c);
        let panel = list
            .ops
            .iter()
            .find_map(|op| match op {
                DrawOp::RoundedRect { rect, .. } => Some(*rect),
                _ => None,
            })
            .expect("code panel");
        assert_eq!(panel.w, col_w, "the panel spans the column");
        let extents = text_extents(&list, &f);
        assert!(
            extents.len() >= 5,
            "an 800-char line must fold into several rows (got {})",
            extents.len()
        );
        for (x, right) in &extents {
            assert!(*x >= x0, "text starts left of the column: {x} < {x0}");
            assert!(
                *right <= panel.right() + 0.5,
                "text runs out of the panel: {right} > {}",
                panel.right()
            );
        }
        // The last row is the short line, still a single row of its own.
        let heights: Vec<f32> = list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::RoundedRect { rect, .. } => Some(rect.h),
                _ => None,
            })
            .collect();
        assert!(heights[0] > 60.0, "panel must grow with the folded rows");
    }

    #[test]
    fn code_panel_tints_keywords_strings_and_comments() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(
            &doc("```rust\nfn main() { // hi\n    let s = \"ok\";\n}\n```\n"),
            800.0,
            &c,
        );
        let mut colors = std::collections::HashSet::new();
        for op in &list.ops {
            if let DrawOp::Text { color, .. } = op {
                colors.insert(color.pack());
            }
        }
        // fg + at least two tint colors (keyword/string/comment/number).
        assert!(
            colors.len() >= 3,
            "code should tint, got {} colors",
            colors.len()
        );
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
            &doc("- a\n  - b\n- [x] done\n- [ ] todo\n\n| h1 | h2 |\n|---|---:|\n| x | 1 |"),
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
        assert!(text.contains('☑'), "checked task box missing: {text}");
        assert!(text.contains('☐'), "unchecked task box missing: {text}");
        assert!(!text.contains("[x]"), "raw task marker leaked: {text}");
        assert!(text.contains("h1"), "table header missing");
        assert!(
            list.ops.iter().any(|op| matches!(op, DrawOp::Line { .. })),
            "table rule missing"
        );
        assert!(list.height > 200.0);
    }

    #[test]
    fn list_items_sit_closer_than_paragraphs() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(&doc("- a\n- b\n- c"), 800.0, &c);
        let mut ys = Vec::new();
        for op in &list.ops {
            if let DrawOp::Text { y, text, .. } = op {
                if matches!(text.as_str(), "a" | "b" | "c") {
                    ys.push(*y);
                }
            }
        }
        ys.sort_by(|a, b| a.total_cmp(b));
        assert_eq!(ys.len(), 3, "expected three item rows: {ys:?}");
        let gap = ys[1] - ys[0];
        let lh = (16.0f32 * 1.55).round();
        assert!(
            gap < lh * 1.35,
            "list items are paragraph-spaced: {gap} for a {lh} line"
        );
    }

    #[test]
    fn fenced_code_names_its_language_and_frames_it() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(&doc("```rust\nfn main() {}\n```\n"), 800.0, &c);
        assert!(
            list.ops.iter().any(|op| matches!(
                op,
                DrawOp::Text { text, color, .. } if text == "rust" && *color == t.muted
            )),
            "language chip missing"
        );
        // The chip sits above a hairline rule inside the panel.
        let rule = list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Line { y0, color, .. } if *color == t.rule => Some(*y0),
                _ => None,
            })
            .count();
        assert!(rule >= 1, "code header rule missing");
    }

    #[test]
    fn an_image_placeholder_names_alt_and_source() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(&doc("![A cat](pictures/cat.png)\n"), 800.0, &c);
        let text: String = list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("A cat"), "alt text missing: {text}");
        assert!(text.contains("pictures/cat.png"), "source missing: {text}");
        // The card is a framed panel, not a bare filled box: an outer rule
        // rect plus the inset background.
        let rounded = list
            .ops
            .iter()
            .filter(|op| matches!(op, DrawOp::RoundedRect { .. }))
            .count();
        assert!(rounded >= 2, "image card frame missing");
    }

    #[test]
    fn a_table_wider_than_the_column_wraps_its_cells() {
        // The report: a table too wide for the reading column had every cell cut
        // off at its column edge with an ellipsis, so most of the text was
        // quietly gone. A cell wraps onto as many rows as it needs now, and its
        // row grows to hold them.
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let cell = "a fairly long description of the alpha row that is wide enough \
                    that the three columns together cannot fit the reading column \
                    at this window width";
        let src = format!(
            "| Name | Description | Notes |\n|---|---|---|\n| alpha | {cell} | first note |\n"
        );
        let list = layout(&doc(&src), 800.0, &c);
        let painted: Vec<&str> = list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            !painted.iter().any(|p| p.contains('…')),
            "a cell was clipped: {painted:?}"
        );
        // A wrap drops the space it broke at, so compare word by word over the
        // pieces in paint order.
        let joined = painted.join(" ");
        let painted: Vec<&str> = joined.split_whitespace().collect();
        for word in cell.split_whitespace().chain(["alpha", "first", "note"]) {
            assert!(
                painted.contains(&word),
                "the word {word:?} never reached the page: {painted:?}"
            );
        }
        // The wrapped rows really are rows: the cell's text sits on more than
        // one baseline.
        let baselines: std::collections::HashSet<i32> = list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, y, .. }
                    if !text.trim().is_empty() && cell.contains(text.trim()) =>
                {
                    Some(*y as i32)
                }
                _ => None,
            })
            .collect();
        assert!(
            baselines.len() > 1,
            "the cell did not wrap onto several rows: {baselines:?}"
        );
        // And every wrapped piece stays inside the reading column.
        let (col_w, x0) = crate::layout::column(800.0, &c);
        for (x, right) in text_extents(&list, &f) {
            assert!(x >= x0 - 0.5, "text starts left of the column: {x}");
            assert!(
                right <= x0 + col_w + 0.5,
                "text runs past the column: {right} at {x}"
            );
        }
    }

    #[test]
    fn a_cell_with_no_breakable_space_still_fits_its_column() {
        // The wrapper hard-breaks a token wider than the column, so a cell
        // holding a long URL wraps rather than running into the column beside
        // it — and wrapping loses none of it.
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let url = "https://example.com/a/very/long/unbreakable/path/that/keeps/going/and/going/until/it/is/wider/than/the/whole/reading/column";
        let src = format!("| Link | Note |\n|---|---|\n| {url} | ok |\n");
        let list = layout(&doc(&src), 800.0, &c);
        let pieces: String = list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, .. } if url.contains(text.as_str()) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .concat();
        assert_eq!(pieces, url, "the cell's text was not all painted");
        let (col_w, x0) = crate::layout::column(800.0, &c);
        for (x, right) in text_extents(&list, &f) {
            assert!(x >= x0 - 0.5, "text starts left of the column: {x}");
            assert!(
                right <= x0 + col_w + 0.5,
                "text runs into the next column: {right}"
            );
        }
    }

    #[test]
    fn table_body_rows_alternate_their_tint() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(&doc("| h |\n|---|\n| a |\n| b |\n| c |\n"), 800.0, &c);
        // One header band plus at least one zebra stripe for the odd row.
        let rects = list
            .ops
            .iter()
            .filter(|op| matches!(op, DrawOp::Rect { .. }))
            .count();
        assert!(rects >= 2, "zebra rows missing: {rects} rects");
    }

    #[test]
    fn short_text_stays_on_one_row() {
        // Regression: rows must not break at the last space when everything fits.
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(&doc("# Editor sample\n"), 1100.0, &c);
        let rows: Vec<String> = list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(rows, vec!["Editor sample".to_string()]);
    }

    #[test]
    fn tables_paint_a_header_band_and_links_underline() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        // Table: a code_bg rect behind the head row plus the separator rule.
        let list = layout(&doc("| h |\n|---|\n| x |\n"), 800.0, &c);
        let rects = list
            .ops
            .iter()
            .filter(|op| matches!(op, DrawOp::Rect { .. }))
            .count();
        assert!(rects >= 1, "header band missing");
        // Link: an accent line just below the text (not a strike mid-line).
        let list = layout(&doc("[t](https://e.com)\n"), 800.0, &c);
        let link_line = list.ops.iter().any(|op| match op {
            DrawOp::Line { color, .. } => *color == t.accent,
            _ => false,
        });
        assert!(link_line, "link underline missing");
    }

    #[test]
    fn decorations_cross_the_glyphs_not_the_line_box() {
        // The report, twice: the strike sat across the tops of the letters and
        // the link underline sat on the baseline, clipping their feet. Both
        // are measured off the glyphs now, so they land on the text.
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(&doc("~~struck~~ and [link](https://e.com)\n"), 800.0, &c);
        let (baseline, size, style) = list
            .ops
            .iter()
            .find_map(|op| match op {
                DrawOp::Text {
                    y,
                    size,
                    style,
                    text,
                    ..
                } if text.contains("struck") => Some((*y, *size, *style)),
                _ => None,
            })
            .expect("the text was laid out");
        let m = f.metrics(style, size);
        let ys: Vec<f32> = list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Line { y0, .. } => Some(*y0),
                _ => None,
            })
            .collect();
        let strike = ys
            .iter()
            .copied()
            .find(|&y| y < baseline)
            .expect("strike-through missing");
        let underline = ys
            .iter()
            .copied()
            .find(|&y| y > baseline)
            .expect("link underline missing");
        let want_strike = baseline - m.ascent * 0.27;
        assert!(
            (strike - want_strike).abs() <= 1.0,
            "strike at {strike}, want {want_strike} (baseline {baseline})"
        );
        let want_underline = baseline - m.descent * 0.75;
        assert!(
            (underline - want_underline).abs() <= 1.0,
            "underline at {underline}, want {want_underline} (baseline {baseline})"
        );
    }

    #[test]
    fn sample_renders_tall() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let src = include_str!("../../samples/sample_markdown.md");
        let list = layout(&doc(src), 900.0, &c);
        assert!(!list.ops.is_empty());
        assert!(!list.hits.is_empty(), "sample link has no hit region");
        assert!(list.height > 400.0);
    }

    #[test]
    fn html_alignment_shifts_rows_within_the_column() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let (col_w, x0) = crate::layout::column(800.0, &c);
        let list = layout(&doc("<div align=\"center\">\nshort\n</div>\n"), 800.0, &c);
        let (x, text) = list
            .ops
            .iter()
            .find_map(|op| match op {
                DrawOp::Text { x, text, .. } if text == "short" => Some((*x, text.clone())),
                _ => None,
            })
            .expect("the centered text was laid out");
        let w = f.measure(FontStyle::Regular, &text, c.body_size());
        let want = x0 + (col_w - w) / 2.0;
        assert!(
            (x - want).abs() <= 1.0,
            "centered text at {x}, want {want} (column {x0}..{})",
            x0 + col_w
        );
    }

    #[test]
    fn ins_mark_and_sup_render_their_decorations() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(&doc("<ins>u</ins> <mark>m</mark> x<sup>2</sup>"), 800.0, &c);
        assert!(
            list.ops.iter().any(|op| matches!(op, DrawOp::Line { .. })),
            "the <ins> rule is missing"
        );
        assert!(
            list.ops.iter().any(|op| matches!(op, DrawOp::Rect { .. })),
            "the <mark> wash is missing"
        );
        // The superscript is drawn smaller than the body text beside it.
        let sizes: Vec<f32> = list
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, size, .. } if text == "2" => Some(*size),
                _ => None,
            })
            .collect();
        assert_eq!(sizes.len(), 1, "superscript text missing");
        assert!(sizes[0] < c.body_size());
    }

    #[test]
    fn an_image_honours_width_and_container_alignment() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let (col_w, x0) = crate::layout::column(800.0, &c);
        let list = layout(
            &doc("<p align=\"center\"><img src=\"a.png\" width=\"200\"></p>"),
            800.0,
            &c,
        );
        let card = list
            .ops
            .iter()
            .find_map(|op| match op {
                DrawOp::RoundedRect { rect, .. } => Some(*rect),
                _ => None,
            })
            .expect("the image card");
        assert_eq!(card.w, 200.0, "the source width was ignored");
        let want = x0 + (col_w - 200.0) / 2.0;
        assert!(
            (card.x - want).abs() <= 1.0,
            "centered card at {}, want {want}",
            card.x
        );
    }

    #[test]
    fn details_and_alerts_render_their_labels_and_bodies() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(
            &doc("<details><summary>More</summary>\n\nBody\n\n</details>"),
            800.0,
            &c,
        );
        let text = text_of(&list);
        assert!(text.contains("More"), "summary missing: {text}");
        assert!(text.contains("Body"), "details body missing: {text}");
        assert!(
            list.ops
                .iter()
                .any(|op| matches!(op, DrawOp::RoundedRect { .. })),
            "summary bar missing"
        );

        let list = layout(&doc("> [!NOTE]\n> body text"), 800.0, &c);
        let text = text_of(&list);
        assert!(text.contains("Note"), "alert label missing: {text}");
        assert!(text.contains("body text"), "alert body missing: {text}");
    }

    #[test]
    fn footnotes_render_numbers_and_a_list_after_the_prose() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(
            &doc("Prose[^b] more[^a].\n\n[^a]: A note.\n[^b]: B note.\n\n[^unused]: Hidden.\n"),
            800.0,
            &c,
        );
        let text = text_of(&list);
        assert!(!text.contains("[^b]"), "reference kept its source: {text}");
        assert!(text.contains("B note") && text.contains("A note"), "{text}");
        assert!(
            !text.contains("Hidden"),
            "unreferenced note printed: {text}"
        );
        assert!(
            text.contains("1.") && text.contains("2."),
            "markers: {text}"
        );
        // Reference numbers are raised and smaller, like the `<sup>` they are.
        let raised = list.ops.iter().any(|op| {
            matches!(op, DrawOp::Text { text, size, .. } if text == "1" && *size < c.body_size())
        });
        assert!(raised, "the reference number was not a superscript: {text}");
        // The section is separated from the prose and sits below it.
        let prose_y = list
            .ops
            .iter()
            .find_map(|op| match op {
                DrawOp::Text { y, text, .. } if text.starts_with("Prose") => Some(*y),
                _ => None,
            })
            .expect("prose");
        let note_y = list
            .ops
            .iter()
            .find_map(|op| match op {
                DrawOp::Text { y, text, .. } if text.contains("B note") => Some(*y),
                _ => None,
            })
            .expect("note body");
        assert!(note_y > prose_y, "notes at {note_y}, prose at {prose_y}");
        assert!(
            list.ops.iter().any(|op| matches!(op, DrawOp::Line { .. })),
            "the separator rule is missing"
        );
    }

    #[test]
    fn footnote_markers_and_notes_carry_jump_hits() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(&doc("Claim[^a].\n\n[^a]: A note.\n"), 800.0, &c);
        let refs: Vec<Rect> = list
            .hits
            .iter()
            .filter_map(|h| match h.action {
                HitAction::FootnoteRef(1) => Some(h.rect),
                _ => None,
            })
            .collect();
        assert_eq!(refs.len(), 1, "the marker is one hit region");
        let backs: Vec<Rect> = list
            .hits
            .iter()
            .filter_map(|h| match h.action {
                HitAction::FootnoteBack(1) => Some(h.rect),
                _ => None,
            })
            .collect();
        assert_eq!(backs.len(), 2, "the number and the arrow both go back");
        assert!(
            backs[0].x < backs[1].x,
            "the number sits in the margin, the arrow after the note"
        );
        assert!(backs[0].y > refs[0].y, "the note sits below its marker");
        // The arrow follows the note's last line of prose instead of taking a
        // row of its own.
        let (note_y, note_x) = list
            .ops
            .iter()
            .find_map(|op| match op {
                DrawOp::Text { x, y, text, .. } if text == "A note." => Some((*y, *x)),
                _ => None,
            })
            .expect("note body");
        let (arrow_y, arrow_x) = list
            .ops
            .iter()
            .find_map(|op| match op {
                DrawOp::Text { x, y, text, .. } if text == "\u{21A9}" => Some((*y, *x)),
                _ => None,
            })
            .expect("return arrow");
        assert!(
            (arrow_y - note_y).abs() < 0.5,
            "the arrow left the note row"
        );
        assert!(arrow_x > note_x, "the arrow did not follow the text");
    }

    #[test]
    fn an_html_table_lays_out_like_a_markdown_one() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(
            &doc("<table>\n<tr><th>Head</th><th>Other</th></tr>\n<tr><td>1</td><td>2</td></tr>\n</table>"),
            800.0,
            &c,
        );
        let text = text_of(&list);
        for want in ["Head", "Other", "1", "2"] {
            assert!(text.contains(want), "cell {want} missing: {text}");
        }
        assert!(
            list.ops.iter().any(|op| matches!(op, DrawOp::Line { .. })),
            "table rule missing"
        );
    }

    fn text_of(list: &DisplayList) -> String {
        list.ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("|")
    }

    /// The outer frame of each image card, in paint order.
    fn image_cards(list: &DisplayList, width: f32) -> Vec<Rect> {
        list.ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::RoundedRect { rect, .. } if (rect.w - width).abs() < 0.5 => Some(*rect),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_html_layout_table_puts_its_images_side_by_side() {
        // The README idiom this round exists for: two screenshots in an HTML
        // table, each `<img width="300">`. The cards used to vanish because
        // table cells only laid out text, and the image row was taken for a
        // header because no row names its cells `<th>`.
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(
            &doc("<table>\n<tr>\n<td><img src=\"light.png\" width=\"300\" alt=\"light\"></td>\n<td><img src=\"dark.png\" width=\"300\" alt=\"dark\"></td>\n</tr>\n<tr>\n<td align=\"center\">Light</td>\n<td align=\"center\">Dark</td>\n</tr>\n</table>"),
            800.0,
            &c,
        );
        let cards = image_cards(&list, 300.0);
        assert_eq!(cards.len(), 2, "both screenshots must draw a card");
        assert_eq!(cards[0].y, cards[1].y, "the cards share a row");
        assert!(
            cards[1].x >= cards[0].right(),
            "the second card starts before the first ends: {:?}",
            cards
        );
        let text = text_of(&list);
        assert!(text.contains("Light") && text.contains("Dark"), "{text}");
    }

    #[test]
    fn a_markdown_table_cell_can_hold_an_image() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(
            &doc("| Left | Right |\n|:-----|------:|\n| ![light](light.png) | ![dark](dark.png) |\n| 1 | 2 |"),
            800.0,
            &c,
        );
        let cards = image_cards(&list, 240.0);
        assert_eq!(cards.len(), 2, "the image row rendered as a blank line");
        assert_eq!(cards[0].y, cards[1].y, "the cards share a row");
        let text = text_of(&list);
        assert!(
            text.contains("1") && text.contains("2"),
            "body lost: {text}"
        );
    }

    #[test]
    fn adjacent_badges_share_a_row_and_stay_links() {
        // `<p align="center">` with one badge `<a><img></a>` per line: GitHub
        // lays them out as one row, and each badge is its link.
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(
            &doc("<p align=\"center\">\n<a href=\"https://e.com/a\"><img src=\"a.png\" width=\"96\" alt=\"a\"></a>\n<a href=\"https://e.com/b\"><img src=\"b.png\" width=\"96\" alt=\"b\"></a>\n</p>"),
            800.0,
            &c,
        );
        let cards = image_cards(&list, 96.0);
        assert_eq!(cards.len(), 2, "badges did not become two cards");
        assert_eq!(
            cards[0].y, cards[1].y,
            "badges stacked instead of sharing a row"
        );
        for href in ["https://e.com/a", "https://e.com/b"] {
            assert!(
                list.hits
                    .iter()
                    .any(|h| matches!(&h.action, HitAction::Link(l) if l == href)),
                "the badge for {href} is not clickable"
            );
        }
        // The row is centred as a whole, like GitHub's centred badge line.
        let (col_w, x0) = crate::layout::column(800.0, &c);
        let left = cards[0].x - x0;
        let right = x0 + col_w - cards[1].right();
        assert!(
            (left - right).abs() <= 2.0,
            "badge row is not centred: {left} left, {right} right"
        );
    }

    #[test]
    fn a_picture_stays_centered_like_the_paragraph_around_it() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let list = layout(
            &doc("<p align=\"center\">\n  <picture>\n    <source media=\"(prefers-color-scheme: dark)\" srcset=\"d.png\">\n    <img src=\"l.png\" width=\"180\" alt=\"Logo\">\n  </picture>\n</p>"),
            800.0,
            &c,
        );
        let cards = image_cards(&list, 180.0);
        assert_eq!(cards.len(), 1, "the fallback image did not render");
        let (col_w, x0) = crate::layout::column(800.0, &c);
        let want = x0 + (col_w - 180.0) / 2.0;
        assert!(
            (cards[0].x - want).abs() <= 1.0,
            "the logo is not centred: {} vs {want}",
            cards[0].x
        );
        assert!(
            !text_of(&list).contains("source"),
            "the <source> tag leaked into the preview"
        );
    }
}

#[cfg(test)]
mod band_tests {
    use super::*;
    use crate::gfx::font::FontSystem;

    fn ctx<'a>(f: &'a FontSystem, t: &'a Theme) -> Ctx<'a> {
        Ctx {
            fonts: f,
            theme: t,
            scale: 1.0,
            zoom: 1.0,
        }
    }

    /// Every block kind the renderer has, repeated enough that a band lands in
    /// the middle of the document.
    fn doc() -> Document {
        let mut s = String::new();
        for i in 0..120 {
            s.push_str(&format!("## Section {i}\n\n"));
            s.push_str(&format!(
                "A paragraph with **bold**, `code`, a [link](https://example.com/{i}) and enough \
                 words to wrap across the reading column at least once, because a wrapped \
                 paragraph is the height case that matters.\n\n"
            ));
            s.push_str("- one\n- two\n  - deeper\n\n");
            s.push_str(&format!("```rust\nfn f{i}() -> u32 {{ {} }}\n```\n\n", i));
            s.push_str("| a | b |\n|---|---|\n| 1 | 2 |\n\n");
            // Every block kind the plan has to measure on its own: a
            // container, a details body, an alert and an image table (which
            // draws cards instead of a text line).
            s.push_str("<div align=\"center\">\n\ncentred **text**\n\n</div>\n\n");
            s.push_str("<details>\n<summary>More</summary>\n\nHidden\n\n</details>\n\n");
            s.push_str("> [!NOTE]\n> alert body\n\n");
            s.push_str(
                "<table>\n<tr><td><img src=\"a.png\" width=\"120\"></td>\
                 <td><img src=\"b.png\" width=\"120\"></td></tr>\n</table>\n\n",
            );
            s.push_str("> quoted text\n\n");
            s.push_str("---\n\n");
        }
        crate::parse::markdown::parse(&s)
    }

    fn all<T: std::fmt::Debug>(items: &[T]) -> Vec<String> {
        items.iter().map(|i| format!("{i:?}")).collect()
    }

    /// A band over the whole document is the full layout: same ops, same hit
    /// regions, same height. The plan measures blocks one at a time, so this
    /// is what proves a block's height and gap do not depend on the document
    /// being rendered around it.
    #[test]
    fn a_whole_document_band_is_the_full_layout() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let d = doc();
        let full = layout(&d, 900.0, &c);
        let plan = plan(&d, 900.0, &c);
        let band = layout_band(&d, &plan, 900.0, &c, f32::NEG_INFINITY, f32::INFINITY);
        assert!((band.height - full.height).abs() < 0.5);
        assert_eq!(all(&band.ops), all(&full.ops), "ops");
        assert_eq!(all(&band.hits), all(&full.hits), "hits");
    }

    #[test]
    fn a_band_claims_nothing_and_misses_nothing_visible() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let d = doc();
        let full = layout(&d, 900.0, &c);
        let plan = plan(&d, 900.0, &c);
        let all_ops = all(&full.ops);
        for top in [0.0, 900.0, 4_000.0, plan.rows.height() - 700.0] {
            let bottom = top + 700.0;
            let band = layout_band(&d, &plan, 900.0, &c, top, bottom);
            let mut seen = 0;
            for s in all(&band.ops) {
                let at = all_ops[seen..]
                    .iter()
                    .position(|a| *a == s)
                    .unwrap_or_else(|| panic!("at {top}: the band invented {s}"));
                seen += at + 1;
            }
            // A full op whose y is strictly inside the band must be painted.
            let got = all(&band.ops);
            for op in &full.ops {
                let y = match op {
                    DrawOp::Text { y, .. } => *y,
                    DrawOp::Rect { rect, .. } | DrawOp::RoundedRect { rect, .. } => rect.y,
                    DrawOp::Line { y0, .. } => *y0,
                };
                if y > top + 80.0 && y < bottom - 80.0 && !got.contains(&format!("{op:?}")) {
                    // Rect/Line ops can belong to a block that started above
                    // the band; only text ink is required to be present.
                    if matches!(op, DrawOp::Text { .. }) {
                        panic!("at {top}: the band missed {op:?}");
                    }
                }
            }
        }
    }

    /// Blocks come from the parser, so an edit re-parses; the *plan* does not
    /// re-measure the blocks the edit left alone. Patching in a paragraph, a
    /// deleted heading and a renamed first heading must land on the index a
    /// fresh plan of the edited document has, gaps included, and a
    /// whole-document band must still be the full layout.
    #[test]
    fn an_edit_patches_the_plan_to_match_a_fresh_one() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let mut base = String::new();
        for i in 0..60 {
            base.push_str(&format!("## Section {i}\n\n"));
            base.push_str(
                "A paragraph with **bold**, `code` and a [link](https://example.com) that is \
                 long enough to wrap across the reading column at least once.\n\n",
            );
            base.push_str(&format!("```rust\nfn f{i}() -> u32 {{ {i} }}\n```\n\n"));
        }
        let before = crate::parse::markdown::parse(&base);
        let mut patched = plan(&before, 900.0, &c);

        let mut edited = base.replacen("## Section 0", "## Renamed", 1);
        edited = edited.replace(
            "## Section 30\n\n",
            "## Section 30\n\nA brand new paragraph that wraps across the reading column \
             and is therefore measured, unlike the blocks around it.\n\n",
        );
        edited = edited.replace("## Section 50\n\n", "");
        let after = crate::parse::markdown::parse(&edited);
        assert_ne!(before, after, "the edit changed the document");
        assert!(patched.patch(&before, &after, 900.0, &c));

        let fresh = plan(&after, 900.0, &c);
        assert_eq!(patched.rows.len(), fresh.rows.len(), "block count");
        for i in 0..fresh.rows.len() {
            assert_eq!(patched.rows.top(i), fresh.rows.top(i), "top {i}");
            assert_eq!(patched.gaps[i], fresh.gaps[i], "gap {i}");
        }
        assert_eq!(patched.rows.height(), fresh.rows.height());

        let full = layout(&after, 900.0, &c);
        let band = layout_band(
            &after,
            &patched,
            900.0,
            &c,
            f32::NEG_INFINITY,
            f32::INFINITY,
        );
        assert_eq!(all(&band.ops), all(&full.ops), "ops after the patch");
        assert_eq!(all(&band.hits), all(&full.hits), "hits after the patch");
    }

    /// An edit that leaves the block list identical (a typo fixed inside a
    /// paragraph still changes its block) patches to the same shape without
    /// touching a neighbour's height.
    #[test]
    fn a_patch_that_changes_one_block_keeps_the_rest_of_the_index() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let base = "one\n\ntwo\n\nthree\n\nfour\n";
        let before = crate::parse::markdown::parse(base);
        let after = crate::parse::markdown::parse(&base.replace("two", "two, edited"));
        let mut patched = plan(&before, 900.0, &c);
        let before_tops: Vec<f32> = (0..before.blocks.len())
            .map(|i| patched.rows.top(i))
            .collect();
        assert!(patched.patch(&before, &after, 900.0, &c));
        let fresh = plan(&after, 900.0, &c);
        assert_eq!(patched.rows.height(), fresh.rows.height());
        // Blocks outside the changed run sit exactly where they did.
        assert_eq!(before_tops[0], patched.rows.top(0));
        assert_eq!(before_tops[3], patched.rows.top(3));
    }
}
