//! `config.toml`: defaults, loading, the template `am setup` writes, and `set_string`,
//! which changes one setting and leaves the rest of the file as it was. The settings are
//! the defaults (`config_defaults.json`) with `config.toml` merged over them; the template
//! (`config_template.toml`) is the commented file `am setup` writes.
//!
//! Editing (`with_value`). The text is taken line by line, each line with its ending. A
//! line `[name]`, with optional spaces inside and around the brackets and an optional
//! `# comment` after, starts the table `name`. In the wanted table, the first line that
//! sets the key (`key = value`, optionally indented, with optional spaces around the `=`)
//! gets its value replaced. The old value is a double-quoted string (with backslash
//! escapes), a single-quoted string, or a bare run of anything but spaces and `#`;
//! whatever follows it on the line, such as a comment, stays. If the table doesn't set the
//! key, `key = value` goes on the line after the table's header. If there's no such table,
//! a blank line, `[section]` and `key = value` are added at the end (after a newline, if
//! the text doesn't end with one). The value is written as an ASCII-only JSON string,
//! which is also a valid TOML basic string. Every other line, comments and blank lines
//! included, is left exactly as it was.

use crate::{am_err, paths::config_path, Result, Value};
use serde_json::Map;

/// The commented `config.toml` that `am setup` writes.
pub const TEMPLATE: &str = include_str!("config_template.toml");

/// The default settings, which `config.toml` is merged over.
pub fn defaults() -> Value {
    serde_json::from_str(include_str!("config_defaults.json")).expect("config_defaults.json")
}

/// A deep merge where tables merge and anything else replaces. Keys of `base` keep their
/// place; new keys follow in `over`'s order.
pub fn merge(base: &Value, over: &Value) -> Value {
    let mut out = base.clone();
    if let (Some(out_map), Some(over_map)) = (out.as_object_mut(), over.as_object()) {
        for (key, value) in over_map {
            let merged = match (out_map.get(key), value) {
                (Some(existing @ Value::Object(_)), Value::Object(_)) => merge(existing, value),
                _ => value.clone(),
            };
            out_map.insert(key.clone(), merged);
        }
    }
    out
}

/// A TOML value as JSON. (Dates and times become strings: nothing in Agent Mgr's config
/// uses them.)
pub fn toml_to_json(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::from(*i),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => Value::Object(t.iter().map(|(k, v)| (k.clone(), toml_to_json(v))).collect::<Map<_, _>>()),
    }
}

/// Parse TOML text into a JSON object. The error's text is the TOML parser's own.
pub fn parse(text: &str) -> std::result::Result<Value, toml::de::Error> {
    let table: toml::Table = text.parse()?;
    Ok(toml_to_json(&toml::Value::Table(table)))
}

/// The settings: the defaults with `config.toml` merged over them.
pub fn load() -> Result<Value> {
    let path = config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(defaults()),
        Err(e) => return Err(e.into()),
    };
    match parse(&text) {
        Ok(user) => Ok(merge(&defaults(), &user)),
        Err(e) => am_err(format!("{} is not valid TOML: {}", path.display(), e.message())),
    }
}

/// A string as ASCII-only JSON. A JSON string is a valid TOML basic string.
fn json_ascii_string(s: &str) -> String {
    crate::json::encode_ascii(&Value::String(s.to_string()))
}

/// The lines of `s`, each with its line end, split where `format::split_lines` splits.
fn splitlines_keepends(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(_, '\n')) = chars.peek() {
                    chars.next();
                    end += 1;
                }
            }
            out.push(&s[start..end]);
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `text` with `key = value` in `[section]`, every other line as it was.
pub fn with_value(text: &str, section: &str, key: &str, value: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    static TABLE: OnceLock<Regex> = OnceLock::new();
    // A line keeps its ending, so a header line may end in a newline.
    let table_re = TABLE.get_or_init(|| Regex::new(r"^\s*\[\s*([^\[\]#]+?)\s*\]\s*(#.*)?\n?\z").unwrap());
    let key_re = Regex::new(&format!(r#"(?s)^(\s*{}\s*=\s*)("(?:[^"\\\n]|\\.)*"|'[^'\n]*'|[^#\s]*)(.*)\z"#, regex::escape(key))).unwrap();
    let literal = json_ascii_string(value);
    let mut lines: Vec<String> = splitlines_keepends(text).into_iter().map(String::from).collect();
    let (mut current, mut header): (Option<String>, Option<usize>) = (None, None);
    for i in 0..lines.len() {
        if let Some(table) = table_re.captures(&lines[i]) {
            current = Some(table[1].to_string());
            if current.as_deref() == Some(section) {
                header = Some(i);
            }
            continue;
        }
        if current.as_deref() != Some(section) {
            continue;
        }
        if let Some(m) = key_re.captures(&lines[i]) {
            lines[i] = format!("{}{}{}", &m[1], literal, &m[3]);
            return lines.concat();
        }
    }
    if let Some(header) = header {
        lines.insert(header + 1, format!("{key} = {literal}\n"));
        return lines.concat();
    }
    let mut text = text.to_string();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    format!("{text}\n[{section}]\n{key} = {literal}\n")
}

/// Set one string in `config.toml`, keeping its comments and layout, and return whether
/// the file changed. Refuses, leaving the file alone, unless the result parses to exactly
/// the old settings with this one value changed. A missing file starts from the template.
pub fn set_string(section: &str, key: &str, value: &str) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let path = config_path();
    let (text, mode) = match std::fs::read_to_string(&path) {
        // CRLF and lone CR line ends are read as `\n`; the file keeps its mode.
        Ok(text) => (text.replace("\r\n", "\n").replace('\r', "\n"), std::fs::metadata(&path)?.permissions().mode() & 0o777),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (TEMPLATE.to_string(), 0o644),
        Err(e) => return Err(e.into()),
    };
    let before = match parse(&text) {
        Ok(v) => v,
        Err(e) => return am_err(format!("{} is not valid TOML, so it wasn't changed: {}", path.display(), e.message())),
    };
    if let Some(Value::Object(table)) = before.get(section) {
        if table.get(key) == Some(&Value::String(value.to_string())) {
            return Ok(false);
        }
    }
    let updated = with_value(&text, section, key, value);
    let mut expected = before.clone();
    if let Some(map) = expected.as_object_mut() {
        let entry = map.entry(section.to_string()).or_insert_with(|| Value::Object(Map::new()));
        if let Some(table) = entry.as_object_mut() {
            table.insert(key.to_string(), Value::String(value.to_string()));
        }
    }
    match parse(&updated) {
        Ok(after) if after == expected => {}
        _ => {
            return am_err(format!(
                "couldn't safely set {key} under [{section}] in {}; set {key} = {} there by hand",
                path.display(),
                json_ascii_string(value)
            ))
        }
    }
    crate::fsx::atomic_write(&path, updated.as_bytes(), mode)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_parses_to_the_defaults_it_documents() {
        let parsed = parse(TEMPLATE).unwrap();
        let merged = merge(&defaults(), &parsed);
        assert_eq!(merged["runner"]["max_live_processes"], 6);
        assert_eq!(merged["backends"]["claude"]["allowed_tools"][0], "Bash(am *)");
        // Merging keeps the defaults' key order.
        let keys: Vec<_> = merged.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys[..3], ["web", "defaults", "backends"]);
    }

    /// Recorded cases (testdata_set_string.json): text, section, key, value, and result.
    #[test]
    fn with_value_matches_the_cases() {
        let cases: Value = serde_json::from_str(include_str!("testdata_set_string.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let s = |i: usize| case[i].as_str().unwrap();
            assert_eq!(with_value(s(0), s(1), s(2), s(3)), s(4), "case {case}");
        }
    }

    #[test]
    fn set_string_edits_in_place_keeps_the_mode_and_refuses_what_it_cant_do_safely() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("AM_HOME", dir.path());
        let path = dir.path().join("config.toml");
        // No file yet: the template, with the value set.
        assert!(set_string("storage", "rclone_remote", "gdrive").unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, TEMPLATE.replace("rclone_remote = \"\"", "rclone_remote = \"gdrive\""));
        // The same value again changes nothing.
        assert!(!set_string("storage", "rclone_remote", "gdrive").unwrap());
        // The file keeps its mode, and its CRLF line ends become LF.
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&path, "[storage]\r\nfolder = \"x\"\r\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(set_string("storage", "rclone_remote", "r").unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[storage]\nrclone_remote = \"r\"\nfolder = \"x\"\n");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        // A section that isn't a table can't be edited safely.
        std::fs::write(&path, "storage = 3\n").unwrap();
        let err = set_string("storage", "rclone_remote", "r").unwrap_err().to_string();
        let want = format!("couldn't safely set rclone_remote under [storage] in {}; set rclone_remote = \"r\" there by hand", path.display());
        assert_eq!(err, want);
        std::fs::write(&path, "[[broken\n").unwrap();
        assert!(set_string("storage", "rclone_remote", "r").unwrap_err().to_string().contains("is not valid TOML, so it wasn't changed"));
    }

    #[test]
    fn json_ascii_string_matches_encode_ascii() {
        assert_eq!(json_ascii_string("a\"b\\é😀\n"), r#""a\"b\\\u00e9\ud83d\ude00\n""#);
    }
}
