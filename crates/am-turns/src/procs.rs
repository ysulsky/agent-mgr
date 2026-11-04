//! The runner's side of long-lived agent processes (see `proc`).
//!
//! An agent has at most one live process, under `agents/<name>/procs/<proc id>/`. A
//! main turn on the claude backend reuses it when its fingerprint still matches: the
//! same backend, model, session, CLI settings and instructions. The shared memory index
//! is part of the instructions, but leaving it out of the fingerprint keeps a process
//! alive across memory edits; each turn is told what changed instead.

use crate::{convert, sys};
use am_core::{agent_dir, fsx::listdir, new_id, now, read_json, write_json, Result, Value};
use am_store::memory;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Finished process dirs kept per agent, for looking into what happened.
pub const KEPT: usize = 4;
const SESSION_FLAGS: [&str; 2] = ["--resume", "--session-id"];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A supervisor runs the code it started with, so a new version replaces it. The version
/// is a hash of `proc.rs` and part of the fingerprint, so a change to the supervisor
/// replaces idle processes at their next turn.
pub fn supervisor_version() -> String {
    hex(&Sha256::digest(include_bytes!("proc.rs")))[..12].to_string()
}

pub fn procs_dir(slug: &str, name: &str) -> PathBuf {
    agent_dir(slug, name).join("procs")
}

fn read(path: &Path) -> Value {
    read_json(path).ok().flatten().filter(|v| am_core::format::truthy(Some(v))).unwrap_or_else(|| json!({}))
}

/// The supervisor holds this lock for as long as it lives.
pub fn alive(directory: &Path) -> bool {
    if !directory.join("pid.json").exists() && !directory.join("exit.json").exists() {
        // Still starting: the supervisor takes the lock before writing pid.json.
        let meta = read(&directory.join("cmd.json"));
        return now() - convert::to_float_or0(meta.get("created")) < 20.0;
    }
    sys::try_lock(&directory.join("alive.lock")).is_none()
}

fn sorted_names(dir: &Path) -> Vec<String> {
    let mut names = listdir(dir).unwrap_or_default();
    names.sort();
    names
}

pub fn live(slug: &str, name: &str) -> Vec<PathBuf> {
    let base = procs_dir(slug, name);
    let mut out = Vec::new();
    for proc_id in sorted_names(&base) {
        let directory = base.join(&proc_id);
        if proc_id.starts_with('.') || directory.join("exit.json").exists() {
            continue;
        }
        if alive(&directory) {
            out.push(directory);
        }
    }
    out
}

pub fn is_idle(directory: &Path) -> bool {
    if directory.join("current.json").exists() {
        return false;
    }
    match fs::read_dir(directory.join("queue")) {
        Ok(_) => !listdir(&directory.join("queue")).unwrap_or_default().iter().any(|n| n.ends_with(".json")),
        Err(_) => true,
    }
}

pub fn last_activity(directory: &Path) -> f64 {
    let state = read(&directory.join("state.json"));
    if am_core::format::truthy(state.get("last_activity")) {
        return convert::to_float(&state["last_activity"]);
    }
    convert::to_float_or0(read(&directory.join("cmd.json")).get("created"))
}

/// What a live process must match to take a turn. `argv` minus its session flags and
/// its system prompt; `instructions` is the system prompt without the memory index.
pub fn fingerprint(backend: &str, model: &str, session: Option<&str>, argv: &[String], instructions: &str) -> String {
    let mut rest: Vec<&str> = Vec::new();
    let mut skip = false;
    for arg in argv {
        if skip {
            skip = false;
            continue;
        }
        if SESSION_FLAGS.contains(&arg.as_str()) || arg == "--append-system-prompt" {
            skip = true;
            continue;
        }
        rest.push(arg);
    }
    let data = sys::encode_ascii(&json!([supervisor_version(), backend, model, session, rest, instructions]));
    hex(&Sha256::digest(data.as_bytes()))[..16].to_string()
}

/// The agent's live, idle process with this fingerprint. Any other idle process of the
/// agent is asked to shut down.
pub fn find(slug: &str, name: &str, fp: &str) -> Result<Option<PathBuf>> {
    let mut found = None;
    for directory in live(slug, name) {
        let spec = read(&directory.join("cmd.json"));
        if directory.join("shutdown.json").exists() || am_core::format::truthy(read(&directory.join("state.json")).get("closing")) {
            continue;
        }
        if spec.get("fingerprint").and_then(Value::as_str) == Some(fp) && found.is_none() && is_idle(&directory) {
            found = Some(directory);
        } else if is_idle(&directory) {
            shutdown(&directory, "changed")?;
        }
    }
    Ok(found)
}

pub fn shutdown(directory: &Path, reason: &str) -> Result<()> {
    if !directory.join("shutdown.json").exists() {
        write_json(&directory.join("shutdown.json"), &json!({"reason": reason, "at": now()}))?;
    }
    Ok(())
}

pub fn shutdown_all(slug: &str, name: &str, reason: &str) -> Result<()> {
    for directory in live(slug, name) {
        if is_idle(&directory) {
            shutdown(&directory, reason)?;
        }
    }
    Ok(())
}

/// The binary to run supervisors with: this one, with symlinks resolved, so a supervisor
/// is this exact install even if `~/.local/bin/am` is repointed to another one.
pub fn am_binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("am"));
    exe.canonicalize().unwrap_or(exe)
}

/// Start a supervisor with its first turn already queued, so that an agent that dies at
/// once (a session it can't resume, say) fails that turn visibly.
#[allow(clippy::too_many_arguments)]
pub fn start(
    slug: &str,
    name: &str,
    argv: &[String],
    cwd: &str,
    env: &BTreeMap<String, String>,
    fp: &str,
    session: Option<&str>,
    cfg: &Value,
    first_turn: (&str, &Path),
) -> Result<(PathBuf, Child)> {
    let proc_id = new_id("p");
    let directory = procs_dir(slug, name).join(&proc_id);
    fs::create_dir_all(directory.join("queue"))?;
    let runner_cfg = &cfg["runner"];
    let idle = if am_core::format::truthy(runner_cfg.get("process_idle")) { convert::text(&runner_cfg["process_idle"]) } else { "55m".into() };
    let idle_seconds = if idle != "0" && idle != "off" { convert::float_value(am_core::parse_duration(&idle)?) } else { json!(0) };
    write_json(
        &directory.join("cmd.json"),
        &json!({
            "argv": argv, "cwd": cwd, "fingerprint": fp, "session": session, "created": now(),
            "idle_seconds": idle_seconds,
            "stop_grace_seconds": runner_cfg["stop_grace_seconds"], "wake": am_core::fsx::wake_path().to_string_lossy(),
        }),
    )?;
    write_json(&directory.join("memory.json"), &memory::snapshot()?)?;
    enqueue(&directory, first_turn.0, first_turn.1)?;
    let mut env = env.clone();
    env.insert("AM_PROC".into(), directory.to_string_lossy().into_owned());
    let log = fs::OpenOptions::new().append(true).create(true).open(directory.join("supervisor.log"))?;
    let mut cmd = Command::new(am_binary());
    cmd.arg("__proc").arg(&directory).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log).env_clear().envs(&env);
    sys::new_session(&mut cmd);
    let child = cmd.spawn()?;
    prune(slug, name);
    Ok((directory, child))
}

pub fn enqueue(directory: &Path, turn_id: &str, turn_dir: &Path) -> Result<()> {
    write_json(
        &directory.join("queue").join(format!("{turn_id}.json")),
        &json!({"turn": turn_id, "turn_dir": turn_dir.to_string_lossy(), "at": now()}),
    )
}

/// What changed in shared memory since this process was last told, as one line, or ""
/// if nothing did. Telling it is recorded, so each change is told once.
pub fn memory_changes(directory: &Path) -> Result<String> {
    let told = read_json(&directory.join("memory.json"))?.filter(|t| !t.is_null());
    let current = memory::snapshot()?;
    let told = match told {
        None => {
            write_json(&directory.join("memory.json"), &current)?;
            return Ok(String::new());
        }
        Some(t) if t == current => return Ok(String::new()),
        Some(t) => t,
    };
    let empty = serde_json::Map::new();
    let told_map = told.as_object().unwrap_or(&empty);
    let current_map = current.as_object().unwrap_or(&empty);
    let mut added: Vec<&String> = current_map.keys().filter(|n| !told_map.contains_key(*n)).collect();
    let mut edited: Vec<&String> = current_map.iter().filter(|(n, v)| told_map.get(*n).is_some_and(|t| t != *v)).map(|(n, _)| n).collect();
    let mut removed: Vec<&String> = told_map.keys().filter(|n| !current_map.contains_key(*n)).collect();
    added.sort();
    edited.sort();
    removed.sort();
    write_json(&directory.join("memory.json"), &current)?;
    let mut summaries = BTreeMap::new();
    for e in memory::list_entries()? {
        let summary: String = convert::text(&e["summary"]).chars().take(120).collect();
        summaries.insert(convert::text(&e["name"]), summary);
    }
    let mut parts = Vec::new();
    if !added.is_empty() {
        let list: Vec<String> = added.iter().map(|n| format!("`{n}` ({})", summaries.get(*n).map_or("", String::as_str))).collect();
        parts.push(format!("added {}", list.join(", ")));
    }
    if !edited.is_empty() {
        parts.push(format!("edited {}", edited.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")));
    }
    if !removed.is_empty() {
        parts.push(format!("removed {}", removed.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")));
    }
    Ok(parts.join("; "))
}

pub fn prune(slug: &str, name: &str) {
    let base = procs_dir(slug, name);
    let finished: Vec<String> = sorted_names(&base).into_iter().filter(|n| base.join(n).join("exit.json").exists()).collect();
    if finished.len() > KEPT {
        for proc_id in &finished[..finished.len() - KEPT] {
            let _ = fs::remove_dir_all(base.join(proc_id));
        }
    }
}

/// Finished rounds the agent ran with no turn open, not yet recorded: (proc dir, stream).
pub fn strays(slug: &str, name: &str) -> Vec<(PathBuf, PathBuf)> {
    let base = procs_dir(slug, name);
    let mut out = Vec::new();
    for proc_id in sorted_names(&base) {
        let directory = base.join(&proc_id).join("strays");
        if !directory.is_dir() {
            continue;
        }
        for filename in sorted_names(&directory) {
            if filename.ends_with(".done") {
                out.push((base.join(&proc_id), directory.join(filename.replace(".done", ".jsonl"))));
            }
        }
    }
    out
}

pub fn all_live(slugs_and_names: &[(String, String)]) -> Vec<(String, String, PathBuf)> {
    let mut out = Vec::new();
    for (slug, name) in slugs_and_names {
        for d in live(slug, name) {
            out.push((slug.clone(), name.clone(), d));
        }
    }
    out
}
