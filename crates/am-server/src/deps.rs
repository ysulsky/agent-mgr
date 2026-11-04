//! Every call from the server into `am-turns` and `am-ops`, in one place, so a change to
//! one of their signatures touches only this file.

use am_core::{Result, Value};
use std::path::Path;

/// `scheduler::HEALTH`: the health check's entry in the schedule state.
pub use am_turns::scheduler::HEALTH;

// ---------------------------------------------------------------- turns

/// The daemon's `Runner`.
pub struct Runner {
    inner: am_turns::runner::Runner,
}

impl Runner {
    pub fn new(cfg: Value) -> Result<Self> {
        Ok(Runner { inner: am_turns::runner::Runner::new(Some(cfg))? })
    }
    pub fn tick(&mut self) -> Result<()> {
        self.inner.tick()
    }
    pub fn reload_config(&mut self) -> Result<()> {
        self.inner.reload_config()
    }
    pub fn cfg(&self) -> &Value {
        self.inner.cfg()
    }
}

pub fn scheduler_tick(cfg: &Value) -> Result<()> {
    am_turns::scheduler::tick(cfg)
}

pub fn run_schedule_now(slug: &str, name: &str) -> Result<()> {
    am_turns::scheduler::run_now(slug, name)
}

pub fn counts_from(project: &Value, state: &Value, name: &str, fallback: f64) -> Result<f64> {
    Ok(am_turns::scheduler::counts_from(project, state, name, fallback))
}

pub fn request_stop(slug: &str, name: &str) -> Result<()> {
    am_turns::runner::request_stop(slug, name)
}

/// `runner::turn_env(slug, name)`: the environment of a turn, in order.
pub fn turn_env(slug: &str, name: &str) -> Result<Vec<(String, String)>> {
    Ok(am_turns::runner::turn_env(slug, name, "").into_iter().collect())
}

/// `backends::read_events(backend, path, offset)`: the events, and the offset after them.
pub fn read_events(backend: &str, path: &Path, offset: u64) -> Result<(Vec<Value>, u64)> {
    Ok(am_turns::backends::read_events(backend, path, offset, 2_000_000))
}

pub fn costs_need_fixing() -> Result<bool> {
    am_turns::costfix::needed()
}

pub fn fix_costs() -> Result<String> {
    let report = am_turns::costfix::run(false)?;
    am_turns::costfix::format_report(&report)
}

// ---------------------------------------------------------------- health

pub fn check_resources(cfg: &Value) -> Result<()> {
    am_ops::health::check_resources(Some(cfg))
}

pub fn check_logins(cfg: &Value, names: Option<&[String]>, fixed: &[String]) -> Result<()> {
    am_ops::health::check_logins(Some(cfg), names, Some(fixed))
}

pub fn health_snapshot(cfg: Option<&Value>) -> Result<Value> {
    am_ops::health::snapshot(cfg)
}

pub fn mark_checking(cfg: &Value) -> Result<()> {
    am_ops::health::mark_checking(None, cfg)
}

pub fn item_login(item: &Value) -> Result<Option<String>> {
    am_ops::health::item_login(item)
}

pub fn fix_for(name: &str) -> Result<(bool, Option<String>)> {
    am_ops::health::fix_for(name, None)
}

pub fn storage_login(remote: &str) -> Result<String> {
    am_ops::health::storage_login(remote)
}

// ---------------------------------------------------------------- storage

pub fn storage_status(cfg: &Value) -> Result<Value> {
    am_ops::storage::status(Some(cfg))
}

pub fn project_status(slug: &str) -> Result<Value> {
    am_ops::storage::project_status(slug)
}

pub fn remote_label(cfg: &Value) -> Result<String> {
    am_ops::storage::remote_label(Some(cfg))
}

pub fn is_offloaded(slug: &str) -> Result<bool> {
    am_ops::storage::is_offloaded(slug)
}

pub fn project_plan(slug: &str) -> Result<Value> {
    am_ops::storage::project_plan(slug)
}

pub fn begin_project_job(slug: &str, kind: &str, unarchive: bool) -> Result<()> {
    am_ops::storage::begin_project_job(slug, kind, unarchive)
}

pub fn run_project_job(slug: &str) -> Result<()> {
    am_ops::storage::run_project_job(slug).map(|_| ())
}

pub fn connect_command(provider: &str) -> Result<Option<String>> {
    Ok(am_ops::storage::connect_command(provider))
}

/// `storage::connected(provider)`, then the remote it set up for that provider, read back
/// from `[storage] rclone_remote`.
pub fn connected(provider: &str) -> Result<String> {
    am_ops::storage::connected(provider)?;
    let cfg = am_core::config::load()?;
    Ok(cfg["storage"]["rclone_remote"].as_str().unwrap_or("").to_string())
}

pub fn remote_root() -> Result<String> {
    am_ops::storage::remote_root(None)
}
