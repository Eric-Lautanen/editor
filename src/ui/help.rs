//! F1 help overlay: the whole legend — modes, shortcuts, file types, how to
//! select and copy — painted over the document. Any click or Escape closes it.

use crate::{
    gfx::{
        canvas::{Canvas, TextSpec},
        font::{FontStyle, FontSystem},
        geom::Rect,
    },
    layout::theme::Theme,
};

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

/// Paint the overlay over the whole window.
pub fn paint(
    canvas: &mut Canvas,
    fonts: &FontSystem,
    theme: &Theme,
    width: f32,
    height: f32,
    scale: f32,
) {
    let size = 13.0 * scale;
    let line_h = (size * 1.6).round();
    let head_h = (size * 1.6 + 6.0 * scale).round();
    let pad = 22.0 * scale;
    let key_w = 110.0 * scale;
    let panel_w = (key_w + 320.0 * scale + 2.0 * pad).min(width - 40.0 * scale);
    let rows: usize = SECTIONS.iter().map(|(_, items)| items.len()).sum();
    let sections_h = SECTIONS.len() as f32 * head_h + rows as f32 * line_h;
    let panel_h = sections_h + FOOTER.len() as f32 * line_h + 3.0 * pad;
    let px = ((width - panel_w) / 2.0).round();
    let py = ((height - panel_h) / 2.0).round().max(8.0);

    // Dim the page, then the panel.
    canvas.fill_rect(
        Rect::new(0.0, 0.0, width, height),
        crate::gfx::color::Color::rgba(0, 0, 0, 96),
    );
    let panel = Rect::new(px, py, panel_w, panel_h);
    canvas.fill_rounded_rect(panel, 8.0 * scale, theme.bg);
    canvas.fill_rounded_rect(
        Rect::new(panel.x, panel.y, panel.w, 2.0 * scale),
        1.0 * scale,
        theme.accent,
    );

    let m = fonts.metrics(FontStyle::Regular, size);
    let baseline = |top: f32| (top + (line_h - m.height()) / 2.0 + m.ascent).round();
    let mut y = py + pad;
    for &(heading, items) in SECTIONS {
        canvas.draw_text(
            fonts,
            heading,
            px + pad,
            baseline(y),
            TextSpec {
                size,
                style: FontStyle::Bold,
                color: theme.accent,
            },
        );
        y += head_h;
        for (key, desc) in items {
            canvas.draw_text(
                fonts,
                key,
                px + pad,
                baseline(y),
                TextSpec {
                    size,
                    style: FontStyle::Mono,
                    color: theme.fg,
                },
            );
            canvas.draw_text(
                fonts,
                desc,
                px + pad + key_w,
                baseline(y),
                TextSpec {
                    size,
                    style: FontStyle::Regular,
                    color: theme.muted,
                },
            );
            y += line_h;
        }
    }
    y += pad * 0.5;
    for line in FOOTER {
        canvas.draw_text(
            fonts,
            line,
            px + pad,
            baseline(y),
            TextSpec {
                size,
                style: FontStyle::Regular,
                color: theme.muted,
            },
        );
        y += line_h;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::canvas::Canvas;

    #[test]
    fn help_overlay_paints_a_dimmed_panel() {
        let fonts = FontSystem::load_embedded();
        let theme = Theme::light();
        let mut c = Canvas::new(900, 520);
        let bg = theme.bg.pack();
        c.fill_rect(Rect::new(0.0, 0.0, 900.0, 520.0), theme.bg);
        paint(&mut c, &fonts, &theme, 900.0, 520.0, 1.0);
        // Left of the panel the page is dimmed; the panel edge itself is a
        // visible step (dimmed outside, plain background inside).
        assert_ne!(c.pixels[260 * 900 + 10], bg, "page not dimmed");
        assert_ne!(
            c.pixels[260 * 900 + 212],
            c.pixels[260 * 900 + 214],
            "panel edge missing"
        );
    }
}
