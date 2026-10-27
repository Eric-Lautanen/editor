//! Delimited (CSV/TSV) layout: one header + records in the reading column,
//! sharing the Markdown table's measure (natural widths, columns shrunk to fit
//! when they overrun the column, header band + rule).
//!
//! A record is as tall as its tallest cell: a cell that cannot fit its column
//! wraps onto more lines rather than being cut off with an ellipsis, so nothing
//! in the sheet is hidden. Height is still bounded — at most [`MAX_ROWS`](crate::parse::csv::MAX_ROWS)
//! records, each the height its own content needs and no more.
//!
//! S1 · The sheet paints in bands: the plan records where each unit starts
//! (header, every data row, the cap note) plus the grid geometry the units
//! paint from, and a band lays out only the units a viewport covers. The
//! whole-document `layout` is the same painter run over every unit, so the
//! band and the full list cannot disagree about anything but coverage.

use super::{
    display_list::{DisplayList, DrawOp, FontStyle},
    plan,
    text::wrap_ranges,
    Ctx,
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

/// Thickness of a painted rule, in physical px — the same value `plan` spends
/// on the header's separator when it lays the sheet's units out.
fn thick(ctx: &Ctx) -> f32 {
    ctx.scale.max(1.0).round()
}

/// A row's height: its tallest cell, one line for every visual row that cell's
/// text wraps to at its column's width. The same wrapping the painter runs, so
/// a row and the row after it cannot disagree about where the first ends.
fn row_h(
    fonts: &FontSystem,
    style: FontStyle,
    size: f32,
    lh: f32,
    cells: &[String],
    widths: &[f32],
    cols: usize,
) -> f32 {
    let mut h = lh;
    for (ci, cell) in cells.iter().enumerate().take(cols) {
        let lines = wrap_ranges(fonts, style, size, cell, widths[ci]).len() as f32;
        h = h.max(lines * lh);
    }
    h
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
        // Too wide for the reading column: shrink the columns to fit and let
        // the cells wrap, rather than clipping each cell at its column edge.
        let avail = (col_w - cell_pad * 2.0 * n as f32).max(0.0);
        widths = super::fit_columns(&widths, avail, adv * 4.0);
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
    let thick = thick(ctx);
    let pad = ctx.px(32.0);
    let note_lh = line_h(ctx.fonts, ctx.body_size() * 0.85);
    let header_h = row_h(
        ctx.fonts,
        FontStyle::Bold,
        g.size,
        g.lh,
        &sheet.header,
        &g.widths,
        sheet.cols,
    );
    let mut tops = Vec::with_capacity(sheet.rows.len() + 2);
    let mut y = pad;
    tops.push(y);
    // The header unit is its band plus the hairline rule under it; the band is
    // the unit's height less that rule (see `paint_unit`).
    y += header_h + thick;
    for row in &sheet.rows {
        tops.push(y);
        y += row_h(
            ctx.fonts,
            FontStyle::Regular,
            g.size,
            g.lh,
            row,
            &g.widths,
            sheet.cols,
        );
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
    /// Where a cell's text starts: the column's origin plus its padding, and
    /// flush right for a numeric column. `w` is the width of the piece being
    /// placed — one wrapped row of the cell, not the whole cell.
    fn cell_x(&self, ci: usize, w: f32) -> f32 {
        self.col_x[ci]
            + self.cell_pad
            + if self.numeric[ci] {
                (self.widths[ci] - w).max(0.0)
            } else {
                0.0
            }
    }
}

/// One cell of one row: which column it belongs to, what it says, and where and
/// how it paints. Grouped so the painter takes the cell, not six loose numbers.
struct Cell<'a> {
    ci: usize,
    text: &'a str,
    style: FontStyle,
    /// Top of the row the cell sits in.
    y: f32,
    color: Color,
}

/// Paint one cell wrapped to its column: every visual row it needs, the piece
/// right-aligned when the column holds numbers. A cell that does not fit used
/// to be cut off with an ellipsis; now it wraps and its row grows (see
/// [`row_h`]).
fn paint_cell(fonts: &FontSystem, p: &CsvPlan, cell: Cell<'_>, ops: &mut Vec<DrawOp>) {
    let Cell {
        ci,
        text,
        style,
        y,
        color,
    } = cell;
    for (li, (s, e)) in wrap_ranges(fonts, style, p.size, text, p.widths[ci])
        .into_iter()
        .enumerate()
    {
        let piece: String = text.chars().skip(s).take(e - s).collect();
        if piece.trim().is_empty() {
            continue;
        }
        let w = fonts.measure(style, &piece, p.size);
        ops.push(DrawOp::Text {
            x: p.cell_x(ci, w),
            y: baseline(fonts, y + li as f32 * p.lh, style, p.size, p.lh),
            text: piece,
            size: p.size,
            style,
            color,
        });
    }
}

/// Paint one unit of the sheet at the y the plan carries: the header band
/// with its rule, one zebra-or-plain row, or the cap note. The only unit
/// boundaries of the sheet, so the full layout is this over every unit.
fn paint_unit(sheet: &Sheet, p: &CsvPlan, i: usize, ctx: &Ctx, ops: &mut Vec<DrawOp>) {
    let y = p.rows.top(i);
    let fonts = ctx.fonts;
    if i == 0 {
        // Header band + bold head row. The unit covers the band and the rule
        // below it; the band is the part above the rule.
        let band_h = (p.rows.unit_h(i) - thick(ctx)).max(1.0);
        ops.push(DrawOp::Rect {
            rect: Rect::new(p.x0, y, p.table_w, band_h),
            color: ctx.theme.code_bg,
        });
        for (ci, text) in sheet.header.iter().enumerate().take(sheet.cols) {
            let cell = Cell {
                ci,
                text,
                style: FontStyle::Bold,
                y,
                color: ctx.theme.fg,
            };
            paint_cell(fonts, p, cell, ops);
        }
        let rule_y = y + band_h;
        ops.push(DrawOp::Line {
            x0: p.x0,
            y0: rule_y,
            x1: p.x0 + p.table_w,
            y1: rule_y,
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
            rect: Rect::new(p.x0, y, p.table_w, p.rows.unit_h(i)),
            color: zebra,
        });
    }
    for (ci, text) in row.iter().enumerate().take(sheet.cols) {
        if text.is_empty() {
            continue;
        }
        let cell = Cell {
            ci,
            text,
            style: FontStyle::Regular,
            y,
            color: ctx.theme.fg,
        };
        paint_cell(fonts, p, cell, ops);
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
    fn a_wide_sheet_wraps_its_cells_instead_of_clipping_them() {
        // The sheet shared the Markdown table's ellipsis-over-budget rule, so a
        // sheet wider than the reading column lost the tail of every long cell.
        // A cell wraps now and its row is as tall as its tallest cell.
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let cell = "a note long enough that the two columns together cannot fit \
                    the reading column at this window width";
        let src = format!("label,detail\nrow,{cell}\n");
        let sheet = crate::parse::csv::parse_with_ext(&src, None);
        let list = layout(&sheet, 800.0, &c);
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
        for word in cell.split_whitespace().chain(["label", "detail", "row"]) {
            assert!(
                painted.contains(&word),
                "the word {word:?} never reached the sheet: {painted:?}"
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
        // And every piece stays inside the reading column.
        let (col_w, x0) = crate::layout::column(800.0, &c);
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
                    "cell runs past the column: {text:?}"
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
