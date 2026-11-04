//! Python's `json.dumps`, byte for byte, in the forms Agent Mgr uses:
//! - `dumps`: `json.dumps(obj, indent=1, ensure_ascii=False) + "\n"`, the record files;
//! - `dumps_compact`: `json.dumps(obj, ensure_ascii=False)`;
//! - `dumps_ascii`: `json.dumps(obj)`, compact and ASCII-only: aiohttp's `json_response`
//!   bodies, SSE events, and the files the turn and proc supervisors write;
//! - `dumps_with`: any `indent` and `ensure_ascii`, for the rest (`am health --json`, ...).

use crate::py::float_repr;
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

/// A JSON number as Python prints it: integers as integers, floats as `repr(float)`.
/// (Known gap, as in `rust-am`: an integer beyond 64 bits was parsed as a float.)
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
            float_repr(f)
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

/// `json.dumps(v, indent=indent, ensure_ascii=ensure_ascii)`, with no trailing newline.
/// With `indent`, items are separated by `,` and a newline; without, by `, `.
pub fn dumps_with(v: &Value, indent: Option<usize>, ensure_ascii: bool) -> String {
    let mut out = String::new();
    dump_at(v, indent, ensure_ascii, 0, &mut out);
    out
}

/// What `core.write_json` writes: `json.dumps(v, indent=1, ensure_ascii=False) + "\n"`.
pub fn dumps(v: &Value) -> String {
    let mut out = dumps_with(v, Some(1), false);
    out.push('\n');
    out
}

/// `json.dumps(v, ensure_ascii=False)`.
pub fn dumps_compact(v: &Value) -> String {
    dumps_with(v, None, false)
}

/// `json.dumps(v)`: compact, with everything outside printable ASCII escaped.
pub fn dumps_ascii(v: &Value) -> String {
    dumps_with(v, None, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_python_indent_1() {
        let v = json!({"a": 1, "b": [true, null, 0.1, 1e16, 1e-5], "c": {"d": "x\ny\u{1}"}, "e": []});
        let want = "{\n \"a\": 1,\n \"b\": [\n  true,\n  null,\n  0.1,\n  1e+16,\n  1e-05\n ],\n \"c\": {\n  \"d\": \"x\\ny\\u0001\"\n },\n \"e\": []\n}\n";
        assert_eq!(dumps(&v), want);
    }

    #[test]
    fn matches_python_ascii() {
        let v = json!({"é": "a\u{7f}😀\n\u{1}", "n": [1.5, -0.0]});
        assert_eq!(dumps_ascii(&v), r#"{"\u00e9": "a\u007f\ud83d\ude00\n\u0001", "n": [1.5, -0.0]}"#);
        assert_eq!(dumps_with(&json!([1, {"a": "é"}]), Some(2), true), "[\n  1,\n  {\n    \"a\": \"\\u00e9\"\n  }\n]");
    }

    #[test]
    fn matches_python_compact() {
        assert_eq!(dumps_compact(&json!({"a": [1, 2.5], "b": "é"})), "{\"a\": [1, 2.5], \"b\": \"é\"}");
    }
}
