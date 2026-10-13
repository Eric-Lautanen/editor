use crate::{
    gfx::{
        canvas::{Canvas, TextSpec},
        color::Color,
        font::{FontStyle, FontSystem},
        geom::Rect,
    },
    layout::theme::Theme,
};

pub const HEIGHT: f32 = 28.0; // logical px

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
    let baseline = (area.y + (area.h - m.height()) / 2.0 + m.ascent).round();
    let pad = 14.0 * scale;

    // Buttons, right-aligned; the vec order is left-to-right on screen, so we
    // walk it in reverse. [Open] | [Preview] [Edit] [Split] | [Help] [About] [Theme]
    let mut bx = area.right() - pad;
    let gap = 6.0 * scale;
    for (i, b) in buttons.iter_mut().enumerate().rev() {
        let tw = fonts.measure(FontStyle::Regular, b.label, size);
        let w = tw + 14.0 * scale;
        b.rect = Rect::new(bx - w, area.y + 3.0 * scale, w, area.h - 6.0 * scale);
        // A group rule sits in the gap immediately left of this button.
        if b.group_start && i != 0 {
            canvas.fill_rect(
                Rect::new(
                    (b.rect.x - gap / 2.0).round(),
                    area.y + 6.0 * scale,
                    scale.max(1.0),
                    area.h - 12.0 * scale,
                ),
                theme.rule,
            );
        }
        bx = b.rect.x - gap;
        if b.active {
            canvas.fill_rounded_rect(
                b.rect,
                5.0 * scale,
                Color::rgba(theme.accent.r, theme.accent.g, theme.accent.b, 60),
            );
        }
        canvas.draw_text(
            fonts,
            b.label,
            b.rect.x + 7.0 * scale,
            (b.rect.y + (b.rect.h - m.height()) / 2.0 + m.ascent).round(),
            TextSpec {
                size,
                style: FontStyle::Regular,
                color: if b.active { theme.accent } else { theme.muted },
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
