//! Starting turns, and handing them to long-lived processes.

use super::{as_str, Runner, HOLD_AFTER_AUTH_FAILURE, LOG, MAX_SIDE_TURNS};
use crate::backends::{self, TurnSpec};
use crate::{log, procs, prompts, convert, store};
use am_core::{active_dir, am_home, new_id, now, project_dir, turn_dir, write_json, Result, Value};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where a main turn goes when it runs on a long-lived process.
pub(crate) struct Persist {
    pub proc: Option<PathBuf>,
    pub instructions: String,
}

impl Runner {
    pub(crate) fn start_main(&mut self, slug: &str, name: &str, agent: &Value, pending: &[Value]) -> Result<()> {
        let (backend, model) = store::effective_backend(slug, agent, Some(&self.cfg))?;
        if !backends::available(&backend, &self.cfg) {
            return self.hold_for_missing_backend(slug, name, &backend);
        }
        let record = agent.get("session").filter(|r| am_core::format::truthy(Some(r))).cloned();
        let mut session: Option<String> = match &record {
            Some(r) if r.get("backend").and_then(Value::as_str) == Some(backend.as_str()) => r.get("id").map(convert::text),
            _ => None,
        };
        // A record from before models were recorded is taken to be on the current model.
        let model_changed = session.as_deref().is_some_and(|s| !s.is_empty())
            && record.as_ref().and_then(|r| r.get("model")).is_some_and(|m| *m != Value::String(model.clone()));
        if model_changed {
            session = None;
        }
        let mut notes: Vec<String> = match agent.get("notes") {
            Some(Value::Array(a)) => a.iter().map(convert::text).collect(),
            _ => vec![],
        };
        let mut history = String::new();
        if let (Some(r), None) = (&record, &session) {
            if model_changed {
                let old = convert::or_str(r.get("model"));
                let old = if old.is_empty() { "default".to_string() } else { old };
                let new = if model.is_empty() { "default" } else { model.as_str() };
                notes.push(prompts::render_kv("notes/model-changed", &[("old", &old), ("new", new)])?);
            } else {
                let old = r.get("backend").map_or_else(|| "None".to_string(), convert::text);
                notes.push(prompts::render_kv("notes/backend-changed", &[("old", &old), ("new", &backend)])?);
            }
            history = self.history(slug, name)?;
        } else if am_core::format::truthy(agent.get("needs_history")) {
            history = self.history(slug, name)?;
        }
        let project = store::get_project(slug)?;
        let system = prompts::system_prompt(&project, agent, true)?;
        let mut persist = None;
        if self.persistent(&backend) {
            let p = self.find_process(slug, name, agent, &project, &backend, &model, session.as_deref(), &system)?;
            let change = match &p.proc {
                Some(dir) => procs::memory_changes(dir)?,
                None => String::new(),
            };
            if !change.is_empty() {
                notes.push(prompts::render_kv("notes/memory-changed", &[("changes", &change)])?);
            }
            persist = Some(p);
        } else {
            procs::shutdown_all(slug, name, "changed")?;
        }
        let prompt = prompts::turn_prompt(slug, agent, pending, &notes, false, &history)?;
        let launch = Launch {
            kind: "main",
            messages: pending,
            backend: &backend,
            model: &model,
            session: session.as_deref(),
            fork: false,
            system: &system,
            prompt: &prompt,
            main_turn: None,
            persist,
            notes: &notes,
        };
        let (turn_id, new_session) = self.launch(slug, name, launch)?;
        store::update_agent(slug, name, |a| {
            let Some(a) = a.as_object_mut() else { return };
            a.insert("notes".into(), json!([]));
            a.insert("needs_history".into(), json!(false));
            a.insert("main_turn".into(), json!(turn_id));
            if let Some(new) = &new_session {
                a.insert("session".into(), json!({"backend": backend, "model": model, "id": new}));
            } else if session.is_none() {
                a.insert("session".into(), Value::Null); // Codex: learned from the stream once the turn starts.
            } else if let Some(Value::Object(s)) = a.get_mut("session") {
                if !s.is_empty() && !s.contains_key("model") {
                    s.insert("model".into(), json!(model));
                }
            }
        })?;
        Ok(())
    }

    pub(crate) fn start_side_turns(
        &mut self,
        slug: &str,
        name: &str,
        agent: &Value,
        main: &Value,
        running: &[Value],
        pending: &[Value],
    ) -> Result<()> {
        let eligible: Vec<&Value> = pending
            .iter()
            .filter(|m| am_store::mail::SIDE_TURN_KINDS.contains(&as_str(m, "kind")) && !am_core::format::truthy(m.get("no_side")))
            .collect();
        if eligible.is_empty() {
            return Ok(());
        }
        // The fork needs the main turn's session to exist on disk.
        let Some(record) = agent.get("session").filter(|r| am_core::format::truthy(Some(r))) else { return Ok(()) };
        if record.get("backend") != main.get("backend") {
            return Ok(());
        }
        let stream = turn_dir(slug, name, as_str(main, "id")).join("stream.jsonl");
        if fs::metadata(&stream).map_or(true, |m| m.len() == 0) {
            return Ok(());
        }
        let mut sides = running.iter().filter(|t| as_str(t, "kind") == "side").count();
        let project = store::get_project(slug)?;
        let system = prompts::system_prompt(&project, agent, true)?;
        let session = record.get("id").map(convert::text);
        for message in eligible {
            if sides >= MAX_SIDE_TURNS {
                return Ok(());
            }
            let messages = [message.clone()];
            let prompt = prompts::turn_prompt(slug, agent, &messages, &[], true, "")?;
            let launch = Launch {
                kind: "side",
                messages: &messages,
                backend: as_str(main, "backend"),
                model: as_str(main, "model"),
                session: session.as_deref(),
                fork: true,
                system: &system,
                prompt: &prompt,
                main_turn: Some(as_str(main, "id")),
                persist: None,
                notes: &[],
            };
            self.launch(slug, name, launch)?;
            sides += 1;
        }
        Ok(())
    }

    fn launch(&mut self, slug: &str, name: &str, l: Launch) -> Result<(String, Option<String>)> {
        if self.debug.dry_launch {
            self.debug.launched_prompt = Some(l.prompt.to_string());
            return Ok(("t-new".into(), None));
        }
        let turn_id = new_id("t");
        let directory = turn_dir(slug, name, &turn_id);
        if let Some(parent) = directory.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir(&directory)?;
        let mut meta = json!({
            // The session's running usage totals as this turn starts it, resumes it or
            // forks it. The turn's own usage is what the totals grew by.
            "usage_base": self.usage_base(slug, name, l.session)?,
            "id": turn_id,
            "project": slug,
            "agent": name,
            "kind": l.kind,
            "backend": l.backend,
            "model": l.model,
            "session_in": l.session,
            "fork": l.fork,
            "main_turn": l.main_turn,
            "messages": l.messages.iter().map(|m| m["id"].clone()).collect::<Vec<_>>(),
            "triggers": l.messages.iter().map(store::describe_trigger).collect::<Vec<_>>(),
            "created": now(),
            "state": "preparing",
        });
        if !l.notes.is_empty() {
            meta["notes"] = json!(l.notes); // Given back if the turn never reaches the agent.
        }
        write_json(&directory.join("meta.json"), &meta)?;
        touch(&active_dir(slug, name).join(&turn_id))?;
        for message in l.messages {
            store::claim(slug, name, as_str(message, "id"), &turn_id)?;
        }
        let images: Vec<String> = l
            .messages
            .iter()
            .flat_map(|m| m.get("attachments").and_then(Value::as_array).cloned().unwrap_or_default())
            .filter(|a| convert::text(a.get("mime").unwrap_or(&json!(""))).starts_with("image/"))
            .map(|a| project_dir(slug).join(convert::text(&a["path"])).to_string_lossy().into_owned())
            .collect();
        let spec = TurnSpec {
            backend: l.backend.into(),
            model: l.model.into(),
            system_prompt: l.system.into(),
            am_home: am_home().to_string_lossy().into_owned(),
            project_dir: project_dir(slug).to_string_lossy().into_owned(),
            session: l.session.map(String::from),
            fork: l.fork,
            images,
            persistent: l.persist.is_some(),
        };
        let (argv, new_session) = backends::build_command(&spec, &self.cfg)?;
        fs::write(directory.join("prompt.md"), l.prompt)?;
        if let Some(persist) = l.persist {
            return self.hand_to_process(slug, name, &directory, meta, persist, &argv, l.session, new_session);
        }
        write_json(
            &directory.join("cmd.json"),
            &json!({
                "argv": argv, "cwd": project_dir(slug).to_string_lossy(),
                "stop_grace_seconds": self.cfg["runner"]["stop_grace_seconds"],
                "wake": am_core::fsx::wake_path().to_string_lossy(),
            }),
        )?;
        let log_file = fs::OpenOptions::new().append(true).create(true).open(directory.join("supervisor.log"))?;
        let mut cmd = Command::new(procs::am_binary());
        cmd.arg("__turn")
            .arg(&directory)
            .stdin(Stdio::null())
            .stdout(log_file.try_clone()?)
            .stderr(log_file)
            .env_clear()
            .envs(super::turn_env(slug, name, &turn_id));
        crate::sys::new_session(&mut cmd);
        self.children.push(cmd.spawn()?);
        meta["state"] = json!("spawned");
        write_json(&directory.join("meta.json"), &meta)?;
        am_core::poke(slug);
        log::info(LOG, &format!("started {} turn {turn_id} for {slug}/{name} on {}", l.kind, l.backend));
        Ok((turn_id, new_session))
    }

    // ------------------------------------------------------------ long-lived processes

    /// Main turns on claude go to a long-lived process. Codex has no equivalent of
    /// `--input-format stream-json` (`codex app-server` is experimental).
    fn persistent(&self, backend: &str) -> bool {
        backend == "claude" && self.cfg["runner"].get("persistent").is_none_or(|v| am_core::format::truthy(Some(v)))
    }

    /// The agent's live process that can take this turn, if there is one. It must be on
    /// the same session, model, CLI settings and instructions, memory aside.
    #[allow(clippy::too_many_arguments)]
    fn find_process(
        &self,
        slug: &str,
        name: &str,
        agent: &Value,
        project: &Value,
        backend: &str,
        model: &str,
        session: Option<&str>,
        system: &str,
    ) -> Result<Persist> {
        let instructions = prompts::system_prompt(project, agent, false)?;
        if let Some(dir) = &self.debug.process {
            return Ok(Persist { proc: Some(dir.clone()), instructions: String::new() });
        }
        let Some(session) = session.filter(|s| !s.is_empty()) else {
            procs::shutdown_all(slug, name, "new session")?;
            return Ok(Persist { proc: None, instructions });
        };
        let spec = TurnSpec {
            backend: backend.into(),
            model: model.into(),
            system_prompt: system.into(),
            am_home: am_home().to_string_lossy().into_owned(),
            project_dir: project_dir(slug).to_string_lossy().into_owned(),
            session: Some(session.into()),
            persistent: true,
            ..Default::default()
        };
        let (argv, _) = backends::build_command(&spec, &self.cfg)?;
        let fp = procs::fingerprint(backend, model, Some(session), &argv, &instructions);
        Ok(Persist { proc: procs::find(slug, name, &fp)?, instructions })
    }

    #[allow(clippy::too_many_arguments)]
    fn hand_to_process(
        &mut self,
        slug: &str,
        name: &str,
        directory: &Path,
        mut meta: Value,
        persist: Persist,
        argv: &[String],
        session: Option<&str>,
        new_session: Option<String>,
    ) -> Result<(String, Option<String>)> {
        let turn_id = as_str(&meta, "id").to_string();
        let (proc, started) = match persist.proc.filter(|p| procs::alive(p) && !p.join("shutdown.json").exists()) {
            Some(proc) => {
                procs::enqueue(&proc, &turn_id, directory)?;
                (proc, "on")
            }
            None => {
                procs::shutdown_all(slug, name, "replaced")?;
                let session = session.map(String::from).or(new_session.clone());
                let fp = procs::fingerprint(as_str(&meta, "backend"), as_str(&meta, "model"), session.as_deref(), argv, &persist.instructions);
                let env = super::turn_env(slug, name, "");
                let cwd = project_dir(slug).to_string_lossy().into_owned();
                let (proc, child) = procs::start(slug, name, argv, &cwd, &env, &fp, session.as_deref(), &self.cfg, (&turn_id, directory))?;
                self.children.push(child); // Reaped when it exits, if this daemon is still up.
                (proc, "with a new")
            }
        };
        let proc_name = proc.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        meta["proc"] = json!(proc_name);
        // For finding and killing an orphaned agent, as for any turn.
        write_json(&directory.join("cmd.json"), &json!({"argv": argv, "cwd": project_dir(slug).to_string_lossy(), "proc": proc_name}))?;
        meta["state"] = json!("spawned");
        write_json(&directory.join("meta.json"), &meta)?;
        am_core::poke(slug);
        log::info(LOG, &format!("started main turn {turn_id} for {slug}/{name} {started} process {proc_name}"));
        Ok((turn_id, new_session))
    }

    /// At most `max_live_processes` live at once: the least recently used idle ones go
    /// first. A process with a turn is never stopped for this.
    pub(crate) fn enforce_process_cap(&self, agents: &[(String, String)]) -> Result<()> {
        let cap = self.cfg["runner"].get("max_live_processes").map_or(6, |v| convert::to_int(Some(v)));
        if cap <= 0 {
            return Ok(());
        }
        let live = procs::all_live(agents);
        let excess = live.len() as i64 - cap;
        if excess <= 0 {
            return Ok(());
        }
        let mut idle: Vec<(f64, PathBuf)> = live
            .into_iter()
            .map(|(_, _, d)| d)
            .filter(|d| procs::is_idle(d) && !d.join("shutdown.json").exists())
            .map(|d| (procs::last_activity(&d), d))
            .collect();
        idle.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for (_, directory) in idle.into_iter().take(excess as usize) {
            log::info(LOG, &format!("stopping idle process {}: over the cap of {cap}", directory.display()));
            procs::shutdown(&directory, "cap")?;
        }
        Ok(())
    }

    /// A round the agent ran with no turn open, as for a background task that ended after
    /// its turn was interrupted, becomes an activity note. Its cost is counted in the
    /// session's next turn, which carries on from its running total.
    pub(crate) fn record_strays(&mut self, slug: &str, name: &str) -> Result<()> {
        for (proc, stream) in procs::strays(slug, name) {
            let done = stream.with_extension("done");
            let parsed = backends::parse_stream("claude", &stream, None, "", None);
            let final_ = am_core::format::strip(&parsed.final_);
            let text = if final_.is_empty() { parsed.error.clone() } else { final_.to_string() };
            if !text.is_empty() {
                let proc_name = proc.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let stem = stream.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let note_id = format!("t-{}{stem}", proc_name.get(2..).unwrap_or(""));
                let meta = json!({"id": note_id, "kind": "background", "triggers": []});
                let state = if parsed.is_error { "failed" } else { "done" };
                self.note(slug, name, &meta, &[], &text, &json!({"ended": now(), "cost_usd": 0}), state)?;
            }
            let _ = fs::rename(&done, stream.with_extension("recorded"));
        }
        Ok(())
    }

    fn hold_for_missing_backend(&self, slug: &str, name: &str, backend: &str) -> Result<()> {
        let binary = convert::text(&self.cfg["backends"][backend]["bin"]);
        store::add_attention(
            slug,
            &format!("{backend} isn't installed"),
            &format!("Agent {name} uses the {backend} backend, but `{binary}` isn't on the daemon's PATH. Its messages are waiting."),
            "alert",
            "runner",
            &format!("missing-{backend}"),
            None,
        )?;
        store::update_agent(slug, name, |a| {
            a["not_before"] = convert::float_value(now() + HOLD_AFTER_AUTH_FAILURE);
        })?;
        Ok(())
    }

    /// A digest of recent turns, for a session that has none of its own.
    pub(crate) fn history(&self, slug: &str, name: &str) -> Result<String> {
        let limit = 6000;
        let mut turn_ids = am_core::fsx::listdir(&am_core::turns_dir(slug, name)).unwrap_or_default();
        turn_ids.sort();
        turn_ids.reverse();
        let mut entries: Vec<String> = Vec::new();
        let mut used = 0;
        for turn_id in turn_ids {
            let directory = turn_dir(slug, name, &turn_id);
            let meta = am_core::read_json(&directory.join("meta.json")).ok().flatten().unwrap_or(json!({}));
            let result = am_core::read_json(&directory.join("result.json")).ok().flatten().unwrap_or(json!({}));
            if as_str(&meta, "kind") != "main" || as_str(&result, "state") != "done" {
                continue;
            }
            let asked: Vec<String> = match meta.get("triggers") {
                Some(Value::Array(ts)) => ts
                    .iter()
                    .map(|t| {
                        let get = |k: &str| t.get(k).map_or_else(|| "None".to_string(), convert::text);
                        format!("{}: {}", get("from"), get("excerpt"))
                    })
                    .collect(),
                _ => vec![],
            };
            let reply = am_core::excerpt(&convert::or_str(result.get("final")), 800);
            let entry = prompts::render_kv("turn/history-entry", &[("turn_id", &turn_id), ("asked", &asked.join("; ")), ("reply", &reply)])?;
            let n = entry.chars().count();
            if used + n > limit {
                break;
            }
            entries.push(entry);
            used += n;
        }
        if entries.is_empty() {
            return Ok(String::new());
        }
        entries.reverse();
        prompts::render_kv("turn/history", &[("entries", &entries.join("\n\n"))])
    }
}

/// What `launch` takes.
pub(crate) struct Launch<'a> {
    pub kind: &'a str,
    pub messages: &'a [Value],
    pub backend: &'a str,
    pub model: &'a str,
    pub session: Option<&'a str>,
    pub fork: bool,
    pub system: &'a str,
    pub prompt: &'a str,
    pub main_turn: Option<&'a str>,
    pub persist: Option<Persist>,
    pub notes: &'a [String],
}

/// Create the file, or update its times. The parent must exist.
fn touch(path: &Path) -> Result<()> {
    fs::OpenOptions::new().append(true).create(true).open(path)?;
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap_or_default();
    unsafe { libc::utimes(c.as_ptr(), std::ptr::null()) };
    Ok(())
}
