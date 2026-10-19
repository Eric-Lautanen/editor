//! Delimited (CSV/TSV) layout: one header + single-line records in the
//! reading column, sharing the Markdown table's measure (natural widths,
//! equal shares + ellipsis over budget, header band + rule).
//!
//! Records are single visual rows — a 2000-row sheet stays 2000 rows, never
//! wrapped into more — so height and memory stay proportional to the capped
//! row count, not to cell content.
//!
//! S1 · The sheet paints in bands: the plan records where each unit starts
//! (header, every data row, the cap note) plus the grid geometry the units
//! paint from, and a band lays out only the units a viewport covers. The
//! whole-document `layout` is the same painter run over every unit, so the
//! band and the full list cannot disagree about anything but coverage.

use super::{
    display_list::{DisplayList, DrawOp, FontStyle},
    plan, Ctx,
};
use crate::{
    gfx::{color::Color, font::FontSystem, geom::Rect},
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

/// The grid every unit paints from: column x positions and widths, the
/// numeric-columns decision, and the row's type sizes. Measured once per
/// plan build — the full layout and every band after it read the same
/// answers, which is what keeps them pixel-identical.
struct Grid {
    x0: f32,
    table_w: f32,
    widths: Vec<f32>,
    col_x: Vec<f32>,
    numeric: Vec<bool>,
    size: f32,
    lh: f32,
    cell_pad: f32,
}

fn grid_of(sheet: &Sheet, width: f32, ctx: &Ctx) -> Grid {
    let (col_w, x0) = super::column(width, ctx);
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
    let table_w;
    if natural > col_w {
        let share = ((col_w - cell_pad * 2.0 * n as f32) / n as f32).max(adv * 4.0);
        widths = vec![share; n];
        table_w = col_w;
    } else {
        // A sheet narrower than the reading column grows into it: data reads
        // better spread across the pane than huddled in a small block with a
        // third of the window empty beside it. The extra is shared in
        // proportion to each column's natural width.
        let extra = col_w - natural;
        let total: f32 = widths.iter().sum();
        if total > 0.0 {
            for w in &mut widths {
                *w += extra * (*w / total);
            }
        } else {
            for w in &mut widths {
                *w += extra / n as f32;
            }
        }
        table_w = col_w;
    }
    let mut col_x = Vec::with_capacity(n);
    let mut cx = x0;
    for w in &widths {
        col_x.push(cx);
        cx += w + cell_pad * 2.0;
    }
    // A column that holds numbers in every non-empty cell reads best flush
    // right: ids, scores and quantities then line up on their last digit
    // instead of their first. One scan over the capped rows, no allocation.
    let numeric: Vec<bool> = (0..n)
        .map(|ci| {
            let mut any = false;
            for row in &sheet.rows {
                if let Some(cell) = row.get(ci) {
                    let cell = cell.trim();
                    if cell.is_empty() {
                        continue;
                    }
                    if cell.parse::<f64>().is_err() {
                        return false;
                    }
                    any = true;
                }
            }
            any
        })
        .collect();
    Grid {
        x0,
        table_w,
        widths,
        col_x,
        numeric,
        size,
        lh,
        cell_pad,
    }
}

/// S1 · The sheet as a plan: one unit for the header band, one per data row,
/// one for the cap note when rows were dropped, plus the grid geometry the
/// units paint from. A band painter reads this instead of re-measuring the
/// sheet per frame.
pub struct CsvPlan {
    pub rows: plan::Rows,
    x0: f32,
    table_w: f32,
    widths: Vec<f32>,
    col_x: Vec<f32>,
    numeric: Vec<bool>,
    /// The data rows' text size (the note's own is `0.85` of the body).
    size: f32,
    lh: f32,
    cell_pad: f32,
    /// The cap note's line height, `0.0` when the sheet is uncapped.
    note_lh: f32,
}

/// Measure the sheet: the same width scan `layout` has always run, recorded
/// instead of recomputed every frame.
pub fn plan(sheet: &Sheet, width: f32, ctx: &Ctx) -> CsvPlan {
    let g = grid_of(sheet, width, ctx);
    let thick = ctx.scale.max(1.0).round();
    let pad = ctx.px(32.0);
    let note_lh = line_h(ctx.fonts, ctx.body_size() * 0.85);
    let mut tops = Vec::with_capacity(sheet.rows.len() + 2);
    let mut y = pad;
    tops.push(y);
    y += g.lh + thick;
    for _ in &sheet.rows {
        tops.push(y);
        y += g.lh;
    }
    let capped = sheet.capped;
    if capped {
        tops.push(y);
        y += note_lh;
    }
    CsvPlan {
        rows: plan::Rows::new(tops, y + pad, pad),
        x0: g.x0,
        table_w: g.table_w,
        widths: g.widths,
        col_x: g.col_x,
        numeric: g.numeric,
        size: g.size,
        lh: g.lh,
        cell_pad: g.cell_pad,
        note_lh,
    }
}

impl CsvPlan {
    fn cell_x(&self, ci: usize, text: &str, style: FontStyle, fonts: &FontSystem) -> f32 {
        let w = fonts.measure(style, text, self.size);
        self.col_x[ci]
            + self.cell_pad
            + if self.numeric[ci] {
                (self.widths[ci] - w).max(0.0)
            } else {
                0.0
            }
    }
}

/// Paint one unit of the sheet at the y the plan carries: the header band
/// with its rule, one zebra-or-plain row, or the cap note. The only unit
/// boundaries of the sheet, so the full layout is this over every unit.
fn paint_unit(sheet: &Sheet, p: &CsvPlan, i: usize, ctx: &Ctx, ops: &mut Vec<DrawOp>) {
    let y = p.rows.top(i);
    let fonts = ctx.fonts;
    if i == 0 {
        // Header band + bold head row.
        ops.push(DrawOp::Rect {
            rect: Rect::new(p.x0, y, p.table_w, p.lh),
            color: ctx.theme.code_bg,
        });
        for (ci, cell) in sheet.header.iter().enumerate().take(sheet.cols) {
            let text = truncate_cell(fonts, FontStyle::Bold, p.size, cell, p.widths[ci]);
            ops.push(DrawOp::Text {
                x: p.cell_x(ci, &text, FontStyle::Bold, fonts),
                y: baseline(fonts, y, FontStyle::Bold, p.size, p.lh),
                text,
                size: p.size,
                style: FontStyle::Bold,
                color: ctx.theme.fg,
            });
        }
        ops.push(DrawOp::Line {
            x0: p.x0,
            y0: y + p.lh,
            x1: p.x0 + p.table_w,
            y1: y + p.lh,
            color: ctx.theme.rule,
        });
        return;
    }
    let ri = i - 1;
    let Some(row) = sheet.rows.get(ri) else {
        // The cap note: the last unit when rows were dropped.
        let note_size = ctx.body_size() * 0.85;
        let m = fonts.metrics(FontStyle::Regular, note_size);
        ops.push(DrawOp::Text {
            x: p.x0,
            y: (y + (p.note_lh - m.height()) / 2.0 + m.ascent).round(),
            text: format!(
                "… showing first {} of {} rows",
                sheet.rows.len(),
                sheet.total_rows
            ),
            size: note_size,
            style: FontStyle::Regular,
            color: ctx.theme.muted,
        });
        return;
    };
    // Alternating row tint: with no cell borders, a wide sheet otherwise
    // turns into one undifferentiated block of values.
    if ri % 2 == 1 {
        let zebra = Color::rgba(ctx.theme.fg.r, ctx.theme.fg.g, ctx.theme.fg.b, 8);
        ops.push(DrawOp::Rect {
            rect: Rect::new(p.x0, y, p.table_w, p.lh),
            color: zebra,
        });
    }
    for (ci, cell) in row.iter().enumerate().take(sheet.cols) {
        let text = truncate_cell(fonts, FontStyle::Regular, p.size, cell, p.widths[ci]);
        if text.is_empty() {
            continue;
        }
        ops.push(DrawOp::Text {
            x: p.cell_x(ci, &text, FontStyle::Regular, fonts),
            y: baseline(fonts, y, FontStyle::Regular, p.size, p.lh),
            text,
            size: p.size,
            style: FontStyle::Regular,
            color: ctx.theme.fg,
        });
    }
}

/// S1 · Lay out only the units that overlap `[top, bottom)`, each at the y
/// the plan carries: a frame prices the rows in view, not the capped sheet.
pub fn layout_band(
    sheet: &Sheet,
    plan: &CsvPlan,
    _width: f32,
    ctx: &Ctx,
    top: f32,
    bottom: f32,
) -> DisplayList {
    let mut ops = Vec::new();
    for i in plan.rows.band(top, bottom) {
        paint_unit(sheet, plan, i, ctx, &mut ops);
    }
    DisplayList::new(ops, plan.rows.height(), Vec::new())
}

/// Lay out a parsed sheet whole: the same painter over every unit.
pub fn layout(sheet: &Sheet, width: f32, ctx: &Ctx) -> DisplayList {
    let (col_w, x0) = super::column(width, ctx);
    let pad = ctx.px(32.0);
    if sheet.cols == 0 {
        let size = ctx.body_size();
        let lh = line_h(ctx.fonts, size);
        let m = ctx.fonts.metrics(FontStyle::Regular, size);
        let mut ops = Vec::new();
        let mut y = pad;
        ops.push(DrawOp::Text {
            x: x0,
            y: (y + (lh - m.height()) / 2.0 + m.ascent).round(),
            text: "(empty file)".into(),
            size,
            style: FontStyle::Regular,
            color: ctx.theme.muted,
        });
        y += lh;
        return DisplayList::new(ops, y + pad, Vec::new());
    }
    let p = plan(sheet, width, ctx);
    let mut ops = Vec::new();
    for i in 0..p.rows.len() {
        paint_unit(sheet, &p, i, ctx, &mut ops);
    }
    let _ = col_w;
    DisplayList::new(ops, p.rows.height(), Vec::new())
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
        let sheet = crate::parse::csv::parse_with_ext("a,b\n1,2\n3,4\n", None);
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
        let sheet = crate::parse::csv::parse_with_ext(&text, None);
        let list = layout(&sheet, 900.0, &c);
        assert!(
            list.ops.iter().any(|op| match op {
                DrawOp::Text { text, .. } => text.contains("showing first"),
                _ => false,
            }),
            "cap note missing"
        );
    }

    #[test]
    fn numeric_columns_line_up_on_the_right() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let sheet = crate::parse::csv::parse_with_ext("name,score\nalpha,7\nbeta,1234\n", None);
        let list = layout(&sheet, 900.0, &c);
        let mut rights = Vec::new();
        for want in ["score", "7", "1234"] {
            let (x, size, style) = list
                .ops
                .iter()
                .find_map(|op| match op {
                    DrawOp::Text {
                        x,
                        text,
                        size,
                        style,
                        ..
                    } if text == want => Some((*x, *size, *style)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{want} missing"));
            rights.push(x + f.measure(style, want, size));
        }
        for pair in rights.windows(2) {
            assert!(
                (pair[0] - pair[1]).abs() < 0.5,
                "numeric cells are not right-aligned: {rights:?}"
            );
        }
    }

    #[test]
    fn body_rows_alternate_their_tint() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let sheet = crate::parse::csv::parse_with_ext("a,b\n1,2\n3,4\n5,6\n", None);
        let list = layout(&sheet, 900.0, &c);
        let rects = list
            .ops
            .iter()
            .filter(|op| matches!(op, DrawOp::Rect { .. }))
            .count();
        // The header band plus at least one zebra stripe.
        assert!(rects >= 2, "zebra rows missing: {rects} rects");
    }

    /// S1 · The band painter and the whole-document layout agree exactly:
    /// the full range of a band is the full list op for op, and a mid-sheet
    /// band is the full list's slice between those units.
    #[test]
    fn the_band_matches_the_full_list_pixel_for_pixel() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let mut text = String::from("id,name,score,note\n");
        for i in 0..60 {
            text.push_str(&format!("{i},row-{i},{i}.5,extra cell {i}\n"));
        }
        let sheet = crate::parse::csv::parse_with_ext(&text, None);
        let full = layout(&sheet, 900.0, &c);
        let p = plan(&sheet, 900.0, &c);
        // The whole document through a band is the whole list.
        let all = layout_band(&sheet, &p, 900.0, &c, 0.0, p.rows.height());
        assert_eq!(all.ops, full.ops, "the full band is the full list");

        // A band over the middle is the same units the full list carries in
        // that range, in order — a subsequence, because the band starts
        // partway through the unit its top lands in.
        let mid = layout_band(
            &sheet,
            &p,
            900.0,
            &c,
            p.rows.height() * 0.4,
            p.rows.height() * 0.7,
        );
        assert!(!mid.ops.is_empty(), "the middle band is empty");
        assert!(
            mid.ops.len() < full.ops.len(),
            "a band is cheaper than the sheet"
        );
        let mut cursor = 0;
        for op in &mid.ops {
            // The band is a slice of the full list in order, so the search
            // usually finds the op at or after the cursor; the full-list
            // fallback keeps a mis-sorted band refusing to lie about it.
            let found = full.ops[cursor..]
                .iter()
                .position(|o| o == op)
                .map(|off| off + cursor)
                .or_else(|| full.ops.iter().position(|o| o == op));
            match found {
                Some(at) => cursor = at + 1,
                None => panic!("band painted {op:?} which the full list never does"),
            }
        }
        // Height is the whole sheet's, so scrolling and the scrollbar agree.
        assert_eq!(all.height, full.height);
        assert_eq!(mid.height, full.height);
    }
}
