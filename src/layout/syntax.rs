//! Line-oriented syntax tinting for code panes and fenced code blocks.
//!
//! Zero dependencies and allocation-light: one call scans one line into
//! `(text, color)` runs, with a tiny [`State`] carried across lines for the
//! few constructs that genuinely span them (block comments, Python
//! docstrings, HTML comments). Language hints map onto a handful of families,
//! so an unknown fence still gets sensible comments and strings instead of
//! flat text, and the tables are static slices rather than runtime maps.

use super::theme::Theme;
use crate::gfx::color::Color;

/// How a fence's info string or a file extension is tinted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lang {
    Plain,
    Unknown,
    Rust,
    Python,
    Shell,
    Ruby,
    CLike,
    Js,
    Go,
    Php,
    Css,
    Html,
    Json,
    Sql,
    Lua,
    Toml,
    Yaml,
    Ini,
}

impl Lang {
    /// Map an extension or a fence's info string onto a highlighter.
    /// Aliases and a leading `.`/`language-` are accepted; unknown hints get
    /// the generic scanner rather than no tint at all.
    pub(crate) fn from_hint(hint: &str) -> Self {
        let lower = hint.trim().trim_start_matches('.').to_ascii_lowercase();
        // Fence info strings can carry options (`rust,ignore`, `c {.x}`).
        let h = lower
            .split(|c: char| c == ',' || c == '{' || c.is_whitespace())
            .next()
            .unwrap_or("");
        let h = h.strip_prefix("language-").unwrap_or(h);
        match h {
            "" | "text" | "plain" | "txt" | "log" => Self::Plain,
            "rs" | "rust" => Self::Rust,
            "py" | "python" | "py3" | "python3" => Self::Python,
            "sh" | "bash" | "zsh" | "ksh" | "fish" | "shell" | "console" => Self::Shell,
            "rb" | "ruby" => Self::Ruby,
            "js" | "javascript" | "jsx" | "mjs" | "cjs" | "ts" | "typescript" | "tsx" => Self::Js,
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "c++" | "cs" | "csharp" | "java"
            | "swift" | "kt" | "kotlin" | "scala" | "dart" | "m" | "mm" | "d" | "groovy"
            | "objc" => Self::CLike,
            "go" | "golang" => Self::Go,
            "php" | "phtml" => Self::Php,
            "css" | "scss" | "sass" | "less" => Self::Css,
            "html" | "htm" | "xml" | "svg" | "xhtml" | "plist" | "vue" => Self::Html,
            "json" | "jsonc" | "json5" => Self::Json,
            "sql" | "psql" | "mysql" | "sqlite" | "postgres" => Self::Sql,
            "lua" => Self::Lua,
            "toml" => Self::Toml,
            "yaml" | "yml" => Self::Yaml,
            "ini" | "cfg" | "conf" => Self::Ini,
            "md" | "markdown" => Self::Plain,
            _ => Self::Unknown,
        }
    }
}

/// A construct that continues from one line to the next. `Default` is "not
/// inside anything", which is where every pane starts.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct State {
    block: Option<Block>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    CComment { nested: bool, depth: u32 },
    HtmlComment,
    LuaComment,
    Triple(char),
    Backtick,
}

/// Tint one line. `state` carries open block comments and docstrings between
/// calls; a line with no continuation leaves it at `None`.
pub(crate) fn runs(
    line: &str,
    lang: Lang,
    state: &mut State,
    theme: &Theme,
) -> Vec<(String, Color)> {
    if lang == Lang::Plain {
        return vec![(line.to_string(), theme.fg)];
    }
    // A whole-line table header reads better as one key-colored run.
    if matches!(lang, Lang::Toml | Lang::Ini) {
        let t = line.trim();
        if t.len() > 2 && t.starts_with('[') && t.ends_with(']') {
            return vec![(line.to_string(), theme.json_key)];
        }
    }
    let chars: Vec<char> = line.chars().collect();
    let mut sink = Sink::new(theme);
    match lang {
        Lang::Html => html(&chars, state, &mut sink),
        Lang::Css => css(&chars, state, &mut sink),
        Lang::Json => json(&chars, state, &mut sink),
        Lang::Sql => sql(&chars, state, &mut sink),
        Lang::Lua => lua(&chars, state, &mut sink),
        _ => code(&chars, lang, state, &mut sink),
    }
    sink.finish()
}

/// Slice a line's tint runs to the char range `s..e`, merging adjacent pieces
/// of the same color. Used after wrapping, so a folded row keeps the colors of
/// the row it came from.
pub(crate) fn slice_runs(runs: &[(String, Color)], s: usize, e: usize) -> Vec<(String, Color)> {
    let mut out: Vec<(String, Color)> = Vec::new();
    let mut off = 0;
    for (text, color) in runs {
        let n = text.chars().count();
        let a = s.max(off);
        let b = e.min(off + n);
        if a < b {
            let piece: String = text.chars().skip(a - off).take(b - a).collect();
            match out.last_mut() {
                Some((last, last_color)) if *last_color == *color => last.push_str(&piece),
                _ => out.push((piece, *color)),
            }
        }
        off += n;
    }
    out
}

/// Accumulates runs, coalescing text that stays the same color.
struct Sink<'a> {
    theme: &'a Theme,
    runs: Vec<(String, Color)>,
    text: String,
    color: Color,
}

impl<'a> Sink<'a> {
    fn new(theme: &'a Theme) -> Self {
        Self {
            theme,
            runs: Vec::new(),
            text: String::new(),
            color: theme.fg,
        }
    }

    fn flush(&mut self) {
        if !self.text.is_empty() {
            self.runs.push((std::mem::take(&mut self.text), self.color));
        }
    }

    fn set(&mut self, color: Color) {
        if color != self.color {
            self.flush();
            self.color = color;
        }
    }

    fn write(&mut self, s: &str) {
        self.text.push_str(s);
    }

    fn write_char(&mut self, c: char) {
        self.text.push(c);
    }

    /// Back to the default text color. Every path that stops tinting has to
    /// go through one of these: a `write` after a colored run would otherwise
    /// inherit that color.
    fn plain(&mut self, s: &str) {
        self.set(self.theme.fg);
        self.write(s);
    }

    fn plain_char(&mut self, c: char) {
        self.set(self.theme.fg);
        self.write_char(c);
    }

    fn run(&mut self, s: &str, color: Color) {
        self.set(color);
        self.write(s);
    }

    fn chars(&mut self, chars: &[char], color: Color) {
        self.set(color);
        self.text.extend(chars.iter().copied());
    }

    fn finish(mut self) -> Vec<(String, Color)> {
        self.flush();
        if self.runs.is_empty() {
            self.runs.push((String::new(), self.theme.fg));
        }
        self.runs
    }
}

// ---------------------------------------------------------------------------
// Shared scanning helpers
// ---------------------------------------------------------------------------

fn starts_with(chars: &[char], i: usize, s: &str) -> bool {
    s.chars()
        .enumerate()
        .all(|(k, c)| chars.get(i + k) == Some(&c))
}

fn find_from(chars: &[char], from: usize, needle: &str) -> Option<usize> {
    (from..chars.len()).find(|&i| starts_with(chars, i, needle))
}

fn find_triple(chars: &[char], from: usize, q: char) -> Option<usize> {
    let mut i = from;
    while i + 2 < chars.len() {
        if chars[i] == q && chars[i + 1] == q && chars[i + 2] == q {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Closing backtick of a JS template literal, skipping escapes.
fn find_backtick(chars: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 2;
        } else if chars[i] == '`' {
            return Some(i);
        } else {
            i += 1;
        }
    }
    None
}

fn is_word_start(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn word_end(chars: &[char], i: usize) -> usize {
    let mut j = i;
    while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
        j += 1;
    }
    j
}

fn skip_space(chars: &[char], i: usize) -> usize {
    let mut j = i;
    while chars.get(j).is_some_and(|c| c.is_whitespace()) {
        j += 1;
    }
    j
}

fn is_call(chars: &[char], end: usize) -> bool {
    let k = skip_space(chars, end);
    chars.get(k) == Some(&'(') || (chars.get(k) == Some(&'!') && chars.get(k + 1) == Some(&'('))
}

/// End of a quoted run starting at `i` (including the closing quote), or the
/// line's end when it is cut off. `escapes` is false for shell/Ruby single
/// quotes, where a backslash is literal.
fn scan_string(chars: &[char], i: usize, quote: char, escapes: bool) -> usize {
    let mut j = i + 1;
    while j < chars.len() {
        if chars[j] == quote {
            // SQL doubles a quote to escape it.
            if quote == '\'' && chars.get(j + 1) == Some(&'\'') {
                j += 2;
                continue;
            }
            return j + 1;
        }
        if escapes && chars[j] == '\\' {
            j += 2;
            continue;
        }
        j += 1;
    }
    chars.len()
}

fn scan_number(chars: &[char], i: usize) -> usize {
    let mut j = i;
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
    j
}

/// End of a Rust raw string (`r"…"`, `r#"…"#`), or `None` when the `r` starts
/// an ordinary identifier.
fn raw_string_end(chars: &[char], i: usize) -> Option<usize> {
    let mut j = i + 1;
    let mut hashes = 0;
    while chars.get(j) == Some(&'#') {
        hashes += 1;
        j += 1;
    }
    if chars.get(j) != Some(&'"') {
        return None;
    }
    j += 1;
    while j < chars.len() {
        if chars[j] == '"' {
            let mut k = 0;
            while k < hashes && chars.get(j + 1 + k) == Some(&'#') {
                k += 1;
            }
            if k == hashes {
                return Some(j + 1 + hashes);
            }
        }
        j += 1;
    }
    Some(chars.len())
}

/// True when the `'` at `i` opens a Rust char literal rather than a lifetime.
fn rust_char_literal(chars: &[char], i: usize) -> bool {
    if chars.get(i + 1) == Some(&'\\') {
        let mut j = i + 2;
        while j < chars.len() && j <= i + 12 {
            if chars[j] == '\'' {
                return true;
            }
            j += 1;
        }
        false
    } else {
        chars.get(i + 2) == Some(&'\'')
    }
}

/// End of a shell `$` expansion: `$name`, `${…}`, `$(…)`, `$1`, `$?`.
fn dollar_end(chars: &[char], i: usize) -> usize {
    match chars.get(i + 1) {
        Some('{') => find_from(chars, i + 2, "}").map_or(chars.len(), |p| p + 1),
        Some('(') => {
            let mut depth = 1;
            let mut j = i + 2;
            while j < chars.len() {
                match chars[j] {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            return j + 1;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            chars.len()
        }
        Some(c) if is_word_start(*c) => word_end(chars, i + 1),
        Some(_) => i + 2,
        None => i + 1,
    }
}

/// Continue a block that was open at the start of this line. Returns the index
/// just past the construct, or the line length when it stays open.
fn consume_block(chars: &[char], state: &mut State, sink: &mut Sink<'_>) -> usize {
    let Some(block) = state.block else {
        return 0;
    };
    let theme = sink.theme;
    match block {
        Block::CComment { nested, mut depth } => {
            let mut i = 0;
            while i < chars.len() {
                if nested && starts_with(chars, i, "/*") {
                    depth += 1;
                    i += 2;
                    continue;
                }
                if starts_with(chars, i, "*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        sink.chars(&chars[..i], theme.muted);
                        state.block = None;
                        return i;
                    }
                    continue;
                }
                i += 1;
            }
            sink.chars(chars, theme.muted);
            state.block = Some(Block::CComment { nested, depth });
            chars.len()
        }
        Block::HtmlComment => {
            if let Some(p) = find_from(chars, 0, "-->") {
                sink.chars(&chars[..p + 3], theme.muted);
                state.block = None;
                p + 3
            } else {
                sink.chars(chars, theme.muted);
                chars.len()
            }
        }
        Block::LuaComment => {
            if let Some(p) = find_from(chars, 0, "]]") {
                sink.chars(&chars[..p + 2], theme.muted);
                state.block = None;
                p + 2
            } else {
                sink.chars(chars, theme.muted);
                chars.len()
            }
        }
        Block::Triple(q) => {
            if let Some(p) = find_triple(chars, 0, q) {
                sink.chars(&chars[..p + 3], theme.json_string);
                state.block = None;
                p + 3
            } else {
                sink.chars(chars, theme.json_string);
                chars.len()
            }
        }
        Block::Backtick => {
            if let Some(p) = find_backtick(chars, 0) {
                sink.chars(&chars[..p + 1], theme.json_string);
                state.block = None;
                p + 1
            } else {
                sink.chars(chars, theme.json_string);
                chars.len()
            }
        }
    }
}

/// Open a block comment at `i`. Returns the index past it, or records the
/// state when it does not end on this line.
fn open_block(
    chars: &[char],
    i: usize,
    open: &str,
    close: &str,
    nested: bool,
    state: &mut State,
    sink: &mut Sink<'_>,
) -> usize {
    let mut depth = 1u32;
    let mut j = i + open.chars().count();
    while j < chars.len() {
        if nested && starts_with(chars, j, open) {
            depth += 1;
            j += open.chars().count();
            continue;
        }
        if starts_with(chars, j, close) {
            depth -= 1;
            j += close.chars().count();
            if depth == 0 {
                sink.chars(&chars[i..j], sink.theme.muted);
                return j;
            }
            continue;
        }
        j += 1;
    }
    sink.chars(&chars[i..], sink.theme.muted);
    state.block = Some(Block::CComment { nested, depth });
    chars.len()
}

// ---------------------------------------------------------------------------
// Code families
// ---------------------------------------------------------------------------

const DQ_SQ: &[char] = &['"', '\''];
const DQ_SQ_BT: &[char] = &['"', '\'', '`'];
const LITERALS: &[&str] = &[
    "true",
    "false",
    "null",
    "nil",
    "None",
    "True",
    "False",
    "undefined",
    "NaN",
    "Infinity",
    "NULL",
    "TRUE",
    "FALSE",
];

const RUST_KEYWORDS: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "const",
    "continue",
    "crate",
    "dyn",
    "else",
    "enum",
    "extern",
    "fn",
    "for",
    "if",
    "impl",
    "in",
    "let",
    "loop",
    "macro",
    "macro_rules",
    "match",
    "mod",
    "move",
    "mut",
    "pub",
    "ref",
    "return",
    "self",
    "Self",
    "static",
    "struct",
    "super",
    "trait",
    "type",
    "union",
    "unsafe",
    "use",
    "where",
    "while",
    "yield",
];
const RUST_TYPES: &[&str] = &[
    "bool",
    "char",
    "str",
    "String",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
    "f32",
    "f64",
    "Option",
    "Result",
    "Vec",
    "Box",
    "Rc",
    "Arc",
    "HashMap",
    "HashSet",
    "BTreeMap",
    "BTreeSet",
    "Some",
    "Ok",
    "Err",
    "Default",
    "Clone",
    "Debug",
    "Display",
    "Iterator",
    "IntoIterator",
    "From",
    "Into",
    "TryFrom",
    "TryInto",
    "Send",
    "Sync",
];

const PY_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield", "match",
    "case", "self", "cls",
];
const PY_TYPES: &[&str] = &[
    "bool",
    "bytes",
    "dict",
    "float",
    "frozenset",
    "int",
    "list",
    "object",
    "set",
    "str",
    "tuple",
    "type",
    "print",
    "len",
    "range",
    "enumerate",
    "zip",
    "map",
    "filter",
    "sum",
    "min",
    "max",
    "sorted",
    "open",
    "super",
    "Exception",
    "ValueError",
    "TypeError",
    "KeyError",
    "IndexError",
];

const SH_KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "function", "select", "time", "in", "break", "continue", "return", "exit", "export", "local",
    "readonly", "declare", "unset", "shift", "set", "source", "alias", "eval", "exec", "trap",
    "wait", "echo", "printf", "cd", "pwd", "read", "test",
];

const RB_KEYWORDS: &[&str] = &[
    "alias",
    "and",
    "begin",
    "break",
    "case",
    "class",
    "def",
    "defined?",
    "do",
    "else",
    "elsif",
    "end",
    "ensure",
    "for",
    "if",
    "in",
    "module",
    "next",
    "not",
    "or",
    "redo",
    "rescue",
    "retry",
    "return",
    "self",
    "super",
    "then",
    "undef",
    "unless",
    "until",
    "when",
    "while",
    "yield",
    "require",
    "include",
    "extend",
    "attr_accessor",
    "attr_reader",
    "attr_writer",
    "puts",
    "print",
    "p",
    "raise",
    "new",
];
const RB_TYPES: &[&str] = &[
    "String",
    "Integer",
    "Float",
    "Array",
    "Hash",
    "Symbol",
    "NilClass",
    "TrueClass",
    "FalseClass",
    "Object",
    "Kernel",
    "File",
    "IO",
    "Time",
    "Range",
    "Regexp",
];

const C_KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "asm",
    "auto",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "constexpr",
    "continue",
    "decltype",
    "default",
    "delete",
    "do",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "extern",
    "final",
    "finally",
    "for",
    "foreach",
    "friend",
    "goto",
    "if",
    "implements",
    "import",
    "in",
    "inline",
    "instanceof",
    "interface",
    "lock",
    "namespace",
    "native",
    "new",
    "noexcept",
    "operator",
    "override",
    "package",
    "private",
    "protected",
    "public",
    "register",
    "reinterpret_cast",
    "return",
    "sizeof",
    "static",
    "static_cast",
    "struct",
    "super",
    "switch",
    "synchronized",
    "template",
    "this",
    "throw",
    "throws",
    "try",
    "typedef",
    "typeid",
    "typename",
    "union",
    "using",
    "virtual",
    "volatile",
    "while",
    "with",
    "yield",
    "async",
    "await",
    "func",
    "defer",
    "go",
    "chan",
    "range",
    "select",
    "fallthrough",
    "unsafe",
    "abstract",
    "assert",
    "readonly",
    "fixed",
    "params",
    "ref",
    "out",
    "is",
    "as",
    "typeof",
    "sizeof",
    "void",
];
const C_TYPES: &[&str] = &[
    "bool", "char", "double", "float", "int", "long", "short", "signed", "unsigned", "void",
    "wchar_t", "byte", "string", "String", "object", "var", "dynamic", "decimal", "uint", "ulong",
    "ushort", "sbyte", "nint", "nuint", "size_t", "ssize_t", "int8_t", "int16_t", "int32_t",
    "int64_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "Option", "Result", "Vec",
];

const JS_KEYWORDS: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "implements",
    "import",
    "in",
    "infer",
    "instanceof",
    "interface",
    "is",
    "keyof",
    "let",
    "namespace",
    "new",
    "of",
    "package",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "satisfies",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "try",
    "type",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "abstract",
    "asserts",
];
const JS_TYPES: &[&str] = &[
    "Array", "Boolean", "Date", "Error", "Function", "JSON", "Map", "Math", "Number", "Object",
    "Promise", "RegExp", "Set", "String", "Symbol", "WeakMap", "WeakSet", "console", "document",
    "window", "process", "Buffer", "require",
];

const SQL_KEYWORDS: &[&str] = &[
    "add",
    "all",
    "alter",
    "and",
    "any",
    "as",
    "asc",
    "begin",
    "between",
    "by",
    "case",
    "cast",
    "check",
    "coalesce",
    "column",
    "commit",
    "concat",
    "constraint",
    "count",
    "create",
    "database",
    "declare",
    "default",
    "delete",
    "desc",
    "distinct",
    "drop",
    "else",
    "end",
    "exists",
    "foreign",
    "from",
    "full",
    "function",
    "group",
    "having",
    "if",
    "in",
    "index",
    "inner",
    "insert",
    "into",
    "is",
    "join",
    "key",
    "left",
    "like",
    "limit",
    "not",
    "null",
    "offset",
    "on",
    "or",
    "order",
    "outer",
    "primary",
    "procedure",
    "references",
    "returning",
    "right",
    "rollback",
    "select",
    "set",
    "table",
    "then",
    "transaction",
    "trigger",
    "union",
    "unique",
    "update",
    "values",
    "view",
    "when",
    "where",
    "with",
    "avg",
    "min",
    "max",
    "sum",
];
const SQL_TYPES: &[&str] = &[
    "int",
    "integer",
    "bigint",
    "smallint",
    "varchar",
    "char",
    "text",
    "date",
    "timestamp",
    "time",
    "boolean",
    "bool",
    "numeric",
    "decimal",
    "real",
    "double",
    "serial",
    "uuid",
    "json",
    "jsonb",
    "blob",
    "float",
];

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in", "local",
    "not", "or", "repeat", "return", "then", "until", "while", "self", "require", "print", "pairs",
    "ipairs", "type", "tostring", "tonumber",
];

const GO_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
];
const GO_TYPES: &[&str] = &[
    "any",
    "bool",
    "byte",
    "comparable",
    "complex64",
    "complex128",
    "error",
    "float32",
    "float64",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "rune",
    "string",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
];

const PHP_KEYWORDS: &[&str] = &[
    "abstract",
    "and",
    "array",
    "as",
    "break",
    "callable",
    "case",
    "catch",
    "class",
    "clone",
    "const",
    "continue",
    "declare",
    "default",
    "do",
    "echo",
    "else",
    "elseif",
    "empty",
    "enddeclare",
    "endfor",
    "endforeach",
    "endif",
    "endswitch",
    "endwhile",
    "enum",
    "eval",
    "exit",
    "extends",
    "final",
    "finally",
    "fn",
    "for",
    "foreach",
    "function",
    "global",
    "goto",
    "if",
    "implements",
    "include",
    "include_once",
    "instanceof",
    "insteadof",
    "interface",
    "isset",
    "list",
    "match",
    "namespace",
    "new",
    "or",
    "print",
    "private",
    "protected",
    "public",
    "readonly",
    "require",
    "require_once",
    "return",
    "static",
    "switch",
    "throw",
    "trait",
    "try",
    "unset",
    "use",
    "var",
    "while",
    "xor",
    "yield",
];
const PHP_TYPES: &[&str] = &[
    "array",
    "bool",
    "callable",
    "float",
    "int",
    "iterable",
    "mixed",
    "never",
    "null",
    "object",
    "parent",
    "self",
    "static",
    "string",
    "void",
    "true",
    "false",
    "stdClass",
    "Exception",
    "Error",
    "Closure",
    "Generator",
    "DateTime",
];

struct Spec {
    line: &'static [&'static str],
    block: Option<(&'static str, &'static str, bool)>,
    quotes: &'static [char],
    backtick: bool,
    triple: bool,
    keywords: &'static [&'static str],
    types: &'static [&'static str],
    preproc: bool,
    at: bool,
    dollar: bool,
    hash_word_start: bool,
    config: bool,
    symbol_colon: bool,
}

impl Spec {
    fn of(lang: Lang) -> Self {
        let mut spec = Self {
            line: &[],
            block: None,
            quotes: DQ_SQ,
            backtick: false,
            triple: false,
            keywords: &[],
            types: &[],
            preproc: false,
            at: false,
            dollar: false,
            hash_word_start: false,
            config: false,
            symbol_colon: false,
        };
        match lang {
            Lang::Rust => {
                spec.line = &["//"];
                spec.block = Some(("/*", "*/", true));
                spec.keywords = RUST_KEYWORDS;
                spec.types = RUST_TYPES;
                spec.at = true;
            }
            Lang::Python => {
                spec.line = &["#"];
                spec.keywords = PY_KEYWORDS;
                spec.types = PY_TYPES;
                spec.triple = true;
                spec.at = true;
            }
            Lang::Shell => {
                spec.line = &["#"];
                spec.hash_word_start = true;
                spec.quotes = DQ_SQ_BT;
                spec.keywords = SH_KEYWORDS;
                spec.dollar = true;
            }
            Lang::Ruby => {
                spec.line = &["#"];
                spec.block = Some(("=begin", "=end", false));
                spec.quotes = DQ_SQ_BT;
                spec.keywords = RB_KEYWORDS;
                spec.types = RB_TYPES;
                spec.at = true;
                spec.dollar = true;
                spec.symbol_colon = true;
            }
            Lang::CLike => {
                spec.line = &["//"];
                spec.block = Some(("/*", "*/", false));
                spec.keywords = C_KEYWORDS;
                spec.types = C_TYPES;
                spec.preproc = true;
                spec.at = true;
            }
            Lang::Js => {
                spec.line = &["//"];
                spec.block = Some(("/*", "*/", false));
                spec.quotes = DQ_SQ_BT;
                spec.backtick = true;
                spec.keywords = JS_KEYWORDS;
                spec.types = JS_TYPES;
                spec.at = true;
            }
            Lang::Go => {
                spec.line = &["//"];
                spec.block = Some(("/*", "*/", false));
                spec.quotes = DQ_SQ;
                spec.backtick = true;
                spec.keywords = GO_KEYWORDS;
                spec.types = GO_TYPES;
            }
            Lang::Php => {
                spec.line = &["//", "#"];
                spec.block = Some(("/*", "*/", false));
                spec.quotes = DQ_SQ;
                spec.keywords = PHP_KEYWORDS;
                spec.types = PHP_TYPES;
                spec.dollar = true;
            }
            Lang::Sql | Lang::Lua => {
                // These have their own scanners (case-insensitive keywords,
                // Lua long strings), so no Spec is needed.
            }
            Lang::Toml | Lang::Yaml | Lang::Ini => {
                spec.line = if lang == Lang::Ini {
                    &[";", "#"]
                } else {
                    &["#"]
                };
                spec.config = true;
            }
            Lang::Unknown => {
                spec.line = &["//", "#"];
                spec.block = Some(("/*", "*/", false));
                spec.quotes = DQ_SQ_BT;
                spec.backtick = true;
            }
            Lang::Plain | Lang::Html | Lang::Css | Lang::Json => {}
        }
        spec
    }

    fn color(&self, word: &str, theme: &Theme) -> Option<Color> {
        let eq = |table: &[&str]| table.contains(&word);
        if eq(LITERALS) {
            Some(theme.json_literal)
        } else if eq(self.keywords) {
            Some(theme.json_key)
        } else if eq(self.types) {
            Some(theme.json_literal)
        } else {
            None
        }
    }
}

fn code(chars: &[char], lang: Lang, state: &mut State, sink: &mut Sink<'_>) {
    let spec = Spec::of(lang);
    let mut i = consume_block(chars, state, sink);
    if i >= chars.len() {
        return;
    }
    while i < chars.len() {
        let c = chars[i];
        // Line comments. `#` needs a word boundary in shell and in config
        // files, where `http://x/#anchor` or a hex color is not a comment.
        if let Some(marker) = spec.line.iter().find(|m| starts_with(chars, i, m)) {
            let needs_boundary = *marker == "#" && (spec.hash_word_start || spec.config);
            if !needs_boundary || i == 0 || chars[i - 1].is_whitespace() {
                sink.chars(&chars[i..], sink.theme.muted);
                return;
            }
        }
        if let Some((open, close, nested)) = spec.block {
            if starts_with(chars, i, open) {
                i = open_block(chars, i, open, close, nested, state, sink);
                continue;
            }
        }
        // C-family preprocessor directive at the start of a line.
        if spec.preproc && c == '#' && chars[..i].iter().all(|c| c.is_whitespace()) {
            let end = word_end(chars, i + 1);
            sink.chars(&chars[i..end.max(i + 1)], sink.theme.json_literal);
            i = end.max(i + 1);
            continue;
        }
        // Rust attributes: `#[derive(Debug)]` is a marker, not a comment.
        if spec.at && lang == Lang::Rust && c == '#' && chars.get(i + 1) == Some(&'[') {
            let end = word_end(chars, i + 2);
            sink.chars(&chars[i..end.max(i + 2)], sink.theme.json_literal);
            i = end.max(i + 2);
            continue;
        }
        // Python decorators, Ruby instance variables, Java annotations.
        if spec.at && c == '@' {
            let end = word_end(chars, i + 1);
            if end > i + 1 {
                sink.chars(&chars[i..end], sink.theme.json_literal);
                i = end;
                continue;
            }
        }
        // Ruby symbols: `:name`.
        if spec.symbol_colon && c == ':' && chars.get(i + 1).is_some_and(|c| is_word_start(*c)) {
            let end = word_end(chars, i + 1);
            sink.chars(&chars[i..end], sink.theme.json_literal);
            i = end;
            continue;
        }
        if spec.dollar && c == '$' {
            let end = dollar_end(chars, i);
            sink.chars(&chars[i..end], sink.theme.json_literal);
            i = end;
            continue;
        }
        if lang == Lang::Rust && c == 'r' {
            if let Some(end) = raw_string_end(chars, i) {
                sink.chars(&chars[i..end], sink.theme.json_string);
                i = end;
                continue;
            }
        }
        // Python docstrings / triple-quoted strings, with continuation.
        if spec.triple
            && matches!(c, '"' | '\'')
            && chars.get(i + 1) == Some(&c)
            && chars.get(i + 2) == Some(&c)
        {
            if let Some(p) = find_triple(chars, i + 3, c) {
                sink.chars(&chars[i..p + 3], sink.theme.json_string);
                i = p + 3;
            } else {
                sink.chars(&chars[i..], sink.theme.json_string);
                state.block = Some(Block::Triple(c));
                return;
            }
            continue;
        }
        // A JS/TS template literal, which may span lines.
        if spec.backtick && c == '`' {
            if let Some(p) = find_backtick(chars, i + 1) {
                sink.chars(&chars[i..p + 1], sink.theme.json_string);
                i = p + 1;
            } else {
                sink.chars(&chars[i..], sink.theme.json_string);
                state.block = Some(Block::Backtick);
                return;
            }
            continue;
        }
        // Quoted runs. A Rust `'` that is not a char literal is a lifetime.
        if spec.quotes.contains(&c) {
            if lang == Lang::Rust && c == '\'' && !rust_char_literal(chars, i) {
                sink.plain_char(c);
                i += 1;
                continue;
            }
            let escapes = !(lang == Lang::Shell && c == '\'');
            let end = scan_string(chars, i, c, escapes);
            sink.chars(&chars[i..end], sink.theme.json_string);
            i = end;
            continue;
        }
        if c.is_ascii_digit() {
            let end = scan_number(chars, i);
            sink.chars(&chars[i..end], sink.theme.json_number);
            i = end;
            continue;
        }
        if is_word_start(c) {
            let end = word_end(chars, i);
            let word: String = chars[i..end].iter().collect();
            let color = spec.color(&word, sink.theme);
            if let Some(color) = color {
                sink.run(&word, color);
            } else if spec.config && is_config_key(chars, end) {
                sink.run(&word, sink.theme.json_key);
            } else if is_call(chars, end) {
                sink.run(&word, sink.theme.accent);
            } else {
                sink.plain(&word);
            }
            i = end;
            continue;
        }
        sink.plain_char(c);
        i += 1;
    }
}

/// A config key sits immediately before `=` or a YAML-style `: `.
fn is_config_key(chars: &[char], end: usize) -> bool {
    let k = skip_space(chars, end);
    match chars.get(k) {
        Some('=') => true,
        Some(':') => {
            let after = skip_space(chars, k + 1);
            chars.get(after).is_none() || after > k + 1
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// HTML / XML
// ---------------------------------------------------------------------------

fn html(chars: &[char], state: &mut State, sink: &mut Sink<'_>) {
    let mut i = consume_block(chars, state, sink);
    while i < chars.len() {
        if starts_with(chars, i, "<!--") {
            if let Some(p) = find_from(chars, i + 4, "-->") {
                sink.chars(&chars[i..p + 3], sink.theme.muted);
                i = p + 3;
            } else {
                sink.chars(&chars[i..], sink.theme.muted);
                state.block = Some(Block::HtmlComment);
                return;
            }
            continue;
        }
        if chars[i] == '<' {
            // Only a tag opener starts a tag: `a < b` in HTML text is not one.
            let next = chars.get(i + 1).copied();
            let tag_like = matches!(next, Some('/') | Some('!') | Some('?'))
                || next.is_some_and(|c| c.is_ascii_alphabetic());
            if !tag_like {
                sink.plain_char('<');
                i += 1;
                continue;
            }
            let mut j = i + 1;
            let mut quote = None;
            while j < chars.len() {
                let c = chars[j];
                if let Some(q) = quote {
                    if c == q {
                        quote = None;
                    }
                } else if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c == '>' {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let end = j.min(chars.len());
            tag_runs(&chars[i..end], sink);
            i = end;
            continue;
        }
        let mut j = i;
        while j < chars.len() && chars[j] != '<' && !starts_with(chars, j, "<!--") {
            j += 1;
        }
        sink.chars(&chars[i..j], sink.theme.fg);
        i = j;
    }
}

/// One `<tag …>` run: tag name key-colored, attributes literal-colored, values
/// as strings.
fn tag_runs(tag: &[char], sink: &mut Sink<'_>) {
    let mut i = 0;
    let mut named = false;
    while i < tag.len() {
        let c = tag[i];
        if c == '"' || c == '\'' {
            let end = scan_string(tag, i, c, true);
            sink.chars(&tag[i..end], sink.theme.json_string);
            i = end;
            continue;
        }
        if is_word_start(c) {
            let end = word_end(tag, i);
            let word: String = tag[i..end].iter().collect();
            let color = if named {
                sink.theme.json_literal
            } else {
                sink.theme.json_key
            };
            sink.run(&word, color);
            named = true;
            i = end;
            continue;
        }
        sink.plain_char(c);
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// CSS
// ---------------------------------------------------------------------------

fn css(chars: &[char], state: &mut State, sink: &mut Sink<'_>) {
    let mut i = consume_block(chars, state, sink);
    while i < chars.len() {
        if starts_with(chars, i, "/*") {
            i = open_block(chars, i, "/*", "*/", false, state, sink);
            continue;
        }
        let c = chars[i];
        if c == '"' || c == '\'' {
            let end = scan_string(chars, i, c, true);
            sink.chars(&chars[i..end], sink.theme.json_string);
            i = end;
            continue;
        }
        if c == '@' {
            let end = word_end(chars, i + 1);
            sink.chars(&chars[i..end.max(i + 1)], sink.theme.json_literal);
            i = end.max(i + 1);
            continue;
        }
        if c == '#' && chars.get(i + 1).is_some_and(|c| c.is_ascii_hexdigit()) {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_alphanumeric() {
                j += 1;
            }
            sink.chars(&chars[i..j], sink.theme.json_literal);
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let end = scan_number(chars, i);
            sink.chars(&chars[i..end], sink.theme.json_number);
            i = end;
            continue;
        }
        // Custom properties are names too: `--accent: #0969da`.
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            let mut j = i + 2;
            while j < chars.len() && (chars[j].is_alphanumeric() || matches!(chars[j], '-' | '_')) {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            if css_property(chars, i, j) {
                sink.run(&word, sink.theme.json_key);
            } else {
                sink.plain(&word);
            }
            i = j;
            continue;
        }
        if is_word_start(c) {
            let end = word_end(chars, i);
            let word: String = chars[i..end].iter().collect();
            if css_property(chars, i, end) {
                sink.run(&word, sink.theme.json_key);
            } else if is_call(chars, end) {
                sink.run(&word, sink.theme.accent);
            } else {
                sink.plain(&word);
            }
            i = end;
            continue;
        }
        sink.plain_char(c);
        i += 1;
    }
}

/// A CSS property name starts a declaration (`color: …`) rather than a
/// selector (`a:hover`): the word sits at a declaration boundary and is
/// followed by a colon.
fn css_property(chars: &[char], start: usize, end: usize) -> bool {
    let k = skip_space(chars, end);
    if chars.get(k) != Some(&':') {
        return false;
    }
    let mut p = start;
    while p > 0 && chars[p - 1].is_whitespace() {
        p -= 1;
    }
    p == 0 || matches!(chars.get(p - 1), Some('{') | Some(';') | Some('}'))
}

// ---------------------------------------------------------------------------
// JSON / JSONC
// ---------------------------------------------------------------------------

fn json(chars: &[char], state: &mut State, sink: &mut Sink<'_>) {
    let mut i = consume_block(chars, state, sink);
    while i < chars.len() {
        if starts_with(chars, i, "//") {
            sink.chars(&chars[i..], sink.theme.muted);
            return;
        }
        if starts_with(chars, i, "/*") {
            i = open_block(chars, i, "/*", "*/", false, state, sink);
            continue;
        }
        let c = chars[i];
        if c == '"' {
            let end = scan_string(chars, i, c, true);
            let after = skip_space(chars, end);
            let color = if chars.get(after) == Some(&':') {
                sink.theme.json_key
            } else {
                sink.theme.json_string
            };
            sink.chars(&chars[i..end], color);
            i = end;
            continue;
        }
        if c.is_ascii_digit() || (c == '-' && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit()))
        {
            let end = scan_number(chars, i);
            sink.chars(&chars[i..end], sink.theme.json_number);
            i = end;
            continue;
        }
        if is_word_start(c) {
            let end = word_end(chars, i);
            let word: String = chars[i..end].iter().collect();
            if LITERALS.contains(&word.as_str()) {
                sink.run(&word, sink.theme.json_literal);
            } else {
                sink.plain(&word);
            }
            i = end;
            continue;
        }
        sink.plain_char(c);
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// SQL / Lua
// ---------------------------------------------------------------------------

fn sql(chars: &[char], state: &mut State, sink: &mut Sink<'_>) {
    let mut i = consume_block(chars, state, sink);
    while i < chars.len() {
        if starts_with(chars, i, "--") {
            sink.chars(&chars[i..], sink.theme.muted);
            return;
        }
        if starts_with(chars, i, "/*") {
            i = open_block(chars, i, "/*", "*/", false, state, sink);
            continue;
        }
        let c = chars[i];
        if c == '\'' || c == '"' {
            let end = scan_string(chars, i, c, true);
            let color = if c == '\'' {
                sink.theme.json_string
            } else {
                sink.theme.json_key
            };
            sink.chars(&chars[i..end], color);
            i = end;
            continue;
        }
        if c.is_ascii_digit() {
            let end = scan_number(chars, i);
            sink.chars(&chars[i..end], sink.theme.json_number);
            i = end;
            continue;
        }
        if is_word_start(c) {
            let end = word_end(chars, i);
            let word: String = chars[i..end].iter().collect();
            if LITERALS.iter().any(|k| k.eq_ignore_ascii_case(&word)) {
                sink.run(&word, sink.theme.json_literal);
            } else if SQL_KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(&word)) {
                sink.run(&word, sink.theme.json_key);
            } else if SQL_TYPES.iter().any(|k| k.eq_ignore_ascii_case(&word)) {
                sink.run(&word, sink.theme.json_literal);
            } else if is_call(chars, end) {
                sink.run(&word, sink.theme.accent);
            } else {
                sink.plain(&word);
            }
            i = end;
            continue;
        }
        sink.plain_char(c);
        i += 1;
    }
}

fn lua(chars: &[char], state: &mut State, sink: &mut Sink<'_>) {
    let mut i = consume_block(chars, state, sink);
    while i < chars.len() {
        if starts_with(chars, i, "--[[") {
            if let Some(p) = find_from(chars, i + 4, "]]") {
                sink.chars(&chars[i..p + 2], sink.theme.muted);
                i = p + 2;
            } else {
                sink.chars(&chars[i..], sink.theme.muted);
                state.block = Some(Block::LuaComment);
                return;
            }
            continue;
        }
        if starts_with(chars, i, "--") {
            sink.chars(&chars[i..], sink.theme.muted);
            return;
        }
        let c = chars[i];
        if c == '"' || c == '\'' {
            let end = scan_string(chars, i, c, true);
            sink.chars(&chars[i..end], sink.theme.json_string);
            i = end;
            continue;
        }
        if c.is_ascii_digit() {
            let end = scan_number(chars, i);
            sink.chars(&chars[i..end], sink.theme.json_number);
            i = end;
            continue;
        }
        if is_word_start(c) {
            let end = word_end(chars, i);
            let word: String = chars[i..end].iter().collect();
            if LITERALS.contains(&word.as_str()) {
                sink.run(&word, sink.theme.json_literal);
            } else if LUA_KEYWORDS.contains(&word.as_str()) {
                sink.run(&word, sink.theme.json_key);
            } else if is_call(chars, end) {
                sink.run(&word, sink.theme.accent);
            } else {
                sink.plain(&word);
            }
            i = end;
            continue;
        }
        sink.plain_char(c);
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::theme::Theme;

    fn tint(line: &str, lang: Lang, state: &mut State) -> Vec<(String, Color)> {
        runs(line, lang, state, &Theme::light())
    }

    fn text_of(runs: &[(String, Color)], color: Color) -> String {
        runs.iter()
            .filter(|(_, c)| *c == color)
            .map(|(s, _)| s.as_str())
            .collect()
    }

    #[test]
    fn hints_map_aliases_and_families() {
        for h in ["rs", "rust", "language-rust", ".rs", "rust,ignore"] {
            assert_eq!(Lang::from_hint(h), Lang::Rust, "{h}");
        }
        for h in ["js", "JavaScript", "ts", "typescript"] {
            assert_eq!(Lang::from_hint(h), Lang::Js, "{h}");
        }
        assert_eq!(Lang::from_hint("c++"), Lang::CLike);
        assert_eq!(Lang::from_hint("go"), Lang::Go);
        assert_eq!(Lang::from_hint("php"), Lang::Php);
        assert_eq!(Lang::from_hint("html"), Lang::Html);
        assert_eq!(Lang::from_hint("yml"), Lang::Yaml);
        assert_eq!(Lang::from_hint(""), Lang::Plain);
        assert_eq!(Lang::from_hint("brainfuck"), Lang::Unknown);
    }

    #[test]
    fn rust_attributes_lifetimes_and_literals() {
        let t = Theme::light();
        let mut st = State::default();
        let runs = tint("#[derive(Debug)]", Lang::Rust, &mut st);
        assert!(
            runs.iter().all(|(_, c)| *c != t.muted),
            "attribute treated as a comment: {runs:?}"
        );
        assert!(
            runs.iter()
                .any(|(s, c)| *c == t.json_literal && s.contains("derive")),
            "attribute not tinted as a marker: {runs:?}"
        );
        let runs = tint("fn f<'a>(x: &'a str) -> &'a str {", Lang::Rust, &mut st);
        assert!(
            runs.iter().all(|(_, c)| *c != t.json_string),
            "lifetime painted as a string: {runs:?}"
        );
        let runs = tint("let c = 'x';", Lang::Rust, &mut st);
        assert!(
            runs.iter().any(|(_, c)| *c == t.json_string),
            "char literal not tinted: {runs:?}"
        );
        let runs = tint("let n = 0x1F + 3.14 - 1e-9;", Lang::Rust, &mut st);
        assert_eq!(
            runs.iter().filter(|(_, c)| *c == t.json_number).count(),
            3,
            "numbers: {runs:?}"
        );
        // Function names read as calls, keywords do not.
        let runs = tint("let x = parse(a);", Lang::Rust, &mut st);
        assert!(
            runs.iter().any(|(s, c)| *c == t.accent && s == "parse"),
            "function name not accented: {runs:?}"
        );
    }

    #[test]
    fn rust_block_comments_carry_across_lines() {
        let t = Theme::light();
        let mut st = State::default();
        let first = tint("let x = 1; /* start", Lang::Rust, &mut st);
        assert!(st.block.is_some(), "block comment not left open");
        assert!(
            first.iter().any(|(_, c)| *c == t.muted),
            "opening comment not tinted: {first:?}"
        );
        let second = tint("end */ let y = 2;", Lang::Rust, &mut st);
        assert!(st.block.is_none(), "block comment did not close");
        assert!(
            second
                .iter()
                .any(|(s, c)| *c == t.muted && s.contains("end */")),
            "closing comment not tinted: {second:?}"
        );
        assert!(
            second.iter().any(|(s, c)| *c == t.json_key && s == "let"),
            "code after the comment lost: {second:?}"
        );
    }

    #[test]
    fn python_docstrings_continue_and_close() {
        let t = Theme::light();
        let mut st = State::default();
        let first = tint("    \"\"\"a docstring", Lang::Python, &mut st);
        assert!(
            text_of(&first, t.json_string).contains("a docstring"),
            "docstring not tinted: {first:?}"
        );
        assert!(st.block.is_some(), "docstring not left open");
        let second = tint("    still inside\"\"\" + x", Lang::Python, &mut st);
        assert!(st.block.is_none(), "docstring did not close");
        assert!(
            text_of(&second, t.json_string).contains("still inside"),
            "docstring tail not tinted: {second:?}"
        );
    }

    #[test]
    fn html_names_tags_attributes_and_comments() {
        let t = Theme::light();
        let mut st = State::default();
        let runs = tint(r#"<a href="x">link</a>"#, Lang::Html, &mut st);
        assert!(
            runs.iter().any(|(s, c)| *c == t.json_key && s == "a"),
            "tag name: {runs:?}"
        );
        assert!(
            runs.iter()
                .any(|(s, c)| *c == t.json_literal && s == "href"),
            "attribute: {runs:?}"
        );
        assert!(
            runs.iter()
                .any(|(s, c)| *c == t.json_string && s == "\"x\""),
            "attribute value: {runs:?}"
        );
        // An HTML comment opens a block that the next line closes.
        let first = tint("<!-- note", Lang::Html, &mut st);
        assert!(st.block.is_some());
        assert!(
            first.iter().any(|(_, c)| *c == t.muted),
            "comment open: {first:?}"
        );
        let second = tint("closed --> text", Lang::Html, &mut st);
        assert!(st.block.is_none());
        assert!(
            second
                .iter()
                .any(|(s, c)| *c == t.muted && s.contains("closed -->")),
            "comment close: {second:?}"
        );
        assert!(
            second.iter().any(|(s, c)| *c == t.fg && s.contains("text")),
            "text after comment: {second:?}"
        );
        // A comparison in text is not a tag.
        let text = tint("a < b", Lang::Html, &mut st);
        assert!(
            text.iter().all(|(_, c)| *c == t.fg),
            "comparison became a tag: {text:?}"
        );
    }

    #[test]
    fn json_distinguishes_keys_from_string_values() {
        let t = Theme::light();
        let mut st = State::default();
        let runs = tint(
            r#"{"key": "value", "n": 1, "b": true}"#,
            Lang::Json,
            &mut st,
        );
        assert_eq!(
            text_of(&runs, t.json_key),
            "\"key\"\"n\"\"b\"",
            "keys: {runs:?}"
        );
        assert_eq!(
            text_of(&runs, t.json_string),
            "\"value\"",
            "value: {runs:?}"
        );
        assert_eq!(text_of(&runs, t.json_number), "1", "number: {runs:?}");
        assert_eq!(text_of(&runs, t.json_literal), "true", "literal: {runs:?}");
    }

    #[test]
    fn css_properties_hex_colors_and_comments() {
        let t = Theme::light();
        let mut st = State::default();
        let runs = tint(
            "--accent: #0969da; color: #ff0; /* note */",
            Lang::Css,
            &mut st,
        );
        assert!(
            runs.iter()
                .any(|(s, c)| *c == t.json_key && s == "--accent"),
            "custom property: {runs:?}"
        );
        assert!(
            runs.iter().any(|(s, c)| *c == t.json_key && s == "color"),
            "property: {runs:?}"
        );
        assert!(
            runs.iter()
                .any(|(s, c)| *c == t.json_literal && s == "#ff0"),
            "hex color: {runs:?}"
        );
        assert!(
            runs.iter()
                .any(|(s, c)| *c == t.muted && s.contains("note")),
            "comment: {runs:?}"
        );
    }

    #[test]
    fn sql_is_case_insensitive_and_takes_line_comments() {
        let t = Theme::light();
        let mut st = State::default();
        let runs = tint("SELECT id FROM users -- hi", Lang::Sql, &mut st);
        assert!(
            runs.iter().any(|(s, c)| *c == t.json_key && s == "SELECT"),
            "keyword: {runs:?}"
        );
        assert!(
            runs.iter().any(|(s, c)| *c == t.json_key && s == "FROM"),
            "keyword: {runs:?}"
        );
        assert!(
            runs.iter()
                .any(|(s, c)| *c == t.muted && s.contains("-- hi")),
            "comment: {runs:?}"
        );
    }

    #[test]
    fn shell_variables_and_word_start_comments() {
        let t = Theme::light();
        let mut st = State::default();
        let runs = tint("echo $HOME # home", Lang::Shell, &mut st);
        assert!(
            runs.iter()
                .any(|(s, c)| *c == t.json_literal && s == "$HOME"),
            "variable: {runs:?}"
        );
        assert!(
            runs.iter()
                .any(|(s, c)| *c == t.muted && s.contains("# home")),
            "comment: {runs:?}"
        );
        // `#` inside a word is not a comment.
        let runs = tint("echo a#b", Lang::Shell, &mut st);
        assert!(
            runs.iter().all(|(_, c)| *c != t.muted),
            "mid-word # became a comment: {runs:?}"
        );
    }

    #[test]
    fn config_keys_and_section_headers() {
        let t = Theme::light();
        let mut st = State::default();
        let runs = tint("port = 8080", Lang::Toml, &mut st);
        assert!(
            runs.iter().any(|(s, c)| *c == t.json_key && s == "port"),
            "key: {runs:?}"
        );
        assert!(
            runs.iter().any(|(s, c)| *c == t.json_number && s == "8080"),
            "number value: {runs:?}"
        );
        let runs = tint("[server.tls]", Lang::Toml, &mut st);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].1, t.json_key);
        // A `#` inside a token is not a comment in YAML.
        let runs = tint("url: http://x/#anchor", Lang::Yaml, &mut st);
        assert!(
            runs.iter().all(|(_, c)| *c != t.muted),
            "URL fragment became a comment: {runs:?}"
        );
    }

    #[test]
    fn unknown_hints_still_tint_comments_strings_and_numbers() {
        let t = Theme::light();
        let mut st = State::default();
        let runs = tint(r#"x = "str" 42 // note"#, Lang::Unknown, &mut st);
        assert!(runs.iter().any(|(_, c)| *c == t.muted), "comment: {runs:?}");
        assert!(
            runs.iter().any(|(_, c)| *c == t.json_string),
            "string: {runs:?}"
        );
        assert!(
            runs.iter().any(|(_, c)| *c == t.json_number),
            "number: {runs:?}"
        );
    }

    #[test]
    fn rust_nested_block_comments_close_at_the_right_depth() {
        let t = Theme::light();
        let mut st = State::default();
        let runs = tint(
            "/* outer /* inner */ still outer */ let x = 1;",
            Lang::Rust,
            &mut st,
        );
        assert!(st.block.is_none(), "nested comment left open");
        assert!(
            runs.iter()
                .any(|(s, c)| *c == t.muted && s.contains("still outer")),
            "inner close ended the outer comment: {runs:?}"
        );
        assert!(
            runs.iter().any(|(s, c)| *c == t.json_key && s == "let"),
            "code after nested comment: {runs:?}"
        );
    }

    #[test]
    fn javascript_template_literals_cross_lines() {
        let t = Theme::light();
        let mut st = State::default();
        let first = tint("const s = `line one", Lang::Js, &mut st);
        assert!(st.block.is_some(), "template literal not left open");
        assert!(
            first.iter().any(|(_, c)| *c == t.json_string),
            "template start: {first:?}"
        );
        let second = tint("line two`;", Lang::Js, &mut st);
        assert!(st.block.is_none(), "template literal did not close");
        assert!(
            second
                .iter()
                .any(|(s, c)| *c == t.json_string && s.contains("line two")),
            "template tail: {second:?}"
        );
    }

    #[test]
    fn python_fstring_stays_one_string_run() {
        let t = Theme::light();
        let mut st = State::default();
        let runs = tint(
            r#"print(f"hello {name} {0x1F} {3.14}")"#,
            Lang::Python,
            &mut st,
        );
        let strings = text_of(&runs, t.json_string);
        assert!(
            strings.contains("{0x1F}") && strings.contains("{3.14}"),
            "f-string body was re-tokenized: {runs:?}"
        );
    }

    #[test]
    fn go_raw_strings_and_php_variables() {
        let t = Theme::light();
        let mut st = State::default();
        // Go's backtick raw strings are one run.
        let runs = tint("msg := `raw text`", Lang::Go, &mut st);
        assert!(
            runs.iter()
                .any(|(s, c)| *c == t.json_string && s == "`raw text`"),
            "Go raw string: {runs:?}"
        );
        // PHP variables are their own tint.
        let runs = tint("$name = 'x';", Lang::Php, &mut st);
        assert!(
            runs.iter()
                .any(|(s, c)| *c == t.json_literal && s == "$name"),
            "PHP variable: {runs:?}"
        );
    }

    #[test]
    fn plain_text_is_one_run() {
        let t = Theme::light();
        let mut st = State::default();
        let runs = tint("no tint here", Lang::Plain, &mut st);
        assert_eq!(runs, vec![("no tint here".to_string(), t.fg)]);
    }

    #[test]
    fn slicing_runs_keeps_offsets_and_merges_colors() {
        let runs = vec![
            ("abc".to_string(), Color::rgb(1, 1, 1)),
            ("def".to_string(), Color::rgb(2, 2, 2)),
        ];
        let sliced = slice_runs(&runs, 2, 5);
        assert_eq!(sliced.len(), 2);
        assert_eq!(sliced[0].0, "c");
        assert_eq!(sliced[1].0, "de");
        // A range wholly inside one run keeps that run's color.
        let sliced = slice_runs(&runs, 4, 6);
        assert_eq!(sliced, vec![("ef".to_string(), Color::rgb(2, 2, 2))]);
    }
}
