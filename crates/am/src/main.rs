//! `am`: the CLI, the daemon and the turn supervisors in one binary.
//!
//! - `am daemon __run`: the daemon process (`am_server::daemon`).
//! - `am __turn DIR`: the one-shot turn supervisor (`am_turns::turn`).
//! - `am __proc DIR`: the long-lived claude process supervisor (`am_turns::proc`).
//! - `am __debug ...`: test hooks for the conformance suite (`debug.rs`).
//! - anything else: the CLI (`cli.rs`).

mod argparse;
mod cli;
mod cli_ext;
mod cli_records;
mod debug;
mod help;
mod parser;
mod textwrap;

use std::ffi::OsString;

fn main() {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let first = argv.get(1).and_then(|a| a.to_str()).unwrap_or("");
    let code = match first {
        "__turn" | "__proc" => supervisor(first, &argv),
        "__debug" => debug::main(&argv[2..]),
        "daemon" if argv.get(2).and_then(|a| a.to_str()) == Some("__run") => run_daemon(),
        _ => cli::main(&argv),
    };
    std::process::exit(code);
}

fn supervisor(which: &str, argv: &[OsString]) -> i32 {
    let Some(dir) = argv.get(2) else {
        eprintln!("usage: am {which} DIR");
        return 2;
    };
    let dir = std::path::Path::new(dir);
    let result = if which == "__turn" { am_turns::turn::supervise(dir) } else { am_turns::proc::supervise(dir) };
    match result {
        Ok(code) => code as i32,
        Err(e) => {
            eprintln!("am {which}: {e}");
            1
        }
    }
}

fn run_daemon() -> i32 {
    match am_server::daemon::main() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("am daemon: {e}");
            1
        }
    }
}
