//! About box (F2): product name and version, author, licence, and the font
//! credit the bundled Liberation faces require. Same chrome as the help
//! legend, so the two read as one family.

use crate::{
    gfx::{
        canvas::{Canvas, TextSpec},
        font::{FontStyle, FontSystem},
        geom::Rect,
    },
    layout::theme::Theme,
};

use super::overlay;

/// Product name as shown in the title bar.
const NAME: &str = "Editor";
/// Read from Cargo so the box cannot drift from what `--version` prints.
const VERSION: &str = env!("CARGO_PKG_VERSION");

const TAGLINE: &[&str] = &[
    "A small native viewer and light editor for Markdown, JSON,",
    "JSONL, CSV, configs, logs, code and plain text.",
];

/// `(label, value)`. The font credit is not decoration: the bundled
/// Liberation faces are SIL OFL 1.1, which requires attribution.
const FIELDS: &[(&str, &str)] = &[
    ("Author", "Eric Lautanen"),
    ("License", "MIT"),
    ("Fonts", "Liberation Sans & Mono — SIL OFL 1.1"),
    (
        "Built with",
        "Rust — standard library only, no dependencies",
    ),
    ("GitHub", "https://github.com/Eric-Lautanen/editor"),
];

const HINT: &str = "F2 or Esc to close";

/// The one setting the About box hosts: background update checks. Drawn as a
/// checkbox row after the field table; the returned rect is what clicks
/// hit-test against. Always-on gates live in `update.rs` regardless — this
/// switch only decides whether the check happens at all.
const UPDATE_TOGGLE: &str = "Check for updates automatically (twice a day)";

/// Paint the box over the whole window, returning the panel it drew and the
/// toggle row's clickable rect.
pub fn paint(
    canvas: &mut Canvas,
    fonts: &FontSystem,
    theme: &Theme,
    width: f32,
    height: f32,
    scale: f32,
    auto_update: bool,
) -> (overlay::Frame, crate::gfx::geom::Rect) {
    let (size, line_h) = overlay::metrics(scale);
    let pad = overlay::pad(scale);
    let col_gap = (size * 1.6).round();
    let gap = (size * 1.1).round();
    let title_size = (size * 1.9).round();
    let title_h = (title_size * 1.5).round();

    // Hug the content: the widest of the title line, the tagline, the field
    // table and the hint.
    let widest = |texts: &[&str], style| {
        texts
            .iter()
            .map(|t| fonts.measure(style, t, size))
            .fold(0.0_f32, f32::max)
    };
    let label_w = widest(
        &FIELDS.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
        FontStyle::Bold,
    );
    let value_w = widest(
        &FIELDS.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
        FontStyle::Regular,
    );
    let title_w = fonts.measure(FontStyle::Bold, NAME, title_size)
        + (size * 0.9).round()
        + fonts.measure(FontStyle::Regular, VERSION, size);
    let content_w = (label_w + col_gap + value_w)
        .max(widest(TAGLINE, FontStyle::Regular))
        .max(title_w)
        .max(fonts.measure(FontStyle::Regular, HINT, size));
    let panel_w = (content_w + 2.0 * pad).min(width - 40.0 * scale);

    let content_h = title_h + TAGLINE.len() as f32 * line_h + gap + FIELDS.len() as f32 * line_h + gap
            + line_h // the update toggle row
            + gap
            + line_h; // the hint
    let f = overlay::frame(
        canvas,
        fonts,
        theme,
        (width, height),
        (panel_w, content_h + 2.0 * pad),
        scale,
    );

    let x = f.content_x();
    let mut y = f.panel.y + pad;

    // Name in display type, version trailing it in muted type on the same
    // baseline.
    let tm = fonts.metrics(FontStyle::Bold, title_size);
    let baseline = (y + (title_h - tm.height()) / 2.0 + tm.ascent).round();
    canvas.draw_text(
        fonts,
        NAME,
        x,
        baseline,
        TextSpec {
            size: title_size,
            style: FontStyle::Bold,
            color: theme.fg,
        },
    );
    canvas.draw_text(
        fonts,
        VERSION,
        x + fonts.measure(FontStyle::Bold, NAME, title_size) + (size * 0.9).round(),
        baseline,
        TextSpec {
            size,
            style: FontStyle::Regular,
            color: theme.muted,
        },
    );
    y += title_h;

    for line in TAGLINE {
        canvas.draw_text(
            fonts,
            line,
            x,
            f.baseline(y),
            TextSpec {
                size,
                style: FontStyle::Regular,
                color: theme.fg,
            },
        );
        y += line_h;
    }

    y += gap;
    let value_x = x + label_w + col_gap;
    for (label, value) in FIELDS {
        let baseline = f.baseline(y);
        canvas.draw_text(
            fonts,
            label,
            x,
            baseline,
            TextSpec {
                size,
                style: FontStyle::Bold,
                color: theme.muted,
            },
        );
        canvas.draw_text(
            fonts,
            value,
            value_x,
            baseline,
            TextSpec {
                size,
                style: FontStyle::Regular,
                color: theme.fg,
            },
        );
        y += line_h;
    }

    // The update-check toggle: a real checkbox (square outline, check glyph
    // when on — drawn with filled slabs, the canvas's only primitive), then
    // the label. Clicking anywhere on the row flips it; the app persists it
    // like any other setting.
    y += gap;
    let toggle_y = y;
    let box_d = (line_h * 0.75).round();
    let bx = x + (line_h * 0.15).round();
    let by = toggle_y + (line_h - box_d) / 2.0;
    let edge = 1.0_f32.max(scale); // ~1 px outline
    let edge_color = if auto_update {
        theme.accent
    } else {
        theme.muted
    };
    // Square outline: four thin slabs.
    canvas.fill_rect(Rect::new(bx, by, box_d, edge), edge_color);
    canvas.fill_rect(Rect::new(bx, by + box_d - edge, box_d, edge), edge_color);
    canvas.fill_rect(Rect::new(bx, by, edge, box_d), edge_color);
    canvas.fill_rect(Rect::new(bx + box_d - edge, by, edge, box_d), edge_color);
    if auto_update {
        let m = box_d * 0.24; // inset from the outline
        let (c0x, c0y) = (bx + m, by + box_d * 0.55); // left arm's start
        let (c1x, c1y) = (bx + box_d * 0.42, by + box_d - m); // the elbow
        let (c2x, c2y) = (bx + box_d - m, by + m); // right arm's end
        let t = (box_d * 0.14).max(edge);
        // A diagonal slab: stamps small squares along the segment.
        let mut slab = |a: (f32, f32), b: (f32, f32)| {
            let steps = ((b.0 - a.0).abs().max((b.1 - a.1).abs()) / (t * 0.5))
                .ceil()
                .max(1.0) as i32;
            for i in 0..=steps {
                let u = i as f32 / steps as f32;
                canvas.fill_rect(
                    Rect::new(
                        a.0 + (b.0 - a.0) * u - t / 2.0,
                        a.1 + (b.1 - a.1) * u - t / 2.0,
                        t,
                        t,
                    ),
                    theme.accent,
                );
            }
        };
        slab((c0x, c0y), (c1x, c1y));
        slab((c1x, c1y), (c2x, c2y));
    }
    canvas.draw_text(
        fonts,
        UPDATE_TOGGLE,
        bx + box_d + (size * 0.4).round(),
        f.baseline(toggle_y),
        TextSpec {
            size,
            style: FontStyle::Regular,
            color: if auto_update { theme.fg } else { theme.muted },
        },
    );
    let toggle = Rect::new(f.panel.x + pad, toggle_y, f.panel.w - 2.0 * pad, line_h);

    y += line_h;
    y += gap;
    canvas.draw_text(
        fonts,
        HINT,
        x,
        f.baseline(y),
        TextSpec {
            size,
            style: FontStyle::Regular,
            color: theme.muted,
        },
    );
    (f, toggle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::{canvas::Canvas, geom::Rect};

    /// The box exists to name the author; guard against it being dropped.
    #[test]
    fn about_credits_the_author() {
        let author = FIELDS
            .iter()
            .find(|(label, _)| *label == "Author")
            .expect("an Author row");
        assert_eq!(author.1, "Eric Lautanen");
    }

    /// The OFL requires attribution for the bundled faces.
    #[test]
    fn about_credits_the_fonts() {
        let fonts = FIELDS
            .iter()
            .find(|(label, _)| *label == "Fonts")
            .expect("a Fonts row");
        assert!(fonts.1.contains("Liberation"), "{}", fonts.1);
        assert!(fonts.1.contains("OFL"), "{}", fonts.1);
    }

    #[test]
    fn about_box_paints_a_centred_panel_with_text() {
        let fonts = FontSystem::load_embedded();
        let theme = Theme::dark();
        let (w, h) = (900.0, 640.0);
        let mut c = Canvas::new(900, 640);
        let bg = theme.bg.pack() & 0xFFFFFF;
        c.fill_rect(Rect::new(0.0, 0.0, w, h), theme.bg);

        let (f, toggle) = paint(&mut c, &fonts, &theme, w, h, 1.0, true);
        assert!(
            (f.panel.x - (w - f.panel.w) / 2.0).abs() < 1.0,
            "not centred"
        );
        // The toggle row is real estate inside the panel the click handler
        // uses — non-degenerate and within the panel's horizontal padding.
        assert!(toggle.w > 0.0 && toggle.h > 0.0, "toggle rect empty");
        assert!(toggle.x >= f.panel.x + f.pad && toggle.y > f.panel.y);
        assert_ne!(c.pixels[10 * 900 + 2] & 0xFFFFFF, bg, "page not dimmed");

        let (x0, x1) = (
            f.content_x() as usize,
            (f.panel.x + f.panel.w - f.pad) as usize,
        );
        let (y0, y1) = (
            (f.panel.y + f.pad) as usize,
            (f.panel.y + f.panel.h - f.pad) as usize,
        );
        let ink = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| y * 900 + x))
            .filter(|&i| c.pixels[i] & 0xFFFFFF != bg)
            .count();
        assert!(ink > 200, "box looks empty: {ink} non-background px");
    }

    /// Nothing may be clipped: the panel has to be wide enough for the widest
    /// row plus padding at both sides.
    #[test]
    fn panel_is_wide_enough_for_its_content() {
        let fonts = FontSystem::load_embedded();
        let theme = Theme::light();
        let (w, h) = (1200.0, 700.0);
        let mut c = Canvas::new(1200, 700);
        let (f, _) = paint(&mut c, &fonts, &theme, w, h, 1.0, false);
        let (size, _) = overlay::metrics(1.0);
        let widest = FIELDS
            .iter()
            .map(|(l, v)| {
                fonts.measure(FontStyle::Bold, l, size)
                    + (size * 1.6).round()
                    + fonts.measure(FontStyle::Regular, v, size)
            })
            .fold(0.0_f32, f32::max);
        assert!(
            f.panel.w >= widest,
            "panel {} narrower than content {widest}",
            f.panel.w
        );
    }
}
