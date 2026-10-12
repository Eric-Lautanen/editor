//! 2-space pretty printer. Output is canonical for a value: object key order is
//! preserved (never sorted), numbers keep their source text, and empty
//! containers stay inline so `{}` round-trips.

use super::Value;

/// A string rendered as JSON source text, quotes included.
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::new();
    escape_into(&mut out, s);
    out
}

fn escape_into(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write(value: &Value, indent: usize, out: &mut String) {
    let pad = |n: usize| "  ".repeat(n);
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(n),
        Value::String(s) => escape_into(out, s),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Object(pairs) if pairs.is_empty() => out.push_str("{}"),
        Value::Array(items) => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                write(item, indent + 1, out);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(indent));
            out.push(']');
        }
        Value::Object(pairs) => {
            out.push_str("{\n");
            for (i, (k, v)) in pairs.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                escape_into(out, k);
                out.push_str(": ");
                write(v, indent + 1, out);
                if i + 1 < pairs.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(indent));
            out.push('}');
        }
    }
}

pub fn pretty(value: &Value) -> String {
    let mut out = String::new();
    write(value, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::super::parser;
    use super::*;

    #[test]
    fn pretty_output_is_canonical() {
        let v = parser::parse(r#"{"b":[1,{"a":null}],"a":{},"e":[]}"#).unwrap();
        assert_eq!(
            pretty(&v),
            "{\n  \"b\": [\n    1,\n    {\n      \"a\": null\n    }\n  ],\n  \"a\": {},\n  \"e\": []\n}"
        );
    }

    #[test]
    fn pretty_round_trips() {
        for src in [
            "{\"a\": 1}",
            "[1, \"x\\n\", true, false, null, -0.5e-2]",
            "{\"snowman\": \"☃\", \"slash\": \"a\\/b\"}",
        ] {
            let v = parser::parse(src).unwrap();
            let again = parser::parse(&pretty(&v)).unwrap();
            assert_eq!(v, again, "round trip failed for {src}");
        }
    }

    #[test]
    fn control_chars_escape() {
        let v = Value::String("a\x01b".into());
        assert_eq!(pretty(&v), "\"a\\u0001b\"");
    }
}
