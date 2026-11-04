//! Delivers timed prompts, schedules and each project's
//! health check.

use crate::{log, prompts, convert, store};
use am_core::{am_err, first_line, format_duration, now, project_dir, read_json, turn_dir, update_json, Result, Value, MANAGER};
use am_store::{items, mail, projects};
use serde_json::json;
use std::path::PathBuf;

const LOG: &str = "am.scheduler";
pub const HEALTH: &str = "_health";

fn state_path(slug: &str) -> PathBuf {
    project_dir(slug).join("schedule-state.json")
}

fn read_state(slug: &str) -> Value {
    read_json(&state_path(slug)).ok().flatten().unwrap_or_else(|| json!({}))
}

/// The file's modification time, in seconds since the epoch.
fn mtime(path: &std::path::Path) -> Option<f64> {
    std::fs::metadata(path).ok().map(|m| am_store::util::mtime(&m))
}

fn duration(v: &Value) -> Result<f64> {
    am_core::clock::parse_duration_value(v)
}

/// Deliver a schedule's prompt, unless the last one hasn't been picked up yet. Returns
/// the message, if one was sent.
pub fn fire(slug: &str, name: &str, entry: &Value) -> Result<Option<Value>> {
    let target = if am_core::format::truthy(entry.get("to")) { convert::text(&entry["to"]) } else { MANAGER.to_string() };
    if !projects::agent_exists(slug, &target) || am_core::format::truthy(projects::get_agent(slug, &target)?.get("retired")) {
        log::warning(LOG, &format!("schedule {name} of {slug}: no agent {target}"));
        return Ok(None);
    }
    for message in mail::pending(slug, &target)? {
        if message.get("meta").and_then(|m| m.get("schedule")).and_then(Value::as_str) == Some(name) {
            return Ok(None);
        }
    }
    let opts = store::SendOpts {
        sender: "scheduler".into(),
        kind: Some("schedule".into()),
        meta: Some(json!({"schedule": name})),
        ..Default::default()
    };
    Ok(Some(store::send(slug, &target, &convert::text(&entry["prompt"]), opts)?))
}

pub fn run_now(slug: &str, name: &str) -> Result<()> {
    let schedules = items::list_schedules(slug)?;
    let Some(entry) = schedules.get(name).cloned() else {
        return am_err(format!("no schedule {} in project {}", am_core::format::quote(name), am_core::format::quote(slug)));
    };
    if projects::is_archived(slug)? {
        return am_err(format!("{slug} is archived, so its schedules don't run; un-archive it first: am project unarchive {slug}"));
    }
    fire(slug, name, &entry)?;
    update_json(&state_path(slug), json!({}), |s| {
        if !s.get(name).is_some_and(Value::is_object) {
            s[name] = json!({});
        }
        s[name]["last"] = json!(now());
        None
    })?;
    Ok(())
}

fn running_minutes(started: Option<f64>) -> String {
    let current = now();
    format!("{}m", ((current - started.unwrap_or(current)) / 60.0).trunc() as i64)
}

/// A snapshot for the manager, so a clean check can be a short one.
pub fn health_prompt(slug: &str, cfg: &Value) -> Result<String> {
    let stuck_after = duration(&cfg["runner"]["stuck_after"])?;
    let mut lines = Vec::new();
    for name in projects::agent_names(slug)? {
        let agent = projects::get_agent(slug, &name)?;
        if am_core::format::truthy(agent.get("retired")) {
            continue;
        }
        let status = projects::agent_status(slug, &name, Some(&agent), Some(cfg))?;
        let mut detail = convert::text(&status["state"]);
        if am_core::format::truthy(status.get("main_turn")) {
            let started = status.get("main_started").filter(|v| am_core::format::truthy(Some(v))).map(convert::to_float);
            detail.push_str(&format!(" for {}", running_minutes(started)));
            let stream = turn_dir(slug, &name, &convert::text(&status["main_turn"])).join("stream.jsonl");
            let quiet = mtime(&stream).map_or(0.0, |m| now() - m);
            detail.push_str(&format!(", last output {}m ago", (quiet / 60.0).trunc() as i64));
            if quiet > stuck_after {
                detail.push_str(" (may be stuck)");
            }
        }
        if am_core::format::truthy(status.get("queued")) {
            detail.push_str(&format!(", {} queued", convert::text(&status["queued"])));
        }
        if am_core::format::truthy(status.get("last_error")) {
            detail.push_str(&format!(", last error: {}", convert::text(&status["last_error"])));
        }
        lines.push(format!("- {name}: {detail}"));
    }
    let mut text = prompts::render_kv("messages/health-check", &[("slug", slug), ("agents", &lines.join("\n"))])?;
    let tasks = items::list_tasks(slug, false)?;
    if !tasks.is_empty() {
        let task_lines: Vec<String> = tasks
            .iter()
            .map(|t| {
                let mut line = format!("- #{} [{}] {}", convert::text(&t["id"]), convert::text(&t["status"]), convert::text(&t["title"]));
                if am_core::format::truthy(t.get("agent")) {
                    line.push_str(&format!(" ({})", convert::text(&t["agent"])));
                }
                line
            })
            .collect();
        text.push_str("\n\n");
        text.push_str(&prompts::render_kv("messages/health-check-tasks", &[("tasks", &task_lines.join("\n"))])?);
    }
    Ok(text)
}

/// A health check looks for stuck work, so with none it would only cost money.
pub fn work_in_flight(slug: &str, cfg: &Value) -> Result<bool> {
    for name in projects::agent_names(slug)? {
        if name == MANAGER {
            continue;
        }
        let status = projects::agent_status(slug, &name, None, Some(cfg))?;
        if am_core::format::truthy(status.get("main_turn")) {
            return Ok(true);
        }
    }
    Ok(items::list_tasks(slug, true)?.iter().any(|t| matches!(t.get("status").and_then(Value::as_str), Some("doing" | "blocked"))))
}

pub fn tick(cfg: &Value) -> Result<()> {
    let current = now();
    for slug in projects::list_slugs()? {
        if let Err(e) = tick_project(&slug, cfg, current) {
            log::exception(LOG, &format!("scheduler failed for {slug}"), &e);
        }
    }
    Ok(())
}

fn value_or(v: Option<&Value>, fallback: f64) -> f64 {
    match v {
        Some(v) if am_core::format::truthy(Some(v)) => convert::to_float(v),
        _ => fallback,
    }
}

/// What a schedule's next run is counted from: its last run, or un-archiving if that's
/// later. A daemon that was down makes up what it missed with one run of each schedule,
/// but a project can be archived for months, and all of its schedules coming due at the
/// moment it's un-archived would be a burst nobody asked for. So those runs are skipped.
pub fn counts_from(project: &Value, state: &Value, name: &str, fallback: f64) -> f64 {
    let last = value_or(state.get(name).and_then(|s| s.get("last")), fallback);
    convert::max_of(last, value_or(project.get("unarchived_at"), 0.0))
}

/// One project's schedules, health check and stuck-manager watch.
pub fn tick_project(slug: &str, cfg: &Value, current: f64) -> Result<()> {
    let project = projects::get_project(slug)?;
    if am_core::format::truthy(project.get("archived_at")) {
        return Ok(());
    }
    let state = read_state(slug);
    let mut updates: Vec<(String, f64)> = Vec::new();
    let mut health_message: Option<Value> = None;
    if let Value::Object(schedules) = items::list_schedules(slug)? {
        for (name, entry) in schedules {
            if !entry.get("enabled").is_none_or(|e| am_core::format::truthy(Some(e))) {
                continue;
            }
            let last = counts_from(&project, &state, &name, value_or(entry.get("created"), current));
            if current >= items::next_fire(&entry, last)? {
                fire(slug, &name, &entry)?;
                updates.push((name.clone(), current));
            }
        }
    }
    let every = match project.get("health_every") {
        Some(v) if am_core::format::truthy(Some(v)) => v.clone(),
        _ => cfg["defaults"]["health_every"].clone(),
    };
    if am_core::format::truthy(Some(&every)) && every.as_str() != Some("off") {
        let every = duration(&every)?;
        let last = counts_from(&project, &state, HEALTH, value_or(project.get("created"), current));
        if current >= last + every {
            updates.push((HEALTH.to_string(), current));
            if work_in_flight(slug, cfg)? {
                let entry = json!({"to": MANAGER, "prompt": health_prompt(slug, cfg)?});
                health_message = fire(slug, HEALTH, &entry)?;
            }
        }
    }
    watch_for_stuck_manager(slug, cfg, current)?;
    if !updates.is_empty() {
        update_json(&state_path(slug), json!({}), |data| {
            for (name, at) in &updates {
                if !data.get(name.as_str()).is_some_and(Value::is_object) {
                    data[name.as_str()] = json!({});
                }
                data[name.as_str()]["last"] = convert::float_value(*at);
            }
            if let Some(message) = &health_message {
                data[HEALTH]["sent"] = convert::float_value(current);
                data[HEALTH]["message"] = message["id"].clone();
            }
            None
        })?;
    }
    Ok(())
}

/// How the last health check that was sent went. Its reply is free text, so whether it
/// found a problem isn't recorded anywhere, unless its turn failed.
fn last_health_check(slug: &str, message_id: Option<&str>) -> Result<Value> {
    let Some(id) = message_id.filter(|i| !i.is_empty()) else { return Ok(Value::Null) };
    let Some(message) = mail::get_message(slug, MANAGER, id)?.filter(|m| am_core::format::truthy(Some(m))) else { return Ok(Value::Null) };
    if !am_core::format::truthy(message.get("claimed_by")) {
        return Ok(json!({"state": "queued"}));
    }
    let turn = convert::text(&message["claimed_by"]);
    let Some(result) = read_json(&turn_dir(slug, MANAGER, &turn).join("result.json"))?.filter(|r| !r.is_null()) else {
        return Ok(json!({"state": "running", "turn": turn}));
    };
    let reply = if am_core::format::truthy(result.get("final")) {
        convert::text(&result["final"])
    } else {
        convert::or_str(result.get("error"))
    };
    Ok(json!({
        "state": result.get("state").cloned().unwrap_or(Value::Null),
        "turn": turn,
        "ended": result.get("ended").cloned().unwrap_or(Value::Null),
        "reply": first_line(&reply, 200),
    }))
}

/// A project's health check, for the Health page. `last` is when one last came due, and
/// `sent` when one last went to the manager: with nothing in progress, none is sent.
pub fn health_status(slug: &str, cfg: &Value) -> Result<Value> {
    let project = projects::get_project(slug)?;
    let state = read_state(slug).get(HEALTH).cloned().unwrap_or_else(|| json!({}));
    let every = [project.get("health_every"), cfg["defaults"].get("health_every")]
        .into_iter()
        .flatten()
        .find(|v| am_core::format::truthy(Some(v)))
        .cloned()
        .unwrap_or_else(|| json!("off"));
    let archived = am_core::format::truthy(project.get("archived_at"));
    let mut next_due = Value::Null;
    if every.as_str() != Some("off") && !archived {
        let from = value_or(state.get("last"), value_or(project.get("created"), now()));
        let counted_from = convert::max_of(from, value_or(project.get("unarchived_at"), 0.0));
        if let Ok(seconds) = duration(&every) {
            next_due = convert::float_value(counted_from + seconds);
        }
    }
    Ok(json!({
        "project": slug,
        "name": project.get("name").cloned().unwrap_or(Value::Null),
        "every": every,
        "archived": archived,
        "last": state.get("last").cloned().unwrap_or(Value::Null),
        "sent": state.get("sent").cloned().unwrap_or(Value::Null),
        "next": next_due,
        "check": last_health_check(slug, state.get("message").and_then(Value::as_str))?,
    }))
}

/// A stuck manager can't take a health check, so the user has to hear directly.
fn watch_for_stuck_manager(slug: &str, cfg: &Value, current: f64) -> Result<()> {
    let status = projects::agent_status(slug, MANAGER, None, Some(cfg))?;
    let key = format!("stuck-{MANAGER}");
    let limit = duration(&cfg["runner"]["stuck_after"])?;
    if am_core::format::truthy(status.get("main_turn")) {
        let turn = convert::text(&status["main_turn"]);
        let stream = turn_dir(slug, MANAGER, &turn).join("stream.jsonl");
        let quiet = match mtime(&stream) {
            Some(m) => current - m,
            None => current - value_or(status.get("main_started"), current),
        };
        if quiet > limit {
            let body = format!(
                "Turn {turn} has produced no output for {}. Messages to the manager are queued until it ends. Interrupt it from the composer if it needs to stop.",
                format_duration(((quiet / 60.0).floor() * 60.0).trunc())
            );
            store::add_attention(slug, "Manager may be stuck", &body, "alert", "scheduler", &key, None)?;
            return Ok(());
        }
    }
    store::resolve_attention_key(slug, &key, "scheduler")
}
