//! What the two supervisors share, kept free of the rest of the crate as Python's
//! `am.turn` and `am.proc` are free of imports: raw clocks (no test hooks, as their own
//! copies of `awake_time` have none), the compact `json.dumps(obj)` they write, locks,
//! signals and spawning in a new session.

use am_core::{json, Value};
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// `time.time()`, with no test offset: the supervisors read the clock directly.
pub fn wall() -> f64 {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    d.as_nanos() as f64 / 1e9
}

/// The supervisors' `_awake_time`: `am_core::awake_time`, which honours the
/// `awake_offset` test hook, as Python's `am.proc` and `am.turn` now do.
pub fn awake() -> f64 {
    am_core::awake_time()
}

/// `json.dumps(obj)` with Python's defaults: `", "` and `": "`, and `ensure_ascii`.
pub use am_core::json::dumps_ascii;

/// The supervisors' `_write_atomic`: `.tmp-<name>` beside it, then a rename. No fsync.
pub fn write_atomic(path: &Path, v: &Value) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".tmp-{name}"));
    if fs::write(&tmp, dumps_ascii(v)).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

/// `_read_json` of `am.proc`: None for a missing or unreadable file.
pub fn read_json(path: &Path) -> Option<Value> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Open `path` for appending and take an exclusive, non-blocking flock on it. None if
/// someone else holds it. The file stays locked until it's dropped.
pub fn try_lock(path: &Path) -> Option<fs::File> {
    let f = fs::OpenOptions::new().append(true).create(true).open(path).ok()?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return None;
    }
    Some(f)
}

/// `os.killpg(pid, sig)`. False if it failed (no such group, or not ours).
pub fn killpg(pid: u32, sig: i32) -> bool {
    unsafe { libc::killpg(pid as libc::pid_t, sig) == 0 }
}

/// `am.core.wake_daemon` with the FIFO's path from `cmd.json`: one byte, never blocking.
pub fn wake(path: Option<&Value>) {
    if let Some(Value::String(path)) = path {
        if !path.is_empty() {
            am_core::wake_fifo(Path::new(path));
        }
    }
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_signum: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// SIGTERM and SIGINT request a stop; SIGHUP is ignored, as is SIGPIPE (Rust's default).
pub fn install_stop_handlers() {
    unsafe {
        let handler = on_term as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

pub fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// `subprocess.Popen(..., start_new_session=True)`.
pub fn new_session(cmd: &mut std::process::Command) {
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// `str(OSError)` as Python prints it: `[Errno 2] No such file or directory: 'claude'`.
pub fn os_error_text(e: &std::io::Error, filename: &str) -> String {
    match e.raw_os_error() {
        Some(errno) => {
            let msg = unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno)) }.to_string_lossy().into_owned();
            format!("[Errno {errno}] {msg}: {}", py_repr(filename))
        }
        None => e.to_string(),
    }
}

/// `repr(str)` for a file name: single quotes unless it has one and no double quote.
pub fn py_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// A child's exit as `Popen.returncode` gives it: the code, or minus the signal.
pub fn return_code(status: std::process::ExitStatus) -> i64 {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => code as i64,
        (None, Some(sig)) => -(sig as i64),
        _ => -1,
    }
}

/// `argv` from `cmd.json`, as strings.
pub fn argv_of(spec: &Value) -> Vec<String> {
    spec.get("argv")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| json::dumps_compact(v))).collect())
        .unwrap_or_default()
}

/// `float(spec.get(key, default))`.
pub fn f64_field(spec: &Value, key: &str, default: f64) -> f64 {
    match spec.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
        Some(Value::Bool(b)) => *b as i64 as f64,
        _ => default,
    }
}

/// Spawn `argv` in `cwd` in a session of its own, with the files given, reporting a
/// failure as Python's `Popen` would.
pub fn spawn(
    argv: &[String],
    cwd: &str,
    stdin: std::process::Stdio,
    stdout: fs::File,
    stderr: fs::File,
) -> Result<std::process::Child, String> {
    let Some(program) = argv.first() else {
        return Err("list index out of range".into());
    };
    if !Path::new(cwd).is_dir() {
        let e = std::io::Error::from_raw_os_error(if Path::new(cwd).exists() { libc::ENOTDIR } else { libc::ENOENT });
        return Err(os_error_text(&e, cwd));
    }
    let mut cmd = std::process::Command::new(program);
    cmd.args(&argv[1..]).current_dir(cwd).stdin(stdin).stdout(stdout).stderr(stderr);
    new_session(&mut cmd);
    cmd.spawn().map_err(|e| os_error_text(&e, program))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_errors_read_like_python() {
        let e = std::io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(os_error_text(&e, "claude"), "[Errno 2] No such file or directory: 'claude'");
    }
}
