//! Local time as Python's `datetime` computes it: `datetime.fromtimestamp(t)` (through
//! `localtime`), naive wall-clock arithmetic, and `naive.timestamp()` (Python's own
//! `_mktime` search, so DST gaps and repeats resolve the same way, with `fold=0`).
//!
//! A naive datetime is held as seconds since 1970-01-01 00:00 *as if it were UTC*, which
//! makes `+ timedelta(minutes=1)` a plain `+ 60`.

/// A naive datetime's fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fields {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Naive seconds from fields.
pub fn naive(f: Fields) -> i64 {
    days_from_civil(f.year, f.month, f.day) * 86400 + (f.hour * 3600 + f.minute * 60 + f.second) as i64
}

/// Fields of naive seconds.
pub fn fields(naive: i64) -> Fields {
    let days = naive.div_euclid(86400);
    let secs = naive.rem_euclid(86400) as u32;
    let (year, month, day) = civil_from_days(days);
    Fields { year, month, day, hour: secs / 3600, minute: secs / 60 % 60, second: secs % 60 }
}

/// `isoweekday() % 7`: Sunday is 0.
pub fn weekday_sun0(naive: i64) -> u32 {
    // 1970-01-01 was a Thursday (4).
    ((naive.div_euclid(86400) + 4).rem_euclid(7)) as u32
}

/// `time.localtime(u)` as naive seconds.
pub fn local(u: i64) -> i64 {
    let t = u as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    naive(Fields {
        year: tm.tm_year as i64 + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec.min(59) as u32,
    })
}

/// `datetime.fromtimestamp(t)` to the whole second, as naive seconds (the microseconds
/// round half-even, so .9999995 and up carries into the next second).
pub fn from_timestamp(t: f64) -> i64 {
    let mut whole = t.trunc();
    let frac = t - whole;
    let us = (frac * 1e6).round_ties_even();
    if us >= 1e6 {
        whole += 1.0;
    } else if us < 0.0 {
        whole -= 1.0;
    }
    local(whole as i64)
}

/// `naive.timestamp()` with `fold=0`: Python's `datetime._mktime`.
pub fn timestamp(t: i64) -> i64 {
    const MAX_FOLD_SECONDS: i64 = 24 * 3600;
    let a = local(t) - t;
    let u1 = t - a;
    let t1 = local(u1);
    let b = if t1 == t {
        let u2 = u1 - MAX_FOLD_SECONDS;
        let b2 = local(u2) - u2;
        if a == b2 {
            return u1;
        }
        b2
    } else {
        t1 - u1
    };
    let u2 = t - b;
    let t2 = local(u2);
    if t2 == t {
        return u2;
    }
    if t1 == t {
        return u1;
    }
    u1.max(u2)
}

/// `datetime.fromtimestamp(t).strftime(fmt)` for the directives the CLI uses:
/// `%Y %m %d %H %M %S %%`.
pub fn strftime(t: f64, fmt: &str) -> String {
    let f = fields(from_timestamp(t));
    let mut out = String::new();
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('Y') => out.push_str(&format!("{}", f.year)),
            Some('m') => out.push_str(&format!("{:02}", f.month)),
            Some('d') => out.push_str(&format!("{:02}", f.day)),
            Some('H') => out.push_str(&format!("{:02}", f.hour)),
            Some('M') => out.push_str(&format!("{:02}", f.minute)),
            Some('S') => out.push_str(&format!("{:02}", f.second)),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for days in [-800_000, -1, 0, 1, 19_000, 20_000, 2_000_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        let f = fields(naive(Fields { year: 2026, month: 11, day: 3, hour: 23, minute: 59, second: 1 }));
        assert_eq!((f.year, f.month, f.day, f.hour, f.minute, f.second), (2026, 11, 3, 23, 59, 1));
        // 2026-11-03 is a Tuesday.
        assert_eq!(weekday_sun0(naive(f)), 2);
    }

    #[test]
    fn timestamp_inverts_local() {
        let now = 1_790_000_000;
        assert_eq!(timestamp(local(now)), now);
    }
}
