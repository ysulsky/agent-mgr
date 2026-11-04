//! Offloading and restoring whole projects: one archive per offload,
//! `<folder>/projects/<slug>/<slug>-<time>.tar.gz`, holding
//!   manifest.json        what's inside, and each agent's sessions
//!   project/...          everything under projects/<slug>/
//!   home/...             the agents' CLI session files, by their path under $HOME
//! It's streamed to `rclone rcat`, so it needs no local space, and checked against the
//! remote's MD5. Restore streams `rclone cat` into a staging dir, checks the SHA-256
//! recorded in the stub, and only then moves things into place.

use crate::storage::{
    alive, human_size, offloaded_info, project_children, project_plan, project_status, rclone, remote_label, remote_root,
    session_files, state_path, user_home, NOT_BACKED_UP, STUB_FILES,
};
use am_core::{am_err, poke, project_dir, projects_dir, read_json, update_json, Error, Result, Value, HOME_SLUG};
use am_store::projects;
use am_store::util::{self, get, has, s};
use md5::Digest;
use serde_json::json;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

/// Update the project's offload.json with `fields`, and poke the project.
fn set(slug: &str, fields: Value) -> Result<()> {
    update_json(&state_path(slug), util::empty(), |s| {
        util::update(s, &fields);
        None
    })?;
    poke(slug);
    Ok(())
}

/// Claim the project for one offload or restore, or say why not.
pub fn begin_project_job(slug: &str, kind: &str, unarchive: bool) -> Result<()> {
    let project = projects::get_project(slug)?;
    if kind == "offload" {
        if slug == HOME_SLUG {
            return am_err("the home project can't be offloaded");
        }
        if !has(&project, "archived_at") {
            return am_err(format!("{slug} isn't archived; archive it first, so nothing changes while it's backed up"));
        }
        remote_root(None)?;
        let mut running = Vec::new();
        for name in projects::agent_names(slug)? {
            let st = projects::agent_status(slug, &name, None, None)?;
            if has(&st, "main_turn") || has(&st, "side_turns") {
                running.push(name);
            }
        }
        if !running.is_empty() && !has(&project, "offloaded") {
            let verb = if running.len() == 1 { "is" } else { "are" };
            return am_err(format!("{} of {slug} {verb} still finishing a turn; offload once it's done", running.join(", ")));
        }
    }
    util::try_update_json(&state_path(slug), util::empty(), |state| {
        let job = get(state, "job").cloned().unwrap_or(Value::Null);
        if am_core::format::truthy(Some(&job)) && alive(get(&job, "pid")) {
            let doing = if get(&job, "kind").and_then(Value::as_str) == Some("offload") { "offloading" } else { "restoring" };
            return am_err(format!("{slug} is already {doing}; wait for it to finish"));
        }
        let f = util::fields(state);
        f.insert(
            "job".into(),
            json!({"kind": kind, "pid": std::process::id(), "started": util::now_value(), "step": "Starting", "unarchive": unarchive}),
        );
        f.insert("error".into(), Value::Null);
        Ok(())
    })?;
    poke(slug);
    Ok(())
}

/// Do the job begin_project_job claimed. A failure is kept for the page to show, and
/// returned.
pub fn run_project_job(slug: &str) -> Result<Value> {
    let state = read_json(&state_path(slug))?.filter(|s| am_core::format::truthy(Some(s))).unwrap_or_else(util::empty);
    let job = get(&state, "job").filter(|j| am_core::format::truthy(Some(j))).cloned().unwrap_or_else(util::empty);
    let kind = get(&job, "kind").and_then(Value::as_str).unwrap_or_default().to_string();
    let result = (|| -> Result<()> {
        match kind.as_str() {
            "offload" => offload_now(slug),
            "restore" => {
                restore_now(slug)?;
                if has(&job, "unarchive") {
                    projects::unarchive_project(slug)?;
                }
                Ok(())
            }
            _ => am_err(format!("no offload or restore of {slug} was started")),
        }
    })();
    if let Err(e) = result {
        let verb = if kind == "offload" { "Offload" } else { "Restore" };
        let mut tail = if kind == "offload" { " Everything is still here." } else { " It's still offloaded, and you can try again." }.to_string();
        if kind == "restore" && has(&job, "unarchive") {
            tail += " The project stays archived.";
        }
        let text = e.to_string();
        set(slug, json!({"job": null, "error": format!("{verb} failed: {}.{tail}", text.trim_end_matches('.'))}))?;
        return Err(e);
    }
    set(slug, json!({"job": null, "error": null}))?;
    project_status(slug)
}

/// The job's step in offload.json, at most once a second.
struct Progress {
    slug: String,
    step: String,
    total: u64,
    done: u64,
    last: Option<Instant>,
}

impl Progress {
    fn new(slug: &str, step: String, total: u64) -> Self {
        Progress { slug: slug.into(), step, total, done: 0, last: None }
    }

    fn add(&mut self, n: u64, force: bool) {
        self.done += n;
        if force || self.last.is_none_or(|t| t.elapsed().as_secs_f64() > 1.0) {
            self.last = Some(Instant::now());
            let text = format!("{} {} of {}", self.step, human_size(self.done as f64), human_size(self.total as f64));
            let (done, total) = (self.done, self.total);
            let _ = update_json(&state_path(&self.slug), util::empty(), |s| {
                if let Some(job) = util::fields(s).get_mut("job").filter(|j| j.is_object()) {
                    util::update(job, &json!({"step": text, "done": done, "total": total}));
                }
                None
            });
            poke(&self.slug);
        }
    }
}

/// Set the job's step, with no progress yet.
fn step(slug: &str, text: &str) -> Result<()> {
    update_json(&state_path(slug), util::empty(), |s| {
        if let Some(job) = util::fields(s).get_mut("job").filter(|j| j.is_object()) {
            util::update(job, &json!({"step": text, "done": 0, "total": 0}));
        }
        None
    })?;
    poke(slug);
    Ok(())
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// Remove a file, a symlink, or a whole directory (not following a symlink).
fn remove(path: &Path) -> Result<()> {
    if path.is_dir() && !is_symlink(path) {
        std::fs::remove_dir_all(path)?;
    } else if util::exists(path) || is_symlink(path) {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn session_paths(info: &Value) -> Vec<String> {
    get(info, "sessions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|e| get(e, "paths").and_then(Value::as_array).cloned().unwrap_or_default())
        .filter_map(|p| p.as_str().map(String::from))
        .collect()
}

/// Upload the project (unless an earlier run did), mark the stub, then remove the local
/// copy and the session files.
fn offload_now(slug: &str) -> Result<()> {
    let root = project_dir(slug);
    let mut info = offloaded_info(slug)?;
    if !am_core::format::truthy(Some(&info)) {
        info = upload(slug)?;
        // The stub is marked before anything is deleted, so a crash after this point leaves an
        // offloaded project with leftovers, which a rerun finishes removing.
        update_json(&root.join("project.json"), Value::Null, |p| {
            util::fields(p).insert("offloaded".into(), info.clone());
            None
        })?;
        set(slug, json!({"last_backup": info}))?;
    }
    step(slug, "Removing the local copy")?;
    for child in project_children(slug, &STUB_FILES)? {
        remove(&child)?;
    }
    let home = user_home();
    for rel in session_paths(&info) {
        remove(&home.join(rel))?;
    }
    Ok(())
}

/// `data` as JSON with one-space indentation, ASCII-only, and a trailing newline.
pub fn json_bytes(data: &Value) -> Vec<u8> {
    (am_core::json::encode_with(data, Some(1), true) + "\n").into_bytes()
}

/// Everything written to rclone's stdin, hashed and counted on the way.
struct Hashing<W: Write> {
    inner: W,
    md5: md5::Md5,
    sha256: sha2::Sha256,
    bytes: u64,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(data)?;
        self.md5.update(&data[..n]);
        sha2::Digest::update(&mut self.sha256, &data[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Add `path` to the archive as `arcname`: the entry itself, then a directory's children in
/// name order, symlinks stored as links. `count` sees each regular file's size.
fn add_tree<W: Write>(tar: &mut tar::Builder<W>, path: &Path, arcname: &str, count: &mut dyn FnMut(u64)) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    tar.append_path_with_name(path, arcname)?;
    if meta.is_file() {
        count(meta.len());
    }
    if meta.is_dir() {
        let mut names: Vec<_> = std::fs::read_dir(path)?.filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        names.sort();
        for name in names {
            let child_arc = format!("{arcname}/{}", name.to_string_lossy());
            add_tree(tar, &path.join(&name), &child_arc, count)?;
        }
    }
    Ok(())
}

/// Stream the archive to the remote and check it there.
fn upload(slug: &str) -> Result<Value> {
    let plan = project_plan(slug)?;
    let sessions = session_files(slug)?;
    let stamp = am_store::localtime::strftime(am_core::now(), "%Y%m%d-%H%M%S");
    let target = format!("{}/projects/{slug}/{slug}-{stamp}.tar.gz", remote_root(None)?);
    let manifest = json!({
        "format": 1,
        "slug": slug,
        "created": util::now_value(),
        "project": format!("projects/{slug}"),
        "files": plan["files"],
        "bytes": plan["bytes"],
        "sessions": sessions,
    });
    let label = remote_label(None)?;
    let total = plan["bytes"].as_u64().unwrap_or(0);
    let mut progress = Progress::new(slug, format!("Uploading to {label}:"), total);
    progress.add(0, true);
    if am_store::setup::which("rclone").is_none() {
        return am_err("rclone is not installed; on a Mac run `brew install rclone`");
    }
    let mut child = Command::new("rclone").args(["rcat", &target]).stdin(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let mut stderr_pipe = child.stderr.take().unwrap();
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });
    let stdin = child.stdin.take().unwrap();
    let hashing = Hashing { inner: stdin, md5: md5::Md5::new(), sha256: sha2::Sha256::default(), bytes: 0 };
    let written = (|| -> std::io::Result<Hashing<std::process::ChildStdin>> {
        // Level 3: much faster than the default, and most of the bulk is already compressed.
        let gz = flate2::GzBuilder::new().mtime(0).write(hashing, flate2::Compression::new(3));
        let mut tar = tar::Builder::new(gz);
        tar.follow_symlinks(false);
        let data = json_bytes(&manifest);
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mtime(am_core::now() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        tar.append_data(&mut header, "manifest.json", data.as_slice())?;
        for child in project_children(slug, &NOT_BACKED_UP).map_err(std::io::Error::other)? {
            let name = child.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            add_tree(&mut tar, &child, &format!("project/{name}"), &mut |n| progress.add(n, false))?;
        }
        let home = user_home();
        for entry in &sessions {
            for rel in get(entry, "paths").and_then(Value::as_array).into_iter().flatten() {
                let rel = rel.as_str().unwrap_or_default();
                add_tree(&mut tar, &home.join(rel), &format!("home/{rel}"), &mut |_| {})?;
            }
        }
        let gz = tar.into_inner()?;
        gz.finish()
    })();
    let (error, hashed) = match written {
        Ok(h) => (None, Some(h)),
        Err(e) => (Some(e.to_string()), None),
    };
    let (md5_hex, sha_hex, archive_bytes) = match hashed {
        Some(h) => {
            let Hashing { inner, md5, sha256, bytes } = h;
            drop(inner);
            (hex(&md5.finalize()), hex(&sha2::Digest::finalize(sha256)), bytes)
        }
        None => (String::new(), String::new(), 0),
    };
    let status = child.wait()?;
    let stderr = String::from_utf8_lossy(&stderr_reader.join().unwrap_or_default()).trim().to_string();
    if error.is_some() || !status.success() {
        let _ = rclone(&["deletefile", &target]);
        let why = if stderr.is_empty() { error.unwrap_or_default() } else { stderr };
        return am_err(format!("uploading to {target} failed, nothing was removed:\n{why}"));
    }
    step(slug, &format!("Checking the upload on {label}"))?;
    let remote_md5 = remote_md5(&target)?;
    if remote_md5 != md5_hex {
        let _ = rclone(&["deletefile", &target]);
        let shown = if remote_md5.is_empty() { "unknown".to_string() } else { remote_md5 };
        return am_err(format!("the upload to {target} doesn't match what was sent (MD5 {shown}, expected {md5_hex}), nothing was removed"));
    }
    Ok(json!({
        "remote": target,
        "label": label,
        "at": util::now_value(),
        "files": plan["files"],
        "bytes": plan["bytes"],
        "archive_bytes": archive_bytes,
        "sha256": sha_hex,
        "md5": md5_hex,
        "sessions": sessions,
    }))
}

/// The remote's MD5 of a file; remotes that keep none are hashed by downloading.
fn remote_md5(target: &str) -> Result<String> {
    for extra in [None, Some("--download")] {
        let mut args = vec!["md5sum"];
        args.extend(extra);
        args.push(target);
        let ran = rclone(&args)?;
        let digest = if ran.code == 0 { ran.stdout.split_whitespace().next().unwrap_or_default().to_string() } else { String::new() };
        if digest.chars().count() == 32 {
            return Ok(digest);
        }
    }
    Ok(String::new())
}

/// Move a path: a rename, or a copy and delete across devices.
fn move_path(source: &Path, dest: &Path) -> Result<()> {
    if std::fs::rename(source, dest).is_ok() {
        return Ok(());
    }
    let status = Command::new("cp").arg("-pR").arg(source).arg(dest).status()?;
    if !status.success() {
        return Err(Error::Other(format!("couldn't move {} to {}", source.display(), dest.display())));
    }
    remove(source)
}

/// Download the archive into a staging directory, then put everything back.
fn restore_now(slug: &str) -> Result<()> {
    let root = project_dir(slug);
    let info = offloaded_info(slug)?;
    if !am_core::format::truthy(Some(&info)) {
        return Ok(()); // Already back.
    }
    let staging = projects_dir().join(format!(".restore-{slug}"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let result = (|| -> Result<()> {
        download(slug, &info, &staging)?;
        step(slug, "Putting everything back")?;
        for child in project_children(slug, &STUB_FILES)? {
            remove(&child)?; // Leftovers of an interrupted offload; all of it is in the backup.
        }
        let home = user_home();
        for rel in session_paths(&info) {
            let (source, dest) = (staging.join("home").join(&rel), home.join(&rel));
            if !(util::exists(&source) || is_symlink(&source)) {
                continue;
            }
            remove(&dest)?;
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            move_path(&source, &dest)?;
        }
        let mut children: Vec<PathBuf> = std::fs::read_dir(staging.join("project"))?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        children.sort();
        for child in children {
            let name = child.file_name().unwrap().to_owned();
            if name != "project.json" {
                std::fs::rename(&child, root.join(name))?;
            }
        }
        // Last: the original project.json has no `offloaded`, so this is what clears the stub.
        std::fs::rename(staging.join("project").join("project.json"), root.join("project.json"))?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// What `rclone cat` sends, hashed and counted as it's read.
struct Reading<R: Read> {
    inner: R,
    sha256: sha2::Sha256,
    progress: Progress,
}

impl<R: Read> Read for Reading<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        sha2::Digest::update(&mut self.sha256, &buf[..n]);
        self.progress.add(n as u64, false);
        Ok(n)
    }
}

/// Unpack the archive into `staging`, checking every path and the SHA-256.
fn download(slug: &str, info: &Value, staging: &Path) -> Result<()> {
    if am_store::setup::which("rclone").is_none() {
        return am_err("rclone is not installed; on a Mac run `brew install rclone`");
    }
    let label = if has(info, "label") { s(info, "label") } else { remote_label(None)? };
    let total = util::num(get(info, "archive_bytes")).unwrap_or(0.0) as u64;
    let mut progress = Progress::new(slug, format!("Downloading from {label}:"), total);
    progress.add(0, true);
    let remote = s(info, "remote");
    let mut child = Command::new("rclone").args(["cat", &remote]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let mut stderr_pipe = child.stderr.take().unwrap();
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });
    let mut reader = Reading { inner: child.stdout.take().unwrap(), sha256: sha2::Sha256::default(), progress };
    let mut dirs: Vec<(PathBuf, u64)> = Vec::new();
    let unpacked = (|| -> std::result::Result<(), String> {
        let gz = flate2::read::GzDecoder::new(&mut reader);
        let mut archive = tar::Archive::new(gz);
        archive.set_preserve_mtime(true);
        for entry in archive.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            let name = entry.path().map_err(|e| e.to_string())?.to_string_lossy().into_owned();
            let top = name.split('/').next().unwrap_or_default();
            if !["manifest.json", "project", "home"].contains(&top) || name.starts_with('/') || name.split('/').any(|p| p == "..") {
                return Err(format!("the backup holds an unexpected path {}", am_core::format::quote(&name)));
            }
            let mode = entry.header().mode().unwrap_or(0o644);
            let mtime = entry.header().mtime().unwrap_or(0);
            let is_dir = entry.header().entry_type().is_dir();
            let kind_is_link = entry.header().entry_type().is_symlink() || entry.header().entry_type().is_hard_link();
            entry.unpack_in(staging).map_err(|e| e.to_string())?;
            let dest = staging.join(name.trim_end_matches('/'));
            if !kind_is_link {
                use std::os::unix::fs::PermissionsExt;
                // No setuid, setgid or sticky bits, and no group or other write.
                let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(mode & 0o755));
            }
            if is_dir {
                dirs.push((dest, mtime));
            }
        }
        Ok(())
    })();
    // Read whatever's left, so the hash covers the whole stream.
    let mut rest = Vec::new();
    let _ = reader.read_to_end(&mut rest);
    let status = child.wait()?;
    let stderr = String::from_utf8_lossy(&stderr_reader.join().unwrap_or_default()).trim().to_string();
    if let Err(e) = &unpacked {
        let why = if stderr.is_empty() { e.clone() } else { stderr };
        return am_err(format!("downloading {remote} failed:\n{why}"));
    }
    if !status.success() {
        return am_err(format!("downloading {remote} failed:\n{stderr}"));
    }
    if hex(&sha2::Digest::finalize(reader.sha256)) != s(info, "sha256") {
        return am_err(format!("{remote} doesn't match the backup that was made (SHA-256 differs)"));
    }
    if !staging.join("project").join("project.json").is_file() {
        return am_err(format!("{remote} has no project.json"));
    }
    for (path, mtime) in dirs.iter().rev() {
        // Extracting a directory's files changed its time.
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(*mtime);
        if let Ok(f) = std::fs::File::open(path) {
            let _ = f.set_times(std::fs::FileTimes::new().set_accessed(t).set_modified(t));
        }
    }
    Ok(())
}

/// Claim and run an offload of the project.
pub fn offload_project(slug: &str) -> Result<Value> {
    begin_project_job(slug, "offload", false)?;
    run_project_job(slug)
}

/// Claim and run a restore of the project, unarchiving it after if `unarchive`.
pub fn restore_project(slug: &str, unarchive: bool) -> Result<Value> {
    begin_project_job(slug, "restore", unarchive)?;
    run_project_job(slug)
}
