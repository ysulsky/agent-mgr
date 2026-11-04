//! `am setup`: create ~/agents. Everything here is safe to re-run.

use crate::{artifacts, memory, projects, util};
use am_core::{am_home, atomic_write, config, config_path, daemon_dir, memory_dir, projects_dir, Result, HOME_SLUG};
use std::path::{Path, PathBuf};

pub const HOME_DESCRIPTION: &str = "The home manager: the first point of contact, overseeing every project.";

/// The binary agents should run: this one.
/// This binary, with symlinks resolved, so the shim names this exact install even when
/// it was run as `~/.local/bin/am`, a link that may later be repointed.
pub fn this_binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("am"));
    exe.canonicalize().unwrap_or(exe)
}

/// Write `$AM_HOME/bin/am`, put first on a turn's PATH, so agents find this exact
/// install whatever the daemon's own PATH holds. It execs this binary.
pub fn write_shim() -> Result<()> {
    let shim = am_home().join("bin").join("am");
    let text = format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", this_binary().display());
    atomic_write(&shim, text.as_bytes(), 0o755)?;
    Ok(())
}

/// Set up `AM_HOME`: returns what was created, and any warnings.
pub fn run() -> Result<(Vec<String>, Vec<String>)> {
    let mut created = Vec::new();
    let home = am_home();
    if !util::exists(&home) {
        created.push(home.display().to_string());
    }
    for directory in [home.clone(), daemon_dir(), memory_dir(), projects_dir()] {
        std::fs::create_dir_all(directory)?;
    }
    if !util::exists(&config_path()) {
        atomic_write(&config_path(), config::TEMPLATE.as_bytes(), 0o644)?;
        created.push(config_path().display().to_string());
    }
    if !util::exists(&home.join(".web-token")) {
        am_core::token::web_token(true)?;
        created.push("web access token".into());
    }
    write_shim()?;
    if !util::exists(&home.join(".memory.git").join("HEAD")) {
        memory::store().init()?;
        created.push("memory history".into());
    }
    if !util::exists(&projects_dir().join(".artifacts.git").join("HEAD")) {
        artifacts::store().init()?;
        created.push("artifact history".into());
    }
    if !projects::exists(HOME_SLUG) {
        projects::create_project("Home", HOME_DESCRIPTION, Some(HOME_SLUG))?;
        created.push("home project".into());
    }
    Ok((created, warnings()?))
}

/// Find a runnable program: a path with a slash is checked as it is; otherwise each
/// directory of $PATH in turn.
pub fn which(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let runnable = |p: &Path| p.is_file() && p.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false);
    if name.contains('/') {
        return runnable(Path::new(name)).then(|| PathBuf::from(name));
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let dir = if dir.as_os_str().is_empty() { PathBuf::from(".") } else { dir };
        let candidate = dir.join(name);
        if runnable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn warnings() -> Result<Vec<String>> {
    let cfg = config::load()?;
    let mut out = Vec::new();
    let bin = |b: &str| cfg["backends"][b]["bin"].as_str().unwrap_or(b).to_string();
    let usable: Vec<&str> = ["claude", "codex"].into_iter().filter(|b| which(&bin(b)).is_some()).collect();
    if usable.is_empty() {
        out.push("Neither claude nor codex is on your PATH, so no agent can run.".to_string());
    }
    for backend in ["claude", "codex"] {
        let default = |k: &str| cfg["defaults"][k].as_str() == Some(backend);
        if !usable.contains(&backend) && (default("manager_backend") || default("worker_backend")) {
            out.push(format!("{backend} is a default backend in config.toml but isn't on your PATH."));
        }
    }
    if which("git").is_none() {
        out.push("git is not on your PATH, so artifact and memory history won't work.".into());
    }
    if which("tmux").is_none() {
        out.push("tmux is not on your PATH, so terminals in the UI won't work. On a Mac: brew install tmux".into());
    }
    if am_core::format::truthy(cfg["storage"].get("rclone_remote")) && which("rclone").is_none() {
        out.push("A storage remote is configured but rclone isn't installed. On a Mac: brew install rclone".into());
    }
    Ok(out)
}

pub fn is_set_up() -> bool {
    util::exists(&config_path()) && projects::exists(HOME_SLUG)
}
