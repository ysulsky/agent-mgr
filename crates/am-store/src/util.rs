//! Small helpers for working with records the way the Python does: `d.get(key)`,
//! truthiness, `now()` as a JSON float, and `repr()` for error messages.

use am_core::{Error, Result, Value};
use serde_json::{Map, Number};

/// `d.get(key)`: `None` for a missing key, or when `d` isn't a dict.
pub fn get<'a>(d: &'a Value, key: &str) -> Option<&'a Value> {
    d.as_object().and_then(|m| m.get(key))
}

/// `bool(d.get(key))`.
pub fn has(d: &Value, key: &str) -> bool {
    am_core::py::truthy(get(d, key))
}

/// `d.get(key)` when it's a non-empty string.
pub fn get_str<'a>(d: &'a Value, key: &str) -> Option<&'a str> {
    get(d, key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// `d.get(key) or default`.
pub fn or<'a>(d: &'a Value, key: &str, default: &'a Value) -> &'a Value {
    match get(d, key) {
        Some(v) if am_core::py::truthy(Some(v)) => v,
        _ => default,
    }
}

/// `str(d[key])`, or `str(d.get(key))` for a missing key ("None").
pub fn s(d: &Value, key: &str) -> String {
    am_core::py::str_of(get(d, key)).unwrap_or_default()
}

/// A float, as `time.time()` and friends land in JSON.
pub fn float(f: f64) -> Value {
    Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// `now()` as a JSON float.
pub fn now_value() -> Value {
    float(am_core::now())
}

/// A JSON number as a float (Python bools are ints): `None` for anything else.
pub fn num(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::Bool(b)) => Some(*b as i64 as f64),
        _ => None,
    }
}

/// `{}`.
pub fn empty() -> Value {
    Value::Object(Map::new())
}

/// `repr(s)`.
pub fn repr(s: &str) -> String {
    am_core::py::repr(s)
}

/// An uncaught Python exception: a failure the CLI reports without a traceback.
pub fn crash<T>(what: impl Into<String>) -> Result<T> {
    Err(Error::Other(what.into()))
}

/// Mutable access to a record's fields, making it a dict first if it isn't one.
pub fn fields(v: &mut Value) -> &mut Map<String, Value> {
    if !v.is_object() {
        *v = empty();
    }
    v.as_object_mut().unwrap()
}

/// `dict.update(changes)`: replaces keys in place, appends new ones.
pub fn update(v: &mut Value, changes: &Value) {
    if let Some(changes) = changes.as_object() {
        let target = fields(v);
        for (k, val) in changes {
            target.insert(k.clone(), val.clone());
        }
    }
}

/// `Path.exists()`: follows symlinks; errors count as missing.
pub fn exists(path: &std::path::Path) -> bool {
    std::fs::metadata(path).is_ok()
}

/// `core.update_json` with a function that may raise: an error leaves the file as it was
/// (Python's exception propagates out of the `with locked(...)` before the write).
pub fn try_update_json(path: &std::path::Path, default: Value, f: impl FnOnce(&mut Value) -> Result<()>) -> Result<Value> {
    let _guard = am_core::locked(path)?;
    let mut data = am_core::read_json(path)?.unwrap_or(default);
    f(&mut data)?;
    am_core::write_json(path, &data)?;
    Ok(data)
}

/// `os.stat(...).st_mtime`: seconds plus nanoseconds, computed as Python does
/// (`sec + nsec * 1e-9`), so the float prints the same.
pub fn mtime(meta: &std::fs::Metadata) -> f64 {
    use std::os::unix::fs::MetadataExt;
    meta.mtime() as f64 + meta.mtime_nsec() as f64 * 1e-9
}
