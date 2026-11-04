//! How values become text in messages, prompts and command output.
//!
//! - Truthiness (`truthy`): a missing value, null, `false`, zero, the empty string, and an
//!   empty array or object are false; anything else is true.
//! - A value as text (`text`, `text_of`): a string as it is, anything else in its literal
//!   form. A missing value prints `None`, as null does.
//! - The literal form (`literal`): a string quoted (see `quote` below), null as `None`,
//!   booleans as `True` and `False`, an integer in decimal, a float as its float text, an
//!   array as `[1, 'a']` and an object as `{'k': 1, 'l': None}`: items separated by `, `,
//!   each key quoted and followed by `: `, in the object's order.
//! - Float text (`float_text`): the shortest digits that read back as the same float. Fixed
//!   notation for 1e-4 <= |x| < 1e16, always with a digit after the point (`1.0`, `0.0001`,
//!   `123.456`); otherwise exponent notation with a sign and at least two exponent digits
//!   (`1e+16`, `1e-05`, `1.5e-07`). Non-finite floats are `nan`, `inf` and `-inf`.
//! - Quoting (`quote`): single quotes, or double quotes if the text has a `'` and no `"`.
//!   A backslash escapes the chosen quote and `\`; newline, carriage return and tab are
//!   written as `\n`, `\r` and `\t`; any other character that isn't printable
//!   (`is_printable`) is `\xNN` below U+0100, `\uNNNN` below U+10000 and `\UNNNNNNNN`
//!   above, in lowercase hex. Everything else, non-ASCII included, is kept as it is.
//! - `fmt_2f`: a number with two digits after the point; a boolean as `0.00` or `1.00`.
//! - Whitespace (`is_space`, `strip`): Unicode whitespace and the separators \x1c-\x1f.
//!   `strip` trims it from both ends.
//! - `parse_int`: an optional `+` or `-` and ASCII digits, with optional whitespace around
//!   them and single underscores allowed between digits. Anything else, or a number too big
//!   for 64 bits, is not an integer.
//! - `split_lines`: the lines of a text without their line ends. A line ends at `\n`,
//!   `\r\n`, `\r`, `\v`, `\f`, \x1c, \x1d, \x1e, U+0085, U+2028 or U+2029. A final line end
//!   doesn't make an empty last line, and empty text has no lines.
//!
//! `text_of` and `try_quote` return a `Result` but always succeed; `fmt_2f` and
//! `excerpt_of` fail for values they have no text for.

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

/// A value as text: a string as it is, anything else in its literal form, and a missing
/// value as `None`.
pub fn text_of(v: Option<&Value>) -> R<String> {
    Ok(match v {
        Some(Value::String(s)) => s.clone(),
        None => "None".into(),
        Some(v) => literal(v),
    })
}

/// A value that's there as text: a string as it is, anything else in its literal form.
pub fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        v => literal(v),
    }
}

/// The literal form of any JSON value: objects, arrays, strings, numbers, booleans, null.
pub fn literal(v: &Value) -> String {
    let mut out = String::new();
    literal_into(v, &mut out);
    out
}

fn literal_into(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("None"),
        Value::Bool(b) => out.push_str(if *b { "True" } else { "False" }),
        Value::Number(n) => out.push_str(&number_text(n)),
        Value::String(s) => out.push_str(&quote(s)),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                literal_into(item, out);
            }
            out.push(']');
        }
        Value::Object(items) => {
            out.push('{');
            for (i, (k, item)) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&quote(k));
                out.push_str(": ");
                literal_into(item, out);
            }
            out.push('}');
        }
    }
}

/// A JSON number as text: integers as integers, floats as `float_text`.
pub fn number_text(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        float_text(n.as_f64().unwrap_or(f64::NAN))
    }
}

/// A float as text: the shortest round-trip digits, fixed notation for 1e-4 <= |x| < 1e16.
pub fn float_text(x: f64) -> String {
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

/// A JSON number with two digits after the point. A boolean counts as 0 or 1.
pub fn fmt_2f(v: Option<&Value>) -> R<String> {
    let f = match v {
        Some(Value::Number(n)) => n.as_f64().ok_or_else(|| Error::Other("not a float".into()))?,
        Some(Value::Bool(b)) => *b as i64 as f64,
        _ => return Err(Error::Other("not representable as text".into())),
    };
    Ok(format!("{:.2}", f))
}

/// `quote`, kept fallible for its callers. Always succeeds.
pub fn try_quote(s: &str) -> R<String> {
    Ok(quote(s))
}

/// A string quoted for messages: single quotes unless the text has a `'` and no `"`;
/// backslash escapes for the quote, `\\`, `\n`, `\r`, `\t`; `\xNN`, `\uNNNN` or
/// `\UNNNNNNNN` for anything `is_printable` rejects.
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

/// Whether `quote` keeps a character as it is: false for control and format characters,
/// separators other than the space, surrogates, private use, and noncharacters.
/// (Unassigned code points count as printable: telling them apart needs the Unicode
/// tables.)
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

/// Whitespace: Unicode whitespace, and also the separators \x1c-\x1f.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// An excerpt of a JSON value: empty for a falsy value, `text::excerpt` of a string, and
/// an error for anything else.
pub fn excerpt_of(v: Option<&Value>, limit: usize) -> R<String> {
    match v {
        _ if !truthy(v) => Ok(String::new()),
        Some(Value::String(s)) => Ok(crate::text::excerpt(s, limit)),
        _ => Err(Error::Other("not representable as text".into())),
    }
}


/// Text as an integer: surrounding whitespace, a sign, and ASCII digits with single
/// underscores between them. (Non-ASCII digits are refused.)
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

/// The lines of `s`: splits on \n, \r\n, \r, \v, \f, \x1c-\x1e, \x85, U+2028, U+2029,
/// and drops the line ends. A final line end doesn't make an empty last line.
pub fn split_lines(s: &str) -> Vec<&str> {
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
    fn float_text_is_shortest_round_trip() {
        for (x, want) in [(0.1, "0.1"), (1.0, "1.0"), (1e16, "1e+16"), (1e-5, "1e-05"), (123.456, "123.456"), (0.0001, "0.0001")] {
            assert_eq!(float_text(x), want);
        }
    }

    #[test]
    fn parse_int_accepts_signs_and_underscores() {
        assert_eq!(parse_int(" 12 "), Some(12));
        assert_eq!(parse_int("-1_000"), Some(-1000));
        assert_eq!(parse_int("+7"), Some(7));
        assert_eq!(parse_int("1__0"), None);
        assert_eq!(parse_int("_1"), None);
        assert_eq!(parse_int("1.0"), None);
        assert_eq!(parse_int(""), None);
    }

    #[test]
    fn quote_escapes_and_picks_quotes() {
        assert_eq!(quote("abc"), "'abc'");
        assert_eq!(quote("it's"), "\"it's\"");
        assert_eq!(quote("it's \"x\""), "'it\\'s \"x\"'");
        assert_eq!(quote("a\nb\\c\x07é\u{a0}🦀"), "'a\\nb\\\\c\\x07é\\xa0🦀'");
        let v: Value = serde_json::from_str(r#"{"a": [1, 2.5, null, true], "b": "x"}"#).unwrap();
        assert_eq!(literal(&v), "{'a': [1, 2.5, None, True], 'b': 'x'}");
    }

    #[test]
    fn split_lines_handles_every_line_break() {
        assert_eq!(split_lines("a\r\nb\rc\n"), vec!["a", "b", "c"]);
        assert_eq!(split_lines("a\n\nb"), vec!["a", "", "b"]);
        assert!(split_lines("").is_empty());
    }
}
