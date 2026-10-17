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

/// What the bar says to the left of the buttons, in paint order: the
/// document's own facts, then whatever the app has to add, then the Copy
/// button (while something is selected), then the link under the pointer.
pub struct Bar<'a> {
    /// Always present, first thing on the bar: `Markdown · 186 lines · 100%`.
    /// Unchanged by mode — the reader's eye can find it at the same place no
    /// matter what is selected.
    pub info: &'a str,
    /// Appended after `info` when non-empty: caret position in Edit, a parse
    /// error, `reloaded`, `open failed: …` and other transients.
    pub detail: &'a str,
    /// The address under the pointer, shown after the Copy button. `None`
    /// when nothing is hovered, so the bar carries no empty slot.
    pub link: Option<&'a str>,
}

/// Cut `text` to fit `max_w`, ending in an ellipsis. One char at a time from
/// the end: status-bar strings are short and this runs once per paint.
fn fit(fonts: &FontSystem, style: FontStyle, text: &str, size: f32, max_w: f32) -> String {
    if fonts.measure(style, text, size) <= max_w {
        return text.to_string();
    }
    let ell = fonts.measure(style, "…", size);
    let mut out: Vec<char> = text.chars().collect();
    while !out.is_empty() {
        out.pop();
        let mut s: String = out.iter().collect();
        s.push('…');
        if fonts.measure(style, &s, size) <= max_w.max(ell) {
            return s;
        }
    }
    "…".to_string()
}

/// Bottom status bar: the document's facts at the far left (then the Copy
/// button and a hovered link), and a row of clickable mode buttons at the
/// right edge. `buttons` carries label/active in; `rect` is filled in for hit
/// testing, as is `copy`'s.
pub fn paint(
    canvas: &mut Canvas,
    fonts: &FontSystem,
    theme: &Theme,
    area: Rect,
    bar: Bar<'_>,
    buttons: &mut [StatusButton],
    copy: &mut Option<StatusButton>,
) {
    let scale = area.h / HEIGHT;
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
    let gap = 6.0 * scale;

    // Buttons, right-aligned; the vec order is left-to-right on screen, so we
    // walk it in reverse. [Find] [Open] | [Preview] [Edit] [Split] | [Help] [About] [Theme]
    let mut bx = area.right() - pad;
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

    // ---- the left cluster -------------------------------------------------
    // `info`, then `detail`, then the Copy button, then the hovered link. It
    // grows to the right and stops short of the buttons; anything too long is
    // clipped rather than painted under them.
    let limit = (bx - gap).max(area.x + pad);
    let mut x = area.x + pad;
    let draw = |canvas: &mut Canvas, text: &str, color: Color, x: f32, max_w: f32| -> f32 {
        let shown = fit(fonts, FontStyle::Regular, text, size, max_w);
        let w = fonts.measure(FontStyle::Regular, &shown, size);
        canvas.draw_text(
            fonts,
            &shown,
            x,
            baseline,
            TextSpec {
                size,
                style: FontStyle::Regular,
                color,
            },
        );
        w
    };

    x += draw(canvas, bar.info, theme.muted, x, limit - x);
    if !bar.detail.is_empty() {
        let sep = format!("  ·  {}", bar.detail);
        x += draw(canvas, &sep, theme.fg, x, limit - x);
    }

    // The Copy button, after the document facts. Accent-tinted so it reads as
    // an action, not another mode pill; full bar height for a finger. Its
    // label is the caller's — "Copy" turning into "Copied" once the press
    // landed, which is the feedback that used to be a status message.
    if let Some(cb) = copy.as_mut() {
        let tw = fonts.measure(FontStyle::Regular, cb.label, size);
        let w = tw + 14.0 * scale;
        let cbx = (x + 10.0 * scale).min((limit - w).max(area.x + pad));
        cb.rect = Rect::new(cbx, area.y, w, area.h);
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
        x = cb.rect.right() + 10.0 * scale;
    }

    // ...and the hovered link last, where it cannot fight the button for
    // space: it is the least important thing on the bar and gets what is left.
    if let Some(link) = bar.link {
        let room = limit - x;
        if room > 24.0 * scale {
            draw(canvas, link, theme.muted, x, room);
        }
    }
    canvas.set_clip(None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::{canvas::Canvas, font::FontSystem};

    const INFO: &str = "Markdown  ·  186 lines  ·  100%";

    /// Paint one bar and hand back the canvas plus whatever rects it filled.
    fn paint_bar(
        detail: &str,
        link: Option<&str>,
        copy_label: Option<&'static str>,
    ) -> (Canvas, Option<StatusButton>) {
        let fonts = FontSystem::load_embedded();
        let theme = Theme::light();
        let area = Rect::new(0.0, 0.0, 900.0, HEIGHT);
        let mut c = Canvas::new(900, HEIGHT as usize);
        let mut buttons: Vec<StatusButton> = Vec::new();
        let mut copy = copy_label.map(|label| StatusButton {
            id: 8,
            label,
            active: false,
            group_start: false,
            rect: Rect::default(),
        });
        paint(
            &mut c,
            &fonts,
            &theme,
            area,
            Bar {
                info: INFO,
                detail,
                link,
            },
            &mut buttons,
            &mut copy,
        );
        (c, copy)
    }

    /// First and last column carrying ink (the bar's own background is not
    /// ink), so a test can ask where the text actually landed.
    fn ink_span(c: &Canvas) -> (usize, usize) {
        let bg = Theme::light().code_bg.pack() & 0xFFFFFF;
        let (w, h) = (c.width, c.height);
        let mut first = None;
        let mut last = 0;
        for x in 0..w {
            // Skip the top hairline rule: it runs the whole width.
            if (1..h).any(|y| c.pixels[y * w + x] & 0xFFFFFF != bg) {
                first.get_or_insert(x);
                last = x;
            }
        }
        (first.expect("the bar painted nothing"), last)
    }

    #[test]
    fn the_document_facts_start_at_the_left_edge() {
        let (c, _) = paint_bar("", None, None);
        let (first, _) = ink_span(&c);
        // Padding is 14 px at 1x; a couple of pixels of glyph bearing is fine.
        assert!(first < 20, "text starts at x={first}, want the far left");
    }

    #[test]
    fn the_copy_button_sits_after_the_data_and_a_link_after_that() {
        let fonts = FontSystem::load_embedded();
        let info_w = fonts.measure(FontStyle::Regular, INFO, 12.5);
        let (plain, _) = paint_bar("", None, None);
        let (_, plain_last) = ink_span(&plain);
        let (with_link, _) = paint_bar("", Some("https://example.com/a"), None);
        let (_, link_last) = ink_span(&with_link);
        assert!(
            link_last > plain_last,
            "the hovered address is painted after the facts ({plain_last} -> {link_last})"
        );
        // With a selection, the button comes first and the link after it.
        let (both, copy) = paint_bar("", Some("https://example.com/a"), Some("Copy"));
        let cb = copy.expect("copy button");
        assert!(
            cb.rect.x >= 14.0 + info_w,
            "the button follows the data: x={} info ends at {}",
            cb.rect.x,
            14.0 + info_w
        );
        // No ink between the info and the button, and the link is to its right.
        let (_, both_last) = ink_span(&both);
        assert!(
            both_last as f32 >= cb.rect.right(),
            "the link is painted right of the button ({both_last} < {})",
            cb.rect.right()
        );
        // Full bar height, so a finger that lands a little high or low still
        // presses it.
        assert!(both.width > 0 && cb.rect.h == HEIGHT);
    }

    #[test]
    fn detail_text_pushes_the_button_right() {
        let (_, short) = paint_bar("", None, Some("Copy"));
        let (_, long) = paint_bar("Ln 3, Col 5  ·  error 2:5", None, Some("Copy"));
        assert!(
            long.expect("copy").rect.x > short.expect("copy").rect.x,
            "the caret/error detail belongs between the facts and the button"
        );
    }

    #[test]
    fn copied_is_wider_than_copy_but_starts_in_the_same_place() {
        let (_, copy) = paint_bar("", None, Some("Copy"));
        let (_, copied) = paint_bar("", None, Some("Copied"));
        let (copy, copied) = (copy.unwrap(), copied.unwrap());
        assert!(
            (copy.rect.x - copied.rect.x).abs() < 0.5,
            "the left edge must not jump when the label changes"
        );
        assert!(copied.rect.w > copy.rect.w);
    }

    #[test]
    fn a_link_too_long_for_the_bar_is_cut_short_of_the_buttons() {
        let long = format!("https://example.com/{}", "segment/".repeat(40));
        let (c, _) = paint_bar("", Some(&long), Some("Copy"));
        let (_, last) = ink_span(&c);
        // No buttons in this paint, so the cluster's limit is the right padding.
        assert!(last < 900 - 14, "the link ran past the bar: {last}");
    }
}
