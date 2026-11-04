//! The two agent CLIs, how to run a turn on each, and how to
//! read what it wrote.
//!
//! Both write JSONL to stdout. A Claude process can emit several `result` events,
//! because a background task finishing wakes it for another round, and a fork can
//! open with an empty one. So the reply is the last non-empty result.
//!
//! Usage is reported as running totals for the whole session, not for the turn:
//! Claude's `total_cost_usd` and Codex's `turn.completed` tokens both carry on from the
//! session's earlier turns when it is resumed, and a fork starts from its parent's.
//! (Claude's per-result `usage` tokens are the exception: they cover only that result.)
//! So `parse_stream` reports the running totals as `running`, and a turn's own usage is
//! the difference from the totals the session had when the turn started, `base`.
//! Codex reports tokens but not cost.

use crate::pyv;
use am_core::{am_err, json, Result, Value};
use serde_json::{json, Map};
use std::path::Path;

/// `backends.TurnSpec`.
#[derive(Clone, Debug, Default)]
pub struct TurnSpec {
    pub backend: String,
    pub model: String,
    pub system_prompt: String,
    pub am_home: String,
    pub project_dir: String,
    /// Resume this session.
    pub session: Option<String>,
    /// Fork `session` instead of continuing it.
    pub fork: bool,
    pub images: Vec<String>,
    /// A long-lived process that takes each turn as a message on stdin (see `proc`).
    pub persistent: bool,
}

/// Token counts, in the order Python's dict has them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tokens {
    pub input: i64,
    pub cached_input: i64,
    pub output: i64,
}

impl Tokens {
    pub fn to_value(self) -> Value {
        json!({"input": self.input, "cached_input": self.cached_input, "output": self.output})
    }
}

/// `backends.StreamResult`.
#[derive(Clone, Debug, Default)]
pub struct StreamResult {
    pub final_: String,
    pub session: Option<String>,
    pub cost_usd: f64,
    pub cost_is_estimate: bool,
    pub tokens: Tokens,
    pub is_error: bool,
    pub error: String,
    pub saw_result: bool,
    /// The session's running totals at the end of the turn: `cost_usd` (a float) for
    /// Claude, `input` (cached included), `cached_input` and `output` (ints) for Codex.
    pub running: Map<String, Value>,
}

pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The argv for a turn and, for a new Claude session, the id it will have. The prompt
/// goes in on stdin.
pub fn build_command(spec: &TurnSpec, cfg: &Value) -> Result<(Vec<String>, Option<String>)> {
    let conf = &cfg["backends"][spec.backend.as_str()];
    match spec.backend.as_str() {
        "claude" => Ok(claude_command(spec, conf)),
        "codex" => Ok((codex_command(spec, conf), None)),
        other => am_err(format!("unknown backend {other}")),
    }
}

fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().map(pyv::str_of).collect(),
        _ => vec![],
    }
}

fn claude_command(spec: &TurnSpec, conf: &Value) -> (Vec<String>, Option<String>) {
    let mut argv: Vec<String> = vec![pyv::str_of(&conf["bin"]), "-p".into(), "--output-format".into(), "stream-json".into(), "--verbose".into()];
    if spec.persistent {
        argv.extend(["--input-format", "stream-json", "--replay-user-messages"].map(String::from));
    }
    let mut session_id = None;
    if let Some(session) = spec.session.as_deref().filter(|s| !s.is_empty()) {
        argv.extend(["--resume".into(), session.to_string()]);
        if spec.fork {
            argv.push("--fork-session".into());
        }
    } else {
        let id = new_session_id();
        argv.extend(["--session-id".into(), id.clone()]);
        session_id = Some(id);
    }
    if !spec.model.is_empty() {
        argv.extend(["--model".into(), spec.model.clone()]);
    }
    argv.extend(["--append-system-prompt".into(), spec.system_prompt.clone()]);
    if am_core::py::truthy(conf.get("permission_mode")) {
        argv.extend(["--permission-mode".into(), pyv::str_of(&conf["permission_mode"])]);
    }
    if am_core::py::truthy(conf.get("allowed_tools")) {
        argv.extend(["--allowedTools".into(), strings(conf.get("allowed_tools")).join(",")]);
    }
    argv.extend(["--add-dir".into(), spec.am_home.clone()]);
    argv.extend(strings(conf.get("extra_args")));
    (argv, session_id)
}

/// A JSON string is a valid TOML basic string for everything we send.
fn toml_string(value: &str) -> String {
    json::dumps_compact(&Value::String(value.to_string()))
}

fn codex_command(spec: &TurnSpec, conf: &Value) -> Vec<String> {
    let mut argv: Vec<String> = vec![pyv::str_of(&conf["bin"]), "exec".into()];
    match spec.session.as_deref().filter(|s| !s.is_empty()) {
        Some(session) if spec.fork => argv.extend(["fork".into(), session.to_string()]),
        Some(session) => argv.extend(["resume".into(), session.to_string()]),
        None => {}
    }
    for image in &spec.images {
        argv.extend(["-i".into(), image.clone()]);
    }
    argv.extend(["--json".into(), "--skip-git-repo-check".into()]);
    if !spec.model.is_empty() {
        argv.extend(["-m".into(), spec.model.clone()]);
    }
    argv.extend(["-c".into(), format!("developer_instructions={}", toml_string(&spec.system_prompt))]);
    let sandbox = if am_core::py::truthy(conf.get("sandbox")) { pyv::str_of(&conf["sandbox"]) } else { "workspace-write".into() };
    argv.extend(["-c".into(), format!("sandbox_mode={}", toml_string(&sandbox)), "-c".into(), "approval_policy=\"never\"".into()]);
    if sandbox == "workspace-write" {
        let roots = json::dumps_compact(&json!([spec.am_home, spec.project_dir]));
        argv.extend(["-c".into(), format!("sandbox_workspace_write.writable_roots={roots}")]);
        let network = if conf.get("network").is_none_or(|v| am_core::py::truthy(Some(v))) { "true" } else { "false" };
        argv.extend(["-c".into(), format!("sandbox_workspace_write.network_access={network}")]);
    }
    argv.extend(strings(conf.get("extra_args")));
    argv.push("-".into());
    argv
}

/// `shutil.which(bin) is not None`, on this process's PATH.
pub fn available(backend: &str, cfg: &Value) -> bool {
    which(&pyv::str_of(&cfg["backends"][backend]["bin"])).is_some()
}

/// `shutil.which`: an executable file, given as a path or found on PATH.
pub fn which(cmd: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let ok = |p: &Path| {
        let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return false };
        let executable = unsafe { libc::access(c.as_ptr(), libc::F_OK | libc::X_OK) == 0 };
        executable && !p.is_dir()
    };
    if cmd.contains('/') {
        let p = Path::new(cmd);
        return ok(p).then(|| p.to_path_buf());
    }
    let path = std::env::var("PATH").unwrap_or_else(|_| "/bin:/usr/bin".into());
    let mut seen = std::collections::HashSet::new();
    for dir in path.split(':') {
        let dir = if dir.is_empty() { "." } else { dir };
        if !seen.insert(dir.to_string()) {
            continue;
        }
        let p = Path::new(dir).join(cmd);
        if ok(&p) {
            return Some(p);
        }
    }
    None
}

// ---------------------------------------------------------------- reading streams

/// `_lines`: each line of the file that is a JSON object, as Python reads the file in
/// text mode (universal newlines, invalid UTF-8 replaced).
pub fn lines(path: &Path) -> Vec<Map<String, Value>> {
    let Ok(bytes) = std::fs::read(path) else { return vec![] };
    let text = String::from_utf8_lossy(&bytes);
    let mut out = Vec::new();
    for line in text.split('\n').flat_map(|l| l.split('\r')) {
        let line = am_core::py::strip(line);
        if line.is_empty() {
            continue;
        }
        if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(line) {
            out.push(obj);
        }
    }
    out
}

/// What a turn wrote. `base` is the session's running totals when the turn started
/// (see `StreamResult::running`), or None for a new session.
pub fn parse_stream(backend: &str, path: &Path, pricing: Option<&Value>, model: &str, base: Option<&Value>) -> StreamResult {
    let empty = json!({});
    let base = base.filter(|b| am_core::py::truthy(Some(b))).unwrap_or(&empty);
    if backend == "claude" {
        parse_claude(path, base)
    } else {
        let pricing = pricing.filter(|p| am_core::py::truthy(Some(p))).unwrap_or(&empty);
        parse_codex(path, pricing, model, base)
    }
}

/// A turn's share of a running total. A total below the base means the counter started
/// again from zero, so all of it is this turn's.
fn since(total: f64, base: f64) -> f64 {
    if total >= base {
        total - base
    } else {
        total
    }
}

fn get<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    obj.get(key)
}

fn parse_claude(path: &Path, base: &Value) -> StreamResult {
    let mut out = StreamResult::default();
    let mut last_text = String::new();
    for obj in lines(path) {
        let kind = get(&obj, "type").and_then(Value::as_str).unwrap_or("");
        if kind == "system" && get(&obj, "subtype").and_then(Value::as_str) == Some("init") {
            if out.session.as_deref().is_none_or(str::is_empty) {
                out.session = pyv::opt_str(get(&obj, "session_id"));
            }
        } else if kind == "assistant" {
            let content = get(&obj, "message").and_then(|m| m.get("content"));
            let mut text = String::new();
            if let Some(Value::Array(blocks)) = content {
                for c in blocks {
                    if c.get("type").and_then(Value::as_str) == Some("text") {
                        text.push_str(c.get("text").and_then(Value::as_str).unwrap_or(""));
                    }
                }
            }
            if !am_core::py::strip(&text).is_empty() {
                last_text = text;
            }
        } else if kind == "result" {
            out.saw_result = true;
            if let Some(s) = pyv::opt_str(get(&obj, "session_id")).filter(|s| !s.is_empty()) {
                out.session = Some(s);
            }
            if let Some(cost) = get(&obj, "total_cost_usd").filter(|v| !v.is_null()) {
                out.running.insert("cost_usd".into(), pyv::float_value(pyv::py_float(cost)));
            }
            let usage = get(&obj, "usage").cloned().unwrap_or(Value::Null);
            let n = |k: &str| pyv::py_int(usage.get(k));
            out.tokens.input += n("input_tokens") + n("cache_creation_input_tokens");
            out.tokens.cached_input += n("cache_read_input_tokens");
            out.tokens.output += n("output_tokens");
            let text = pyv::or_str(get(&obj, "result"));
            let is_error = am_core::py::truthy(get(&obj, "is_error"));
            // The opening empty result of a resumed session says nothing.
            if !am_core::py::strip(&text).is_empty() || is_error {
                out.is_error = is_error;
                if is_error {
                    out.error = if !text.is_empty() {
                        text
                    } else if am_core::py::truthy(get(&obj, "errors")) {
                        crate::sys::dumps_ascii(&obj["errors"])
                    } else if am_core::py::truthy(get(&obj, "subtype")) {
                        pyv::str_of(&obj["subtype"])
                    } else {
                        "error".into()
                    };
                } else {
                    out.error = String::new();
                    out.final_ = text;
                }
            }
        }
    }
    if out.final_.is_empty() && !out.is_error {
        out.final_ = last_text;
    }
    if let Some(cost) = out.running.get("cost_usd").and_then(Value::as_f64) {
        let b = if am_core::py::truthy(base.get("cost_usd")) { pyv::py_float(&base["cost_usd"]) } else { 0.0 };
        out.cost_usd = pyv::py_max(since(cost, b), 0.0);
    }
    out
}

fn parse_codex(path: &Path, pricing: &Value, model: &str, base: &Value) -> StreamResult {
    let mut out = StreamResult::default();
    let mut last_text = String::new();
    let prices = pricing.get(model).filter(|p| am_core::py::truthy(Some(p))).cloned().unwrap_or(json!({}));
    for obj in lines(path) {
        let kind = get(&obj, "type").and_then(Value::as_str).unwrap_or("");
        if kind == "thread.started" {
            if out.session.as_deref().is_none_or(str::is_empty) {
                out.session = pyv::opt_str(get(&obj, "thread_id"));
            }
        } else if kind == "item.completed" {
            let item = get(&obj, "item").cloned().unwrap_or(Value::Null);
            let text = pyv::or_str(item.get("text"));
            if item.get("type").and_then(Value::as_str) == Some("agent_message") && !am_core::py::strip(&text).is_empty() {
                last_text = text;
            }
        } else if kind == "turn.completed" {
            out.saw_result = true;
            let usage = get(&obj, "usage").cloned().unwrap_or(Value::Null);
            let n = |k: &str| pyv::py_int(usage.get(k));
            out.running = Map::new();
            out.running.insert("input".into(), n("input_tokens").into());
            out.running.insert("cached_input".into(), n("cached_input_tokens").into());
            out.running.insert("output".into(), n("output_tokens").into());
        } else if kind == "turn.failed" || kind == "error" {
            out.is_error = true;
            out.error = codex_error_text(&obj, kind);
        }
    }
    if !out.running.is_empty() {
        let b = |k: &str| pyv::py_int(base.get(k));
        let r = |k: &str| out.running.get(k).and_then(Value::as_i64).unwrap_or(0);
        // One running total started again means they all did.
        let reset = ["input", "cached_input", "output"].iter().any(|k| r(k) < b(k));
        let d = |k: &str| r(k) - if reset { 0 } else { b(k) };
        let (total_in, cached, output) = (d("input"), d("cached_input"), d("output"));
        out.tokens = Tokens { input: (total_in - cached).max(0), cached_input: cached, output };
        if am_core::py::truthy(Some(&prices)) {
            let price = |k: &str, default: f64| prices.get(k).map_or(default, pyv::py_float);
            let input_price = price("input", 0.0);
            out.cost_is_estimate = true;
            out.cost_usd = ((total_in - cached).max(0) as f64 * input_price
                + cached as f64 * price("cached_input", input_price)
                + output as f64 * price("output", 0.0))
                / 1_000_000.0;
        }
    }
    out.final_ = last_text;
    // A reconnect `error` followed by a completed turn is not a failure.
    if out.saw_result && !out.final_.is_empty() {
        out.is_error = false;
        out.error = String::new();
    }
    out
}

/// `str(message or error or kind)` for a codex `turn.failed` or `error` event.
fn codex_error_text(obj: &Map<String, Value>, kind: &str) -> String {
    let error = get(obj, "error");
    let message = match error {
        Some(Value::Object(e)) => e.get("message"),
        _ => get(obj, "message"),
    };
    if am_core::py::truthy(message) {
        pyv::str_of(message.unwrap())
    } else if am_core::py::truthy(error) {
        pyv::str_of(error.unwrap())
    } else {
        kind.to_string()
    }
}

/// The session of a running turn. Codex only says once it has started.
pub fn peek_session(backend: &str, path: &Path) -> Option<String> {
    for obj in lines(path) {
        if backend == "codex" && get(&obj, "type").and_then(Value::as_str) == Some("thread.started") {
            return pyv::opt_str(get(&obj, "thread_id"));
        }
        if backend == "claude" && am_core::py::truthy(get(&obj, "session_id")) {
            return pyv::opt_str(get(&obj, "session_id"));
        }
    }
    None
}

// ---------------------------------------------------------------- the turn viewer

fn clip(text: &Value, limit: usize) -> String {
    let text = match text {
        Value::String(s) => s.clone(),
        other => json::dumps_compact(other),
    };
    let n = text.chars().count();
    if n <= limit {
        text
    } else {
        let head: String = text.chars().take(limit).collect();
        format!("{head}\n… {} more characters", n - limit)
    }
}

fn tool_summary(tool_input: &Value) -> String {
    for key in ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt"] {
        if am_core::py::truthy(tool_input.get(key)) {
            return clip(&tool_input[key], 400);
        }
    }
    clip(tool_input, 400)
}

/// Reduce a raw stream line to events for the turn view.
pub fn normalize_events(backend: &str, obj: &Value) -> Vec<Value> {
    if backend == "claude" {
        normalize_claude(obj)
    } else {
        normalize_codex(obj)
    }
}

fn or_empty_object(v: Option<&Value>) -> Value {
    match v {
        Some(v) if am_core::py::truthy(Some(v)) => v.clone(),
        _ => json!({}),
    }
}

fn normalize_claude(obj: &Value) -> Vec<Value> {
    let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
    let mut out = Vec::new();
    match kind {
        "assistant" => {
            if let Some(Value::Array(blocks)) = obj.get("message").and_then(|m| m.get("content")) {
                for block in blocks {
                    if !block.is_object() {
                        continue;
                    }
                    let btype = block.get("type").and_then(Value::as_str);
                    let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                    if btype == Some("text") && !am_core::py::strip(text).is_empty() {
                        out.push(json!({"k": "text", "text": block["text"]}));
                    } else if btype == Some("tool_use") {
                        let name = block.get("name").cloned().unwrap_or(json!("tool"));
                        out.push(json!({"k": "tool", "name": name, "summary": tool_summary(&or_empty_object(block.get("input")))}));
                    }
                }
            }
        }
        "user" => {
            if let Some(Value::Array(blocks)) = obj.get("message").and_then(|m| m.get("content")) {
                for block in blocks {
                    if block.is_object() && block.get("type").and_then(Value::as_str) == Some("tool_result") {
                        let mut body = block.get("content").cloned().unwrap_or(Value::Null);
                        if let Value::Array(parts) = &body {
                            let texts: Vec<&str> = parts
                                .iter()
                                .filter(|b| b.is_object())
                                .map(|b| b.get("text").and_then(Value::as_str).unwrap_or(""))
                                .collect();
                            body = Value::String(texts.join("\n"));
                        }
                        let body = if am_core::py::truthy(Some(&body)) { body } else { json!("") };
                        out.push(json!({"k": "output", "text": clip(&body, 4000), "error": am_core::py::truthy(block.get("is_error"))}));
                    }
                }
            }
        }
        "result" => {
            let text = match obj.get("result") {
                Some(v) if am_core::py::truthy(Some(v)) => v.clone(),
                _ => json!(""),
            };
            let is_error = am_core::py::truthy(obj.get("is_error"));
            if am_core::py::strip(text.as_str().unwrap_or("x")).is_empty() && !is_error {
                return vec![];
            }
            out.push(json!({"k": "result", "text": text, "error": is_error, "cost": obj.get("total_cost_usd").cloned().unwrap_or(Value::Null)}));
        }
        "system" if obj.get("subtype").and_then(Value::as_str) == Some("init") => {
            out.push(json!({"k": "init", "model": obj.get("model").cloned().unwrap_or(Value::Null), "mode": obj.get("permissionMode").cloned().unwrap_or(Value::Null)}));
        }
        _ => {}
    }
    out
}

fn normalize_codex(obj: &Value) -> Vec<Value> {
    let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
    let item = or_empty_object(obj.get("item"));
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    if kind == "item.started" && item_type == "command_execution" {
        let command = item.get("command").cloned().unwrap_or(json!(""));
        return vec![json!({"k": "tool", "name": "shell", "summary": clip(&command, 400)})];
    }
    if kind == "item.started" && matches!(item_type, "mcp_tool_call" | "web_search" | "file_change") {
        let rest: Map<String, Value> =
            item.as_object().map(|m| m.iter().filter(|(k, _)| !matches!(k.as_str(), "id" | "type" | "status")).map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        return vec![json!({"k": "tool", "name": item_type, "summary": clip(&Value::Object(rest), 400)})];
    }
    if kind == "item.completed" {
        let text = pyv::or_str(item.get("text"));
        if item_type == "agent_message" && !am_core::py::strip(&text).is_empty() {
            return vec![json!({"k": "text", "text": item["text"]})];
        }
        if item_type == "command_execution" {
            let output = match item.get("aggregated_output") {
                Some(v) if am_core::py::truthy(Some(v)) => v.clone(),
                _ => json!(""),
            };
            let error = match item.get("exit_code") {
                None | Some(Value::Null) | Some(Value::Bool(false)) => false,
                Some(Value::Number(n)) => n.as_f64() != Some(0.0),
                Some(_) => true,
            };
            return vec![json!({"k": "output", "text": clip(&output, 4000), "error": error})];
        }
    }
    if kind == "turn.completed" {
        return vec![json!({"k": "usage", "usage": or_empty_object(obj.get("usage"))})];
    }
    if kind == "turn.failed" || kind == "error" {
        let map = obj.as_object().cloned().unwrap_or_default();
        return vec![json!({"k": "result", "text": codex_error_text(&map, kind), "error": true})];
    }
    vec![]
}

/// Normalized events from a byte offset on, and the offset to continue from. Stops at
/// the last complete line, so a line being written is read next time. Python's default
/// `max_bytes` is 2,000,000.
pub fn read_events(backend: &str, path: &Path, offset: u64, max_bytes: u64) -> (Vec<Value>, u64) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return (vec![], offset) };
    let mut data = Vec::new();
    if f.seek(SeekFrom::Start(offset)).is_err() || f.take(max_bytes).read_to_end(&mut data).is_err() {
        return (vec![], offset);
    }
    let Some(end) = data.iter().rposition(|b| *b == b'\n') else { return (vec![], offset) };
    let mut events = Vec::new();
    // bytes.splitlines(): \n, \r\n and \r.
    let chunk = &data[..end + 1];
    let mut start = 0;
    let mut i = 0;
    let mut lines: Vec<&[u8]> = Vec::new();
    while i < chunk.len() {
        match chunk[i] {
            b'\n' => {
                lines.push(&chunk[start..i]);
                start = i + 1;
            }
            b'\r' => {
                lines.push(&chunk[start..i]);
                if chunk.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    for line in lines {
        if let Ok(obj @ Value::Object(_)) = serde_json::from_slice::<Value>(line) {
            events.extend(normalize_events(backend, &obj));
        }
    }
    (events, offset + end as u64 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_cost_is_the_growth_of_the_running_total() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(
            &path,
            "{\"type\": \"system\", \"subtype\": \"init\", \"session_id\": \"s1\"}\n\
             {\"type\": \"result\", \"result\": \"\", \"total_cost_usd\": 0.5, \"usage\": {}}\n\
             {\"type\": \"assistant\", \"message\": {\"content\": [{\"type\": \"text\", \"text\": \"hi\"}]}}\n\
             {\"type\": \"result\", \"result\": \"done\", \"session_id\": \"s1\", \"total_cost_usd\": 0.53, \"usage\": {\"input_tokens\": 10, \"cache_read_input_tokens\": 5, \"output_tokens\": 2}}\n",
        )
        .unwrap();
        let r = parse_stream("claude", &path, None, "", Some(&json!({"cost_usd": 0.5})));
        assert_eq!(r.final_, "done");
        assert_eq!(r.session.as_deref(), Some("s1"));
        assert_eq!(r.tokens, Tokens { input: 10, cached_input: 5, output: 2 });
        assert_eq!(r.cost_usd, 0.53 - 0.5);
        assert_eq!(json::dumps_compact(&Value::Object(r.running)), "{\"cost_usd\": 0.53}");
    }

    #[test]
    fn codex_tokens_and_estimate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(
            &path,
            "{\"type\": \"thread.started\", \"thread_id\": \"th\"}\n\
             {\"type\": \"item.completed\", \"item\": {\"type\": \"agent_message\", \"text\": \"ok\"}}\n\
             {\"type\": \"turn.completed\", \"usage\": {\"input_tokens\": 200, \"cached_input_tokens\": 80, \"output_tokens\": 20}}\n",
        )
        .unwrap();
        let pricing = json!({"m": {"input": 1.25, "cached_input": 0.125, "output": 10}});
        let r = parse_stream("codex", &path, Some(&pricing), "m", Some(&json!({"input": 100, "cached_input": 40, "output": 10})));
        assert_eq!(r.tokens, Tokens { input: 60, cached_input: 40, output: 10 });
        assert!(r.cost_is_estimate);
        assert_eq!(r.cost_usd, (60.0 * 1.25 + 40.0 * 0.125 + 10.0 * 10.0) / 1_000_000.0);
    }
}
