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
/// (0=Preview, 1=Edit, 2=Split, 3=Help, 4=Theme, 5=Open, 6=About, 7=Find).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusButton {
    pub id: u8,
    pub label: &'static str,
    pub active: bool,
    /// Draw a hairline rule in the gap to this button's left, splitting the
    /// row into visual groups (`Find Open | Preview Edit Split | Help About | Theme`).
    pub group_start: bool,
    /// Hit rect: the full bar height, so a finger that lands a little high or
    /// low still presses the button (the pill it paints is inset inside it).
    pub rect: Rect,
}

/// Bottom status bar: left text, right-aligned info text, a row of
/// clickable mode buttons at the right edge, and — while `copy` is `Some` —
/// a Copy button at the far left (the touch-friendly way to copy a
/// selection). `buttons` carries label/active in; `rect` is filled in for
/// hit testing, as is `copy`'s.
pub fn paint(
    canvas: &mut Canvas,
    fonts: &FontSystem,
    theme: &Theme,
    area: Rect,
    text: (&str, &str),
    buttons: &mut [StatusButton],
    copy: &mut Option<StatusButton>,
) {
    let scale = area.h / HEIGHT;
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
    // walk it in reverse. [Find] [Open] | [Preview] [Edit] [Split] | [Help] [About] [Theme]
    let mut bx = area.right() - pad;
    let gap = 6.0 * scale;
    for (i, b) in buttons.iter_mut().enumerate().rev() {
        let tw = fonts.measure(FontStyle::Regular, b.label, size);
        let w = tw + 14.0 * scale;
        // Full bar height, so a finger lands even when it strays vertically;
        // the pill below paints inset inside it.
        b.rect = Rect::new(bx - w, area.y, w, area.h);
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
                Rect::new(
                    b.rect.x,
                    area.y + 4.0 * scale,
                    b.rect.w,
                    area.h - 8.0 * scale,
                ),
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

    // The Copy button, far left, only while a selection exists. Accent-tinted
    // so it reads as an action, not another mode pill; full bar height for a
    // finger. The left text starts after it.
    let mut left_x = area.x + pad;
    if let Some(cb) = copy.as_mut() {
        let tw = fonts.measure(FontStyle::Regular, cb.label, size);
        let w = tw + 14.0 * scale;
        cb.rect = Rect::new(area.x + pad, area.y, w, area.h);
        canvas.fill_rounded_rect(
            Rect::new(
                cb.rect.x,
                area.y + 4.0 * scale,
                cb.rect.w,
                area.h - 8.0 * scale,
            ),
            5.0 * scale,
            Color::rgba(theme.accent.r, theme.accent.g, theme.accent.b, 48),
        );
        canvas.draw_text(
            fonts,
            cb.label,
            cb.rect.x + 7.0 * scale,
            (cb.rect.y + (cb.rect.h - m.height()) / 2.0 + m.ascent - 1.0 * scale).round(),
            TextSpec {
                size,
                style: FontStyle::Regular,
                color: theme.fg,
            },
        );
        left_x = cb.rect.right() + 10.0 * scale;
    }

    // Left text, then right info text butted against the button block.
    canvas.draw_text(
        fonts,
        left,
        left_x,
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
