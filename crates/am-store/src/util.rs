//! Small helpers for working with records: field lookups, truthiness, `now()` as a JSON
//! float, and `quote()` for error messages.

use am_core::{Error, Result, Value};
use serde_json::{Map, Number};

/// A field of a record: `None` for a missing key, or when `d` isn't an object.
pub fn get<'a>(d: &'a Value, key: &str) -> Option<&'a Value> {
    d.as_object().and_then(|m| m.get(key))
}

/// Whether a field is there and truthy.
pub fn has(d: &Value, key: &str) -> bool {
    am_core::format::truthy(get(d, key))
}

/// A field, when it's a non-empty string.
pub fn get_str<'a>(d: &'a Value, key: &str) -> Option<&'a str> {
    get(d, key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// A field if it's truthy, else `default`.
pub fn or<'a>(d: &'a Value, key: &str, default: &'a Value) -> &'a Value {
    match get(d, key) {
        Some(v) if am_core::format::truthy(Some(v)) => v,
        _ => default,
    }
}

/// A field as text (`am_core::format::text_of`): "None" for a missing key.
pub fn s(d: &Value, key: &str) -> String {
    am_core::format::text_of(get(d, key)).unwrap_or_default()
}

/// A float, such as a timestamp, as a JSON number (null if it isn't finite).
pub fn float(f: f64) -> Value {
    Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// `now()` as a JSON float.
pub fn now_value() -> Value {
    float(am_core::now())
}

/// A JSON number as a float, with `true` and `false` as 1 and 0: `None` for anything else.
pub fn num(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::Bool(b)) => Some(*b as i64 as f64),
        _ => None,
    }
}

/// An empty object.
pub fn empty() -> Value {
    Value::Object(Map::new())
}

/// A string quoted for messages (`am_core::format::quote`).
pub fn quote(s: &str) -> String {
    am_core::format::quote(s)
}

/// An unexpected failure, as opposed to a usage error (`am_err`); the CLI prints its message.
pub fn crash<T>(what: impl Into<String>) -> Result<T> {
    Err(Error::Other(what.into()))
}

/// Mutable access to a record's fields, making it an object first if it isn't one.
pub fn fields(v: &mut Value) -> &mut Map<String, Value> {
    if !v.is_object() {
        *v = empty();
    }
    v.as_object_mut().unwrap()
}

/// Merge the fields of `changes` into a record: replaces keys in place, appends new ones.
pub fn update(v: &mut Value, changes: &Value) {
    if let Some(changes) = changes.as_object() {
        let target = fields(v);
        for (k, val) in changes {
            target.insert(k.clone(), val.clone());
        }
    }
}

/// Whether a path exists: follows symlinks; errors count as missing.
pub fn exists(path: &std::path::Path) -> bool {
    std::fs::metadata(path).is_ok()
}

/// `am_core::update_json` with a function that may fail: an error leaves the file as it
/// was (it returns, still under the lock, before the write).
pub fn try_update_json(path: &std::path::Path, default: Value, f: impl FnOnce(&mut Value) -> Result<()>) -> Result<Value> {
    let _guard = am_core::locked(path)?;
    let mut data = am_core::read_json(path)?.unwrap_or(default);
    f(&mut data)?;
    am_core::write_json(path, &data)?;
    Ok(data)
}

/// A file's modification time: seconds plus nanoseconds, always computed as
/// `sec + nsec * 1e-9`, so the float (and how it prints) is stable.
pub fn mtime(meta: &std::fs::Metadata) -> f64 {
    use std::os::unix::fs::MetadataExt;
    meta.mtime() as f64 + meta.mtime_nsec() as f64 * 1e-9
}
