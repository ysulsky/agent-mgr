//! Finishing turns, and when turns go wrong.

use super::{as_str, classify_failure, record_usage_totals, Runner, HOLD_AFTER_AUTH_FAILURE, HOLD_AFTER_LIMIT, LOG};
use crate::backends::{self, StreamResult};
use crate::{log, prompts, convert, store};
use am_core::{excerpt, first_line, now, read_json, turn_dir, write_json, Result, Value, MANAGER};
use serde_json::json;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// A stored number plus `n`: an integer stays an integer, a float a float, and a bool
/// counts as 0 or 1. Anything else counts as missing, giving `n`.
fn add_int(v: Option<&Value>, n: i64) -> Value {
    match v {
        Some(Value::Number(x)) if x.is_i64() => json!(x.as_i64().unwrap() + n),
        Some(Value::Number(x)) if x.is_u64() => json!(x.as_u64().unwrap() as i64 + n),
        Some(Value::Number(x)) => convert::float_value(x.as_f64().unwrap_or(0.0) + n as f64),
        Some(Value::Bool(b)) => json!(*b as i64 + n),
        _ => json!(n),
    }
}

fn messages_of(slug: &str, name: &str, meta: &Value) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    if let Some(Value::Array(ids)) = meta.get("messages") {
        for id in ids {
            if let Some(m) = store::get_message(slug, name, &convert::text(id))?.filter(|m| am_core::format::truthy(Some(m))) {
                out.push(m);
            }
        }
    }
    Ok(out)
}

/// Whether the exit code isn't 0. A missing or non-numeric code counts as nonzero.
fn nonzero(code: Option<&Value>) -> bool {
    match code {
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::Bool(b)) => *b,
        _ => true,
    }
}

fn final_or_no_reply(result: &Value) -> Result<String> {
    let final_ = convert::or_str(result.get("final"));
    let final_ = am_core::format::strip(&final_);
    if final_.is_empty() {
        prompts::render_kv("messages/no-reply", &[])
    } else {
        Ok(final_.to_string())
    }
}

impl Runner {
    pub(crate) fn finish(&mut self, slug: &str, name: &str, meta: &Value, exit_info: &Value) -> Result<()> {
        let turn_id = as_str(meta, "id").to_string();
        let directory = turn_dir(slug, name, &turn_id);
        if directory.join("result.json").exists() {
            self.clear_active(slug, name, &turn_id);
            return Ok(());
        }
        let base = match meta.get("usage_base") {
            Some(b) => b.clone(),
            None => self.usage_base(slug, name, meta.get("session_in").and_then(Value::as_str))?,
        };
        let parsed = backends::parse_stream(
            as_str(meta, "backend"),
            &directory.join("stream.jsonl"),
            self.cfg.get("pricing"),
            as_str(meta, "model"),
            Some(&base),
        );
        let stopped = am_core::format::truthy(exit_info.get("stopped")) || directory.join("interrupt.json").exists();
        let failed = !stopped && (nonzero(exit_info.get("code")) || parsed.is_error);
        let mut error = String::new();
        if failed {
            error = parsed.error.clone();
            if error.is_empty() {
                error = tail(&directory.join("stderr.log"), 1500);
            }
            if error.is_empty() {
                error = format!("exit code {}", exit_info.get("code").map_or_else(|| "None".to_string(), convert::text));
            }
        }
        let messages = messages_of(slug, name, meta)?;
        let ended = exit_info.get("ended").filter(|e| am_core::format::truthy(Some(e))).cloned().unwrap_or_else(|| json!(now()));
        let mut result = json!({
            "state": if stopped { "interrupted" } else if failed { "failed" } else { "done" },
            "final": parsed.final_,
            "error": error,
            "code": exit_info.get("code").cloned().unwrap_or(Value::Null),
            "ended": ended,
            "started": exit_info.get("started").cloned().unwrap_or(Value::Null),
            "cost_usd": convert::float_value(parsed.cost_usd),
            "cost_is_estimate": parsed.cost_is_estimate,
            "tokens": parsed.tokens.to_value(),
            "session": parsed.session,
        });
        self.add_usage(slug, name, meta, &parsed, &result)?;
        if stopped {
            self.after_interrupt(slug, name, meta, &messages, &mut result)?;
        } else if failed {
            self.after_failure(slug, name, meta, &messages, &mut result)?;
        } else if as_str(meta, "kind") == "side" {
            self.route_side(slug, name, meta, &messages, &result)?;
        } else {
            self.route_main(slug, name, meta, &messages, &result)?;
        }
        write_json(&directory.join("result.json"), &result)?;
        self.clear_active(slug, name, &turn_id);
        log::info(LOG, &format!("turn {turn_id} of {slug}/{name}: {}", as_str(&result, "state")));
        Ok(())
    }

    fn add_usage(&self, slug: &str, name: &str, meta: &Value, parsed: &StreamResult, result: &Value) -> Result<()> {
        let main = as_str(meta, "kind") == "main";
        let done = as_str(result, "state") == "done";
        store::update_agent(slug, name, |agent| {
            let cost = agent.get("cost_usd").map_or(0.0, convert::to_float);
            agent["cost_usd"] = convert::float_value(cost + parsed.cost_usd);
            if parsed.cost_is_estimate {
                agent["cost_is_estimate"] = json!(true);
            }
            if agent.get("tokens").is_none() {
                agent["tokens"] = json!({"input": 0, "output": 0});
            }
            let tokens = &mut agent["tokens"];
            tokens["input"] = add_int(tokens.get("input"), parsed.tokens.input);
            tokens["output"] = add_int(tokens.get("output"), parsed.tokens.output);
            tokens["cached_input"] = add_int(tokens.get("cached_input"), parsed.tokens.cached_input);
            agent["turns"] = add_int(agent.get("turns"), 1);
            if main {
                record_usage_totals(agent, parsed.session.as_deref(), &parsed.running);
                let record = json!({"backend": meta["backend"], "model": meta.get("model").cloned().unwrap_or(json!("")), "id": parsed.session});
                let has_session = parsed.session.as_deref().is_some_and(|s| !s.is_empty());
                if done {
                    agent["retries"] = json!(0);
                    agent["not_before"] = json!(0);
                    agent["last_error"] = Value::Null;
                    if has_session {
                        agent["session"] = record;
                    }
                } else if has_session && agent.get("session").and_then(|s| s.get("backend")).unwrap_or(&Value::Null) == &meta["backend"] {
                    agent["session"] = record;
                }
                if agent.get("main_turn") == meta.get("id") {
                    agent["main_turn"] = Value::Null;
                }
            }
        })?;
        if done && main {
            for key in ["login", "limit", "failing"] {
                store::resolve_attention_key(slug, &format!("{key}-{name}"), "runner")?;
            }
        }
        Ok(())
    }

    pub(crate) fn route_main(&mut self, slug: &str, name: &str, meta: &Value, messages: &[Value], result: &Value) -> Result<()> {
        let turn_id = as_str(meta, "id");
        let short = turn_id.get(2..).unwrap_or("");
        let agent = store::get_agent(slug, name)?;
        let final_ = final_or_no_reply(result)?;
        let questions: Vec<Value> = messages.iter().filter(|m| as_str(m, "kind") == "user").cloned().collect();
        self.answer_or_note(slug, name, meta, messages, &questions, &final_, result)?;
        let worker = as_str(&agent, "role") == "worker";
        for (index, message) in messages.iter().filter(|m| as_str(m, "kind") == "agent").enumerate() {
            if worker && message.get("from").and_then(Value::as_str) == Some(format!("{slug}/{MANAGER}").as_str()) {
                continue; // The report below already reaches the manager.
            }
            self.reply_to_agent(slug, name, message, &final_, &format!("m-{short}-p{index}"))?;
        }
        if worker {
            let opts = store::SendOpts {
                sender: format!("{slug}/{name}"),
                kind: Some("report".into()),
                msg_id: Some(format!("m-{short}-r")),
                meta: Some(json!({"turn": turn_id, "worker": name})),
                ..Default::default()
            };
            store::send(slug, MANAGER, &final_, opts)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn answer_or_note(&self, slug: &str, name: &str, meta: &Value, messages: &[Value], questions: &[Value], final_: &str, result: &Value) -> Result<()> {
        let turn_id = as_str(meta, "id");
        if let Some(question) = store::answer_target(questions) {
            let mut answered = false;
            for q in questions {
                if store::answered_in_turn(slug, name, as_str(q, "id"), turn_id)? {
                    answered = true;
                    break;
                }
            }
            if !answered {
                store::publish_answer(slug, name, as_str(&question, "id"), final_, Some(turn_id))?;
                return Ok(());
            }
        }
        self.note(slug, name, meta, messages, final_, result, "done")
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn note(&self, slug: &str, name: &str, meta: &Value, messages: &[Value], text: &str, result: &Value, state: &str) -> Result<()> {
        let or = |v: Option<&Value>, fallback: Value| v.filter(|x| am_core::format::truthy(Some(x))).cloned().unwrap_or(fallback);
        let note = json!({
            "id": meta["id"],
            "ts": or(result.get("ended"), json!(now())),
            "started": or(result.get("started"), meta.get("created").cloned().unwrap_or(Value::Null)),
            "text": first_line(text, 200),
            "full": text,
            "state": state,
            "kind": meta.get("kind").cloned().unwrap_or(Value::Null),
            "triggers": messages.iter().map(store::describe_trigger).collect::<Vec<_>>(),
            "cost_usd": result.get("cost_usd").cloned().unwrap_or(json!(0)),
            "cost_is_estimate": result.get("cost_is_estimate").cloned().unwrap_or(json!(false)),
            "tokens": result.get("tokens").cloned().unwrap_or(Value::Null),
            "agent": name,
        });
        store::write_note(slug, name, as_str(meta, "id"), &note)
    }

    pub(crate) fn route_side(&mut self, slug: &str, name: &str, meta: &Value, messages: &[Value], result: &Value) -> Result<()> {
        let turn_id = as_str(meta, "id");
        let short = turn_id.get(2..).unwrap_or("");
        let final_ = final_or_no_reply(result)?;
        let Some(message) = messages.first() else { return Ok(()) };
        let kind = as_str(message, "kind");
        if kind == "user" {
            if !store::answered_in_turn(slug, name, as_str(message, "id"), turn_id)? {
                store::publish_answer(slug, name, as_str(message, "id"), &final_, Some(turn_id))?;
            }
            let opts = store::SendOpts {
                sender: format!("{slug}/{name}"),
                kind: Some("fyi".into()),
                wake: false,
                msg_id: Some(format!("m-{short}-f")),
                meta: Some(json!({"fyi": "worker-answer", "worker": name, "original": message["id"]})),
                ..Default::default()
            };
            store::send(slug, MANAGER, &final_, opts)?;
        } else if kind == "agent" {
            self.reply_to_agent(slug, name, message, &final_, &format!("m-{short}-p0"))?;
        }
        let asked_by = if kind == "user" { "The user".to_string() } else { message.get("from").map_or_else(|| "None".into(), convert::text) };
        let question = message.get("text").map_or_else(String::new, convert::text);
        let text = prompts::render_kv("messages/side-exchange", &[("asked_by", &asked_by), ("question", &question), ("reply", &final_)])?;
        let opts = store::SendOpts {
            sender: "daemon".into(),
            kind: Some("exchange".into()),
            no_side: true,
            msg_id: Some(format!("m-{short}-x")),
            meta: Some(json!({"side_turn": turn_id, "original": message["id"]})),
            allow_retired: true,
            ..Default::default()
        };
        store::send(slug, name, &text, opts)?;
        Ok(())
    }

    fn reply_to_agent(&self, slug: &str, name: &str, message: &Value, text: &str, msg_id: &str) -> Result<()> {
        let sender = message.get("from").map_or_else(String::new, convert::text);
        let Some((to_slug, to_name)) = sender.split_once('/') else { return Ok(()) };
        if !store::agent_exists(to_slug, to_name) {
            return Ok(());
        }
        let original = message.get("text").map_or_else(String::new, convert::text);
        let opts = store::SendOpts {
            sender: format!("{slug}/{name}"),
            kind: Some("reply".into()),
            msg_id: Some(msg_id.into()),
            reply_to: Some(json!({"id": message["id"], "excerpt": excerpt(&original, 240)})),
            ..Default::default()
        };
        match store::send(to_slug, to_name, text, opts) {
            Err(e) if !store::is_am_error(&e) => Err(e),
            _ => Ok(()),
        }
    }

    // ------------------------------------------------------------ when turns go wrong

    pub(crate) fn after_interrupt(&mut self, slug: &str, name: &str, meta: &Value, messages: &[Value], result: &mut Value) -> Result<()> {
        let turn_id = as_str(meta, "id");
        let mut unanswered: Vec<String> = Vec::new();
        for m in messages {
            if as_str(m, "kind") == "user" && !store::answered_in_turn(slug, name, as_str(m, "id"), turn_id)? {
                unanswered.push(convert::text(&m["id"]));
            }
        }
        let unanswered = unanswered.join(", ");
        let (note, mut line);
        if let Some(limit) = meta.get("time_limit").filter(|l| am_core::format::truthy(Some(l))) {
            let text = limit.get("text").map_or_else(|| "None".into(), convert::text);
            let role = limit.get("role").map_or_else(|| "None".into(), convert::text);
            let mut values = vec![("turn_id", turn_id), ("limit", text.as_str()), ("role", role.as_str())];
            note = if !unanswered.is_empty() {
                values.push(("unanswered", &unanswered));
                prompts::render_kv("notes/time-limit-unanswered", &values)?
            } else {
                prompts::render_kv("notes/time-limit", &values)?
            };
            line = format!("Stopped after {text}, the time limit for a {role} turn");
        } else if !unanswered.is_empty() {
            note = prompts::render_kv("notes/interrupted-unanswered", &[("turn_id", turn_id), ("unanswered", &unanswered)])?;
            line = "Interrupted".to_string();
        } else {
            note = prompts::render_kv("notes/interrupted", &[("turn_id", turn_id)])?;
            line = "Interrupted".to_string();
        }
        if am_core::format::truthy(meta.get("auto_interrupt")) {
            // It was set aside for the user's message, so its own messages run again.
            for message in messages {
                store::requeue(slug, name, as_str(message, "id"), false)?;
            }
            line = "Interrupted for a message from the user; its messages run again next".to_string();
        }
        if as_str(meta, "kind") == "main" {
            store::update_agent(slug, name, |a| push_note(a, &note))?;
        }
        self.note(slug, name, meta, messages, &line, result, "interrupted")
    }

    pub(crate) fn after_failure(&mut self, slug: &str, name: &str, meta: &Value, messages: &[Value], result: &mut Value) -> Result<()> {
        let error = convert::or_str(result.get("error"));
        let turn_id = as_str(meta, "id");
        if as_str(meta, "kind") == "side" {
            // The message isn't lost: the main turn will take it.
            for message in messages {
                store::requeue(slug, name, as_str(message, "id"), true)?;
            }
            result["state"] = json!("failed");
            log::warning(LOG, &format!("side turn {turn_id} failed: {}", excerpt(&error, 240)));
            return Ok(());
        }
        let stderr = tail(&turn_dir(slug, name, turn_id).join("stderr.log"), 1500);
        let kind = classify_failure(&format!("{error}\n{stderr}"));
        let agent = store::get_agent(slug, name)?;
        let limit = convert::to_int(self.cfg["runner"].get("max_retries"));
        let retries_so_far = agent.get("retries").cloned().unwrap_or(json!(0));
        let next = convert::to_int(Some(&retries_so_far)) + 1;
        match kind {
            "session" if next <= limit => {
                let template = if as_str(&agent, "role") == "manager" { "notes/session-lost-manager" } else { "notes/session-lost" };
                let note = prompts::render_kv(template, &[])?;
                return self.retry(slug, name, meta, messages, json!(next), 0.0, Some(&note), result, true, true, None);
            }
            "auth" | "limit" => {
                let auth = kind == "auth";
                let hold = if auth { HOLD_AFTER_AUTH_FAILURE } else { HOLD_AFTER_LIMIT };
                let backend = as_str(meta, "backend");
                let title = if auth { format!("{backend} login needed") } else { format!("{backend} usage limit reached") };
                let key = format!("{}-{name}", if auth { "login" } else { "limit" });
                let run = if !auth {
                    None
                } else if backend == "claude" {
                    Some("claude auth login")
                } else {
                    Some("codex login")
                };
                let body = format!("Agent {name} can't run, and its messages are waiting. It retries every few minutes.\n\n{}", excerpt(&error, 600));
                store::add_attention(slug, &title, &body, if auth { "action" } else { "alert" }, "runner", &key, run)?;
                let last_error = excerpt(&error, 300);
                return self.retry(slug, name, meta, messages, retries_so_far, hold, None, result, false, false, Some(&last_error));
            }
            "other" if next <= limit => {
                let delay = 30.0 * 4f64.powi((next - 1) as i32);
                let note = prompts::render_kv("notes/failed", &[("turn_id", turn_id), ("error", &excerpt(&error, 300))])?;
                return self.retry(slug, name, meta, messages, json!(next), delay, Some(&note), result, false, false, None);
            }
            _ => {}
        }
        // Out of retries.
        self.note(slug, name, meta, messages, &format!("Turn failed: {}", excerpt(&error, 600)), result, "failed")?;
        let body = format!("The last attempt failed with:\n\n{}\n\nSend it another message to try again.", excerpt(&error, 1000));
        store::add_attention(slug, &format!("Agent {name} keeps failing"), &body, "alert", "runner", &format!("failing-{name}"), None)?;
        store::update_agent(slug, name, |a| {
            a["retries"] = json!(0);
            a["not_before"] = json!(0);
            a["last_error"] = json!(excerpt(&error, 300));
        })?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn retry(
        &self,
        slug: &str,
        name: &str,
        meta: &Value,
        messages: &[Value],
        retries: Value,
        delay: f64,
        note: Option<&str>,
        result: &mut Value,
        clear_session: bool,
        needs_history: bool,
        last_error: Option<&str>,
    ) -> Result<()> {
        for message in messages {
            store::requeue(slug, name, as_str(message, "id"), false)?;
        }
        // The notes this turn carried go back to the agent, as for a turn that never ran.
        // When the retry needs a new session (a first turn was interrupted before its CLI
        // created one, say), or the CLI refused the turn, the agent never read them, and
        // the "was interrupted" note would be lost.
        let given: Vec<Value> = if as_str(meta, "kind") == "main" { meta.get("notes").and_then(Value::as_array).cloned().unwrap_or_default() } else { vec![] };
        store::update_agent(slug, name, |a| {
            a["retries"] = retries;
            a["not_before"] = convert::float_value(now() + delay);
            if !given.is_empty() {
                let mut merged = given.clone();
                if let Some(Value::Array(old)) = a.get("notes") {
                    merged.extend(old.iter().filter(|n| !given.contains(n)).cloned());
                }
                a["notes"] = Value::Array(merged);
            }
            if let Some(note) = note.filter(|n| !n.is_empty()) {
                push_note(a, note);
            }
            if clear_session {
                a["session"] = Value::Null;
                a["needs_history"] = json!(needs_history);
            }
            if let Some(e) = last_error.filter(|e| !e.is_empty()) {
                a["last_error"] = json!(e);
            }
        })?;
        result["state"] = json!("retrying");
        Ok(())
    }

    /// The supervisor is gone with no exit record, as after a machine restart.
    pub(crate) fn vanished(&mut self, slug: &str, name: &str, meta: &Value) -> Result<()> {
        let turn_id = as_str(meta, "id");
        let directory = turn_dir(slug, name, turn_id);
        kill_orphan(&directory);
        let messages = messages_of(slug, name, meta)?;
        let mut result = json!({"state": "vanished", "ended": now(), "final": "", "error": "process vanished"});
        log::warning(LOG, &format!("turn {turn_id} of {slug}/{name} vanished"));
        if as_str(meta, "kind") == "side" {
            for message in &messages {
                store::requeue(slug, name, as_str(message, "id"), true)?;
            }
        } else {
            let agent = store::get_agent(slug, name)?;
            let retries = convert::to_int(agent.get("retries")) + 1;
            if retries <= convert::to_int(self.cfg["runner"].get("max_retries")) {
                let note = prompts::render_kv("notes/vanished", &[("turn_id", turn_id)])?;
                self.retry(slug, name, meta, &messages, json!(retries), 0.0, Some(&note), &mut result, false, false, None)?;
                result["state"] = json!("vanished");
            } else {
                self.note(slug, name, meta, &messages, "Turn lost: its process vanished, and retries ran out.", &result, "failed")?;
                store::update_agent(slug, name, |a| {
                    a["retries"] = json!(0);
                    a["last_error"] = json!("turn process vanished");
                })?;
            }
        }
        write_json(&directory.join("result.json"), &result)?;
        self.clear_active(slug, name, turn_id);
        Ok(())
    }
}

/// Append `note` to the agent's `notes`, starting the array if there isn't one.
fn push_note(agent: &mut Value, note: &str) {
    if !agent.get("notes").is_some_and(Value::is_array) {
        if agent.get("notes").is_none() {
            agent["notes"] = json!([]);
        } else {
            return; // Leave a non-array as it is.
        }
    }
    if let Some(Value::Array(notes)) = agent.get_mut("notes") {
        notes.push(json!(note));
    }
}

/// The end of a file, decoded with replacement, stripped.
fn tail(path: &Path, limit: u64) -> String {
    let Ok(mut f) = std::fs::File::open(path) else { return String::new() };
    let Ok(len) = f.seek(SeekFrom::End(0)) else { return String::new() };
    if f.seek(SeekFrom::Start(len.saturating_sub(limit))).is_err() {
        return String::new();
    }
    let mut data = Vec::new();
    if f.read_to_end(&mut data).is_err() {
        return String::new();
    }
    am_core::format::strip(&String::from_utf8_lossy(&data)).to_string()
}

/// If only the supervisor died, the agent may still be running. After a reboot the pid
/// may belong to something else, so check it's really ours.
fn kill_orphan(directory: &Path) {
    let Some(pid) = read_json(&directory.join("pid.json")).ok().flatten() else { return };
    let Some(child) = pid.get("child").and_then(Value::as_i64).filter(|c| *c != 0) else { return };
    let spec = read_json(&directory.join("cmd.json")).ok().flatten().unwrap_or(json!({}));
    let argv0 = spec.get("argv").and_then(|a| a.get(0)).map_or_else(String::new, convert::text);
    let program = Path::new(&argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let Ok(out) = std::process::Command::new("ps").args(["-o", "command=", "-p", &child.to_string()]).output() else { return };
    let command = String::from_utf8_lossy(&out.stdout);
    if !program.is_empty() && command.contains(&program) {
        crate::sys::killpg(child as u32, libc::SIGTERM);
    }
}
