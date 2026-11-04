//! Ids and slugs.

use crate::{am_err, Result};
use regex::Regex;
use std::sync::OnceLock;

const BASE36: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

pub(crate) fn random_from(alphabet: &[u8], n: usize) -> String {
    let mut out = String::with_capacity(n);
    let limit = 256 - 256 % alphabet.len();
    while out.len() < n {
        let mut buf = [0u8; 32];
        getrandom::getrandom(&mut buf).expect("getrandom");
        for b in buf {
            if (b as usize) < limit && out.len() < n {
                out.push(alphabet[b as usize % alphabet.len()] as char);
            }
        }
    }
    out
}

/// A short id that sorts by creation time, `m-0mh3k2x9a7f3q`: nine base36 digits of
/// milliseconds, then four random ones.
pub fn new_id(prefix: &str) -> String {
    let mut ms = (crate::clock::now() * 1000.0) as u64;
    let mut stamp = Vec::new();
    while ms > 0 {
        stamp.push(BASE36[(ms % 36) as usize]);
        ms /= 36;
    }
    while stamp.len() < 9 {
        stamp.push(b'0');
    }
    stamp.reverse();
    format!("{prefix}-{}{}", String::from_utf8(stamp).unwrap(), random_from(BASE36, 4))
}

/// Valid agent names. Like the other patterns here, it also allows one final newline.
pub fn agent_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9][a-z0-9_-]{0,47}\n?$").unwrap())
}

/// Valid project slugs, also allowing one final newline.
pub fn slug_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9][a-z0-9-]{0,63}\n?$").unwrap())
}

/// Whether `item_id` is `^[a-z]-[a-z0-9-]{1,80}$`, also allowing one final newline.
pub fn is_item_id(item_id: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z]-[a-z0-9-]{1,80}\n?$").unwrap()).is_match(item_id)
}

/// Check an id: ids become file names, so one from a request can't be a path.
pub fn check_item_id(item_id: &str) -> Result<&str> {
    if is_item_id(item_id) {
        Ok(item_id)
    } else {
        am_err(format!("invalid id {}", crate::format::try_quote(item_id).unwrap_or_else(|_| format!("'{item_id}'"))))
    }
}

/// A project slug from a name: lowercased, each run of other characters as `-`, no `-` at
/// either end, at most 64 characters.
pub fn slugify(name: &str) -> Result<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let lowered = name.to_lowercase();
    let replaced = RE.get_or_init(|| Regex::new(r"[^a-z0-9]+").unwrap()).replace_all(&lowered, "-");
    let trimmed: String = replaced.trim_matches('-').chars().take(64).collect();
    let slug = trimmed.trim_matches('-').to_string();
    if slug.is_empty() {
        return am_err(format!("cannot make a project slug from {}", crate::format::try_quote(name).unwrap_or_else(|_| format!("'{name}'"))));
    }
    Ok(slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_sort_by_time_and_slugs() {
        let a = new_id("m");
        assert_eq!(a.len(), 2 + 13);
        assert!(check_item_id(&a).is_ok());
        assert!(check_item_id("../x").is_err());
        assert!(is_item_id("m-abc\n") && !is_item_id("m-abc\n\n") && !is_item_id("m-"));
        assert_eq!(slugify("Agent Mgr!").unwrap(), "agent-mgr");
        assert!(slugify("!!!").is_err());
    }
}
