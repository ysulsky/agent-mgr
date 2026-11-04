//! The am-ops bodies of `am __debug ...`, the test hooks for end-to-end tests (see
//! crates/am/src/debug.rs): storage's, and health's check-logins, mark-checking and
//! check-resources.

use crate::{health, storage};

/// A JSON array of names (`null` for none).
fn names(text: &str) -> am_core::Result<Option<Vec<String>>> {
    let v: serde_json::Value = serde_json::from_str(text)?;
    Ok(v.as_array().map(|a| a.iter().filter_map(|n| n.as_str().map(String::from)).collect()))
}

/// Run `command` if it's one of ours: its exit status, or None if it isn't.
pub fn run(command: &str, args: &[String]) -> Option<i32> {
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or_default();
    let result: am_core::Result<i32> = match command {
        "storage-connected" => storage::connected(arg(0)).map(|changed| {
            println!("{}", if changed { "true" } else { "false" });
            0
        }),
        "storage-status" => storage::status(None).map(|status| {
            println!("{}", am_core::json::encode_with(&status, Some(1), true));
            0
        }),
        "connect-command" => Ok(match storage::connect_command(arg(0)) {
            Some(found) => {
                println!("{found}");
                0
            }
            None => 1,
        }),
        // check-logins [NAMES_JSON [FIXED_JSON]]: `health::check_logins(cfg, names, fixed)`.
        "check-logins" => (|| {
            let chosen = if args.is_empty() { None } else { names(arg(0))? };
            let fixed = if args.len() > 1 { names(arg(1))?.unwrap_or_default() } else { vec![] };
            health::check_logins(Some(&am_core::config::load()?), chosen.as_deref(), Some(&fixed))?;
            Ok(0)
        })(),
        "mark-checking" => (|| {
            let chosen = if args.is_empty() { None } else { names(arg(0))? };
            health::mark_checking(chosen.as_deref(), &am_core::config::load()?)?;
            Ok(0)
        })(),
        "check-resources" => (|| {
            health::check_resources(Some(&am_core::config::load()?))?;
            Ok(0)
        })(),
        _ => return None,
    };
    Some(match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("am: {e}");
            1
        }
    })
}
