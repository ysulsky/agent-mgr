//! The commands that drive other crates: the daemon (am-server), health and
//! storage (am-ops), costs and schedules (am-turns). The printing is here; the work is
//! theirs.

use crate::args::Parsed;
use crate::cli::{ago, offloaded_info, require_setup, say, Out};
use am_core::{am_err, now, format, Result, Value};
use am_server::daemon;
use am_store::util::{get, has, num, s};
use am_store::projects;
use std::io::Write;

/// Write what's been printed so far, before something slow.
fn flush(out: &mut Out) {
    let _ = std::io::stdout().write_all(out);
    let _ = std::io::stdout().flush();
    out.clear();
}

pub fn cmd_daemon(ns: &Parsed, out: &mut Out) -> Result<()> {
    require_setup()?;
    match ns.str("action").unwrap_or_default() {
        "start" => {
            let was = daemon::running_pid()?;
            let pid = daemon::start(None, None)?;
            say(out, format!("The daemon {} (pid {pid}).", if was.is_some() { "is already running" } else { "started" }));
        }
        "stop" => say(
            out,
            if daemon::stop(None)? {
                "Stopped. Running turns continue, and the next daemon adopts them."
            } else {
                "The daemon isn't running."
            },
        ),
        "restart" => say(out, format!("Restarted (pid {}). Running turns were not interrupted.", daemon::restart(None)?)),
        "status" => match daemon::running_pid()? {
            Some(pid) => say(out, format!("Running (pid {pid}) at {}", daemon::base_url(None)?)),
            None => say(out, "Not running."),
        },
        "run" => daemon::main()?,
        _ => {}
    }
    Ok(())
}

pub fn cmd_url(out: &mut Out) -> Result<()> {
    require_setup()?;
    say(out, daemon::url_with_token()?);
    if daemon::running_pid()?.is_none() {
        eprintln!("The daemon isn't running; start it with `am daemon start`.");
    }
    Ok(())
}

/// How long until `ts`: `due now`, `in 5m`, `in 3h`, or `-` for none.
fn until(ts: Option<&Value>) -> String {
    if !format::truthy(ts) {
        return "-".into();
    }
    let seconds = (num(ts).unwrap_or(0.0) - now()).trunc() as i64;
    if seconds <= 0 {
        "due now".into()
    } else if seconds < 5400 {
        format!("in {}m", ((seconds as f64 / 60.0).round_ties_even() as i64).max(1))
    } else {
        format!("in {}h", (seconds as f64 / 3600.0).round_ties_even() as i64)
    }
}

fn list<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    get(v, key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// The health snapshot as `am health` prints it.
fn print_health(snapshot: &Value, out: &mut Out) {
    say(out, format!("Health: {}", s(&snapshot["summary"], "label")));
    say(out, "\nLogins");
    for login in list(snapshot, "logins") {
        let label = s(login, "label");
        let state = if has(login, "checking") { format!("{label} (checking)") } else { label };
        let mut line = format!("  {}: {state}, checked {}", s(login, "name"), ago(get(login, "checked")));
        if has(login, "detail") {
            line += &format!(". {}", s(login, "detail"));
        }
        say(out, line);
        if has(login, "fix") {
            say(out, format!("    Fix: {}", s(login, "fix")));
        }
    }
    say(out, "\nMachine");
    for row in list(snapshot, "machine") {
        let percent = get(row, "percent").filter(|p| !p.is_null());
        let reading = match percent {
            None if has(row, "read") => "couldn't read".to_string(),
            None => "not read yet".to_string(),
            Some(p) => format!("{:.0}%", num(Some(p)).unwrap_or(0.0)),
        };
        say(out, format!("  {}: {reading}, warns at {}%, read {}", s(row, "label"), s(row, "limit"), ago(get(row, "read"))));
    }
    say(out, "\nProject health checks");
    for row in list(snapshot, "projects") {
        let every = if has(row, "archived") {
            "archived".to_string()
        } else if get(row, "every").and_then(Value::as_str) == Some("off") {
            "off".to_string()
        } else {
            format!("every {}", s(row, "every"))
        };
        say(
            out,
            format!(
                "  {}: {every}, last sent {}, last due {}, next {}",
                s(row, "project"),
                ago(get(row, "sent")),
                ago(get(row, "last")),
                until(get(row, "next"))
            ),
        );
        let empty = am_store::util::empty();
        let check = am_store::util::or(row, "check", &empty);
        if has(row, "problem") {
            say(out, format!("    Problem: {}", s(row, "problem")));
        }
        if has(check, "reply") {
            say(out, format!("    Last reply: {}", s(check, "reply")));
        } else if matches!(get(check, "state").and_then(Value::as_str), Some("queued" | "running")) {
            say(out, format!("    Last check: {}", s(check, "state")));
        }
    }
}

pub fn cmd_health(ns: &Parsed, out: &mut Out) -> Result<()> {
    use am_ops::health;
    require_setup()?;
    let cfg = am_core::config::load()?;
    if ns.flag("check") {
        health::check_resources(Some(&cfg))?;
        health::check_logins(Some(&cfg), None, None)?;
    }
    let snapshot = health::snapshot(Some(&cfg))?;
    if ns.flag("json") {
        say(out, am_core::json::encode_with(&snapshot, Some(1), true));
    } else {
        print_health(&snapshot, out);
    }
    Ok(())
}

pub fn cmd_fix_costs(ns: &Parsed, out: &mut Out) -> Result<()> {
    use am_turns::costfix;
    require_setup()?;
    let dry_run = ns.flag("dry_run");
    if !dry_run && daemon::running_pid()?.is_some() {
        return am_err("stop the daemon first (`am daemon stop`), or let it run this when it starts");
    }
    say(out, costfix::format_report(&costfix::run(dry_run)?)?);
    Ok(())
}

/// As `am_ops::storage::human_size`: a byte count as `512 B`, `1.5 KB`, ... `TB`.
pub fn human_size(n: Option<&Value>) -> String {
    let mut n = num(n).unwrap_or(0.0);
    for unit in ["B", "KB", "MB", "GB", "TB"] {
        if n < 1024.0 || unit == "TB" {
            return if unit == "B" { format!("{n:.0} {unit}") } else { format!("{n:.1} {unit}") };
        }
        n /= 1024.0;
    }
    format!("{n:.1} TB")
}

pub fn cmd_storage(ns: &Parsed, out: &mut Out) -> Result<()> {
    use am_ops::storage;
    match ns.str("action").unwrap_or_default() {
        "offload" => {
            let entry = storage::offload(ns.str("path").unwrap_or_default(), ns.str("note").unwrap_or_default(), ns.flag("keep_local"))?;
            say(out, format!("Offloaded {} ({}) to {}", s(&entry, "path"), human_size(get(&entry, "bytes")), s(&entry, "remote")));
        }
        "restore" => {
            let entry = storage::restore(ns.str("path").unwrap_or_default())?;
            say(out, format!("Restored {} from {}", s(&entry, "path"), s(&entry, "remote")));
        }
        "list" => {
            let entries = storage::list_entries()?;
            for entry in &entries {
                let place = if has(entry, "local_removed") { "remote only" } else { "also local" };
                say(
                    out,
                    format!(
                        "{}  {}  {}  {place}  {}",
                        s(entry, "path"),
                        human_size(get(entry, "bytes")),
                        ago(get(entry, "offloaded_at")),
                        s(entry, "remote")
                    ),
                );
            }
            if entries.is_empty() {
                say(out, "Nothing has been offloaded.");
            }
        }
        _ => {}
    }
    Ok(())
}

/// `am project offload`.
pub fn offload(slug: &str, out: &mut Out) -> Result<()> {
    use am_ops::storage;
    let info = offloaded_info(slug)?;
    if format::truthy(Some(&info)) && projects::agent_names(slug)?.is_empty() {
        say(out, format!("{slug} is already offloaded, to {}.", s(&info, "remote")));
        return Ok(());
    }
    let plan = storage::project_plan(slug)?;
    say(
        out,
        format!(
            "Backing up {slug} to {}: {} files, {}, and {} agent session files...",
            s(&plan, "remote"),
            s(&plan, "files"),
            s(&plan, "size"),
            s(&plan, "session_files")
        ),
    );
    flush(out);
    let result = storage::offload_project(slug)?;
    let info = &result["info"];
    let sha: String = s(info, "sha256").chars().take(12).collect();
    say(out, format!("Offloaded {slug} to {} (sha256 {sha}, {} compressed).", s(info, "remote"), human_size(get(info, "archive_bytes"))));
    say(out, format!("Only project.json is left here. `am project restore {slug}` brings everything back; un-archiving does that first."));
    Ok(())
}

/// `am project restore`.
pub fn restore(slug: &str, out: &mut Out) -> Result<()> {
    let info = offloaded_info(slug)?;
    if !format::truthy(Some(&info)) {
        say(out, format!("{slug} isn't offloaded."));
        return Ok(());
    }
    let size = get(&info, "archive_bytes").filter(|v| format::truthy(Some(v)));
    say(out, format!("Restoring {slug} from {} ({})...", s(&info, "remote"), human_size(size)));
    flush(out);
    restore_project(slug, false)?;
    say(out, format!("Restored {slug}, checked against its SHA-256. It's still archived: `am project unarchive {slug}` resumes it."));
    Ok(())
}

/// `am_ops::storage::restore_project`, un-archiving too if `unarchive`.
pub fn restore_project(slug: &str, unarchive: bool) -> Result<()> {
    am_ops::storage::restore_project(slug, unarchive)?;
    Ok(())
}

/// `am_turns::scheduler::run_now`: deliver a schedule now.
pub fn run_schedule_now(slug: &str, name: &str) -> Result<()> {
    am_turns::scheduler::run_now(slug, name)
}
