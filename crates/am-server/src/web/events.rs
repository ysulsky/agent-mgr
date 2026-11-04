//! Live events (`Hub`, `api_events`): tells open pages what changed, so they can fetch
//! it. CLI writes touch `<project>/.changed` and wake the daemon, which sets `hub_wake`;
//! artifact edits by agents show up in a scan of artifacts/.

use super::http::{Handled, Req, Resp};
use super::App;
use crate::daemon::Daemon;
use am_core::{project_dir, Value};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Default)]
struct Scan {
    changed: HashMap<String, i128>,
    artifacts: HashMap<String, BTreeMap<String, f64>>,
    last_artifact_scan: Option<Instant>,
    scanned: bool,
}

#[derive(Default)]
pub struct Hub {
    subscribers: Mutex<Vec<(u64, mpsc::Sender<String>)>>,
    next_id: AtomicU64,
    scan: Mutex<Scan>,
}

fn scan_artifacts(slug: &str) -> BTreeMap<String, f64> {
    fn walk(dir: &std::path::Path, out: &mut BTreeMap<String, f64>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut subdirs = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            // A link to a directory counts as a directory, not a file, but isn't walked.
            let is_dir = std::fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false);
            if is_dir {
                if !name.starts_with('.') && !entry.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
                    subdirs.push(path);
                }
            } else if !name.starts_with('.') {
                if let Ok(meta) = std::fs::metadata(&path) {
                    out.insert(path.to_string_lossy().into_owned(), super::files::st_mtime(&meta));
                }
            }
        }
        for sub in subdirs {
            walk(&sub, out);
        }
    }
    let mut out = BTreeMap::new();
    walk(&project_dir(slug).join("artifacts"), &mut out);
    out
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn publish(&self, event: &Value) {
        let text = am_core::json::encode_ascii(event);
        let mut subscribers = self.subscribers.lock().unwrap();
        subscribers.retain(|(_, queue)| !matches!(queue.try_send(text.clone()), Err(mpsc::error::TrySendError::Closed(_))));
    }

    fn subscribe(self: &Arc<Self>) -> (Subscription, mpsc::Receiver<String>) {
        let (send, receive) = mpsc::channel(500);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.subscribers.lock().unwrap().push((id, send));
        (Subscription { hub: self.clone(), id }, receive)
    }

    pub fn scan(&self) -> am_core::Result<Vec<Value>> {
        let mut state = self.scan.lock().unwrap();
        let mut events = Vec::new();
        let slugs = am_store::projects::list_slugs()?;
        for slug in &slugs {
            use std::os::unix::fs::MetadataExt;
            if let Ok(meta) = std::fs::metadata(project_dir(slug).join(".changed")) {
                let mtime = meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128;
                if state.changed.get(slug) != Some(&mtime) {
                    // The first scan only learns what's there. After that, a project
                    // we haven't seen is one that was just created.
                    if state.scanned {
                        events.push(json!({"type": "changed", "project": slug}));
                    }
                    state.changed.insert(slug.clone(), mtime);
                }
            }
        }
        state.scanned = true;
        if state.last_artifact_scan.is_none_or(|at| at.elapsed() > Duration::from_secs(1)) {
            state.last_artifact_scan = Some(Instant::now());
            for slug in &slugs {
                let current = scan_artifacts(slug);
                let before = state.artifacts.insert(slug.clone(), current.clone());
                let Some(before) = before else { continue };
                if before == current {
                    continue;
                }
                let root = format!("{}/", project_dir(slug).to_string_lossy());
                let paths: BTreeSet<&String> = before.keys().chain(current.keys()).filter(|p| before.get(*p) != current.get(*p)).collect();
                let paths: Vec<String> = paths.into_iter().map(|p| p.strip_prefix(&root).unwrap_or(p).to_string()).collect();
                events.push(json!({"type": "artifacts", "project": slug, "paths": paths}));
                events.push(json!({"type": "changed", "project": slug}));
            }
        }
        Ok(events)
    }

    /// Scan on each wake, and every 0.25 s for what didn't wake the daemon.
    pub async fn run(self: Arc<Self>, daemon: Arc<Daemon>) {
        let mut failed = false;
        while !daemon.stop_event.is_set() {
            daemon.hub_wake.clear(); // Before the scan, so a wake during it brings on another.
            let hub = self.clone();
            match tokio::task::spawn_blocking(move || crate::daemon::caught(|| hub.scan())).await {
                Ok(Ok(events)) => {
                    failed = false;
                    for event in &events {
                        self.publish(event);
                    }
                }
                Ok(Err(e)) if !failed => {
                    failed = true;
                    am_core::log::exception("am.web", "watching for changes failed", &e);
                }
                _ => {}
            }
            tokio::select! {
                _ = daemon.hub_wake.wait_for(0.25) => {}
                _ = daemon.stop_event.wait() => {}
            }
        }
    }
}

struct Subscription {
    hub: Arc<Hub>,
    id: u64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.hub.subscribers.lock().unwrap().retain(|(id, _)| *id != self.id);
    }
}

/// `/api/events`: `: connected`, then `data: <event>` for each, and `: keep-alive` after
/// 20 s without one.
pub async fn api_events(app: Arc<App>, _req: Req) -> Handled {
    let (subscription, receive) = app.hub.subscribe();
    let stream = futures_util::stream::unfold((Some(subscription), receive, true), |(subscription, mut receive, first)| async move {
        let subscription = subscription?;
        if first {
            return Some((Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(b": connected\n\n")), (Some(subscription), receive, false)));
        }
        let chunk = match tokio::time::timeout(Duration::from_secs(20), receive.recv()).await {
            Ok(Some(text)) => format!("data: {text}\n\n"),
            Ok(None) => return None,
            Err(_) => ": keep-alive\n\n".to_string(),
        };
        Some((Ok(axum::body::Bytes::from(chunk)), (Some(subscription), receive, false)))
    });
    let mut response = axum::response::Response::new(axum::body::Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert("content-type", "text/event-stream".parse().unwrap());
    headers.insert("cache-control", "no-cache".parse().unwrap());
    headers.insert("x-accel-buffering", "no".parse().unwrap());
    Ok(Resp::prepared(response))
}
