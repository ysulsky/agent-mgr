//! `am __debug ...`: hooks for the conformance suite. Not part of the CLI surface, and
//! never needed by the live system. The Python implementation has the same commands in
//! `src/am/_debug.py`, so `tests/impl.py` drives either one the same way.
//!
//! | Command | Does |
//! |---|---|
//! | `ping` | prints `ok` (the suite's smoke test) |
//! | `tick` | one runner tick (`Runner().tick()`), then exits |
//! | `ticker` | a runner that ticks once per line on stdin and answers `ok` per tick, keeping its in-memory state between ticks, as a Python `Runner` object does. Refuses to start if a daemon holds `daemon/lock`, but doesn't hold the lock itself: like an in-process Python `Runner`, it writes no `daemon/pid`, and a held lock with no pid makes `running_pid` fail. |
//! | `scheduler-tick [SLUG [AT]]` | `scheduler.tick(cfg)`, or `_tick_project(SLUG, cfg, AT or now)` |
//! | `render NAME [JSON]` | `prompts.render(NAME, **JSON)` |
//! | `system-prompt SLUG AGENT` | `prompts.system_prompt(...)` |
//! | `turn-prompt SLUG AGENT JSON` | `prompts.turn_prompt(...)` with the arguments in JSON |
//! | `proc-shutdown DIR [REASON]` | `procs.shutdown(DIR, REASON)` |
//! | `wake` | `core.wake_daemon()`: a CLI write's wake, with nothing written |
//!
//! Owners: rust-lead for the framework; each command's body is the owning worker's.

use std::ffi::OsString;

pub fn main(args: &[OsString]) -> i32 {
    let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    if let Some(command) = args.first() {
        if let Some(code) = am_turns::debug::run(command, &args[1..]) {
            return code;
        }
        if let Some(code) = am_ops::debug::run(command, &args[1..]) {
            return code;
        }
    }
    match args.first().map(String::as_str) {
        Some("ping") => {
            println!("ok");
            0
        }
        Some("wake") => {
            am_core::wake_daemon();
            0
        }
        Some(other) => {
            eprintln!("am __debug: {other} isn't written yet");
            2
        }
        None => {
            eprintln!("usage: am __debug COMMAND ...");
            2
        }
    }
}
