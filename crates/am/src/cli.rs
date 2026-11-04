//! The `am` CLI.
//!
//! Each `cmd_*` follows its Python function line by line. Output is collected and written
//! at the end; an `AmError` prints `am: <message>` and exits 1, and usage errors exit 2.

use crate::argparse::Namespace;
use am_core::{am_err, excerpt, first_line, now, py, Error, Result, Value, MANAGER};
use am_store::util::{get, has, s};
use am_store::{items, mail, projects};
use std::ffi::OsString;
use std::io::{Read, Write};

/// What a command prints.
pub type Out = Vec<u8>;

/// `print(text)`.
pub fn say(out: &mut Out, text: impl AsRef<str>) {
    out.extend_from_slice(text.as_ref().as_bytes());
    out.push(b'\n');
}

pub fn main(argv: &[OsString]) -> i32 {
    let mut args = Vec::new();
    for a in &argv[1..] {
        match a.to_str() {
            Some(s) => args.push(s.to_string()),
            None => {
                eprintln!("am: arguments must be UTF-8");
                return 1;
            }
        }
    }
    let ns = match crate::parser::build().parse_args(&args) {
        Ok(ns) => ns,
        Err(exit) => {
            let _ = std::io::stdout().write_all(exit.stdout.as_bytes());
            let _ = std::io::stderr().write_all(exit.stderr.as_bytes());
            return exit.code;
        }
    };
    let mut out = Out::new();
    let result = dispatch(&ns, &mut out);
    let mut stdout = std::io::stdout().lock();
    let written = stdout.write_all(&out).and_then(|_| stdout.flush());
    if let Err(e) = written {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            return 0;
        }
    }
    match result {
        Ok(()) => 0,
        Err(Error::Am(msg)) => {
            eprintln!("am: {msg}");
            1
        }
        Err(e) => {
            eprintln!("am: {e}");
            1
        }
    }
}

fn dispatch(ns: &Namespace, out: &mut Out) -> Result<()> {
    match ns.str("command").unwrap_or_default() {
        "setup" => cmd_setup(out),
        "daemon" => crate::cli_ext::cmd_daemon(ns, out),
        "url" => crate::cli_ext::cmd_url(out),
        "health" => crate::cli_ext::cmd_health(ns, out),
        "send" => cmd_send(ns, out),
        "stop" => cmd_stop(ns, out),
        "answer" => cmd_answer(ns, out),
        "questions" => cmd_questions(ns, out),
        "show" => cmd_show(ns, out),
        "project" => cmd_project(ns, out),
        "agent" => cmd_agent(ns, out),
        "task" => crate::cli_records::cmd_task(ns, out),
        "attention" => crate::cli_records::cmd_attention(ns, out),
        "schedule" => crate::cli_records::cmd_schedule(ns, out),
        "artifact" => crate::cli_records::cmd_artifact(ns, out),
        "memory" => crate::cli_records::cmd_memory(ns, out),
        "fix-costs" => crate::cli_ext::cmd_fix_costs(ns, out),
        "storage" => crate::cli_ext::cmd_storage(ns, out),
        other => am_err(format!("unknown command {other}")),
    }
}

// ---------------------------------------------------------------- helpers

/// `os.environ.get(name)`, with an empty value as unset where Python tests truthiness.
pub fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

/// `_sender()`.
pub fn sender() -> String {
    match (env("AM_PROJECT"), env("AM_AGENT")) {
        (Some(p), Some(a)) => format!("{p}/{a}"),
        _ => "user".into(),
    }
}

/// `_author()`.
pub fn author() -> Option<String> {
    std::env::var("AM_AGENT").ok()
}

pub fn stdin_is_tty() -> bool {
    unsafe { libc::isatty(0) == 1 }
}

/// `sys.stdin.read()`: UTF-8, with line ends as they are.
pub fn read_stdin() -> Result<String> {
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes)?;
    String::from_utf8(bytes).map_err(|_| Error::Other("'utf-8' codec can't decode the standard input".into()))
}

/// `open(file, encoding="utf-8").read()`: universal newlines.
fn read_file(path: &str) -> Result<String> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::Other(format!("[Errno 2] No such file or directory: {}", py::repr(path))))
        }
        Err(e) => return Err(e.into()),
    };
    let text = String::from_utf8(bytes).map_err(|_| Error::Other(format!("'utf-8' codec can't decode {path}")))?;
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

/// `_read_text`: text from arguments, `-f FILE`, or stdin with `-`.
pub fn read_text(parts: Option<&[String]>, file: Option<&str>, required: bool) -> Result<String> {
    if let Some(f) = file.filter(|f| !f.is_empty()) {
        return if f == "-" { read_stdin() } else { read_file(f) };
    }
    if let Some(parts) = parts {
        if parts.len() == 1 && parts[0] == "-" {
            return read_stdin();
        }
        if !parts.is_empty() {
            return Ok(parts.join(" "));
        }
    }
    if !stdin_is_tty() {
        let data = read_stdin()?;
        if !py::strip(&data).is_empty() {
            return Ok(data);
        }
    }
    if required {
        return am_err("no text given: pass it as an argument, with -f FILE, or on stdin with -");
    }
    Ok(String::new())
}

/// `_ago(ts)`.
pub fn ago(ts: Option<&Value>) -> String {
    if !py::truthy(ts) {
        return "-".into();
    }
    let ts = am_store::util::num(ts).unwrap_or(0.0);
    let seconds = (now() - ts).trunc() as i64;
    if seconds < 90 {
        format!("{seconds}s ago")
    } else if seconds < 5400 {
        format!("{}m ago", seconds.div_euclid(60))
    } else if seconds < 129600 {
        format!("{}h ago", seconds.div_euclid(3600))
    } else {
        format!("{}d ago", seconds.div_euclid(86400))
    }
}

/// `_project(args)`.
pub fn project_of(ns: &Namespace) -> Result<String> {
    projects::require_current_project(ns.str("project"))
}

/// `f"{value:.2f}"`.
pub fn f2(v: Option<&Value>) -> String {
    format!("{:.2}", am_store::util::num(v).unwrap_or(0.0))
}

/// `excerpt(value)` for a JSON value that should be text.
pub fn excerpt_value(v: Option<&Value>, limit: usize) -> String {
    match v {
        Some(Value::String(t)) => excerpt(t, limit),
        Some(other) if py::truthy(Some(other)) => excerpt(&py::str_value(other), limit),
        _ => String::new(),
    }
}

/// The current project, as `args.project or projects.current_project()`.
fn current(ns: &Namespace) -> Result<Option<String>> {
    match ns.str("project").filter(|p| !p.is_empty()) {
        Some(p) => Ok(Some(p.to_string())),
        None => projects::current_project(),
    }
}

/// `_require_setup()`.
pub fn require_setup() -> Result<()> {
    if !am_store::setup::is_set_up() {
        return am_err(format!("{} isn't set up; run `am setup`", am_core::am_home().display()));
    }
    Ok(())
}

// ---------------------------------------------------------------- setup

fn cmd_setup(out: &mut Out) -> Result<()> {
    let (created, warnings) = am_store::setup::run()?;
    let home = am_core::am_home();
    if !created.is_empty() {
        say(out, format!("Set up {}: created {}.", home.display(), created.join(", ")));
    } else {
        say(out, format!("{} is already set up.", home.display()));
    }
    for warning in warnings {
        say(out, format!("Warning: {warning}"));
    }
    say(out, "Next: `am daemon start`, then open the address from `am url`.");
    Ok(())
}

// ---------------------------------------------------------------- messages

fn cmd_send(ns: &Namespace, out: &mut Out) -> Result<()> {
    let current = current(ns)?;
    let (slug, name) = projects::parse_address(ns.str("agent").unwrap_or_default(), current.as_deref())?;
    let sender = sender();
    if sender == format!("{slug}/{name}") {
        return am_err("an agent can't send a message to itself");
    }
    let text = read_text(ns.list("text"), ns.str("file"), true)?;
    let mut reply_to = None;
    if let Some(id) = ns.str("reply_to").filter(|r| !r.is_empty()) {
        let Some(item) = mail::find_item(id, Some(&slug))? else { return am_err(format!("no item {id}")) };
        let empty = am_store::util::empty();
        let message = am_store::util::or(&item, "message", &empty);
        let body = [get(message, "text"), get(&item, "text"), get(&item, "title")].into_iter().flatten().find(|v| py::truthy(Some(v)));
        reply_to = Some(serde_json::json!({"id": id, "excerpt": excerpt_value(body, 240)}));
    }
    let interrupt = ns.flag("interrupt");
    let message = mail::send(
        &slug,
        &name,
        &text,
        mail::SendOpts { sender, interrupt, reply_to, ..Default::default() },
    )?;
    let id = s(&message, "id");
    if projects::is_archived(&slug)? {
        say(out, format!("{id} held for {slug}/{name}: {slug} is archived, so this is delivered when it's un-archived"));
    } else {
        say(out, format!("{id} {} {slug}/{name}", if interrupt { "interrupting" } else { "sent to" }));
    }
    Ok(())
}

fn cmd_stop(ns: &Namespace, out: &mut Out) -> Result<()> {
    let current = current(ns)?;
    let (slug, name) = projects::parse_address(ns.str("agent").unwrap_or_default(), current.as_deref())?;
    // runner.request_stop: ask the runner to stop an agent's main turn, from any process.
    am_core::write_json(&am_core::agent_dir(&slug, &name).join("stop.json"), &serde_json::json!({"at": am_store::util::now_value()}))?;
    am_core::poke(&slug);
    say(out, format!("Stopping the main turn of {slug}/{name}."));
    Ok(())
}

fn cmd_answer(ns: &Namespace, out: &mut Out) -> Result<()> {
    let id = ns.str("id").unwrap_or_default();
    let question = id.split('@').next().unwrap_or_default().to_string();
    let (project, agent) = (std::env::var("AM_PROJECT").ok(), std::env::var("AM_AGENT").ok());
    let slug = current(ns)?;
    let item = mail::find_item(&question, slug.as_deref())?;
    let item = match item {
        Some(i) if get(&i, "type").and_then(Value::as_str) == Some("message") => i,
        _ => return am_err(format!("no question {question}; see `am questions`")),
    };
    let (ip, ia) = (s(&item, "project"), s(&item, "agent"));
    if agent.as_deref().is_some_and(|a| !a.is_empty()) && (Some(ip.as_str()), Some(ia.as_str())) != (project.as_deref(), agent.as_deref()) {
        return am_err(format!("{question} was asked of {ip}/{ia}, not you"));
    }
    let text = read_text(ns.list("text"), ns.str("file"), true)?;
    let answer = mail::publish_answer(&ip, &ia, &question, &text, am_core::current_turn().as_deref())?;
    say(out, format!("Published {}", s(&answer, "id")));
    Ok(())
}

/// `items[-limit:]`.
pub fn last_n<T: Clone>(items: &[T], limit: i64) -> Vec<T> {
    let len = items.len() as i64;
    let start = if limit <= 0 { (-limit).min(len) } else { (len - limit).max(0) };
    items[start as usize..].to_vec()
}

fn cmd_questions(ns: &Namespace, out: &mut Out) -> Result<()> {
    let slug = project_of(ns)?;
    let name = ns.str("agent").filter(|a| !a.is_empty()).map(String::from).or_else(|| env("AM_AGENT")).unwrap_or(MANAGER.into());
    let ids = last_n(&mail::user_message_ids(&slug, &name)?, ns.int("limit").unwrap_or(20));
    if ids.is_empty() {
        say(out, "No questions yet.");
    }
    for question in ids {
        let message = mail::get_message(&slug, &name, &question)?.filter(|m| py::truthy(Some(m))).unwrap_or_else(am_store::util::empty);
        let answers = mail::answer_versions(&slug, &name, &question)?;
        let text = get(&message, "text").cloned().unwrap_or_else(|| "".into());
        say(out, format!("{question}  {}: {}", ago(get(&message, "ts")), excerpt_value(Some(&text), 100)));
        match answers.last() {
            Some(latest) => say(out, format!("  v{}: {}", s(latest, "version"), excerpt_value(get(latest, "text"), 140))),
            None => say(out, "  (no answer yet)"),
        }
    }
    Ok(())
}

fn cmd_show(ns: &Namespace, out: &mut Out) -> Result<()> {
    let id = ns.str("id").unwrap_or_default();
    match mail::find_item(id, current(ns)?.as_deref())? {
        None => am_err(format!("nothing found with id {id}")),
        Some(item) => {
            out.extend_from_slice(mail::render_item(&item)?.as_bytes());
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- projects

fn print_settings(slug: &str, out: &mut Out) -> Result<()> {
    let cfg = am_core::config::load()?;
    let project = projects::get_project(slug)?;
    say(out, format!("{} ({slug})", s(&project, "name")));
    if has(&project, "archived_at") {
        say(out, format!("Archived {}: nothing new runs, and messages wait until `am project unarchive {slug}`", ago(get(&project, "archived_at"))));
    }
    if has(&project, "offloaded") {
        let info = &project["offloaded"];
        say(out, format!("Offloaded {} to {}: only this stub is here until `am project restore {slug}`", ago(get(info, "at")), s(info, "remote")));
        return Ok(());
    }
    if has(&project, "description") {
        say(out, format!("Description: {}", s(&project, "description")));
    }
    if has(&project, "status") {
        say(out, format!("Status: {}", s(&project, "status")));
    }
    let defaults = &cfg["defaults"];
    let or_default = |key: &str| -> String {
        if has(&project, key) { s(&project, key) } else { s(defaults, key) }
    };
    say(out, format!("Health check: every {}", or_default("health_every")));
    for role in ["manager", "worker"] {
        let backend = or_default(&format!("{role}_backend"));
        let key = format!("{role}_model");
        let model = if has(&project, &key) {
            s(&project, &key)
        } else if has(defaults, &key) {
            s(defaults, &key)
        } else {
            "CLI default".into()
        };
        say(out, format!("Default {role}: {backend}, model {model}"));
    }
    let attention = items::attention_counts(&items::list_attention(slug, false)?);
    let label = s(&attention, "label");
    say(out, format!("Attention: {}", if label.is_empty() { "nothing open".into() } else { label }));
    say(out, format!("Open tasks: {}", items::list_tasks(slug, false)?.len()));
    say(out, format!("Cost so far: ${:.2}", projects::project_cost(slug)?));
    Ok(())
}

fn running(status: &Value) -> bool {
    has(status, "main_turn") || has(status, "side_turns")
}

fn archive(slug: &str, out: &mut Out) -> Result<()> {
    let already = projects::is_archived(slug)?;
    projects::archive_project(slug)?;
    if already {
        say(out, format!("{slug} is already archived."));
        return Ok(());
    }
    say(out, format!("Archived {slug}. Nothing was deleted. Nothing new runs and messages wait until `am project unarchive {slug}`."));
    let mut busy = Vec::new();
    for name in projects::agent_names(slug)? {
        if running(&projects::agent_status(slug, &name, None, None)?) {
            busy.push(name);
        }
    }
    if !busy.is_empty() {
        say(out, format!("Still running, and will finish normally: {}.", busy.join(", ")));
    }
    Ok(())
}

/// `storage.offloaded_info`.
pub fn offloaded_info(slug: &str) -> Result<Value> {
    Ok(get(&projects::get_project(slug)?, "offloaded").cloned().unwrap_or(Value::Null))
}

fn unarchive(slug: &str, out: &mut Out) -> Result<()> {
    if !projects::is_archived(slug)? {
        say(out, format!("{slug} isn't archived."));
        return Ok(());
    }
    let info = offloaded_info(slug)?;
    if py::truthy(Some(&info)) {
        say(out, format!("{slug} is offloaded, so it's restored first, from {}...", s(&info, "remote")));
        crate::cli_ext::restore_project(slug, true)?;
    } else {
        projects::unarchive_project(slug)?;
    }
    say(out, format!("Un-archived {slug}. Schedules resume from now, and runs missed while it was archived are skipped."));
    let mut held = 0;
    for name in projects::agent_names(slug)? {
        held += projects::agent_status(slug, &name, None, None)?["queued"].as_i64().unwrap_or(0);
    }
    if held > 0 {
        say(out, format!("{held} held message{} will be delivered now.", if held == 1 { "" } else { "s" }));
    }
    Ok(())
}

fn cmd_project(ns: &Namespace, out: &mut Out) -> Result<()> {
    match ns.str("action").unwrap_or_default() {
        "create" => {
            let name = ns.list("name").unwrap_or_default().join(" ");
            let project = projects::create_project(&name, ns.str("description").unwrap_or_default(), None)?;
            let slug = s(&project, "slug");
            say(out, format!("Created project {slug}. Its manager is waiting for a first message: am send {slug} \"...\""));
        }
        "list" => {
            let cfg = am_core::config::load()?;
            let mut slugs = Vec::new();
            for slug in projects::list_slugs()? {
                let archived = projects::is_archived(&slug)?;
                slugs.push((archived, slug));
            }
            // Archived projects come last (a stable sort). Their attention items don't count.
            slugs.sort_by_key(|(archived, _)| *archived);
            for (_, slug) in slugs {
                let project = projects::get_project(&slug)?;
                let archived = has(&project, "archived_at");
                let mut states = Vec::new();
                for name in projects::agent_names(&slug)? {
                    states.push(projects::agent_status(&slug, &name, None, Some(&cfg))?);
                }
                let busy = states.iter().filter(|st| running(st)).count();
                let mark = if has(&project, "offloaded") { " (archived, offloaded)" } else if archived { " (archived)" } else { "" };
                let what = ["status", "description"].iter().find(|k| has(&project, k)).map_or_else(|| s(&project, "name"), |k| s(&project, k));
                // Python's sum() starts from 0, not Rust's -0.0, so an empty sum prints "0.00".
                let cost = states.iter().map(|st| am_store::util::num(get(st, "cost_usd")).unwrap_or(0.0)).fold(0.0, |a, b| a + b);
                let mut line = format!("{slug}{mark}: {what}  [{busy} running, ${cost:.2}");
                if archived {
                    let held: i64 = states.iter().map(|st| st["queued"].as_i64().unwrap_or(0)).sum();
                    if held > 0 {
                        line += &format!(", {held} held");
                    }
                } else {
                    let attention = items::attention_counts(&items::list_attention(&slug, false)?);
                    if attention["open"].as_i64().unwrap_or(0) > 0 {
                        line += &format!(", {}", s(&attention, "label"));
                    }
                }
                say(out, line + "]");
            }
        }
        "archive" => archive(ns.str("name").unwrap_or_default(), out)?,
        "unarchive" => unarchive(ns.str("name").unwrap_or_default(), out)?,
        "offload" => crate::cli_ext::offload(ns.str("name").unwrap_or_default(), out)?,
        "restore" => crate::cli_ext::restore(ns.str("name").unwrap_or_default(), out)?,
        "show" => {
            let slug = match ns.str("slug").filter(|s| !s.is_empty()) {
                Some(s) => s.to_string(),
                None => project_of(ns)?,
            };
            print_settings(&slug, out)?;
            say(out, "Agents:");
            print_agents(&slug, false, out)?;
        }
        "set" => {
            let slug = project_of(ns)?;
            let mut fields = serde_json::Map::new();
            for key in ["description", "status", "name"] {
                if let Some(v) = ns.str(key) {
                    fields.insert(key.into(), v.into());
                }
            }
            if let Some(every) = ns.str("health_every") {
                if every != "off" {
                    am_core::parse_duration(every)?;
                }
                fields.insert("health_every".into(), every.into());
            }
            for (arg, key) in [("backend", "manager_backend"), ("model", "manager_model"), ("worker_backend", "worker_backend"), ("worker_model", "worker_model")] {
                if let Some(v) = ns.str(arg) {
                    fields.insert(key.into(), if v.is_empty() { Value::Null } else { v.into() });
                }
            }
            if fields.is_empty() {
                return print_settings(&slug, out);
            }
            let mut names: Vec<String> = fields.keys().cloned().collect();
            names.sort();
            projects::update_project(&slug, &Value::Object(fields))?;
            say(out, format!("Updated {slug}: {}", names.join(", ")));
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------- agents

fn print_agents(slug: &str, include_retired: bool, out: &mut Out) -> Result<()> {
    let cfg = am_core::config::load()?;
    for name in projects::agent_names(slug)? {
        let status = projects::agent_status(slug, &name, None, Some(&cfg))?;
        if has(&status, "retired") && !include_retired {
            continue;
        }
        let mut detail = s(&status, "state");
        if has(&status, "main_started") {
            detail += &format!(" since {}", ago(get(&status, "main_started")));
        }
        let sides = status["side_turns"].as_array().map_or(0, Vec::len);
        if sides > 0 {
            detail += &format!(", {sides} side");
        }
        if has(&status, "queued") {
            detail += &format!(", {} queued", s(&status, "queued"));
        }
        let model = if has(&status, "model") { s(&status, "model") } else { "default model".into() };
        say(out, format!("{name}  [{}, {}/{model}]  {detail}  ${}", s(&status, "role"), s(&status, "backend"), f2(get(&status, "cost_usd"))));
        if has(&status, "purpose") {
            say(out, format!("  {}", s(&status, "purpose")));
        }
        if has(&status, "last_error") {
            say(out, format!("  last error: {}", s(&status, "last_error")));
        }
    }
    Ok(())
}

fn cmd_agent(ns: &Namespace, out: &mut Out) -> Result<()> {
    let slug = project_of(ns)?;
    let name = ns.str("name").unwrap_or_default();
    match ns.str("action").unwrap_or_default() {
        "create" => {
            projects::create_agent(&slug, name, "worker", ns.str("backend"), ns.str("model"), ns.str("purpose").unwrap_or_default())?;
            say(out, format!("Created worker {name} in {slug}. Give it work: am send {name} \"...\""));
        }
        "list" => print_agents(&slug, ns.flag("all"), out)?,
        "set" => {
            let (backend, model) = (ns.str("backend"), ns.str("model"));
            if backend.is_none() && model.is_none() {
                return am_err("pass --backend and/or --model");
            }
            projects::get_agent(&slug, name)?;
            projects::set_agent_backend(&slug, name, backend, model)?;
            say(out, format!("Updated {name}; it takes effect on its next turn."));
        }
        "retire" => {
            projects::get_agent(&slug, name)?;
            projects::retire_agent(&slug, name)?;
            say(out, format!("Retired {name}."));
        }
        "show" => {
            let status = projects::agent_status(&slug, name, None, None)?;
            say(out, am_core::json::dumps_with(&status, Some(1), true));
            say(out, format!("Turns: {}", am_core::turns_dir(&slug, name).display()));
        }
        _ => {}
    }
    Ok(())
}

/// First-line helper shared with the records commands.
pub fn first(text: &str, limit: usize) -> String {
    first_line(text, limit)
}
