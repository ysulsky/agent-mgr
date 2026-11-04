//! Python's view of JSON values, as the ported code uses them: `str(v)`, `repr(v)`,
//! `int(v or 0)`, `float(v)`, `v or ""`, and `max`. (Candidates for `am_core::py`.)

use am_core::py::{float_repr, truthy};
use serde_json::{Number, Value};

/// `str(v)`.
pub fn str_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// `repr(v)` for a value read from JSON: dicts, lists, strings, numbers, bools, None.
pub fn repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).into(),
        Value::Number(n) => number(n),
        Value::String(s) => repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!("{{{}}}", o.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr(v))).collect::<Vec<_>>().join(", ")),
    }
}

fn number(n: &Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        float_repr(n.as_f64().unwrap_or(f64::NAN))
    }
}

/// `repr(str)`: single quotes unless the text has one and no double quote; escapes as
/// Python's `unicode_repr` does for the characters it can't print.
pub fn repr_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (0x80..0xa0).contains(&(c as u32)) || c == '\u{ad}' => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if is_unprintable(c) => {
                let u = c as u32;
                if u <= 0xff {
                    out.push_str(&format!("\\x{u:02x}"));
                } else if u <= 0xffff {
                    out.push_str(&format!("\\u{u:04x}"));
                } else {
                    out.push_str(&format!("\\U{u:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// Characters `str.isprintable()` rejects beyond the C0/C1 controls: separators other
/// than the space, format characters, surrogates and private use. An approximation of
/// Python's Unicode tables, enough for text that reaches an error message.
fn is_unprintable(c: char) -> bool {
    matches!(c as u32,
        0x2028 | 0x2029 | 0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x2064 | 0xfeff
        | 0xe000..=0xf8ff | 0xf0000..=0x10ffff | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x202f | 0x205f | 0x3000)
}

/// `obj.get(key)` where a string is expected: None for a missing key or null.
pub fn opt_str(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(v) => Some(str_of(v)),
    }
}

/// `v or ""` where a string is expected.
pub fn or_str(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(Some(v)) => str_of(v),
        _ => String::new(),
    }
}

/// `int(v or 0)`.
pub fn py_int(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or_else(|| n.as_f64().map_or(0, |f| f.trunc() as i64)),
        Some(Value::Bool(b)) => *b as i64,
        Some(Value::String(s)) => am_core::py::strip(s).replace('_', "").parse().unwrap_or(0),
        _ => 0,
    }
}

/// `float(v)`.
pub fn py_float(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::Bool(b) => *b as i64 as f64,
        Value::String(s) => am_core::py::strip(s).parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `float(v or 0)`.
pub fn py_float_or0(v: Option<&Value>) -> f64 {
    match v {
        Some(v) if truthy(Some(v)) => py_float(v),
        _ => 0.0,
    }
}

/// A float as JSON keeps it: written with a `.0` even when whole.
pub fn float_value(f: f64) -> Value {
    Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// `max(a, b)`: `a` unless `b` is greater.
pub fn py_max(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repr_like_python() {
        assert_eq!(repr(&json!({"a": [1, 2.0, null, true], "b": "it's"})), "{'a': [1, 2.0, None, True], 'b': \"it's\"}");
        assert_eq!(py_int(Some(&json!(3.9))), 3);
        assert_eq!(py_int(None), 0);
    }
}
