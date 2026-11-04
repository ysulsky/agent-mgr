//! Log lines as Python's `logging.basicConfig(format="%(asctime)s %(name)s %(levelname)s
//! %(message)s")` writes them, to stderr (the daemon's log file). INFO and above, as the
//! daemon configures it.

use std::io::Write;

fn line(name: &str, level: &str, message: &str) {
    let now = chrono::Local::now();
    let stamp = format!("{},{:03}", now.format("%Y-%m-%d %H:%M:%S"), now.timestamp_subsec_millis());
    let _ = writeln!(std::io::stderr(), "{stamp} {name} {level} {message}");
}

pub fn info(name: &str, message: &str) {
    line(name, "INFO", message);
}

pub fn warning(name: &str, message: &str) {
    line(name, "WARNING", message);
}

/// `log.exception`: the message, then the error on the lines below.
pub fn exception(name: &str, message: &str, error: &dyn std::fmt::Display) {
    line(name, "ERROR", &format!("{message}\n{error}"));
}
