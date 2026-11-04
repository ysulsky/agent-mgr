//! JSON text, in the forms Agent Mgr writes:
//! - `encode`: the record files. One-space indentation, non-ASCII kept as it is, and a
//!   trailing newline.
//! - `encode_compact`: one line, non-ASCII kept as it is: JSON put on a backend's command
//!   line, and values shown in the turn viewer.
//! - `encode_ascii`: one line, ASCII-only: HTTP response bodies, SSE events, terminal
//!   socket messages, and the files the turn and proc supervisors write.
//! - `encode_with`: any indentation, ASCII-only or not, with no trailing newline, for the
//!   rest (`am health --json`, `am agent show`, link artifacts, the offload archive's
//!   manifest, ...).
//!
//! Layout. On one line, items are separated by `, ` and each key from its value by `: `.
//! With an indentation of n, each item of an array or object goes on its own line, indented
//! n spaces per level of nesting, with a `,` after every item but the last; a key is still
//! followed by `: `, and the closing bracket goes on its own line at its parent's
//! indentation. An empty array or object is `[]` or `{}` either way. Keys keep their order.
//!
//! Strings. `"` and `\` are escaped with a backslash; newline, carriage return, tab,
//! backspace and form feed are written as `\n`, `\r`, `\t`, `\b`, `\f`; any other character
//! below U+0020 is `\u00XX`. Everything else is kept as it is, except in the ASCII-only
//! forms, which write every character outside printable ASCII (space to `~`) as `\uXXXX`
//! in lowercase hex, a character above U+FFFF as its UTF-16 surrogate pair (U+1F600 is
//! `\ud83d\ude00`).
//!
//! Numbers. Integers as integers, floats as `format::float_text` writes them (`0.1`, `1.0`,
//! `1e+16`, `1e-05`), and non-finite floats as `NaN`, `Infinity` and `-Infinity`.
//! `null`, `true` and `false` as themselves.

use crate::format::float_text;
use serde_json::{Number, Value};

fn dump_str(s: &str, ascii: bool, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        if ascii && !(' '..='~').contains(&c) && !matches!(c, '\n' | '\r' | '\t' | '\x08' | '\x0c') {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{:04x}", unit));
            }
            continue;
        }
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A JSON number: integers as integers, floats as `format::float_text`. (An integer
/// beyond 64 bits was parsed as a float, so it's written as one.)
pub fn number(n: &Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        let f = n.as_f64().unwrap_or(f64::NAN);
        if f.is_nan() {
            "NaN".into()
        } else if f.is_infinite() {
            (if f > 0.0 { "Infinity" } else { "-Infinity" }).into()
        } else {
            float_text(f)
        }
    }
}

fn dump_at(v: &Value, indent: Option<usize>, ascii: bool, depth: usize, out: &mut String) {
    let newline = |out: &mut String, depth: usize| {
        if let Some(n) = indent {
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', n * depth));
        }
    };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&number(n)),
        Value::String(s) => dump_str(s, ascii, out),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Object(items) if items.is_empty() => out.push_str("{}"),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(if indent.is_some() { "," } else { ", " });
                }
                newline(out, depth + 1);
                dump_at(item, indent, ascii, depth + 1, out);
            }
            newline(out, depth);
            out.push(']');
        }
        Value::Object(items) => {
            out.push('{');
            for (i, (k, item)) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(if indent.is_some() { "," } else { ", " });
                }
                newline(out, depth + 1);
                dump_str(k, ascii, out);
                out.push_str(": ");
                dump_at(item, indent, ascii, depth + 1, out);
            }
            newline(out, depth);
            out.push('}');
        }
    }
}

/// `v` with `indent` spaces per level (or on one line), ASCII-only if `ensure_ascii`, with
/// no trailing newline. With `indent`, items are separated by `,` and a newline; without,
/// by `, `.
pub fn encode_with(v: &Value, indent: Option<usize>, ensure_ascii: bool) -> String {
    let mut out = String::new();
    dump_at(v, indent, ensure_ascii, 0, &mut out);
    out
}

/// A record, as `write_json` writes it: one-space indentation, non-ASCII kept, and a
/// trailing newline.
pub fn encode(v: &Value) -> String {
    let mut out = encode_with(v, Some(1), false);
    out.push('\n');
    out
}

/// One line, with non-ASCII kept as it is.
pub fn encode_compact(v: &Value) -> String {
    encode_with(v, None, false)
}

/// One line, with everything outside printable ASCII escaped.
pub fn encode_ascii(v: &Value) -> String {
    encode_with(v, None, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn indent_1_layout() {
        let v = json!({"a": 1, "b": [true, null, 0.1, 1e16, 1e-5], "c": {"d": "x\ny\u{1}"}, "e": []});
        let want = "{\n \"a\": 1,\n \"b\": [\n  true,\n  null,\n  0.1,\n  1e+16,\n  1e-05\n ],\n \"c\": {\n  \"d\": \"x\\ny\\u0001\"\n },\n \"e\": []\n}\n";
        assert_eq!(encode(&v), want);
    }

    #[test]
    fn ascii_escapes() {
        let v = json!({"é": "a\u{7f}😀\n\u{1}", "n": [1.5, -0.0]});
        assert_eq!(encode_ascii(&v), r#"{"\u00e9": "a\u007f\ud83d\ude00\n\u0001", "n": [1.5, -0.0]}"#);
        assert_eq!(encode_with(&json!([1, {"a": "é"}]), Some(2), true), "[\n  1,\n  {\n    \"a\": \"\\u00e9\"\n  }\n]");
    }

    #[test]
    fn compact_layout() {
        assert_eq!(encode_compact(&json!({"a": [1, 2.5], "b": "é"})), "{\"a\": [1, 2.5], \"b\": \"é\"}");
    }
}
