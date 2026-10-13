//! F1 help overlay: the whole legend — modes, shortcuts, file types, how to
//! select and copy — painted over the document. Any click or Escape closes it.

use crate::{
    gfx::{
        canvas::{Canvas, TextSpec},
        font::{FontStyle, FontSystem},
    },
    layout::theme::Theme,
};

use super::overlay;

/// `(section, &[(key, description)])`
const SECTIONS: &[(&str, &[(&str, &str)])] = &[
    ("Modes", &[("Ctrl+E", "cycle Preview → Edit → Split")]),
    (
        "Files",
        &[
            ("Ctrl+O", "open a file in a new tab"),
            ("drag & drop", "drop files anywhere to open them"),
            ("Ctrl+Tab", "next tab (Shift = previous)"),
            ("Ctrl+W", "close tab (press twice if unsaved)"),
            ("Ctrl+S", "save"),
        ],
    ),
    (
        "Editing  (Edit / Split)",
        &[
            ("type", "edit the document"),
            ("arrows", "move the caret (Shift = select)"),
            ("mouse drag", "select text"),
            ("Ctrl+A", "select all"),
            ("Ctrl+C / X / V", "copy / cut / paste"),
            ("Ctrl+Z / Y", "undo / redo"),
            ("Tab", "insert four spaces"),
        ],
    ),
    (
        "View",
        &[
            ("wheel, arrows", "scroll"),
            ("Ctrl + / - / 0", "zoom in / out / reset"),
            ("Ctrl+T", "light / dark theme"),
            ("click [+]", "fold / unfold JSON nodes"),
            ("click a link", "show its address in the status bar"),
        ],
    ),
    (
        "App",
        &[
            ("F1", "this help"),
            ("F2", "about readitor"),
            ("right-click", "context menu"),
            ("Ctrl+Q", "quit"),
        ],
    ),
];

const FOOTER: &[&str] = &[
    "Preview renders Markdown documents, JSON as a collapsible tree, JSONL as",
    "per-line records, and text/log files wrapped. Selecting and copying text",
    "happens in Edit or Split (Ctrl+E); in Preview use right-click → Copy all.",
];

/// One row of the legend, in paint order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    /// Section heading, by index into `SECTIONS`.
    Heading(usize),
    /// An item: `(section, item)`.
    Item(usize, usize),
    /// Footer line, by index into `FOOTER`.
    Footer(usize),
}

/// Blank space above every heading after the first, and above the footer.
///
/// Without it a heading sits flush against the previous section's last row and
/// reads as though it belongs to the section above it.
fn section_gap(scale: f32) -> f32 {
    (overlay::metrics(scale).0 * 0.95).round()
}

/// Top edge of every row relative to the panel's top edge, in paint order,
/// plus the content height.
///
/// Kept separate from painting so the spacing can be asserted directly rather
/// than by reading pixels back.
fn layout(scale: f32) -> (Vec<(Row, f32)>, f32) {
    let (_, line_h) = overlay::metrics(scale);
    let gap = section_gap(scale);
    let mut out = Vec::new();
    let mut y = overlay::pad(scale);
    for (si, &(_, items)) in SECTIONS.iter().enumerate() {
        if si > 0 {
            y += gap;
        }
        out.push((Row::Heading(si), y));
        y += line_h;
        for ii in 0..items.len() {
            out.push((Row::Item(si, ii), y));
            y += line_h;
        }
    }
    y += gap;
    for fi in 0..FOOTER.len() {
        out.push((Row::Footer(fi), y));
        y += line_h;
    }
    (out, y)
}

/// Paint the overlay over the whole window, returning the panel it drew.
pub fn paint(
    canvas: &mut Canvas,
    fonts: &FontSystem,
    theme: &Theme,
    width: f32,
    height: f32,
    scale: f32,
) -> overlay::Frame {
    let (size, _) = overlay::metrics(scale);
    let pad = overlay::pad(scale);
    let col_gap = (size * 1.6).round();

    // Size the columns from the widest strings rather than a magic number:
    // a fixed 110px key column left "Ctrl+C / X / V" flush against its
    // description, with a single pixel to spare.
    let key_w = SECTIONS
        .iter()
        .flat_map(|(_, items)| items.iter().map(|(k, _)| *k))
        .map(|k| fonts.measure(FontStyle::Mono, k, size))
        .fold(0.0_f32, f32::max);
    let desc_w = SECTIONS
        .iter()
        .flat_map(|(_, items)| items.iter().map(|(_, d)| *d))
        .map(|d| fonts.measure(FontStyle::Regular, d, size))
        .fold(0.0_f32, f32::max);
    let foot_w = FOOTER
        .iter()
        .map(|l| fonts.measure(FontStyle::Regular, l, size))
        .fold(0.0_f32, f32::max);

    let content_w = (key_w + col_gap + desc_w).max(foot_w);
    let panel_w = (content_w + 2.0 * pad).min(width - 40.0 * scale);
    let (rows, content_h) = layout(scale);
    let f = overlay::frame(
        canvas,
        fonts,
        theme,
        (width, height),
        (panel_w, content_h + pad),
        scale,
    );

    let x = f.content_x();
    let desc_x = x + key_w + col_gap;
    for &(row, top) in &rows {
        let y = f.baseline(f.panel.y + top);
        let spec = |style, color| TextSpec {
            size: f.size,
            style,
            color,
        };
        match row {
            Row::Heading(si) => {
                canvas.draw_text(
                    fonts,
                    SECTIONS[si].0,
                    x,
                    y,
                    spec(FontStyle::Bold, theme.accent),
                );
            }
            Row::Item(si, ii) => {
                let (key, desc) = SECTIONS[si].1[ii];
                canvas.draw_text(fonts, key, x, y, spec(FontStyle::Mono, theme.fg));
                canvas.draw_text(
                    fonts,
                    desc,
                    desc_x,
                    y,
                    spec(FontStyle::Regular, theme.muted),
                );
            }
            Row::Footer(fi) => {
                canvas.draw_text(
                    fonts,
                    FOOTER[fi],
                    x,
                    y,
                    spec(FontStyle::Regular, theme.muted),
                );
            }
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::{canvas::Canvas, geom::Rect};

    #[test]
    fn help_overlay_paints_a_dimmed_panel() {
        let fonts = FontSystem::load_embedded();
        let theme = Theme::light();
        // Tall enough that the whole panel is on screen: the legend is ~30
        // rows, and on a shorter window its tail is legitimately clipped.
        let (w, h) = (900.0, 900.0);
        let mut c = Canvas::new(900, 900);
        let bg = theme.bg.pack() & 0xFFFFFF;
        c.fill_rect(Rect::new(0.0, 0.0, w, h), theme.bg);

        let f = paint(&mut c, &fonts, &theme, w, h, 1.0);
        // The page is dimmed outside the panel.
        assert_ne!(c.pixels[10 * 900 + 2] & 0xFFFFFF, bg, "page not dimmed");

        // And there is real text inside its content area.
        let (x0, x1) = (
            f.content_x() as usize,
            ((f.panel.x + f.panel.w - f.pad) as usize).min(900),
        );
        let (y0, y1) = (
            (f.panel.y + f.pad) as usize,
            ((f.panel.y + f.panel.h - f.pad) as usize).min(900),
        );
        let ink = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| y * 900 + x))
            .filter(|&i| c.pixels[i] & 0xFFFFFF != bg)
            .count();
        assert!(ink > 200, "panel looks empty: {ink} non-background px");
    }

    /// A window shorter than the legend must clip, not panic.
    #[test]
    fn help_overlay_survives_a_short_window() {
        let fonts = FontSystem::load_embedded();
        let theme = Theme::dark();
        let mut c = Canvas::new(500, 300);
        let f = paint(&mut c, &fonts, &theme, 500.0, 300.0, 1.0);
        assert!(f.panel.y >= 8.0, "panel starts off-screen at {}", f.panel.y);
    }

    /// The reported bug: a heading sat flush under the previous section's last
    /// row, so sections ran together.
    #[test]
    fn headings_are_separated_from_the_section_above() {
        let scale = 1.0;
        let (rows, _) = layout(scale);
        let (_, line_h) = overlay::metrics(scale);
        let gap = section_gap(scale);
        let mut checked = 0;
        for w in rows.windows(2) {
            if let (Row::Item(..), Row::Heading(_)) = (w[0].0, w[1].0) {
                let space = w[1].1 - (w[0].1 + line_h);
                assert!(
                    space >= gap - 0.5,
                    "heading collides with the section above: {space}px gap"
                );
                checked += 1;
            }
        }
        assert_eq!(
            checked,
            SECTIONS.len() - 1,
            "expected a gap between sections"
        );
    }

    #[test]
    fn footer_is_separated_from_the_last_section() {
        let scale = 1.0;
        let (rows, _) = layout(scale);
        let (_, line_h) = overlay::metrics(scale);
        let last_item = rows
            .iter()
            .rev()
            .find(|(r, _)| matches!(r, Row::Item(..)))
            .expect("items");
        let first_footer = rows
            .iter()
            .find(|(r, _)| matches!(r, Row::Footer(_)))
            .expect("footer");
        let space = first_footer.1 - (last_item.1 + line_h);
        assert!(
            space >= section_gap(scale) - 0.5,
            "footer crowds the last section: {space}px"
        );
    }

    /// Every row must advance, and the panel must be tall enough for the
    /// content plus padding at both ends.
    #[test]
    fn layout_rows_are_ordered_and_fit_the_panel() {
        let scale = 1.0;
        let (rows, content_h) = layout(scale);
        let pad = overlay::pad(scale);
        assert_eq!(rows[0].1, pad, "first row should start at the top padding");
        for w in rows.windows(2) {
            assert!(w[1].1 > w[0].1, "rows must advance downward");
        }
        let last = rows.last().expect("rows");
        assert!(
            last.1 + overlay::metrics(scale).1 <= content_h,
            "last row overflows the content height"
        );
    }
}
