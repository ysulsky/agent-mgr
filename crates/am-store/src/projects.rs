//! Projects and agents: plain JSON files under projects/<slug>/.

use crate::util::{self, get, has, now_value, quote};
use am_core::{
    active_dir, agent_dir, agents_dir, am_err, inbox_dir, list_json_ids, paths, poke, project_dir, projects_dir, read_json,
    turn_dir, update_json, write_json, Result, Value, HOME_SLUG, MANAGER,
};
use serde_json::json;
use std::path::{Path, PathBuf};

pub const PROJECT_SUBDIRS: [&str; 7] = ["workspace", "results", "cache", "artifacts", "attachments", "attention", "agents"];
pub const BACKENDS: [&str; 2] = ["claude", "codex"];

pub fn list_slugs() -> Result<Vec<String>> {
    let root = projects_dir();
    let mut slugs: Vec<String> = am_core::fsx::listdir(&root)?
        .into_iter()
        .filter(|n| !n.starts_with('.') && util::exists(&root.join(n).join("project.json")))
        .collect();
    slugs.sort();
    Ok(slugs)
}

pub fn exists(slug: &str) -> bool {
    util::exists(&project_dir(slug).join("project.json"))
}

pub fn get_project(slug: &str) -> Result<Value> {
    match read_json(&project_dir(slug).join("project.json"))? {
        Some(data) if !data.is_null() => Ok(data),
        _ => am_err(format!("no project {}; see `am project list`", quote(slug))),
    }
}

const PROJECT_SETTINGS: [&str; 8] =
    ["name", "description", "status", "health_every", "manager_backend", "manager_model", "worker_backend", "worker_model"];

/// Update a project: `fields` is an object of settings.
pub fn update_project(slug: &str, fields: &Value) -> Result<Value> {
    get_project(slug)?;
    let empty = serde_json::Map::new();
    let map = fields.as_object().unwrap_or(&empty);
    let mut unknown: Vec<&str> = map.keys().map(String::as_str).filter(|k| !PROJECT_SETTINGS.contains(k)).collect();
    if !unknown.is_empty() {
        unknown.sort();
        return am_err(format!("unknown project settings: {}", unknown.join(", ")));
    }
    for key in ["manager_backend", "worker_backend"] {
        if has(fields, key) {
            check_backend(&util::s(fields, key))?;
        }
    }
    let result = update_json(&project_dir(slug).join("project.json"), Value::Null, |d| {
        util::update(d, fields);
        None
    })?;
    poke(slug);
    Ok(result)
}

pub fn check_backend(backend: &str) -> Result<()> {
    if !BACKENDS.contains(&backend) {
        return am_err(format!("unknown backend {}; use {}", quote(backend), BACKENDS.join(" or ")));
    }
    Ok(())
}

pub fn create_project(name: &str, description: &str, slug: Option<&str>) -> Result<Value> {
    let slug = match slug.filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => am_core::slugify(name)?,
    };
    if !am_core::ids::slug_re().is_match(&slug) {
        return am_err(format!("invalid project slug {}", quote(&slug)));
    }
    let root = project_dir(&slug);
    if util::exists(&root.join("project.json")) {
        if is_archived(&slug)? {
            return am_err(format!(
                "project {} already exists, and is archived; `am project unarchive {slug}` brings it back",
                quote(&slug)
            ));
        }
        return am_err(format!("project {} already exists", quote(&slug)));
    }
    for sub in PROJECT_SUBDIRS {
        std::fs::create_dir_all(root.join(sub))?;
    }
    let log = root.join("log.md");
    if !util::exists(&log) {
        std::fs::write(&log, format!("# {name} log\n\nThe manager's private notes.\n"))?;
    }
    touch(&root.join(".changed"))?;
    let project = json!({
        "slug": slug,
        "name": name,
        "description": description,
        "status": "",
        "created": now_value(),
        "archived_at": null,
        "health_every": null,
        "manager_backend": null,
        "manager_model": null,
        "worker_backend": null,
        "worker_model": null,
    });
    write_json(&root.join("project.json"), &project)?;
    create_agent(&slug, MANAGER, "manager", None, None, "")?;
    poke(&slug);
    Ok(project)
}

/// Create a file if missing, else update its times.
pub fn touch(path: &Path) -> Result<()> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(|e| am_core::Error::Other(e.to_string()))?;
    if unsafe { libc::utimes(c.as_ptr(), std::ptr::null()) } != 0 {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().write(true).create(true).truncate(false).mode(0o666).open(path)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- archiving

pub fn is_archived(slug: &str) -> Result<bool> {
    Ok(has(&get_project(slug)?, "archived_at"))
}

/// Pause a project. This only sets a flag, which the runner and scheduler read.
/// Repeating it is harmless, and keeps the original time.
pub fn archive_project(slug: &str) -> Result<Value> {
    if slug == HOME_SLUG {
        return am_err("the home project can't be archived");
    }
    get_project(slug)?;
    let result = update_json(&project_dir(slug).join("project.json"), Value::Null, |p| {
        if !has(p, "archived_at") {
            util::fields(p).insert("archived_at".into(), now_value());
        }
        None
    })?;
    poke(slug);
    Ok(result)
}

/// Resume a project. Only a real un-archive sets `unarchived_at`, which the scheduler
/// counts from.
pub fn unarchive_project(slug: &str) -> Result<Value> {
    get_project(slug)?;
    let result = update_json(&project_dir(slug).join("project.json"), Value::Null, |p| {
        if has(p, "archived_at") {
            let f = util::fields(p);
            f.insert("archived_at".into(), Value::Null);
            f.insert("unarchived_at".into(), now_value());
        }
        None
    })?;
    poke(slug);
    Ok(result)
}

// ---------------------------------------------------------------- agents

pub fn agent_names(slug: &str) -> Result<Vec<String>> {
    let dir = agents_dir(slug);
    let mut names: Vec<String> =
        am_core::fsx::listdir(&dir)?.into_iter().filter(|n| util::exists(&dir.join(n).join("agent.json"))).collect();
    names.sort();
    Ok(names)
}

pub fn get_agent(slug: &str, name: &str) -> Result<Value> {
    match read_json(&agent_dir(slug, name).join("agent.json"))? {
        Some(data) if !data.is_null() => Ok(data),
        _ => {
            refuse_if_offloaded(slug)?;
            am_err(format!("no agent {} in project {}; see `am agent list`", quote(name), quote(slug)))
        }
    }
}

/// Refuse to touch an offloaded project: it's only a stub, and anything written into it
/// would be replaced on restore.
pub fn refuse_if_offloaded(slug: &str) -> Result<()> {
    let project = read_json(&project_dir(slug).join("project.json"))?.unwrap_or(Value::Null);
    if has(&project, "offloaded") {
        return am_err(format!("{slug} is offloaded, so only a stub is here; `am project restore {slug}` brings it back"));
    }
    Ok(())
}

pub fn agent_exists(slug: &str, name: &str) -> bool {
    util::exists(&agent_dir(slug, name).join("agent.json"))
}

/// Apply `change` to `agent.json` under its lock.
pub fn update_agent(slug: &str, name: &str, change: impl FnOnce(&mut Value)) -> Result<Value> {
    let result = update_json(&agent_dir(slug, name).join("agent.json"), Value::Null, |a| {
        change(a);
        None
    })?;
    poke(slug);
    Ok(result)
}

/// Create an agent. Usually called with role "worker" and purpose "".
pub fn create_agent(
    slug: &str,
    name: &str,
    role: &str,
    backend: Option<&str>,
    model: Option<&str>,
    purpose: &str,
) -> Result<Value> {
    if !am_core::ids::agent_name_re().is_match(name) {
        return am_err(format!("invalid agent name {}: use lowercase letters, digits, - and _", quote(name)));
    }
    if name == MANAGER && role != "manager" {
        return am_err("the name 'manager' is reserved for the project's manager");
    }
    if let Some(b) = backend.filter(|b| !b.is_empty()) {
        check_backend(b)?;
    }
    refuse_if_offloaded(slug)?;
    let root = agent_dir(slug, name);
    if util::exists(&root.join("agent.json")) {
        return am_err(format!("agent {} already exists in project {}", quote(name), quote(slug)));
    }
    for sub in ["inbox", "delivered", "answers", "turns", "active", "notes"] {
        std::fs::create_dir_all(root.join(sub))?;
    }
    let agent = json!({
        "name": name,
        "project": slug,
        "role": role,
        "backend": backend,
        "model": model,
        "purpose": purpose,
        "created": now_value(),
        "retired": false,
        "session": null,
        "cost_usd": 0.0,
        "tokens": {"input": 0, "output": 0},
        "turns": 0,
        "retries": 0,
        "not_before": 0,
        "notes": [],
        "last_error": null,
    });
    write_json(&root.join("agent.json"), &agent)?;
    poke(slug);
    Ok(agent)
}

/// Set an agent's backend and model. `None` leaves a field alone and an empty string
/// returns it to the project default.
pub fn set_agent_backend(slug: &str, name: &str, backend: Option<&str>, model: Option<&str>) -> Result<Value> {
    if let Some(b) = backend.filter(|b| !b.is_empty()) {
        check_backend(b)?;
    }
    update_agent(slug, name, |agent| {
        let f = util::fields(agent);
        if let Some(b) = backend {
            f.insert("backend".into(), if b.is_empty() { Value::Null } else { b.into() });
        }
        if let Some(m) = model {
            f.insert("model".into(), m.into());
        }
    })
}

pub fn retire_agent(slug: &str, name: &str) -> Result<Value> {
    if name == MANAGER {
        return am_err("a project's manager cannot be retired");
    }
    update_agent(slug, name, |a| {
        let f = util::fields(a);
        f.insert("retired".into(), true.into());
        f.insert("retired_at".into(), now_value());
    })
}

/// An agent's (backend, model), from its own setting, then the project's, then config.
/// A default model only applies on the backend it was set for. An empty model means the
/// CLI's own default.
pub fn effective_backend(slug: &str, agent: &Value, cfg: Option<&Value>) -> Result<(String, String)> {
    let project = get_project(slug)?;
    effective_backend_in(&project, agent, cfg)
}

/// `effective_backend` with the project already read.
pub fn effective_backend_in(project: &Value, agent: &Value, cfg: Option<&Value>) -> Result<(String, String)> {
    let loaded;
    let cfg = match cfg.filter(|c| am_core::format::truthy(Some(c))) {
        Some(c) => c,
        None => {
            loaded = am_core::config::load()?;
            &loaded
        }
    };
    let prefix = if get(agent, "role").and_then(Value::as_str) == Some("manager") { "manager" } else { "worker" };
    let defaults = cfg.get("defaults").cloned().unwrap_or(Value::Null);
    let config_backend = defaults.get(format!("{prefix}_backend")).cloned().unwrap_or(Value::Null);
    let config_model = util::or(&defaults, &format!("{prefix}_model"), &Value::String(String::new())).clone();
    let project_backend = get(project, &format!("{prefix}_backend")).cloned().unwrap_or(Value::Null);
    let project_model = get(project, &format!("{prefix}_model")).cloned().unwrap_or(Value::Null);
    let t = |v: &Value| am_core::format::truthy(Some(v));
    let backend = if has(agent, "backend") {
        agent["backend"].clone()
    } else if t(&project_backend) {
        project_backend.clone()
    } else {
        config_backend.clone()
    };
    let model = if has(agent, "model") {
        agent["model"].clone()
    } else if t(&project_model) && (if t(&project_backend) { &project_backend } else { &config_backend }) == &backend {
        project_model
    } else if config_backend == backend {
        config_model
    } else {
        Value::String(String::new())
    };
    Ok((am_core::format::text(&backend), am_core::format::text(&model)))
}

/// Turns of this agent that are not finished, each from its `meta.json`.
pub fn active_turns(slug: &str, name: &str) -> Result<Vec<Value>> {
    let mut names = am_core::fsx::listdir(&active_dir(slug, name))?;
    names.sort();
    let mut out = Vec::new();
    for tid in names {
        if tid.starts_with('.') {
            continue;
        }
        if let Some(meta) = read_json(&turn_dir(slug, name, &tid).join("meta.json"))? {
            if am_core::format::truthy(Some(&meta)) {
                out.push(meta);
            }
        }
    }
    Ok(out)
}

pub fn agent_status(slug: &str, name: &str, agent: Option<&Value>, cfg: Option<&Value>) -> Result<Value> {
    let owned;
    let agent = match agent.filter(|a| am_core::format::truthy(Some(a))) {
        Some(a) => a,
        None => {
            owned = get_agent(slug, name)?;
            &owned
        }
    };
    let turns = active_turns(slug, name)?;
    let kind_is = |t: &Value, k: &str| get(t, "kind").and_then(Value::as_str) == Some(k);
    let main = turns.iter().find(|t| kind_is(t, "main"));
    let sides: Vec<&Value> = turns.iter().filter(|t| kind_is(t, "side")).collect();
    let pending = list_json_ids(&inbox_dir(slug, name))?;
    let not_before = get(agent, "not_before").cloned().unwrap_or(0.into());
    let state = if has(agent, "retired") {
        "retired"
    } else if main.is_some() {
        "running"
    } else if !sides.is_empty() {
        "answering"
    } else if has(agent, "last_error") {
        "error"
    } else if !pending.is_empty() && is_archived(slug)? {
        "held"
    } else if !pending.is_empty() && util::num(Some(&not_before)).unwrap_or(0.0) > am_core::now() {
        "waiting"
    } else if !pending.is_empty() {
        "starting"
    } else {
        "idle"
    };
    let (backend, model) = effective_backend(slug, agent, cfg)?;
    let field = |t: &Value, k: &str| get(t, k).cloned().unwrap_or(Value::Null);
    Ok(json!({
        "name": name,
        "project": slug,
        "role": field(agent, "role"),
        "purpose": get(agent, "purpose").cloned().unwrap_or_else(|| "".into()),
        "backend": backend,
        "model": model,
        "own_backend": field(agent, "backend"),
        "own_model": field(agent, "model"),
        "state": state,
        "retired": has(agent, "retired"),
        "main_turn": main.map_or(Value::Null, |m| field(m, "id")),
        "main_started": main.map_or(Value::Null, |m| field(m, "created")),
        "side_turns": sides.iter().map(|t| field(t, "id")).collect::<Vec<_>>(),
        "running_turns": turns.iter().map(|t| json!({
            "id": field(t, "id"),
            "kind": field(t, "kind"),
            "started": field(t, "created"),
            "stopping": get(t, "stopping").cloned().unwrap_or(false.into()),
        })).collect::<Vec<_>>(),
        "queued": pending.len(),
        "cost_usd": get(agent, "cost_usd").cloned().unwrap_or_else(|| util::float(0.0)),
        "cost_is_estimate": has(agent, "cost_is_estimate"),
        "tokens": get(agent, "tokens").cloned().unwrap_or_else(util::empty),
        "turns": get(agent, "turns").cloned().unwrap_or(0.into()),
        "last_error": field(agent, "last_error"),
        "not_before": not_before,
    }))
}

/// Every agent's `cost_usd` added up, from 0.
pub fn project_cost(slug: &str) -> Result<f64> {
    let mut total = 0.0;
    for name in agent_names(slug)? {
        total += util::num(get(&get_agent(slug, &name)?, "cost_usd")).unwrap_or(0.0);
    }
    Ok(total)
}

/// An absolute path with symlinks resolved, whether or not it exists: the real path of
/// the longest existing prefix, with the rest appended.
pub fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    if let Ok(real) = std::fs::canonicalize(&absolute) {
        return real;
    }
    let mut rest = Vec::new();
    let mut head = absolute.clone();
    loop {
        match head.file_name() {
            Some(name) => rest.push(name.to_os_string()),
            None => return absolute,
        }
        head.pop();
        if let Ok(real) = std::fs::canonicalize(&head) {
            let mut out = real;
            for part in rest.iter().rev() {
                if part == ".." {
                    out.pop();
                } else if part != "." {
                    out.push(part);
                }
            }
            return out;
        }
    }
}

/// The project a path is inside of, if any.
pub fn project_for_path(path: &Path) -> Result<Option<String>> {
    let resolved = resolve(path);
    let root = resolve(&projects_dir());
    let Ok(rel) = resolved.strip_prefix(&root) else { return Ok(None) };
    match rel.components().next() {
        Some(first) => {
            let first = first.as_os_str().to_string_lossy().into_owned();
            Ok(if exists(&first) { Some(first) } else { None })
        }
        None => Ok(None),
    }
}

/// The current project: $AM_PROJECT, else the current directory's.
pub fn current_project() -> Result<Option<String>> {
    if let Some(env) = std::env::var("AM_PROJECT").ok().filter(|s| !s.is_empty()) {
        return Ok(Some(env));
    }
    match std::env::current_dir() {
        Ok(cwd) => project_for_path(&cwd),
        Err(e) => Err(e.into()),
    }
}

pub fn require_current_project(explicit: Option<&str>) -> Result<String> {
    let slug = match explicit.filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => match current_project()? {
            Some(s) => s,
            None => return am_err("no project here: pass --project, or run inside a project directory"),
        },
    };
    if !exists(&slug) {
        return am_err(format!("no project {}; see `am project list`", quote(&slug)));
    }
    Ok(slug)
}

/// Resolve `worker`, `project/worker`, or `project` (its manager) to (slug, agent).
pub fn parse_address(address: &str, default_project: Option<&str>) -> Result<(String, String)> {
    let default_project = default_project.filter(|d| !d.is_empty());
    if let Some((slug, name)) = address.split_once('/') {
        return Ok((slug.into(), if name.is_empty() { MANAGER.into() } else { name.into() }));
    }
    if let Some(d) = default_project {
        if agent_exists(d, address) {
            return Ok((d.into(), address.into()));
        }
    }
    if exists(address) {
        return Ok((address.into(), MANAGER.into()));
    }
    if address == MANAGER {
        return Ok((default_project.unwrap_or(HOME_SLUG).into(), MANAGER.into()));
    }
    match default_project {
        Some(d) => am_err(format!("no agent {} in project {} and no project with that name", quote(address), quote(d))),
        None => am_err(format!("no project {}; address an agent as project/agent", quote(address))),
    }
}

/// Remove a directory tree, ignoring errors.
pub fn remove_tree(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

/// Where a project's files are, for callers that only have this module.
pub fn dir(slug: &str) -> PathBuf {
    paths::project_dir(slug)
}
