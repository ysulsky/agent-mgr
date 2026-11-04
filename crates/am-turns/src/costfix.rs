//! Recompute every recorded turn's cost and tokens from the
//! turn's own stream.
//!
//! Before cost accounting used running totals (see backends.rs), each turn was booked the
//! session's whole running total, so an agent's cost grew by everything its session had
//! ever cost, every turn. This replays each agent's turns in the order they started and
//! finished, recomputes each turn's share the way the runner now does, and rewrites the
//! turn's result, its chat note and the agent's totals. It also records each session's
//! running totals, which the runner needs as the base for the next turn, and gives turns
//! the old code launched that are still running the base they started from.
//!
//! Everything is recomputed from stream.jsonl, never from the stored numbers, so running
//! it again changes nothing. Files it changes are copied first to
//! $AM_HOME/backups/cost-fix-<time>/. Run it with the daemon stopped, or let the daemon
//! run it once at startup.

use crate::runner::record_usage_totals;
use crate::{backends, convert, store};
use am_core::{agent_dir, am_home, daemon_dir, read_json, turn_dir, write_json, Result, Value};
use serde_json::{json, Map};
use std::fs;
use std::path::{Path, PathBuf};

pub const MARKER: &str = "cost-accounting-v2";
const KEYS: [&str; 3] = ["input", "cached_input", "output"];

struct TurnRec {
    id: String,
    dir: PathBuf,
    meta: Value,
    result: Option<Value>,
    created: f64,
    ended: f64,
}

struct Change {
    turn: usize,
    cost_usd: f64,
    tokens: Value,
}

struct Plan {
    slug: String,
    name: String,
    old_cost: f64,
    outside: Value,
    new_cost: f64,
    cost_is_estimate: bool,
    tokens: Value,
    usage_totals: Map<String, Value>,
    changes: Vec<Change>,
    unbased: Vec<(String, Value)>,
    turns: Vec<TurnRec>,
    missing_streams: i64,
}

fn read(path: &Path) -> Option<Value> {
    read_json(path).ok().flatten().filter(|v| !v.is_null())
}

fn f(v: Option<&Value>) -> f64 {
    convert::to_float_or0(v)
}

/// Equality of JSON values, with numbers compared by value: `1` equals `1.0`, and `true`
/// equals `1`.
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Bool(x), Value::Number(y)) | (Value::Number(y), Value::Bool(x)) => y.as_f64() == Some(*x as i64 as f64),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same_value(a, b)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same_value(v, w))),
        _ => a == b,
    }
}

/// The value if it's truthy, else null.
fn or_none(v: Option<&Value>) -> Value {
    v.filter(|x| am_core::format::truthy(Some(x))).cloned().unwrap_or(Value::Null)
}

fn turns(slug: &str, name: &str) -> Vec<TurnRec> {
    let mut out = Vec::new();
    // In directory listing order, unsorted: the old totals are summed in it.
    for turn_id in am_core::fsx::listdir(&am_core::turns_dir(slug, name)).unwrap_or_default() {
        let dir = turn_dir(slug, name, &turn_id);
        let Some(meta) = read(&dir.join("meta.json")).filter(|m| am_core::format::truthy(Some(m))) else { continue };
        let result = read(&dir.join("result.json"));
        let exit_info = read(&dir.join("exit.json")).unwrap_or(json!({}));
        let created = f(meta.get("created"));
        let ended_v = result.as_ref().and_then(|r| r.get("ended")).filter(|e| am_core::format::truthy(Some(e))).or(exit_info.get("ended"));
        let ended = match ended_v {
            Some(v) if am_core::format::truthy(Some(v)) => convert::to_float(v),
            _ => created,
        };
        out.push(TurnRec { id: turn_id, dir, meta, result, created, ended });
    }
    out
}

/// What the agent's records should say. Changes nothing.
fn plan_agent(slug: &str, name: &str, cfg: &Value) -> Result<Plan> {
    let turns = turns(slug, name);
    // (time, order, id, index, is_launch)
    let mut events: Vec<(f64, u8, String, usize, bool)> = Vec::new();
    for (i, turn) in turns.iter().enumerate() {
        events.push((turn.created, 1, turn.id.clone(), i, true));
        // Only turns the runner finished were ever booked a cost.
        if turn.result.as_ref().is_some_and(|r| r.get("cost_usd").is_some()) {
            events.push((turn.ended, 0, turn.id.clone(), i, false));
        }
    }
    events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut running = json!({}); // The agent object that record_usage_totals keeps totals in.
    let mut bases: std::collections::HashMap<String, Value> = Default::default();
    let mut changes = Vec::new();
    let mut total_cost = 0.0;
    let mut estimate = false;
    let mut tokens = [0i64; 3];
    let mut missing_streams = 0;
    for (_, _, _, i, launch) in events {
        let turn = &turns[i];
        let meta = &turn.meta;
        if launch {
            let base = match meta.get("usage_base") {
                Some(b) => b.clone(),
                None => match meta.get("session_in").filter(|s| am_core::format::truthy(Some(s))) {
                    Some(session) => running.get("usage_totals").and_then(|t| t.get(convert::text(session))).cloned().unwrap_or(Value::Null),
                    None => Value::Null,
                },
            };
            bases.insert(turn.id.clone(), base);
            continue;
        }
        let result = turn.result.clone().unwrap_or(json!({}));
        let stream = turn.dir.join("stream.jsonl");
        let (cost, turn_tokens, is_estimate);
        if stream.exists() {
            let backend = meta.get("backend").map_or_else(|| "claude".to_string(), convert::text);
            let model = meta.get("model").map_or_else(String::new, convert::text);
            let base = bases.get(&turn.id).filter(|b| !b.is_null());
            let parsed = backends::parse_stream(&backend, &stream, cfg.get("pricing"), &model, base);
            cost = parsed.cost_usd;
            turn_tokens = parsed.tokens.to_value();
            is_estimate = parsed.cost_is_estimate;
            if meta.get("kind").and_then(Value::as_str) == Some("main") {
                record_usage_totals(&mut running, parsed.session.as_deref(), &parsed.running);
            }
        } else {
            missing_streams += 1;
            cost = f(result.get("cost_usd"));
            turn_tokens = result.get("tokens").filter(|t| am_core::format::truthy(Some(t))).cloned().unwrap_or(json!({}));
            is_estimate = am_core::format::truthy(result.get("cost_is_estimate"));
        }
        total_cost += cost;
        estimate = estimate || is_estimate;
        for (k, key) in KEYS.iter().enumerate() {
            tokens[k] += convert::to_int(turn_tokens.get(*key));
        }
        let old = f(result.get("cost_usd"));
        if (old - cost).abs() > 1e-9 || !same_value(&or_none(result.get("tokens")), &or_none(Some(&turn_tokens))) {
            changes.push(Change { turn: i, cost_usd: cost, tokens: turn_tokens });
        }
    }
    // Turns the old code launched that haven't finished: still running, or ended while no
    // daemon was watching. The runner finishes them later with the base worked out here,
    // as of their launch. Worked out when they finish, it would be wrong for a side turn
    // whose main turn finishes first, as the session's totals have moved on by then.
    let unbased: Vec<(String, Value)> = turns
        .iter()
        .filter(|t| t.result.is_none() && t.meta.get("usage_base").is_none())
        .map(|t| (t.id.clone(), bases.get(&t.id).cloned().unwrap_or(Value::Null)))
        .collect();
    let agent = store::get_agent(slug, name)?;
    let old_cost = f(agent.get("cost_usd"));
    let old_turn_sum: f64 = turns.iter().fold(0.0, |s, t| s + t.result.as_ref().map_or(0.0, |r| f(r.get("cost_usd"))));
    // Usage the agent was booked that no turn on disk accounts for is kept as it was.
    // It's worked out on the first run and stored, so later runs don't depend on which
    // files an interrupted run got to.
    let accounted = agent.get("cost_accounting").is_some_and(|v| same_value(v, &json!(2)));
    let stored = if accounted { agent.get("usage_outside_turns").filter(|o| !o.is_null()).cloned() } else { None };
    let outside = match stored {
        Some(o) => o,
        None => {
            let old_tokens = agent.get("tokens").filter(|t| am_core::format::truthy(Some(t))).cloned().unwrap_or(json!({}));
            let mut o = Map::new();
            o.insert("cost_usd".into(), convert::float_value(convert::max_of(old_cost - old_turn_sum, 0.0)));
            for key in KEYS {
                let sum: i64 = turns.iter().map(|t| convert::to_int(t.result.as_ref().and_then(|r| r.get("tokens")).and_then(|x| x.get(key)))).sum();
                o.insert(key.into(), json!((convert::to_int(old_tokens.get(key)) - sum).max(0)));
            }
            Value::Object(o)
        }
    };
    let mut new_tokens = Map::new();
    for (k, key) in KEYS.iter().enumerate() {
        new_tokens.insert((*key).into(), json!(tokens[k] + convert::to_int(outside.get(*key))));
    }
    Ok(Plan {
        slug: slug.into(),
        name: name.into(),
        old_cost,
        new_cost: total_cost + f(outside.get("cost_usd")),
        outside,
        cost_is_estimate: estimate,
        tokens: Value::Object(new_tokens),
        usage_totals: running.get("usage_totals").and_then(Value::as_object).cloned().unwrap_or_default(),
        changes,
        unbased,
        turns,
        missing_streams,
    })
}

/// Copy a file into the backup, keeping its access and modification times. Once per file.
fn backup(path: &Path, root: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let home = am_home();
    let rel = path.strip_prefix(&home).unwrap_or(path);
    let target = root.join(rel);
    if target.exists() {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(path, &target)?;
    let meta = fs::metadata(path)?;
    let times = fs::FileTimes::new().set_accessed(meta.accessed()?).set_modified(meta.modified()?);
    fs::OpenOptions::new().write(true).open(&target)?.set_times(times)?;
    Ok(())
}

fn apply(plan: &Plan, root: &Path) -> Result<()> {
    let (slug, name) = (plan.slug.as_str(), plan.name.as_str());
    // The agent first: it holds what a rerun needs, so stopping part way is safe.
    backup(&agent_dir(slug, name).join("agent.json"), root)?;
    store::update_agent(slug, name, |agent| {
        agent["cost_usd"] = convert::float_value(plan.new_cost);
        agent["tokens"] = plan.tokens.clone();
        if plan.cost_is_estimate {
            agent["cost_is_estimate"] = json!(true);
        }
        // Keep totals the runner recorded for sessions this replay didn't see.
        let mut totals = agent.get("usage_totals").and_then(Value::as_object).cloned().unwrap_or_default();
        for (k, v) in &plan.usage_totals {
            totals.insert(k.clone(), v.clone());
        }
        agent["usage_totals"] = Value::Object(totals);
        agent["usage_outside_turns"] = plan.outside.clone();
        agent["cost_accounting"] = json!(2);
    })?;
    for (turn_id, base) in &plan.unbased {
        let Some(t) = plan.turns.iter().find(|t| &t.id == turn_id) else { continue };
        let meta_path = t.dir.join("meta.json");
        let Some(mut meta) = read(&meta_path) else { continue };
        if meta.get("usage_base").is_some() {
            continue;
        }
        backup(&meta_path, root)?;
        meta["usage_base"] = base.clone();
        write_json(&meta_path, &meta)?;
    }
    for change in &plan.changes {
        let turn = &plan.turns[change.turn];
        let result_path = turn.dir.join("result.json");
        backup(&result_path, root)?;
        let mut result = read(&result_path).unwrap_or(json!({}));
        result["cost_usd"] = convert::float_value(change.cost_usd);
        result["tokens"] = change.tokens.clone();
        write_json(&result_path, &result)?;
        let note_path = agent_dir(slug, name).join("notes").join(format!("{}.json", turn.id));
        if let Some(mut note) = read(&note_path) {
            backup(&note_path, root)?;
            note["cost_usd"] = convert::float_value(change.cost_usd);
            note["tokens"] = change.tokens.clone();
            write_json(&note_path, &note)?;
        }
    }
    Ok(())
}

fn needs_write(plan: &Plan) -> Result<bool> {
    let agent = store::get_agent(&plan.slug, &plan.name)?;
    let tokens = agent.get("tokens").filter(|t| am_core::format::truthy(Some(t))).cloned().unwrap_or(json!({}));
    let accounted = agent.get("cost_accounting").is_some_and(|v| same_value(v, &json!(2)));
    let totals = agent.get("usage_totals").filter(|t| am_core::format::truthy(Some(t))).cloned().unwrap_or(json!({}));
    Ok(!plan.changes.is_empty()
        || !plan.unbased.is_empty()
        || (plan.old_cost - plan.new_cost).abs() > 1e-9
        || !same_value(&tokens, &plan.tokens)
        || !accounted
        || plan.usage_totals.iter().any(|(k, v)| !totals.get(k).is_some_and(|t| same_value(t, v))))
}

/// Recompute every agent's costs. Returns old and new totals per project and agent.
pub fn run(dry_run: bool) -> Result<Value> {
    let cfg = am_core::config::load()?;
    let stamp = chrono::Local::now().format("cost-fix-%Y%m%d-%H%M%S").to_string();
    let backup_root = am_home().join("backups").join(stamp);
    let mut report = json!({"projects": {}, "backup": null, "dry_run": dry_run});
    for slug in store::list_slugs()? {
        let (mut old, mut new) = (0.0, 0.0);
        let mut agents = Map::new();
        for name in store::agent_names(&slug)? {
            let plan = plan_agent(&slug, &name, &cfg)?;
            old += plan.old_cost;
            new += plan.new_cost;
            let outside = f(plan.outside.get("cost_usd"));
            agents.insert(
                name.clone(),
                json!({
                    "old": convert::float_value(plan.old_cost),
                    "new": convert::float_value(plan.new_cost),
                    "turns_changed": plan.changes.len(),
                    "old_matched_turns": outside < 1e-6,
                    "outside": convert::float_value(outside),
                    "missing_streams": plan.missing_streams,
                }),
            );
            if !dry_run && needs_write(&plan)? {
                apply(&plan, &backup_root)?;
                report["backup"] = json!(backup_root.to_string_lossy());
            }
        }
        report["projects"][slug.as_str()] = json!({"old": convert::float_value(old), "new": convert::float_value(new), "agents": agents});
    }
    if !dry_run {
        write_json(&daemon_dir().join(MARKER), &json!({"at": crate::sys::wall(), "backup": report["backup"]}))?;
    }
    Ok(report)
}

pub fn needed() -> Result<bool> {
    Ok(!daemon_dir().join(MARKER).exists())
}

fn money(v: &Value) -> String {
    format!("${:.2}", f(Some(v)))
}

pub fn format_report(report: &Value) -> Result<String> {
    let mut lines = Vec::new();
    let (mut old_all, mut new_all) = (0.0, 0.0);
    if let Some(Value::Object(projects)) = report.get("projects") {
        for (slug, project) in projects {
            old_all += f(project.get("old"));
            new_all += f(project.get("new"));
            lines.push(format!("{slug}: {} -> {}", money(&project["old"]), money(&project["new"])));
            if let Some(Value::Object(agents)) = project.get("agents") {
                for (name, agent) in agents {
                    let mut flags = Vec::new();
                    if am_core::format::truthy(agent.get("turns_changed")) {
                        flags.push(format!("{} turns changed", convert::text(&agent["turns_changed"])));
                    }
                    if !am_core::format::truthy(agent.get("old_matched_turns")) {
                        flags.push(format!("{} not from any turn on disk, kept", money(&agent["outside"])));
                    }
                    if am_core::format::truthy(agent.get("missing_streams")) {
                        flags.push(format!("{} turns without a stream kept as they were", convert::text(&agent["missing_streams"])));
                    }
                    let extra = if flags.is_empty() { String::new() } else { format!("  ({})", flags.join(", ")) };
                    lines.push(format!("  {name}: {} -> {}{extra}", money(&agent["old"]), money(&agent["new"])));
                }
            }
        }
    }
    lines.push(format!("all projects: ${old_all:.2} -> ${new_all:.2}"));
    if am_core::format::truthy(report.get("dry_run")) {
        lines.push("Dry run: nothing was changed.".into());
    } else if am_core::format::truthy(report.get("backup")) {
        lines.push(format!("Changed files were backed up to {}", convert::text(&report["backup"])));
    } else {
        lines.push("Nothing needed changing.".into());
    }
    Ok(lines.join("\n"))
}
