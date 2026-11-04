//! Test hooks. When `AM_TEST_HOOKS` names a JSON file, a few readings come from it instead
//! of the machine, read afresh on every use so a test can change them mid-run. Without the
//! variable nothing here has any effect.
//!
//! Keys: `now_offset` and `awake_offset` (seconds added to the clocks), `disk_percent`,
//! `memory_percent` (a number, or null for "unknown"), `login_timeout`, `checking_for`,
//! `kill_wait_seconds`, `stop_wait_seconds`. A missing or unreadable file means no hooks.

use crate::Value;

/// The hook's value, if hooks are on and the key is present (even as null).
pub fn get(key: &str) -> Option<Value> {
    let path = std::env::var_os("AM_TEST_HOOKS").filter(|p| !p.is_empty())?;
    let bytes = std::fs::read(path).ok()?;
    let all: Value = serde_json::from_slice(&bytes).ok()?;
    all.get(key).cloned()
}

pub fn f64_or(key: &str, default: f64) -> f64 {
    get(key).and_then(|v| v.as_f64()).unwrap_or(default)
}
