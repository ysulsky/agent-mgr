//! `am storage offload`: move old results to shared storage through rclone.
//!
//! Each offload is recorded in storage/manifest.json and leaves `<path>.offloaded`
//! behind, so agents can find the data and bring it back with `am storage restore`.
//!
//! `am project offload` backs up a whole archived project instead: everything under
//! projects/<slug>/ plus its agents' CLI session files, as one tar.gz streamed to the
//! remote and checked there. Only project.json stays, marked offloaded, until `am project
//! restore` brings it all back. The project jobs are in `project_jobs.rs`.

use am_core::{am_err, am_home, project_dir, read_json, update_json, write_json, Result, Value};
use am_store::util::{self, get, has, s};
use am_store::projects;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use crate::project_jobs::{begin_project_job, json_bytes, offload_project, restore_project, run_project_job};

pub const STUB_SUFFIX: &str = ".offloaded";

/// What Settings can connect: (provider, remote, rclone arguments). drive.file lets rclone
/// see only the files it made, which is all that offloading needs.
pub const PROVIDERS: [(&str, &str, &[&str]); 1] =
    [("google-drive", "gdrive", &["config", "create", "gdrive", "drive", "scope=drive.file"])];

/// The rclone remote a provider's Connect creates.
pub fn provider_remote(provider: &str) -> Option<&'static str> {
    PROVIDERS.iter().find(|(name, _, _)| *name == provider).map(|(_, remote, _)| *remote)
}

fn manifest() -> PathBuf {
    am_home().join("storage").join("manifest.json")
}

fn load(cfg: Option<&Value>) -> Result<Value> {
    match cfg.filter(|c| am_core::format::truthy(Some(c))) {
        Some(c) => Ok(c.clone()),
        None => am_core::config::load(),
    }
}

/// A `[storage]` setting as text, or empty if it's missing or falsy.
fn storage_str(cfg: &Value, key: &str) -> String {
    let section = cfg.get("storage").cloned().unwrap_or(Value::Null);
    if has(&section, key) { s(&section, key) } else { String::new() }
}

/// `<remote>:<folder>`, or why there's none.
pub fn remote_root(cfg: Option<&Value>) -> Result<String> {
    let cfg = load(cfg)?;
    let remote = storage_str(&cfg, "rclone_remote");
    if remote.is_empty() {
        return am_err(format!(
            "no storage remote is configured, so nothing was offloaded. To use Google Drive, ask the user to \
             click Connect Google Drive under Storage in the home project's Settings. For another rclone \
             remote, set [storage] rclone_remote in {}.",
            am_home().join("config.toml").display()
        ));
    }
    Ok(format!("{}:{}", remote.trim_end_matches(':'), folder(&cfg)))
}

fn folder(cfg: &Value) -> String {
    let f = storage_str(cfg, "folder");
    let f = if f.is_empty() { "agent-mgr".to_string() } else { f };
    f.trim_matches('/').to_string()
}

/// What an rclone run returned: its exit status and its output as text.
pub struct Ran {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run rclone, capturing its output with line ends as `\n`.
pub fn rclone(args: &[&str]) -> Result<Ran> {
    if am_store::setup::which("rclone").is_none() {
        return am_err("rclone is not installed; on a Mac run `brew install rclone`");
    }
    let out = Command::new("rclone").args(args).stdin(std::process::Stdio::inherit()).output()?;
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace("\r\n", "\n").replace('\r', "\n");
    Ok(Ran { code: out.status.code().unwrap_or(-1), stdout: text(&out.stdout), stderr: text(&out.stderr) })
}

/// Where a path lives in the remote, relative to AM_HOME.
fn relative(path: &Path) -> Result<String> {
    let home = projects::resolve(&am_home());
    let resolved = projects::resolve(path);
    match resolved.strip_prefix(&home) {
        Ok(rel) => Ok(rel.to_string_lossy().into_owned()),
        Err(_) => am_err(format!("{} is outside {}; only data under AM_HOME can be offloaded", path.display(), home.display())),
    }
}

/// rclone's stderr without surrounding whitespace.
fn strip(text: &str) -> &str {
    am_core::format::strip(text)
}

/// Copy a path to the remote and check the copy, record it in the manifest, and unless
/// `keep_local`, replace it with a `.offloaded` stub.
pub fn offload(path_text: &str, note: &str, keep_local: bool) -> Result<Value> {
    let path = projects::resolve(&am_core::expand_user(path_text));
    if !util::exists(&path) {
        return am_err(format!("nothing to offload at {path_text}"));
    }
    let relative = relative(&path)?;
    let protected = ["config.toml", ".web-token", "daemon", "memory", ".memory.git", "projects", "storage"];
    let parts: Vec<&str> = relative.split('/').collect();
    if protected.contains(&relative.as_str()) || protected[..5].contains(&parts[0]) || (parts[0] == "projects" && parts.len() <= 2) {
        return am_err(format!("refusing to offload {relative}: choose data inside a project, such as its results/"));
    }
    if parts.len() >= 3 && parts[0] == "projects" && ["agents", "attention", "project.json", "artifacts"].contains(&parts[2]) {
        return am_err(format!("refusing to offload {relative}: {} holds project state", parts[2]));
    }
    let root = remote_root(None)?;
    let target = format!("{root}/{relative}");
    let size = size_of(&path);
    let local = path.to_string_lossy().into_owned();
    let is_dir = path.is_dir();
    let (copy, check): (Vec<String>, Vec<String>) = if is_dir {
        (vec!["copy".into(), local.clone(), target.clone()], vec!["check".into(), local.clone(), target.clone(), "--one-way".into()])
    } else {
        let parent = target.rsplit_once('/').map_or(target.as_str(), |p| p.0).to_string();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let dir = path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        (
            vec!["copyto".into(), local.clone(), target.clone()],
            vec!["check".into(), dir, parent, "--one-way".into(), "--include".into(), format!("/{name}")],
        )
    };
    fn as_strs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }
    let ran = rclone(&as_strs(&copy))?;
    if ran.code != 0 {
        return am_err(format!("rclone copy to {target} failed, local data untouched:\n{}", strip(&ran.stderr)));
    }
    let ran = rclone(&as_strs(&check))?;
    if ran.code != 0 {
        return am_err(format!("rclone could not verify {target}, local data untouched:\n{}", strip(&ran.stderr)));
    }
    let entry = json!({
        "path": relative,
        "remote": target,
        "is_dir": is_dir,
        "bytes": size,
        "note": note,
        "offloaded_at": util::now_value(),
        "local_removed": !keep_local,
    });
    update_json(&manifest(), util::empty(), |m| {
        util::fields(m).insert(relative.clone(), entry.clone());
        None
    })?;
    if !keep_local {
        if is_dir {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
        write_json(Path::new(&format!("{local}{STUB_SUFFIX}")), &entry)?;
    }
    Ok(entry)
}

/// Bring an offloaded path back from the remote, remove its stub, and mark it restored in
/// the manifest.
pub fn restore(path_text: &str) -> Result<Value> {
    let raw = path_text.strip_suffix(STUB_SUFFIX).unwrap_or(path_text);
    let path = projects::resolve(&am_core::expand_user(raw));
    let relative = relative(&path)?;
    let manifest_data = read_json(&manifest())?.unwrap_or_else(util::empty);
    let Some(entry) = get(&manifest_data, &relative).cloned() else {
        return am_err(format!("{relative} is not in the offload manifest; see `am storage list`"));
    };
    let op = if has(&entry, "is_dir") { "copy" } else { "copyto" };
    let remote = s(&entry, "remote");
    let local = path.to_string_lossy().into_owned();
    let ran = rclone(&[op, &remote, &local])?;
    if ran.code != 0 {
        return am_err(format!("rclone restore from {remote} failed:\n{}", strip(&ran.stderr)));
    }
    let stub = PathBuf::from(format!("{local}{STUB_SUFFIX}"));
    if util::exists(&stub) {
        std::fs::remove_file(&stub)?;
    }
    update_json(&manifest(), util::empty(), |m| {
        if let Some(e) = util::fields(m).get_mut(&relative) {
            let f = util::fields(e);
            f.insert("local_removed".into(), false.into());
            f.insert("restored_at".into(), util::now_value());
        }
        None
    })?;
    Ok(entry)
}

/// `s` quoted for a POSIX shell: as it is if it's only ASCII letters, digits and
/// `@%+=:,./-_`, else in single quotes with each `'` as `'"'"'`; empty as `''`.
pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c)) {
        return s.into();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// The one command Connect may run for this provider, with rclone named by the full path
/// the daemon finds.
pub fn connect_command(provider: &str) -> Option<String> {
    let (_, _, args) = PROVIDERS.iter().find(|(name, _, _)| *name == provider)?;
    let bin = am_store::setup::which("rclone").map_or("rclone".to_string(), |p| p.to_string_lossy().into_owned());
    Some(std::iter::once(bin.as_str()).chain(args.iter().copied()).map(shell_quote).collect::<Vec<_>>().join(" "))
}

/// After its command succeeded, point [storage] at the new remote. True if config.toml
/// changed.
pub fn connected(provider: &str) -> Result<bool> {
    let Some((_, remote, _)) = PROVIDERS.iter().find(|(name, _, _)| *name == provider) else {
        return util::crash(format!("KeyError: {}", util::quote(provider)));
    };
    am_core::config::set_string("storage", "rclone_remote", remote)
}

/// What the Storage section of Settings shows. Reads files only.
pub fn status(cfg: Option<&Value>) -> Result<Value> {
    let cfg = load(cfg)?;
    let remote = storage_str(&cfg, "rclone_remote").trim_end_matches(':').to_string();
    let folder = folder(&cfg);
    let mut login = Value::Null;
    if !remote.is_empty() {
        let name = crate::health::storage_login(&remote)?;
        let state_file = crate::health::read_state()?;
        let empty = util::empty();
        let logins = util::or(&state_file, "logins", &empty);
        let entry = util::or(logins, &name, &empty);
        let state = if has(entry, "state") { s(entry, "state") } else { "unchecked".into() };
        login = json!({"name": name, "state": state, "label": crate::health::state_label(&state).unwrap_or(""), "checked": get(entry, "checked").cloned().unwrap_or(Value::Null)});
    }
    let entries = list_entries()?;
    let mut connect = serde_json::Map::new();
    for (name, _, _) in PROVIDERS {
        connect.insert(name.into(), connect_command(name).map_or(Value::Null, Value::from));
    }
    let shown: Vec<Value> = entries
        .iter()
        .map(|e| {
            let mut e = e.clone();
            let size = human_size(util::num(get(&e, "bytes")).unwrap_or(0.0));
            util::fields(&mut e).insert("size".into(), size.into());
            e
        })
        .collect();
    // Folded from 0.0: an f64 sum of nothing is -0.0, which would print as "-0 B".
    let removed = entries.iter().filter(|e| has(e, "local_removed")).map(|e| util::num(get(e, "bytes")).unwrap_or(0.0)).fold(0.0, |a, b| a + b);
    Ok(json!({
        "remote": remote,
        "folder": folder,
        "target": if remote.is_empty() { String::new() } else { format!("{remote}:{folder}") },
        "rclone": am_store::setup::which("rclone").is_some(),
        "connect": connect,
        "login": login,
        "entries": shown,
        "offloaded_size": human_size(removed),
        "config": am_home().join("config.toml").to_string_lossy(),
    }))
}

/// The manifest's entries, newest first.
pub fn list_entries() -> Result<Vec<Value>> {
    let data = read_json(&manifest())?.unwrap_or_else(util::empty);
    let mut entries: Vec<Value> = data.as_object().map(|m| m.values().cloned().collect()).unwrap_or_default();
    entries.sort_by(|a, b| {
        let at = |e: &Value| util::num(get(e, "offloaded_at")).unwrap_or(0.0);
        at(b).partial_cmp(&at(a)).unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(entries)
}

/// A file's size, or the total of a directory's files.
fn size_of(path: &Path) -> u64 {
    if path.is_file() {
        return std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    }
    let mut total = 0;
    walk_files(path, &mut |p| total += std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
    total
}

/// Every file under `dir`, not following symlinked directories.
pub(crate) fn walk_files(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            dirs.push(p);
        } else {
            f(&p);
        }
    }
    for d in dirs {
        if !std::fs::symlink_metadata(&d).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            walk_files(&d, f);
        }
    }
}

/// A byte count in B, KB, MB, GB or TB (steps of 1024): bytes whole, the rest to one
/// decimal.
pub fn human_size(n: f64) -> String {
    let mut n = n;
    for unit in ["B", "KB", "MB", "GB", "TB"] {
        if n < 1024.0 || unit == "TB" {
            return if unit == "B" { format!("{n:.0} {unit}") } else { format!("{n:.1} {unit}") };
        }
        n /= 1024.0;
    }
    format!("{n:.1} TB")
}

// ---------------------------------------------------------------- whole projects: what's there

pub const STATE_FILE: &str = "offload.json";
pub const NOT_BACKED_UP: [&str; 4] = ["project.json.lock", STATE_FILE, "offload.json.lock", ".changed"];
pub const STUB_FILES: [&str; 5] = ["project.json", "project.json.lock", STATE_FILE, "offload.json.lock", ".changed"];

/// The storage remote as named to the user: "Google Drive" for the Drive remote, else
/// the rclone remote's name.
pub fn remote_label(cfg: Option<&Value>) -> Result<String> {
    let cfg = load(cfg)?;
    let remote = storage_str(&cfg, "rclone_remote").trim_end_matches(':').to_string();
    let named = PROVIDERS.iter().find(|(_, r, _)| *r == remote).map(|(name, _, _)| *name);
    Ok(if named == Some("google-drive") { "Google Drive".into() } else { remote })
}

pub(crate) fn state_path(slug: &str) -> PathBuf {
    project_dir(slug).join(STATE_FILE)
}

/// Whether a job's recorded pid is a live process.
pub(crate) fn alive(pid: Option<&Value>) -> bool {
    let pid = match pid {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(t)) => am_core::format::parse_int(t),
        Some(Value::Bool(b)) => Some(*b as i64),
        _ => None,
    };
    match pid {
        Some(p) if (i32::MIN as i64..=i32::MAX as i64).contains(&p) => unsafe { libc::kill(p as i32, 0) == 0 },
        _ => false,
    }
}

/// The project's `offloaded` record, or null.
pub fn offloaded_info(slug: &str) -> Result<Value> {
    Ok(get(&projects::get_project(slug)?, "offloaded").cloned().unwrap_or(Value::Null))
}

/// Whether the project is offloaded.
pub fn is_offloaded(slug: &str) -> Result<bool> {
    Ok(am_core::format::truthy(Some(&offloaded_info(slug)?)))
}

/// The user's home directory.
pub(crate) fn user_home() -> PathBuf {
    am_core::expand_user("~")
}

/// Each agent's CLI session and its files under $HOME.
pub fn session_files(slug: &str) -> Result<Vec<Value>> {
    let home = user_home();
    let mut out = Vec::new();
    for name in projects::agent_names(slug)? {
        let agent = read_json(&am_core::agents_dir(slug).join(&name).join("agent.json"))?.unwrap_or(Value::Null);
        let empty = util::empty();
        let session = util::or(&agent, "session", &empty);
        let sid = get(session, "id").and_then(Value::as_str).unwrap_or_default().to_string();
        let backend = get(session, "backend").cloned().unwrap_or(Value::Null);
        if sid.is_empty() || sid.contains('/') || sid.starts_with('.') {
            continue;
        }
        let mut found: Vec<PathBuf> = Vec::new();
        match backend.as_str() {
            Some("claude") => {
                let root = home.join(".claude").join("projects");
                let mut dirs = am_core::fsx::listdir(&root)?;
                dirs.sort();
                for d in dirs {
                    let jsonl = root.join(&d).join(format!("{sid}.jsonl"));
                    if std::fs::symlink_metadata(&jsonl).is_ok() && util::exists(&jsonl) {
                        found.push(jsonl.clone());
                        if root.join(&d).join(&sid).is_dir() {
                            found.push(root.join(&d).join(&sid));
                        }
                    }
                }
            }
            Some("codex") => {
                let mut hits = Vec::new();
                rglob_rollouts(&home.join(".codex").join("sessions"), &sid, &mut hits);
                hits.sort();
                found = hits;
            }
            _ => {}
        }
        let paths: Vec<String> = found
            .iter()
            .filter(|f| !std::fs::symlink_metadata(f).map(|m| m.file_type().is_symlink()).unwrap_or(false))
            .map(|f| f.strip_prefix(&home).unwrap_or(f).to_string_lossy().into_owned())
            .collect();
        out.push(json!({"agent": name, "backend": backend, "id": sid, "paths": paths}));
    }
    Ok(out)
}

/// Files named `rollout-*<sid>.jsonl` anywhere under `dir`, not following symlinked
/// directories.
fn rglob_rollouts(dir: &Path, sid: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if !std::fs::symlink_metadata(&p).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                rglob_rollouts(&p, sid, out);
            }
        } else if name.starts_with("rollout-") && name.ends_with(&format!("{sid}.jsonl")) && name.len() >= 8 + sid.len() + 6 {
            out.push(p);
        }
    }
}

/// How many files a path holds and their total size; a file or a symlink is one.
pub(crate) fn files_and_bytes(path: &Path) -> (u64, u64) {
    let link = std::fs::symlink_metadata(path);
    if path.is_file() || link.as_ref().is_ok_and(|m| m.file_type().is_symlink()) {
        return (1, link.map(|m| m.len()).unwrap_or(0));
    }
    let (mut count, mut total) = (0, 0);
    walk_files(path, &mut |p| {
        count += 1;
        total += std::fs::symlink_metadata(p).map(|m| m.len()).unwrap_or(0);
    });
    (count, total)
}

/// The project's top-level entries, sorted, without `keep`.
pub(crate) fn project_children(slug: &str, keep: &[&str]) -> Result<Vec<PathBuf>> {
    let mut names: Vec<String> = am_core::fsx::listdir(&project_dir(slug))?.into_iter().filter(|n| !keep.contains(&n.as_str())).collect();
    names.sort();
    Ok(names.into_iter().map(|n| project_dir(slug).join(n)).collect())
}

/// What an offload would upload.
pub fn project_plan(slug: &str) -> Result<Value> {
    let (mut files, mut total) = (0, 0);
    for child in project_children(slug, &NOT_BACKED_UP)? {
        let (n, b) = files_and_bytes(&child);
        files += n;
        total += b;
    }
    let sessions = session_files(slug)?;
    let home = user_home();
    let (mut session_count, mut session_bytes) = (0, 0);
    for entry in &sessions {
        for rel in entry["paths"].as_array().into_iter().flatten() {
            let (n, b) = files_and_bytes(&home.join(rel.as_str().unwrap_or_default()));
            session_count += n;
            session_bytes += b;
        }
    }
    Ok(json!({
        "files": files,
        "bytes": total,
        "size": human_size(total as f64),
        "agents": sessions.len(),
        "session_files": session_count,
        "session_size": human_size(session_bytes as f64),
        "freed_size": human_size((total + session_bytes) as f64),
        "remote": remote_label(None)?,
    }))
}

/// What Settings, the stub page and the sidebar show.
pub fn project_status(slug: &str) -> Result<Value> {
    let state = read_json(&state_path(slug))?.filter(|s| am_core::format::truthy(Some(s))).unwrap_or_else(util::empty);
    let mut job = get(&state, "job").cloned().unwrap_or(Value::Null);
    if am_core::format::truthy(Some(&job)) && !alive(get(&job, "pid")) {
        job = Value::Null; // Its process ended without finishing, such as a daemon restart.
    }
    let info = offloaded_info(slug)?;
    let shown = if am_core::format::truthy(Some(&info)) {
        let mut i = info.clone();
        let size = human_size(util::num(get(&info, "bytes")).unwrap_or(0.0));
        util::fields(&mut i).insert("size".into(), size.into());
        i
    } else {
        info.clone()
    };
    Ok(json!({
        "offloaded": am_core::format::truthy(Some(&info)),
        "info": shown,
        "job": job,
        "error": get(&state, "error").cloned().unwrap_or(Value::Null),
        "last_backup": get(&state, "last_backup").cloned().unwrap_or(Value::Null),
    }))
}
