//! Shared memory: facts about the user and how to get things done.
//!
//! One markdown file per entry in ~/agents/memory, recorded in git.

use crate::gitstore::GitStore;
use crate::util::{self, quote};
use am_core::{am_err, am_home, atomic_write, memory_dir, poke, Result, Value};
use regex::Regex;
use serde_json::{json, Map};
use std::sync::OnceLock;

pub fn store() -> GitStore {
    GitStore::new(am_home().join(".memory.git"), memory_dir())
}

/// The file name of an entry, refusing names that could be paths.
fn file(name: &str) -> Result<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9 _.-]{0,79}\n?$").unwrap());
    let name = name.strip_suffix(".md").unwrap_or(name);
    if !re.is_match(name) || name.contains("..") {
        return am_err(format!("invalid memory name {}: use letters, digits, spaces, - _ and .", quote(name)));
    }
    Ok(format!("{name}.md"))
}

/// A file's text, with invalid UTF-8 replaced and `\r\n` and `\r` read as `\n`.
fn read_text_lossy(path: &std::path::Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).replace("\r\n", "\n").replace('\r', "\n"))
}

/// Every entry as `{name, mtime, summary}`, by name ignoring case.
pub fn list_entries() -> Result<Vec<Value>> {
    let mut out: Vec<(String, Value)> = Vec::new();
    for filename in am_core::fsx::listdir(&memory_dir())? {
        if !filename.ends_with(".md") || filename.starts_with('.') {
            continue;
        }
        let path = memory_dir().join(&filename);
        let Ok(link) = std::fs::symlink_metadata(&path) else { continue };
        if link.file_type().is_symlink() || !path.is_file() {
            continue;
        }
        let text = read_text_lossy(&path)?;
        let summary = am_core::format::split_lines(&text)
            .into_iter()
            .find(|l| !am_core::format::strip(l).is_empty())
            .map(|l| am_core::format::strip(l.trim_matches(|c| c == '#' || c == ' ')).to_string())
            .unwrap_or_default();
        let mtime = util::mtime(&std::fs::metadata(&path)?);
        let name = filename[..filename.len() - 3].to_string();
        out.push((name.to_lowercase(), json!({"name": name, "mtime": util::float(mtime), "summary": summary})));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out.into_iter().map(|(_, v)| v).collect())
}

pub fn read(name: &str) -> Result<String> {
    let path = memory_dir().join(file(name)?);
    let missing = || am_err(format!("no memory entry {}; see `am memory list`", quote(name)));
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_symlink() => return missing(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return missing(),
        _ => {}
    }
    match std::fs::read(&path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Ok(text.replace("\r\n", "\n").replace('\r', "\n")),
            Err(_) => util::crash(format!("{} isn't valid UTF-8", path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing(),
        Err(e) => Err(e.into()),
    }
}

pub fn add(name: &str, text: &str, author: Option<&str>) -> Result<Option<String>> {
    let filename = file(name)?;
    if util::exists(&memory_dir().join(&filename)) {
        return am_err(format!("memory entry {} exists; use `am memory edit`", quote(name)));
    }
    let message = format!("Add {}", &filename[..filename.len() - 3]);
    write(&filename, text, &message, author)
}

pub fn edit(name: &str, text: &str, author: Option<&str>) -> Result<Option<String>> {
    let filename = file(name)?;
    if !util::exists(&memory_dir().join(&filename)) {
        return am_err(format!("no memory entry {}; use `am memory add`", quote(name)));
    }
    let message = format!("Edit {}", &filename[..filename.len() - 3]);
    write(&filename, text, &message, author)
}

fn write(filename: &str, text: &str, message: &str, author: Option<&str>) -> Result<Option<String>> {
    if am_core::format::strip(text).is_empty() {
        return am_err("a memory entry cannot be empty; use `am memory rm` to delete one");
    }
    let body = if text.ends_with('\n') { text.to_string() } else { format!("{text}\n") };
    atomic_write(&memory_dir().join(filename), body.as_bytes(), 0o644)?;
    let rev = store().commit(&[filename.to_string()], message, author)?;
    notify();
    Ok(rev)
}

pub fn remove(name: &str, author: Option<&str>) -> Result<Option<String>> {
    let filename = file(name)?;
    let path = memory_dir().join(&filename);
    if !util::exists(&path) {
        return am_err(format!("no memory entry {}", quote(name)));
    }
    std::fs::remove_file(&path)?;
    let message = format!("Remove {}", &filename[..filename.len() - 3]);
    let rev = store().commit(&[filename], &message, author)?;
    notify();
    Ok(rev)
}

/// The history of one entry, or of all of memory (callers pass a limit of 100).
pub fn log(name: Option<&str>, limit: i64) -> Result<Vec<Value>> {
    let path = match name.filter(|n| !n.is_empty()) {
        Some(n) => Some(file(n)?),
        None => None,
    };
    store().log(path.as_deref(), limit)
}

pub fn show_at(name: &str, rev: &str) -> Result<String> {
    match store().show(rev, &file(name)?)? {
        Some(data) => Ok(String::from_utf8_lossy(&data).into_owned()),
        None => am_err(format!("no memory entry {} at revision {rev}", quote(name))),
    }
}

fn notify() {
    poke("home");
}

/// Each entry's name and a hash of its text, to tell later what changed.
pub fn snapshot() -> Result<Value> {
    use sha2::Digest;
    let mut out = Map::new();
    for entry in list_entries()? {
        let name = util::s(&entry, "name");
        let Ok(text) = std::fs::read(memory_dir().join(format!("{name}.md"))) else { continue };
        let digest = sha2::Sha256::digest(&text);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        out.insert(name, hex[..16].into());
    }
    Ok(Value::Object(out))
}

/// The list of entries, for the top of a turn's prompt.
pub fn index_text() -> Result<String> {
    let entries = list_entries()?;
    Ok(entries
        .iter()
        .map(|e| {
            let summary: String = util::s(e, "summary").chars().take(120).collect();
            format!("- {}: {summary}", util::s(e, "name"))
        })
        .collect::<Vec<_>>()
        .join("\n"))
}
