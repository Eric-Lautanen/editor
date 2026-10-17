//! Delimited (CSV/TSV) layout: one header + single-line records in the
//! reading column, sharing the Markdown table's measure (natural widths,
//! equal shares + ellipsis over budget, header band + rule).
//!
//! Records are single visual rows — a 2000-row sheet stays 2000 rows, never
//! wrapped into more — so height and memory stay proportional to the capped
//! row count, not to cell content.

use super::{
    display_list::{DisplayList, DrawOp, FontStyle},
    Ctx,
};
use crate::{
    gfx::{font::FontSystem, geom::Rect},
    parse::csv::Sheet,
};

fn line_h(_fonts: &FontSystem, size: f32) -> f32 {
    (size * 1.55).round()
}

fn baseline(fonts: &FontSystem, top: f32, style: FontStyle, size: f32, lh: f32) -> f32 {
    let m = fonts.metrics(style, size);
    (top + (lh - m.height()) / 2.0 + m.ascent).round()
}

fn truncate_cell(
    fonts: &FontSystem,
    style: FontStyle,
    size: f32,
    text: &str,
    budget: f32,
) -> String {
    if fonts.measure(style, text, size) <= budget {
        return text.to_string();
    }
    // Accumulate the kept prefix once: popping a char and re-measuring the
    // whole cell each time is quadratic in the cell length, and every capped
    // sheet runs this over every cell it paints.
    let ell = fonts.measure(style, "…", size);
    let mut s = String::new();
    let mut w = 0.0;
    for ch in text.chars() {
        let cw = fonts.advance(style, ch, size);
        if w + cw + ell > budget {
            break;
        }
        w += cw;
        s.push(ch);
    }
    s.push('…');
    s
}

pub fn layout(sheet: &Sheet, width: f32, ctx: &Ctx) -> DisplayList {
    let (col_w, x0) = super::column(width, ctx);
    let pad = ctx.px(32.0);
    let mut ops = Vec::new();
    let mut y = pad;
    if sheet.cols == 0 {
        let size = ctx.body_size();
        let lh = line_h(ctx.fonts, size);
        let m = ctx.fonts.metrics(FontStyle::Regular, size);
        ops.push(DrawOp::Text {
            x: x0,
            y: (y + (lh - m.height()) / 2.0 + m.ascent).round(),
            text: "(empty file)".into(),
            size,
            style: FontStyle::Regular,
            color: ctx.theme.muted,
        });
        y += lh;
        return DisplayList {
            ops,
            height: y + pad,
            hits: Vec::new(),
        };
    }
    let n = sheet.cols;
    let size = ctx.body_size() * 0.92;
    let lh = line_h(ctx.fonts, size);
    let adv = ctx.fonts.advance(FontStyle::Mono, '0', size).max(1.0);
    let cell_pad = adv * 1.5;
    let mut widths = vec![0.0f32; n];
    for (ci, cell) in sheet.header.iter().enumerate().take(n) {
        widths[ci] = widths[ci].max(ctx.fonts.measure(FontStyle::Bold, cell, size));
    }
    for row in &sheet.rows {
        for (ci, cell) in row.iter().enumerate().take(n) {
            widths[ci] = widths[ci].max(ctx.fonts.measure(FontStyle::Regular, cell, size));
        }
    }
    let natural: f32 = widths.iter().sum::<f32>() + cell_pad * 2.0 * n as f32;
    if natural > col_w {
        let share = ((col_w - cell_pad * 2.0 * n as f32) / n as f32).max(adv * 4.0);
        widths = vec![share; n];
    }
    let table_w = col_w.min(natural);
    let mut col_x = Vec::with_capacity(n);
    let mut cx = x0;
    for w in &widths {
        col_x.push(cx);
        cx += w + cell_pad * 2.0;
    }
    // Header band + bold head row.
    ops.push(DrawOp::Rect {
        rect: Rect::new(x0, y, table_w, lh),
        color: ctx.theme.code_bg,
    });
    for (ci, cell) in sheet.header.iter().enumerate().take(n) {
        let text = truncate_cell(ctx.fonts, FontStyle::Bold, size, cell, widths[ci]);
        ops.push(DrawOp::Text {
            x: col_x[ci] + cell_pad,
            y: baseline(ctx.fonts, y, FontStyle::Bold, size, lh),
            text,
            size,
            style: FontStyle::Bold,
            color: ctx.theme.fg,
        });
    }
    y += lh;
    let thick = ctx.scale.max(1.0).round();
    ops.push(DrawOp::Line {
        x0,
        y0: y,
        x1: x0 + table_w,
        y1: y,
        color: ctx.theme.rule,
    });
    y += thick;
    for row in &sheet.rows {
        for (ci, cell) in row.iter().enumerate().take(n) {
            let text = truncate_cell(ctx.fonts, FontStyle::Regular, size, cell, widths[ci]);
            if text.is_empty() {
                continue;
            }
            ops.push(DrawOp::Text {
                x: col_x[ci] + cell_pad,
                y: baseline(ctx.fonts, y, FontStyle::Regular, size, lh),
                text,
                size,
                style: FontStyle::Regular,
                color: ctx.theme.fg,
            });
        }
        y += lh;
    }
    if sheet.capped {
        let size = ctx.body_size() * 0.85;
        let lh = line_h(ctx.fonts, size);
        let m = ctx.fonts.metrics(FontStyle::Regular, size);
        ops.push(DrawOp::Text {
            x: x0,
            y: (y + (lh - m.height()) / 2.0 + m.ascent).round(),
            text: format!(
                "… showing first {} of {} rows",
                sheet.rows.len(),
                sheet.total_rows
            ),
            size,
            style: FontStyle::Regular,
            color: ctx.theme.muted,
        });
        y += lh;
    }
    DisplayList {
        ops,
        height: y + pad,
        hits: Vec::new(),
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
    fn table_fits_the_column_with_a_header_band() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let sheet = crate::parse::csv::parse("a,b\n1,2\n3,4\n");
        let list = layout(&sheet, 900.0, &c);
        let (col_w, x0) = crate::layout::column(900.0, &c);
        assert!(
            list.ops.iter().any(|op| matches!(op, DrawOp::Rect { .. })),
            "header band missing"
        );
        for op in &list.ops {
            if let DrawOp::Text {
                x,
                text,
                size,
                style,
                ..
            } = op
            {
                let right = x + f.measure(*style, text, *size);
                assert!(
                    right <= x0 + col_w + 0.5,
                    "cell runs past the column: {right}"
                );
            }
        }
    }

    #[test]
    fn capped_sheets_say_so() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let mut text = String::from("h\n");
        for i in 0..crate::parse::csv::MAX_ROWS + 5 {
            text.push_str(&format!("{i}\n"));
        }
        let sheet = crate::parse::csv::parse(&text);
        let list = layout(&sheet, 900.0, &c);
        assert!(
            list.ops.iter().any(|op| match op {
                DrawOp::Text { text, .. } => text.contains("showing first"),
                _ => false,
            }),
            "cap note missing"
        );
    }
}
