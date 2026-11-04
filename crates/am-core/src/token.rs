//! The web access token (`daemon.web_token`, moved here so `am setup` needn't depend on
//! the server).

use crate::{am_err, fsx::atomic_write, paths::token_path, Result};
use base64::Engine;

/// The token in `$AM_HOME/.web-token`, created (32 random bytes, URL-safe base64, as
/// `secrets.token_urlsafe(32)`) if `create` and it's missing.
pub fn web_token(create: bool) -> Result<String> {
    let path = token_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => return Ok(crate::py::strip(&text).to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !create {
                return am_err(format!("no web token at {}; run `am setup`", path.display()));
            }
        }
        Err(e) => return Err(e.into()),
    }
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("getrandom");
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    atomic_write(&path, format!("{token}\n").as_bytes(), 0o600)?;
    Ok(token)
}
