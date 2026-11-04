//! The detached process that owns one turn, `am __turn DIR`.
//!
//! It runs in its own session, so it outlives the daemon. It holds an exclusive flock on
//! `alive.lock` for as long as it lives, which gives the daemon a liveness check that
//! survives daemon restarts and can't be fooled by pid reuse. The agent's stdout goes
//! straight to `stream.jsonl`, with no pipe that could break. Everything about the turn
//! is recorded in its directory, so a runner can adopt a turn it didn't start (after a
//! daemon restart, say). The files: `cmd.json` and `prompt.md` (from the runner),
//! `alive.lock`, `pid.json` (`supervisor`, `child`, `started`, `awake_started`),
//! `stream.jsonl`, `stderr.log`, and `exit.json` (`code`, `started`, `ended`, and
//! `stopped`, or `spawn_error` if the agent never started).

use crate::sys;
use am_core::{Result, Value};
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// Supervise the turn in `turn_dir`: 75 if another supervisor owns the turn, else 0.
pub fn supervise(turn_dir: &Path) -> Result<i64> {
    let Some(_lock) = sys::try_lock(&turn_dir.join("alive.lock")) else {
        return Ok(75); // Another supervisor already owns this turn.
    };
    let spec: Value = serde_json::from_slice(&fs::read(turn_dir.join("cmd.json"))?)?;
    let grace = sys::f64_field(&spec, "stop_grace_seconds", 10.0);
    sys::install_stop_handlers();

    let started = sys::wall();
    let argv = sys::argv_of(&spec);
    let cwd = spec.get("cwd").and_then(Value::as_str).unwrap_or(".").to_string();
    let opened = (|| -> std::result::Result<_, String> {
        let prompt = turn_dir.join("prompt.md");
        let stdin = fs::File::open(&prompt).map_err(|e| sys::os_error_text(&e, &prompt.to_string_lossy()))?;
        let append = |name: &str| {
            let path = turn_dir.join(name);
            OpenOptions::new().append(true).create(true).open(&path).map_err(|e| sys::os_error_text(&e, &path.to_string_lossy()))
        };
        let out = append("stream.jsonl")?;
        let err = append("stderr.log")?;
        sys::spawn(&argv, &cwd, stdin.into(), out, err)
    })();
    let mut child = match opened {
        Ok(child) => child,
        Err(e) => {
            if let Ok(mut log) = OpenOptions::new().append(true).create(true).open(turn_dir.join("stderr.log")) {
                let _ = writeln!(log, "could not start {}: {e}", argv.first().map_or("", String::as_str));
            }
            sys::write_atomic(
                &turn_dir.join("exit.json"),
                &json!({"code": 127, "started": started, "ended": sys::wall(), "spawn_error": e}),
            );
            sys::wake(spec.get("wake"));
            return Ok(0);
        }
    };

    // `awake_started` times the turn's limit, so time the machine spends asleep doesn't count.
    sys::write_atomic(
        &turn_dir.join("pid.json"),
        &json!({"supervisor": std::process::id(), "child": child.id(), "started": started, "awake_started": sys::awake()}),
    );

    let pid = child.id();
    let (mut sent_term, mut sent_kill) = (false, false);
    let mut stop_at: Option<f64> = None;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break sys::return_code(status),
            Ok(None) => {}
            Err(_) => break -1,
        }
        if sys::stop_requested() {
            let at = *stop_at.get_or_insert_with(sys::wall);
            if !sent_term {
                sent_term = sys::killpg(pid, libc::SIGTERM);
            } else if !sent_kill && sys::wall() - at > grace {
                sent_kill = sys::killpg(pid, libc::SIGKILL);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    sys::write_atomic(
        &turn_dir.join("exit.json"),
        &json!({"code": code, "started": started, "ended": sys::wall(), "stopped": sys::stop_requested()}),
    );
    sys::wake(spec.get("wake"));
    Ok(0)
}
