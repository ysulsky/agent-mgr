//! JSON values turned into text and numbers, as this crate reads them out of records.
//!
//! - A value as text (`text`): a string as it is, anything else in its literal form.
//! - The literal form (`literal`): null as `None`, booleans as `True` and `False`, an
//!   integer in decimal, a float as its float text (`am_core::format::float_text`), a
//!   string quoted, an array as `[1, 'a']` and an object as `{'k': 1}`: items separated by
//!   `, `, each key quoted and followed by `: `, in the object's order.
//! - Quoting (`quote`): single quotes, or double quotes if the text has a `'` and no `"`.
//!   A backslash escapes the chosen quote and `\`; `\n`, `\r` and `\t` are written as those
//!   escapes; the C0 controls, DEL, the C1 controls and the soft hyphen as `\xNN`; other
//!   characters that don't print (`is_unprintable`) as `\xNN`, `\uNNNN` or `\UNNNNNNNN`,
//!   in lowercase hex. Everything else, non-ASCII included, is kept as it is.
//! - `opt_str`: a value as text, or None for a missing value or null.
//! - `or_str`: a truthy value (`am_core::format::truthy`) as text, else "".
//! - As an integer (`to_int`): an integer as it is, a float truncated toward zero, a bool
//!   as 0 or 1, and a string that, with surrounding whitespace and every `_` removed,
//!   reads as a decimal integer. Anything else, a missing value included, is 0.
//! - As a float (`to_float`): a number's value, a bool as 0 or 1, and a string that, with
//!   surrounding whitespace removed, reads as a float. Anything else is 0.0.
//!   `to_float_or0` is 0.0 for a falsy or missing value.
//! - `float_value`: a float kept as a JSON float, so a whole one is written `2.0`. NaN and
//!   the infinities become null.
//! - `max_of(a, b)`: `a` unless `b` is greater, so a tie or a NaN keeps `a`.

use am_core::format::{float_text, truthy};
use serde_json::{Number, Value};

/// A value as text: a string as it is, anything else in its literal form.
pub fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => literal(other),
    }
}

/// The literal form of a JSON value: objects, arrays, strings, numbers, booleans, null.
pub fn literal(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).into(),
        Value::Number(n) => number(n),
        Value::String(s) => quote(s),
        Value::Array(a) => format!("[{}]", a.iter().map(literal).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!("{{{}}}", o.iter().map(|(k, v)| format!("{}: {}", quote(k), literal(v))).collect::<Vec<_>>().join(", ")),
    }
}

fn number(n: &Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        float_text(n.as_f64().unwrap_or(f64::NAN))
    }
}

/// A string quoted for messages: single quotes unless the text has one and no double
/// quote, with escapes for the characters that don't print.
pub fn quote(s: &str) -> String {
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

/// Characters that don't print, beyond the C0/C1 controls: separators other than the
/// space, the common format characters, and private use. A short list rather than the
/// full Unicode tables, enough for text that reaches an error message.
fn is_unprintable(c: char) -> bool {
    matches!(c as u32,
        0x2028 | 0x2029 | 0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x2064 | 0xfeff
        | 0xe000..=0xf8ff | 0xf0000..=0x10ffff | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x202f | 0x205f | 0x3000)
}

/// A value as text where a string is expected: None for a missing key or null.
pub fn opt_str(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(v) => Some(text(v)),
    }
}

/// A value as text where a string is expected, or "" for a falsy value.
pub fn or_str(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(Some(v)) => text(v),
        _ => String::new(),
    }
}

/// A value as an integer, or 0 (see the module doc).
pub fn to_int(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or_else(|| n.as_f64().map_or(0, |f| f.trunc() as i64)),
        Some(Value::Bool(b)) => *b as i64,
        Some(Value::String(s)) => am_core::format::strip(s).replace('_', "").parse().unwrap_or(0),
        _ => 0,
    }
}

/// A value as a float, or 0.0 (see the module doc).
pub fn to_float(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::Bool(b) => *b as i64 as f64,
        Value::String(s) => am_core::format::strip(s).parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `to_float` of a truthy value, else 0.0.
pub fn to_float_or0(v: Option<&Value>) -> f64 {
    match v {
        Some(v) if truthy(Some(v)) => to_float(v),
        _ => 0.0,
    }
}

/// A float as JSON keeps it: written with a `.0` even when whole.
pub fn float_value(f: f64) -> Value {
    Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// The larger of two floats: `a` unless `b` is greater.
pub fn max_of(a: f64, b: f64) -> f64 {
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
    fn literal_formats_values() {
        assert_eq!(literal(&json!({"a": [1, 2.0, null, true], "b": "it's"})), "{'a': [1, 2.0, None, True], 'b': \"it's\"}");
        assert_eq!(to_int(Some(&json!(3.9))), 3);
        assert_eq!(to_int(None), 0);
    }
}
