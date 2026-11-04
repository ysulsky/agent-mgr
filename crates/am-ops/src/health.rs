//! Machine and login checks. Resource alerts go to the home manager, which follows
//! the storage policy in its instructions. Expired logins become attention items with
//! a Run button.
//!
//! The latest result of each check is kept in daemon/health-state.json, so the Health page
//! and `am health` are right after a restart. Output from the checks is never stored or
//! logged, because it can hold account details: only a short detail picked out of it is.

use am_core::{daemon_dir, hooks, now, poke, format, read_json, update_json, write_json, Result, Value, HOME_SLUG, MANAGER};
use am_store::{items, mail, projects};
use regex::Regex;
use serde_json::{json, Map};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::OnceLock;

const LOG: &str = "am.health";

pub const LOGIN_TIMEOUT: f64 = 45.0;
/// A daemon that died mid-check doesn't leave a row spinning for good.
pub const CHECKING_FOR: f64 = LOGIN_TIMEOUT + 30.0;

pub const OK: &str = "ok";
pub const EXPIRED: &str = "expired";
pub const MISSING: &str = "missing";
pub const UNKNOWN: &str = "unknown";
pub const UNCHECKED: &str = "unchecked";

/// What a login state is shown as.
pub fn state_label(state: &str) -> Option<&'static str> {
    Some(match state {
        OK => "OK",
        EXPIRED => "Expired",
        MISSING => "Not installed",
        UNKNOWN => "Unknown",
        UNCHECKED => "Not checked yet",
        _ => return None,
    })
}

/// A check that can't reach its service fails the same way an expired login does.
fn offline_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)no such host|could not resolve|couldn't resolve|temporary failure in name resolution|name or service not known|network is (unreachable|down)|connection (refused|reset|timed out)|dial tcp|tls handshake|i/o timeout|deadline exceeded",
        )
        .unwrap()
    })
}

fn turn_login_title() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\S+) login needed$").unwrap())
}

/// A test hook's value, if hooks are on and it's set (possibly to null).
fn hooked(key: &str) -> Option<Value> {
    hooks::get(key)
}

/// How full the disk holding `AM_HOME` is, in percent.
pub fn disk_percent() -> Result<f64> {
    if let Some(v) = hooked("disk_percent") {
        return Ok(v.as_f64().unwrap_or(0.0));
    }
    let path = std::ffi::CString::new(am_core::am_home().as_os_str().as_encoded_bytes()).map_err(|e| am_core::Error::Other(e.to_string()))?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let total = st.f_blocks as f64 * st.f_frsize as f64;
    let used = (st.f_blocks as f64 - st.f_bfree as f64) * st.f_frsize as f64;
    Ok(used / total * 100.0)
}

fn output_of(argv: &[&str]) -> Option<String> {
    let out = std::process::Command::new(argv[0]).args(&argv[1..]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Memory in real use, in percent. Reclaimable caches don't count.
pub fn memory_percent() -> Result<Option<f64>> {
    if let Some(v) = hooked("memory_percent") {
        return Ok(v.as_f64());
    }
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        let mut info = BTreeMap::new();
        for line in text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(n) = parts[1].parse::<f64>() {
                    info.insert(line.split(':').next().unwrap_or("").to_string(), n);
                }
            }
        }
        if let (Some(available), Some(total)) = (info.get("MemAvailable"), info.get("MemTotal")) {
            return Ok(Some((1.0 - available / total) * 100.0));
        }
    }
    let Some(total) = output_of(&["sysctl", "-n", "hw.memsize"]).and_then(|t| format::strip(&t).parse::<f64>().ok()) else { return Ok(None) };
    let Some(text) = output_of(&["vm_stat"]) else { return Ok(None) };
    static PAGE: OnceLock<Regex> = OnceLock::new();
    static ROW: OnceLock<Regex> = OnceLock::new();
    let page = PAGE
        .get_or_init(|| Regex::new(r"page size of (\d+)").unwrap())
        .captures(&text)
        .and_then(|c| c[1].parse::<f64>().ok())
        .unwrap_or(4096.0);
    let mut pages = BTreeMap::new();
    for c in ROW.get_or_init(|| Regex::new(r#"(?m)^"?([A-Za-z -]+)"?:\s+(\d+)\."#).unwrap()).captures_iter(&text) {
        pages.insert(c[1].to_string(), c[2].parse::<f64>().unwrap_or(0.0));
    }
    let free: f64 = ["Pages free", "Pages inactive", "Pages speculative", "Pages purgeable"].iter().map(|k| pages.get(*k).copied().unwrap_or(0.0)).sum();
    if total == 0.0 || pages.is_empty() {
        return Ok(None);
    }
    Ok(Some((1.0 - free * page / total) * 100.0))
}

fn state_path() -> PathBuf {
    daemon_dir().join("health-state.json")
}

/// The saved health state (daemon/health-state.json), or `{}`.
pub fn read_state() -> Result<Value> {
    Ok(read_json(&state_path())?.unwrap_or_else(|| json!({})))
}

fn update_state(change: impl FnOnce(&mut Map<String, Value>)) -> Result<()> {
    update_json(&state_path(), json!({}), |state| {
        if let Some(map) = state.as_object_mut() {
            change(map);
        }
        None
    })?;
    poke(HOME_SLUG); // Open pages hear about this as they do any other change.
    Ok(())
}

fn loaded(cfg: Option<&Value>) -> Result<std::borrow::Cow<'_, Value>> {
    Ok(match cfg.filter(|c| format::truthy(Some(c))) {
        Some(cfg) => std::borrow::Cow::Borrowed(cfg),
        None => std::borrow::Cow::Owned(am_core::config::load()?),
    })
}

fn duration(v: &Value) -> Result<f64> {
    am_core::clock::parse_duration_value(v)
}

/// `x` rounded to a whole number.
fn fmt0(x: f64) -> String {
    format!("{x:.0}")
}

/// Read disk and memory use, save them, and alert the home manager about any over its
/// limit (at most once per `alert_cooldown`).
pub fn check_resources(cfg: Option<&Value>) -> Result<()> {
    let cfg = loaded(cfg)?;
    let health = &cfg["health"];
    let cooldown = duration(&health["alert_cooldown"])?;
    let alerts_path = daemon_dir().join("health.json");
    let mut state = read_json(&alerts_path)?.unwrap_or_else(|| json!({}));
    let readings: [(&str, Option<f64>, &Value, &str); 2] = [
        ("disk", Some(disk_percent()?), &health["disk_warn_percent"], "messages/disk-alert"),
        ("memory", memory_percent()?, &health["memory_warn_percent"], "messages/memory-alert"),
    ];
    let read_at = now();
    let resources: Map<String, Value> = readings.iter().map(|(name, value, _, _)| (name.to_string(), json!({"percent": value, "read": read_at}))).collect();
    update_state(|s| {
        s.insert("resources".into(), Value::Object(resources));
    })?;
    let mut changed = false;
    for (name, value, limit, text) in readings {
        let Some(value) = value else { continue };
        let since = state.get(name).and_then(Value::as_f64).unwrap_or(0.0);
        if value < limit.as_f64().unwrap_or(f64::INFINITY) || now() - since < cooldown {
            continue;
        }
        let home = am_core::am_home().to_string_lossy().into_owned();
        let message = am_turns::prompts::render(text, &json!({"percent": fmt0(value), "am_home": home}))?;
        let opts = mail::SendOpts {
            sender: "health".into(),
            kind: Some("system".into()),
            meta: Some(json!({"alert": name, "percent": value})),
            ..Default::default()
        };
        mail::send(HOME_SLUG, MANAGER, &message, opts)?;
        state[name] = json!(now());
        changed = true;
        am_core::log::warning(LOG, &format!("alerted home manager: {name} at {}%", fmt0(value)));
    }
    if changed {
        write_json(&alerts_path, &state)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- what each tool says about its account

fn claude_account(output: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""email"\s*:\s*"([^"@\s]+@[^"@\s]+)""#).unwrap()).captures(output).map(|c| c[1].to_string()).unwrap_or_default()
}

fn codex_account(output: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"Logged in using (ChatGPT|an API key)").unwrap())
        .captures(output)
        .map(|c| format!("Using {}", &c[1]))
        .unwrap_or_default()
}

fn gh_account(output: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"Logged in to \S+ account ([A-Za-z0-9-]+)").unwrap()).captures(output).map(|c| c[1].to_string()).unwrap_or_default()
}

/// The name of the login check for a storage remote.
pub fn storage_login(remote: &str) -> Result<String> {
    Ok(if remote.to_lowercase().contains("drive") { "google-drive".into() } else { format!("rclone-{remote}") })
}

/// One login check, as `login_checks` lists them.
#[derive(Clone)]
pub struct LoginCheck {
    pub name: String,
    pub title: String,
    pub check: Vec<String>,
    pub fix: Option<String>,
    pub bad_output: Option<String>,
    pub account: Option<fn(&str) -> String>,
    pub missing_fix: Option<String>,
    pub missing_title: Option<String>,
}

/// POSIX shell-style word splitting. Unquoted whitespace separates words. Single quotes
/// keep everything up to the next `'` as it is. Inside double quotes a backslash escapes
/// only `\`, `"`, `$`, `` ` `` and a newline, and is kept before anything else. Outside
/// quotes a backslash takes the next character as it is. Quoted and unquoted parts next to
/// each other make one word, so `''` is an empty word. An unclosed quote or a final
/// backslash is an error.
pub fn shell_words(text: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return am_core::am_err("No closing quotation"),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('\\' | '"' | '$' | '`' | '\n')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return am_core::am_err("No closing quotation"),
                        },
                        Some(c) => word.push(c),
                        None => return am_core::am_err("No closing quotation"),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(c) => word.push(c),
                    None => return am_core::am_err("No escaped character"),
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

fn opt_str(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(v) => Some(format::text(v)),
    }
}

/// The logins to check: Claude, Codex, and as configured GitHub, the storage remote and
/// any extra ones.
pub fn login_checks(cfg: &Value) -> Result<Vec<LoginCheck>> {
    let bin = |name: &str| format::text(&cfg["backends"][name]["bin"]);
    let mut checks = vec![
        LoginCheck {
            name: "claude".into(),
            title: "Claude login expired".into(),
            check: vec![bin("claude"), "auth".into(), "status".into()],
            fix: Some(format!("{} auth login", bin("claude"))),
            bad_output: Some("\"loggedIn\": false".into()),
            account: Some(claude_account),
            missing_fix: None,
            missing_title: None,
        },
        LoginCheck {
            name: "codex".into(),
            title: "Codex login expired".into(),
            check: vec![bin("codex"), "login".into(), "status".into()],
            fix: Some(format!("{} login", bin("codex"))),
            bad_output: None,
            account: Some(codex_account),
            missing_fix: None,
            missing_title: None,
        },
    ];
    let logins = &cfg["logins"];
    if format::truthy(logins.get("gh")) {
        checks.push(LoginCheck {
            name: "gh".into(),
            title: "GitHub login expired".into(),
            check: vec!["gh".into(), "auth".into(), "status".into()],
            fix: Some("gh auth login".into()),
            bad_output: None,
            account: Some(gh_account),
            missing_fix: None,
            missing_title: None,
        });
    }
    let remote = cfg["storage"].get("rclone_remote").filter(|r| format::truthy(Some(r))).map(format::text).unwrap_or_default();
    let remote = remote.trim_end_matches(':');
    if !remote.is_empty() {
        checks.push(LoginCheck {
            name: storage_login(remote)?,
            title: format!("Storage login expired ({remote})"),
            check: vec!["rclone".into(), "about".into(), format!("{remote}:")],
            fix: Some(format!("rclone config reconnect {remote}:")),
            bad_output: None,
            account: None,
            missing_fix: Some("brew install rclone".into()),
            missing_title: Some("rclone is needed for storage offload".into()),
        });
    }
    if let Some(extra) = logins.get("extra").filter(|e| format::truthy(Some(e))).and_then(Value::as_array) {
        for entry in extra {
            let name = format::text(&entry["name"]);
            checks.push(LoginCheck {
                title: format!("{name} login expired"),
                check: shell_words(&format::text(&entry["check"]))?,
                fix: opt_str(entry.get("fix")),
                missing_fix: opt_str(entry.get("missing_fix")),
                name,
                bad_output: None,
                account: None,
                missing_title: None,
            });
        }
    }
    Ok(checks)
}

fn which(name: &str, path: Option<&str>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let runnable = |p: &std::path::Path| std::fs::metadata(p).is_ok_and(|m| !m.is_dir() && m.permissions().mode() & 0o111 != 0);
    if name.contains('/') {
        let p = PathBuf::from(name);
        return runnable(&p).then_some(p);
    }
    let path = path.map(String::from).or_else(|| std::env::var("PATH").ok()).unwrap_or_else(|| "/bin:/usr/bin".into());
    path.split(':').map(|d| std::path::Path::new(if d.is_empty() { "." } else { d }).join(name)).find(|p| runnable(p))
}

fn result(state: &str, detail: String) -> (String, String) {
    (state.to_string(), detail)
}

/// Run a check, and say what it found: a state, and a detail that's safe to show.
pub fn probe_login(check: &LoginCheck) -> (String, String) {
    let program = check.check.first().cloned().unwrap_or_default();
    let tool = program.rsplit('/').next().unwrap_or("").to_string();
    if which(&program, None).is_none() {
        return result(MISSING, format!("{tool} isn't on the daemon's PATH."));
    }
    let env = am_turns::runner::turn_env(HOME_SLUG, MANAGER, "");
    let timeout = hooked("login_timeout").and_then(|v| v.as_f64()).unwrap_or(LOGIN_TIMEOUT);
    let mut command = std::process::Command::new(&program);
    command
        .args(&check.check[1..])
        .env_clear()
        .envs(env.iter())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return result(UNKNOWN, format!("{tool} couldn't be run.")),
    };
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let readers = (
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = std::io::Read::read_to_end(&mut out, &mut b);
            b
        }),
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = std::io::Read::read_to_end(&mut err, &mut b);
            b
        }),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout.max(0.0));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return result(UNKNOWN, format!("The check didn't finish within {}s.", format::text(&hooked("login_timeout").unwrap_or(json!(45)))));
            }
        }
    };
    let stdout = String::from_utf8_lossy(&readers.0.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&readers.1.join().unwrap_or_default()).into_owned();
    let output = format!("{stdout}\n{stderr}");
    use std::os::unix::process::ExitStatusExt;
    if status.signal().is_some() {
        return result(UNKNOWN, format!("{tool} was killed before it could tell."));
    }
    let failed = status.code() != Some(0);
    if failed && offline_re().is_match(&output) {
        return result(UNKNOWN, format!("{tool} couldn't reach its service."));
    }
    if failed || check.bad_output.as_deref().is_some_and(|bad| !bad.is_empty() && stdout.contains(bad)) {
        return result(EXPIRED, format!("{tool} reports no valid login."));
    }
    result(OK, check.account.map(|account| account(&output)).unwrap_or_default())
}

fn record_login(name: &str, state: &str, detail: &str) -> Result<()> {
    update_state(|s| {
        let logins = s.entry("logins").or_insert_with(|| json!({}));
        let before = logins.get(name).filter(|b| format::truthy(Some(b))).cloned().unwrap_or_else(|| json!({}));
        let mut entry = json!({"state": state, "detail": detail, "checked": now()});
        if state == UNKNOWN {
            // Not knowing now doesn't undo what the last answer was.
            let known = if before.get("state").and_then(Value::as_str) == Some(UNKNOWN) { before.get("last_known") } else { before.get("state") };
            if let Some(known) = known.filter(|k| format::truthy(Some(k))) {
                entry["last_known"] = known.clone();
            }
        }
        logins[name] = entry;
    })
}

/// Show these logins as being checked, or all of them.
pub fn mark_checking(names: Option<&[String]>, cfg: &Value) -> Result<()> {
    let chosen: Vec<String> = login_checks(cfg)?.into_iter().map(|c| c.name).filter(|n| names.is_none_or(|names| names.contains(n))).collect();
    update_state(|s| {
        let logins = s.entry("logins").or_insert_with(|| json!({}));
        for name in &chosen {
            if logins.get(name).is_none() {
                logins[name.as_str()] = json!({});
            }
            logins[name.as_str()]["checking_since"] = json!(now());
        }
    })
}

fn check_login(check: &LoginCheck, fixed: bool) -> Result<()> {
    let name = &check.name;
    let (state, detail) = match std::panic::catch_unwind(|| probe_login(check)) {
        Ok(found) => found,
        Err(_) => {
            am_core::log::error(LOG, &format!("login check failed to run: {name}"));
            (UNKNOWN.to_string(), "The check failed to run.".to_string())
        }
    };
    record_login(name, &state, &detail)?;
    let key = format!("login-{name}");
    match state.as_str() {
        UNKNOWN => return Ok(()),
        OK => {
            items::resolve_attention_key(HOME_SLUG, &key, "health")?;
            if fixed {
                clear_turn_reports(name)?;
            }
            return Ok(());
        }
        _ => {}
    }
    let program = check.check.first().cloned().unwrap_or_default();
    let (title, fix, body) = if state == MISSING {
        (
            check.missing_title.clone().unwrap_or_else(|| format!("{program} is not installed")),
            check.missing_fix.clone(),
            format!("`{program}` isn't on the daemon's PATH ({}).", std::env::var("PATH").unwrap_or_default()),
        )
    } else {
        (check.title.clone(), check.fix.clone(), format!("`{}` reports no valid login.", check.check.join(" ")))
    };
    let opts = items::AttentionOpts { body, kind: Some("action".into()), run: fix, source: "health".into(), key: Some(key), ..Default::default() };
    items::add_attention(HOME_SLUG, &title, opts)?;
    am_core::log::warning(LOG, &format!("login check failed: {name}"));
    Ok(())
}

/// Check these logins, or all of them, together, so that one that hangs doesn't hold up the
/// rest. `fixed` are ones the user just ran a fix for.
pub fn check_logins(cfg: Option<&Value>, names: Option<&[String]>, fixed: Option<&[String]>) -> Result<()> {
    let cfg = loaded(cfg)?;
    let fixed: BTreeSet<String> = fixed.unwrap_or(&[]).iter().cloned().collect();
    let checks: Vec<LoginCheck> = login_checks(&cfg)?.into_iter().filter(|c| names.is_none_or(|n| n.contains(&c.name))).collect();
    if !checks.is_empty() {
        let chosen: Vec<String> = checks.iter().map(|c| c.name.clone()).collect();
        mark_checking(Some(&chosen), &cfg)?;
        std::thread::scope(|scope| {
            let running: Vec<_> = checks.iter().map(|c| scope.spawn(|| check_login(c, fixed.contains(&c.name)))).collect();
            for handle in running {
                if let Ok(Err(e)) = handle.join() {
                    am_core::log::error(LOG, &format!("login check failed: {e}"));
                }
            }
        });
    }
    if names.is_none() {
        forget_removed_logins(&checks.iter().map(|c| c.name.clone()).collect())?;
    }
    Ok(())
}

fn forget_removed_logins(current: &BTreeSet<String>) -> Result<()> {
    let state = read_state()?;
    let removed: Vec<String> = state.get("logins").and_then(Value::as_object).map(|l| l.keys().filter(|k| !current.contains(*k)).cloned().collect()).unwrap_or_default();
    if removed.is_empty() {
        return Ok(());
    }
    for name in &removed {
        items::resolve_attention_key(HOME_SLUG, &format!("login-{name}"), "health")?;
    }
    update_state(|s| {
        if let Some(logins) = s.get_mut("logins").and_then(Value::as_object_mut) {
            for name in &removed {
                logins.shift_remove(name);
            }
        }
    })
}

// ---------------------------------------------------------------- logins that turns found expired

/// Which login an attention item is about, if it's one of the two kinds that are: a
/// check's, keyed by the login, and a turn's, keyed by the agent.
pub fn item_login(item: &Value) -> Result<Option<String>> {
    let key = item.get("key").filter(|k| format::truthy(Some(k))).map(format::text).unwrap_or_default();
    let Some(rest) = key.strip_prefix("login-") else { return Ok(None) };
    let source = item.get("source").and_then(Value::as_str);
    if source == Some("runner") {
        let title = item.get("title").filter(|t| format::truthy(Some(t))).map(format::text).unwrap_or_default();
        return Ok(turn_login_title().captures(&title).map(|c| c[1].to_string()));
    }
    Ok((source == Some("health")).then(|| rest.to_string()))
}

/// A status check can say a login is fine while the service refuses it, so a turn that
/// was refused counts until its agent's next turn works. An archived project's agents
/// don't retry, so its items would never clear.
fn turn_reports(attention: &[(String, Vec<Value>)]) -> Result<BTreeMap<String, Vec<Value>>> {
    let mut reports: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for (slug, entries) in attention {
        if format::truthy(projects::get_project(slug)?.get("archived_at")) {
            continue;
        }
        for item in entries {
            if let Some(login) = item_login(item)? {
                if item.get("source").and_then(Value::as_str) == Some("runner") {
                    let key = format::text(&item["key"]);
                    let agent = key.strip_prefix("login-").unwrap_or(&key).to_string();
                    reports.entry(login).or_default().push(json!({"project": slug, "agent": agent, "at": item.get("created").cloned().unwrap_or(Value::Null)}));
                }
            }
        }
    }
    Ok(reports)
}

/// The user ran the fix, and the check agrees. Don't make them wait for agents to get
/// around to retrying: resume them now, and any that are still refused will say so again.
fn clear_turn_reports(name: &str) -> Result<()> {
    for slug in projects::list_slugs()? {
        for item in items::list_attention(&slug, false)? {
            if item.get("source").and_then(Value::as_str) == Some("runner") && item_login(&item)?.as_deref() == Some(name) {
                items::resolve_attention(&slug, &format::text(&item["id"]), "health")?;
                let key = format::text(&item["key"]);
                let agent = key.strip_prefix("login-").unwrap_or(&key);
                if projects::agent_exists(&slug, agent) {
                    projects::update_agent(&slug, agent, |a| {
                        a["not_before"] = json!(0);
                    })?;
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- what the Health page and `am health` show

fn login_row(check: &LoginCheck, entry: &Value, reports: &[Value]) -> Value {
    let text = |key: &str| entry.get(key).filter(|v| format::truthy(Some(v))).map(format::text);
    let mut state = text("state").unwrap_or_else(|| UNCHECKED.into());
    let mut detail = text("detail").unwrap_or_default();
    let last_known = entry.get("last_known").cloned().unwrap_or(Value::Null);
    let last_known_text = last_known.as_str().map(String::from);
    if !reports.is_empty() {
        let mut who = reports.iter().take(3).map(|r| format!("{} in {}", format::text(&r["agent"]), format::text(&r["project"]))).collect::<Vec<_>>().join(", ");
        if reports.len() > 3 {
            who.push_str(" and others");
        }
        if state == EXPIRED {
            detail = format!("{detail} Waiting on it: {who}.");
        } else if state != MISSING {
            let lead = if state == OK { "The status check looks fine, but a turn" } else { "A turn" };
            detail = format!("{lead} by {who} failed with an auth error. It retries every few minutes.");
            state = EXPIRED.into();
        }
    }
    if state == UNKNOWN && format::truthy(Some(&last_known)) {
        let known = last_known_text.clone().unwrap_or_else(|| format::text(&last_known));
        detail = format!("{detail} Last known: {}.", state_label(&known).map(String::from).unwrap_or(known));
    }
    let mut fix = None;
    if state == EXPIRED || (state == UNKNOWN && last_known_text.as_deref() == Some(EXPIRED)) {
        fix = check.fix.clone();
    } else if state == MISSING {
        fix = check.missing_fix.clone();
    }
    if (state == EXPIRED || state == MISSING) && fix.as_deref().is_none_or(str::is_empty) {
        detail.push_str(" No fix command is configured, so this has to be fixed outside Agent Mgr.");
    }
    let since = entry.get("checking_since").and_then(Value::as_f64).filter(|s| *s != 0.0);
    let checking_for = hooked("checking_for").and_then(|v| v.as_f64()).unwrap_or(CHECKING_FOR);
    json!({
        "name": check.name,
        "state": state,
        "label": state_label(&state).unwrap_or(""),
        "detail": format::strip(&detail),
        "checked": entry.get("checked").cloned().unwrap_or(Value::Null),
        "checking": since.is_some_and(|since| now() - since < checking_for),
        "fix": fix,
        "last_known": last_known,
        "reports": reports,
    })
}

fn machine_rows(cfg: &Value, state: &Value) -> Vec<Value> {
    let mut rows = Vec::new();
    for (name, label, limit) in [("disk", "Disk", &cfg["health"]["disk_warn_percent"]), ("memory", "Memory", &cfg["health"]["memory_warn_percent"])] {
        let reading = state.get("resources").and_then(|r| r.get(name)).filter(|r| format::truthy(Some(r))).cloned().unwrap_or_else(|| json!({}));
        let percent = reading.get("percent").cloned().unwrap_or(Value::Null);
        let high = percent.as_f64().is_some_and(|p| limit.as_f64().is_some_and(|l| p >= l));
        rows.push(json!({
            "name": name,
            "label": label,
            "percent": percent,
            "limit": limit,
            "read": reading.get("read").cloned().unwrap_or(Value::Null),
            "high": high,
        }));
    }
    rows
}

fn project_rows(cfg: &Value, attention: &[(String, Vec<Value>)]) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for (slug, entries) in attention {
        let mut row = am_turns::scheduler::health_status(slug, cfg)?;
        let stuck = format!("stuck-{MANAGER}");
        let problem = if entries.iter().any(|i| i.get("key").and_then(Value::as_str) == Some(stuck.as_str())) {
            json!("The manager may be stuck")
        } else if matches!(row.get("check").filter(|c| format::truthy(Some(c))).and_then(|c| c.get("state")).and_then(Value::as_str), Some("failed" | "vanished" | "abandoned")) {
            json!("The last check's turn failed")
        } else {
            Value::Null
        };
        row["problem"] = problem;
        rows.push(row);
    }
    Ok(rows)
}

fn summary(logins: &[Value], machine: &[Value], project_rows: &[Value]) -> Value {
    let state = |l: &Value| l["state"].as_str().unwrap_or("").to_string();
    let name = |l: &Value| format::text(&l["name"]);
    let bad: Vec<String> = logins
        .iter()
        .filter(|l| state(l) == EXPIRED || state(l) == MISSING)
        .map(|l| if state(l) == EXPIRED { format!("{} login expired", name(l)) } else { format!("{} isn't installed", name(l)) })
        .collect();
    let mut warnings: Vec<String> = logins.iter().filter(|l| state(l) == UNKNOWN).map(|l| format!("{} login unknown", name(l))).collect();
    let unchecked = logins.iter().any(|l| state(l) == UNCHECKED);
    for row in machine.iter().filter(|r| r["high"] == true) {
        warnings.push(format!("{} at {}%", format::text(&row["label"]), fmt0(row["percent"].as_f64().unwrap_or(0.0))));
    }
    for row in project_rows.iter().filter(|r| format::truthy(r.get("problem")) && !format::truthy(r.get("archived"))) {
        warnings.push(format!("{}: {}", format::text(&row["name"]), format::text(&row["problem"])));
    }
    let (level, messages, label) = if !bad.is_empty() {
        let label = if bad.len() == 1 { bad[0].clone() } else { format!("{} logins need fixing", bad.len()) };
        ("bad", [bad, warnings].concat(), label)
    } else if !warnings.is_empty() {
        let label = if warnings.len() == 1 { warnings[0].clone() } else { format!("{} health warnings", warnings.len()) };
        ("warn", warnings, label)
    } else if unchecked {
        ("warn", vec![], "Not checked yet".to_string())
    } else {
        ("ok", vec![], "All healthy".to_string())
    };
    json!({"level": level, "label": label, "messages": messages})
}

/// Everything the Health page shows. Reads files only: nothing here runs a check.
pub fn snapshot(cfg: Option<&Value>) -> Result<Value> {
    let cfg = loaded(cfg)?;
    let state = read_state()?;
    let mut attention = Vec::new();
    for slug in projects::list_slugs()? {
        let entries = items::list_attention(&slug, false)?;
        attention.push((slug, entries));
    }
    let reports = turn_reports(&attention)?;
    let stored = state.get("logins").cloned().unwrap_or_else(|| json!({}));
    let empty = json!({});
    let logins: Vec<Value> = login_checks(&cfg)?
        .iter()
        .map(|c| {
            let entry = stored.get(&c.name).filter(|e| format::truthy(Some(e))).unwrap_or(&empty);
            login_row(c, entry, reports.get(&c.name).map(Vec::as_slice).unwrap_or(&[]))
        })
        .collect();
    let machine = machine_rows(&cfg, &state);
    let project_rows = project_rows(&cfg, &attention)?;
    Ok(json!({
        "summary": summary(&logins, &machine, &project_rows),
        "checking": logins.iter().any(|l| l["checking"] == true),
        "logins": logins,
        "machine": machine,
        "projects": project_rows,
        "check_every": {"logins": cfg["health"]["login_check_every"], "machine": cfg["health"]["check_every"]},
    }))
    .map(|mut v| {
        // Key order: summary, logins, machine, projects, checking, check_every.
        let map = v.as_object_mut().unwrap();
        let checking = map.shift_remove("checking").unwrap();
        let check_every = map.shift_remove("check_every").unwrap();
        map.insert("checking".into(), checking);
        map.insert("check_every".into(), check_every);
        v
    })
}

/// Whether there's a login with this name, and the one command Fix may run for it right
/// now, if any. The command comes from config and this module, never from a request.
pub fn fix_for(name: &str, cfg: Option<&Value>) -> Result<(bool, Option<String>)> {
    let cfg = loaded(cfg)?;
    for row in snapshot(Some(&cfg))?["logins"].as_array().cloned().unwrap_or_default() {
        if row["name"] == name {
            return Ok((true, row["fix"].as_str().map(String::from)));
        }
    }
    Ok((false, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_words_split_with_quotes() {
        assert_eq!(shell_words(r#"test -f "/a b/c" 'd e' f\ g"#).unwrap(), vec!["test", "-f", "/a b/c", "d e", "f g"]);
        assert_eq!(shell_words("sleep 40").unwrap(), vec!["sleep", "40"]);
        assert!(shell_words("'open").is_err());
    }

    #[test]
    fn accounts() {
        assert_eq!(claude_account(r#"{"email": "a@b.c"}"#), "a@b.c");
        assert_eq!(codex_account("Logged in using ChatGPT"), "Using ChatGPT");
        assert_eq!(gh_account("Logged in to github.com account octo-cat"), "octo-cat");
        assert_eq!(storage_login("gdrive").unwrap(), "google-drive");
        assert_eq!(storage_login("s3").unwrap(), "rclone-s3");
    }
}
