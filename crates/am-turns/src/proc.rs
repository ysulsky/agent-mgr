//! A long-lived agent process, and the detached supervisor that
//! owns it, `am __proc DIR`.
//!
//! The supervisor starts `claude -p --input-format stream-json` and keeps it between
//! turns, so a turn skips the CLI's start and the instructions go once per process. The
//! runner hands it a turn by dropping `queue/<turn id>.json`. The supervisor writes the
//! turn's prompt to the agent's stdin as one user message, copies what the agent prints
//! into the turn's own `stream.jsonl`, and writes the turn's `exit.json` when the agent
//! has finished with it. To the runner, the turn then looks exactly like one run by
//! `am __turn`: while the turn is open the supervisor holds its `alive.lock`, and its
//! `pid.json` names this supervisor and the agent.
//!
//! Like `am __turn`, the supervisor runs in its own session and outlives the daemon. It
//! holds `alive.lock` in the proc dir for its whole life. The agent writes stdout and
//! stderr straight to files in the proc dir, never a pipe, and the supervisor tails them.
//!
//! How the CLI marks a turn (measured on Claude Code 2.1.316): with
//! `--replay-user-messages`, a user message with a uuid gets `command_lifecycle` events
//! for that uuid, `queued`, `started`, then `completed` or `cancelled`, and is echoed back
//! (`isReplay`) just before its `result`. A background task that finishes after the
//! result wakes the CLI for another round of its own (`task_notification`, then `init`
//! ... `result`), which a one-shot `-p` turn would include, so a turn stays open while
//! the CLI has background tasks. An interrupt is a `control_request`; the CLI ends the
//! turn with an error `result` at once and stays up for the next one.
//!
//! Files in the proc dir: `cmd.json` (from the runner), `alive.lock`, `pid.json`,
//! `stream.jsonl`, `stderr.log`, `queue/`, `current.json` (the open turn), `state.json`,
//! `memory.json` (written by the runner), `shutdown.json` (a request from the runner),
//! `strays/` (rounds the CLI ran with no turn open) and `exit.json`. A runner reads these
//! files, not the supervisor's memory, so it can use a process it didn't start.

use crate::sys;
use am_core::{Result, Value};
use serde_json::json;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::time::Duration;

const POLL: Duration = Duration::from_millis(50);
/// After a task finishes, the CLI starts its own round for it within a moment.
const WAKE_WAIT_SECONDS: f64 = 15.0;

/// The uuid a turn's message carries, so its lifecycle events and replay can be told
/// from another's.
pub fn message_uuid(turn_id: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, format!("am-turn:{turn_id}").as_bytes()).to_string()
}

/// The turn the agent has open.
pub struct Turn {
    pub id: String,
    pub dir: PathBuf,
    pub uuid: String,
    started: f64,
    awake_started: f64,
    _lock: File,
    out: Option<File>,
    stderr_offset: u64,
    replayed: bool,
    command_done: bool,
    error: bool,
    interrupt_sent: f64,
    term_sent: f64,
    kill_sent: f64,
}

/// The supervisor of one long-lived agent process.
pub struct Supervisor {
    dir: PathBuf,
    spec: Value,
    grace: f64,
    idle_seconds: f64,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    offset: u64,
    partial: Vec<u8>,
    turn: Option<Turn>,
    /// The CLI is in a round: between `init` and `result`.
    busy: bool,
    /// Background tasks the CLI says are running.
    tasks: usize,
    /// A task finished; its round is due before this.
    wake_until: f64,
    /// The CLI marks turns with lifecycle events or replays.
    markers: bool,
    turns: i64,
    last_activity: f64,
    /// SIGTERM: stop the open turn and the process.
    stop: bool,
    closing: String,
    closing_at: f64,
    stray: Option<File>,
    stray_count: u32,
}

/// Supervise the process in `directory`: 75 if another supervisor owns it, else 0.
pub fn supervise(directory: &Path) -> Result<i64> {
    Supervisor::new(directory)?.run()
}

impl Supervisor {
    pub fn new(directory: &Path) -> Result<Supervisor> {
        let spec: Value = serde_json::from_slice(&fs::read(directory.join("cmd.json"))?)?;
        let grace = sys::f64_field(&spec, "stop_grace_seconds", 10.0);
        let idle_seconds = sys::f64_field(&spec, "idle_seconds", 3300.0);
        let queue = directory.join("queue");
        if !queue.is_dir() {
            fs::create_dir(&queue)?;
        }
        Ok(Supervisor {
            dir: directory.to_path_buf(),
            spec,
            grace,
            idle_seconds,
            child: None,
            stdin: None,
            offset: 0,
            partial: Vec::new(),
            turn: None,
            busy: false,
            tasks: 0,
            wake_until: 0.0,
            markers: false,
            turns: 0,
            last_activity: sys::wall(),
            stop: false,
            closing: String::new(),
            closing_at: 0.0,
            stray: None,
            stray_count: 0,
        })
    }

    fn child_pid(&self) -> u32 {
        self.child.as_ref().map_or(0, Child::id)
    }

    // ------------------------------------------------------------ running

    /// Run the process to its end: 75 if another supervisor owns the process, else 0.
    pub fn run(mut self) -> Result<i64> {
        let Some(_lock) = sys::try_lock(&self.dir.join("alive.lock")) else {
            return Ok(75); // Another supervisor owns this process.
        };
        sys::install_stop_handlers();

        let started = sys::wall();
        let argv = sys::argv_of(&self.spec);
        let cwd = self.spec.get("cwd").and_then(Value::as_str).unwrap_or(".").to_string();
        let opened = (|| -> std::result::Result<Child, String> {
            let append = |name: &str| {
                let path = self.dir.join(name);
                OpenOptions::new().append(true).create(true).open(&path).map_err(|e| sys::os_error_text(&e, &path.to_string_lossy()))
            };
            let out = append("stream.jsonl")?;
            let err = append("stderr.log")?;
            sys::spawn(&argv, &cwd, Stdio::piped(), out, err)
        })();
        let mut child = match opened {
            Ok(child) => child,
            Err(e) => {
                if let Ok(mut log) = OpenOptions::new().append(true).create(true).open(self.dir.join("stderr.log")) {
                    let _ = writeln!(log, "could not start {}: {e}", argv.first().map_or("", String::as_str));
                }
                self.fail_queue(Some(127), &e, false);
                sys::write_atomic(
                    &self.dir.join("exit.json"),
                    &json!({"code": 127, "started": started, "ended": sys::wall(), "reason": "spawn", "error": e}),
                );
                return Ok(0);
            }
        };
        self.stdin = child.stdin.take();
        sys::write_atomic(
            &self.dir.join("pid.json"),
            &json!({"supervisor": std::process::id(), "child": child.id(), "started": started}),
        );
        self.child = Some(child);
        self.save_state(true);

        let code: i64 = loop {
            self.read_output(false);
            let polled = self.child.as_mut().map(|c| c.try_wait());
            match polled {
                Some(Ok(Some(status))) => break sys::return_code(status),
                Some(Ok(None)) => {}
                _ => break -1,
            }
            if sys::stop_requested() {
                self.stop = true;
            }
            if self.turn.is_some() {
                self.watch_turn();
            }
            if self.turn.is_none() && self.closing.is_empty() {
                if let Some((turn_id, directory)) = self.next_request() {
                    self.open(&turn_id, &directory);
                } else if self.stop {
                    self.close_process("stopped");
                } else if self.dir.join("shutdown.json").exists() {
                    let reason = sys::read_json(&self.dir.join("shutdown.json"))
                        .and_then(|v| v.get("reason").cloned())
                        .and_then(|r| r.as_str().filter(|s| !s.is_empty()).map(str::to_string))
                        .unwrap_or_else(|| "shutdown".into());
                    self.close_process(&reason);
                }
                // Wall time, not awake time: what idling saves is a prompt cache that
                // expires an hour after its last use, asleep or not.
                else if sys::wall() - self.last_activity > self.idle_seconds && !self.busy {
                    self.close_process("idle");
                }
            }
            if !self.closing.is_empty() {
                self.escalate_close();
            }
            std::thread::sleep(POLL);
        };
        if sys::stop_requested() {
            self.stop = true;
        }

        self.read_output(true);
        if let Some(turn) = &self.turn {
            let done = turn.command_done;
            let stopped = turn.interrupt_sent != 0.0 || self.stop;
            // A turn the CLI hadn't finished didn't succeed, whatever the exit code says.
            self.close_turn(Some(if code != 0 || done || stopped { code } else { 1 }), stopped);
        }
        if !self.closing.is_empty() {
            let reason = self.closing.clone();
            self.fail_queue(None, &reason, true);
        } else {
            self.fail_queue(Some(code), "the agent process exited", false);
        }
        let reason = if self.closing.is_empty() { "exited".to_string() } else { self.closing.clone() };
        sys::write_atomic(
            &self.dir.join("exit.json"),
            &json!({"code": code, "started": started, "ended": sys::wall(), "reason": reason, "turns": self.turns}),
        );
        let _ = fs::remove_file(self.dir.join("current.json"));
        self.save_state(false);
        Ok(0)
    }

    fn save_state(&self, live: bool) {
        sys::write_atomic(
            &self.dir.join("state.json"),
            &json!({
                "live": live, "busy": self.turn.is_some(), "turns": self.turns, "last_activity": self.last_activity,
                "closing": self.closing,
            }),
        );
    }

    // ------------------------------------------------------------ the agent's output

    fn read_output(&mut self, final_: bool) {
        let Ok(mut f) = File::open(self.dir.join("stream.jsonl")) else { return };
        let mut data = Vec::new();
        if f.seek(SeekFrom::Start(self.offset)).is_err() || f.read_to_end(&mut data).is_err() {
            return;
        }
        if data.is_empty() {
            return;
        }
        self.offset += data.len() as u64;
        let mut all = std::mem::take(&mut self.partial);
        all.extend_from_slice(&data);
        let mut lines: Vec<Vec<u8>> = all.split(|b| *b == b'\n').map(<[u8]>::to_vec).collect();
        self.partial = lines.pop().unwrap_or_default();
        if final_ && !self.partial.is_empty() {
            lines.push(std::mem::take(&mut self.partial));
        }
        for raw in lines {
            // Skip blank lines: only ASCII space, \t, \n, \r, \v and \f.
            if !raw.iter().all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)) {
                self.handle(&raw);
            }
        }
    }

    fn handle(&mut self, raw: &[u8]) {
        let obj = match serde_json::from_slice::<Value>(raw) {
            Ok(v @ Value::Object(_)) => v,
            _ => json!({}),
        };
        let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
        let subtype = obj.get("subtype").and_then(Value::as_str).unwrap_or("");
        let is_init = kind == "system" && subtype == "init";
        if let Some(turn) = &mut self.turn {
            if let Some(out) = &mut turn.out {
                let _ = out.write_all(raw);
                let _ = out.write_all(b"\n");
            }
        } else if self.stray.is_some() || is_init {
            self.write_stray(raw);
        }
        if is_init {
            self.busy = true;
            self.wake_until = 0.0;
            let again = self.turn.as_ref().filter(|t| t.interrupt_sent != 0.0 && !t.command_done).map(|t| t.id.clone());
            if let Some(id) = again {
                // The interrupt stopped an earlier round, and the turn's own came next.
                self.send(&json!({"type": "control_request", "request_id": format!("stop-{id}-again"), "request": {"subtype": "interrupt"}}));
            }
        } else if kind == "system" && subtype == "background_tasks_changed" {
            self.tasks = obj.get("tasks").and_then(Value::as_array).map_or(0, Vec::len);
        } else if kind == "system" && subtype == "task_notification" {
            self.wake_until = sys::awake() + WAKE_WAIT_SECONDS;
        } else if kind == "command_lifecycle" {
            self.markers = true;
            if let Some(turn) = &mut self.turn {
                let state = obj.get("state").and_then(Value::as_str).unwrap_or("");
                if obj.get("command_uuid").and_then(Value::as_str) == Some(turn.uuid.as_str()) && matches!(state, "completed" | "cancelled") {
                    turn.command_done = true;
                }
            }
        } else if kind == "user" && am_core::format::truthy(obj.get("isReplay")) {
            self.markers = true;
            if let Some(turn) = &mut self.turn {
                if obj.get("uuid").and_then(Value::as_str) == Some(turn.uuid.as_str()) {
                    turn.replayed = true;
                }
            }
        } else if kind == "result" {
            self.busy = false;
            let markers = self.markers;
            if let Some(turn) = &mut self.turn {
                let text = obj.get("result").and_then(Value::as_str).unwrap_or("");
                let is_error = am_core::format::truthy(obj.get("is_error"));
                if !am_core::format::strip(text).is_empty() || is_error {
                    turn.error = is_error;
                }
                // Without markers, the first result after the message is the turn's.
                if turn.replayed || !markers {
                    turn.command_done = true;
                }
            } else {
                self.stray_done();
            }
        }
        self.last_activity = sys::wall();
    }

    // Rounds the CLI runs with no turn open, such as for a background task that ends
    // after an interrupted turn. The runner shows each finished one as a note.

    fn write_stray(&mut self, raw: &[u8]) {
        if self.stray.is_none() {
            let _ = fs::create_dir_all(self.dir.join("strays"));
            self.stray_count += 1;
            let path = self.dir.join("strays").join(format!("{:04}.jsonl", self.stray_count));
            self.stray = OpenOptions::new().append(true).create(true).open(path).ok();
            if self.stray.is_none() {
                return;
            }
        }
        if let Some(f) = &mut self.stray {
            let _ = f.write_all(raw);
            let _ = f.write_all(b"\n");
        }
    }

    fn stray_done(&mut self) {
        if self.stray.take().is_none() {
            return;
        }
        sys::write_atomic(&self.dir.join("strays").join(format!("{:04}.done", self.stray_count)), &json!({"ended": sys::wall()}));
    }

    // ------------------------------------------------------------ turns

    fn next_request(&self) -> Option<(String, PathBuf)> {
        let queue = self.dir.join("queue");
        let mut names: Vec<String> = am_core::fsx::listdir(&queue)
            .ok()?
            .into_iter()
            .filter(|n| n.ends_with(".json") && !n.starts_with('.'))
            .collect();
        names.sort();
        for name in names {
            let request = sys::read_json(&queue.join(&name));
            let _ = fs::remove_file(queue.join(&name));
            if let Some(request) = request {
                let turn_dir = request.get("turn_dir").and_then(Value::as_str).unwrap_or("");
                if !turn_dir.is_empty() {
                    let turn = match request.get("turn") {
                        Some(Value::String(s)) => s.clone(),
                        _ => String::new(),
                    };
                    return Some((turn, PathBuf::from(turn_dir)));
                }
            }
        }
        None
    }

    fn open(&mut self, turn_id: &str, directory: &Path) {
        let Some(lock) = sys::try_lock(&directory.join("alive.lock")) else {
            return; // Someone else owns this turn.
        };
        let out = OpenOptions::new().append(true).create(true).open(directory.join("stream.jsonl")).ok();
        let stderr_offset = fs::metadata(self.dir.join("stderr.log")).map_or(0, |m| m.len());
        // A round already under way: what's left of it goes in with this turn.
        self.stray = None;
        let turn = Turn {
            id: turn_id.to_string(),
            dir: directory.to_path_buf(),
            uuid: message_uuid(turn_id),
            started: sys::wall(),
            awake_started: sys::awake(),
            _lock: lock,
            out,
            stderr_offset,
            replayed: false,
            command_done: false,
            error: false,
            interrupt_sent: 0.0,
            term_sent: 0.0,
            kill_sent: 0.0,
        };
        // `awake_started` times the turn's limit from when the agent gets the turn, and
        // only while the machine is awake, as `am __turn` records it.
        sys::write_atomic(
            &directory.join("pid.json"),
            &json!({
                "supervisor": std::process::id(), "child": self.child_pid(), "started": turn.started,
                "awake_started": turn.awake_started, "proc": self.dir_name(),
            }),
        );
        sys::write_atomic(&self.dir.join("current.json"), &json!({"turn": turn_id, "uuid": turn.uuid, "opened": turn.started}));
        let uuid = turn.uuid.clone();
        self.turn = Some(turn);
        self.save_state(true);
        if directory.join("interrupt.json").exists() {
            // Stopped before it reached the agent: the agent never sees it.
            self.close_turn(Some(0), true);
            return;
        }
        let prompt = fs::read(directory.join("prompt.md")).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        self.send(&json!({
            "type": "user", "uuid": uuid, "session_id": "", "parent_tool_use_id": null,
            "message": {"role": "user", "content": prompt},
        }));
    }

    fn dir_name(&self) -> String {
        self.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    fn send(&mut self, obj: &Value) {
        if let Some(stdin) = &mut self.stdin {
            let mut line = sys::encode_ascii(obj);
            line.push('\n');
            // The agent is gone if this fails; its exit closes the turn.
            let _ = stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush());
        }
    }

    fn watch_turn(&mut self) {
        let pid = self.child_pid();
        let Some(turn) = &self.turn else { return };
        if (self.stop || turn.dir.join("interrupt.json").exists()) && turn.interrupt_sent == 0.0 {
            let request = json!({"type": "control_request", "request_id": format!("stop-{}", turn.id), "request": {"subtype": "interrupt"}});
            if let Some(turn) = &mut self.turn {
                turn.interrupt_sent = sys::awake();
            }
            self.send(&request);
        }
        let Some(turn) = &self.turn else { return };
        let (command_done, interrupt_sent, error) = (turn.command_done, turn.interrupt_sent, turn.error);
        if command_done && !self.busy {
            if interrupt_sent != 0.0 {
                let tasks_left = self.tasks > 0;
                self.close_turn(Some(0), true);
                if tasks_left || self.stop {
                    // An interrupt stops everything the turn started, as killing a one-shot
                    // turn does. The CLI keeps its background tasks, so it goes too.
                    self.close_process("stopped");
                }
                return;
            }
            if self.tasks == 0 && sys::awake() >= self.wake_until {
                self.close_turn(Some(0), false);
                if error {
                    // After a failure, a retry starts clean.
                    self.close_process("failed");
                }
                return;
            }
        }
        let grace = self.grace;
        let Some(turn) = &mut self.turn else { return };
        if turn.interrupt_sent != 0.0 {
            let waited = sys::awake() - turn.interrupt_sent;
            if waited > grace && turn.term_sent == 0.0 {
                if sys::killpg(pid, libc::SIGTERM) {
                    turn.term_sent = sys::awake();
                }
            } else if turn.term_sent != 0.0
                && sys::awake() - turn.term_sent > grace
                && turn.kill_sent == 0.0
                && sys::killpg(pid, libc::SIGKILL)
            {
                turn.kill_sent = sys::awake();
            }
        }
    }

    fn close_turn(&mut self, code: Option<i64>, stopped: bool) {
        let Some(mut turn) = self.turn.take() else { return };
        self.copy_stderr(&turn);
        turn.out = None;
        sys::write_atomic(
            &turn.dir.join("exit.json"),
            &json!({"code": code, "started": turn.started, "ended": sys::wall(), "stopped": stopped, "proc": self.dir_name()}),
        );
        let _ = fs::remove_file(self.dir.join("current.json"));
        drop(turn); // Releases its alive.lock, after exit.json, as `am __turn` does.
        self.turns += 1;
        self.last_activity = sys::wall();
        self.save_state(true);
        sys::wake(self.spec.get("wake"));
    }

    fn copy_stderr(&self, turn: &Turn) {
        let Ok(mut f) = File::open(self.dir.join("stderr.log")) else { return };
        let mut data = Vec::new();
        if f.seek(SeekFrom::Start(turn.stderr_offset)).is_err() || f.read_to_end(&mut data).is_err() {
            return;
        }
        if let Ok(mut out) = OpenOptions::new().append(true).create(true).open(turn.dir.join("stderr.log")) {
            let _ = out.write_all(&data);
        }
    }

    /// Turns handed over that the agent never got. If it died at start (a session that
    /// can't be resumed, say), the next one is told how, so the runner can classify the
    /// failure. If the process was shutting down, they simply run again.
    fn fail_queue(&mut self, code: Option<i64>, reason: &str, never_ran: bool) {
        loop {
            let Some((_turn_id, directory)) = self.next_request() else {
                sys::wake(self.spec.get("wake"));
                return;
            };
            if never_ran {
                sys::write_atomic(
                    &directory.join("exit.json"),
                    &json!({"code": null, "never_ran": true, "reason": reason, "ended": sys::wall()}),
                );
                continue;
            }
            if let Ok(data) = fs::read(self.dir.join("stderr.log")) {
                if let Ok(mut out) = OpenOptions::new().append(true).create(true).open(directory.join("stderr.log")) {
                    let _ = out.write_all(&data[data.len().saturating_sub(4000)..]);
                }
            }
            let now = sys::wall();
            let code = match code {
                Some(c) if c != 0 => c,
                _ => 1,
            };
            sys::write_atomic(
                &directory.join("exit.json"),
                &json!({"code": code, "started": now, "ended": now, "stopped": false, "proc": self.dir_name()}),
            );
        }
    }

    // ------------------------------------------------------------ shutting down

    /// End the agent: close its stdin, then escalate if it doesn't go.
    fn close_process(&mut self, reason: &str) {
        if !self.closing.is_empty() {
            return;
        }
        self.closing = reason.to_string();
        self.closing_at = sys::awake();
        self.save_state(true);
        self.stdin = None;
        if reason == "stopped" && self.child.is_some() {
            sys::killpg(self.child_pid(), libc::SIGTERM);
        }
    }

    fn escalate_close(&self) {
        if self.child.is_none() {
            return;
        }
        let waited = sys::awake() - self.closing_at;
        if waited > 2.0 * self.grace {
            sys::killpg(self.child_pid(), libc::SIGKILL);
        } else if waited > self.grace {
            sys::killpg(self.child_pid(), libc::SIGTERM);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn message_uuid_is_stable() {
        // The v5 uuid of "am-turn:t-0mh3k2x9a7f3q" in the URL namespace.
        assert_eq!(super::message_uuid("t-0mh3k2x9a7f3q"), EXPECTED_UUID);
    }
    const EXPECTED_UUID: &str = "c5a9bbb8-11b4-534e-ac26-39b9bbbe9d97";
}
