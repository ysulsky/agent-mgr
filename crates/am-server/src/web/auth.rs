//! Who may ask: the session cookie, capability tokens for artifacts, the private host name
//! and the same-origin check (`web.py`, "auth").

use super::http::{found, quote, raised, Fail, Req};
use crate::daemon::{hex, hmac_sha256, web_token};
use am_core::Result;

pub const COOKIE: &str = "am_session";
/// A capability lasts 12 to 13 hours: see `make_capability`.
pub const CAP_MIN_LIFETIME: f64 = 12.0 * 3600.0;

/// Constant-time equality (`hmac.compare_digest`).
pub fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn session_value() -> Result<String> {
    Ok(hex(&hmac_sha256(web_token(false)?.as_bytes(), b"am-session-v1")))
}

pub fn authed(req: &Req) -> Result<bool> {
    let cookie = req.cookie(COOKIE).unwrap_or_default();
    Ok(!cookie.is_empty() && same(cookie.as_bytes(), session_value()?.as_bytes()))
}

fn capability_key() -> Result<[u8; 32]> {
    Ok(hmac_sha256(web_token(false)?.as_bytes(), b"am-artifact-capability-v1"))
}

fn capability_signature(slug: &str, expiry: u64) -> Result<String> {
    let full = hex(&hmac_sha256(&capability_key()?, format!("{slug}\n{expiry}").as_bytes()));
    Ok(full[..40].to_string())
}

/// Port of `web.make_capability`.
///
/// A token for GETs of one project's artifacts and results. Expiry rounds up to
/// the hour: it lasts 12 to 13 hours, and URLs stay the same within an hour, so
/// artifacts' relative images and scripts cache.
pub fn make_capability(slug: &str, at: Option<f64>) -> Result<String> {
    let at = at.unwrap_or_else(am_core::now);
    let expiry = (((at + CAP_MIN_LIFETIME) / 3600.0).ceil() * 3600.0) as u64;
    Ok(format!("{expiry}.{}", capability_signature(slug, expiry)?))
}

/// Port of `web.check_capability`.
pub fn check_capability(token: &str, slug: &str, at: Option<f64>) -> Result<bool> {
    let at = at.unwrap_or_else(am_core::now);
    let (expiry_text, signature) = token.split_once('.').unwrap_or((token, ""));
    if expiry_text.is_empty() || !expiry_text.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(false);
    }
    let Ok(expiry) = expiry_text.parse::<u64>() else { return Ok(false) };
    if (expiry as f64) < at {
        return Ok(false);
    }
    Ok(same(signature.as_bytes(), capability_signature(slug, expiry)?.as_bytes()))
}

pub fn is_same_origin(req: &Req) -> bool {
    match req.header("origin") {
        // Not sent by a browser's cross-origin machinery, and still needs the cookie.
        None => true,
        Some(origin) => origin.split_once("://").map_or(origin, |(_, rest)| rest) == req.host(),
    }
}

/// `_check_host`: only the private name is answered, but the token on any name is sent on
/// to it.
pub fn check_host(expected: Option<&str>, req: &Req) -> Result<(), Fail> {
    let Some(expected) = expected else { return Ok(()) };
    let host = req.host();
    let name = host.rsplit_once(':').map_or(host, |(name, _)| name);
    if name.to_lowercase() == expected {
        return Ok(());
    }
    // Only someone with the token learns the private name: anyone who knew it could host
    // a page under it on another port, and the browser would send that page the cookie.
    let token = req.query("token").unwrap_or("");
    if req.path == "/" && !token.is_empty() && same(token.as_bytes(), web_token(false)?.as_bytes()) {
        let authority = match host.rsplit_once(':') {
            Some((_, port)) if !port.is_empty() => format!("{expected}:{port}"),
            _ => expected.to_string(),
        };
        return Err(found(&format!("http://{authority}/?token={}", quote(token, "/"))));
    }
    Err(raised(421, Some("Open the address from `am url`.")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_token<T>(token: &str, f: impl FnOnce() -> T) -> T {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".web-token"), format!("{token}\n")).unwrap();
        let _guard = crate::test_env::lock();
        std::env::set_var("AM_HOME", dir.path());
        let out = f();
        std::env::remove_var("AM_HOME");
        out
    }

    #[test]
    fn capability_tokens_last_12_to_13_hours() {
        with_token("k", || {
            let at = 1_800_000_000.0;
            let token = make_capability("demo", Some(at)).unwrap();
            let expiry: f64 = token.split('.').next().unwrap().parse().unwrap();
            assert!(CAP_MIN_LIFETIME <= expiry - at && expiry - at <= CAP_MIN_LIFETIME + 3600.0);
            assert!(check_capability(&token, "demo", Some(at)).unwrap());
            assert!(!check_capability(&token, "other", Some(at)).unwrap(), "a capability is for one project");
            assert!(!check_capability(&token, "demo", Some(expiry + 1.0)).unwrap(), "it expires");
            let forged = format!("{}.{}", expiry as u64 + 3600, token.split('.').nth(1).unwrap());
            assert!(!check_capability(&forged, "demo", Some(at)).unwrap(), "the expiry is signed");
            assert!(!check_capability("garbage", "demo", Some(at)).unwrap());
        });
        // What Python makes with the token "k": web.make_capability("demo", 1_800_000_000.0)
        // and web._session_value(). Links survive a switch of implementation.
        let python = "1800043200.97faa20ee0db739ec2ede9b9ccdc4f76a1a85411";
        with_token("k", || {
            assert_eq!(make_capability("demo", Some(1_800_000_000.0)).unwrap(), python);
            assert_eq!(session_value().unwrap(), "0bb53d62316e42a7cd33bff18d7325132c20ede592ca516c7fd78af12265e37c");
        });
        with_token("another-token", || {
            assert!(!check_capability(python, "demo", Some(1_800_000_000.0)).unwrap(), "changing the web token revokes them");
        });
    }
}
