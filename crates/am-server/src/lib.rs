//! The daemon and the web server.
//!
//! - `daemon`: `am daemon start|stop|restart|status`, the `daemon/lock` flock, `daemon/pid`,
//!   `web.json`, the `daemon/wake` FIFO, and the runner, scheduler and health loops on tokio.
//!   The daemon process itself is `am daemon __run`, so `am daemon stop` can recognize it
//!   by its command line (`\bam daemon\b`).
//! - `web`: the web server: the JSON API, SSE events, terminal websockets, artifacts, and
//!   the UI's files from `assets::webui_dir()`, served as they are.
//! - `deps`: the calls into `am-turns` and `am-ops`, in one place.

pub mod daemon;
pub mod deps;
pub mod event;
pub mod web;

pub use daemon::{cmd_daemon, cmd_url};

#[cfg(test)]
pub(crate) mod test_env {
    //! Tests that set `AM_HOME` take this lock, since the environment is the process's.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    pub fn lock() -> std::sync::MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
}
