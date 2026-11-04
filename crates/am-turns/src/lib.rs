//! Everything that makes and runs a turn.
//!
//! - `prompts`: templates from `assets::prompts_dir()`, the system prompt and turn prompt.
//! - `backends`: claude and codex argv, stream parsing, per-turn cost deltas, events.
//! - `turn`: the one-shot turn supervisor, `am __turn DIR` (was `python -m am.turn`).
//! - `proc`: the long-lived claude process supervisor, `am __proc DIR` (was `python -m am.proc`).
//! - `procs`: the runner's side of long-lived processes.
//! - `runner`: `Runner::tick`, the heart of the daemon.
//! - `costfix`: `am fix-costs`.
//! - `scheduler`: schedules, health-check prompts, the stuck-manager watch.
//!
//! The supervisors are detached (their own session), record everything in their
//! directory, and must keep working with no daemon running.

pub mod backends;
pub mod log;
pub mod store;
pub mod costfix;
pub mod debug;
pub mod proc;
pub mod procs;
pub mod prompts;
pub mod runner;
pub mod pyv;
pub mod scheduler;
pub mod sys;
pub mod turn;
