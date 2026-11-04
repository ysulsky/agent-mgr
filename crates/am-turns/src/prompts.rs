//! System prompts for each role, and the prompt that starts a
//! turn.
//!
//! All the text an agent reads lives in `src/am/prompts/` (see its README.md), read at
//! runtime from `am_core::assets::prompts_dir()`, and is loaded with `render`.

use crate::pyv;
use am_core::{am_home, project_dir, Error, Result, Value, HOME_SLUG};
use am_store::{mail, memory};
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

fn placeholder_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap())
}

fn template(name: &str) -> Result<String> {
    let path = am_core::assets::prompts_dir().join(format!("{name}.md"));
    Ok(std::fs::read_to_string(path)?)
}

/// The template prompts/<name>.md with each `{key}` replaced by its value, without its
/// final newline. A placeholder with no value is an error. Only `{` + identifier + `}`
/// is a placeholder: other braces and `$` are plain text, and values are inserted as
/// they are, never read as templates themselves.
pub fn render_kv(name: &str, values: &[(&str, &str)]) -> Result<String> {
    let text = template(name)?;
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in placeholder_re().captures_iter(text) {
        let whole = caps.get(0).unwrap();
        let key = &caps[1];
        let Some((_, value)) = values.iter().find(|(k, _)| *k == key) else {
            return Err(Error::Other(format!("'prompts/{name}.md needs a value for {{{key}}}'")));
        };
        out.push_str(&text[last..whole.start()]);
        out.push_str(value);
        last = whole.end();
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// `prompts.render(name, **values)` with the values as a JSON object, each inserted as
/// `str(value)`.
pub fn render(name: &str, values: &Value) -> Result<String> {
    let owned: Vec<(String, String)> = values.as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), pyv::str_of(v))).collect()).unwrap_or_default();
    let pairs: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    render_kv(name, &pairs)
}

fn s(v: Option<&Value>) -> String {
    v.map(pyv::str_of).unwrap_or_else(|| "None".into())
}

/// The instructions. Without `memory_index`, what a live process must still match to
/// take another turn: memory changes are told to it turn by turn instead.
pub fn system_prompt(project: &Value, agent: &Value, memory_index: bool) -> Result<String> {
    let slug = pyv::str_of(&project["slug"]);
    let parts: Vec<&str> = if agent.get("role").and_then(Value::as_str) == Some("manager") {
        if slug == HOME_SLUG {
            vec!["common", "home", "manager"]
        } else {
            vec!["common", "manager"]
        }
    } else {
        vec!["common", "worker"]
    };
    let purpose = if am_core::py::truthy(agent.get("purpose")) {
        render_kv("roles/worker-purpose", &[("purpose", &pyv::str_of(&agent["purpose"]))])?
    } else {
        String::new()
    };
    let name = s(project.get("name"));
    let agent_name = s(agent.get("name"));
    let pdir = project_dir(&slug).to_string_lossy().into_owned();
    let home = am_home().to_string_lossy().into_owned();
    let values = [
        ("name", name.as_str()),
        ("slug", slug.as_str()),
        ("agent", agent_name.as_str()),
        ("purpose", purpose.as_str()),
        ("project_dir", pdir.as_str()),
        ("am_home", home.as_str()),
    ];
    // Each role file is a whole document, final newline included.
    let mut sections = Vec::new();
    for p in parts {
        sections.push(render_kv(&format!("roles/{p}"), &values)? + "\n");
    }
    // The memory index is here rather than in each turn's prompt, so it's cached with
    // the rest of the instructions. A change to memory changes the instructions, and
    // each session then caches its context again on its next turn.
    let index = if memory_index { memory::index_text()? } else { String::new() };
    if !index.is_empty() {
        sections.push(render_kv("roles/memory", &[("index", &safe_str(&index))])? + "\n");
    }
    Ok(sections.join("\n\n"))
}

/// `datetime.fromtimestamp(ts).strftime("%Y-%m-%d %H:%M")`, in the local zone.
pub fn time_text(ts: f64) -> String {
    use chrono::TimeZone;
    // fromtimestamp rounds to the microsecond, half to even.
    let mut secs = ts.trunc();
    let mut us = round_half_even((ts - secs) * 1e6);
    if us >= 1e6 {
        secs += 1.0;
        us -= 1e6;
    } else if us < 0.0 {
        secs -= 1.0;
        us += 1e6;
    }
    match chrono::Local.timestamp_opt(secs as i64, (us as u32) * 1000) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t.format("%Y-%m-%d %H:%M").to_string(),
        chrono::LocalResult::None => String::new(),
    }
}

fn round_half_even(x: f64) -> f64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 {
        2.0 * (x / 2.0).round()
    } else {
        r
    }
}

fn ts_of(v: Option<&Value>) -> f64 {
    match v {
        None => 0.0,
        Some(v) => pyv::py_float(v),
    }
}

fn block_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)<(/?)(\s*am-)").unwrap())
}

/// `_safe`: whatever an agent or a file supplies can't form a block boundary, and so
/// can't pose as another sender. Everything dynamic in a prompt goes through here.
pub fn safe(v: Option<&Value>) -> String {
    match v {
        Some(v) if am_core::py::truthy(Some(v)) => safe_str(&pyv::str_of(v)),
        _ => String::new(),
    }
}

pub fn safe_str(text: &str) -> String {
    block_tag_re().replace_all(text, |c: &regex::Captures| format!("<\\{}{}", &c[1], &c[2])).into_owned()
}

/// `_attr`: a value that goes in a block's attribute.
pub fn attr(v: Option<&Value>) -> String {
    s(v).replace('"', "'").replace(['<', '>'], "")
}

pub fn render_message(slug: &str, message: &Value) -> Result<String> {
    let mut lines: Vec<String> = Vec::new();
    if let Some(reply) = message.get("reply_to").filter(|r| am_core::py::truthy(Some(r))) {
        let excerpt = reply.get("excerpt").cloned().unwrap_or(Value::String(String::new()));
        lines.push(render_kv("turn/message-reply", &[("id", &safe(reply.get("id"))), ("excerpt", &safe(Some(&excerpt)))])?);
    }
    if let Some(comment) = message.get("comment").filter(|c| am_core::py::truthy(Some(c))) {
        let artifact = safe(comment.get("artifact"));
        if am_core::py::truthy(comment.get("rev")) {
            lines.push(render_kv("turn/message-comment-at-revision", &[("artifact", &artifact), ("rev", &safe(comment.get("rev")))])?);
        } else {
            lines.push(render_kv("turn/message-comment", &[("artifact", &artifact)])?);
        }
        let quote = comment.get("quote").cloned().unwrap_or(Value::String(String::new()));
        lines.push(format!("> {}", safe(Some(&quote)).replace('\n', "\n> ")));
    }
    let meta = message.get("meta").filter(|m| am_core::py::truthy(Some(m))).cloned().unwrap_or(Value::Null);
    if message.get("kind").and_then(Value::as_str) == Some("fyi") {
        match meta.get("fyi").and_then(Value::as_str) {
            Some("user-to-worker") => lines.push(render_kv("turn/message-fyi-user-to-worker", &[("worker", &safe(meta.get("worker")))])?),
            Some("worker-answer") => lines.push(render_kv("turn/message-fyi-worker-answer", &[("worker", &safe(meta.get("worker")))])?),
            _ => {}
        }
    }
    let text = message.get("text").cloned().unwrap_or(Value::String(String::new()));
    lines.push(safe(Some(&text)));
    if let Some(Value::Array(attachments)) = message.get("attachments") {
        for attachment in attachments {
            let path = project_dir(slug).join(pyv::str_of(&attachment["path"]));
            lines.push(render_kv("turn/message-attachment", &[("path", &safe_str(&path.to_string_lossy()))])?);
        }
    }
    render_kv(
        "turn/message",
        &[
            ("id", &attr(message.get("id"))),
            ("sender", &attr(message.get("from"))),
            ("kind", &attr(message.get("kind"))),
            ("time", &time_text(ts_of(message.get("ts")))),
            ("body", &lines.join("\n")),
        ],
    )
}

pub fn turn_prompt(slug: &str, agent: &Value, messages: &[Value], notes: &[String], side: bool, history: &str) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    let manager = agent.get("role").and_then(Value::as_str) == Some("manager");
    if side {
        parts.push(render_kv("turn/side-turn", &[])?);
    }
    if !notes.is_empty() {
        let lines: Vec<String> = notes.iter().map(|n| format!("- {}", safe_str(n))).collect();
        parts.push(render_kv("turn/notes", &[("notes", &lines.join("\n"))])?);
    }
    if !history.is_empty() {
        parts.push(safe_str(history));
    }
    if manager && !side {
        let in_turn: HashSet<String> = messages.iter().map(|m| pyv::str_of(&m["id"])).collect();
        let recent = recent_questions(slug, &pyv::str_of(&agent["name"]), &in_turn)?;
        if !recent.is_empty() {
            parts.push(recent);
        }
    }
    for m in messages {
        parts.push(render_message(slug, m)?);
    }
    if manager && !side {
        if let Some(target) = mail::answer_target(messages) {
            let count = messages.iter().filter(|m| m.get("kind").and_then(Value::as_str) == Some("user")).count();
            let template = if count > 1 { "turn/answer-card-several" } else { "turn/answer-card" };
            parts.push(render_kv(template, &[("id", &pyv::str_of(&target["id"]))])?);
        } else {
            parts.push(render_kv("turn/no-user-message", &[])?);
        }
    }
    Ok(parts.join("\n\n") + "\n")
}

pub const RECENT_QUESTIONS: usize = 3;
pub const RECENT_QUESTION_CHARS: usize = 1500;

/// The user's last few messages to this agent in full, answered or not, so a message
/// sent in parts reads as one. Those that are also in this turn are only named.
pub fn recent_questions(slug: &str, name: &str, in_turn: &HashSet<String>) -> Result<String> {
    let all = mail::user_message_ids(slug, name)?;
    let ids = &all[all.len().saturating_sub(RECENT_QUESTIONS)..];
    if ids.iter().all(|q| in_turn.contains(q)) {
        return Ok(String::new()); // Nothing that isn't already below.
    }
    let mut blocks: Vec<String> = Vec::new();
    for question in ids {
        let Some(message) = mail::get_message(slug, name, question)? else { continue };
        if !am_core::py::truthy(Some(&message)) {
            continue;
        }
        let id = attr(Some(&Value::String(question.clone())));
        let time = time_text(ts_of(message.get("ts")));
        if in_turn.contains(question) {
            blocks.push(render_kv("turn/recent-question-new", &[("id", &id), ("time", &time)])?);
            continue;
        }
        let raw = message.get("text").map(pyv::str_of).unwrap_or_default();
        let text = am_core::py::strip(&raw);
        let cut = text.chars().count() > RECENT_QUESTION_CHARS;
        let head: String = text.chars().take(RECENT_QUESTION_CHARS).collect();
        let mut lines: Vec<String> = safe_str(&head).split('\n').map(|line| format!("> {line}")).collect();
        if cut {
            lines.push(render_kv("turn/recent-question-cut", &[("limit", &RECENT_QUESTION_CHARS.to_string()), ("id", &id)])?);
        }
        let text = lines.join("\n");
        let version = mail::latest_answer_version(slug, name, question)?;
        if version != 0 {
            let version = version.to_string();
            blocks.push(render_kv("turn/recent-question", &[("version", &version), ("id", &id), ("time", &time), ("text", &text)])?);
        } else {
            blocks.push(render_kv("turn/recent-question-unanswered", &[("id", &id), ("time", &time), ("text", &text)])?);
        }
    }
    if blocks.is_empty() {
        return Ok(String::new());
    }
    render_kv("turn/recent-questions", &[("questions", &blocks.join("\n\n"))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_is_strict_and_literal() {
        let text = render_kv("notes/failed", &[("turn_id", "t-1"), ("error", "{turn_id} $x")]).unwrap();
        assert!(text.contains("(t-1)") && text.contains("{turn_id} $x"));
        assert!(render_kv("notes/failed", &[("error", "boom")]).unwrap_err().to_string().contains("turn_id"));
    }

    #[test]
    fn safe_breaks_block_tags() {
        assert_eq!(safe_str("a <am-message> </AM-x> < am-y"), "a <\\am-message> <\\/AM-x> <\\ am-y");
    }
}
