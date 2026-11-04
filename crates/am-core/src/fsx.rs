//! The file protocol: atomic writes, locked read-modify-write, JSON records, and waking
//! the daemon.

use crate::{json, paths, Result, Value};
use std::fs;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Read a JSON file: `None` for a missing file (or a parent that isn't a directory).
/// A JSON `null` is `Some(Value::Null)`; callers that treat any falsy value as empty use
/// `format::truthy`.
pub fn read_json(path: &Path) -> Result<Option<Value>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Write a record atomically, as `json::encode` writes it: one-space indentation,
/// non-ASCII kept as it is, a trailing newline.
pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    write_json_mode(path, value, 0o644)
}

pub fn write_json_mode(path: &Path, value: &Value, mode: u32) -> Result<()> {
    Ok(atomic_write(path, json::encode(value).as_bytes(), mode)?)
}

/// Write atomically: a `.tmp-` file beside the target, fsync, chmod, rename.
pub fn atomic_write(path: &Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let (tmp, mut f) = loop {
        let tmp = dir.join(format!(".tmp-{}", crate::ids::random_from(b"abcdefghijklmnopqrstuvwxyz0123456789_", 8)));
        match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp) {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        f.write_all(data)?;
        // Plain fsync(2): File::sync_all is F_FULLFSYNC on macOS, which is far slower.
        if unsafe { libc::fsync(f.as_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        drop(f);
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// An exclusive flock on `<path>.lock`, held until the guard drops.
pub struct LockGuard(#[allow(dead_code)] fs::File);

pub fn locked(path: &Path) -> std::io::Result<LockGuard> {
    let mut lock = path.as_os_str().to_owned();
    lock.push(".lock");
    let lock = PathBuf::from(lock);
    if let Some(parent) = lock.parent() {
        fs::create_dir_all(parent)?;
    }
    let f = fs::OpenOptions::new().append(true).create(true).open(&lock)?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(LockGuard(f))
}

/// Apply `f` to the file's value (or `default`) under its lock, write it back and return
/// it. `f` mutates in place, or returns a replacement.
pub fn update_json(path: &Path, default: Value, f: impl FnOnce(&mut Value) -> Option<Value>) -> Result<Value> {
    let _guard = locked(path)?;
    let mut data = read_json(path)?.unwrap_or(default);
    let data = match f(&mut data) {
        Some(replacement) => replacement,
        None => data,
    };
    write_json(path, &data)?;
    Ok(data)
}

/// A directory's entry names, with a missing directory (or a file in its place) as empty.
/// Unsorted.
pub fn listdir(dir: &Path) -> std::io::Result<Vec<String>> {
    match fs::read_dir(dir) {
        Ok(entries) => {
            let mut names = Vec::new();
            for e in entries {
                // Names that aren't UTF-8 are skipped: such files can't be ours.
                if let Ok(name) = e?.file_name().into_string() {
                    names.push(name);
                }
            }
            Ok(names)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR) => Ok(vec![]),
        Err(e) => Err(e),
    }
}

/// Sorted ids of `<id>.json` files, ignoring in-flight temp files.
pub fn list_json_ids(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut ids: Vec<String> = listdir(dir)?
        .into_iter()
        .filter(|n| n.ends_with(".json") && !n.starts_with('.'))
        .map(|n| n[..n.len() - 5].to_string())
        .collect();
    ids.sort();
    Ok(ids)
}

/// Touch `<project>/.changed` (ignored if the project is gone), then wake the daemon.
pub fn poke(slug: &str) {
    let path = paths::project_dir(slug).join(".changed");
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap_or_default();
    if unsafe { libc::utimes(c.as_ptr(), std::ptr::null()) } != 0 {
        let _ = fs::OpenOptions::new().write(true).create(true).truncate(false).mode(0o666).open(&path);
    }
    wake_daemon();
}

pub fn wake_path() -> PathBuf {
    paths::daemon_dir().join("wake")
}

/// Wake the daemon: one byte into the `daemon/wake` FIFO, without blocking. With no daemon
/// reading, the open fails at once (ENXIO or ENOENT); with the buffer full, a wake is
/// already waiting. The daemon still polls, so a lost wake costs only the poll. Inside the
/// daemon, `am-server` wakes its loops directly instead.
pub fn wake_daemon() {
    let hook = WAKE_HOOK.read().unwrap_or_else(|e| e.into_inner()).clone();
    match hook {
        Some(hook) => hook(),
        None => wake_fifo(&wake_path()),
    }
}

type WakeHook = std::sync::Arc<dyn Fn() + Send + Sync>;
static WAKE_HOOK: std::sync::RwLock<Option<WakeHook>> = std::sync::RwLock::new(None);

/// Inside the daemon, a wake goes to this hook instead of the FIFO. The daemon sets one
/// that wakes only its pages, so its own pokes don't wake its runner; `None` clears it at
/// shutdown.
pub fn set_wake_hook(hook: Option<Box<dyn Fn() + Send + Sync>>) {
    *WAKE_HOOK.write().unwrap_or_else(|e| e.into_inner()) = hook.map(WakeHook::from);
}

/// The write behind `wake_daemon`, to a given FIFO. The `__turn` and `__proc` supervisors
/// use it with the `wake` path recorded in their `cmd.json`, after writing `exit.json`, so
/// they wake the daemon of the home that started them.
pub fn wake_fifo(path: &Path) {
    let c = match std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) {
        Ok(c) => c,
        Err(_) => return,
    };
    unsafe {
        let fd = libc::open(c.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK);
        if fd >= 0 {
            libc::write(fd, b"\n".as_ptr() as *const libc::c_void, 1);
            libc::close(fd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_round_trips_and_keeps_key_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b.json");
        let v: Value = serde_json::from_str(r#"{"z": 1, "a": [1.5, "é"], "m": {}}"#).unwrap();
        write_json(&path, &v).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\n \"z\": 1,\n \"a\": [\n  1.5,\n  \"é\"\n ],\n \"m\": {}\n}\n");
        assert_eq!(read_json(&path).unwrap(), Some(v));
        assert_eq!(read_json(&dir.path().join("missing.json")).unwrap(), None);
    }

    #[test]
    fn a_wake_hook_replaces_the_fifo_write() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        set_wake_hook(Some(Box::new(|| {
            CALLS.fetch_add(1, Ordering::SeqCst);
        })));
        wake_daemon();
        set_wake_hook(None);
        wake_daemon();
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn update_json_applies_under_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.json");
        update_json(&path, serde_json::json!([]), |v| {
            v.as_array_mut().unwrap().push(1.into());
            None
        })
        .unwrap();
        assert_eq!(read_json(&path).unwrap(), Some(serde_json::json!([1])));
        assert!(dir.path().join("t.json.lock").exists());
    }
}
