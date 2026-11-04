//! `cli.py`'s tasks, attention, schedules, artifacts and memory commands.

use crate::argparse::Namespace;
use crate::cli::{ago, author, first, project_of, read_stdin, read_text, say, stdin_is_tty, Out};
use am_core::{am_err, Result, Value};
use am_store::util::{get, has, s};
use am_store::{artifacts, items, memory, projects};
use serde_json::json;

fn print_task(task: &Value, out: &mut Out) {
    let owner = if has(task, "agent") { format!(" ({})", s(task, "agent")) } else { String::new() };
    say(out, format!("#{} [{}] {}{owner}", s(task, "id"), s(task, "status"), s(task, "title")));
    if has(task, "notes") {
        say(out, format!("  {}", s(task, "notes")));
    }
}

pub fn cmd_task(ns: &Namespace, out: &mut Out) -> Result<()> {
    let slug = project_of(ns)?;
    match ns.str("action").unwrap_or_default() {
        "add" => {
            let title = ns.list("title").unwrap_or_default().join(" ");
            let task = items::add_task(&slug, &title, ns.str("agent"), ns.str("notes").unwrap_or_default(), ns.str("status").unwrap_or("todo"))?;
            say(out, format!("Added task #{}", s(&task, "id")));
        }
        "list" => {
            let all = ns.flag("all");
            let tasks = items::list_tasks(&slug, all)?;
            for task in &tasks {
                print_task(task, out);
            }
            if tasks.is_empty() {
                say(out, if all { "No tasks." } else { "No open tasks." });
            }
        }
        "set" => {
            let title = ns.list("title").map(|t| t.join(" "));
            let fields = json!({
                "status": ns.str("status"),
                "title": title,
                "agent": ns.str("agent"),
                "notes": ns.str("notes"),
            });
            print_task(&items::update_task(&slug, ns.int("id").unwrap_or(0), &fields)?, out);
        }
        "done" => print_task(&items::update_task(&slug, ns.int("id").unwrap_or(0), &json!({"status": "done"}))?, out),
        _ => {}
    }
    Ok(())
}

pub fn cmd_attention(ns: &Namespace, out: &mut Out) -> Result<()> {
    let slug = project_of(ns)?;
    let by = author().filter(|a| !a.is_empty()).unwrap_or("user".into());
    match ns.str("action").unwrap_or_default() {
        "add" => {
            let mut body = ns.str("body").map(String::from);
            if body.as_deref() == Some("-") {
                body = Some(read_stdin()?);
            }
            let item = items::add_attention(
                &slug,
                &ns.list("title").unwrap_or_default().join(" "),
                items::AttentionOpts {
                    body: body.unwrap_or_default(),
                    kind: ns.str("kind").map(String::from),
                    run: ns.str("run").map(String::from),
                    r#ref: ns.str("ref").map(String::from),
                    source: by,
                    key: None,
                },
            )?;
            say(out, format!("Added {}", s(&item, "id")));
        }
        "resolve" => {
            let id = ns.str("id").unwrap_or_default();
            items::resolve_attention(&slug, id, &by)?;
            say(out, format!("Resolved {id}"));
        }
        "list" => {
            let entries = items::list_attention(&slug, ns.flag("all"))?;
            for item in &entries {
                say(out, format!("{} [{}, {}] {}", s(item, "id"), s(item, "kind"), s(item, "state"), s(item, "title")));
            }
            if entries.is_empty() {
                say(out, "Nothing needs attention.");
            }
        }
        _ => {}
    }
    Ok(())
}

/// `entry.get('every') or entry.get('cron')`.
fn when(entry: &Value) -> String {
    if has(entry, "every") { s(entry, "every") } else { s(entry, "cron") }
}

pub fn cmd_schedule(ns: &Namespace, out: &mut Out) -> Result<()> {
    let slug = project_of(ns)?;
    let name = ns.str("name").unwrap_or_default();
    match ns.str("action").unwrap_or_default() {
        "set" => {
            let mut prompt = None;
            if ns.str("prompt").is_some() || ns.str("file").is_some_and(|f| !f.is_empty()) {
                let parts = ns.str("prompt").map(|p| vec![p.to_string()]);
                prompt = Some(read_text(parts.as_deref(), ns.str("file"), true)?);
            }
            let enabled = if ns.flag("on") { Some(true) } else if ns.flag("off") { Some(false) } else { None };
            let entry = items::set_schedule(
                &slug,
                name,
                items::ScheduleOpts {
                    every: ns.str("every").map(String::from),
                    cron: ns.str("cron").map(String::from),
                    to: ns.str("to").map(String::from),
                    prompt,
                    enabled,
                },
            )?;
            let off = if has(&entry, "enabled") { "" } else { " (off)" };
            say(out, format!("Schedule {name}: {} to {}{off}", when(&entry), s(&entry, "to")));
        }
        "list" => {
            let entries = items::list_schedules(&slug)?;
            let map = entries.as_object().cloned().unwrap_or_default();
            if !map.is_empty() && projects::is_archived(&slug)? {
                say(out, format!("Paused: {slug} is archived."));
            }
            for (name, entry) in &map {
                let on = get(entry, "enabled").is_none_or(|v| am_core::py::truthy(Some(v)));
                let prompt = get(entry, "prompt").and_then(Value::as_str).unwrap_or_default();
                say(out, format!("{name}: {} to {}{}: {}", when(entry), s(entry, "to"), if on { "" } else { " (off)" }, first(prompt, 100)));
            }
            if map.is_empty() {
                say(out, "No schedules.");
            }
        }
        "rm" => {
            items::remove_schedule(&slug, name)?;
            say(out, format!("Removed {name}"));
        }
        "run" => {
            crate::cli_ext::run_schedule_now(&slug, name)?;
            say(out, format!("Delivered {name}"));
        }
        _ => {}
    }
    Ok(())
}

fn published(result: &Value, out: &mut Out) {
    let path = s(result, "path");
    if has(result, "rev") {
        say(out, format!("Published {path} at {}", s(result, "rev")));
    } else {
        say(out, format!("{path} has no changes to publish."));
    }
}

fn print_log(entries: &[Value], out: &mut Out) {
    for entry in entries {
        let ts = am_store::util::num(get(entry, "ts")).unwrap_or(0.0);
        let when = am_store::localtime::strftime(ts, "%Y-%m-%d %H:%M");
        say(out, format!("{}  {when}  {}  {}", s(entry, "rev"), s(entry, "author"), s(entry, "subject")));
    }
}

pub fn cmd_artifact(ns: &Namespace, out: &mut Out) -> Result<()> {
    let slug = project_of(ns)?;
    let author = author();
    match ns.str("action").unwrap_or_default() {
        "publish" => {
            let result = artifacts::publish(
                &slug,
                ns.str("file").unwrap_or_default(),
                ns.str("message").unwrap_or_default(),
                author.as_deref(),
                ns.str("as_name"),
            )?;
            published(&result, out);
        }
        "link" => {
            let result = artifacts::link(
                &slug,
                ns.str("name").unwrap_or_default(),
                ns.str("url").unwrap_or_default(),
                ns.str("message").unwrap_or_default(),
                ns.str("title").unwrap_or_default(),
                ns.str("description").unwrap_or_default(),
                author.as_deref(),
            )?;
            published(&result, out);
        }
        "log" => {
            let entries = artifacts::log(&slug, ns.str("name"), 200)?;
            print_log(&entries, out);
            if entries.is_empty() {
                say(out, "No versions yet.");
            }
        }
        "show" => {
            let spec = ns.str("spec").unwrap_or_default();
            let (name, rev) = spec.rsplit_once('@').unwrap_or(("", spec));
            if name.is_empty() {
                return am_err("use NAME@REV; see `am artifact log NAME`");
            }
            out.extend_from_slice(&artifacts::show(&slug, name, rev)?);
        }
        "list" => {
            for entry in artifacts::list_artifacts(&slug)? {
                say(out, format!("{}  {}", s(&entry, "path"), ago(get(&entry, "mtime"))));
            }
        }
        _ => {}
    }
    Ok(())
}

/// `_edit_in_editor`: $EDITOR (else vi) on a temporary `.md` file.
fn edit_in_editor(initial: &str) -> Result<String> {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("tmp{}.md", am_core::new_id("e").replace('-', "")));
    std::fs::write(&path, initial)?;
    let editor = crate::cli::env("EDITOR").unwrap_or("vi".into());
    let status = std::process::Command::new(&editor).arg(&path).status();
    let result = match status {
        Ok(s) if s.success() => Ok(std::fs::read_to_string(&path)?),
        Ok(s) => Err(am_core::Error::Other(format!("Command '['{editor}', '{}']' returned non-zero exit status {}.", path.display(), s.code().unwrap_or(-1)))),
        Err(e) => Err(e.into()),
    };
    let _ = std::fs::remove_file(&path);
    result
}

pub fn cmd_memory(ns: &Namespace, out: &mut Out) -> Result<()> {
    let name = ns.str("name").unwrap_or_default();
    let author = author();
    match ns.str("action").unwrap_or_default() {
        "add" => {
            let text = read_text(ns.list("text"), ns.str("file"), true)?;
            let rev = memory::add(name, &text, author.as_deref())?;
            say(out, format!("Added {name}{}", rev.map(|r| format!(" at {r}")).unwrap_or_default()));
        }
        "edit" => {
            let has_text = ns.list("text").is_some_and(|t| !t.is_empty());
            let text = if has_text || ns.str("file").is_some_and(|f| !f.is_empty()) || !stdin_is_tty() {
                read_text(ns.list("text"), ns.str("file"), true)?
            } else {
                edit_in_editor(&memory::read(name)?)?
            };
            match memory::edit(name, &text, author.as_deref())? {
                Some(rev) => say(out, format!("Saved {name} at {rev}")),
                None => say(out, format!("{name} didn't change.")),
            }
        }
        "rm" => {
            memory::remove(name, author.as_deref())?;
            say(out, format!("Removed {name}"));
        }
        "list" => {
            let entries = memory::list_entries()?;
            for entry in &entries {
                let summary: String = s(entry, "summary").chars().take(100).collect();
                say(out, format!("{}: {summary}", s(entry, "name")));
            }
            if entries.is_empty() {
                say(out, "Memory is empty.");
            }
        }
        "show" => {
            let (base, rev) = name.split_once('@').unwrap_or((name, ""));
            let text = if rev.is_empty() { memory::read(base)? } else { memory::show_at(base, rev)? };
            out.extend_from_slice(text.as_bytes());
        }
        "log" => print_log(&memory::log(ns.str("name"), 100)?, out),
        _ => {}
    }
    Ok(())
}
