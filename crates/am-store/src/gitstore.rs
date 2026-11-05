//! Small helper for the two git-recorded trees: artifacts and memory.
//!
//! Both keep the git directory outside the tree it records (`projects/.artifacts.git`,
//! `.memory.git`). Codex's sandbox makes directories named `.git` read-only, so a
//! regular repository couldn't be committed to from inside a turn.

use crate::util;
use am_core::{am_err, locked, Result, Value};
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;

/// A finished git command: its exit code and output, as bytes.
#[derive(Debug, Clone)]
pub struct Completed {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Completed {
    /// The output as text: UTF-8, with invalid bytes replaced and `\r\n` and `\r` read as `\n`.
    pub fn stdout_text(&self) -> String {
        text(&self.stdout)
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n").replace('\r', "\n")
}

/// A git directory and the work tree it records.
#[derive(Debug, Clone)]
pub struct GitStore {
    pub git_dir: PathBuf,
    pub work_tree: PathBuf,
}

impl GitStore {
    pub fn new(git_dir: PathBuf, work_tree: PathBuf) -> Self {
        GitStore { git_dir, work_tree }
    }

    /// Run git with no user or system config, its own identity, and this store's
    /// directories. With `check`, a failure is `git <cmd> failed: <stderr>`.
    pub fn run(&self, args: &[&str], check: bool, author: Option<&str>) -> Result<Completed> {
        let name = author.filter(|a| !a.is_empty()).unwrap_or("am");
        let mut cmd = Command::new("git");
        cmd.args(["-c", &format!("user.name={name}"), "-c", "user.email=am@localhost", "-c", "commit.gpgsign=false"]);
        cmd.args(args).current_dir(&self.work_tree);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&key);
            }
        }
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", &self.work_tree)
            .stdin(std::process::Stdio::inherit());
        let out = cmd.output()?;
        let done = Completed { code: out.status.code().unwrap_or(-1), stdout: out.stdout, stderr: out.stderr };
        if check && done.code != 0 {
            return am_err(format!("git {} failed: {}", args.first().unwrap_or(&""), am_core::format::strip(&text(&done.stderr))));
        }
        Ok(done)
    }

    pub fn init(&self) -> Result<()> {
        std::fs::create_dir_all(&self.work_tree)?;
        if util::exists(&self.git_dir.join("HEAD")) {
            return Ok(());
        }
        let _lock = locked(&self.git_dir)?;
        if util::exists(&self.git_dir.join("HEAD")) {
            return Ok(());
        }
        self.run(&["init", "--quiet", "--initial-branch=main"], true, None)?;
        self.run(&["commit", "--allow-empty", "--quiet", "-m", "Start of history"], true, None)?;
        Ok(())
    }

    /// Commit changes under `paths`, relative to the work tree, including deletions.
    /// Returns the new short revision, or None if nothing changed.
    pub fn commit(&self, paths: &[String], message: &str, author: Option<&str>) -> Result<Option<String>> {
        self.init()?;
        let _lock = locked(&self.git_dir)?;
        let with_paths = |head: &[&str]| -> Vec<String> {
            head.iter().map(|s| s.to_string()).chain(std::iter::once("--".to_string())).chain(paths.iter().cloned()).collect()
        };
        let add = with_paths(&["add", "--all"]);
        self.run(&add.iter().map(String::as_str).collect::<Vec<_>>(), true, None)?;
        let diff = with_paths(&["diff", "--cached", "--quiet"]);
        if self.run(&diff.iter().map(String::as_str).collect::<Vec<_>>(), false, None)?.code == 0 {
            return Ok(None);
        }
        let commit = with_paths(&["commit", "--quiet", "-m", message]);
        self.run(&commit.iter().map(String::as_str).collect::<Vec<_>>(), true, author)?;
        let rev = self.run(&["rev-parse", "--short", "HEAD"], true, None)?;
        Ok(Some(am_core::format::strip(&rev.stdout_text()).to_string()))
    }

    /// The history (of `path`, if given), as `{rev, ts, author, subject}`, newest first.
    pub fn log(&self, path: Option<&str>, limit: i64) -> Result<Vec<Value>> {
        self.init()?;
        let max = format!("--max-count={limit}");
        let mut args = vec!["log", max.as_str(), "--format=%h%x09%ct%x09%an%x09%s"];
        if let Some(p) = path.filter(|p| !p.is_empty()) {
            args.extend(["--", p]);
        }
        let out = self.run(&args, false, None)?.stdout_text();
        let mut entries = Vec::new();
        for line in am_core::format::split_lines(&out) {
            let parts: Vec<&str> = line.splitn(4, '\t').collect();
            if parts.len() == 4 {
                let Some(ts) = am_core::format::parse_int(parts[1]) else {
                    return util::crash(format!("not an integer: {}", util::quote(parts[1])));
                };
                entries.push(json!({"rev": parts[0], "ts": ts, "author": parts[2], "subject": parts[3]}));
            }
        }
        Ok(entries)
    }

    /// A file's bytes at a revision, or None.
    pub fn show(&self, rev: &str, path: &str) -> Result<Option<Vec<u8>>> {
        if rev.is_empty() || rev.starts_with('-') || !rev.chars().all(char::is_alphanumeric) {
            return am_err(format!("invalid revision {}", util::quote(rev)));
        }
        self.init()?;
        let spec = format!("{rev}:{path}");
        let done = self.run(&["show", &spec], false, None)?;
        Ok(if done.code == 0 { Some(done.stdout) } else { None })
    }
}
