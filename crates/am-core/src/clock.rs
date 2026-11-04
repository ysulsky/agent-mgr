//! Clocks and durations. Both clocks honour the test hooks' offsets.

use crate::{am_err, hooks, Result};
use regex::Regex;
use std::sync::OnceLock;

/// Seconds since the Unix epoch, plus the `now_offset` test hook.
pub fn now() -> f64 {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    // Nanoseconds as a double, divided by 1e9, which settles how the last digits round.
    d.as_nanos() as f64 / 1e9 + hooks::f64_or("now_offset", 0.0)
}

/// Seconds the machine has been awake since boot (CLOCK_UPTIME_RAW on macOS,
/// CLOCK_MONOTONIC elsewhere), so a sleeping Mac doesn't use up a turn's time limit. Plus
/// the `awake_offset` test hook.
pub fn awake_time() -> f64 {
    #[cfg(target_os = "macos")]
    let clock = libc::CLOCK_UPTIME_RAW;
    #[cfg(not(target_os = "macos"))]
    let clock = libc::CLOCK_MONOTONIC;
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(clock, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9 + hooks::f64_or("awake_offset", 0.0)
}

fn duration_unit(unit: &str) -> f64 {
    match unit {
        "m" | "min" => 60.0,
        "h" | "hr" => 3600.0,
        "d" => 86400.0,
        "w" => 604800.0,
        _ => 1.0,
    }
}

/// A duration in text: `90`, `90s`, `30m`, `2h`, `1d`, `1h30m`. Seconds.
/// (A JSON number is used as it is; see `parse_duration_value`.)
pub fn parse_duration(text: &str) -> Result<f64> {
    static PARTS: OnceLock<Regex> = OnceLock::new();
    static ONE: OnceLock<Regex> = OnceLock::new();
    let parts_re = PARTS.get_or_init(|| Regex::new(r"\d+(?:\.\d+)?\s*[a-z]*").unwrap());
    let one_re = ONE.get_or_init(|| Regex::new(r"^(\d+(?:\.\d+)?)\s*(s|sec|m|min|h|hr|d|w)?$").unwrap());
    let original = text;
    let text = crate::format::strip(text).to_lowercase();
    let parts: Vec<&str> = parts_re.find_iter(&text).map(|m| m.as_str()).collect();
    let bad = || am_err(format!("cannot parse duration {}; try 90s, 30m, 2h or 1d", crate::format::try_quote(crate::format::strip(&original.to_lowercase())).unwrap_or_else(|_| format!("'{text}'"))));
    if parts.is_empty() || parts.concat().replace(' ', "") != text.replace(' ', "") {
        return bad();
    }
    let mut total = 0.0;
    for part in parts {
        let Some(m) = one_re.captures(part) else { return bad() };
        let n: f64 = m[1].parse().unwrap_or(0.0);
        total += n * duration_unit(m.get(2).map_or("", |u| u.as_str()));
    }
    Ok(total)
}

/// A duration from a config value, which may be a number or a string.
pub fn parse_duration_value(v: &serde_json::Value) -> Result<f64> {
    match v {
        serde_json::Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::Bool(b) => Ok(*b as i64 as f64),
        serde_json::Value::String(s) => parse_duration(s),
        other => am_err(format!("cannot parse duration {other}; try 90s, 30m, 2h or 1d")),
    }
}

/// Whole seconds in the largest unit that divides them: `1d`, `2h`, `30m`, `90s`, `0s`.
pub fn format_duration(seconds: f64) -> String {
    let s = seconds.trunc() as i64;
    if s != 0 && s % 86400 == 0 {
        format!("{}d", s / 86400)
    } else if s != 0 && s % 3600 == 0 {
        format!("{}h", s / 3600)
    } else if s != 0 && s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90").unwrap(), 90.0);
        assert_eq!(parse_duration("1h30m").unwrap(), 5400.0);
        assert_eq!(parse_duration(" 2H ").unwrap(), 7200.0);
        assert_eq!(parse_duration("1.5d").unwrap(), 129600.0);
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("3x").is_err());
        assert_eq!(format_duration(7200.0), "2h");
        assert_eq!(format_duration(90.0), "90s");
        assert_eq!(format_duration(0.0), "0s");
    }
}
