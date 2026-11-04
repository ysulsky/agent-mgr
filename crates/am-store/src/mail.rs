//! Messages, versioned answers and the chat timeline.
//!
//! A mailbox is a directory with one file per message, so senders need no lock:
//! `inbox/<id>.json` is written by atomic rename, and claiming moves it to
//! `delivered/<id>.json`. Answers are `answers/<question id>/v<N>.json`, numbered
//! under a flock because both a manager (`am answer`) and the runner may publish.

use crate::projects;
use crate::util::{self, get, has, now_value};
use am_core::{
    agent_dir, am_err, answers_dir, check_item_id, delivered_dir, excerpt, first_line, inbox_dir, list_json_ids, locked,
    new_id, poke, project_dir, read_json, turn_dir, update_json, write_json, Result, Value, MANAGER,
};
use serde_json::{json, Map};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// Kinds a busy worker answers right away on a side turn.
pub const SIDE_TURN_KINDS: [&str; 2] = ["user", "agent"];

/// The keyword arguments of `mail.send`, with Python's defaults (`Default`).
#[derive(Clone, Debug)]
pub struct SendOpts {
    /// `sender="user"`
    pub sender: String,
    /// `kind=None`: "user" from the user, else "agent".
    pub kind: Option<String>,
    /// `attachments=None` (a list of dicts).
    pub attachments: Option<Vec<Value>>,
    pub reply_to: Option<Value>,
    pub comment: Option<Value>,
    pub interrupt: bool,
    /// `wake=True`
    pub wake: bool,
    pub no_side: bool,
    pub msg_id: Option<String>,
    pub meta: Option<Value>,
    pub allow_retired: bool,
}

impl Default for SendOpts {
    fn default() -> Self {
        SendOpts {
            sender: "user".into(),
            kind: None,
            attachments: None,
            reply_to: None,
            comment: None,
            interrupt: false,
            wake: true,
            no_side: false,
            msg_id: None,
            meta: None,
            allow_retired: false,
        }
    }
}

impl SendOpts {
    /// `sender=...`, everything else default.
    pub fn from(sender: impl Into<String>) -> Self {
        SendOpts { sender: sender.into(), ..Default::default() }
    }
}

/// Port of `mail.send`.
pub fn send(slug: &str, name: &str, text: &str, opts: SendOpts) -> Result<Value> {
    let agent = projects::get_agent(slug, name)?;
    if has(&agent, "retired") && !opts.allow_retired {
        return am_err(format!("agent {slug}/{name} is retired"));
    }
    let kind = opts.kind.clone().unwrap_or_else(|| if opts.sender == "user" { "user".into() } else { "agent".into() });
    let msg_id = opts.msg_id.clone().filter(|m| !m.is_empty()).unwrap_or_else(|| new_id("m"));
    let attachments = opts.attachments.clone().unwrap_or_default();
    let meta = opts.meta.clone().filter(|m| am_core::py::truthy(Some(m))).unwrap_or_else(util::empty);
    let message = json!({
        "id": msg_id,
        "project": slug,
        "to": name,
        "from": opts.sender,
        "kind": kind,
        "text": text,
        "ts": now_value(),
        "wake": opts.wake,
        "interrupt": opts.interrupt,
        "no_side": opts.no_side,
        "attachments": attachments,
        "reply_to": opts.reply_to.clone().unwrap_or(Value::Null),
        "comment": opts.comment.clone().unwrap_or(Value::Null),
        "meta": meta,
    });
    if message_exists(slug, name, &msg_id) {
        return Ok(message); // Deterministic ids make redelivery after a crash harmless.
    }
    write_json(&inbox_dir(slug, name).join(format!("{msg_id}.json")), &message)?;
    if kind == "user" && get(&agent, "role").and_then(Value::as_str) == Some("worker") {
        copy_to_manager(slug, name, &message)?;
    }
    poke(slug);
    Ok(message)
}

/// `mail._copy_to_manager`: the user may talk to any agent; the worker's manager gets an
/// FYI copy.
fn copy_to_manager(slug: &str, worker: &str, message: &Value) -> Result<()> {
    let id = util::s(message, "id");
    send(
        slug,
        MANAGER,
        message["text"].as_str().unwrap_or_default(),
        SendOpts {
            sender: format!("{slug}/{worker}"),
            kind: Some("fyi".into()),
            wake: false,
            msg_id: Some(format!("{id}-f")),
            meta: Some(json!({"fyi": "user-to-worker", "worker": worker, "original": id})),
            ..Default::default()
        },
    )?;
    Ok(())
}

/// `mail._exists`.
fn message_exists(slug: &str, name: &str, msg_id: &str) -> bool {
    util::exists(&inbox_dir(slug, name).join(format!("{msg_id}.json")))
        || util::exists(&delivered_dir(slug, name).join(format!("{msg_id}.json")))
}

/// `m.get("ts", 0)` as a number for sorting.
fn ts_of(m: &Value) -> f64 {
    util::num(get(m, "ts")).unwrap_or(0.0)
}

/// Port of `mail.pending`: inbox messages, interrupting ones first, then oldest first.
pub fn pending(slug: &str, name: &str) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for mid in list_json_ids(&inbox_dir(slug, name))? {
        if let Some(message) = read_json(&inbox_dir(slug, name).join(format!("{mid}.json")))? {
            if am_core::py::truthy(Some(&message)) {
                out.push(message);
            }
        }
    }
    // A stable sort, as Python's.
    out.sort_by(|a, b| {
        let key = |m: &Value| (!has(m, "interrupt"), ts_of(m));
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0).then(ka.1.partial_cmp(&kb.1).unwrap_or(std::cmp::Ordering::Equal))
    });
    Ok(out)
}

fn unlink_if_there(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Port of `mail.claim`.
pub fn claim(slug: &str, name: &str, msg_id: &str, turn_id: &str) -> Result<Option<Value>> {
    let src = inbox_dir(slug, name).join(format!("{msg_id}.json"));
    let Some(mut message) = read_json(&src)? else { return Ok(None) };
    let f = util::fields(&mut message);
    f.insert("claimed_by".into(), turn_id.into());
    f.insert("claimed_at".into(), now_value());
    write_json(&delivered_dir(slug, name).join(format!("{msg_id}.json")), &message)?;
    unlink_if_there(&src)?;
    Ok(Some(message))
}

/// Port of `mail.requeue(slug, name, msg_id, **changes)`: move a claimed message back to
/// the inbox, for a retry. `changes` is a dict (or null for none).
pub fn requeue(slug: &str, name: &str, msg_id: &str, changes: &Value) -> Result<()> {
    let src = delivered_dir(slug, name).join(format!("{msg_id}.json"));
    let Some(mut message) = read_json(&src)? else { return Ok(()) };
    let f = util::fields(&mut message);
    f.shift_remove("claimed_by");
    f.shift_remove("claimed_at");
    util::update(&mut message, changes);
    write_json(&inbox_dir(slug, name).join(format!("{msg_id}.json")), &message)?;
    unlink_if_there(&src)
}

/// Port of `mail.mark_interrupt`: upgrade a queued message so it interrupts the running
/// main turn.
pub fn mark_interrupt(slug: &str, name: &str, msg_id: &str) -> Result<bool> {
    check_item_id(msg_id)?;
    let path = inbox_dir(slug, name).join(format!("{msg_id}.json"));
    let Some(mut message) = read_json(&path)? else { return Ok(false) };
    util::fields(&mut message).insert("interrupt".into(), true.into());
    write_json(&path, &message)?;
    poke(slug);
    Ok(true)
}

/// Port of `mail.answer_target`: the user's message a turn's final reply answers, the
/// newest of them (the first, of equally new ones).
pub fn answer_target(messages: &[Value]) -> Option<Value> {
    let mut best: Option<&Value> = None;
    for m in messages.iter().filter(|m| get(m, "kind").and_then(Value::as_str) == Some("user")) {
        if best.is_none_or(|b| ts_of(m) > ts_of(b)) {
            best = Some(m);
        }
    }
    best.cloned()
}

/// Port of `mail.get_message`.
pub fn get_message(slug: &str, name: &str, msg_id: &str) -> Result<Option<Value>> {
    check_item_id(msg_id)?;
    let delivered = read_json(&delivered_dir(slug, name).join(format!("{msg_id}.json")))?;
    if am_core::py::truthy(delivered.as_ref()) {
        return Ok(delivered);
    }
    read_json(&inbox_dir(slug, name).join(format!("{msg_id}.json")))
}

// ---------------------------------------------------------------- answers

/// The versions in an answer directory, as `int(n[1:-5])` of each `v*.json`. `None` for a
/// missing directory; an error, as Python's ValueError, for a name `int` refuses.
fn version_numbers(directory: &Path) -> Result<Option<Vec<i64>>> {
    let names = match std::fs::read_dir(directory) {
        Ok(_) => am_core::fsx::listdir(directory)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for n in names {
        if n.starts_with('v') && n.ends_with(".json") {
            let inner = if n.len() >= 6 { &n[1..n.len() - 5] } else { "" };
            match am_core::py::parse_int(inner) {
                Some(v) => out.push(v),
                None => return util::crash(format!("invalid literal for int() with base 10: {}", util::repr(inner))),
            }
        }
    }
    Ok(Some(out))
}

/// Port of `mail.answer_versions`.
pub fn answer_versions(slug: &str, name: &str, question: &str) -> Result<Vec<Value>> {
    let directory = answers_dir(slug, name).join(question);
    let Some(mut versions) = version_numbers(&directory)? else { return Ok(vec![]) };
    versions.sort();
    versions.iter().map(|v| Ok(read_json(&directory.join(format!("v{v}.json")))?.unwrap_or(Value::Null))).collect()
}

/// Port of `mail.latest_answer_version`.
pub fn latest_answer_version(slug: &str, name: &str, question: &str) -> Result<i64> {
    let directory = answers_dir(slug, name).join(question);
    Ok(version_numbers(&directory)?.and_then(|v| v.into_iter().max()).unwrap_or(0))
}

/// Port of `mail.publish_answer`: publish the next version of the answer to a user's
/// question.
pub fn publish_answer(slug: &str, name: &str, question: &str, text: &str, turn: Option<&str>) -> Result<Value> {
    let question = question.split('@').next().unwrap_or_default();
    let Some(message) = get_message(slug, name, question)? else {
        return am_err(format!("{question} is not a message to {slug}/{name}; see `am questions`"));
    };
    if get(&message, "kind").and_then(Value::as_str) != Some("user") {
        return am_err(format!(
            "{question} is a {} message, not a question from the user",
            am_core::py::str_of(get(&message, "kind"))?
        ));
    }
    if am_core::py::strip(text).is_empty() {
        return am_err("an answer cannot be empty");
    }
    let directory = answers_dir(slug, name).join(question);
    let answer;
    {
        let _lock = locked(&directory.join("versions"))?;
        let version = latest_answer_version(slug, name, question)? + 1;
        answer = json!({
            "id": format!("{question}@v{version}"),
            "question": question,
            "version": version,
            "text": text,
            "ts": now_value(),
            "turn": turn,
            "agent": name,
        });
        write_json(&directory.join(format!("v{version}.json")), &answer)?;
    }
    poke(slug);
    Ok(answer)
}

/// Port of `mail.answered_in_turn`.
pub fn answered_in_turn(slug: &str, name: &str, question: &str, turn: &str) -> Result<bool> {
    Ok(answer_versions(slug, name, question)?.iter().any(|a| get(a, "turn").and_then(Value::as_str) == Some(turn)))
}

// ---------------------------------------------------------------- reading items

/// Port of `mail.write_note`.
pub fn write_note(slug: &str, name: &str, turn_id: &str, note: &Value) -> Result<()> {
    write_json(&agent_dir(slug, name).join("notes").join(format!("{turn_id}.json")), note)?;
    poke(slug);
    Ok(())
}

/// Port of `mail.find_item`: a message `m-...`, answer version `m-...@v2`, note or turn
/// `t-...`, or attention item `a-...`. Searches `slug` first, then every project.
pub fn find_item(item_id: &str, slug: Option<&str>) -> Result<Option<Value>> {
    let base = item_id.split_once('@').map_or(item_id, |p| p.0);
    if !am_core::is_item_id(base) {
        return Ok(None);
    }
    let slug = slug.filter(|s| !s.is_empty());
    let candidates: Vec<String> = match slug {
        Some(s) => std::iter::once(s.to_string()).chain(projects::list_slugs()?.into_iter().filter(|x| x != s)).collect(),
        None => projects::list_slugs()?,
    };
    for project in candidates {
        if let Some(found) = find_in_project(item_id, &project)? {
            if am_core::py::truthy(Some(&found)) {
                return Ok(Some(found));
            }
        }
    }
    Ok(None)
}

fn find_in_project(item_id: &str, slug: &str) -> Result<Option<Value>> {
    if item_id.starts_with("a-") {
        let item = read_json(&project_dir(slug).join("attention").join(format!("{item_id}.json")))?;
        let Some(item) = item.filter(|i| am_core::py::truthy(Some(i))) else { return Ok(None) };
        let mut merged = Map::new();
        merged.insert("type".into(), "attention".into());
        merged.insert("project".into(), slug.into());
        if let Value::Object(fields) = item {
            for (k, v) in fields {
                merged.insert(k, v);
            }
        } else {
            return util::crash("'attention item' object is not a mapping");
        }
        return Ok(Some(Value::Object(merged)));
    }
    let (base, version) = item_id.split_once('@').unwrap_or((item_id, ""));
    for name in projects::agent_names(slug)? {
        if base.starts_with("t-") {
            let note = read_json(&agent_dir(slug, &name).join("notes").join(format!("{base}.json")))?;
            let meta = read_json(&turn_dir(slug, &name, base).join("meta.json"))?;
            if am_core::py::truthy(note.as_ref()) || am_core::py::truthy(meta.as_ref()) {
                let result = read_json(&turn_dir(slug, &name, base).join("result.json"))?
                    .filter(|r| am_core::py::truthy(Some(r)))
                    .unwrap_or_else(util::empty);
                return Ok(Some(json!({
                    "type": "turn",
                    "project": slug,
                    "agent": name,
                    "id": base,
                    "note": note.unwrap_or(Value::Null),
                    "meta": meta.unwrap_or(Value::Null),
                    "result": result,
                })));
            }
            continue;
        }
        let Some(message) = get_message(slug, &name, base)? else { continue };
        let answers = answer_versions(slug, &name, base)?;
        if !version.is_empty() {
            let wanted = version.trim_start_matches('v');
            let Some(answer) = answers.iter().find(|a| am_core::py::str_of(get(a, "version")).ok().as_deref() == Some(wanted))
            else {
                return Ok(None);
            };
            let Value::Object(fields) = answer.clone() else { return util::crash("'answer' object is not a mapping") };
            let mut merged = fields;
            merged.insert("type".into(), "answer".into());
            merged.insert("project".into(), slug.into());
            merged.insert("agent".into(), name.as_str().into());
            merged.insert("question".into(), message);
            return Ok(Some(Value::Object(merged)));
        }
        return Ok(Some(json!({
            "type": "message",
            "project": slug,
            "agent": name,
            "message": message,
            "answers": answers,
        })));
    }
    Ok(None)
}

/// `item[key]` as an f-string prints it.
fn sv(v: &Value, key: &str) -> String {
    util::s(v, key)
}

/// A text field `"\n".join` would take: a string, else Python's TypeError.
fn text_field(v: &Value, key: &str) -> Result<String> {
    match get(v, key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => util::crash(format!("sequence item: expected str instance, {} found", type_name(other))),
        None => util::crash(format!("KeyError: {}", util::repr(key))),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Port of `mail.render_item`: plain text for `am show`.
pub fn render_item(item: &Value) -> Result<String> {
    let kind = get(item, "type").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "answer" => {
            let q = get(item, "question").cloned().unwrap_or(Value::Null);
            let qtext = get(&q, "text").and_then(Value::as_str).unwrap_or_default();
            Ok(format!(
                "Answer {} ({}/{})\nQuestion {}: {}\n\n{}\n",
                sv(item, "id"),
                sv(item, "project"),
                sv(item, "agent"),
                sv(&q, "id"),
                excerpt(qtext, 300),
                sv(item, "text")
            ))
        }
        "message" => {
            let empty = util::empty();
            let message = get(item, "message").unwrap_or(&empty);
            let mut lines = vec![format!(
                "Message {} from {} to {}/{} ({})",
                sv(message, "id"),
                sv(message, "from"),
                sv(item, "project"),
                sv(item, "agent"),
                sv(message, "kind")
            )];
            if has(message, "reply_to") {
                lines.push(format!("In reply to {}", sv(&message["reply_to"], "id")));
            }
            if has(message, "comment") {
                let c = &message["comment"];
                let rev = if has(c, "rev") { format!("@{}", sv(c, "rev")) } else { String::new() };
                lines.push(format!("Comment on {}{rev}", sv(c, "artifact")));
                lines.push(format!("Passage: {}", sv(c, "quote")));
            }
            lines.push(String::new());
            lines.push(text_field(message, "text")?);
            if let Some(Value::Array(attachments)) = get(message, "attachments") {
                for a in attachments {
                    let path = if has(a, "abspath") { get(a, "abspath") } else { get(a, "path") };
                    lines.push(format!("Attachment: {}", am_core::py::str_of(path)?));
                }
            }
            if let Some(Value::Array(answers)) = get(item, "answers") {
                if let Some(latest) = answers.last() {
                    lines.push(String::new());
                    lines.push(format!("Latest answer {} of {}:", sv(latest, "id"), answers.len()));
                    lines.push(String::new());
                    lines.push(text_field(latest, "text")?);
                }
            }
            Ok(lines.join("\n") + "\n")
        }
        "turn" => {
            let empty = util::empty();
            let note = util::or(item, "note", &empty);
            let result = util::or(item, "result", &empty);
            let mut lines = vec![format!("Turn {} of {}/{}", sv(item, "id"), sv(item, "project"), sv(item, "agent"))];
            if let Some(Value::Array(triggers)) = get(note, "triggers") {
                for t in triggers {
                    lines.push(format!(
                        "Prompted by {} {} from {}: {}",
                        sv(t, "kind"),
                        sv(t, "id"),
                        sv(t, "from"),
                        sv(t, "excerpt")
                    ));
                }
            }
            let none = Value::String("(no reply yet)".into());
            let text = [get(result, "final"), get(note, "full"), get(note, "text")]
                .into_iter()
                .flatten()
                .find(|v| am_core::py::truthy(Some(v)))
                .unwrap_or(&none);
            let Some(text) = text.as_str() else {
                return util::crash(format!("can only concatenate str (not \"{}\") to str", type_name(text)));
            };
            Ok(lines.join("\n") + "\n\n" + text + "\n")
        }
        "attention" => {
            let mut lines = vec![
                format!("Attention {} ({}): {}", sv(item, "id"), sv(item, "project"), sv(item, "title")),
                format!("State: {}", sv(item, "state")),
            ];
            if has(item, "body") {
                lines.push(String::new());
                lines.push(text_field(item, "body")?);
            }
            if has(item, "run") {
                lines.push(String::new());
                lines.push(format!("Fix command: {}", sv(item, "run")));
            }
            Ok(lines.join("\n") + "\n")
        }
        _ => Ok(am_core::py::repr_value(item) + "\n"),
    }
}

// ---------------------------------------------------------------- the chat timeline

/// `mail._kind_cache`: a message's kind never changes, so it's read once per process.
fn kind_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<String, String>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn kind_of(directory: &Path, msg_id: &str) -> Result<String> {
    if let Some(kind) = kind_cache().lock().unwrap().get(msg_id) {
        return Ok(kind.clone());
    }
    let message = read_json(&directory.join(format!("{msg_id}.json")))?;
    let kind = match message.as_ref().and_then(|m| get(m, "kind")) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => am_core::py::str_value(other),
        None => String::new(),
    };
    if am_core::py::truthy(message.as_ref()) {
        kind_cache().lock().unwrap().insert(msg_id.to_string(), kind.clone());
    }
    Ok(kind)
}

/// Port of `mail.user_message_ids`.
pub fn user_message_ids(slug: &str, name: &str) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for directory in [delivered_dir(slug, name), inbox_dir(slug, name)] {
        for mid in list_json_ids(&directory)? {
            if kind_of(&directory, &mid)? == "user" {
                ids.push(mid);
            }
        }
    }
    ids.sort();
    Ok(ids)
}

/// `p[0][2:]`: an id without its prefix.
fn tail(id: &str) -> &str {
    id.get(2..).unwrap_or("")
}

/// `read_json(project_dir(slug) / "ui.json", {}).get("seen", {})`.
fn seen_versions(slug: &str) -> Result<Value> {
    let ui = read_json(&project_dir(slug).join("ui.json"))?.unwrap_or_else(util::empty);
    Ok(get(&ui, "seen").cloned().unwrap_or_else(util::empty))
}

/// Port of `mail.chat`: the items of an agent's chat, oldest first. `before` pages
/// backwards; `since` returns everything from that id on.
pub fn chat(slug: &str, name: &str, limit: usize, before: Option<&str>, since: Option<&str>) -> Result<Value> {
    let questions = user_message_ids(slug, name)?;
    let notes = list_json_ids(&agent_dir(slug, name).join("notes"))?;
    let active = projects::active_turns(slug, name)?;
    let active_ids: Vec<String> = active.iter().filter_map(|t| get(t, "id").and_then(Value::as_str)).map(String::from).collect();
    let mut merged: Vec<(String, bool)> =
        questions.iter().map(|q| (q.clone(), true)).chain(notes.iter().map(|n| (n.clone(), false))).collect();
    merged.sort_by(|a, b| tail(&a.0).cmp(tail(&b.0)));
    if let Some(before) = before.filter(|b| !b.is_empty()) {
        merged.retain(|p| tail(&p.0) < tail(before));
    }
    let since = since.filter(|s| !s.is_empty());
    if let Some(since) = since {
        merged.retain(|p| tail(&p.0) >= tail(since));
    }
    let mut has_more = false;
    if since.is_none() && merged.len() > limit {
        has_more = true;
        merged = merged.split_off(merged.len() - limit);
    }
    let seen = seen_versions(slug)?;
    let mut items = Vec::new();
    for (item_id, is_question) in merged {
        if is_question {
            items.push(question_item(slug, name, &item_id, &active_ids, &seen)?);
        } else if let Some(note) = read_json(&agent_dir(slug, name).join("notes").join(format!("{item_id}.json")))? {
            if am_core::py::truthy(Some(&note)) {
                let mut merged = Map::new();
                merged.insert("type".into(), "note".into());
                if let Value::Object(fields) = note {
                    for (k, v) in fields {
                        merged.insert(k, v);
                    }
                }
                items.push(Value::Object(merged));
            }
        }
    }
    let mut running = Vec::new();
    for turn in &active {
        let messages = get(turn, "messages").and_then(Value::as_array).cloned().unwrap_or_default();
        if !messages.iter().any(|m| m.as_str().is_some_and(|m| questions.iter().any(|q| q == m))) {
            running.push(json!({
                "type": "running",
                "id": turn.get("id").cloned().unwrap_or(Value::Null),
                "kind": get(turn, "kind").cloned().unwrap_or(Value::Null),
                "started": get(turn, "created").cloned().unwrap_or(Value::Null),
                "triggers": get(turn, "triggers").cloned().unwrap_or_else(|| json!([])),
                "stopping": get(turn, "stopping").cloned().unwrap_or(false.into()),
            }));
        }
    }
    Ok(json!({"items": items, "running": running, "has_more": has_more}))
}

fn question_item(slug: &str, name: &str, msg_id: &str, active: &[String], seen: &Value) -> Result<Value> {
    let message = get_message(slug, name, msg_id)?
        .filter(|m| am_core::py::truthy(Some(m)))
        .unwrap_or_else(|| json!({"id": msg_id, "text": "", "ts": 0}));
    let in_inbox = util::exists(&inbox_dir(slug, name).join(format!("{msg_id}.json")));
    let answers = answer_versions(slug, name, msg_id)?;
    let turn_id = get(&message, "claimed_by").cloned().unwrap_or(Value::Null);
    let status = if in_inbox {
        Value::from("queued")
    } else if turn_id.as_str().is_some_and(|t| active.iter().any(|a| a == t)) {
        Value::from("working")
    } else if !answers.is_empty() {
        Value::from("answered")
    } else {
        let result = match turn_id.as_str().filter(|t| !t.is_empty()) {
            Some(t) => read_json(&turn_dir(slug, name, t).join("result.json"))?,
            None => None,
        };
        let empty = util::empty();
        let result = result.filter(|r| am_core::py::truthy(Some(r))).unwrap_or(empty);
        util::or(&result, "state", &Value::from("delivered")).clone()
    };
    let field = |k: &str| get(&message, k).cloned().unwrap_or(Value::Null);
    Ok(json!({
        "type": "question",
        "id": msg_id,
        "from": field("from"),
        "ts": field("ts"),
        "text": get(&message, "text").cloned().unwrap_or_else(|| "".into()),
        "attachments": util::or(&message, "attachments", &json!([])).clone(),
        "reply_to": field("reply_to"),
        "comment": field("comment"),
        "status": status,
        "turn": turn_id,
        "interrupting": has(&message, "interrupt"),
        "answers": answers,
        "seen": get(seen, msg_id).cloned().unwrap_or(0.into()),
    }))
}

/// Port of `mail.unread_summary`: answers the user hasn't seen, new ones (`unread`) and new
/// versions of ones they have (`updated`), per agent.
pub fn unread_summary(slug: &str) -> Result<Value> {
    let seen = seen_versions(slug)?;
    let mut per_agent = Map::new();
    let (mut total_unread, mut total_updated) = (0, 0);
    for name in projects::agent_names(slug)? {
        let mut unread = 0;
        let mut updated: Vec<(String, i64)> = Vec::new();
        for question in am_core::fsx::listdir(&answers_dir(slug, &name))? {
            if !question.starts_with("m-") {
                continue;
            }
            let latest = latest_answer_version(slug, &name, &question)?;
            let have = util::num(get(&seen, &question)).unwrap_or(0.0);
            if latest != 0 && have == 0.0 {
                unread += 1;
            } else if latest as f64 > have {
                updated.push((question, latest));
            }
        }
        updated.sort();
        total_unread += unread;
        total_updated += updated.len();
        let updated: Vec<Value> = updated.into_iter().map(|(q, v)| json!({"question": q, "version": v})).collect();
        per_agent.insert(name, json!({"unread": unread, "updated": updated}));
    }
    Ok(json!({"unread": total_unread, "updated": total_updated, "agents": per_agent}))
}

/// Port of `mail.mark_seen`: `versions` maps question ids to the version seen.
pub fn mark_seen(slug: &str, versions: &Value) -> Result<()> {
    update_json(&project_dir(slug).join("ui.json"), util::empty(), |ui| {
        let f = util::fields(ui);
        let seen = f.entry("seen").or_insert_with(util::empty);
        let seen = util::fields(seen);
        if let Some(versions) = versions.as_object() {
            for (question, version) in versions {
                let have = util::num(seen.get(question)).unwrap_or(0.0);
                if util::num(Some(version)).is_some_and(|v| v > have) {
                    seen.insert(question.clone(), version.clone());
                }
            }
        }
        None
    })?;
    poke(slug);
    Ok(())
}

/// Port of `mail.describe_trigger`.
pub fn describe_trigger(message: &Value) -> Value {
    json!({
        "id": get(message, "id").cloned().unwrap_or(Value::Null),
        "kind": get(message, "kind").cloned().unwrap_or(Value::Null),
        "from": get(message, "from").cloned().unwrap_or(Value::Null),
        "excerpt": excerpt(get(message, "text").and_then(Value::as_str).unwrap_or_default(), 160),
    })
}

/// Port of `mail.note_line`.
pub fn note_line(text: &str) -> String {
    first_line(text, 200)
}
