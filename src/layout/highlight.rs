//! Tinted plain-text layouts: INI/YAML/TOML configs, logs, and generic code.
//!
//! All line-local, zero dependencies, same cost class as [`super::text`]:
//! one pass over buffer lines, wrapping with [`super::text::wrap_ranges`]
//! and slicing the tint runs into visual rows — so a wrapped line keeps its
//! colors and memory stays proportional to the laid-out rows, exactly like
//! the plain viewer. Unknown shapes degrade to default text, never errors.

use super::{
    display_list::{DisplayList, DrawOp, FontStyle},
    text,
    theme::Theme,
    Ctx,
};
use crate::{core::buffer::Buffer, gfx::color::Color};

/// Which tint applies. `Code` carries the file extension (no dot) so the
/// shared scanner can pick comment flavors; everything else is extension-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Ini,
    Yaml,
    Toml,
    Log,
    Code,
}

/// Split one source line into `(text, color)` runs for `kind`.
fn tint_line(line: &str, kind: Kind, code_lang: &str, theme: &Theme) -> Vec<(String, Color)> {
    match kind {
        Kind::Code => super::markdown::code_spans(line, Some(code_lang), theme),
        Kind::Ini => tint_ini(line, theme),
        Kind::Yaml => tint_yaml(line, theme),
        Kind::Toml => tint_toml(line, theme),
        Kind::Log => tint_log(line, theme),
    }
}

fn is_comment_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with(';') || t.starts_with('#')
}

/// `key <sep> value` with an optional trailing `;`/`#` comment. Colors the
/// key, quotes/numbers/bools in the value, and the comment tail.
fn tint_kv(line: &str, sep: &[char], theme: &Theme, out: &mut Vec<(String, Color)>) -> bool {
    let bytes = line.as_bytes();
    let mut qi = false;
    let mut pos = None;
    for (i, c) in line.char_indices() {
        if c == '"' || c == '\'' {
            qi = !qi;
        } else if !qi && sep.contains(&c) {
            pos = Some(i);
            break;
        }
    }
    let Some(p) = pos else { return false };
    let (key, rest) = (&line[..p], &line[p..]);
    if key.trim().is_empty() {
        return false;
    }
    // Trailing comment: first `;`/`#` outside quotes past the separator.
    let mut qi = false;
    let mut comment_at = None;
    for (i, c) in rest.char_indices().skip(1) {
        if c == '"' || c == '\'' {
            qi = !qi;
        } else if !qi
            && (c == ';'
                || (c == '#'
                    && bytes
                        .get(i.wrapping_sub(1))
                        .is_some_and(|b| b.is_ascii_whitespace())))
        {
            comment_at = Some(p + i);
            break;
        }
    }
    let (value_part, comment) = match comment_at {
        Some(a) => (&line[p..a], Some(&line[a..])),
        None => (rest, None),
    };
    out.push((key.to_string(), theme.json_key));
    out.push((value_part.to_string(), value_color(value_part, theme)));
    if let Some(c) = comment {
        out.push((c.to_string(), theme.muted));
    }
    true
}

fn value_color(v: &str, theme: &Theme) -> Color {
    let t = v
        .trim()
        .trim_start_matches(['=', ':'])
        .trim()
        .trim_matches(|c| c == '"' || c == '\'');
    if t.parse::<f64>().is_ok() && !t.is_empty() {
        theme.json_number
    } else if matches!(
        t.to_lowercase().as_str(),
        "true" | "false" | "null" | "nil" | "none" | "yes" | "no" | "on" | "off"
    ) {
        theme.json_literal
    } else if v.contains('"') || v.contains('\'') {
        theme.json_string
    } else {
        theme.fg
    }
}

fn tint_ini(line: &str, theme: &Theme) -> Vec<(String, Color)> {
    if is_comment_line(line) {
        return vec![(line.to_string(), theme.muted)];
    }
    let t = line.trim();
    if t.starts_with('[') && t.ends_with(']') && t.len() > 2 {
        return vec![(line.to_string(), theme.json_key)];
    }
    let mut out = Vec::new();
    if tint_kv(line, &['=', ':'], theme, &mut out) {
        return out;
    }
    vec![(line.to_string(), theme.fg)]
}

fn tint_toml(line: &str, theme: &Theme) -> Vec<(String, Color)> {
    if line.trim_start().starts_with('#') {
        return vec![(line.to_string(), theme.muted)];
    }
    let t = line.trim();
    if (t.starts_with('[') && t.ends_with(']')) || (t.starts_with("[[") && t.ends_with("]]")) {
        return vec![(line.to_string(), theme.json_key)];
    }
    let mut out = Vec::new();
    if tint_kv(line, &['='], theme, &mut out) {
        return out;
    }
    vec![(line.to_string(), theme.fg)]
}

fn tint_yaml(line: &str, theme: &Theme) -> Vec<(String, Color)> {
    // Whole-line comment or document marker.
    let t = line.trim_start();
    if t.starts_with('#') || t == "---" || t == "..." {
        return vec![(line.to_string(), theme.muted)];
    }
    // List marker `- ` keeps its dash muted, the rest tints as key/value.
    let stripped = line.trim_start();
    if stripped.starts_with("- ") || stripped == "-" {
        let dash: usize = line
            .char_indices()
            .find(|(_, c)| *c == '-')
            .map(|(i, _)| i + 1)
            .unwrap_or(0);
        let mut out = vec![
            (line[..dash].to_string(), theme.muted),
            (line[dash..].to_string(), theme.fg),
        ];
        let rest = line[dash..].to_string();
        if let Some(colon) = rest.find(':') {
            let after = &rest[colon + 1..];
            if after.is_empty() || after.starts_with([' ', '\t']) {
                out.pop();
                out.push((rest[..colon].to_string(), theme.json_key));
                out.push((
                    rest[colon..].to_string(),
                    value_color(&rest[colon + 1..], theme),
                ));
            }
        }
        return out;
    }
    let mut out = Vec::new();
    // `key:` only when the colon is followed by space/end (not a URL scheme).
    if let Some(colon) = line.find(':') {
        let after = &line[colon + 1..];
        if (after.is_empty() || after.starts_with([' ', '\t']))
            && !line[..colon].trim().is_empty()
            && !line[..colon].contains(['#', '[', ']', '{', '}', ',', '"', '\''])
        {
            // Inline `#` comment tail outside quotes.
            let mut qi = false;
            let mut comment_at = None;
            for (i, c) in after.char_indices() {
                if c == '"' || c == '\'' {
                    qi = !qi;
                } else if !qi
                    && c == '#'
                    && (i == 0 || after.as_bytes()[i - 1].is_ascii_whitespace())
                {
                    comment_at = Some(colon + 1 + i);
                    break;
                }
            }
            out.push((line[..colon].to_string(), theme.json_key));
            match comment_at {
                Some(a) => {
                    out.push((
                        line[colon..a].to_string(),
                        value_color(&line[colon + 1..a], theme),
                    ));
                    out.push((line[a..].to_string(), theme.muted));
                }
                None => out.push((line[colon..].to_string(), value_color(after, theme))),
            }
            return out;
        }
    }
    // Inline comment tail for plain values.
    let mut qi = false;
    for (i, c) in line.char_indices() {
        if c == '"' || c == '\'' {
            qi = !qi;
        } else if !qi && c == '#' && (i == 0 || line.as_bytes()[i - 1].is_ascii_whitespace()) {
            return vec![
                (line[..i].to_string(), theme.fg),
                (line[i..].to_string(), theme.muted),
            ];
        }
    }
    vec![(line.to_string(), theme.fg)]
}

/// Severity class of a log line's level word.
///
/// One definition for both the tint and the status-bar counts — they used to
/// keep separate copies of the level lists and disagree by construction. The
/// comparison is ASCII-case-insensitive on the borrowed word, so a log line
/// costs no case-folding allocation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Other,
}

/// Classify `word` (already stripped of punctuation) as a log level.
pub fn level_of(word: &str) -> Level {
    const ERROR: [&str; 7] = [
        "ERROR", "ERR", "FATAL", "CRIT", "CRITICAL", "FAIL", "FAILED",
    ];
    const WARN: [&str; 2] = ["WARN", "WARNING"];
    const INFO: [&str; 2] = ["INFO", "NOTICE"];
    const DEBUG: [&str; 2] = ["DEBUG", "TRACE"];
    let eq = |set: &[&str]| set.iter().any(|k| k.eq_ignore_ascii_case(word));
    if eq(&ERROR) {
        Level::Error
    } else if eq(&WARN) {
        Level::Warn
    } else if eq(&INFO) {
        Level::Info
    } else if eq(&DEBUG) {
        Level::Debug
    } else {
        Level::Other
    }
}

fn tint_log(line: &str, theme: &Theme) -> Vec<(String, Color)> {
    let mut out = Vec::new();
    let mut rest = line;
    // Leading `[bracketed]` or timestamp-ish prefix stays muted.
    if let Some(t) = rest.trim_start().strip_prefix('[') {
        if let Some(end) = t.find(']') {
            let n = rest.len() - t.len() - 1;
            out.push((rest[..n + end + 2].to_string(), theme.muted));
            rest = &rest[n + end + 2..];
        }
    } else {
        // `2024-..`, `12:34:56`, `1.5s`-free: digits, dashes, colons, dots,
        // spaces, commas and T/Z separators lead the line.
        let prefix = rest
            .char_indices()
            .take_while(|&(i, c)| (c.is_ascii_digit() || " -:,./TZ".contains(c)) && i < 32)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        if prefix >= 8 {
            out.push((rest[..prefix].to_string(), theme.muted));
            rest = &rest[prefix..];
        }
    }
    // First bare word: a known level colors the whole tail's head word. The
    // iterator is consumed one word at a time — collecting the words only to
    // read the first one allocated a Vec per log line.
    if let Some(first) = rest.split_whitespace().next() {
        let level = first.trim_matches(|c: char| !c.is_alphanumeric());
        let color = match level_of(level) {
            Level::Error => Some(theme.error),
            Level::Warn => Some(theme.json_number),
            Level::Info => Some(theme.json_key),
            Level::Debug => Some(theme.muted),
            Level::Other => None,
        };
        if let Some(color) = color {
            let start = rest.find(first).unwrap_or(0);
            if start > 0 {
                out.push((rest[..start].to_string(), theme.fg));
            }
            out.push((first.to_string(), color));
            out.push((rest[start + first.len()..].to_string(), theme.fg));
            return out;
        }
    }
    out.push((rest.to_string(), theme.fg));
    out
}

/// Tinted preview for configs, logs, and generic code: monospace rows in the
/// reading column, wrapped exactly like [`text::layout_plain`].
pub fn layout(buf: &Buffer, width: f32, ctx: &Ctx, kind: Kind, code_lang: &str) -> DisplayList {
    let style = FontStyle::Mono;
    let p = text::plain_params(width, ctx, true);
    let m = ctx.fonts.metrics(style, p.size);
    let mut ops = Vec::new();
    let mut y = p.pad;
    for i in 0..buf.line_count() {
        let line = text::expand_tabs(buf.line(i));
        let runs = tint_line(&line, kind, code_lang, ctx.theme);
        // Char offsets of each run for slicing wrapped rows.
        let mut offsets = Vec::with_capacity(runs.len() + 1);
        let mut acc = 0;
        offsets.push(0);
        for (text, _) in &runs {
            acc += text.chars().count();
            offsets.push(acc);
        }
        for (s, e) in text::wrap_ranges(ctx.fonts, style, p.size, &line, p.col_w) {
            let baseline = (y + (p.line_h - m.height()) / 2.0 + m.ascent).round();
            let mut pen = p.x0;
            for (ri, (text, color)) in runs.iter().enumerate() {
                let (rs, re) = (offsets[ri], offsets[ri + 1]);
                let (a, b) = (s.max(rs), e.min(re));
                if a >= b {
                    continue;
                }
                let piece: String = text.chars().skip(a - rs).take(b - a).collect();
                if piece.is_empty() {
                    continue;
                }
                // Measure before the op takes ownership: cloning the piece
                // just to keep a copy for `measure` doubled the run count in
                // allocations on every laid-out row.
                let w = ctx.fonts.measure(style, &piece, p.size);
                ops.push(DrawOp::Text {
                    x: pen,
                    y: baseline,
                    text: piece,
                    size: p.size,
                    style,
                    color: *color,
                });
                pen += w;
            }
            y += p.line_h;
        }
    }
    DisplayList {
        ops,
        height: y + p.pad,
        hits: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::font::FontSystem;

    fn theme() -> crate::layout::theme::Theme {
        crate::layout::theme::Theme::light()
    }

    #[test]
    fn ini_sections_keys_and_comments_tint() {
        let t = theme();
        let sec = tint_line("[server]", Kind::Ini, "", &t);
        assert_eq!(sec.len(), 1);
        assert_eq!(sec[0].1, t.json_key);
        let kv = tint_line("port = 8080 ; web", Kind::Ini, "", &t);
        assert!(
            kv.iter().any(|(_, c)| *c == t.json_number),
            "number: {kv:?}"
        );
        assert!(kv.iter().any(|(_, c)| *c == t.muted), "comment: {kv:?}");
        let com = tint_line("; hello", Kind::Ini, "", &t);
        assert!(com.iter().all(|(_, c)| *c == t.muted));
    }

    #[test]
    fn yaml_keys_lists_and_comments_tint() {
        let t = theme();
        let kv = tint_line("port: 8080", Kind::Yaml, "", &t);
        assert_eq!(kv[0], ("port".to_string(), t.json_key));
        let li = tint_line("- name: web", Kind::Yaml, "", &t);
        assert!(li.iter().any(|(_, c)| *c == t.json_key), "list key: {li:?}");
        let com = tint_line("# hello", Kind::Yaml, "", &t);
        assert!(com.iter().all(|(_, c)| *c == t.muted));
        // A URL scheme colon is not a key.
        let url = tint_line("url: https://e.com", Kind::Yaml, "", &t);
        assert_eq!(url[0].1, t.json_key);
    }

    #[test]
    fn toml_tables_and_keys_tint() {
        let t = theme();
        let tab = tint_line("[server]", Kind::Toml, "", &t);
        assert_eq!(tab[0].1, t.json_key);
        let kv = tint_line("port = 8080", Kind::Toml, "", &t);
        assert!(kv.iter().any(|(_, c)| *c == t.json_number), "{kv:?}");
    }

    #[test]
    fn log_levels_tint() {
        let t = theme();
        let err = tint_line("2024-01-01 10:00:00 ERROR boom", Kind::Log, "", &t);
        assert!(err.iter().any(|(_, c)| *c == t.error), "{err:?}");
        assert!(err.iter().any(|(_, c)| *c == t.muted), "timestamp: {err:?}");
        let warn = tint_line("WARN slow", Kind::Log, "", &t);
        assert!(warn.iter().any(|(_, c)| *c == t.json_number), "{warn:?}");
        let info = tint_line("INFO up", Kind::Log, "", &t);
        assert!(info.iter().any(|(_, c)| *c == t.json_key), "{info:?}");
    }

    #[test]
    fn layout_keeps_wrapped_rows_inside_the_column() {
        let f = FontSystem::load_embedded();
        let t = theme();
        let c = Ctx {
            fonts: &f,
            theme: &t,
            scale: 1.0,
            zoom: 1.0,
        };
        let buf = Buffer::from_text("port = 8080 ; a comment that is long ".repeat(20).as_str());
        let list = layout(&buf, 900.0, &c, Kind::Ini, "");
        let (col_w, x0) = crate::layout::column(900.0, &c);
        assert!(!list.ops.is_empty());
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
                assert!(right <= x0 + col_w + 0.5, "row past column: {right}");
            }
        }
    }
}
