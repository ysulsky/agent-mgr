//! Python semantics that output depends on: truthiness, str(), repr() of a float or a
//! plain string, `:.2f`, and str.split()/strip() whitespace. (From branch `rust-am`.)
//! `str_of` and `repr_str` return an error where Python's output isn't reproduced yet
//! (containers, non-ASCII repr): extend them rather than working around them.

use crate::{Error, Result as R};
use serde_json::Value;

pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_none_or(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `str(value)`, as an f-string prints it: a string as it is, anything else as its repr.
pub fn str_of(v: Option<&Value>) -> R<String> {
    Ok(match v {
        Some(Value::String(s)) => s.clone(),
        None => "None".into(),
        Some(v) => repr_value(v),
    })
}

/// `str(value)` for a value that's there.
pub fn str_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        v => repr_value(v),
    }
}

/// `repr(value)` of what `json.load` gives: dicts, lists, str, int, float, bool, None.
pub fn repr_value(v: &Value) -> String {
    let mut out = String::new();
    repr_into(v, &mut out);
    out
}

fn repr_into(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("None"),
        Value::Bool(b) => out.push_str(if *b { "True" } else { "False" }),
        Value::Number(n) => out.push_str(&number_str(n)),
        Value::String(s) => out.push_str(&repr(s)),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                repr_into(item, out);
            }
            out.push(']');
        }
        Value::Object(items) => {
            out.push('{');
            for (i, (k, item)) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&repr(k));
                out.push_str(": ");
                repr_into(item, out);
            }
            out.push('}');
        }
    }
}

/// A JSON number as Python's `str()` prints it: ints as ints, floats as `repr(float)`.
pub fn number_str(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        float_repr(n.as_f64().unwrap_or(f64::NAN))
    }
}

/// `repr(float)`: the shortest round-trip digits, fixed notation for 1e-4 <= |x| < 1e16.
pub fn float_repr(x: f64) -> String {
    if !x.is_finite() {
        return (if x.is_nan() { "nan" } else if x > 0.0 { "inf" } else { "-inf" }).into();
    }
    let e = format!("{:e}", x);
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let (sign, mant) = mant.strip_prefix('-').map_or(("", mant), |m| ("-", m));
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let n = digits.len() as i32;
    let decpt = exp + 1;
    let body = if decpt <= -4 || decpt > 16 {
        let rest = &digits[1..];
        let frac = if rest.is_empty() { String::new() } else { format!(".{rest}") };
        format!("{}{}e{}{:02}", &digits[..1], frac, if exp < 0 { '-' } else { '+' }, exp.abs())
    } else if decpt <= 0 {
        format!("0.{}{}", "0".repeat((-decpt) as usize), digits)
    } else if decpt >= n {
        format!("{}{}.0", digits, "0".repeat((decpt - n) as usize))
    } else {
        format!("{}.{}", &digits[..decpt as usize], &digits[decpt as usize..])
    };
    format!("{sign}{body}")
}

/// `format(value, ".2f")` for a JSON number (bools are ints in Python).
pub fn fmt_2f(v: Option<&Value>) -> R<String> {
    let f = match v {
        Some(Value::Number(n)) => n.as_f64().ok_or_else(|| Error::Other("not a float".into()))?,
        Some(Value::Bool(b)) => *b as i64 as f64,
        _ => return Err(Error::Other("not representable as Python would print it".into())),
    };
    Ok(format!("{:.2}", f))
}

/// `repr(str)`, as `f"{x!r}"` prints it. Always succeeds; kept fallible for its callers.
pub fn repr_str(s: &str) -> R<String> {
    Ok(repr(s))
}

/// `repr(str)`: single quotes unless the text has a `'` and no `"`; backslash escapes for
/// the quote, `\\`, `\n`, `\r`, `\t`; `\xNN`, `\uNNNN` or `\UNNNNNNNN` for anything
/// `str.isprintable()` rejects. (Unassigned code points, which Python also escapes,
/// are kept: that needs the Unicode tables.)
pub fn repr(s: &str) -> String {
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
            c if !is_printable(c) => {
                let n = c as u32;
                if n < 0x100 {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n < 0x10000 {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// `str.isprintable()` for one character: false for control and format characters,
/// separators other than the space, private use, and noncharacters.
pub fn is_printable(c: char) -> bool {
    let n = c as u32;
    !(n < 0x20
        || (0x7f..=0xa0).contains(&n)
        || n == 0xad
        || (0x600..=0x605).contains(&n)
        || n == 0x61c
        || n == 0x6dd
        || n == 0x70f
        || (0x890..=0x891).contains(&n)
        || n == 0x8e2
        || n == 0x1680
        || n == 0x180e
        || (0x2000..=0x200f).contains(&n)
        || (0x2028..=0x202f).contains(&n)
        || (0x205f..=0x2064).contains(&n)
        || (0x2066..=0x206f).contains(&n)
        || n == 0x3000
        || (0xd800..=0xf8ff).contains(&n)
        || n == 0xfeff
        || (0xfff9..=0xfffb).contains(&n)
        || (0xfdd0..=0xfdef).contains(&n)
        || (n & 0xfffe) == 0xfffe
        || n == 0x110bd
        || n == 0x110cd
        || (0x13430..=0x1343f).contains(&n)
        || (0x1bca0..=0x1bca3).contains(&n)
        || (0x1d173..=0x1d17a).contains(&n)
        || n == 0xe0001
        || (0xe0020..=0xe007f).contains(&n)
        || n >= 0xf0000)
}

/// Python's `str.isspace`, which also counts the separators \x1c-\x1f.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `excerpt(value or "")` where the value came from JSON.
pub fn excerpt_of(v: Option<&Value>, limit: usize) -> R<String> {
    match v {
        _ if !truthy(v) => Ok(String::new()),
        Some(Value::String(s)) => Ok(crate::text::excerpt(s, limit)),
        _ => Err(Error::Other("not representable as Python would print it".into())),
    }
}


/// `int(text)` for a `str`: surrounding whitespace, a sign, and digits with single
/// underscores between them. (Non-ASCII digits, which Python also accepts, are refused.)
pub fn parse_int(text: &str) -> Option<i64> {
    let t = strip(text);
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    if !digits.bytes().all(|c| c.is_ascii_digit() || c == b'_') {
        return None;
    }
    let n: i64 = digits.replace('_', "").parse().ok()?;
    Some(if neg { -n } else { n })
}

/// `str.splitlines()`: splits on \n, \r\n, \r, \v, \f, \x1c-\x1e, \x85,  ,  ,
/// and drops the line ends. A final line end doesn't make an empty last line.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let is_break = matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}');
        if is_break {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(_, '\n')) = chars.peek() {
                    chars.next();
                    end += 1;
                }
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_matches_python() {
        for (x, want) in [(0.1, "0.1"), (1.0, "1.0"), (1e16, "1e+16"), (1e-5, "1e-05"), (123.456, "123.456"), (0.0001, "0.0001")] {
            assert_eq!(float_repr(x), want);
        }
    }

    #[test]
    fn parse_int_matches_python() {
        assert_eq!(parse_int(" 12 "), Some(12));
        assert_eq!(parse_int("-1_000"), Some(-1000));
        assert_eq!(parse_int("+7"), Some(7));
        assert_eq!(parse_int("1__0"), None);
        assert_eq!(parse_int("_1"), None);
        assert_eq!(parse_int("1.0"), None);
        assert_eq!(parse_int(""), None);
    }

    #[test]
    fn repr_matches_python() {
        assert_eq!(repr("abc"), "'abc'");
        assert_eq!(repr("it's"), "\"it's\"");
        assert_eq!(repr("it's \"x\""), "'it\\'s \"x\"'");
        assert_eq!(repr("a\nb\\c\x07é\u{a0}🦀"), "'a\\nb\\\\c\\x07é\\xa0🦀'");
        let v: Value = serde_json::from_str(r#"{"a": [1, 2.5, null, true], "b": "x"}"#).unwrap();
        assert_eq!(repr_value(&v), "{'a': [1, 2.5, None, True], 'b': 'x'}");
    }

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(splitlines("a\r\nb\rc\n"), vec!["a", "b", "c"]);
        assert_eq!(splitlines("a\n\nb"), vec!["a", "", "b"]);
        assert!(splitlines("").is_empty());
    }
}
