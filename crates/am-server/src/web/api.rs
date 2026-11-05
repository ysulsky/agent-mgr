//! The JSON API: project state, chat, turns, rail actions, health and memory.

use super::files::{clean_attachments, truncate};
use super::http::{raised, Fail, Handled, Req, Resp};
use super::{App, Route};
use crate::deps;
use am_core::{active_dir, am_home, project_dir, format, read_json, turn_dir, turns_dir, Value, HOME_SLUG, MANAGER};
use am_store::{artifacts, items, mail, memory, projects};
use serde_json::{json, Map};

// ---------------------------------------------------------------- request helpers

/// The route's project, which must exist, or 404.
pub fn slug(req: &Req) -> Result<String, Fail> {
    let slug = req.param("slug");
    if !am_core::ids::slug_re().is_match(slug) || !projects::exists(slug) {
        return Err(raised(404, Some(&format!("no project {slug}"))));
    }
    Ok(slug.to_string())
}

/// The route's agent of that project, or 404.
pub fn agent(req: &Req) -> Result<(String, String), Fail> {
    let slug = slug(req)?;
    let name = req.param("agent");
    if !am_core::ids::agent_name_re().is_match(name) || !projects::agent_exists(&slug, name) {
        return Err(raised(404, Some(&format!("no agent {name}"))));
    }
    Ok((slug, name.to_string()))
}

/// A body field as text (`format::text`), or `default` if it's missing.
fn text(body: &Map<String, Value>, key: &str, default: &str) -> String {
    match body.get(key) {
        Some(v) => format::text(v),
        None => default.to_string(),
    }
}

/// A body field as text, or None if it's missing or falsy (`format::truthy`).
fn text_or_none(body: &Map<String, Value>, key: &str) -> Option<String> {
    body.get(key).filter(|v| format::truthy(Some(v))).map(format::text)
}

/// `text` as an integer, or a 500.
fn parse_int_or_fail(text: &str) -> Result<i64, Fail> {
    format::parse_int(text).ok_or_else(|| Fail::Internal(format!("not an integer: {}", format::quote(text))))
}

/// A JSON value as an integer: a bool as 0 or 1, a number truncated, a string parsed;
/// anything else is a 500.
fn int_of(v: &Value) -> Result<i64, Fail> {
    match v {
        Value::Bool(b) => Ok(*b as i64),
        Value::Number(n) => Ok(n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0).trunc() as i64)),
        Value::String(s) => parse_int_or_fail(s),
        other => Err(Fail::Internal(format!("not a number: {}", format::literal(other)))),
    }
}

/// A sum of numbers: integers stay integers until a float joins them. An empty sum is 0.
fn sum_numbers<'a>(values: impl Iterator<Item = &'a Value>) -> Value {
    let mut int: i64 = 0;
    let mut float: Option<f64> = None;
    for v in values {
        match (v.as_i64(), float) {
            (Some(i), None) => int += i,
            (Some(i), Some(f)) => float = Some(f + i as f64),
            (None, None) => float = Some(int as f64 + v.as_f64().unwrap_or(0.0)),
            (None, Some(f)) => float = Some(f + v.as_f64().unwrap_or(0.0)),
        }
    }
    match float {
        Some(f) => json!(f),
        None => json!(int),
    }
}

fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get(key)
}

fn or_default(v: &Value, key: &str, default: Value) -> Value {
    v.get(key).cloned().unwrap_or(default)
}

/// `a` with `extra`'s keys set over it: `a`'s keys keep their order, new ones go last.
fn merged(a: &Value, extra: &[(&str, Value)]) -> Value {
    let mut out = a.as_object().cloned().unwrap_or_default();
    for (k, v) in extra {
        out.insert((*k).to_string(), v.clone());
    }
    Value::Object(out)
}

// ---------------------------------------------------------------- state

pub fn project_summary(slug: &str, cfg: &Value) -> am_core::Result<Value> {
    let project = projects::get_project(slug)?;
    let mut statuses = Vec::new();
    for name in projects::agent_names(slug)? {
        statuses.push(projects::agent_status(slug, &name, None, Some(cfg))?);
    }
    let attention = items::attention_counts(&items::list_attention(slug, false)?);
    let unread = mail::unread_summary(slug)?;
    let manager = statuses.iter().find(|s| s["name"] == MANAGER);
    let running = statuses.iter().filter(|s| format::truthy(get(s, "main_turn")) || format::truthy(get(s, "side_turns"))).count();
    let has_error = statuses.iter().any(|s| s["state"] == "error");
    let archived = format::truthy(get(&project, "archived_at"));
    Ok(json!({
        "slug": slug,
        "name": project["name"],
        "description": or_default(&project, "description", json!("")),
        "status": or_default(&project, "status", json!("")),
        "archived": archived,
        "offloaded": format::truthy(get(&project, "offloaded")),
        "held": if archived { sum_numbers(statuses.iter().map(|s| &s["queued"])) } else { json!(0) },
        "running": running,
        "manager_busy": manager.is_some_and(|m| format::truthy(get(m, "main_turn"))),
        "has_error": has_error,
        "attention": attention,
        "unread": unread["unread"],
        "updated": unread["updated"],
        "cost_usd": sum_numbers(statuses.iter().map(|s| &s["cost_usd"])),
    }))
}

/// The executable `name`: as given if it has a `/`, else the first one on `PATH`.
pub fn which(name: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let runnable = |p: &std::path::Path| std::fs::metadata(p).is_ok_and(|m| !m.is_dir() && m.permissions().mode() & 0o111 != 0);
    if name.contains('/') {
        let p = std::path::PathBuf::from(name);
        return runnable(&p).then_some(p);
    }
    let path = std::env::var("PATH").unwrap_or_else(|_| "/bin:/usr/bin".into());
    path.split(':').map(|dir| std::path::Path::new(if dir.is_empty() { "." } else { dir }).join(name)).find(|p| runnable(p))
}

pub fn overview() -> am_core::Result<Value> {
    let cfg = am_core::config::load()?;
    let mut summaries = Vec::new();
    for slug in projects::list_slugs()? {
        summaries.push(project_summary(&slug, &cfg)?);
    }
    let home = summaries.iter().find(|s| s["slug"] == HOME_SLUG).cloned().unwrap_or(Value::Null);
    let others: Vec<&Value> = summaries.iter().filter(|s| s["slug"] != HOME_SLUG).collect();
    let mut backends = Map::new();
    if let Some(all) = cfg["backends"].as_object() {
        for (name, conf) in all {
            let models = conf.get("models").filter(|m| format::truthy(Some(m))).cloned().unwrap_or(json!([]));
            let available = which(conf["bin"].as_str().unwrap_or("")).is_some();
            backends.insert(name.clone(), json!({"models": models, "available": available}));
        }
    }
    let health = deps::health_snapshot(Some(&cfg))?;
    Ok(json!({
        "home": home,
        "projects": others.iter().filter(|s| !format::truthy(get(s, "archived"))).collect::<Vec<_>>(),
        "archived": others.iter().filter(|s| format::truthy(get(s, "archived"))).collect::<Vec<_>>(),
        "backends": backends,
        "defaults": cfg["defaults"],
        "tmux": which("tmux").is_some(),
        "health": health["summary"],
    }))
}

fn agents_of(slug: &str, cfg: &Value, unread: &Value) -> am_core::Result<Vec<Value>> {
    let mut out = Vec::new();
    for name in projects::agent_names(slug)? {
        let mut status = projects::agent_status(slug, &name, None, Some(cfg))?;
        let mine = unread["agents"].get(&name);
        status["unread"] = mine.and_then(|m| m.get("unread")).cloned().unwrap_or(json!(0));
        status["updated"] = mine.and_then(|m| m.get("updated")).cloned().unwrap_or(json!([]));
        out.push(status);
    }
    Ok(out)
}

/// Everything the project page and its rail show. On the home page the rail
/// spans every project that isn't archived, except Artifacts, which shows the home
/// project's own. An archived project's own page still shows all of its own.
pub fn project_state(slug: &str, scope_all: bool) -> am_core::Result<Value> {
    let cfg = am_core::config::load()?;
    let project = projects::get_project(slug)?;
    let unread = mail::unread_summary(slug)?;
    let slugs: Vec<String> = if scope_all {
        let mut out = Vec::new();
        for s in projects::list_slugs()? {
            if s == slug || !projects::is_archived(&s)? {
                out.push(s);
            }
        }
        out
    } else {
        vec![slug.to_string()]
    };
    let mut agents = Vec::new();
    let mut tasks = Vec::new();
    let mut attention = Vec::new();
    for each in &slugs {
        let name = projects::get_project(each)?["name"].clone();
        let each_unread = if each == slug { unread.clone() } else { mail::unread_summary(each)? };
        for mut agent in agents_of(each, &cfg, &each_unread)? {
            agent["project_name"] = name.clone();
            agents.push(agent);
        }
        for task in items::list_tasks(each, true)? {
            tasks.push(merged(&task, &[("project", json!(each)), ("project_name", name.clone())]));
        }
        for item in items::list_attention(each, false)? {
            attention.push(merged(&item, &[("project", json!(each)), ("project_name", name.clone())]));
        }
    }
    let created = |i: &Value| i.get("created").and_then(Value::as_f64).unwrap_or(0.0);
    attention.sort_by(|a, b| created(b).partial_cmp(&created(a)).unwrap_or(std::cmp::Ordering::Equal));
    let state_file = read_json(&project_dir(slug).join("schedule-state.json"))?.unwrap_or_else(|| json!({}));
    let mut schedules = Vec::new();
    if let Some(all) = items::list_schedules(slug)?.as_object() {
        for (name, entry) in all {
            let last = state_file.get(name).and_then(|s| s.get("last")).cloned().unwrap_or(Value::Null);
            let mut next = Value::Null;
            let enabled = entry.get("enabled").is_none_or(|e| format::truthy(Some(e)));
            if enabled && !format::truthy(get(&project, "archived_at")) {
                let fallback = entry.get("created").filter(|c| format::truthy(Some(c))).and_then(Value::as_f64).unwrap_or_else(am_core::now);
                // An error here (or a panic) just means no next fire.
                if let Ok(Ok(counts)) = std::panic::catch_unwind(|| deps::counts_from(&project, &state_file, name, fallback)) {
                    if let Ok(at) = items::next_fire(entry, counts) {
                        next = json!(at);
                    }
                }
            }
            schedules.push(merged(entry, &[("last", last), ("next", next)]));
        }
    }
    let health_every = project.get("health_every").filter(|h| format::truthy(Some(h))).cloned().unwrap_or_else(|| cfg["defaults"]["health_every"].clone());
    let health_last = state_file.get(deps::HEALTH).and_then(|s| s.get("last")).cloned().unwrap_or(Value::Null);
    let storage = if slug == HOME_SLUG { deps::storage_status(&cfg)? } else { Value::Null };
    let offload = if slug == HOME_SLUG {
        Value::Null
    } else {
        merged(
            &deps::project_status(slug)?,
            &[
                ("remote_label", json!(deps::remote_label(&cfg)?)),
                ("connected", json!(format::truthy(cfg["storage"].get("rclone_remote")))),
            ],
        )
    };
    let attention_counts = items::attention_counts(&attention);
    Ok(json!({
        "project": project,
        "summary": project_summary(slug, &cfg)?,
        "scope": if scope_all { "all" } else { "project" },
        "agents": agents,
        "tasks": tasks,
        "attention": attention,
        "attention_counts": attention_counts,
        "artifacts": artifacts::shown_artifacts(slug)?,
        "schedules": schedules,
        "health_every": health_every,
        "health_last": health_last,
        "paths": {"project": project_dir(slug).to_string_lossy(), "home": am_home().to_string_lossy()},
        "defaults": cfg["defaults"],
        "unread": unread,
        "storage": storage,
        "offload": offload,
    }))
}

// ---------------------------------------------------------------- handlers

pub fn handle(app: &App, route: Route, req: &Req) -> Handled {
    use Route::*;
    let daemon = &app.daemon;
    match route {
        Overview => Resp::ok(&overview()?),
        Project => {
            let slug = slug(req)?;
            Resp::ok(&project_state(&slug, req.query("scope") == Some("all"))?)
        }
        CreateProject => {
            let body = req.json_object()?;
            let name = format::strip(&text(&body, "name", "")).to_string();
            if name.is_empty() {
                return Err(Fail::Am("a project needs a name".into()));
            }
            Resp::ok(&projects::create_project(&name, &text(&body, "description", ""), None)?)
        }
        UpdateProject => {
            let slug = slug(req)?;
            let body = req.json_object()?;
            let mut fields = Map::new();
            for key in ["name", "description", "status", "manager_backend", "manager_model", "worker_backend", "worker_model", "health_every"] {
                if let Some(value) = body.get(key) {
                    let keep = value != "" || key == "description" || key == "status";
                    fields.insert(key.into(), if keep { value.clone() } else { Value::Null });
                }
            }
            if let Some(every) = fields.get("health_every").filter(|v| format::truthy(Some(v))) {
                if every != "off" {
                    am_core::clock::parse_duration_value(every)?;
                }
            }
            Resp::ok(&projects::update_project(&slug, &Value::Object(fields))?)
        }
        Archive => Resp::ok(&projects::archive_project(&slug(req)?)?),
        Unarchive => {
            // An offloaded project comes back first, and is un-archived only once that succeeded.
            let slug = slug(req)?;
            if deps::is_offloaded(&slug)? {
                start_project_job(app, &slug, "restore", true)?;
                return Resp::ok(&merged(&projects::get_project(&slug)?, &[("restoring", json!(true))]));
            }
            let project = projects::unarchive_project(&slug)?;
            daemon.wake_event.set();
            Resp::ok(&project)
        }
        OffloadPlan => Resp::ok(&deps::project_plan(&slug(req)?)?),
        Offload | Restore => {
            // The server picks the paths: a request names only the project.
            let slug = slug(req)?;
            start_project_job(app, &slug, if route == Offload { "offload" } else { "restore" }, false)?;
            Ok(Resp::json(202, &deps::project_status(&slug)?))
        }
        Seen => {
            let slug = slug(req)?;
            let body = req.json_object()?;
            let mut versions = Map::new();
            if let Some(given) = body.get("versions").filter(|v| format::truthy(Some(v))) {
                let given = given.as_object().ok_or_else(|| Fail::Internal("versions isn't an object".into()))?;
                for (k, v) in given {
                    versions.insert(k.clone(), json!(int_of(v)?));
                }
            }
            mail::mark_seen(&slug, &Value::Object(versions))?;
            Resp::ok(&json!({"ok": true}))
        }
        Chat => {
            let (slug, name) = agent(req)?;
            let limit = match req.query("limit") {
                None => 200,
                Some(text) => format::parse_int(text).ok_or_else(|| Fail::Am("limit must be a number".into()))?,
            };
            let limit = limit.clamp(1, 1000) as usize;
            Resp::ok(&mail::chat(&slug, &name, limit, req.query("before"), req.query("since"))?)
        }
        Send => send(app, req),
        Interrupt => {
            let (slug, name) = agent(req)?;
            let body = if req.can_read_body() { req.json_object()? } else { Map::new() };
            match body.get("message_id").filter(|m| format::truthy(Some(m))) {
                Some(message_id) => {
                    if !mail::mark_interrupt(&slug, &name, &format::text(message_id))? {
                        return Err(Fail::Am("that message has already been delivered".into()));
                    }
                }
                None => deps::request_stop(&slug, &name)?,
            }
            daemon.wake_event.set();
            Resp::ok(&json!({"ok": true}))
        }
        UpdateAgent => {
            let (slug, name) = agent(req)?;
            let body = req.json_object()?;
            let backend = body.get("backend").filter(|v| !v.is_null()).map(format::text);
            let model = body.get("model").filter(|v| !v.is_null()).map(format::text);
            projects::set_agent_backend(&slug, &name, backend.as_deref(), model.as_deref())?;
            Resp::ok(&projects::agent_status(&slug, &name, None, None)?)
        }
        RetireAgent => {
            let (slug, name) = agent(req)?;
            projects::retire_agent(&slug, &name)?;
            Resp::ok(&json!({"ok": true}))
        }
        Turns => turns(req),
        Turn => turn(req),
        AttentionSeen => Resp::ok(&items::mark_attention_seen(&slug(req)?, req.param("item"))?),
        AttentionResolve => Resp::ok(&items::resolve_attention(&slug(req)?, req.param("item"), "user")?),
        SetSchedule => {
            let slug = slug(req)?;
            let body = req.json_object()?;
            let name = format::strip(&text(&body, "name", "")).to_string();
            if name.is_empty() || name.starts_with('_') {
                return Err(Fail::Am("a schedule needs a name".into()));
            }
            let opts = items::ScheduleOpts {
                every: text_or_none(&body, "every"),
                cron: text_or_none(&body, "cron"),
                to: text_or_none(&body, "to"),
                prompt: body.get("prompt").filter(|v| !v.is_null()).map(format::text),
                enabled: body.get("enabled").filter(|v| !v.is_null()).map(|v| format::truthy(Some(v))),
            };
            Resp::ok(&items::set_schedule(&slug, &name, opts)?)
        }
        RemoveSchedule => {
            items::remove_schedule(&slug(req)?, req.param("name"))?;
            Resp::ok(&json!({"ok": true}))
        }
        RunSchedule => {
            deps::run_schedule_now(&slug(req)?, req.param("name"))?;
            daemon.wake_event.set();
            Resp::ok(&json!({"ok": true}))
        }
        Health => Resp::ok(&deps::health_snapshot(None)?),
        HealthCheck => {
            // Check now. The page learns the results through live events, one login at a time.
            deps::mark_checking(&am_core::config::load()?)?;
            let data = deps::health_snapshot(None)?;
            daemon.request_login_check(None, &[]);
            Resp::ok(&data)
        }
        MemoryList => Resp::ok(&json!({"entries": memory::list_entries()?})),
        MemoryGet => {
            let name = req.param("name");
            let rev = req.query("rev");
            let text = match rev.filter(|r| !r.is_empty()) {
                Some(rev) => memory::show_at(name, rev)?,
                None => memory::read(name)?,
            };
            Resp::ok(&json!({"name": name, "text": text, "rev": rev, "log": memory::log(Some(name), 100)?}))
        }
        MemoryAdd => {
            let body = req.json_object()?;
            let rev = memory::add(format::strip(&text(&body, "name", "")), &text(&body, "text", ""), None)?;
            Resp::ok(&json!({"rev": rev}))
        }
        MemoryEdit => {
            let body = req.json_object()?;
            Resp::ok(&json!({"rev": memory::edit(req.param("name"), &text(&body, "text", ""), None)?}))
        }
        MemoryRemove => Resp::ok(&json!({"rev": memory::remove(req.param("name"), None)?})),
        other => Err(Fail::Internal(format!("{other:?} isn't a JSON route"))),
    }
}

/// Claim the project now, so a refusal reaches the caller, then do the work in the background.
/// Progress and errors land in the project's offload.json, which Settings shows.
fn start_project_job(app: &App, slug: &str, kind: &'static str, unarchive: bool) -> Result<(), Fail> {
    deps::begin_project_job(slug, kind, unarchive)?;
    let daemon = app.daemon.clone();
    let slug = slug.to_string();
    app.runtime.spawn(async move {
        let job_slug = slug.clone();
        let result = tokio::task::spawn_blocking(move || crate::daemon::caught(|| deps::run_project_job(&job_slug))).await;
        match result {
            Ok(Err(e)) => am_core::log::warning("am.web", &format!("{kind} of {slug} failed: {e}")),
            Err(e) => am_core::log::warning("am.web", &format!("{kind} of {slug} failed: {e}")),
            Ok(Ok(())) => {}
        }
        daemon.wake_event.set();
    });
    Ok(())
}

fn send(app: &App, req: &Req) -> Handled {
    let (slug, name) = agent(req)?;
    let body = req.json_object()?;
    let text_ = text(&body, "text", "");
    let given: Vec<Value> = match body.get("attachments").filter(|a| format::truthy(Some(a))) {
        Some(Value::Array(list)) => list.clone(),
        Some(_) => return Err(Fail::Internal("attachments isn't a list".into())),
        None => vec![],
    };
    let attachments = clean_attachments(&slug, &given)?;
    if format::strip(&text_).is_empty() && attachments.is_empty() {
        return Err(Fail::Am("a message can't be empty".into()));
    }
    let field = |v: &Value, key: &str, n: usize| truncate(&v.get(key).map(format::text).unwrap_or_default(), n);
    let reply_to = body.get("reply_to").filter(|r| format::truthy(Some(r))).map(|r| json!({"id": field(r, "id", 80), "excerpt": field(r, "excerpt", 400)}));
    let comment = body
        .get("comment")
        .filter(|c| format::truthy(Some(c)))
        .map(|c| json!({"artifact": field(c, "artifact", 400), "rev": field(c, "rev", 40), "quote": field(c, "quote", 4000)}));
    let opts = mail::SendOpts {
        sender: "user".into(),
        attachments: Some(attachments),
        reply_to: reply_to.clone(),
        comment,
        interrupt: format::truthy(body.get("interrupt")),
        ..Default::default()
    };
    let message = mail::send(&slug, &name, &text_, opts)?;
    if let Some(reply_to) = &reply_to {
        let id = reply_to["id"].as_str().unwrap_or("");
        if id.starts_with("a-") {
            let _ = items::mark_attention_seen(&slug, id);
        }
    }
    app.daemon.wake_event.set();
    Resp::ok(&message)
}

/// The first `n` items of `list`; a negative `n` drops that many from the end instead.
fn head<T: Clone>(list: &[T], n: i64) -> Vec<T> {
    let len = list.len() as i64;
    let end = if n >= 0 { n.min(len) } else { (len + n).max(0) };
    list[..end as usize].to_vec()
}

fn turns(req: &Req) -> Handled {
    let (slug, name) = agent(req)?;
    let limit = parse_int_or_fail(req.query("limit").unwrap_or("20"))?;
    let mut ids = match std::fs::read_dir(turns_dir(&slug, &name)) {
        Ok(entries) => entries.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect::<Vec<_>>(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Resp::ok(&json!({"turns": []})),
        Err(e) => return Err(e.into()),
    };
    ids.sort();
    ids.reverse();
    let mut out = Vec::new();
    for turn_id in head(&ids, limit) {
        let directory = turn_dir(&slug, &name, &turn_id);
        let meta = read_json(&directory.join("meta.json"))?.filter(|m| format::truthy(Some(m))).unwrap_or_else(|| json!({}));
        let result = read_json(&directory.join("result.json"))?.filter(|r| format::truthy(Some(r))).unwrap_or_else(|| json!({}));
        let pick = |v: &Value, k: &str| v.get(k).cloned().unwrap_or(Value::Null);
        out.push(json!({
            "id": turn_id,
            "kind": pick(&meta, "kind"),
            "backend": pick(&meta, "backend"),
            "model": pick(&meta, "model"),
            "created": pick(&meta, "created"),
            "state": result.get("state").filter(|s| format::truthy(Some(s))).cloned().unwrap_or(json!("running")),
            "ended": pick(&result, "ended"),
            "cost_usd": pick(&result, "cost_usd"),
            "triggers": meta.get("triggers").cloned().unwrap_or(json!([])),
        }));
    }
    Resp::ok(&json!({"turns": out}))
}

/// A file's text: UTF-8, with `\r\n` and `\r` read as `\n`. Text that isn't UTF-8 is a 500.
pub fn read_text(path: &std::path::Path) -> std::io::Result<Result<String, Fail>> {
    let bytes = std::fs::read(path)?;
    Ok(match String::from_utf8(bytes) {
        Ok(text) => Ok(text.replace("\r\n", "\n").replace('\r', "\n")),
        Err(e) => Err(Fail::Internal(format!("not valid UTF-8: {}", e))),
    })
}

fn turn(req: &Req) -> Handled {
    let (slug, name) = agent(req)?;
    let turn_id = req.param("turn");
    static TURN_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    if !TURN_RE.get_or_init(|| regex::Regex::new(r"^t-[a-z0-9]+$").unwrap()).is_match(turn_id) {
        return Err(super::http::not_found());
    }
    let directory = turn_dir(&slug, &name, turn_id);
    let Some(meta) = read_json(&directory.join("meta.json"))? else {
        return Err(raised(404, Some(&format!("no turn {turn_id}"))));
    };
    let offset = parse_int_or_fail(req.query("offset").unwrap_or("0"))?;
    let backend = meta.get("backend").and_then(Value::as_str).unwrap_or("").to_string();
    let (events, next) = deps::read_events(&backend, &directory.join("stream.jsonl"), offset.max(0) as u64)?;
    let mut shown = Map::new();
    for k in ["id", "kind", "agent", "project", "backend", "model", "created", "main_turn", "fork", "stopping", "triggers"] {
        shown.insert(k.into(), meta.get(k).cloned().unwrap_or(Value::Null));
    }
    let mut data = json!({
        "meta": shown,
        "events": events,
        "offset": next,
        "result": read_json(&directory.join("result.json"))?,
        "running": active_dir(&slug, &name).join(turn_id).exists(),
    });
    if offset == 0 {
        if let Ok(text) = read_text(&directory.join("prompt.md")) {
            data["prompt"] = json!(text?);
        }
    }
    Resp::ok(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_keep_integers_until_a_float() {
        assert_eq!(sum_numbers([json!(1), json!(2)].iter()), json!(3));
        assert_eq!(sum_numbers([json!(0), json!(0.5), json!(1)].iter()), json!(1.5));
        assert_eq!(sum_numbers(std::iter::empty()), json!(0));
    }

    #[test]
    fn slices_clamp_and_count_from_the_end() {
        assert_eq!(head(&[1, 2, 3], 2), vec![1, 2]);
        assert_eq!(head(&[1, 2, 3], -1), vec![1, 2]);
        assert_eq!(head(&[1, 2, 3], 10), vec![1, 2, 3]);
    }
}
