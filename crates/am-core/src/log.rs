//! Python's `logging`, as Agent Mgr uses it. The daemon calls `configure()`, which is
//! `logging.basicConfig(level=INFO, format="%(asctime)s %(name)s %(levelname)s %(message)s")`
//! to stderr (its `daemon.log`). Anywhere else, as with Python's last-resort handler,
//! warnings and errors go to stderr as the bare message and info is dropped.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

static CONFIGURED: AtomicBool = AtomicBool::new(false);

pub fn configure() {
    CONFIGURED.store(true, Ordering::SeqCst);
}

pub fn is_configured() -> bool {
    CONFIGURED.load(Ordering::SeqCst)
}

/// `%(asctime)s`: local time as `2026-11-03 23:58:31,235`.
pub fn asctime() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02},{:03}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        now.subsec_millis()
    )
}

fn emit(name: &str, level: &str, message: &str) {
    let line = if is_configured() {
        format!("{} {name} {level} {message}\n", asctime())
    } else if level == "INFO" || level == "DEBUG" {
        return;
    } else {
        format!("{message}\n")
    };
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

pub fn info(name: &str, message: &str) {
    emit(name, "INFO", message);
}

pub fn warning(name: &str, message: &str) {
    emit(name, "WARNING", message);
}

pub fn error(name: &str, message: &str) {
    emit(name, "ERROR", message);
}

/// `log.exception(message)`: the message, then what went wrong (Python prints the
/// traceback there).
pub fn exception(name: &str, message: &str, error: &dyn std::fmt::Display) {
    emit(name, "ERROR", &format!("{message}\n{error}"));
}
