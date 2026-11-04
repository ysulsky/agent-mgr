//! Tasks, attention items and schedules: the manager's per-project records.

use crate::localtime;
use crate::projects;
use crate::util::{self, get, has, now_value, repr};
use am_core::{
    am_err, attention_dir, check_item_id, format_duration, list_json_ids, new_id, parse_duration, poke, project_dir,
    read_json, update_json, write_json, Result, Value,
};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::PathBuf;

// ---------------------------------------------------------------- tasks

pub const TASK_STATES: [&str; 4] = ["todo", "doing", "blocked", "done"];

fn tasks_path(slug: &str) -> PathBuf {
    project_dir(slug).join("tasks.json")
}

fn tasks_default() -> Value {
    json!({"next": 1, "items": []})
}

/// Port of `items.list_tasks`.
pub fn list_tasks(slug: &str, include_done: bool) -> Result<Vec<Value>> {
    let data = read_json(&tasks_path(slug))?.unwrap_or_else(tasks_default);
    let Some(Value::Array(tasks)) = get(&data, "items") else {
        return util::crash(format!("{} has no list of items", tasks_path(slug).display()));
    };
    Ok(if include_done {
        tasks.clone()
    } else {
        tasks.iter().filter(|t| get(t, "status").and_then(Value::as_str) != Some("done")).cloned().collect()
    })
}

/// Port of `items.add_task`. Python's defaults: agent None, notes "", status "todo".
pub fn add_task(slug: &str, title: &str, agent: Option<&str>, notes: &str, status: &str) -> Result<Value> {
    if !TASK_STATES.contains(&status) {
        return am_err(format!("unknown task status {}; use {}", repr(status), TASK_STATES.join(", ")));
    }
    let mut created = Value::Null;
    update_json(&tasks_path(slug), tasks_default(), |data| {
        let id = get(data, "next").cloned().unwrap_or(Value::Null);
        created = json!({
            "id": id,
            "title": title,
            "status": status,
            "agent": agent,
            "notes": notes,
            "created": now_value(),
            "updated": now_value(),
        });
        let f = util::fields(data);
        let next = f.get("next").and_then(Value::as_i64).unwrap_or(0) + 1;
        f.insert("next".into(), next.into());
        if let Some(Value::Array(items)) = f.get_mut("items") {
            items.push(created.clone());
        }
        None
    })?;
    poke(slug);
    Ok(created)
}

/// Port of `items.update_task(slug, task_id, **fields)`: `fields` is a dict; null values
/// are left alone, as Python skips `None`.
pub fn update_task(slug: &str, task_id: i64, fields: &Value) -> Result<Value> {
    if let Some(status) = get(fields, "status").filter(|s| am_core::py::truthy(Some(s))) {
        let status = am_core::py::str_value(status);
        if !TASK_STATES.contains(&status.as_str()) {
            return am_err(format!("unknown task status {}; use {}", repr(&status), TASK_STATES.join(", ")));
        }
    }
    let mut found: Option<Value> = None;
    update_json(&tasks_path(slug), tasks_default(), |data| {
        if let Some(Value::Array(items)) = util::fields(data).get_mut("items") {
            for task in items.iter_mut() {
                if get(task, "id").and_then(Value::as_i64) == Some(task_id) {
                    if let Some(changes) = fields.as_object() {
                        let t = util::fields(task);
                        for (k, v) in changes {
                            if !v.is_null() {
                                t.insert(k.clone(), v.clone());
                            }
                        }
                    }
                    util::fields(task).insert("updated".into(), now_value());
                    found = Some(task.clone());
                    break;
                }
            }
        }
        None
    })?;
    let Some(found) = found.filter(|f| am_core::py::truthy(Some(f))) else {
        return am_err(format!("no task {task_id} in project {}", repr(slug)));
    };
    poke(slug);
    Ok(found)
}

// ---------------------------------------------------------------- attention

pub const ATTENTION_KINDS: [&str; 3] = ["action", "alert", "result"];

/// The keyword arguments of `items.add_attention`, with Python's defaults.
#[derive(Clone, Debug, Default)]
pub struct AttentionOpts {
    /// `body=""`
    pub body: String,
    /// `kind="action"` when `None`.
    pub kind: Option<String>,
    pub run: Option<String>,
    pub r#ref: Option<String>,
    /// `source=""`
    pub source: String,
    pub key: Option<String>,
}

/// Port of `items.add_attention`: add an item for the user. With `key`, an open item with
/// that key is reused so repeated checks don't pile up.
pub fn add_attention(slug: &str, title: &str, opts: AttentionOpts) -> Result<Value> {
    let kind = opts.kind.as_deref().unwrap_or("action");
    if !ATTENTION_KINDS.contains(&kind) {
        return am_err(format!("unknown attention kind {}; use {}", repr(kind), ATTENTION_KINDS.join(", ")));
    }
    if let Some(key) = opts.key.as_deref().filter(|k| !k.is_empty()) {
        for item in list_attention(slug, false)? {
            if get(&item, "key").and_then(Value::as_str) == Some(key) {
                return Ok(item);
            }
        }
    }
    let item = json!({
        "id": new_id("a"),
        "project": slug,
        "title": title,
        "body": opts.body,
        "kind": kind,
        "run": opts.run,
        "ref": opts.r#ref,
        "source": opts.source,
        "key": opts.key,
        "state": "new",
        "created": now_value(),
    });
    write_json(&attention_dir(slug).join(format!("{}.json", util::s(&item, "id"))), &item)?;
    poke(slug);
    Ok(item)
}

/// Port of `items.list_attention`.
pub fn list_attention(slug: &str, include_resolved: bool) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for item_id in list_json_ids(&attention_dir(slug))? {
        if let Some(item) = read_json(&attention_dir(slug).join(format!("{item_id}.json")))? {
            if am_core::py::truthy(Some(&item))
                && (include_resolved || get(&item, "state").and_then(Value::as_str) != Some("resolved"))
            {
                out.push(item);
            }
        }
    }
    Ok(out)
}

/// Port of `items.get_attention`.
pub fn get_attention(slug: &str, item_id: &str) -> Result<Value> {
    check_item_id(item_id)?;
    match read_json(&attention_dir(slug).join(format!("{item_id}.json")))? {
        Some(item) => Ok(item),
        None => am_err(format!("no attention item {item_id} in project {}", repr(slug))),
    }
}

/// `items._update_attention(slug, item_id, **fields)`.
fn update_attention(slug: &str, item_id: &str, fields: Value) -> Result<Value> {
    get_attention(slug, item_id)?;
    let result = update_json(&attention_dir(slug).join(format!("{item_id}.json")), Value::Null, |d| {
        util::update(d, &fields);
        None
    })?;
    poke(slug);
    Ok(result)
}

/// Port of `items.mark_attention_seen`.
pub fn mark_attention_seen(slug: &str, item_id: &str) -> Result<Value> {
    let item = get_attention(slug, item_id)?;
    if get(&item, "state").and_then(Value::as_str) != Some("new") {
        return Ok(item);
    }
    update_attention(slug, item_id, json!({"state": "seen", "seen_at": now_value()}))
}

/// Port of `items.resolve_attention`.
pub fn resolve_attention(slug: &str, item_id: &str, by: &str) -> Result<Value> {
    update_attention(slug, item_id, json!({"state": "resolved", "resolved_at": now_value(), "resolved_by": by}))
}

/// Port of `items.resolve_attention_key`.
pub fn resolve_attention_key(slug: &str, key: &str, by: &str) -> Result<()> {
    for item in list_attention(slug, false)? {
        if get(&item, "key").and_then(Value::as_str) == Some(key) {
            resolve_attention(slug, &util::s(&item, "id"), by)?;
        }
    }
    Ok(())
}

/// Port of `items.attention_counts`: `2 need you` while everything is new, `1 new · 2 open`
/// while part is, and `2 open` once all of it has been looked at.
pub fn attention_counts(items: &[Value]) -> Value {
    let total = items.len();
    let new = items.iter().filter(|i| get(i, "state").and_then(Value::as_str) == Some("new")).count();
    let label = if total == 0 {
        String::new()
    } else if new == total {
        format!("{total} need{} you", if total == 1 { "s" } else { "" })
    } else if new > 0 {
        format!("{new} new · {total} open")
    } else {
        format!("{total} open")
    };
    json!({"open": total, "new": new, "label": label})
}

// ---------------------------------------------------------------- schedules

fn schedules_path(slug: &str) -> PathBuf {
    project_dir(slug).join("schedules.json")
}

/// Port of `items.list_schedules`: a dict of name to entry, in file order.
pub fn list_schedules(slug: &str) -> Result<Value> {
    Ok(read_json(&schedules_path(slug))?.unwrap_or_else(util::empty))
}

/// The keyword arguments of `items.set_schedule`.
#[derive(Clone, Debug, Default)]
pub struct ScheduleOpts {
    pub every: Option<String>,
    pub cron: Option<String>,
    pub to: Option<String>,
    pub prompt: Option<String>,
    pub enabled: Option<bool>,
}

/// Port of `items.set_schedule`.
pub fn set_schedule(slug: &str, name: &str, opts: ScheduleOpts) -> Result<Value> {
    let every = opts.every.as_deref().filter(|e| !e.is_empty());
    let cron = opts.cron.as_deref().filter(|c| !c.is_empty());
    if every.is_some() && cron.is_some() {
        return am_err("use either --every or --cron, not both");
    }
    if let Some(every) = every {
        if parse_duration(every)? < 60.0 {
            return am_err("the shortest schedule is 1m");
        }
    }
    if let Some(cron) = cron {
        parse_cron(cron)?;
    }
    let mut result = Value::Null;
    util::try_update_json(&schedules_path(slug), util::empty(), |data| {
        let existing = get(data, name).filter(|e| am_core::py::truthy(Some(e))).cloned();
        let mut entry =
            existing.unwrap_or_else(|| json!({"name": name, "to": "manager", "enabled": true, "created": now_value()}));
        let f = util::fields(&mut entry);
        if let Some(every) = every {
            f.insert("every".into(), format_duration(parse_duration(every)?).into());
            f.shift_remove("cron");
        }
        if let Some(cron) = cron {
            f.insert("cron".into(), cron.into());
            f.shift_remove("every");
        }
        if let Some(to) = opts.to.as_deref().filter(|t| !t.is_empty()) {
            f.insert("to".into(), to.into());
        }
        if let Some(prompt) = &opts.prompt {
            f.insert("prompt".into(), prompt.as_str().into());
        }
        if let Some(enabled) = opts.enabled {
            f.insert("enabled".into(), enabled.into());
        }
        if !(has(&entry, "every") || has(&entry, "cron")) {
            return am_err("a new schedule needs --every or --cron");
        }
        if !has(&entry, "prompt") {
            return am_err("a new schedule needs a prompt");
        }
        util::fields(data).insert(name.into(), entry.clone());
        result = entry;
        Ok(())
    })?;
    let to = util::s(&result, "to");
    if !projects::agent_exists(slug, &to) {
        return am_err(format!("schedule saved, but agent {} doesn't exist in {} yet", repr(&to), repr(slug)));
    }
    poke(slug);
    Ok(result)
}

/// Port of `items.remove_schedule`.
pub fn remove_schedule(slug: &str, name: &str) -> Result<()> {
    let mut found = false;
    update_json(&schedules_path(slug), util::empty(), |data| {
        if util::fields(data).shift_remove(name).is_some() {
            found = true;
        }
        None
    })?;
    if !found {
        return am_err(format!("no schedule {} in project {}", repr(name), repr(slug)));
    }
    poke(slug);
    Ok(())
}

/// A parsed five-field cron: the allowed minutes, hours, days, months and weekdays
/// (Sunday 0; a 7 is folded into 0).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cron {
    pub minute: BTreeSet<u32>,
    pub hour: BTreeSet<u32>,
    pub day: BTreeSet<u32>,
    pub month: BTreeSet<u32>,
    pub weekday: BTreeSet<u32>,
}

const CRON_NAMES: [(&str, i64); 19] = [
    ("jan", 1), ("feb", 2), ("mar", 3), ("apr", 4), ("may", 5), ("jun", 6), ("jul", 7), ("aug", 8), ("sep", 9),
    ("oct", 10), ("nov", 11), ("dec", 12), ("sun", 0), ("mon", 1), ("tue", 2), ("wed", 3), ("thu", 4), ("fri", 5),
    ("sat", 6),
];
const CRON_RANGES: [(i64, i64); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];

/// `items._cron_value`.
fn cron_value(text: &str) -> Option<i64> {
    let lower = text.to_lowercase();
    match CRON_NAMES.iter().find(|(n, _)| *n == lower) {
        Some((_, v)) => Some(*v),
        None => am_core::py::parse_int(text),
    }
}

/// `items._cron_field`: `None` where Python raises ValueError or KeyError.
fn cron_field(text: &str, low: i64, high: i64) -> Option<BTreeSet<u32>> {
    let mut values = BTreeSet::new();
    for part in text.split(',') {
        let (body, step_text) = part.split_once('/').unwrap_or((part, ""));
        let step = if step_text.is_empty() { 1 } else { am_core::py::parse_int(step_text)? };
        if step < 1 {
            return None;
        }
        let (start, end) = if body == "*" {
            (low, high)
        } else if let Some((a, b)) = body.split_once('-') {
            (cron_value(a)?, cron_value(b)?)
        } else {
            let start = cron_value(body)?;
            (start, if step_text.is_empty() { start } else { high })
        };
        if start < low || end > high || start > end {
            return None;
        }
        let mut v = start;
        while v <= end {
            values.insert(v as u32);
            v += step;
        }
    }
    Some(values)
}

/// `expression.split()`.
fn split_fields(expression: &str) -> Vec<&str> {
    expression.split(am_core::py::is_space).filter(|f| !f.is_empty()).collect()
}

/// Port of `items.parse_cron`: standard five-field cron, minute hour day month weekday.
pub fn parse_cron(expression: &str) -> Result<Cron> {
    let fields = split_fields(expression);
    if fields.len() != 5 {
        return am_err(format!("cron {} needs five fields: minute hour day month weekday", repr(expression)));
    }
    let mut parsed = Vec::new();
    for (f, (lo, hi)) in fields.iter().zip(CRON_RANGES) {
        match cron_field(f, lo, hi) {
            Some(set) => parsed.push(set),
            None => return am_err(format!("cannot parse cron {}", repr(expression))),
        }
    }
    let weekday = parsed[4].iter().map(|d| if *d == 7 { 0 } else { *d }).collect();
    Ok(Cron {
        minute: parsed[0].clone(),
        hour: parsed[1].clone(),
        day: parsed[2].clone(),
        month: parsed[3].clone(),
        weekday,
    })
}

/// Port of `items.cron_matches`, for a naive local datetime (see `localtime`).
pub fn cron_matches(parsed: &Cron, moment: i64, day_restricted: bool, weekday_restricted: bool) -> bool {
    let f = localtime::fields(moment);
    if !parsed.minute.contains(&f.minute) || !parsed.hour.contains(&f.hour) || !parsed.month.contains(&f.month) {
        return false;
    }
    let day_ok = parsed.day.contains(&f.day);
    let weekday_ok = parsed.weekday.contains(&localtime::weekday_sun0(moment));
    // As in cron: when both are restricted, either one matching is enough.
    if day_restricted && weekday_restricted {
        return day_ok || weekday_ok;
    }
    day_ok && weekday_ok
}

/// Port of `items.next_cron_fire`: the first matching local minute after `after`.
pub fn next_cron_fire(expression: &str, after: f64) -> Result<f64> {
    let parsed = parse_cron(expression)?;
    let fields = split_fields(expression);
    let (day_restricted, weekday_restricted) = (fields[2] != "*", fields[4] != "*");
    let start = localtime::from_timestamp(after);
    let mut moment = start - start.rem_euclid(60) + 60;
    for _ in 0..366 * 24 * 60 {
        if cron_matches(&parsed, moment, day_restricted, weekday_restricted) {
            return Ok(localtime::timestamp(moment) as f64);
        }
        moment += 60;
    }
    am_err(format!("cron {} never fires", repr(expression)))
}

/// Port of `items.next_fire`.
pub fn next_fire(entry: &Value, last: f64) -> Result<f64> {
    if has(entry, "cron") {
        return next_cron_fire(&util::s(entry, "cron"), last);
    }
    let every = get(entry, "every").cloned().unwrap_or(Value::Null);
    Ok(last + am_core::clock::parse_duration_value(&every)?)
}

/// Port of `items.project_paths_exist`.
pub fn project_paths_exist(slug: &str) -> bool {
    project_dir(slug).is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_parses_as_python_does() {
        let c = parse_cron("*/15 9-17 * jan-mar mon-fri").unwrap();
        assert_eq!(c.minute.iter().copied().collect::<Vec<_>>(), vec![0, 15, 30, 45]);
        assert_eq!(c.hour.len(), 9);
        assert_eq!(c.month.iter().copied().collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(c.weekday.iter().copied().collect::<Vec<_>>(), vec![1, 2, 3, 4, 5]);
        assert_eq!(parse_cron("0 0 * * 7").unwrap().weekday.iter().copied().collect::<Vec<_>>(), vec![0]);
        assert_eq!(parse_cron("5/20 * * * *").unwrap().minute.iter().copied().collect::<Vec<_>>(), vec![5, 25, 45]);
        assert!(parse_cron("60 * * * *").is_err());
        assert!(parse_cron("* * * *").is_err());
        assert!(parse_cron("*/0 * * * *").is_err());
        assert!(parse_cron("5-1 * * * *").is_err());
        assert!(parse_cron("x * * * *").is_err());
    }

    #[test]
    fn next_cron_fire_is_a_later_matching_minute() {
        let after = 1_790_000_000.0;
        let next = next_cron_fire("*/5 * * * *", after).unwrap();
        assert!(next > after && next - after <= 300.0);
        assert_eq!(localtime::fields(localtime::local(next as i64)).minute % 5, 0);
    }
}
