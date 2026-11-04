//! Terminals (`web.Pty`, `_bridge`, `_terminal`, `_run_command`): a process on a
//! pseudo-terminal, bridged to a websocket. The project and home terminals attach to a
//! tmux session that outlives the page; Run, Fix and storage Connect run a command the
//! server chooses, never one the request names.

use super::api::{slug, which};
use super::http::{raised, Fail, Handled, Req, Resp};
use super::{App, Route};
use crate::deps;
use am_core::{am_home, project_dir, HOME_SLUG, MANAGER};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::FromRequestParts;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::unix::AsyncFd;

pub struct Pty {
    master: Option<AsyncFd<OwnedFd>>,
    child: tokio::process::Child,
    pid: i32,
}

impl Pty {
    /// Start `argv` with the pty as its controlling terminal (so it gets SIGWINCH on a
    /// resize), in a session of its own.
    pub fn start(argv: &[String], cwd: &std::path::Path, env: &[(String, String)]) -> std::io::Result<Pty> {
        let (mut master_fd, mut slave_fd) = (-1, -1);
        if unsafe { libc::openpty(&mut master_fd, &mut slave_fd, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        use std::os::fd::FromRawFd;
        let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        let mut command = tokio::process::Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(cwd)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(std::process::Stdio::from(slave.try_clone()?))
            .stdout(std::process::Stdio::from(slave.try_clone()?))
            .stderr(std::process::Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        let pid = child.id().unwrap_or(0) as i32;
        #[allow(deprecated)] // The fd is owned here, so the soundness concern doesn't apply.
        let master = AsyncFd::new(master)?;
        Ok(Pty { master: Some(master), child, pid })
    }

    /// The next output, or None at the end (EOF, or EIO once the terminal is gone).
    pub async fn read(&self) -> Option<Vec<u8>> {
        let master = self.master.as_ref()?;
        loop {
            let mut guard = master.readable().await.ok()?;
            let mut buf = vec![0u8; 65536];
            let result = guard.try_io(|fd| {
                let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if n < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match result {
                Ok(Ok(0)) => return None,
                Ok(Ok(n)) => {
                    buf.truncate(n);
                    return Some(buf);
                }
                Ok(Err(_)) => return None,
                Err(_would_block) => continue,
            }
        }
    }

    pub fn write(&self, data: &[u8]) {
        if let Some(master) = &self.master {
            let mut rest = data;
            while !rest.is_empty() {
                let n = unsafe { libc::write(master.as_raw_fd(), rest.as_ptr() as *const libc::c_void, rest.len()) };
                if n <= 0 {
                    return; // contextlib.suppress(OSError)
                }
                rest = &rest[n as usize..];
            }
        }
    }

    pub fn resize(&self, cols: i64, rows: i64) {
        let (Ok(cols), Ok(rows)) = (u16::try_from(cols), u16::try_from(rows)) else { return };
        if let Some(master) = &self.master {
            let size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
            unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
        }
    }

    /// Close the terminal and wait for the process: up to 2 s, then its group is killed.
    pub async fn close(mut self) -> Option<i32> {
        self.master = None;
        let code = |status: std::process::ExitStatus| {
            use std::os::unix::process::ExitStatusExt;
            status.code().unwrap_or_else(|| -status.signal().unwrap_or(0))
        };
        match tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await {
            Ok(Ok(status)) => Some(code(status)),
            Ok(Err(_)) => None,
            Err(_) => {
                if self.pid > 1 {
                    unsafe { libc::killpg(self.pid, libc::SIGKILL) };
                }
                self.child.wait().await.ok().map(code)
            }
        }
    }
}

/// Pump the pty to the socket and the socket to the pty until either ends. Returns
/// whether the socket is still open.
async fn bridge(socket: &mut WebSocket, pty: &Pty) -> bool {
    let (mut sink, mut stream) = socket.split();
    let output = async {
        while let Some(data) = pty.read().await {
            if sink.send(Message::Binary(data.into())).await.is_err() {
                return false;
            }
        }
        true
    };
    let input = async {
        while let Some(message) = stream.next().await {
            match message {
                Ok(Message::Binary(data)) => pty.write(&data),
                Ok(Message::Text(text)) => {
                    let Ok(control) = serde_json::from_str::<serde_json::Value>(text.as_str()) else { continue };
                    let int = |key: &str| -> Option<i64> {
                        let v = control.get(key)?;
                        v.as_i64().or_else(|| v.as_f64().map(|f| f.trunc() as i64)).or_else(|| v.as_str().and_then(am_core::py::parse_int))
                    };
                    match control.get("type").and_then(|t| t.as_str()) {
                        Some("resize") => {
                            if let (Some(cols), Some(rows)) = (int("cols"), int("rows")) {
                                pty.resize(cols, rows);
                            }
                        }
                        Some("input") => {
                            if let Some(data) = control.get("data") {
                                pty.write(am_core::py::str_value(data).as_bytes());
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
        false
    };
    tokio::select! {
        open = output => open,
        open = input => open,
    }
}

fn exit_message(code: Option<i32>) -> Message {
    Message::Text(am_core::json::dumps_ascii(&json!({"type": "exit", "code": code})).into())
}

fn terminal_env(slug: Option<&str>) -> am_core::Result<Vec<(String, String)>> {
    let mut env: Vec<(String, String)> = deps::turn_env(slug.unwrap_or(HOME_SLUG), MANAGER)?
        .into_iter()
        .filter(|(k, _)| !matches!(k.as_str(), "AM_AGENT" | "AM_TURN" | "TMUX" | "TMUX_PANE"))
        .filter(|(k, _)| slug.is_some() || k != "AM_PROJECT")
        .collect();
    let mut set = |key: &str, value: &str, overwrite: bool| match env.iter_mut().find(|(k, _)| k == key) {
        Some(entry) if overwrite => entry.1 = value.to_string(),
        Some(_) => {}
        None => env.push((key.to_string(), value.to_string())),
    };
    set("TERM", "xterm-256color", true);
    set("COLORTERM", "truecolor", true);
    set("LANG", "en_US.UTF-8", false);
    Ok(env)
}

fn query_int(req: &Req, key: &str, default: i64) -> Result<i64, ()> {
    match req.query(key) {
        None => Ok(default),
        Some(text) => am_core::py::parse_int(text).ok_or(()),
    }
}

async fn upgrade(req: &mut Req, max: usize) -> Result<WebSocketUpgrade, Fail> {
    match WebSocketUpgrade::from_request_parts(&mut req.parts, &()).await {
        Ok(ws) => Ok(ws.max_message_size(max)),
        Err(_) => Err(raised(400, Some("No WebSocket UPGRADE hdr"))),
    }
}

const MAX_MESSAGE: usize = 4 * 1024 * 1024;

/// The project and home terminals: `tmux new-session -A`, so the session outlives the page.
async fn terminal(mut req: Req, slug: Option<String>) -> Handled {
    let ws = upgrade(&mut req, MAX_MESSAGE).await?;
    let size = (query_int(&req, "cols", 80), query_int(&req, "rows", 24));
    let response = ws.on_upgrade(move |mut socket| async move {
        if which("tmux").is_none() {
            let error = json!({"type": "error", "message": "tmux isn't installed. On a Mac: brew install tmux"});
            let _ = socket.send(Message::Text(am_core::json::dumps_ascii(&error).into())).await;
            let _ = socket.close().await;
            return;
        }
        let session = format!("am-{}", slug.as_deref().unwrap_or(HOME_SLUG));
        let cwd = match &slug {
            Some(slug) => project_dir(slug),
            None => am_home(),
        };
        let mut argv: Vec<String> = ["tmux", "-u", "new-session", "-A", "-s", &session, "-c"].iter().map(|s| s.to_string()).collect();
        argv.push(cwd.to_string_lossy().into_owned());
        argv.push("-e".into());
        argv.push(format!("AM_HOME={}", am_home().to_string_lossy()));
        if let Some(slug) = &slug {
            argv.push("-e".into());
            argv.push(format!("AM_PROJECT={slug}"));
        }
        let pty = match terminal_env(slug.as_deref()).map_err(|e| e.to_string()).and_then(|env| Pty::start(&argv, &cwd, &env).map_err(|e| e.to_string())) {
            Ok(pty) => pty,
            Err(e) => {
                am_core::log::error("am.web", &format!("couldn't start a terminal: {e}"));
                let _ = socket.close().await;
                return;
            }
        };
        if let (Ok(cols), Ok(rows)) = size {
            pty.resize(cols, rows);
        }
        let open = bridge(&mut socket, &pty).await;
        let code = pty.close().await;
        if open {
            let _ = socket.send(exit_message(code)).await;
            let _ = socket.close().await;
        }
    });
    Ok(Resp::prepared(response))
}

type After = Box<dyn FnOnce(Option<i32>) -> Option<String> + Send>;

/// A command the server holds, in a terminal in the UI. `after(exit_code)` runs once it has
/// ended, and the terminal shows any line it returns.
async fn run_command(mut req: Req, command: String, slug: String, after: After) -> Handled {
    let ws = upgrade(&mut req, MAX_MESSAGE).await?;
    let size = (query_int(&req, "cols", 100), query_int(&req, "rows", 28));
    let response = ws.on_upgrade(move |mut socket| async move {
        let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
        let argv = vec![shell, "-lc".into(), command];
        let cwd = project_dir(&slug);
        let pty = match terminal_env(Some(&slug)).map_err(|e| e.to_string()).and_then(|env| Pty::start(&argv, &cwd, &env).map_err(|e| e.to_string())) {
            Ok(pty) => pty,
            Err(e) => {
                am_core::log::error("am.web", &format!("couldn't start a command: {e}"));
                let _ = socket.close().await;
                return;
            }
        };
        if let (Ok(cols), Ok(rows)) = size {
            pty.resize(cols, rows);
        }
        let open = bridge(&mut socket, &pty).await;
        let code = pty.close().await;
        let note = tokio::task::spawn_blocking(move || after(code)).await.ok().flatten();
        if open {
            if let Some(note) = note {
                let _ = socket.send(Message::Binary(format!("\r\n{note}\r\n").into_bytes().into())).await;
            }
            let _ = socket.send(exit_message(code)).await;
            let _ = socket.close().await;
        }
    });
    Ok(Resp::prepared(response))
}

pub async fn handle(app: Arc<App>, route: Route, req: Req) -> Handled {
    let daemon = app.daemon.clone();
    match route {
        Route::ProjectTerminal => {
            let slug = tokio::task::block_in_place(|| slug(&req))?;
            terminal(req, Some(slug)).await
        }
        Route::HomeTerminal => terminal(req, None).await,
        Route::RunFix => {
            // The Run button: an item's fix command, in a terminal in the UI.
            let (slug, item) = tokio::task::block_in_place(|| -> Result<_, Fail> {
                let slug = slug(&req)?;
                Ok((slug.clone(), am_store::items::get_attention(&slug, req.param("item"))?))
            })?;
            let Some(run) = item.get("run").filter(|r| am_core::py::truthy(Some(r))) else {
                return Err(raised(400, Some("this item has no command to run")));
            };
            let run = am_core::py::str_value(run);
            let login = deps::item_login(&item)?;
            let after: After = Box::new(move |_| {
                daemon.request_login_check(None, &login.into_iter().collect::<Vec<_>>());
                None
            });
            run_command(req, run, slug, after).await
        }
        Route::FixLogin => {
            // The Fix button on the Health page. A request names a login, and can't say what to run.
            let name = req.param("name").to_string();
            let (known, command) = tokio::task::block_in_place(|| deps::fix_for(&name))?;
            if !known {
                return Err(raised(404, Some(&format!("no login {name}"))));
            }
            let Some(command) = command.filter(|c| !c.is_empty()) else {
                return Err(raised(409, Some(&format!("{name} has nothing to fix right now"))));
            };
            let after: After = Box::new(move |_| {
                daemon.request_login_check(Some(std::slice::from_ref(&name)), std::slice::from_ref(&name));
                None
            });
            run_command(req, command, HOME_SLUG.into(), after).await
        }
        Route::StorageConnect => {
            // Connect under Storage in Settings: log in to a provider in a terminal, then
            // point [storage] at it. A request names the provider, and can't say what to run.
            let provider = req.param("provider").to_string();
            let Some(command) = deps::connect_command(&provider)? else {
                return Err(raised(404, Some(&format!("no storage provider {provider}"))));
            };
            let cfg = am_core::config::load()?;
            if let Some(current) = cfg["storage"].get("rclone_remote").filter(|r| am_core::py::truthy(Some(r))) {
                let current = am_core::py::str_value(current);
                return Err(raised(409, Some(&format!("storage already uses the remote {current}; reconnect it from the Health page"))));
            }
            if which("rclone").is_none() {
                return Err(raised(409, Some("rclone isn't installed; on a Mac run `brew install rclone`")));
            }
            let after: After = Box::new(move |code| {
                if code != Some(0) {
                    return None;
                }
                let remote = match deps::connected(&provider) {
                    Ok(remote) => remote,
                    Err(e) => {
                        am_core::log::warning("am.web", &format!("couldn't record the new storage remote: {e}"));
                        return Some(format!("Logged in, but storage isn't set up: {e}"));
                    }
                };
                daemon.reload_config();
                if let Ok(name) = deps::storage_login(&remote) {
                    daemon.request_login_check(Some(std::slice::from_ref(&name)), std::slice::from_ref(&name));
                }
                am_core::poke(HOME_SLUG);
                Some(format!("Connected. Offloaded data now goes to {}.", deps::remote_root().unwrap_or_default()))
            });
            run_command(req, command, HOME_SLUG.into(), after).await
        }
        other => Err(Fail::Internal(format!("{other:?} isn't a terminal route"))),
    }
}
