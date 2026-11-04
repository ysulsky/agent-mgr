//! The records under `AM_HOME`: projects and agents, mail (messages, answers, notes, the
//! chat), items (tasks, attention, schedules and cron), shared memory and artifacts with
//! their git stores, and `am setup`.
//!
//! Everything here is synchronous `std` code that works on files, whether or not a daemon
//! is running.

pub mod artifacts;
pub mod gitstore;
pub mod items;
pub mod localtime;
mod mime_default;
pub mod mail;
pub mod memory;
pub mod projects;
pub mod setup;
pub mod util;
