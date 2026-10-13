use crate::{
    gfx::{
        canvas::{Canvas, TextSpec},
        color::Color,
        font::{FontStyle, FontSystem},
        geom::Rect,
    },
    layout::theme::Theme,
};

pub const HEIGHT: f32 = 32.0; // logical px

/// A clickable segment of the status bar. `id` is app-defined
/// (0=Preview, 1=Edit, 2=Split, 3=Help, 4=Theme, 5=Open, 6=About).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusButton {
    pub id: u8,
    pub label: &'static str,
    pub active: bool,
    /// Draw a hairline rule in the gap to this button's left, splitting the
    /// row into visual groups (`Open | Preview Edit Split | Help About | Theme`).
    pub group_start: bool,
    pub rect: Rect,
}

/// Bottom status bar: left text, right-aligned info text, and a row of
/// clickable mode buttons at the right edge. `buttons` carries label/active
/// in; `rect` is filled in for hit testing.
pub fn paint(
    canvas: &mut Canvas,
    fonts: &FontSystem,
    theme: &Theme,
    area: Rect,
    scale: f32,
    text: (&str, &str),
    buttons: &mut [StatusButton],
) {
    let (left, right) = text;
    canvas.set_clip(Some(area));
    canvas.fill_rect(area, theme.code_bg);
    canvas.fill_rect(
        Rect::new(area.x, area.y, area.w, scale.max(1.0).round()),
        theme.rule,
    );
    let size = 12.5 * scale;
    let m = fonts.metrics(FontStyle::Regular, size);
    // Optical centering: bias 1px up so cap-height text reads centred with
    // a touch more air below than above.
    let baseline = (area.y + (area.h - m.height()) / 2.0 + m.ascent - 1.0 * scale).round();
    let pad = 14.0 * scale;

    // Buttons, right-aligned; the vec order is left-to-right on screen, so we
    // walk it in reverse. [Open] | [Preview] [Edit] [Split] | [Help] [About] [Theme]
    let mut bx = area.right() - pad;
    let gap = 6.0 * scale;
    for (i, b) in buttons.iter_mut().enumerate().rev() {
        let tw = fonts.measure(FontStyle::Regular, b.label, size);
        let w = tw + 14.0 * scale;
        // Taller bar with symmetric 4px insets: larger click target with
        // more air above and below the text.
        b.rect = Rect::new(bx - w, area.y + 4.0 * scale, w, area.h - 8.0 * scale);
        // A group rule sits in the gap immediately left of this button.
        if b.group_start && i != 0 {
            canvas.fill_rect(
                Rect::new(
                    (b.rect.x - gap / 2.0).round(),
                    area.y + 8.0 * scale,
                    scale.max(1.0),
                    area.h - 16.0 * scale,
                ),
                theme.rule,
            );
        }
        bx = b.rect.x - gap;
        if b.active {
            // Neutral "selected pill": a subtle fg tint with fg text. The old
            // accent-blue wash read as cheap next to the otherwise grey
            // chrome; this matches the title-bar hover language and stays
            // legible in both themes.
            canvas.fill_rounded_rect(
                b.rect,
                5.0 * scale,
                Color::rgba(theme.fg.r, theme.fg.g, theme.fg.b, 32),
            );
        }
        canvas.draw_text(
            fonts,
            b.label,
            b.rect.x + 7.0 * scale,
            (b.rect.y + (b.rect.h - m.height()) / 2.0 + m.ascent - 1.0 * scale).round(),
            TextSpec {
                size,
                style: FontStyle::Regular,
                color: if b.active { theme.fg } else { theme.muted },
            },
        );
    }

    // Left text, then right info text butted against the button block.
    canvas.draw_text(
        fonts,
        left,
        area.x + pad,
        baseline,
        TextSpec {
            size,
            style: FontStyle::Regular,
            color: theme.fg,
        },
    );
    let rw = fonts.measure(FontStyle::Regular, right, size);
    canvas.draw_text(
        fonts,
        right,
        bx - gap - rw,
        baseline,
        TextSpec {
            size,
            style: FontStyle::Regular,
            color: theme.muted,
        },
    );
    canvas.set_clip(None);
}
