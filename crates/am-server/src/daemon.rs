//! The daemon: the web UI, the runner, the scheduler and the health checks.
//!
//! Stopping it doesn't stop agents: turns are detached, and the next daemon adopts them.
//!
//! The daemon process is `am daemon __run`, and `am daemon stop` checks a recorded pid's
//! command line against `\bam daemon\b` before signalling it. The daemon holds
//! `daemon/lock` (flock) for its whole life, so two can't run on one home at once.

use crate::event::Event;
use am_core::{am_err, am_home, awake_time, daemon_dir, hooks, log, now, py, read_json, write_json, Error, Result, Value};
use regex::Regex;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const LOG: &str = "am.daemon";

pub const LOCK_WAIT_SECONDS: f64 = 3.0;
/// How long `stop` and `restart` wait for the old daemon to exit, and `start` for a new one
/// to come up, in seconds the machine is awake. A daemon still there after the wait is killed.
pub const STOP_WAIT_SECONDS: f64 = 60.0;
pub const START_WAIT_SECONDS: f64 = 60.0;

/// How long a killed daemon's lock is waited for (the `kill_wait_seconds` test hook).
pub fn kill_wait_seconds() -> f64 {
    hooks::f64_or("kill_wait_seconds", 10.0)
}

/// `STOP_WAIT_SECONDS`, or the `stop_wait_seconds` test hook.
pub fn stop_wait_seconds() -> f64 {
    hooks::f64_or("stop_wait_seconds", STOP_WAIT_SECONDS)
}

/// Port of `daemon.web_token`.
pub fn web_token(create: bool) -> Result<String> {
    am_core::token::web_token(create)
}

fn lock_path() -> PathBuf {
    daemon_dir().join("lock")
}

fn try_flock(file: &File) -> bool {
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// `open(daemon_dir() / "lock", "a")`: created if the daemon directory exists.
fn open_lock() -> std::io::Result<File> {
    OpenOptions::new().append(true).create(true).open(lock_path())
}

fn lock_is_held() -> bool {
    match open_lock() {
        Ok(file) => !try_flock(&file),
        Err(_) => false,
    }
}

/// Python's `int(text)` for a pid file: surrounding whitespace, a sign, digits.
fn py_int(text: &str) -> Option<i64> {
    let text = py::strip(text);
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let digits: String = digits.chars().filter(|c| *c != '_').collect();
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse::<i64>().ok().map(|n| sign * n)
}

fn wall_time() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

/// Port of `daemon.running_pid`.
///
/// The daemon's pid, or None if it isn't running. It holds a flock on `lock` for as long
/// as it lives, and writes its pid the moment it has it, so this is right all the way
/// through startup and shutdown.
pub fn running_pid() -> Result<Option<i64>> {
    if !lock_is_held() {
        return Ok(None);
    }
    let deadline = wall_time() + 2.0;
    loop {
        if let Ok(text) = fs::read_to_string(daemon_dir().join("pid")) {
            if let Some(pid) = py_int(&text) {
                if pid > 1 {
                    return Ok(Some(pid));
                }
            }
        }
        if wall_time() > deadline {
            return am_err(format!("something holds {} but no daemon pid is recorded", lock_path().display()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `subprocess.run(argv, capture_output=True, text=True, timeout=seconds).stdout`, or None
/// if it couldn't run or ran out of time.
pub(crate) fn output_within(argv: &[&str], seconds: f64) -> Option<String> {
    let mut child = Command::new(argv[0]).args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut out);
        out
    });
    let deadline = std::time::Instant::now() + Duration::from_secs_f64(seconds);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    reader.join().ok().map(|out| String::from_utf8_lossy(&out).into_owned())
}

fn daemon_command_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bam daemon\b").unwrap())
}

/// A recorded pid can be stale, so look before sending anything to it.
fn is_daemon_process(pid: i64) -> bool {
    match output_within(&["ps", "-o", "command=", "-p", &pid.to_string()], 5.0) {
        Some(command) => daemon_command_re().is_match(&command),
        None => false,
    }
}

pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<sha2::Sha256> as Mac>::new_from_slice(key).expect("any key length");
    mac.update(message);
    mac.finalize().into_bytes().into()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn loaded(cfg: Option<&Value>) -> Result<std::borrow::Cow<'_, Value>> {
    Ok(match cfg {
        Some(cfg) => std::borrow::Cow::Borrowed(cfg),
        None => std::borrow::Cow::Owned(am_core::config::load()?),
    })
}

fn str_at<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Port of `daemon.public_host`.
///
/// The only hostname the UI answers to, or None to answer to any.
///
/// Cookies don't isolate ports, so one set for 127.0.0.1 would also be sent to every other
/// server on 127.0.0.1 that the browser visits. So a loopback UI gets a private name that
/// no other server can know, under .localhost, which always means this machine. Refusing
/// every other name also stops DNS rebinding.
pub fn public_host(cfg: Option<&Value>) -> Result<Option<String>> {
    let cfg = loaded(cfg)?;
    let web = &cfg["web"];
    let configured = str_at(web, "public_host").to_lowercase();
    if !configured.is_empty() {
        return Ok(Some(configured));
    }
    if matches!(str_at(web, "host"), "127.0.0.1" | "::1" | "localhost") {
        let digest = hex(&hmac_sha256(web_token(false)?.as_bytes(), b"am-host-v1"));
        return Ok(Some(format!("am-{}.localhost", &digest[..16])));
    }
    Ok(None)
}

fn gethostname() -> String {
    let mut buf = [0u8; 256];
    unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Port of `daemon.base_url`.
pub fn base_url(cfg: Option<&Value>) -> Result<String> {
    let info = read_json(&daemon_dir().join("web.json"))?.filter(|v| py::truthy(Some(v)));
    let cfg = loaded(cfg)?;
    let from_info = |key: &str| info.as_ref().and_then(|i| i.get(key)).filter(|v| py::truthy(Some(v))).cloned();
    let mut host = match public_host(Some(&cfg))? {
        Some(host) => host,
        None => py::str_of(Some(&from_info("host").unwrap_or_else(|| cfg["web"]["host"].clone())))?,
    };
    let port = py::str_of(Some(&from_info("port").unwrap_or_else(|| cfg["web"]["port"].clone())))?;
    if host == "0.0.0.0" || host == "::" {
        host = gethostname();
    }
    if host.contains(':') && !host.starts_with('[') {
        host = format!("[{host}]");
    }
    Ok(format!("http://{host}:{port}"))
}

/// Port of `daemon.url_with_token`.
pub fn url_with_token() -> Result<String> {
    Ok(format!("{}/?token={}", base_url(None)?, web_token(false)?))
}

/// Python's `str(float)`, for the lock wait handed to a replacement daemon.
fn float_str(x: f64) -> String {
    py::float_repr(x)
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or_else(|| -status.signal().unwrap_or(0))
}

/// Port of `daemon.start`.
///
/// Start the daemon, unless one is running, and return its pid. `replacing` is a
/// daemon that was asked to stop and may still be exiting: the new one waits for it.
pub fn start(timeout: Option<f64>, replacing: Option<i64>) -> Result<i64> {
    let timeout = timeout.unwrap_or(START_WAIT_SECONDS);
    if let Some(pid) = running_pid()? {
        if Some(pid) != replacing {
            return Ok(pid);
        }
    }
    fs::create_dir_all(daemon_dir())?;
    match fs::remove_file(daemon_dir().join("web.json")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let log_path = daemon_dir().join("daemon.log");
    let out = OpenOptions::new().append(true).create(true).open(&log_path)?;
    // This exact install, links resolved: `~/.local/bin/am` may be repointed later.
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe.canonicalize().unwrap_or(exe));
    command.args(["daemon", "__run"]).stdin(Stdio::null()).stdout(out.try_clone()?).stderr(out).env("AM_HOME", am_home());
    if replacing.is_some_and(|pid| pid != 0) {
        command.env("AM_DAEMON_LOCK_WAIT", float_str(timeout));
    }
    // start_new_session=True
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let pid = child.id() as i64;
    let deadline = awake_time() + timeout;
    while awake_time() < deadline {
        if let Ok(Some(info)) = read_json(&daemon_dir().join("web.json")) {
            if info.get("pid").and_then(Value::as_i64) == Some(pid) {
                return Ok(pid);
            }
        }
        if let Some(status) = child.try_wait()? {
            return am_err(format!("the daemon exited with code {}; see {}", exit_code(status), log_path.display()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    am_err(format!("the daemon didn't come up within {timeout:.0}s; see {}", log_path.display()))
}

/// Wait for the daemon's lock to be let go, for up to `timeout` seconds awake.
fn released(timeout: f64) -> bool {
    let deadline = awake_time() + timeout;
    loop {
        if !lock_is_held() {
            return true;
        }
        if awake_time() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `os.kill`, never to a process group or init: a pid of 1 or less is refused.
fn kill(pid: i64, signal: libc::c_int) -> Result<()> {
    if pid <= 1 {
        return Err(Error::Other(format!("refusing to signal pid {pid}")));
    }
    if unsafe { libc::kill(pid as libc::pid_t, signal) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ESRCH) {
            return Err(e.into());
        }
    }
    Ok(())
}

/// Port of `daemon.stop`.
///
/// Stop the daemon. It's asked to, and waited for while it exits, for `timeout` seconds
/// the machine is awake, so sleep doesn't use the wait up. If it's still there then, it's
/// killed: turns are detached, so that interrupts none of them, and the next daemon
/// adopts them.
pub fn stop(timeout: Option<f64>) -> Result<bool> {
    let timeout = timeout.unwrap_or_else(stop_wait_seconds);
    let Some(pid) = running_pid()? else { return Ok(false) };
    if !is_daemon_process(pid) {
        return am_err(format!("process {pid} isn't the daemon, so nothing was stopped; see {}", daemon_dir().display()));
    }
    kill(pid, libc::SIGTERM)?;
    if released(timeout) {
        return Ok(true);
    }
    if is_daemon_process(pid) {
        log::warning(LOG, &format!("the daemon (pid {pid}) didn't exit within {timeout:.0}s of being asked to; killing it"));
        kill(pid, libc::SIGKILL)?;
        if released(kill_wait_seconds()) {
            return Ok(true);
        }
    }
    am_err(format!("the daemon (pid {pid}) didn't stop within {timeout:.0}s, or when killed"))
}

/// Port of `daemon.restart`.
///
/// Replace the daemon, and return the new one's pid. Once the old daemon has been asked
/// to stop, a new one is always started, so a slow exit never leaves none running.
pub fn restart(timeout: Option<f64>) -> Result<i64> {
    let timeout = timeout.unwrap_or_else(stop_wait_seconds);
    let old = running_pid()?;
    if let Some(old) = old {
        if !is_daemon_process(old) {
            return am_err(format!("process {old} isn't the daemon, so nothing was restarted; see {}", daemon_dir().display()));
        }
    }
    let mut failed: Option<String> = None;
    if old.is_some() {
        match stop(Some(timeout)) {
            Ok(_) => {}
            // Still exiting, somehow. The new daemon waits for its lock and takes over.
            Err(Error::Am(message)) => {
                log::warning(LOG, &format!("{message}; starting the new daemon anyway"));
                failed = Some(message);
            }
            Err(e) => return Err(e),
        }
    }
    match start(Some(timeout + START_WAIT_SECONDS), if failed.is_some() { old } else { None }) {
        Ok(pid) => Ok(pid),
        Err(Error::Am(message)) => am_err(match failed {
            Some(before) => format!("{message} (and before that: {before})"),
            None => message,
        }),
        Err(e) => Err(e),
    }
}

/// `cli.cmd_daemon`'s output for each action, so the CLI can call it as it is. (The
/// `_require_setup` check is the CLI's.)
pub fn cmd_daemon(action: &str) -> Result<()> {
    match action {
        "start" => {
            let was = running_pid()?;
            let pid = start(None, None)?;
            println!("The daemon {} (pid {pid}).", if was.is_some() { "is already running" } else { "started" });
        }
        "stop" => println!(
            "{}",
            if stop(None)? { "Stopped. Running turns continue, and the next daemon adopts them." } else { "The daemon isn't running." }
        ),
        "restart" => println!("Restarted (pid {}). Running turns were not interrupted.", restart(None)?),
        "status" => match running_pid()? {
            Some(pid) => println!("Running (pid {pid}) at {}", base_url(None)?),
            None => println!("Not running."),
        },
        "run" | "__run" => main()?,
        other => return am_err(format!("unknown daemon action {other}")),
    }
    Ok(())
}

/// `cli.cmd_url`'s output.
pub fn cmd_url() -> Result<()> {
    println!("{}", url_with_token()?);
    if running_pid()?.is_none() {
        eprintln!("The daemon isn't running; start it with `am daemon start`.");
    }
    Ok(())
}

// ---------------------------------------------------------------- the daemon process

/// Logins the health loop has been asked to check: `None` means all of them.
struct Recheck {
    names: Option<BTreeSet<String>>,
    fixed: BTreeSet<String>,
}

/// Port of `daemon.Daemon`: the shared state of the daemon's loops and its web server.
pub struct Daemon {
    lock: Option<File>,
    pub cfg: Value,
    runner: Mutex<crate::deps::Runner>,
    /// The runner's config, readable without waiting for a tick to finish.
    runner_cfg: Mutex<Value>,
    pub stop_event: Event,
    /// The runner's: look for work now.
    pub wake_event: Event,
    /// Open pages': look for changes now.
    pub hub_wake: Event,
    login_recheck: Event,
    recheck: Mutex<Recheck>,
}

pub(crate) fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".into()
    }
}

/// Run `f`, turning a panic into an error, as Python turns any exception in a tick into
/// a logged one.
pub(crate) fn caught<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(panic) => Err(Error::Other(panic_text(&*panic))),
    }
}

impl Daemon {
    pub fn new(lock: Option<File>) -> Result<Self> {
        let cfg = am_core::config::load()?;
        Ok(Daemon {
            lock,
            runner: Mutex::new(crate::deps::Runner::new(cfg.clone())?),
            runner_cfg: Mutex::new(cfg.clone()),
            cfg,
            stop_event: Event::new(),
            wake_event: Event::new(),
            hub_wake: Event::new(),
            login_recheck: Event::new(),
            recheck: Mutex::new(Recheck { names: Some(BTreeSet::new()), fixed: BTreeSet::new() }),
        })
    }

    /// The runner's current config (`self.runner.cfg`).
    pub fn runner_cfg(&self) -> Value {
        self.runner_cfg.lock().unwrap().clone()
    }

    /// Use a config.toml that was just changed now, rather than from the runner's next tick.
    pub fn reload_config(&self) {
        let mut runner = self.runner.lock().unwrap();
        if let Err(e) = caught(|| runner.reload_config()) {
            log::exception(LOG, "reloading the config failed", &e);
        }
        *self.runner_cfg.lock().unwrap() = runner.cfg().clone();
    }

    /// Have the health loop check these logins as soon as it can, or everything. One
    /// loop does all the checking, so an older result can't overwrite a newer one.
    pub fn request_login_check(&self, names: Option<&[String]>, fixed: &[String]) {
        let mut recheck = self.recheck.lock().unwrap();
        match names {
            None => recheck.names = None,
            Some(names) => {
                if let Some(pending) = recheck.names.as_mut() {
                    pending.extend(names.iter().cloned());
                }
            }
        }
        recheck.fixed.extend(fixed.iter().cloned());
        drop(recheck);
        self.login_recheck.set();
    }

    async fn runner_loop(self: Arc<Self>) {
        let tick_seconds = am_core::clock::parse_duration_value(&self.cfg["runner"]["tick_seconds"]).unwrap_or(0.3);
        while !self.stop_event.is_set() {
            // Cleared before the tick, so a wake during it brings on another at once.
            self.wake_event.clear();
            let daemon = self.clone();
            let result = tokio::task::spawn_blocking(move || {
                let mut runner = daemon.runner.lock().unwrap();
                let result = caught(|| runner.tick());
                *daemon.runner_cfg.lock().unwrap() = runner.cfg().clone();
                result
            })
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => log_once("runner tick failed", &e),
                Err(e) => log_once("runner tick failed", &e),
            }
            tokio::select! {
                _ = self.wake_event.wait_for(tick_seconds) => {}
                _ = self.stop_event.wait() => {}
            }
        }
    }

    /// AM_HOME was deleted, or replaced by a new one that this daemon doesn't own.
    fn home_is_gone(&self) -> bool {
        use std::os::unix::fs::MetadataExt;
        let Some(lock) = &self.lock else { return false };
        match (fs::metadata(lock_path()), lock.metadata()) {
            (Ok(on_disk), Ok(held)) => on_disk.ino() != held.ino(),
            _ => true,
        }
    }

    async fn scheduler_loop(self: Arc<Self>) {
        while !self.stop_event.is_set() {
            if self.lock.is_some() && self.home_is_gone() {
                // It would otherwise run forever, holding the port the next daemon needs.
                log::warning(LOG, &format!("{} is gone; stopping", am_home().display()));
                self.stop_event.set();
                return;
            }
            let cfg = self.runner_cfg();
            let result = tokio::task::spawn_blocking(move || caught(|| crate::deps::scheduler_tick(&cfg))).await;
            match result {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => log_once("scheduler tick failed", &e),
                Err(e) => log_once("scheduler tick failed", &e),
            }
            self.stop_event.wait_for(5.0).await;
        }
    }

    async fn health_loop(self: Arc<Self>) {
        let mut last_resources = 0.0;
        let mut last_logins = 0.0;
        while !self.stop_event.is_set() {
            let cfg = self.runner_cfg();
            let current = now();
            let forced = self.login_recheck.is_set();
            let (requested, fixed) = {
                let mut recheck = self.recheck.lock().unwrap();
                (recheck.names.replace(BTreeSet::new()), std::mem::take(&mut recheck.fixed))
            };
            self.login_recheck.clear();
            let result: Result<()> = async {
                let health = &cfg["health"];
                let logins_due = current - last_logins >= am_core::clock::parse_duration_value(&health["login_check_every"])?;
                let names: Option<Vec<String>> = if logins_due { None } else { requested.map(|n| n.into_iter().collect()) };
                if (forced && names.is_none()) || current - last_resources >= am_core::clock::parse_duration_value(&health["check_every"])? {
                    last_resources = current;
                    let cfg = cfg.clone();
                    off_loop(move || crate::deps::check_resources(&cfg)).await?;
                }
                if forced || logins_due {
                    if names.is_none() {
                        last_logins = current;
                    }
                    let cfg = cfg.clone();
                    let fixed: Vec<String> = fixed.into_iter().collect();
                    off_loop(move || crate::deps::check_logins(&cfg, names.as_deref(), &fixed)).await?;
                }
                Ok(())
            }
            .await;
            if let Err(e) = result {
                log_once("health check failed", &e);
            }
            tokio::select! {
                _ = self.login_recheck.wait_for(15.0) => {}
                _ = self.stop_event.wait() => {}
            }
        }
    }

    /// Read `wake_daemon`'s FIFO, and wake the runner and open pages on each write. It's
    /// opened for writing as well as reading, so that a CLI closing its end is never an
    /// end of file. Left in place at exit: with no reader, opening it fails at once. In
    /// this process a poke goes straight to the pages, not to the runner, whose own writes
    /// poke as it ticks and would otherwise wake it again and again.
    fn listen_for_wakes(self: &Arc<Self>) -> Option<tokio::task::JoinHandle<()>> {
        let path = am_core::fsx::wake_path();
        let opened = (|| -> std::io::Result<File> {
            use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
            match fs::symlink_metadata(&path) {
                Ok(meta) if !meta.file_type().is_fifo() => fs::remove_file(&path)?,
                _ => {}
            }
            let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(std::io::Error::other)?;
            if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(e);
                }
            }
            OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(&path)
        })();
        let file = match opened {
            Ok(file) => file,
            Err(e) => {
                log::warning(LOG, &format!("can't read {} ({e}); CLI writes are found by polling", path.display()));
                return None;
            }
        };
        #[allow(deprecated)] // The fd is owned here, so the soundness concern doesn't apply.
        let fd = match tokio::io::unix::AsyncFd::new(file) {
            Ok(fd) => fd,
            Err(e) => {
                log::warning(LOG, &format!("can't read {} ({e}); CLI writes are found by polling", path.display()));
                return None;
            }
        };
        let daemon = self.clone();
        let pages = Arc::downgrade(self);
        am_core::set_wake_hook(Some(Box::new(move || {
            if let Some(daemon) = pages.upgrade() {
                daemon.hub_wake.set();
            }
        })));
        Some(tokio::spawn(async move {
            loop {
                let Ok(mut guard) = fd.readable().await else { return };
                let mut buf = [0u8; 4096];
                loop {
                    match std::io::Read::read(&mut fd.get_ref(), &mut buf) {
                        Ok(0) => break,
                        Ok(_) => continue,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            guard.clear_ready();
                            break;
                        }
                        Err(_) => break,
                    }
                }
                daemon.wake_event.set();
                daemon.hub_wake.set();
            }
        }))
    }

    /// Port of `daemon.Daemon.run`.
    pub async fn run(self: Arc<Self>) -> Result<()> {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let signals = {
            let daemon = self.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = terminate.recv() => {}
                    _ = interrupt.recv() => {}
                }
                daemon.stop_event.set();
            })
        };
        let server = crate::web::start_server(self.clone()).await?;
        let wakes = self.listen_for_wakes();
        write_json(
            &daemon_dir().join("web.json"),
            &serde_json::json!({"pid": std::process::id(), "host": server.host, "port": server.port, "started": now()}),
        )?;
        log::info(LOG, &format!("listening on {}:{}", server.host, server.port));
        let loops = [
            tokio::spawn(self.clone().runner_loop()),
            tokio::spawn(self.clone().scheduler_loop()),
            tokio::spawn(self.clone().health_loop()),
        ];
        self.stop_event.wait().await;
        log::info(LOG, "stopping; running turns continue");
        // A tick in progress finishes, as Python's executor threads do: the next daemon
        // mustn't start while one is half done. Health checks aren't waited for.
        let (runner, scheduler, health) = {
            let [a, b, c] = loops;
            (a, b, c)
        };
        health.abort();
        let _ = runner.await;
        let _ = scheduler.await;
        server.shutdown().await;
        if let Some(wakes) = wakes {
            am_core::set_wake_hook(None);
            wakes.abort();
        }
        signals.abort();
        match fs::remove_file(daemon_dir().join("web.json")) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        Ok(())
    }
}

/// Python logs every failed tick with its traceback. A port in progress fails the same way
/// on every tick, so the same message is logged once.
fn log_once(what: &str, error: &dyn std::fmt::Display) {
    static SEEN: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    let text = format!("{what}: {error}");
    if SEEN.get_or_init(Default::default).lock().unwrap().insert(text) {
        log::exception(LOG, what, error);
    }
}

/// `Daemon._off_loop`: run `f` on a thread of its own that the daemon's exit doesn't wait
/// for. For health checks: a login check can take its whole timeout, and a stop has no need
/// to wait for one.
pub(crate) async fn off_loop<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    let (send, receive) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("am-health".into())
        .spawn(move || {
            let _ = send.send(caught(f));
        })
        .map_err(Error::Io)?;
    receive.await.unwrap_or_else(|_| Err(Error::Other("the check's thread went away".into())))
}

/// Exclusive for this AM_HOME. Waits briefly, because a daemon being replaced may be
/// a moment from exiting, and `am` checking on the daemon holds the lock for an instant.
fn take_lock() -> Result<File> {
    fs::create_dir_all(daemon_dir())?;
    let lock = open_lock()?;
    let wait = std::env::var("AM_DAEMON_LOCK_WAIT").ok().filter(|s| !s.is_empty()).and_then(|s| py::strip(&s).parse::<f64>().ok());
    let deadline = awake_time() + wait.unwrap_or(LOCK_WAIT_SECONDS);
    loop {
        if try_flock(&lock) {
            return Ok(lock);
        }
        if awake_time() > deadline {
            eprintln!("another daemon is already running for {}", am_home().display());
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Recompute costs recorded by the old accounting, once, before any turn runs.
fn fix_costs_once() {
    let needed = match caught(crate::deps::costs_need_fixing) {
        Ok(needed) => needed,
        Err(e) => {
            log::exception(LOG, "recomputing recorded costs failed; `am fix-costs` retries it", &e);
            return;
        }
    };
    if !needed {
        return;
    }
    match caught(crate::deps::fix_costs) {
        Ok(text) => log::info(LOG, &format!("recomputed recorded costs:\n{text}")),
        Err(e) => log::exception(LOG, "recomputing recorded costs failed; `am fix-costs` retries it", &e),
    }
}

fn quiet_panics() {
    // A panic is reported where it's caught (a failed tick or request), not by the hook.
    std::panic::set_hook(Box::new(|_| {}));
}

/// Port of `daemon.main`: the daemon process, `am daemon __run`.
pub fn main() -> Result<()> {
    log::configure();
    quiet_panics();
    let lock = take_lock()?;
    am_core::atomic_write(&daemon_dir().join("pid"), std::process::id().to_string().as_bytes(), 0o644)?;
    web_token(true)?;
    if let Err(e) = caught(am_store::setup::write_shim) {
        log::exception(LOG, "writing the am shim failed", &e);
    }
    fix_costs_once();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let daemon = Arc::new(Daemon::new(Some(lock))?);
    let result = runtime.block_on(daemon.run());
    // Anything still running (a health check, a request's blocking work) isn't waited for.
    runtime.shutdown_background();
    if let Err(e) = result {
        log::error(LOG, &format!("{e}"));
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_files_parse_as_python_int_does() {
        assert_eq!(py_int(" 123\n"), Some(123));
        assert_eq!(py_int("-1"), Some(-1));
        assert_eq!(py_int("+7"), Some(7));
        assert_eq!(py_int("1_000"), Some(1000));
        assert_eq!(py_int(""), None);
        assert_eq!(py_int("garbage"), None);
        assert_eq!(py_int("1__0"), None);
    }

    #[test]
    fn nothing_is_signalled_for_a_bad_pid() {
        for pid in [-1, 0, 1] {
            assert!(kill(pid, 0).is_err(), "{pid}");
        }
        let _guard = crate::test_env::lock();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("AM_HOME", home.path());
        fs::create_dir_all(daemon_dir()).unwrap();
        let held = open_lock().unwrap();
        assert!(try_flock(&held));
        for recorded in ["-1", "0", "1", "", "garbage"] {
            fs::write(daemon_dir().join("pid"), recorded).unwrap();
            match stop(Some(1.0)) {
                Err(Error::Am(message)) => assert!(message.contains("no daemon pid is recorded"), "{message}"),
                other => panic!("{recorded:?}: {:?}", other.map_err(|e| e.to_string())),
            }
        }
        drop(held);
        std::env::remove_var("AM_HOME");
    }

    #[test]
    fn the_daemon_command_line_is_recognized() {
        let re = daemon_command_re();
        assert!(re.is_match("/x/target/debug/am daemon __run"));
        assert!(!re.is_match("/x/target/debug/am __turn /tmp/t"));
        assert!(!re.is_match("/x/cam daemon"));
    }
}
