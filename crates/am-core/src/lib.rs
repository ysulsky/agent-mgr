//! The base every other crate builds on: `am.core` and `am.config`.
//!
//! The file protocol is the contract with the Python implementation and with live data:
//! every write is an atomic rename, read-modify-write goes through a flock lock file, and a
//! mailbox is one file per message. JSON is written exactly as Python's
//! `json.dumps(obj, indent=1, ensure_ascii=False) + "\n"` writes it, and records stay
//! `serde_json::Value` with their key order, so a read-modify-write never drops or reorders
//! a key.

pub mod assets;
pub mod clock;
pub mod config;
pub mod fsx;
pub mod hooks;
pub mod ids;
pub mod json;
pub mod log;
pub mod paths;
pub mod py;
pub mod text;
pub mod token;

pub use clock::{awake_time, format_duration, now, parse_duration};
pub use fsx::{atomic_write, list_json_ids, locked, poke, read_json, update_json, set_wake_hook, wake_daemon, wake_fifo, write_json};
pub use ids::{check_item_id, is_item_id, new_id, slugify};
pub use paths::*;
pub use serde_json::Value;
pub use text::{excerpt, first_line};

/// An error. `Am` is Python's `AmError`: a message for whoever runs `am`, printed as
/// `am: <message>` with exit status 1. The rest are failures Python would raise as an
/// uncaught exception.
#[derive(Debug)]
pub enum Error {
    Am(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    Other(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Am(m) | Error::Other(m) => f.write_str(m),
            Error::Io(e) => write!(f, "{e}"),
            Error::Json(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

/// `raise AmError(msg)`.
pub fn am_err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Am(msg.into()))
}
