//! Paths under `AM_HOME` (`core.py`, "paths").

use std::path::{Path, PathBuf};

pub const HOME_SLUG: &str = "home";
pub const MANAGER: &str = "manager";

/// `Path(os.environ.get("AM_HOME") or "~/agents").expanduser()`.
pub fn am_home() -> PathBuf {
    let raw = std::env::var("AM_HOME").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "~/agents".into());
    expand_user(&raw)
}

/// `Path.expanduser()` for `~` and `~/...` (`~user` is left alone, as Python fails on it).
pub fn expand_user(raw: &str) -> PathBuf {
    if raw == "~" || raw.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            let home = PathBuf::from(home);
            return if raw == "~" { home } else { home.join(&raw[2..]) };
        }
    }
    PathBuf::from(raw)
}

pub fn config_path() -> PathBuf {
    am_home().join("config.toml")
}
pub fn token_path() -> PathBuf {
    am_home().join(".web-token")
}
pub fn daemon_dir() -> PathBuf {
    am_home().join("daemon")
}
pub fn memory_dir() -> PathBuf {
    am_home().join("memory")
}
pub fn projects_dir() -> PathBuf {
    am_home().join("projects")
}
pub fn project_dir(slug: &str) -> PathBuf {
    projects_dir().join(slug)
}
pub fn agents_dir(slug: &str) -> PathBuf {
    project_dir(slug).join("agents")
}
pub fn agent_dir(slug: &str, name: &str) -> PathBuf {
    agents_dir(slug).join(name)
}
pub fn inbox_dir(slug: &str, name: &str) -> PathBuf {
    agent_dir(slug, name).join("inbox")
}
pub fn delivered_dir(slug: &str, name: &str) -> PathBuf {
    agent_dir(slug, name).join("delivered")
}
pub fn answers_dir(slug: &str, name: &str) -> PathBuf {
    agent_dir(slug, name).join("answers")
}
pub fn turns_dir(slug: &str, name: &str) -> PathBuf {
    agent_dir(slug, name).join("turns")
}
pub fn active_dir(slug: &str, name: &str) -> PathBuf {
    agent_dir(slug, name).join("active")
}
pub fn turn_dir(slug: &str, name: &str, turn_id: &str) -> PathBuf {
    turns_dir(slug, name).join(turn_id)
}
pub fn attention_dir(slug: &str) -> PathBuf {
    project_dir(slug).join("attention")
}

/// `core.current_turn`: `$AM_TURN`, or for a long-lived process the open turn in
/// `$AM_PROC/current.json`.
pub fn current_turn() -> Option<String> {
    if let Some(turn) = std::env::var("AM_TURN").ok().filter(|s| !s.is_empty()) {
        return Some(turn);
    }
    let proc = std::env::var("AM_PROC").ok().filter(|s| !s.is_empty())?;
    let current = crate::read_json(&Path::new(&proc).join("current.json")).ok().flatten()?;
    match current.get("turn") {
        Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}
