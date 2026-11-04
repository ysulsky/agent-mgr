//! Watching turns (`runner.py`, "watching turns").

use super::{as_str, Runner, LOG, SPAWN_WINDOW_SECONDS};
use crate::{backends, log, procs, pyv, store, sys};
use am_core::{active_dir, awake_time, now, read_json, turn_dir, write_json, Result, Value};
use serde_json::json;
use std::path::Path;

fn read(path: &Path) -> Option<Value> {
    read_json(path).ok().flatten()
}

impl Runner {
    /// Finish turns that ended. Returns the ones still running.
    pub(crate) fn reap(&mut self, slug: &str, name: &str) -> Result<Vec<Value>> {
        let mut running = Vec::new();
        let dir = active_dir(slug, name);
        if !dir.is_dir() {
            return Ok(running);
        }
        let mut turn_ids = am_core::fsx::listdir(&dir)?;
        turn_ids.sort();
        for turn_id in turn_ids {
            if turn_id.starts_with('.') {
                continue;
            }
            let directory = turn_dir(slug, name, &turn_id);
            let Some(mut meta) = read(&directory.join("meta.json")).filter(|m| !m.is_null()) else {
                self.clear_active(slug, name, &turn_id);
                continue;
            };
            if as_str(&meta, "state") == "preparing" && directory.join("pid.json").exists() {
                // The daemon died between handing the turn over and recording that it had:
                // it's running, so it isn't abandoned.
                let pid = read(&directory.join("pid.json")).filter(|p| am_core::py::truthy(Some(p))).unwrap_or(json!({}));
                meta["state"] = json!("spawned");
                if am_core::py::truthy(pid.get("proc")) {
                    meta["proc"] = pid["proc"].clone();
                }
                write_json(&directory.join("meta.json"), &meta)?;
            }
            if as_str(&meta, "state") == "preparing" {
                self.abandon_prepared(slug, name, &meta)?;
                continue;
            }
            let exit_info = read(&directory.join("exit.json")).filter(|e| !e.is_null());
            if let Some(exit_info) = &exit_info {
                if am_core::py::truthy(exit_info.get("never_ran")) {
                    self.abandon_prepared(slug, name, &meta)?;
                } else {
                    self.finish(slug, name, &meta, exit_info)?;
                }
                continue;
            }
            if am_core::py::truthy(meta.get("proc")) && !directory.join("pid.json").exists() && !self.proc_alive(slug, name, &meta) {
                self.abandon_prepared(slug, name, &meta)?; // Its process ended before taking it.
                continue;
            }
            if self.alive(&directory, &meta) {
                running.push(meta);
                continue;
            }
            // The supervisor writes exit.json before it releases the lock.
            match read(&directory.join("exit.json")).filter(|e| !e.is_null()) {
                Some(exit_info) => self.finish(slug, name, &meta, &exit_info)?,
                None => self.vanished(slug, name, &meta)?,
            }
        }
        Ok(running)
    }

    fn alive(&self, directory: &Path, meta: &Value) -> bool {
        if !directory.join("pid.json").exists() {
            // The supervisor takes the lock before writing pid.json. Don't touch the lock
            // until it has had time to, or we could make it think it lost a race.
            return now() - meta.get("created").map_or(0.0, pyv::py_float) < SPAWN_WINDOW_SECONDS;
        }
        sys::try_lock(&directory.join("alive.lock")).is_none()
    }

    pub(crate) fn clear_active(&self, slug: &str, name: &str, turn_id: &str) {
        let _ = std::fs::remove_file(active_dir(slug, name).join(turn_id));
        am_core::poke(slug);
    }

    fn proc_alive(&self, slug: &str, name: &str, meta: &Value) -> bool {
        let directory = procs::procs_dir(slug, name).join(pyv::str_of(&meta["proc"]));
        procs::alive(&directory) && !directory.join("exit.json").exists()
    }

    /// The daemon died while setting this turn up, or its process ended before taking
    /// it: it never ran.
    pub(crate) fn abandon_prepared(&self, slug: &str, name: &str, meta: &Value) -> Result<()> {
        if let Some(Value::Array(ids)) = meta.get("messages") {
            for id in ids {
                store::requeue(slug, name, &pyv::str_of(id), false)?;
            }
        }
        if am_core::py::truthy(meta.get("notes")) && as_str(meta, "kind") == "main" {
            let notes = meta["notes"].as_array().cloned().unwrap_or_default();
            store::update_agent(slug, name, |a| {
                let mut merged = notes.clone();
                if let Some(Value::Array(old)) = a.get("notes") {
                    merged.extend(old.iter().filter(|n| !notes.contains(n)).cloned());
                }
                a["notes"] = Value::Array(merged);
            })?;
        }
        write_json(&turn_dir(slug, name, as_str(meta, "id")).join("result.json"), &json!({"state": "abandoned", "ended": now()}))?;
        self.clear_active(slug, name, as_str(meta, "id"));
        Ok(())
    }

    pub(crate) fn learn_session(&self, slug: &str, name: &str, agent: &Value, main: &Value) -> Result<()> {
        if let Some(record) = agent.get("session").filter(|r| am_core::py::truthy(Some(r))) {
            if record.get("backend") == main.get("backend") {
                return Ok(());
            }
        }
        let backend = as_str(main, "backend");
        let stream = turn_dir(slug, name, as_str(main, "id")).join("stream.jsonl");
        if let Some(session) = backends::peek_session(backend, &stream).filter(|s| !s.is_empty()) {
            let model = main.get("model").cloned().unwrap_or(json!(""));
            store::update_agent(slug, name, |a| {
                a["session"] = json!({"backend": backend, "model": model, "id": session});
            })?;
        }
        Ok(())
    }

    pub(crate) fn stop(&self, slug: &str, name: &str, meta: &mut Value) -> Result<()> {
        let directory = turn_dir(slug, name, as_str(meta, "id"));
        let Some(pid) = read(&directory.join("pid.json")).filter(|p| am_core::py::truthy(Some(p))) else {
            return Ok(()); // Still starting; the next tick tries again.
        };
        if !directory.join("interrupt.json").exists() {
            write_json(&directory.join("interrupt.json"), &json!({"at": now()}))?;
        }
        if now() - meta.get("stop_sent").map_or(0.0, pyv::py_float) < 5.0 {
            return Ok(());
        }
        meta["stopping"] = json!(true);
        meta["stop_sent"] = json!(now());
        write_json(&directory.join("meta.json"), meta)?;
        if !am_core::py::truthy(meta.get("proc")) {
            // A long-lived process's supervisor watches for interrupt.json instead: a
            // signal would stop the whole process, not just this turn.
            if let Some(sup) = pid.get("supervisor").and_then(Value::as_i64) {
                unsafe { libc::kill(sup as libc::pid_t, libc::SIGTERM) };
            }
        }
        am_core::poke(slug);
        log::info(LOG, &format!("stopping turn {} of {slug}/{name}", as_str(meta, "id")));
        Ok(())
    }

    /// Stop a turn that has run longer than its role allows. The clock starts when the
    /// agent's process does, so time spent queued or being set up doesn't count, and it
    /// counts only time the machine is awake (see `awake_elapsed`).
    pub(crate) fn enforce_time_limit(&self, slug: &str, name: &str, agent: &Value, running: &mut [Value]) -> Result<()> {
        if running.is_empty() {
            return Ok(());
        }
        let role = if as_str(agent, "role") == "manager" { "manager" } else { "worker" };
        let key = format!("{role}_turn_limit");
        let Some(limit) = self.seconds(&key, false) else { return Ok(()) };
        let text = pyv::str_of(&self.cfg["runner"][key.as_str()]);
        for meta in running.iter_mut() {
            match self.awake_elapsed(slug, name, meta)? {
                Some(elapsed) if elapsed >= limit => {}
                _ => continue,
            }
            if !am_core::py::truthy(meta.get("time_limit")) {
                meta["time_limit"] = json!({"seconds": pyv::float_value(limit), "text": text, "role": role});
                log::info(LOG, &format!("turn {} of {slug}/{name} hit the {text} limit", as_str(meta, "id")));
            }
            self.stop(slug, name, meta)?;
        }
        Ok(())
    }

    /// How long the turn's process has run while the machine was awake, or None if it
    /// hasn't started. The supervisor records when it started on the awake clock, which
    /// every process shares, so this holds across daemon restarts. A turn whose
    /// supervisor is from before that was recorded is timed from when a runner first saw
    /// it, which is recorded in its meta, so it can't be stopped early, and a restart
    /// doesn't start its clock again.
    fn awake_elapsed(&self, slug: &str, name: &str, meta: &mut Value) -> Result<Option<f64>> {
        let directory = turn_dir(slug, name, as_str(meta, "id"));
        let pid = read(&directory.join("pid.json")).filter(|p| am_core::py::truthy(Some(p))).unwrap_or(json!({}));
        if !am_core::py::truthy(pid.get("started")) {
            return Ok(None);
        }
        let started = match pid.get("awake_started") {
            Some(v) => v.clone(),
            None => meta.get("awake_seen").cloned().unwrap_or(Value::Null),
        };
        if started.is_null() {
            meta["awake_seen"] = json!(awake_time());
            write_json(&directory.join("meta.json"), meta)?;
            return Ok(Some(0.0));
        }
        let elapsed = awake_time() - pyv::py_float(&started);
        // Recorded before a reboot, so it isn't running.
        Ok(if elapsed >= 0.0 { Some(elapsed) } else { None })
    }

    /// A message from the user doesn't wait behind a manager's housekeeping. If the
    /// running turn wasn't started by the user and is still going after a short grace
    /// period, it's interrupted the way "Interrupt and send" does it, and its own messages
    /// are delivered again with the user's, which goes first.
    pub(crate) fn auto_interrupt(&self, slug: &str, name: &str, main: &mut Value, pending: &[Value]) -> Result<()> {
        let Some(grace) = self.seconds("auto_interrupt_after", true) else { return Ok(()) };
        if am_core::py::truthy(main.get("stopping")) {
            return Ok(());
        }
        if let Some(Value::Array(triggers)) = main.get("triggers") {
            if triggers.iter().any(|t| as_str(t, "kind") == "user") {
                return Ok(());
            }
        }
        let waiting: Vec<&Value> = pending
            .iter()
            .filter(|m| as_str(m, "kind") == "user" && now() - pyv::py_float_or0(m.get("ts")) >= grace)
            .collect();
        if waiting.is_empty() {
            return Ok(());
        }
        let ids: Vec<Value> = waiting.iter().map(|m| m["id"].clone()).collect();
        main["auto_interrupt"] = json!({"at": now(), "for": ids});
        write_json(&turn_dir(slug, name, as_str(main, "id")).join("meta.json"), main)?;
        for message in &waiting {
            store::mark_interrupt(slug, name, as_str(message, "id"))?;
        }
        let list: Vec<String> = waiting.iter().map(|m| pyv::str_of(&m["id"])).collect();
        log::info(LOG, &format!("interrupting {slug}/{name}'s turn {} for {}", as_str(main, "id"), list.join(", ")));
        self.stop(slug, name, main)
    }
}
