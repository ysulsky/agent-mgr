//! The bodies of this crate's `am __debug` commands, test hooks for end-to-end tests
//! (the table is in the `am` binary's `debug.rs`). `run` answers None for a command
//! that isn't this crate's.

use crate::runner::Runner;
use crate::{procs, prompts, convert, scheduler, store};
use am_core::{Result, Value};
use serde_json::json;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

pub fn run(command: &str, rest: &[String]) -> Option<i32> {
    let outcome = match command {
        "tick" => tick(),
        "ticker" => return Some(ticker()),
        "scheduler-tick" => scheduler_tick(rest),
        "render" => render(rest),
        "system-prompt" => system_prompt(rest),
        "turn-prompt" => turn_prompt(rest),
        "runner-call" => runner_call(rest),
        "health-prompt" => health_prompt(rest),
        "proc-shutdown" => proc_shutdown(rest),
        "fix-costs" => fix_costs(rest),
        _ => return None,
    };
    Some(match outcome {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("am __debug {command}: {e}");
            1
        }
    })
}

fn arg(rest: &[String], i: usize) -> Result<&str> {
    rest.get(i).map(String::as_str).ok_or_else(|| am_core::Error::Other("missing argument".into()))
}

fn json_arg(rest: &[String], i: usize) -> Result<Value> {
    match rest.get(i) {
        Some(text) => Ok(serde_json::from_str(text)?),
        None => Ok(json!({})),
    }
}

fn out(text: &str) {
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

fn tick() -> Result<()> {
    Runner::new(None)?.tick()
}

/// A daemon holds `daemon/lock` for its whole life. The ticker checks it but doesn't
/// take it, as a `Runner` driven by a test takes no lock at all.
fn daemon_running() -> bool {
    let lock = am_core::daemon_dir().join("lock");
    if !lock.exists() {
        return false;
    }
    crate::sys::try_lock(&lock).is_none()
}

fn ticker() -> i32 {
    if daemon_running() {
        eprintln!("am __debug ticker: a daemon is running for {}", am_core::am_home().display());
        return 1;
    }
    let mut runner = match Runner::new(None) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("am __debug ticker: {e}");
            return 1;
        }
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        if line.is_err() {
            break;
        }
        let answer = match runner.tick() {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("error: {e}"),
        };
        let _ = writeln!(stdout, "{answer}");
        let _ = stdout.flush();
    }
    0
}

fn scheduler_tick(rest: &[String]) -> Result<()> {
    let cfg = am_core::config::load()?;
    match rest.first() {
        Some(slug) => {
            let at = match rest.get(1) {
                Some(t) => t.trim().parse::<f64>().map_err(|e| am_core::Error::Other(e.to_string()))?,
                None => am_core::now(),
            };
            scheduler::tick_project(slug, &cfg, at)
        }
        None => scheduler::tick(&cfg),
    }
}

fn render(rest: &[String]) -> Result<()> {
    out(&prompts::render(arg(rest, 0)?, &json_arg(rest, 1)?)?);
    Ok(())
}

fn system_prompt(rest: &[String]) -> Result<()> {
    let (slug, name) = (arg(rest, 0)?, arg(rest, 1)?);
    out(&prompts::system_prompt(&store::get_project(slug)?, &store::get_agent(slug, name)?, true)?);
    Ok(())
}

fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().map(convert::text).collect(),
        _ => vec![],
    }
}

fn values_list(v: Option<&Value>) -> Vec<Value> {
    v.and_then(Value::as_array).cloned().unwrap_or_default()
}

fn turn_prompt(rest: &[String]) -> Result<()> {
    let (slug, name) = (arg(rest, 0)?, arg(rest, 1)?);
    let values = json_arg(rest, 2)?;
    let side = am_core::format::truthy(values.get("side"));
    let history = values.get("history").map_or_else(String::new, convert::text);
    let text = prompts::turn_prompt(slug, &store::get_agent(slug, name)?, &values_list(values.get("messages")), &strings(values.get("notes")), side, &history)?;
    out(&text);
    Ok(())
}

/// `runner-call METHOD JSON`: a Runner's note- and routing-making steps on made-up turns,
/// so tests can check the text they write.
fn runner_call(rest: &[String]) -> Result<()> {
    let method = arg(rest, 0)?;
    let values = json_arg(rest, 1)?;
    let mut runner = Runner::new(None)?;
    let slug = convert::text(&values["slug"]);
    let name = convert::text(&values["agent"]);
    let messages = values_list(values.get("messages"));
    let meta = values.get("meta").cloned().unwrap_or(json!({}));
    let mut result = values.get("result").cloned().unwrap_or(json!({}));
    match method {
        "start-main" => {
            runner.debug.dry_launch = true;
            if am_core::format::truthy(values.get("proc")) {
                runner.debug.process = Some(PathBuf::from(convert::text(&values["proc"])));
            }
            let agent = store::get_agent(&slug, &name)?;
            runner.debug_start_main(&slug, &name, &agent, &messages)?;
            out(runner.debug.launched_prompt.as_deref().unwrap_or(""));
            Ok(())
        }
        "vanished" => runner.debug_vanished(&slug, &name, &meta),
        "after-interrupt" | "after-failure" | "route-side" | "route-main" => runner.debug_step(method, &slug, &name, &meta, &messages, &mut result),
        other => Err(am_core::Error::Other(format!("unknown runner method {other}"))),
    }
}

fn health_prompt(rest: &[String]) -> Result<()> {
    out(&scheduler::health_prompt(arg(rest, 0)?, &am_core::config::load()?)?);
    Ok(())
}

fn proc_shutdown(rest: &[String]) -> Result<()> {
    procs::shutdown(Path::new(arg(rest, 0)?), rest.get(1).map_or("test", String::as_str))
}

/// `fix-costs [--dry-run]`: what `am fix-costs` prints, straight from this crate's costfix.
fn fix_costs(rest: &[String]) -> Result<()> {
    let report = crate::costfix::run(rest.iter().any(|a| a == "--dry-run"))?;
    out(&(crate::costfix::format_report(&report)? + "\n"));
    Ok(())
}
