//! Tinted plain-text layouts: INI/YAML/TOML configs, logs, and generic code.
//!
//! Zero dependencies and the same cost class as [`super::text`]: one pass over
//! buffer lines, wrapping with [`super::text::wrap_ranges`] and slicing the
//! tint runs into visual rows — so a wrapped line keeps its colors and memory
//! stays proportional to the laid-out rows, exactly like the plain viewer.
//! Config and log tinting is line-local; code tinting carries block comments
//! and docstrings across lines with [`super::syntax::State`]. Unknown shapes
//! degrade to default text, never errors.

use super::{
    display_list::{DisplayList, DrawOp, FontStyle},
    plan, syntax, text,
    theme::Theme,
    Ctx,
};
use crate::{
    core::buffer::{Buffer, EditSpan},
    gfx::color::Color,
};

/// Which tint applies. `Code` carries the file extension (no dot) so the
/// shared scanner can pick comment flavors; everything else is extension-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Plain text: the same line grid, no tint. Sharing the variant is what
    /// lets the plain pane use the line plan and band painter too.
    Plain,
    Ini,
    Yaml,
    Toml,
    Log,
    Code,
}

/// Split one source line into `(text, color)` runs for `kind`. `Kind::Code`
/// starts a fresh syntax state; [`layout`] keeps one across lines.
fn tint_line(line: &str, kind: Kind, code_lang: &str, theme: &Theme) -> Vec<(String, Color)> {
    match kind {
        Kind::Plain => vec![(line.to_string(), theme.fg)],
        Kind::Code => syntax::runs(
            line,
            syntax::Lang::from_hint(code_lang),
            &mut syntax::State::default(),
            theme,
        ),
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

/// Split a comment tail off `s`, ignoring markers inside quotes. `#` counts
/// only at the start of the text or after whitespace, so a hex color
/// (`#ffcc00`) is a value; `;` always starts a comment.
fn split_comment(s: &str, hash: bool, semicolon: bool) -> (&str, Option<&str>) {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
            continue;
        }
        let marker = (hash && c == '#') || (semicolon && c == ';');
        if !marker {
            continue;
        }
        let after_space = i == 0 || s[..i].chars().next_back().is_some_and(char::is_whitespace);
        if c == '#' && !after_space {
            continue;
        }
        return (&s[..i], Some(&s[i..]));
    }
    (s, None)
}

/// Split a value into tinted runs: quoted strings, numbers and literal words
/// get their own colors, everything else stays `fg`. This is what makes
/// `tags = ["a", "b"]` or `limits: { min: -128 }` read like structure instead
/// of one flat-colored blob.
fn value_runs(v: &str, theme: &Theme) -> Vec<(String, Color)> {
    let chars: Vec<char> = v.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |out: &mut Vec<(String, Color)>, cur: &mut String| {
        if !cur.is_empty() {
            out.push((std::mem::take(cur), theme.fg));
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' || c == '\'' {
            flush(&mut out, &mut cur);
            let mut j = i + 1;
            while j < chars.len() {
                if chars[j] == '\\' {
                    j += 2;
                    continue;
                }
                if chars[j] == c {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let end = j.min(chars.len());
            out.push((chars[i..end].iter().collect(), theme.json_string));
            i = end;
            continue;
        }
        let signed = c == '-' || c == '+';
        let number = c.is_ascii_digit()
            || (signed
                && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())
                && (i == 0 || chars[i - 1].is_whitespace() || "=:[,(".contains(chars[i - 1])));
        if number {
            flush(&mut out, &mut cur);
            let mut j = if signed { i + 1 } else { i };
            while j < chars.len() {
                let d = chars[j];
                let continues = d.is_ascii_alphanumeric()
                    || d == '_'
                    || (d == '.' && chars.get(j + 1) != Some(&'.'))
                    || (matches!(d, '+' | '-') && j > i && matches!(chars[j - 1], 'e' | 'E'));
                if !continues {
                    break;
                }
                j += 1;
            }
            out.push((chars[i..j].iter().collect(), theme.json_number));
            i = j;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            if is_literal(&word) {
                flush(&mut out, &mut cur);
                out.push((word, theme.json_literal));
            } else {
                cur.push_str(&word);
            }
            i = j;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    flush(&mut out, &mut cur);
    if out.is_empty() {
        out.push((v.to_string(), theme.fg));
    }
    out
}

fn is_literal(word: &str) -> bool {
    [
        "true", "false", "null", "nil", "none", "yes", "no", "on", "off",
    ]
    .iter()
    .any(|k| k.eq_ignore_ascii_case(word))
}

/// First `sep` outside quotes, or `None`.
fn find_separator(s: &str, seps: &[char]) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
        } else if seps.contains(&c) {
            return Some(i);
        }
    }
    None
}

/// `key <sep> value` with an optional trailing comment. Colors the key, then
/// quotes/numbers/literals inside the value, then the comment tail.
fn tint_kv(
    line: &str,
    sep: &[char],
    hash_comment: bool,
    semicolon_comment: bool,
    theme: &Theme,
    out: &mut Vec<(String, Color)>,
) -> bool {
    let Some(p) = find_separator(line, sep) else {
        return false;
    };
    let (key, rest) = (&line[..p], &line[p..]);
    if key.trim().is_empty() {
        return false;
    }
    out.push((key.to_string(), theme.json_key));
    let (value, comment) = split_comment(rest, hash_comment, semicolon_comment);
    out.extend(value_runs(value, theme));
    if let Some(c) = comment {
        out.push((c.to_string(), theme.muted));
    }
    true
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
    if tint_kv(line, &['=', ':'], true, true, theme, &mut out) {
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
    if tint_kv(line, &['='], true, false, theme, &mut out) {
        return out;
    }
    vec![(line.to_string(), theme.fg)]
}

/// `key:` at a position where the colon is followed by whitespace or the end
/// of the line (a URL scheme's colon is not a key), with a key free of the
/// punctuation that would make it an inline flow collection.
fn yaml_key_colon(s: &str) -> Option<usize> {
    let colon = s.find(':')?;
    let after = &s[colon + 1..];
    if !(after.is_empty() || after.starts_with([' ', '\t'])) {
        return None;
    }
    let key = s[..colon].trim();
    if key.is_empty() || key.contains(['#', '[', ']', '{', '}', ',', '"', '\'']) {
        return None;
    }
    Some(colon)
}

/// Tint `body` as a YAML mapping entry, or as a plain scalar when it is not
/// one. The indent is already in `body` when called for a top-level line.
fn tint_yaml_entry(body: &str, theme: &Theme, out: &mut Vec<(String, Color)>) {
    if let Some(colon) = yaml_key_colon(body) {
        out.push((body[..colon].to_string(), theme.json_key));
        let (value, comment) = split_comment(&body[colon..], true, false);
        out.extend(value_runs(value, theme));
        if let Some(c) = comment {
            out.push((c.to_string(), theme.muted));
        }
    } else {
        let (value, comment) = split_comment(body, true, false);
        out.extend(value_runs(value, theme));
        if let Some(c) = comment {
            out.push((c.to_string(), theme.muted));
        }
    }
}

fn tint_yaml(line: &str, theme: &Theme) -> Vec<(String, Color)> {
    let indent = line.len() - line.trim_start().len();
    let t = &line[indent..];
    // Whole-line comment or document marker.
    if t.starts_with('#') || t == "---" || t == "..." {
        return vec![(line.to_string(), theme.muted)];
    }
    // List marker `- ` keeps its dash muted; what follows is a mapping entry
    // or a scalar (`- 42`).
    if let Some(rest) = t.strip_prefix('-') {
        if rest.is_empty() || rest.starts_with([' ', '\t']) {
            let mut out = vec![(line[..indent + 1].to_string(), theme.muted)];
            let after = &line[indent + 1..];
            let body = after.trim_start();
            let lead = &after[..after.len() - body.len()];
            if !lead.is_empty() {
                out.push((lead.to_string(), theme.fg));
            }
            if !body.is_empty() {
                tint_yaml_entry(body, theme, &mut out);
            }
            return out;
        }
    }
    let mut out = Vec::new();
    tint_yaml_entry(line, theme, &mut out);
    out
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

/// Lines between syntax checkpoints. A band that starts far into a file needs
/// the tint state at its first line; a checkpoint every 256 lines turns that
/// into one short scan instead of a scan of everything above it.
const STATE_EVERY: usize = 256;

/// C1 · The edit-validation scan walks at most this many checkpoint windows:
/// a keystroke needs two, and anything wider than the cap is re-proved the
/// old way (the next band's lazy fill) instead of pricing the edit for it.
const STATE_VALIDATE_SPANS: usize = 8;

/// Per-line plan for a big code, log or config pane: where every buffer line
/// starts in document space, plus the tint state at periodic checkpoints.
///
/// Rows are counted, not laid out — with the same fit test [`layout`] makes —
/// and only a line that overflows the column is wrapped for real. A big file
/// therefore plans in milliseconds, keeps one `f32` per line instead of an op
/// per wrapped run, and paints a band.
pub struct LinePlan {
    pub rows: plan::Rows,
    /// The tint state at the start of each checkpoint line; `None` until the
    /// band that needs it fills it. Empty for the line-local kinds, whose
    /// colour never depends on the line above.
    ///
    /// `Cell` rather than `&mut`: painting holds a shared borrow of the plan
    /// alongside the layout context, and a checkpoint is a cache write, not a
    /// change to what the plan describes.
    states: Vec<std::cell::Cell<Option<syntax::State>>>,
}

impl LinePlan {
    /// C1 · Carry the tint checkpoints a width-change replan must not
    /// throw away: the states are a property of the *text* — what line each
    /// checkpoint is keyed to and nothing about the width — so a plan built
    /// at a new width adopts the old plan's known checkpoints when the two
    /// still describe the same line count. `None` when the shapes disagree:
    /// the caller builds a lazy plan, exactly as before.
    pub(crate) fn adopt_states(&mut self, old: &LinePlan, line_count: usize) {
        if self.states.is_empty() || old.states.is_empty() {
            return;
        }
        let share = self.states.len().min(old.states.len());
        if old.states.len() != line_count / STATE_EVERY + 2 {
            return;
        }
        for i in 0..share {
            self.states[i].set(old.states[i].get());
        }
    }

    /// Patch the plan for the lines an edit touched: only the inserted lines
    /// are counted again, and the tint checkpoints from the edit onward are
    /// dropped so a band there rescans them from the text (validated by the
    /// bounded scan C1 added — see below).
    ///
    /// The row count is the same function [`build_plan`] uses, so the result is
    /// the plan the document would have been given whole.
    pub fn patch(
        &mut self,
        buf: &Buffer,
        width: f32,
        ctx: &Ctx,
        kind: Kind,
        code_lang: &str,
        edits: &[EditSpan],
    ) -> bool {
        let p = text::plain_params(width, ctx, true);
        let adv = ctx.fonts.advance(FontStyle::Mono, '0', p.size).max(1.0);
        let mut heights: Vec<f32> = Vec::new();
        let mut first = usize::MAX;
        for e in edits {
            if e.start + e.inserted > buf.line_count() {
                return false;
            }
            heights.clear();
            for i in e.start..e.start + e.inserted {
                let line = text::expand_tabs(buf.line(i));
                heights.push(line_rows(&line, ctx, &p, adv) as f32 * p.line_h);
            }
            if !self.rows.splice(e.start, e.removed, &heights) {
                return false;
            }
            first = first.min(e.start);
        }
        if kind == Kind::Code && first != usize::MAX {
            // C1 · A line's colour depends on everything above it, so the
            // checkpoints at and after the first touched line may have
            // changed — and an edit can only change the text of the lines
            // it replaced.
            //
            // Instead of dropping every checkpoint below the edit (which
            // made the next far band re-lex the whole document), the
            // checkpoints are re-derived from the one above the edit and
            // compared against what the plan already carries: a keystroke
            // that does not change the state — the overwhelming majority —
            // re-proves every stored checkpoint in one 256-line scan and
            // the far bands keep a known state; one that does change it (a
            // comment opened, a triple quote) breaks the match at the first
            // boundary it reaches and only the checkpoints after that are
            // dropped.
            let cells = buf.line_count() / STATE_EVERY + 2;
            self.states
                .resize_with(cells, || std::cell::Cell::new(None));
            let from = first / STATE_EVERY;
            // The first checkpoint whose window lies wholly past the edit:
            // its entering state is derived from text that includes the
            // edit, so it is the one comparison that proves or refutes
            // everything below the edit.
            let later_edited = edits
                .iter()
                .map(|e| e.start + e.inserted.saturating_sub(1))
                .max()
                .unwrap_or(first);
            let decisive = later_edited / STATE_EVERY + 1;
            // The derivation walks lines the edit did not move, so the
            // batch must leave every line count alone (a changed count
            // shifts the line each checkpoint is keyed to) and stay inside
            // the bounded span.
            let mut stable = edits.iter().all(|e| e.inserted == e.removed)
                && decisive < cells
                && decisive - from <= STATE_VALIDATE_SPANS + 1;
            let mut old = None;
            if stable {
                old = self.states[decisive].get();
            }
            let mut st = if stable {
                // The state entering `from * STATE_EVERY`: known whenever
                // its checkpoint was filled — text above the edit's own
                // lines cannot have changed — and the opening state at
                // checkpoint zero.
                let base = if from == 0 {
                    Some(syntax::State::default())
                } else {
                    self.states[from].get()
                };
                match base {
                    Some(s) => s,
                    // Nothing to prove the chain from: the checkpoint the
                    // edit left untouched was never filled. Drop from it —
                    // the next band refills what it needs, the way the lazy
                    // path always did.
                    None => {
                        stable = false;
                        syntax::State::default()
                    }
                }
            } else {
                syntax::State::default()
            };
            if !stable {
                for c in &mut self.states[from..] {
                    c.set(None);
                }
            }
            if stable {
                let lang = syntax::Lang::from_hint(code_lang);
                let mut at = from * STATE_EVERY;
                while at < decisive * STATE_EVERY && at < buf.line_count() {
                    if at.is_multiple_of(STATE_EVERY) {
                        self.states[at / STATE_EVERY].set(Some(st));
                    }
                    let line = text::expand_tabs(buf.line(at));
                    syntax::runs(&line, lang, &mut st, ctx.theme);
                    at += 1;
                }
                if old == Some(st) {
                    // The text below the edit runs the same state: every
                    // checkpoint after it keeps its value.
                } else {
                    for c in &mut self.states[decisive..] {
                        c.set(None);
                    }
                    self.states[decisive].set(Some(st));
                }
            }
        }
        true
    }

    /// The tint state where a band starts, filling checkpoints from the
    /// nearest known one. Each line is walked once while filling, so scrolling
    /// through a file costs one extra pass in total; a jump into the middle
    /// pays for the lines between the last checkpoint and the band, once.
    ///
    /// C1 · A plan built with its checkpoints filled (`build_plan_states`),
    /// one whose checkpoints a benign edit re-proved ([`Self::patch`]), or a
    /// replan that adopted them ([`Self::adopt_states`]) has a known state at
    /// the band without scanning anything.
    fn state_at(
        &self,
        line: usize,
        buf: &Buffer,
        lang: syntax::Lang,
        theme: &Theme,
    ) -> syntax::State {
        if self.states.is_empty() {
            return syntax::State::default();
        }
        let cp = (line / STATE_EVERY).min(self.states.len() - 1);
        let known = |k: usize| self.states[k].get();
        if cp == 0 {
            // The document opens outside any construct; no scan is needed to
            // know where checkpoint zero starts.
            self.states[0].set(Some(syntax::State::default()));
        } else if known(cp).is_none() {
            let prev = (0..cp).rev().find(|&k| known(k).is_some()).unwrap_or(0);
            let mut st = known(prev).unwrap_or_default();
            let mut at = prev * STATE_EVERY;
            while at < (cp + 1) * STATE_EVERY && at < buf.line_count() {
                if at.is_multiple_of(STATE_EVERY) && at / STATE_EVERY <= cp {
                    self.states[at / STATE_EVERY].set(Some(st));
                }
                let line = text::expand_tabs(buf.line(at));
                syntax::runs(&line, lang, &mut st, theme);
                at += 1;
            }
        }
        // The band may start between checkpoints: scan the lines from the
        // checkpoint to it, which is at most `STATE_EVERY` lines.
        let mut st = known(cp).unwrap_or_default();
        let mut at = cp * STATE_EVERY;
        while at < line && at < buf.line_count() {
            let line = text::expand_tabs(buf.line(at));
            syntax::runs(&line, lang, &mut st, theme);
            at += 1;
        }
        st
    }

    /// Lay out the lines whose rows overlap `[top, bottom)`, each at the y the
    /// plan carries. The tint state is the one a full layout would have at
    /// that line, so the band is the whole list with the rest left out.
    #[allow(clippy::too_many_arguments)]
    pub fn layout_band(
        &self,
        buf: &Buffer,
        width: f32,
        ctx: &Ctx,
        kind: Kind,
        code_lang: &str,
        top: f32,
        bottom: f32,
    ) -> DisplayList {
        let p = text::plain_params(width, ctx, true);
        let adv = ctx.fonts.advance(FontStyle::Mono, '0', p.size).max(1.0);
        let lang = syntax::Lang::from_hint(code_lang);
        let range = self.rows.band(top, bottom);
        let mut state = if kind == Kind::Code {
            self.state_at(range.start, buf, lang, ctx.theme)
        } else {
            syntax::State::default()
        };
        let mut ops = Vec::new();
        for i in range {
            let line = text::expand_tabs(buf.line(i));
            let runs = if kind == Kind::Code {
                syntax::runs(&line, lang, &mut state, ctx.theme)
            } else {
                tint_line(&line, kind, "", ctx.theme)
            };
            let _ = emit_line(
                &mut ops,
                ctx,
                &p,
                adv,
                &line,
                runs,
                self.rows.top(i),
                top,
                bottom,
            );
        }
        DisplayList::new(ops, self.rows.height(), Vec::new())
    }
}

/// Plan a code/log/config pane: one entry per buffer line, at the y a full
/// layout would give it.
pub fn build_plan(buf: &Buffer, width: f32, ctx: &Ctx, kind: Kind) -> LinePlan {
    build_plan_states(buf, width, ctx, kind, "", false)
}

/// The plan with its tint checkpoints filled ahead of any band: the
/// whole-document syntax scan the lazy [`LinePlan::state_at`] would otherwise
/// put on the first band that starts far from every known checkpoint. This is
/// whole-document work, so it runs where nothing waits on it: the worker
/// plans fill as they lay off-thread, and an inline build fills too only when
/// there is no worker to hand it to (headless and CLI runs) — there the fill
/// is the same lex the far band would pay anyway, moved off the frame that
/// must respond. Interactive builds below the worker threshold stay lazy: an
/// inline build stays in the milliseconds the row counting costs, and a small
/// file's far band scan is bounded by the file.
pub fn build_plan_states(
    buf: &Buffer,
    width: f32,
    ctx: &Ctx,
    kind: Kind,
    code_lang: &str,
    eager: bool,
) -> LinePlan {
    let p = text::plain_params(width, ctx, true);
    let adv = ctx.fonts.advance(FontStyle::Mono, '0', p.size).max(1.0);
    let n = buf.line_count();
    let mut tops = Vec::with_capacity(n);
    let mut y = p.pad;
    let states = if kind == Kind::Code {
        (0..n / STATE_EVERY + 2)
            .map(|_| std::cell::Cell::new(None))
            .collect()
    } else {
        Vec::new()
    };
    let fill = kind == Kind::Code && eager;
    let lang = syntax::Lang::from_hint(code_lang);
    let mut st = syntax::State::default();
    for i in 0..n {
        tops.push(y);
        let line = text::expand_tabs(buf.line(i));
        y += line_rows(&line, ctx, &p, adv) as f32 * p.line_h;
        if fill {
            // Checkpoint k is the state entering line `k * STATE_EVERY`, the
            // same ordering `state_at`'s lazy fill keeps — an eager plan and
            // a lazily filled one carry identical states.
            if i % STATE_EVERY == 0 {
                states[i / STATE_EVERY].set(Some(st));
            }
            syntax::runs(&line, lang, &mut st, ctx.theme);
        }
    }
    LinePlan {
        rows: plan::Rows::new(tops, y + p.pad, p.pad),
        states,
    }
}

/// Rows one line wraps to in the tinted pane — the fit test [`build_plan`] and
/// [`LinePlan::patch`] share, so a patched plan cannot drift from a fresh one.
fn line_rows(line: &str, ctx: &Ctx, p: &text::PlainParams, adv: f32) -> usize {
    if line.is_ascii() && line.len() as f32 * adv <= p.col_w {
        1
    } else {
        text::wrap_ranges(ctx.fonts, FontStyle::Mono, p.size, line, p.col_w)
            .len()
            .max(1)
    }
}

/// One tinted line at `y`, emitting only the rows that touch `[top, bottom)`.
/// A full layout passes the whole document as the band, so both paths run this
/// one function and cannot disagree about a row's baseline or colour.
#[allow(clippy::too_many_arguments)]
fn emit_line(
    ops: &mut Vec<DrawOp>,
    ctx: &Ctx,
    p: &text::PlainParams,
    adv: f32,
    line: &str,
    runs: Vec<(String, Color)>,
    mut y: f32,
    top: f32,
    bottom: f32,
) -> f32 {
    let style = FontStyle::Mono;
    let m = ctx.fonts.metrics(style, p.size);
    let visible = |y: f32| y + p.line_h >= top && y <= bottom;
    // A line that fits the column is one visual row, and every run of it is
    // emitted whole — text moved straight into its op. The general path below
    // (char-offset table, wrap call, a fresh `String` per run per visual row)
    // is what a *wrapped* line needs; paying it for every line of a code or
    // log pane was most of the remaining layout cost.
    if line.is_ascii() && line.len() as f32 * adv <= p.col_w {
        if visible(y) {
            let baseline = (y + (p.line_h - m.height()) / 2.0 + m.ascent).round();
            let mut pen = p.x0;
            for (text, color) in runs {
                if text.is_empty() {
                    continue;
                }
                let w = text.len() as f32 * adv;
                ops.push(DrawOp::Text {
                    x: pen,
                    y: baseline,
                    text,
                    size: p.size,
                    style,
                    color,
                });
                pen += w;
            }
        }
        return y + p.line_h;
    }
    // Char offsets of each run for slicing wrapped rows.
    let mut offsets = Vec::with_capacity(runs.len() + 1);
    let mut acc = 0;
    offsets.push(0);
    for (text, _) in &runs {
        acc += text.chars().count();
        offsets.push(acc);
    }
    for (s, e) in text::wrap_ranges(ctx.fonts, style, p.size, line, p.col_w) {
        if visible(y) {
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
                // Non-ASCII runs fall back to the font: the mono cell is only
                // fixed for the embedded faces' own coverage (a CJK glyph or
                // an emoji out of the fallback chain is wider than one cell).
                let w = if piece.is_ascii() {
                    piece.len() as f32 * adv
                } else {
                    ctx.fonts.measure(style, &piece, p.size)
                };
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
        }
        y += p.line_h;
    }
    y
}

/// Tinted preview for configs, logs, and generic code: monospace rows in the
/// reading column, wrapped exactly like [`text::layout_plain`].
pub fn layout(buf: &Buffer, width: f32, ctx: &Ctx, kind: Kind, code_lang: &str) -> DisplayList {
    let p = text::plain_params(width, ctx, true);
    let adv = ctx.fonts.advance(FontStyle::Mono, '0', p.size).max(1.0);
    // Code tinting carries block comments and docstrings across lines; the
    // config/log tinters are line-local.
    let lang = syntax::Lang::from_hint(code_lang);
    let mut code_state = syntax::State::default();
    let mut ops = Vec::new();
    let mut y = p.pad;
    for i in 0..buf.line_count() {
        let line = text::expand_tabs(buf.line(i));
        let runs = if kind == Kind::Code {
            syntax::runs(&line, lang, &mut code_state, ctx.theme)
        } else {
            tint_line(&line, kind, "", ctx.theme)
        };
        y = emit_line(
            &mut ops,
            ctx,
            &p,
            adv,
            &line,
            runs,
            y,
            f32::NEG_INFINITY,
            f32::INFINITY,
        );
    }
    DisplayList::new(ops, y + p.pad, Vec::new())
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
    fn values_tint_by_shape_not_whole_blob() {
        let t = theme();
        // A TOML array: the strings and numbers inside it get their colors,
        // while the separators stay plain.
        let arr = tint_line(r#"tags = ["one", "two", 3]"#, Kind::Toml, "", &t);
        let strings: Vec<&str> = arr
            .iter()
            .filter(|(_, c)| *c == t.json_string)
            .map(|(s, _)| s.as_str())
            .collect();
        assert_eq!(strings, vec!["\"one\"", "\"two\""], "{arr:?}");
        assert!(
            arr.iter().any(|(s, c)| *c == t.json_number && s == "3"),
            "{arr:?}"
        );
        // A YAML flow mapping keeps its numbers as numbers.
        let flow = tint_line("limits: { min: -128, max: 20 }", Kind::Yaml, "", &t);
        assert!(
            flow.iter().any(|(s, c)| *c == t.json_number && s == "-128"),
            "{flow:?}"
        );
        // A list item that is only a scalar is tinted too.
        let item = tint_line("- 42", Kind::Yaml, "", &t);
        assert!(
            item.iter().any(|(s, c)| *c == t.json_number && s == "42"),
            "{item:?}"
        );
        // Booleans are literals, not keys.
        let flag = tint_line("enabled = true", Kind::Toml, "", &t);
        assert!(flag.iter().any(|(_, c)| *c == t.json_literal), "{flag:?}");
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

#[cfg(test)]
mod band_tests {
    use super::*;
    use crate::core::buffer::Pos;
    use crate::gfx::font::FontSystem;
    use crate::layout::theme::Theme;

    fn ctx<'a>(f: &'a FontSystem, t: &'a Theme) -> Ctx<'a> {
        Ctx {
            fonts: f,
            theme: t,
            scale: 1.0,
            zoom: 1.0,
        }
    }

    /// 700 lines with a block comment that opens near the top and closes past
    /// the second checkpoint: a band in the middle has to know it is inside a
    /// comment it never painted.
    fn doc() -> Buffer {
        let mut s = String::new();
        s.push_str("/*\n");
        for i in 1..620 {
            s.push_str(&format!("still inside the comment, line {i}\n"));
        }
        s.push_str(
            "*/
",
        );
        for i in 620..700 {
            s.push_str(&format!(
                "pub fn f{i}(input: &str) -> usize {{ input.len() + {} }}\n",
                "x".repeat(i % 50)
            ));
        }
        Buffer::from_text(&s)
    }

    fn text_of(op: &DrawOp) -> String {
        format!("{op:?}")
    }

    #[test]
    fn a_whole_document_band_is_the_full_code_layout() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let buf = doc();
        let full = layout(&buf, 900.0, &c, Kind::Code, "rs");
        let plan = build_plan(&buf, 900.0, &c, Kind::Code);
        let band = plan.layout_band(
            &buf,
            900.0,
            &c,
            Kind::Code,
            "rs",
            f32::NEG_INFINITY,
            f32::INFINITY,
        );
        assert!((band.height - full.height).abs() < 0.5);
        assert_eq!(
            band.ops.iter().map(text_of).collect::<Vec<_>>(),
            full.ops.iter().map(text_of).collect::<Vec<_>>()
        );
    }

    /// The tint state a band starts with is the state the full layout has
    /// there: 600 lines into a comment, every row is comment-coloured, even
    /// though the band itself never saw the `/*`.
    #[test]
    fn a_band_starting_inside_a_comment_keeps_the_comment_colour() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let buf = doc();
        let plan = build_plan(&buf, 900.0, &c, Kind::Code);
        let top = plan.rows.top(600);
        // A band wholly inside the comment: lines 600-614, well before the
        // `*/` at 620.
        let band = plan.layout_band(&buf, 900.0, &c, Kind::Code, "rs", top, plan.rows.top(615));
        assert!(!band.ops.is_empty());
        for op in &band.ops {
            if let DrawOp::Text { color, text, .. } = op {
                assert_eq!(*color, t.muted, "outside the comment: {text:?}");
            }
        }
        // And the code below the comment still tints as code, not comment.
        let top = plan.rows.top(650);
        let band = plan.layout_band(&buf, 900.0, &c, Kind::Code, "rs", top, top + 400.0);
        assert!(
            band.ops
                .iter()
                .any(|op| matches!(op, DrawOp::Text { color, .. } if *color != t.muted)),
            "code after the comment must not stay muted"
        );
    }

    #[test]
    fn a_line_band_claims_nothing_and_misses_nothing_visible() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let buf = doc();
        let full = layout(&buf, 900.0, &c, Kind::Code, "rs");
        let plan = build_plan(&buf, 900.0, &c, Kind::Code);
        let lh = text::line_height(900.0, &c);
        let all: Vec<String> = full.ops.iter().map(text_of).collect();
        for top in [0.0, 300.0, 2_500.0, plan.rows.height() - 500.0] {
            let bottom = top + 700.0;
            let band = plan.layout_band(&buf, 900.0, &c, Kind::Code, "rs", top, bottom);
            let mut seen = 0;
            for op in &band.ops {
                let s = text_of(op);
                let at = all[seen..]
                    .iter()
                    .position(|a| *a == s)
                    .unwrap_or_else(|| panic!("at {top}: the band invented {s}"));
                seen += at + 1;
            }
            let got = band.ops.iter().map(text_of).collect::<Vec<_>>();
            let inside = full
                .ops
                .iter()
                .filter(|op| matches!(op, DrawOp::Text { y, .. } if *y >= top + lh && *y <= bottom - lh))
                .map(text_of)
                .collect::<Vec<_>>();
            for s in inside {
                assert!(got.contains(&s), "at {top}: the band missed {s}");
            }
        }
    }

    /// An edit is patched onto the plan, and a band deep below it keeps the
    /// colour a full layout gives those lines. The edit closes the long
    /// comment early and opens a new one, so every checkpoint below it is
    /// stale: a band that trusted the old state would paint the rest of the
    /// file muted.
    #[test]
    fn an_edit_patches_the_plan_and_rescans_the_tint_below_it() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let mut buf = doc();
        let mut plan = build_plan(&buf, 900.0, &c, Kind::Code);
        // Close the 620-line comment at line 300 and reopen it right away: the
        // text below the edit is unchanged, its syntax state is not.
        buf.insert(Pos { line: 300, col: 0 }, "*/\n/*\n");
        let edits = buf.take_edits().expect("one edit");
        assert!(plan.patch(&buf, 900.0, &c, Kind::Code, "rs", &edits));
        let fresh = build_plan(&buf, 900.0, &c, Kind::Code);
        assert_eq!(plan.rows.len(), fresh.rows.len());
        for i in 0..fresh.rows.len() {
            assert_eq!(plan.rows.top(i), fresh.rows.top(i), "top {i}");
        }
        assert_eq!(plan.rows.height(), fresh.rows.height());

        // A band well past the edit, compared op for op with the full layout
        // over the same rows.
        let top = plan.rows.top(650);
        let bottom = plan.rows.top(700);
        let band = plan.layout_band(&buf, 900.0, &c, Kind::Code, "rs", top, bottom);
        // The patched plan's band is the fresh plan's band, op for op: every
        // row, every run and every colour, at the same y.
        let expected = fresh.layout_band(&buf, 900.0, &c, Kind::Code, "rs", top, bottom);
        assert_eq!(
            band.ops.iter().map(text_of).collect::<Vec<_>>(),
            expected.ops.iter().map(text_of).collect::<Vec<_>>(),
            "band below the edit"
        );
        // And it is code, not the comment the old checkpoints still claimed.
        assert!(
            band.ops
                .iter()
                .any(|op| matches!(op, DrawOp::Text { color, .. } if *color != t.muted)),
            "the band below the closed comment must not stay muted"
        );
    }

    /// A deletion shifts the lines below up: the plan is spliced, not
    /// re-counted, and the pane below still bands at the right tops.
    #[test]
    fn a_deletion_patches_the_plan_and_shifts_the_pane_up() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let mut buf = doc();
        let mut plan = build_plan(&buf, 900.0, &c, Kind::Code);
        let before = plan.rows.top(650);
        buf.delete(Pos { line: 10, col: 0 }, Pos { line: 60, col: 0 });
        let edits = buf.take_edits().expect("one edit");
        assert!(plan.patch(&buf, 900.0, &c, Kind::Code, "rs", &edits));
        let fresh = build_plan(&buf, 900.0, &c, Kind::Code);
        for i in 0..fresh.rows.len() {
            assert_eq!(plan.rows.top(i), fresh.rows.top(i), "top {i}");
        }
        assert_eq!(plan.rows.height(), fresh.rows.height());
        // 50 lines of 1.55em text left the document above this one.
        let lh = text::line_height(900.0, &c);
        assert!((before - plan.rows.top(600) - 50.0 * lh).abs() < 0.5);
    }

    /// C1 · A plan whose checkpoints were filled during the build paints the
    /// same band a lazily filled plan paints, op for op: the whole-document
    /// scan the worker runs is exactly what the lazy fill would discover at
    /// the first far band, and the two ways to learn it must agree.
    #[test]
    fn eager_checkpoints_fill_the_same_tint_the_lazy_scan_finds() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        let buf = doc();
        let eager = build_plan_states(&buf, 900.0, &c, Kind::Code, "rs", true);
        let lazy = build_plan(&buf, 900.0, &c, Kind::Code);
        // The fixture's tint depends on the lines above: a 620-line comment
        // opens near the top, so both a band inside it and one after it
        // carry a state the lazy fill has to reconstruct.
        for (top, bottom) in [
            (lazy.rows.top(300), lazy.rows.top(360)),
            (lazy.rows.top(700), lazy.rows.top(760)),
        ] {
            let band_eager = eager.layout_band(&buf, 900.0, &c, Kind::Code, "rs", top, bottom);
            let band_lazy = lazy.layout_band(&buf, 900.0, &c, Kind::Code, "rs", top, bottom);
            assert_eq!(
                band_eager.ops, band_lazy.ops,
                "eager and lazy bands disagree over [{top:.0}, {bottom:.0})"
            );
            assert!(
                !band_eager.ops.is_empty(),
                "no band over [{top:.0}, {bottom:.0})"
            );
        }
    }

    /// C1 · The edit patch re-proves the checkpoints a benign keystroke
    /// could not change: every stored state survives, so the far bands keep
    /// a known tint. Only an edit that actually changes the state running
    /// down the document drops them past the boundary it breaks.
    #[test]
    fn a_benign_keystroke_reproves_checkpoints_and_a_state_change_drops_the_rest() {
        let f = FontSystem::load_embedded();
        let t = Theme::light();
        let c = ctx(&f, &t);
        // Eager (worker-shaped) plan: every checkpoint known.
        let mut buf = doc();
        let mut plan = build_plan_states(&buf, 900.0, &c, Kind::Code, "rs", true);
        let before: Vec<_> = plan.states.iter().map(|s| s.get()).collect();

        // A keystroke inside line 10: same line count, no construct opened.
        buf.insert(Pos { line: 10, col: 0 }, "x");
        let edits = buf.take_edits().expect("one edit");
        assert!(plan.patch(&buf, 900.0, &c, Kind::Code, "rs", &edits));
        let n = plan.states.len().min(before.len());
        for (i, want) in before.iter().enumerate().take(n) {
            assert!(
                plan.states[i].get() == *want,
                "checkpoint {i} should survive a benign keystroke"
            );
        }

        // An edit that opens a block comment replaces line 340's text with
        // one line (same line count): the state entering the next boundary
        // differs, so the checkpoints after it are dropped, the ones
        // between are re-derived, and a deep band still paints what a
        // fresh plan paints.
        let head_len = buf.line(340).len();
        let _ = buf.delete(
            Pos { line: 340, col: 0 },
            Pos {
                line: 340,
                col: head_len,
            },
        );
        buf.insert(Pos { line: 340, col: 0 }, "/* opened comment");
        let edits = buf.take_edits().expect("two edits");
        assert!(plan.patch(&buf, 900.0, &c, Kind::Code, "rs", &edits));
        let fresh = build_plan_states(&buf, 900.0, &c, Kind::Code, "rs", true);
        // Line 340: 340/256 = 1, decisive boundary = 2.
        {
            let i = 2;
            assert!(
                plan.states[i].get() == fresh.states[i].get(),
                "checkpoint {i} re-derived to the fresh plan's state"
            );
        }
        assert!(
            plan.states[3].get().is_none(),
            "the checkpoints past the broken boundary are dropped"
        );
        // A band deep below still paints exactly the fresh plan's band.
        let top = plan.rows.top(700);
        let bottom = plan.rows.top(760);
        let band = plan.layout_band(&buf, 900.0, &c, Kind::Code, "rs", top, bottom);
        let want = fresh.layout_band(&buf, 900.0, &c, Kind::Code, "rs", top, bottom);
        assert_eq!(band.ops, want.ops, "band deep below the state change");
    }
}
