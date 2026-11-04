//! The app's pages and static files, artifacts (capability URLs, the stable address and
//! the artifact API) and attachments (`web.py`, "pages", "artifacts" and part of "chat").

use super::api::slug;
use super::auth;
use super::http::{file_response, found, not_found, quote, raised, Fail, Handled, Req, Resp};
use am_core::{new_id, project_dir, py, Value};
use am_store::{artifacts, projects};
use regex::Regex;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const INLINE_IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp", "image/heic", "image/avif"];

/// `web.SELECTION_BRIDGE`, byte for byte: added to HTML artifacts shown in the panel. An
/// artifact has an opaque origin, so it can only reach the app by postMessage, and the app
/// checks which window it came from.
pub const SELECTION_BRIDGE: &str = r#"<script data-am-bridge>(function(){
var post=function(m){m.am=m.am||'x';window.parent.postMessage(m,'*')};
var timer=null;
function report(){
  var s=window.getSelection();var t=s?s.toString():'';
  if(!t.trim()||!s.rangeCount){post({am:'selection',text:''});return}
  var r=s.getRangeAt(0).getBoundingClientRect();
  post({am:'selection',text:t,rect:{left:r.left,top:r.top,right:r.right,bottom:r.bottom}});
}
document.addEventListener('selectionchange',function(){clearTimeout(timer);timer=setTimeout(report,80)});
window.addEventListener('scroll',function(){clearTimeout(timer);timer=setTimeout(report,50)},true);
window.addEventListener('resize',function(){clearTimeout(timer);timer=setTimeout(report,50)});
document.addEventListener('keydown',function(e){
  if(e.key==='Escape'){post({am:'key',key:'Escape'});return}
  if(e.metaKey||e.ctrlKey||e.altKey)return;
  var s=window.getSelection();
  if(s&&s.toString().trim()&&e.key.length===1){e.preventDefault();post({am:'key',key:e.key})}
});
window.addEventListener('message',function(e){
  if(e.source===window.parent&&e.data&&e.data.am==='clear-selection'){var s=window.getSelection();if(s)s.removeAllRanges()}
});
post({am:'ready'});
})();</script>"#;

// ---------------------------------------------------------------- pages

fn signed_out_page(message: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>Agent Mgr</title>\
<style>body{{font:16px system-ui;margin:20vh auto;max-width:32rem;padding:0 1rem;color:#333}}\
@media(prefers-color-scheme:dark){{body{{background:#17181a;color:#ddd}}}}</style>\
<h1>Agent Mgr</h1><p>{message}</p>"
    )
}

/// `/`, and the app's other pages: sign in with `?token=`, or the app if signed in.
pub fn index(req: &Req) -> Handled {
    if let Some(token) = req.query("token").filter(|t| !t.is_empty()) {
        if !auth::same(token.as_bytes(), crate::daemon::web_token(false)?.as_bytes()) {
            return Ok(Resp::html(401, signed_out_page("That access token isn't valid.")));
        }
        // A page rather than a redirect: a redirect from a link on another site would be
        // cross-site, and a SameSite=Strict cookie wouldn't be sent on it.
        let page = r#"<!doctype html><meta charset=utf-8><meta http-equiv="refresh" content="0;url=/"><title>Agent Mgr</title>"#;
        let cookie = format!("{}={}; HttpOnly; Max-Age=31536000; Path=/; SameSite=Strict", auth::COOKIE, auth::session_value()?);
        return Ok(Resp::html(200, page).with_header("Set-Cookie", cookie));
    }
    if !auth::authed(req)? {
        return Ok(Resp::html(401, signed_out_page("Run `am url` and open the address it prints.")));
    }
    file_response(req, &am_core::assets::webui_dir().join("index.html"), vec![("Cache-Control".into(), "no-cache".into())])
}

/// `Path.resolve()` for a path that must exist, or None.
fn resolved(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

fn strictly_inside(root: &Path, target: &Path) -> bool {
    target != root && target.starts_with(root)
}

pub fn static_file(req: &Req) -> Handled {
    let root = resolved(&am_core::assets::webui_dir()).ok_or_else(not_found)?;
    let target = resolved(&root.join(req.param("path"))).ok_or_else(not_found)?;
    if !strictly_inside(&root, &target) || !target.is_file() {
        return Err(not_found());
    }
    file_response(req, &target, vec![("Cache-Control".into(), "no-cache".into())])
}

// ---------------------------------------------------------------- artifacts

fn rev_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[0-9a-f]{4,40}$").unwrap())
}

fn capability_base(req: &Req, slug: &str, token: &str) -> String {
    format!("http://{}/a/{token}/{slug}/", req.host())
}

/// Agent-written HTML may run scripts. A CSP `sandbox` without allow-same-origin gives
/// it an opaque origin, so it has no cookies and no access to the UI or its storage. It
/// can't reach the network either: connect-src and form-action are 'none', and everything
/// it loads must come from this project's capability, or be inline.
fn artifact_csp(req: &Req, slug: &str, token: &str, sandboxed: bool) -> String {
    let base = capability_base(req, slug, token);
    if !sandboxed {
        return "frame-ancestors 'self'".into();
    }
    let source = format!("{base} 'unsafe-inline' 'unsafe-eval' data: blob:");
    format!(
        "sandbox allow-scripts allow-popups allow-popups-to-escape-sandbox allow-downloads allow-modals; \
default-src 'none'; \
script-src {source}; style-src {source}; img-src {base} data: blob:; font-src {base} data:; media-src {base} data: blob:; \
connect-src 'none'; form-action 'none'; frame-ancestors 'self'"
    )
}

fn inject_bridge(data: Vec<u8>) -> Vec<u8> {
    let bridge = SELECTION_BRIDGE.as_bytes();
    let lower = data.to_ascii_lowercase();
    match lower.windows(7).rposition(|w| w == b"</body>") {
        Some(at) => [&data[..at], bridge, &data[at..]].concat(),
        None => [&data[..], bridge].concat(),
    }
}

fn clean_relative(path: &str) -> Result<String, Fail> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    if parts.is_empty() || parts.iter().any(|p| *p == ".." || p.contains('\0') || p.contains('\\')) {
        return Err(not_found());
    }
    Ok(parts.join("/"))
}

/// `/a/<token>/<slug>/{artifacts,results}/<path>`, and `artifacts@<rev>` for an old
/// version, whose relative links then resolve within that same version.
pub fn serve_capability(req: &Req) -> Handled {
    let token = req.param("token");
    let slug = req.param("slug");
    if !am_core::ids::slug_re().is_match(slug) || !auth::check_capability(token, slug, None)? {
        return Err(raised(403, Some("this link has expired or is invalid")));
    }
    if !projects::exists(slug) {
        return Err(not_found());
    }
    let area = req.param("area");
    let rel = clean_relative(req.param("path"))?;
    let mut rev = None;
    if let Some(r) = area.strip_prefix("artifacts@") {
        if !rev_re().is_match(r) {
            return Err(not_found());
        }
        rev = Some(r);
    } else if area != "artifacts" && area != "results" {
        return Err(not_found());
    }
    let name = rel.rsplit('/').next().unwrap_or("");
    if name.starts_with('.') {
        return Err(not_found());
    }
    let mut data = match rev {
        Some(rev) => match artifacts::show(slug, &rel, rev) {
            Ok(data) => data,
            Err(am_core::Error::Am(_)) => return Err(not_found()),
            Err(e) => return Err(e.into()),
        },
        None => {
            let target = artifacts::contained(slug, area, &rel).ok_or_else(not_found)?;
            std::fs::read(target)?
        }
    };
    let mut mime = artifacts::mime_of(&rel);
    let kind = artifacts::kind_of(&rel);
    if kind == "markdown" {
        mime = "text/plain; charset=utf-8".into();
    }
    if kind == "html" && req.query("embed") == Some("1") {
        data = inject_bridge(data);
    }
    // Browsers' PDF viewers refuse to run in a sandbox. A PDF can't script this origin,
    // so only what really is served as one goes without.
    let csp = artifact_csp(req, slug, token, mime != "application/pdf");
    let cache = if rev.is_some() { "private, max-age=3600, immutable" } else { "no-cache" };
    Ok(Resp::new(200, &mime, data)
        .with_header("Content-Security-Policy", csp)
        .with_header("X-Content-Type-Options", "nosniff")
        .with_header("Referrer-Policy", "no-referrer")
        .with_header("Cache-Control", cache))
}

pub fn capability_url(slug: &str, rel_path: &str, rev: Option<&str>, embed: bool) -> am_core::Result<String> {
    let token = auth::make_capability(slug, None)?;
    let mut parts: Vec<String> = rel_path.split('/').map(String::from).collect();
    if let Some(rev) = rev {
        parts[0] = format!("{}@{rev}", parts[0]);
    }
    let quoted: Vec<String> = parts.iter().map(|p| quote(p, "@")).collect();
    Ok(format!("/a/{token}/{slug}/{}{}", quoted.join("/"), if embed { "?embed=1" } else { "" }))
}

fn artifact_path(path: &str) -> am_core::Result<String> {
    Ok(format!("artifacts/{}", artifacts::clean_name(path)?))
}

fn http_url(v: Option<&Value>) -> Option<String> {
    let url = v?.as_str()?;
    (url.starts_with("http://") || url.starts_with("https://")).then(|| url.to_string())
}

/// `/artifacts/<slug>/<name>`: the address that doesn't expire. It takes the cookie,
/// and sends the browser on to a fresh capability URL.
pub fn stable_artifact(req: &Req) -> Handled {
    let slug = slug(req)?;
    let path = artifact_path(req.param("name"))?;
    let rev = req.query("rev").filter(|r| !r.is_empty());
    if path.ends_with(".link") {
        if let Some(rev) = rev {
            let data = artifacts::show(&slug, &path, rev)?;
            if let Ok(parsed) = serde_json::from_slice::<Value>(&data) {
                let parsed = parsed.as_object().ok_or_else(|| Fail::Internal("a link that isn't an object".into()))?;
                if let Some(url) = http_url(parsed.get("url")) {
                    return Err(found(&url));
                }
            }
        } else {
            let inside = artifacts::contained(&slug, "artifacts", path.split_once('/').map_or("", |(_, rest)| rest));
            let info = match inside {
                Some(inside) => artifacts::read_link(&inside),
                None => json!({}),
            };
            if py::truthy(info.get("url")) {
                return Err(found(info["url"].as_str().unwrap_or("")));
            }
        }
    }
    Err(found(&capability_url(&slug, &path, rev, false)?))
}

/// `os.stat(...).st_mtime`, computed as CPython does.
pub fn st_mtime(meta: &std::fs::Metadata) -> f64 {
    use std::os::unix::fs::MetadataExt;
    meta.mtime() as f64 + meta.mtime_nsec() as f64 * 1e-9
}

/// `bytes.decode(errors="replace")`.
pub fn decode_replace(data: &[u8]) -> String {
    String::from_utf8_lossy(data).into_owned()
}

pub fn api_artifact(req: &Req) -> Handled {
    let slug = slug(req)?;
    let path = artifact_path(req.query("path").unwrap_or(""))?;
    let rev = req.query("rev").filter(|r| !r.is_empty());
    let name = path.split_once('/').map_or("", |(_, rest)| rest).to_string();
    if let Some(rev) = rev {
        if !rev_re().is_match(rev) {
            return Err(Fail::Am(format!("invalid revision {}", py::repr_str(rev)?)));
        }
    }
    let kind = artifacts::kind_of(&path);
    let target = artifacts::contained(&slug, "artifacts", &name).unwrap_or_else(|| project_dir(&slug).join(".missing").join(&name));
    let meta = std::fs::metadata(&target).ok().filter(|m| m.is_file());
    let exists = meta.is_some();
    let versions = artifacts::log(&slug, Some(&name), 200)?;
    if !exists && rev.is_none() && versions.is_empty() {
        return Err(raised(404, Some(&format!("no artifact {path}"))));
    }
    let unpublished = exists && rev.is_none() && artifacts::has_unpublished_changes(&slug, &name)?;
    let mut info = json!({
        "path": path,
        "name": name,
        "kind": kind,
        "exists": exists,
        "mtime": meta.as_ref().map(st_mtime),
        "size": meta.as_ref().map(|m| m.len()),
        "rev": rev,
        "versions": versions,
        "url": capability_url(&slug, &path, rev, false)?,
        "embed_url": capability_url(&slug, &path, rev, true)?,
        "stable_url": format!("/artifacts/{slug}/{}{}", quote(&name, "/"), rev.map(|r| format!("?rev={r}")).unwrap_or_default()),
        "unpublished": unpublished,
    });
    if matches!(kind, "markdown" | "text" | "link") {
        let raw = match rev {
            Some(rev) => artifacts::show(&slug, &name, rev)?,
            None if exists => std::fs::read(&target)?,
            None => vec![],
        };
        info["text"] = decode_replace(&raw[..raw.len().min(2_000_000)]).into();
        if kind == "link" {
            if rev.is_some() {
                if let Ok(Value::Object(data)) = serde_json::from_slice::<Value>(&raw) {
                    let text = |key: &str| py::str_of(Some(data.get(key).unwrap_or(&Value::String(String::new())))).unwrap_or_default();
                    let url = text("url");
                    info["link"] = json!({
                        "url": if url.starts_with("http://") || url.starts_with("https://") { url } else { String::new() },
                        "title": text("title"),
                        "description": text("description"),
                    });
                }
            } else {
                info["link"] = artifacts::read_link(&target);
            }
        }
    }
    Resp::ok(&info)
}

// ---------------------------------------------------------------- attachments

/// A part of a `multipart/form-data` body.
struct Part {
    name: Option<String>,
    filename: Option<String>,
    content_type: Option<String>,
    data: Vec<u8>,
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > haystack.len() {
        return None;
    }
    haystack[from..].windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// A Content-Disposition parameter: quoted (with backslash escapes) or a token, and
/// `name*=UTF-8''...` (RFC 5987) preferred over `name=`.
fn disposition_param(header: &str, key: &str) -> Option<String> {
    let mut plain = None;
    let mut extended = None;
    let mut rest = header;
    while let Some(semi) = rest.find(';') {
        rest = rest[semi + 1..].trim_start();
        let Some(eq) = rest.find('=') else { break };
        let param = rest[..eq].trim().to_ascii_lowercase();
        let after = &rest[eq + 1..];
        let (value, remaining) = if let Some(quoted) = after.strip_prefix('"') {
            let mut value = String::new();
            let mut chars = quoted.char_indices();
            let mut end = quoted.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, next)) = chars.next() {
                            value.push(next);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    c => value.push(c),
                }
            }
            (value, &quoted[end.min(quoted.len())..])
        } else {
            let end = after.find(';').unwrap_or(after.len());
            (after[..end].trim().to_string(), &after[end..])
        };
        if param == key {
            plain = Some(value);
        } else if param == format!("{key}*") {
            let encoded = value.split_once("''").map_or(value.as_str(), |(_, v)| v).to_string();
            extended = Some(super::http::unquote_path(&encoded, false));
        }
        rest = remaining;
    }
    extended.or(plain)
}

fn parse_multipart(content_type: &str, body: &[u8]) -> Result<Vec<Part>, Fail> {
    let boundary = disposition_param(&format!("x; {}", content_type.split_once(';').map_or("", |(_, p)| p)), "boundary")
        .filter(|b| !b.is_empty())
        .ok_or_else(|| Fail::Internal("a multipart body needs a boundary".into()))?;
    let delimiter = format!("--{boundary}").into_bytes();
    let mut parts = Vec::new();
    let Some(first) = find(body, &delimiter, 0) else { return Ok(parts) };
    let mut at = first + delimiter.len();
    loop {
        if body[at..].starts_with(b"--") {
            break;
        }
        // Past the CRLF after the delimiter.
        let Some(line_end) = find(body, b"\r\n", at) else { break };
        let headers_start = line_end + 2;
        let Some(headers_end) = find(body, b"\r\n\r\n", headers_start).or_else(|| body[headers_start..].starts_with(b"\r\n").then_some(headers_start - 2)) else {
            break;
        };
        let header_text = String::from_utf8_lossy(&body[headers_start..headers_end.max(headers_start)]).into_owned();
        let data_start = headers_end + 4;
        let mut closing = delimiter.clone();
        closing.splice(0..0, b"\r\n".iter().copied());
        let Some(data_end) = find(body, &closing, data_start) else { break };
        let mut part = Part { name: None, filename: None, content_type: None, data: body[data_start..data_end].to_vec() };
        for line in header_text.split("\r\n") {
            let Some((key, value)) = line.split_once(':') else { continue };
            let value = value.trim();
            match key.trim().to_ascii_lowercase().as_str() {
                "content-disposition" => {
                    part.name = disposition_param(value, "name");
                    part.filename = disposition_param(value, "filename");
                }
                "content-type" => part.content_type = Some(value.to_string()),
                _ => {}
            }
        }
        parts.push(part);
        at = data_end + closing.len();
    }
    Ok(parts)
}

fn safe_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[^A-Za-z0-9._-]+").unwrap())
}

pub fn api_upload(req: &Req) -> Handled {
    let slug = slug(req)?;
    // client_max_size doesn't apply to multipart streams.
    if req.content_length().unwrap_or(0) > super::MAX_UPLOAD || req.too_large.is_some() {
        return Err(Fail::Am(format!("uploads are limited to {} MB", super::MAX_UPLOAD / (1024 * 1024))));
    }
    let content_type = req.header("content-type").unwrap_or("");
    if !content_type.to_ascii_lowercase().starts_with("multipart/") {
        return Err(Fail::Internal(format!("not a multipart body: {content_type}")));
    }
    let mut saved = Vec::new();
    for part in parse_multipart(content_type, &req.body)? {
        if part.name.as_deref() != Some("file") {
            continue;
        }
        let given = part.filename.filter(|f| !f.is_empty()).unwrap_or_else(|| "file".into());
        let filename = given.rsplit('/').next().unwrap_or("").to_string();
        let replaced = safe_name_re().replace_all(&filename, "_");
        let safe = replaced.trim_start_matches('.');
        let safe = if safe.is_empty() { "file" } else { safe };
        let stored = format!("{}-{safe}", new_id("f"));
        am_core::atomic_write(&project_dir(&slug).join("attachments").join(&stored), &part.data, 0o644)?;
        saved.push(json!({
            "path": format!("attachments/{stored}"),
            "name": filename,
            "mime": part.content_type.unwrap_or_else(|| "application/octet-stream".into()),
            "size": part.data.len(),
        }));
    }
    Resp::ok(&json!({"files": saved}))
}

pub fn api_attachment(req: &Req) -> Handled {
    let slug = slug(req)?;
    let name = req.param("name");
    let root = resolved(&project_dir(&slug).join("attachments")).ok_or_else(not_found)?;
    let target = resolved(&root.join(name)).ok_or_else(not_found)?;
    if target.parent() != Some(root.as_path()) || !target.is_file() {
        return Err(not_found());
    }
    let file_name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mime = artifacts::mime_of(&file_name);
    let mut headers: Vec<(String, String)> = vec![
        ("Content-Security-Policy".into(), "sandbox; default-src 'none'".into()),
        ("X-Content-Type-Options".into(), "nosniff".into()),
        ("Cache-Control".into(), "private, max-age=86400".into()),
    ];
    if INLINE_IMAGE_TYPES.contains(&mime.as_str()) {
        headers.push(("Content-Type".into(), mime));
    } else {
        headers.push(("Content-Type".into(), "application/octet-stream".into()));
        headers.push(("Content-Disposition".into(), format!("attachment; filename=\"{file_name}\"")));
    }
    file_response(req, &target, headers)
}

/// `_clean_attachments`: attachments named in a message must be files in attachments/.
pub fn clean_attachments(slug: &str, attachments: &[Value]) -> Result<Vec<Value>, Fail> {
    let mut out = Vec::new();
    let root = resolved(&project_dir(slug).join("attachments"));
    for attachment in attachments {
        let get = |key: &str| attachment.get(key);
        let path = py::str_of(Some(get("path").unwrap_or(&Value::String(String::new())))).unwrap_or_default();
        let target = resolved(&project_dir(slug).join(&path));
        let ok = match (&root, &target) {
            (Some(root), Some(target)) => strictly_inside(root, target) && target.is_file(),
            _ => false,
        };
        if !ok {
            return Err(Fail::Am(format!("unknown attachment {path}")));
        }
        let target = target.unwrap();
        let default_name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let name = match get("name") {
            Some(v) => py::str_of(Some(v)).unwrap_or_default(),
            None => default_name,
        };
        let mime = match get("mime") {
            Some(v) => py::str_of(Some(v)).unwrap_or_default(),
            None => String::new(),
        };
        let size = std::fs::metadata(&target)?.len();
        out.push(json!({"path": path, "name": truncate(&name, 200), "mime": truncate(&mime, 100), "size": size}));
    }
    Ok(out)
}

/// `text[:n]`, in characters.
pub fn truncate(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_bodies_parse() {
        let body = b"--b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"shot one.png\"\r\nContent-Type: image/png\r\n\r\n\x89PNG\r\n--b\r\nContent-Disposition: form-data; name=\"other\"\r\n\r\nx\r\n--b--\r\n";
        let parts = parse_multipart("multipart/form-data; boundary=b", body).unwrap_or_else(|_| panic!("the multipart body didn't parse"));
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].filename.as_deref(), Some("shot one.png"));
        assert_eq!(parts[0].content_type.as_deref(), Some("image/png"));
        assert_eq!(parts[0].data, b"\x89PNG");
        assert_eq!(parts[1].name.as_deref(), Some("other"));
        assert_eq!(disposition_param("form-data; name=\"a\\\"b\"; filename*=UTF-8''%C3%A9.txt", "filename").as_deref(), Some("é.txt"));
    }

    #[test]
    fn the_bridge_goes_before_the_last_body_tag() {
        let out = inject_bridge(b"<html><BODY>x</Body></html>".to_vec());
        let text = String::from_utf8(out).unwrap();
        assert!(text.find("data-am-bridge").unwrap() < text.find("</Body>").unwrap());
        assert!(String::from_utf8(inject_bridge(b"plain".to_vec())).unwrap().ends_with("</script>"));
    }

    #[test]
    fn relative_paths_are_cleaned() {
        assert_eq!(clean_relative("a/./b//c").ok(), Some("a/b/c".into()));
        assert!(clean_relative("../x").is_err());
        assert!(clean_relative("").is_err());
        assert!(clean_relative("a\\b").is_err());
    }
}
