//! The web server: UI, JSON API, live events, terminals and artifact serving.
//!
//! axum serves the connections, and one fallback handler does the rest: the routes (the
//! `ROUTES` table, matched against `path_safe`, with a 404 for an unknown path and a 405
//! for a method the path doesn't take), then the `security` middleware, then the handler.
//!
//! - `http`: requests, responses, HTTP errors, path decoding, file responses.
//! - `auth`: the cookie, capability tokens, the host and origin checks.
//! - `files`: the app's pages and static files, artifacts, attachments.
//! - `api`: the JSON API.
//! - `events`: the live-events hub and `/api/events`.
//! - `terminal`: ptys bridged to websockets.

pub mod api;
pub mod auth;
pub mod events;
pub mod files;
pub mod http;
mod mime_table;
pub mod terminal;

pub use auth::{check_capability, make_capability, CAP_MIN_LIFETIME};

use crate::daemon::Daemon;
use am_core::{log, Result};
use axum::extract::State;
use http::{internal_error, raised_resp, unquote_param, unquote_path, Fail, Handled, Req, Resp};
use regex::Regex;
use std::sync::{Arc, OnceLock};

pub const MAX_UPLOAD: u64 = 64 * 1024 * 1024;
pub const APP_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; \
font-src 'self' data:; connect-src 'self'; frame-src 'self'; object-src 'none'; base-uri 'self'; \
form-action 'self'; frame-ancestors 'self'";

/// What every handler can reach.
pub struct App {
    pub daemon: Arc<Daemon>,
    pub hub: Arc<events::Hub>,
    pub public_host: Option<String>,
    pub runtime: tokio::runtime::Handle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Index,
    Static,
    Capability,
    StableArtifact,
    Overview,
    Events,
    CreateProject,
    Project,
    UpdateProject,
    Archive,
    Unarchive,
    OffloadPlan,
    Offload,
    Restore,
    Upload,
    Attachment,
    Seen,
    Chat,
    Send,
    Interrupt,
    UpdateAgent,
    RetireAgent,
    Turns,
    Turn,
    AttentionSeen,
    AttentionResolve,
    RunFix,
    SetSchedule,
    RemoveSchedule,
    RunSchedule,
    Artifact,
    ProjectTerminal,
    HomeTerminal,
    Health,
    HealthCheck,
    FixLogin,
    StorageConnect,
    MemoryList,
    MemoryAdd,
    MemoryGet,
    MemoryEdit,
    MemoryRemove,
}

/// The routes, in the order they're tried. A GET route also answers HEAD.
const ROUTES: &[(&str, &str, Route)] = &[
    ("GET", "/", Route::Index),
    ("GET", "/memory", Route::Index),
    ("GET", "/terminal", Route::Index),
    ("GET", "/health", Route::Index),
    ("GET", "/p/{tail:.*}", Route::Index),
    ("GET", "/static/{path:.*}", Route::Static),
    ("GET", "/a/{token}/{slug}/{area}/{path:.*}", Route::Capability),
    ("GET", "/artifacts/{slug}/{name:.*}", Route::StableArtifact),
    ("GET", "/api/overview", Route::Overview),
    ("GET", "/api/events", Route::Events),
    ("POST", "/api/projects", Route::CreateProject),
    ("GET", "/api/projects/{slug}", Route::Project),
    ("PATCH", "/api/projects/{slug}", Route::UpdateProject),
    ("POST", "/api/projects/{slug}/archive", Route::Archive),
    ("POST", "/api/projects/{slug}/unarchive", Route::Unarchive),
    ("GET", "/api/projects/{slug}/offload", Route::OffloadPlan),
    ("POST", "/api/projects/{slug}/offload", Route::Offload),
    ("POST", "/api/projects/{slug}/restore", Route::Restore),
    ("POST", "/api/projects/{slug}/attachments", Route::Upload),
    ("GET", "/api/projects/{slug}/attachments/{name}", Route::Attachment),
    ("POST", "/api/projects/{slug}/seen", Route::Seen),
    ("GET", "/api/projects/{slug}/agents/{agent}/chat", Route::Chat),
    ("POST", "/api/projects/{slug}/agents/{agent}/messages", Route::Send),
    ("POST", "/api/projects/{slug}/agents/{agent}/interrupt", Route::Interrupt),
    ("PATCH", "/api/projects/{slug}/agents/{agent}", Route::UpdateAgent),
    ("POST", "/api/projects/{slug}/agents/{agent}/retire", Route::RetireAgent),
    ("GET", "/api/projects/{slug}/agents/{agent}/turns", Route::Turns),
    ("GET", "/api/projects/{slug}/agents/{agent}/turns/{turn}", Route::Turn),
    ("POST", "/api/projects/{slug}/attention/{item}/seen", Route::AttentionSeen),
    ("POST", "/api/projects/{slug}/attention/{item}/resolve", Route::AttentionResolve),
    ("GET", "/api/projects/{slug}/attention/{item}/run", Route::RunFix),
    ("POST", "/api/projects/{slug}/schedules", Route::SetSchedule),
    ("DELETE", "/api/projects/{slug}/schedules/{name}", Route::RemoveSchedule),
    ("POST", "/api/projects/{slug}/schedules/{name}/run", Route::RunSchedule),
    ("GET", "/api/projects/{slug}/artifact", Route::Artifact),
    ("GET", "/api/projects/{slug}/terminal", Route::ProjectTerminal),
    ("GET", "/api/terminal", Route::HomeTerminal),
    ("GET", "/api/health", Route::Health),
    ("POST", "/api/health/check", Route::HealthCheck),
    ("GET", "/api/health/logins/{name}/fix", Route::FixLogin),
    ("GET", "/api/storage/connect/{provider}", Route::StorageConnect),
    ("GET", "/api/memory", Route::MemoryList),
    ("POST", "/api/memory", Route::MemoryAdd),
    ("GET", "/api/memory/{name}", Route::MemoryGet),
    ("PUT", "/api/memory/{name}", Route::MemoryEdit),
    ("DELETE", "/api/memory/{name}", Route::MemoryRemove),
];

struct Compiled {
    method: &'static str,
    plain: Option<&'static str>,
    pattern: Option<Regex>,
    names: Vec<String>,
    route: Route,
}

/// A route pattern as a regex: `{name}` is `[^{}/]+`, `{name:re}` is `re`.
fn compile(pattern: &'static str) -> (Option<Regex>, Vec<String>) {
    if !pattern.contains('{') {
        return (None, vec![]);
    }
    let mut re = String::from("^");
    let mut names = Vec::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        re.push_str(&regex::escape(&rest[..open]));
        let close = rest[open..].find('}').unwrap() + open;
        let inner = &rest[open + 1..close];
        let (name, expr) = inner.split_once(':').unwrap_or((inner, "[^{}/]+"));
        re.push_str(&format!("(?P<{name}>{expr})"));
        names.push(name.to_string());
        rest = &rest[close + 1..];
    }
    re.push_str(&regex::escape(rest));
    re.push('$');
    (Some(Regex::new(&re).unwrap()), names)
}

fn routes() -> &'static [Compiled] {
    static TABLE: OnceLock<Vec<Compiled>> = OnceLock::new();
    TABLE.get_or_init(|| {
        ROUTES
            .iter()
            .map(|(method, pattern, route)| {
                let (regex, names) = compile(pattern);
                Compiled { method, plain: if regex.is_none() { Some(pattern) } else { None }, pattern: regex, names, route: *route }
            })
            .collect()
    })
}

pub enum Resolved {
    Found(Route, Vec<(String, String)>),
    NotAllowed(Vec<String>),
    NotFound,
}

/// The first route whose path and method match; else 405 with every method allowed for
/// the path, or 404.
pub fn resolve(method: &str, path_safe: &str) -> Resolved {
    let mut allowed: Vec<String> = Vec::new();
    for compiled in routes() {
        let params = if let Some(plain) = compiled.plain {
            if plain != path_safe {
                continue;
            }
            vec![]
        } else {
            let Some(caps) = compiled.pattern.as_ref().unwrap().captures(path_safe) else { continue };
            compiled.names.iter().map(|n| (n.clone(), unquote_param(caps.name(n).map_or("", |m| m.as_str())))).collect()
        };
        let methods: &[&str] = if compiled.method == "GET" { &["GET", "HEAD"] } else { std::slice::from_ref(&compiled.method) };
        if methods.contains(&method) {
            return Resolved::Found(compiled.route, params);
        }
        for m in methods {
            if !allowed.iter().any(|a| a == m) {
                allowed.push(m.to_string());
            }
        }
    }
    if allowed.is_empty() {
        Resolved::NotFound
    } else {
        allowed.sort();
        Resolved::NotAllowed(allowed)
    }
}

fn is_streaming(route: Route) -> bool {
    matches!(route, Route::Events | Route::RunFix | Route::ProjectTerminal | Route::HomeTerminal | Route::FixLogin | Route::StorageConnect)
}

async fn run_handler(app: Arc<App>, resolved: Resolved, mut req: Req) -> Handled {
    let route = match resolved {
        Resolved::Found(route, params) => {
            req.params = params;
            route
        }
        Resolved::NotAllowed(allowed) => return Err(Fail::Http(Box::new(raised_resp(405, None).with_header("Allow", allowed.join(","))))),
        Resolved::NotFound => return Err(http::not_found()),
    };
    if is_streaming(route) {
        return match route {
            Route::Events => events::api_events(app, req).await,
            _ => terminal::handle(app, route, req).await,
        };
    }
    let blocking_app = app.clone();
    match tokio::task::spawn_blocking(move || http::catch(|| handle_sync(&blocking_app, route, &req))).await {
        Ok(result) => result,
        Err(e) => Err(Fail::Internal(e.to_string())),
    }
}

fn handle_sync(app: &App, route: Route, req: &Req) -> Handled {
    use Route::*;
    match route {
        Index => files::index(req),
        Static => files::static_file(req),
        Capability => files::serve_capability(req),
        StableArtifact => files::stable_artifact(req),
        Artifact => files::api_artifact(req),
        Upload => files::api_upload(req),
        Attachment => files::api_attachment(req),
        _ => api::handle(app, route, req),
    }
}

fn unexpected(req: &Req, error: &str) -> Resp {
    log::error("am.web", &format!("Error handling request {} {}\n{error}", req.parts.method, req.parts.uri));
    internal_error()
}

/// The `security` middleware, around the handler.
async fn security(app: Arc<App>, req: Req, resolved: Resolved) -> Resp {
    if let Err(fail) = auth::check_host(app.public_host.as_deref(), &req) {
        return match fail {
            Fail::Http(resp) => *resp,
            other => unexpected(&req, &fail_text(&other)),
        };
    }
    let path = req.path.clone();
    let summary = format!("{} {}", req.parts.method, req.parts.uri);
    if path.starts_with("/a/") {
        // Authorized by its capability, and has its own headers.
        return match run_handler(app, resolved, req).await {
            Ok(resp) => resp,
            Err(Fail::Http(resp)) => *resp,
            Err(other) => {
                log::error("am.web", &format!("Error handling request {summary}\n{}", fail_text(&other)));
                internal_error()
            }
        };
    }
    if path.starts_with("/api/") || path.starts_with("/artifacts/") {
        match auth::authed(&req) {
            Ok(true) => {}
            Ok(false) => return raised_resp(401, Some("not signed in; open the address from `am url`")),
            Err(e) => return unexpected(&req, &e.to_string()),
        }
        let method = req.parts.method.as_str();
        let upgrade = req.header("upgrade").unwrap_or("").to_lowercase() == "websocket";
        let unsafe_ = !(method == "GET" || method == "HEAD") || upgrade;
        if unsafe_ && !auth::is_same_origin(&req) {
            return raised_resp(403, Some("cross-origin request refused"));
        }
    }
    let mut resp = match run_handler(app, resolved, req).await {
        Ok(resp) => resp,
        Err(Fail::Http(resp)) => return *resp,
        Err(Fail::Am(message)) => Resp::json(400, &serde_json::json!({"error": message})),
        Err(Fail::BadJson) => Resp::json(400, &serde_json::json!({"error": "the request body isn't valid JSON"})),
        Err(Fail::Internal(e)) => {
            log::error("am.web", &format!("Error handling request {summary}\n{e}"));
            return internal_error();
        }
    };
    if !resp.is_prepared() {
        resp.set_default("Content-Security-Policy", APP_CSP);
        resp.set_default("X-Content-Type-Options", "nosniff");
        resp.set_default("Referrer-Policy", "no-referrer");
    }
    resp
}

fn fail_text(fail: &Fail) -> String {
    match fail {
        Fail::Http(resp) => format!("HTTP {}", resp.status),
        Fail::Am(message) => message.clone(),
        Fail::BadJson => "the request body isn't valid JSON".into(),
        Fail::Internal(e) => e.clone(),
    }
}

async fn dispatch(State(app): State<Arc<App>>, request: axum::extract::Request) -> axum::response::Response {
    let (parts, body) = request.into_parts();
    let raw_path = parts.uri.path().to_string();
    let path = unquote_path(&raw_path, false);
    let path_safe = unquote_path(&raw_path, true);
    let query = http::parse_query(parts.uri.query());
    let declared: Option<u64> = parts.headers.get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
    let upgrade = parts.headers.get("upgrade").and_then(|v| v.to_str().ok()).is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let (body, too_large) = if upgrade {
        (Default::default(), None)
    } else if declared.is_some_and(|n| n > MAX_UPLOAD) {
        (Default::default(), declared)
    } else {
        match axum::body::to_bytes(body, MAX_UPLOAD as usize).await {
            Ok(bytes) => (bytes, None),
            Err(_) => (Default::default(), Some(MAX_UPLOAD + 1)),
        }
    };
    let resolved = resolve(parts.method.as_str(), &path_safe);
    let req = Req { method: parts.method.to_string(), path, path_safe, query, parts, body, too_large, params: vec![] };
    security(app, req, resolved).await.into_response()
}

/// The running server: where it listens, and how to stop it.
pub struct Server {
    pub host: String,
    pub port: u16,
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    hub_task: tokio::task::JoinHandle<()>,
}

impl Server {
    /// Stop: no new connections, a second for the requests in flight, then the rest
    /// (event streams, terminals) are dropped.
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        let mut task = self.task;
        if tokio::time::timeout(std::time::Duration::from_secs(1), &mut task).await.is_err() {
            task.abort();
        }
        self.hub_task.abort();
    }
}

/// Listen on `[web] host` and `port` (0 for any free one).
pub async fn start_server(daemon: Arc<Daemon>) -> Result<Server> {
    let cfg = daemon.cfg.clone();
    let host = cfg["web"]["host"].as_str().unwrap_or("127.0.0.1").to_string();
    let port = cfg["web"]["port"].as_u64().unwrap_or(8737) as u16;
    let public_host = crate::daemon::public_host(Some(&cfg))?;
    let hub = Arc::new(events::Hub::new());
    let app = Arc::new(App { daemon: daemon.clone(), hub: hub.clone(), public_host, runtime: tokio::runtime::Handle::current() });
    let listener = match tokio::net::TcpListener::bind((host.as_str(), port)).await {
        Ok(listener) => listener,
        Err(e) => {
            return Err(am_core::Error::Other(format!(
                "can't listen on {host}:{port}: {}",
                e.to_string().split(" (os error").next().unwrap_or("").to_lowercase()
            )))
        }
    };
    let actual = listener.local_addr()?.port();
    let router = axum::Router::new().fallback(dispatch).with_state(app);
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        let shutdown = async move {
            let _ = stopped.wait_for(|s| *s).await;
        };
        if let Err(e) = axum::serve(listener, router).with_graceful_shutdown(shutdown).await {
            log::error("am.web", &format!("the web server failed: {e}"));
        }
    });
    let hub_task = tokio::spawn(hub.run(daemon));
    Ok(Server { host, port: actual, stop, task, hub_task })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(method: &str, path: &str) -> Option<(Route, Vec<(String, String)>)> {
        match resolve(method, &unquote_path(path, true)) {
            Resolved::Found(route, params) => Some((route, params)),
            _ => None,
        }
    }

    #[test]
    fn routes_match_paths_and_methods() {
        assert_eq!(found("GET", "/").unwrap().0, Route::Index);
        assert_eq!(found("HEAD", "/static/app.js").unwrap().1, vec![("path".into(), "app.js".into())]);
        assert_eq!(found("GET", "/static/%2e%2e/config.toml").unwrap().1, vec![("path".into(), "../config.toml".into())]);
        assert_eq!(found("GET", "/p/").unwrap().0, Route::Index);
        let (route, params) = found("POST", "/api/projects/demo/attention/..%2F..%2Fother%2Fproject/resolve").unwrap();
        assert_eq!(route, Route::AttentionResolve);
        assert_eq!(params[1], ("item".into(), "../../other/project".into()));
        assert!(matches!(resolve("DELETE", "/api/overview"), Resolved::NotAllowed(a) if a == ["GET", "HEAD"]));
        assert!(matches!(resolve("POST", "/api/memory/x"), Resolved::NotAllowed(a) if a == ["DELETE", "GET", "HEAD", "PUT"]));
        assert!(matches!(resolve("GET", "/api/projects"), Resolved::NotAllowed(a) if a == ["POST"]));
        assert!(matches!(resolve("GET", "/api/nope"), Resolved::NotFound));
        assert!(matches!(resolve("GET", "/api/memory/a/b"), Resolved::NotFound));
    }
}
