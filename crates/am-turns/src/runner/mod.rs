//! Runs agents, turning messages into turns and turns into
//! messages.
//!
//! The runner keeps no state that matters in memory. A turn in progress is an
//! `active/<turn id>` marker plus its turn directory, so a new daemon adopts running
//! turns just by reading the disk. Every effect of finishing a turn can be repeated
//! safely, since derived messages have ids computed from the turn's id.
//!
//! The port is split by the Python file's sections: `start` (starting turns and
//! long-lived processes), `watch` (watching turns) and `finish` (finishing turns, and
//! when they go wrong).

mod finish;
mod start;
mod watch;

use crate::{log, procs, pyv, store};
use am_core::{agent_dir, am_home, now, write_json, Result, Value};
use regex::Regex;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Child;
use std::sync::OnceLock;

pub(crate) const LOG: &str = "am.runner";

pub const MAX_SIDE_TURNS: usize = 4;
pub const SPAWN_WINDOW_SECONDS: f64 = 20.0;
pub const HOLD_AFTER_AUTH_FAILURE: f64 = 300.0;
pub const HOLD_AFTER_LIMIT: f64 = 600.0;
pub const USAGE_TOTALS_KEPT: usize = 8;

/// Identity of whatever Claude Code session the daemon happened to be started from. A
/// turn is its own session, not a child of that one.
pub const PARENT_SESSION_ENV: [&str; 10] = [
    "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_EXECPATH", "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_SESSION_ATTENDED", "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN", "CLAUDE_PID", "CLAUDE_EFFORT",
];

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).unwrap())
}

fn auth_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    re(&RE, r"(?i)not logged in|please run /login|invalid api key|authentication|unauthorized|\b401\b|login required|token (has )?expired")
}

fn limit_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    re(&RE, r"(?i)rate.?limit|usage limit|limit reached|hit your .*limit|too many requests|\b429\b|overloaded|quota")
}

fn session_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    re(&RE, r"(?i)no conversation found|session .*not found|no rollout|thread .*not found|could not find (session|thread)")
}

/// `Path.home()`: `$HOME`, else the password database.
fn home_dir() -> String {
    if let Some(h) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        return h.to_string_lossy().into_owned();
    }
    am_core::expand_user("~").to_string_lossy().into_owned()
}

/// GIT_CEILING_DIRECTORIES for a turn: git looking for a repository stops before it
/// climbs into the home directory or the folder that holds AM_HOME. A stray repository
/// there (one with a whole home directory of untracked files) would otherwise make every
/// git command in a turn, and Claude Code's own status check, scan all of it.
/// Repositories below these folders are found as usual. The user's own ceilings are kept.
pub fn git_ceiling(existing: &str) -> String {
    let home = am_home();
    let parent = match home.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_string_lossy().into_owned(),
        _ => ".".into(),
    };
    let mut entries = vec![home_dir(), parent];
    if !existing.is_empty() {
        entries.extend(existing.split(':').map(String::from));
    }
    let mut out: Vec<String> = Vec::new();
    for entry in entries {
        if !out.contains(&entry) || entry.is_empty() {
            out.push(entry);
        }
    }
    out.join(":")
}

/// The environment a turn's agent runs in.
pub fn turn_env(slug: &str, name: &str, turn_id: &str) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        .filter(|(k, _)| !PARENT_SESSION_ENV.contains(&k.as_str()))
        .collect();
    let home = am_home().to_string_lossy().into_owned();
    let bin_dir = am_home().join("bin").to_string_lossy().into_owned();
    let path = format!("{bin_dir}:{}", env.get("PATH").map_or("", String::as_str));
    let ceiling = git_ceiling(env.get("GIT_CEILING_DIRECTORIES").map_or("", String::as_str));
    env.insert("AM_HOME".into(), home);
    env.insert("AM_PROJECT".into(), slug.into());
    env.insert("AM_AGENT".into(), name.into());
    env.insert("AM_TURN".into(), turn_id.into());
    env.insert("PATH".into(), path);
    env.insert("GIT_CEILING_DIRECTORIES".into(), ceiling);
    env
}

pub fn classify_failure(text: &str) -> &'static str {
    if session_re().is_match(text) {
        "session"
    } else if auth_re().is_match(text) {
        "auth"
    } else if limit_re().is_match(text) {
        "limit"
    } else {
        "other"
    }
}

/// Ask the runner to stop an agent's main turn, from any process.
pub fn request_stop(slug: &str, name: &str) -> Result<()> {
    write_json(&agent_dir(slug, name).join("stop.json"), &json!({"at": now()}))?;
    am_core::poke(slug);
    Ok(())
}

/// Remember a session's running usage totals, the base for its next turn's usage.
pub fn record_usage_totals(agent: &mut Value, session: Option<&str>, running: &serde_json::Map<String, Value>) {
    let Some(session) = session.filter(|s| !s.is_empty()) else { return };
    if running.is_empty() {
        return;
    }
    let Some(agent) = agent.as_object_mut() else { return };
    let totals = agent.entry("usage_totals").or_insert_with(|| json!({}));
    let Some(totals) = totals.as_object_mut() else { return };
    totals.shift_remove(session);
    totals.insert(session.to_string(), Value::Object(running.clone()));
    while totals.len() > USAGE_TOTALS_KEPT {
        let Some(oldest) = totals.keys().next().cloned() else { break };
        totals.shift_remove(&oldest);
    }
}

/// Test hooks for `am __debug runner-call start-main`: launch nothing, and record the
/// prompt; or hand the turn to this process dir.
#[derive(Default)]
#[doc(hidden)]
pub struct DebugHooks {
    pub dry_launch: bool,
    pub launched_prompt: Option<String>,
    pub process: Option<PathBuf>,
}

/// `runner.Runner`.
pub struct Runner {
    cfg: Value,
    children: Vec<Child>,
    config_mtime: Option<std::time::SystemTime>,
    cap_checked: f64,
    #[doc(hidden)]
    pub debug: DebugHooks,
}

fn as_str<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

impl Runner {
    pub fn new(cfg: Option<Value>) -> Result<Runner> {
        let cfg = match cfg {
            Some(cfg) => cfg,
            None => am_core::config::load()?,
        };
        Ok(Runner { cfg, children: Vec::new(), config_mtime: None, cap_checked: 0.0, debug: DebugHooks::default() })
    }

    pub fn cfg(&self) -> &Value {
        &self.cfg
    }

    // ------------------------------------------------------------ the loop

    pub fn tick(&mut self) -> Result<()> {
        self.reload_config()?;
        self.children.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
        let mut agents: Vec<(String, String)> = Vec::new();
        for slug in store::list_slugs()? {
            for name in store::agent_names(&slug).unwrap_or_default() {
                agents.push((slug.clone(), name));
            }
        }
        for (slug, name) in &agents {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.step(slug, name)));
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(e)) => log::exception(LOG, &format!("step failed for {slug}/{name}"), &e),
                Err(_) => log::exception(LOG, &format!("step failed for {slug}/{name}"), &"panic"),
            }
        }
        if now() - self.cap_checked > 2.0 {
            self.cap_checked = now();
            if let Err(e) = self.enforce_process_cap(&agents) {
                log::exception(LOG, "process cap check failed", &e);
            }
        }
        Ok(())
    }

    pub fn reload_config(&mut self) -> Result<()> {
        let Ok(meta) = std::fs::metadata(am_home().join("config.toml")) else { return Ok(()) };
        let mtime = meta.modified().ok();
        if mtime != self.config_mtime {
            match am_core::config::load() {
                Ok(cfg) => {
                    self.cfg = cfg;
                    self.config_mtime = mtime;
                }
                Err(am_core::Error::Am(_)) | Err(am_core::Error::Io(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn step(&mut self, slug: &str, name: &str) -> Result<()> {
        let mut running = self.reap(slug, name)?;
        let agent = store::get_agent(slug, name)?;
        let main = running.iter().position(|t| as_str(t, "kind") == "main");
        let stop_file = agent_dir(slug, name).join("stop.json");
        if stop_file.exists() {
            if let Some(i) = main {
                self.stop(slug, name, &mut running[i])?;
            }
            let _ = std::fs::remove_file(&stop_file);
        }
        self.enforce_time_limit(slug, name, &agent, &mut running)?;
        self.record_strays(slug, name)?;
        if am_core::py::truthy(agent.get("retired")) {
            if let Some(i) = main {
                self.stop(slug, name, &mut running[i])?;
            }
            procs::shutdown_all(slug, name, "retired")?;
            return Ok(());
        }
        if store::is_archived(slug)? {
            // A turn already running finishes normally, and is reaped above. Nothing new
            // starts, not even a side turn, so messages wait in the inbox for un-archiving.
            if let Some(i) = main {
                self.learn_session(slug, name, &agent, &running[i])?;
            }
            procs::shutdown_all(slug, name, "archived")?;
            return Ok(());
        }
        let pending = store::pending(slug, name)?;
        if let Some(i) = main {
            self.learn_session(slug, name, &agent, &running[i])?;
            let role = as_str(&agent, "role");
            if pending.iter().any(|m| am_core::py::truthy(m.get("interrupt"))) {
                self.stop(slug, name, &mut running[i])?;
            } else if role == "manager" {
                self.auto_interrupt(slug, name, &mut running[i], &pending)?;
            } else if role == "worker" {
                let main = running[i].clone();
                self.start_side_turns(slug, name, &agent, &main, &running, &pending)?;
            }
            return Ok(());
        }
        if !pending.iter().any(|m| m.get("wake").is_none_or(|w| am_core::py::truthy(Some(w)))) {
            return Ok(());
        }
        let interrupting = pending.iter().any(|m| am_core::py::truthy(m.get("interrupt")));
        let not_before = agent.get("not_before").map_or(0.0, pyv::py_float);
        if not_before > now() && !interrupting {
            return Ok(());
        }
        self.start_main(slug, name, &agent, &pending)
    }

    /// A duration from [runner], or None when it's "off" (or "0", unless zero is allowed).
    fn seconds(&self, key: &str, allow_zero: bool) -> Option<f64> {
        let raw = self.cfg["runner"].get(key).map_or_else(|| "off".to_string(), pyv::str_of);
        let text = am_core::py::strip(&raw);
        if text == "off" || text.is_empty() {
            return None;
        }
        match am_core::parse_duration(text) {
            Ok(seconds) if seconds > 0.0 || allow_zero => Some(seconds),
            Ok(_) => None,
            Err(_) => {
                log::warning(LOG, &format!("ignoring runner.{key} = {}", am_core::py::repr(text)));
                None
            }
        }
    }

    fn usage_base(&self, slug: &str, name: &str, session: Option<&str>) -> Result<Value> {
        let Some(session) = session.filter(|s| !s.is_empty()) else { return Ok(Value::Null) };
        let agent = store::get_agent(slug, name)?;
        Ok(agent.get("usage_totals").and_then(|t| t.get(session)).cloned().unwrap_or(Value::Null))
    }
}

/// Entry points for `am __debug runner-call`, which drives single steps of a Runner on
/// made-up turns, as Python's prompt goldens call its methods.
#[doc(hidden)]
impl Runner {
    pub fn debug_start_main(&mut self, slug: &str, name: &str, agent: &Value, pending: &[Value]) -> Result<()> {
        self.start_main(slug, name, agent, pending)
    }

    pub fn debug_vanished(&mut self, slug: &str, name: &str, meta: &Value) -> Result<()> {
        self.vanished(slug, name, meta)
    }

    pub fn debug_step(&mut self, method: &str, slug: &str, name: &str, meta: &Value, messages: &[Value], result: &mut Value) -> Result<()> {
        match method {
            "after-interrupt" => self.after_interrupt(slug, name, meta, messages, result),
            "after-failure" => self.after_failure(slug, name, meta, messages, result),
            "route-side" => self.route_side(slug, name, meta, messages, result),
            _ => self.route_main(slug, name, meta, messages, result),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_failure_as_python() {
        assert_eq!(classify_failure("No conversation found with session ID: x"), "session");
        assert_eq!(classify_failure("no rollout found for thread id y"), "session");
        assert_eq!(classify_failure("Not logged in · Please run /login"), "auth");
        assert_eq!(classify_failure("HTTP 401 Unauthorized"), "auth");
        assert_eq!(classify_failure("You've hit your usage limit"), "limit");
        assert_eq!(classify_failure("API Error: 429 too many requests"), "limit");
        assert_eq!(classify_failure("boom"), "other");
    }

    #[test]
    fn usage_totals_keep_the_last_eight_sessions_in_order() {
        let mut agent = json!({"usage_totals": {}});
        let running: serde_json::Map<String, Value> = json!({"cost_usd": 0.5}).as_object().unwrap().clone();
        for i in 0..10 {
            record_usage_totals(&mut agent, Some(&format!("s{i}")), &running);
        }
        record_usage_totals(&mut agent, Some("s5"), &running);
        let keys: Vec<&String> = agent["usage_totals"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["s2", "s3", "s4", "s6", "s7", "s8", "s9", "s5"]);
    }
}
