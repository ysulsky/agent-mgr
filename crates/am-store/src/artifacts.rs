//! Artifacts: human-facing results, published to projects/<slug>/artifacts/.
//!
//! Every project's artifacts are recorded in one git repository at
//! `projects/.artifacts.git`, whose work tree is `projects/`. Only paths under
//! `<slug>/artifacts/` are ever added to it.

use crate::gitstore::GitStore;
use crate::projects::resolve;
use crate::util::{self, get, repr};
use am_core::{am_err, atomic_write, poke, project_dir, projects_dir, Result, Value};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const ARTIFACTS: &str = "artifacts";

/// Port of `artifacts.store`.
pub fn store() -> GitStore {
    GitStore::new(projects_dir().join(".artifacts.git"), projects_dir())
}

/// `os.path.splitext(p)[1]`: the extension, ignoring a file name's leading dots.
pub fn splitext(p: &str) -> (&str, &str) {
    let sep = p.rfind('/').map_or(-1, |i| i as isize);
    let Some(dot) = p.rfind('.') else { return (p, "") };
    if dot as isize > sep {
        let mut i = (sep + 1) as usize;
        while i < dot {
            if p.as_bytes()[i] != b'.' {
                return (&p[..dot], &p[dot..]);
            }
            i += 1;
        }
    }
    (p, "")
}

/// Port of `artifacts.kind_of`.
pub fn kind_of(path: &str) -> &'static str {
    match splitext(path).1.to_lowercase().as_str() {
        ".md" | ".markdown" => "markdown",
        ".html" | ".htm" => "html",
        ".png" | ".jpg" | ".jpeg" | ".gif" | ".webp" | ".svg" => "image",
        ".pdf" => "pdf",
        ".link" => "link",
        ".txt" | ".log" | ".csv" | ".tsv" | ".json" | ".yaml" | ".yml" => "text",
        _ => "other",
    }
}

/// `PurePosixPath(name).parts` for a relative path: no empty or `.` components.
fn posix_parts(name: &str) -> Vec<&str> {
    name.split('/').filter(|p| !p.is_empty() && *p != ".").collect()
}

/// Port of `artifacts.clean_name`: normalize `eval.html` or `artifacts/eval.html` to a
/// path inside artifacts/.
pub fn clean_name(name: &str) -> Result<String> {
    if name.starts_with('/') {
        return am_err(format!("expected a path inside artifacts/, got {}", repr(name)));
    }
    let mut parts = posix_parts(name);
    if parts.first() == Some(&ARTIFACTS) {
        parts.remove(0);
    }
    if parts.is_empty() || parts.iter().any(|p| *p == ".." || *p == "." || p.starts_with('.')) {
        return am_err(format!("invalid artifact name {}", repr(name)));
    }
    Ok(parts.join("/"))
}

/// `artifacts._git_path`.
fn git_path(slug: &str, name: &str) -> String {
    format!("{slug}/{ARTIFACTS}/{name}")
}

/// Port of `artifacts.publish`: publish a file or directory into artifacts/ and commit
/// it. A path that's already in artifacts/ publishes the in-place edit.
pub fn publish(slug: &str, source: &str, message: &str, author: Option<&str>, as_name: Option<&str>) -> Result<Value> {
    if am_core::py::strip(message).is_empty() {
        return am_err("say what changed with -m");
    }
    let artifacts_root = resolve(&project_dir(slug).join(ARTIFACTS));
    let mut src = am_core::expand_user(source);
    if !src.is_absolute() {
        // Relative to here if it exists, else to the project: the same place for agents.
        if !util::exists(&src) {
            src = project_dir(slug).join(src);
        }
    }
    let src = resolve(&src);
    if !util::exists(&src) {
        return am_err(format!("nothing to publish at {source}"));
    }
    let name = if src == artifacts_root || src.starts_with(&artifacts_root) {
        src.strip_prefix(&artifacts_root).unwrap().to_string_lossy().into_owned()
    } else {
        let base = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let name = clean_name(as_name.filter(|a| !a.is_empty()).unwrap_or(&base))?;
        let dest = artifacts_root.join(&name);
        if src.is_dir() {
            if util::exists(&dest) {
                std::fs::remove_dir_all(&dest)?;
            }
            copytree(&src, &dest)?;
        } else {
            atomic_write(&dest, &std::fs::read(&src)?, 0o644)?;
        }
        name
    };
    let path = if name.is_empty() { format!("{slug}/{ARTIFACTS}") } else { git_path(slug, &name) };
    let shown = if name.is_empty() { ARTIFACTS.to_string() } else { name.clone() };
    let rev = store().commit(&[path], &format!("[{slug}] {shown}: {message}"), author)?;
    poke(slug);
    Ok(json!({"path": if name.is_empty() { ARTIFACTS.to_string() } else { format!("{ARTIFACTS}/{name}") }, "rev": rev}))
}

/// `shutil.copytree(src, dest)`: following symlinks, copying modes and times (`copy2`).
fn copytree(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copytree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
            copy_times(&from, &to)?;
        }
    }
    copy_times(src, dest)?;
    Ok(())
}

fn copy_times(from: &Path, to: &Path) -> Result<()> {
    let meta = std::fs::metadata(from)?;
    std::fs::set_permissions(to, meta.permissions())?;
    let times = std::fs::FileTimes::new().set_accessed(meta.accessed()?).set_modified(meta.modified()?);
    if let Ok(f) = std::fs::File::options().write(true).open(to).or_else(|_| std::fs::File::open(to)) {
        let _ = f.set_times(times);
    }
    Ok(())
}

/// `urllib.parse.urlsplit(url).scheme`, or Python's ValueError for a bracketed host that
/// doesn't close.
pub fn url_scheme(url: &str) -> std::result::Result<String, String> {
    let url: String = url.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut scheme = String::new();
    let mut rest = url.as_str();
    if let Some(i) = url.find(':') {
        let first = url.chars().next().unwrap();
        if i > 0
            && first.is_ascii_alphabetic()
            && url[..i].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            scheme = url[..i].to_lowercase();
            rest = &url[i + 1..];
        }
    }
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        let netloc = &after[..end];
        if netloc.contains('[') != netloc.contains(']') {
            return Err("Invalid IPv6 URL".into());
        }
    }
    Ok(scheme)
}

/// Port of `artifacts.link`.
pub fn link(
    slug: &str,
    name: &str,
    url: &str,
    message: &str,
    title: &str,
    description: &str,
    author: Option<&str>,
) -> Result<Value> {
    let scheme = match url_scheme(url) {
        Ok(s) => s,
        Err(e) => return util::crash(e),
    };
    if scheme != "http" && scheme != "https" {
        return am_err(format!("a link artifact must be http or https, got {}", repr(url)));
    }
    let mut clean = clean_name(name)?;
    if !clean.ends_with(".link") {
        clean.push_str(".link");
    }
    let basename = clean.rsplit('/').next().unwrap_or(&clean);
    let title = if title.is_empty() { basename[..basename.len() - 5].to_string() } else { title.to_string() };
    let payload = json!({"url": url, "title": title, "description": description});
    let text = am_core::json::dumps_with(&payload, Some(1), true) + "\n";
    atomic_write(&project_dir(slug).join(ARTIFACTS).join(&clean), text.as_bytes(), 0o644)?;
    publish(slug, &format!("{ARTIFACTS}/{clean}"), message, author, None)
}

/// `os.walk(root)` with hidden directories pruned: each directory's files, top-down, in
/// directory order, not following symlinked directories.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !name.starts_with('.') {
                dirs.push(path);
            }
        } else if !name.starts_with('.') {
            out.push(path);
        }
    }
    for d in dirs {
        if !std::fs::symlink_metadata(&d).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            walk(&d, out);
        }
    }
}

/// Port of `artifacts.list_artifacts`: every file, newest first.
pub fn list_artifacts(slug: &str) -> Result<Vec<Value>> {
    let root = project_dir(slug).join(ARTIFACTS);
    let mut files = Vec::new();
    walk(&root, &mut files);
    let mut out = Vec::new();
    for path in files {
        let rel = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().into_owned();
        if contained(slug, ARTIFACTS, &rel).is_none() {
            continue;
        }
        let Ok(stat) = std::fs::metadata(&path) else { continue };
        let kind = kind_of(&rel);
        let mut entry = json!({
            "path": format!("{ARTIFACTS}/{rel}"),
            "name": rel,
            "mtime": util::float(util::mtime(&stat)),
            "size": stat.len(),
            "kind": kind,
        });
        if kind == "link" {
            util::update(&mut entry, &read_link(&path));
        }
        out.push(entry);
    }
    sort_newest_first(&mut out);
    Ok(out)
}

fn sort_newest_first(entries: &mut [Value]) {
    entries.sort_by(|a, b| {
        let (ma, mb) = (util::num(get(a, "mtime")).unwrap_or(0.0), util::num(get(b, "mtime")).unwrap_or(0.0));
        mb.partial_cmp(&ma).unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Port of `artifacts.shown_artifacts`: a folder with an index page is one entry, named
/// after the folder, which opens the page. The outermost index wins, and index.html wins
/// over index.md.
pub fn shown_artifacts(slug: &str) -> Result<Vec<Value>> {
    let entries = list_artifacts(slug)?;
    let by_name: HashMap<String, &Value> = entries.iter().map(|e| (util::s(e, "name"), e)).collect();
    let index_of = |folder: &str| -> Option<&Value> {
        by_name.get(&format!("{folder}/index.html")).or_else(|| by_name.get(&format!("{folder}/index.md"))).copied()
    };
    let mut out = Vec::new();
    let mut folders: Vec<(String, Vec<&Value>)> = Vec::new();
    for entry in &entries {
        let name = util::s(entry, "name");
        let parts: Vec<&str> = name.split('/').collect();
        let folder = (1..parts.len()).map(|i| parts[..i].join("/")).find(|prefix| index_of(prefix).is_some());
        match folder {
            None => out.push(entry.clone()),
            Some(folder) => match folders.iter_mut().find(|(f, _)| *f == folder) {
                Some((_, members)) => members.push(entry),
                None => folders.push((folder, vec![entry])),
            },
        }
    }
    for (folder, members) in folders {
        let mut shown = index_of(&folder).cloned().unwrap_or(Value::Null);
        let newest = members.iter().filter_map(|m| util::num(get(m, "mtime"))).fold(f64::NEG_INFINITY, f64::max);
        let f = util::fields(&mut shown);
        f.insert("name".into(), folder.into());
        f.insert("mtime".into(), util::float(newest));
        out.push(shown);
    }
    sort_newest_first(&mut out);
    Ok(out)
}

/// Port of `artifacts.read_link`.
pub fn read_link(path: &Path) -> Value {
    let invalid = || json!({"url": "", "title": "invalid link"});
    let Ok(bytes) = std::fs::read(path) else { return invalid() };
    let Ok(text) = String::from_utf8(bytes) else { return invalid() };
    let Ok(data) = serde_json::from_str::<Value>(&text) else { return invalid() };
    let Some(map) = data.as_object() else { return invalid() };
    let field = |k: &str| map.get(k).map(am_core::py::str_value).unwrap_or_default();
    let url = field("url");
    match url_scheme(&url) {
        Ok(s) if s == "http" || s == "https" => {}
        _ => return invalid(),
    }
    json!({"url": url, "title": field("title"), "description": field("description")})
}

/// Port of `artifacts.log` (Python's default limit is 200).
pub fn log(slug: &str, name: Option<&str>, limit: i64) -> Result<Vec<Value>> {
    let name = name.filter(|n| !n.is_empty());
    let path = match name {
        Some(n) => git_path(slug, &clean_name(n)?),
        None => format!("{slug}/{ARTIFACTS}"),
    };
    let prefix = format!("[{slug}] ");
    let mut entries = store().log(Some(&path), limit)?;
    for entry in entries.iter_mut() {
        let mut subject = util::s(entry, "subject");
        if let Some(rest) = subject.strip_prefix(&prefix) {
            let mut rest = rest.to_string();
            if name.is_some() {
                if let Some((_, after)) = rest.split_once(": ") {
                    if !after.is_empty() {
                        rest = after.to_string();
                    }
                }
            }
            subject = rest;
        }
        util::fields(entry).insert("subject".into(), subject.into());
    }
    Ok(entries.into_iter().filter(|e| util::s(e, "subject") != "Start of history").collect())
}

/// Port of `artifacts.show`.
pub fn show(slug: &str, name: &str, rev: &str) -> Result<Vec<u8>> {
    match store().show(rev, &git_path(slug, &clean_name(name)?))? {
        Some(data) => Ok(data),
        None => am_err(format!("no {name} at revision {rev}; see `am artifact log {name}`")),
    }
}

/// Port of `artifacts.has_unpublished_changes`.
pub fn has_unpublished_changes(slug: &str, name: &str) -> Result<bool> {
    let path = git_path(slug, &clean_name(name)?);
    let done = store().run(&["status", "--porcelain", "--", &path], false, None)?;
    Ok(!am_core::py::strip(&done.stdout_text()).is_empty())
}

/// Python's strict `mimetypes` map: its defaults, then each of `mimetypes.knownfiles`
/// that exists (on a Mac, `/etc/apache2/mime.types`), read once.
fn types_map() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        use crate::mime_default::{DEFAULT_TYPES, KNOWN_FILES};
        let mut map: HashMap<String, String> = DEFAULT_TYPES.iter().map(|(e, t)| (e.to_string(), t.to_string())).collect();
        for file in KNOWN_FILES {
            if !Path::new(file).is_file() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(file) else { continue };
            for line in text.split_inclusive('\n') {
                let mut words: Vec<&str> = line.split(am_core::py::is_space).filter(|w| !w.is_empty()).collect();
                if let Some(i) = words.iter().position(|w| w.starts_with('#')) {
                    words.truncate(i);
                }
                if let Some((kind, suffixes)) = words.split_first() {
                    for suffix in suffixes {
                        map.insert(format!(".{suffix}"), kind.to_string());
                    }
                }
            }
        }
        map
    })
}

/// `mimetypes.guess_type(url)[0]`, strict.
pub fn guess_type(url: &str) -> Option<String> {
    use crate::mime_default::{ENCODINGS, SUFFIXES};
    let mut url = url.to_string();
    if let Ok(scheme) = url_scheme(&url) {
        if scheme.len() > 1 {
            let mut rest = url[url.find(':').unwrap() + 1..].to_string();
            if let Some(after) = rest.strip_prefix("//") {
                rest = after[after.find('/').unwrap_or(after.len())..].to_string();
            }
            let end = rest.find(['?', '#']).unwrap_or(rest.len());
            rest.truncate(end);
            if let Some(semi) = rest.rfind(';') {
                if !rest[semi..].contains('/') {
                    rest.truncate(semi);
                }
            }
            if scheme == "data" {
                return None;
            }
            url = rest;
        }
    }
    let (mut base, mut ext) = {
        let (b, e) = splitext(&url);
        (b.to_string(), e.to_string())
    };
    while let Some((_, to)) = SUFFIXES.iter().find(|(from, _)| *from == ext.to_lowercase()) {
        let joined = format!("{base}{to}");
        let (b, e) = splitext(&joined);
        (base, ext) = (b.to_string(), e.to_string());
    }
    if ENCODINGS.contains(&ext.as_str()) {
        let (_, e) = splitext(&base);
        ext = e.to_string();
    }
    types_map().get(&ext.to_lowercase()).cloned()
}

/// Port of `artifacts.mime_of`: a file's type, from its suffix alone.
pub fn mime_of(path: &str) -> String {
    let suffix = splitext(path).1.to_lowercase();
    if suffix == ".md" || suffix == ".markdown" {
        return "text/markdown; charset=utf-8".into();
    }
    if suffix == ".link" {
        return "application/json".into();
    }
    match guess_type(&format!("file{suffix}")) {
        None => "application/octet-stream".into(),
        Some(g) if g.starts_with("text/") || g == "application/javascript" => format!("{g}; charset=utf-8"),
        Some(g) => g,
    }
}

/// Port of `artifacts.contained`: `<project>/<area>/<relative>`, if that really is a file
/// inside the area, with symlinks followed before checking.
pub fn contained(slug: &str, area: &str, relative: &str) -> Option<PathBuf> {
    let base = resolve(&project_dir(slug)).join(area);
    let target = resolve(&base.join(relative));
    (target != base && target.starts_with(&base) && target.is_file()).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_kinds() {
        assert_eq!(splitext("a/b.tar.gz"), ("a/b.tar", ".gz"));
        assert_eq!(splitext(".bashrc"), (".bashrc", ""));
        assert_eq!(splitext("a.b/c"), ("a.b/c", ""));
        assert_eq!(kind_of("x/Report.HTML"), "html");
        assert_eq!(clean_name("artifacts/./a//b.md").unwrap(), "a/b.md");
        assert!(clean_name("artifacts").is_err());
        assert!(clean_name("a/.hidden").is_err());
        assert!(clean_name("/abs").is_err());
        assert_eq!(url_scheme(" HTTPS://x").unwrap(), "https");
        assert_eq!(url_scheme("1http://x").unwrap(), "");
        assert!(url_scheme("http://[::1/").is_err());
    }

    #[test]
    fn mime_types_match_python() {
        assert_eq!(mime_of("a.md"), "text/markdown; charset=utf-8");
        assert_eq!(mime_of("a.html"), "text/html; charset=utf-8");
        assert_eq!(mime_of("a.png"), "image/png");
        assert_eq!(mime_of("a.gz"), "application/octet-stream");
        assert_eq!(mime_of("a.tgz"), "application/x-tar");
        assert_eq!(mime_of("noext"), "application/octet-stream");
    }
}
