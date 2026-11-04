//! The HTTP layer: how a path is decoded and matched, the bodies and headers of HTTP
//! errors (which skip the app's security headers), JSON responses, and file responses
//! (ETag, Last-Modified, conditional and range requests).

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::body::Bytes;
use std::path::Path;

/// A request, with its path decoded (see `unquote_path`).
pub struct Req {
    pub method: String,
    /// The path with every %XX decoded (as UTF-8 where valid).
    pub path: String,
    /// The path decoded except `%2F` and `%25`, which routing keeps.
    pub path_safe: String,
    pub query: Vec<(String, String)>,
    /// The method, URI and headers, kept whole for a websocket upgrade.
    pub parts: axum::http::request::Parts,
    pub body: Bytes,
    /// The body was over `MAX_UPLOAD`, and wasn't read.
    pub too_large: Option<u64>,
    /// Route parameters, decoded (`%2F` and `%25` too).
    pub params: Vec<(String, String)>,
}

impl Req {
    pub fn headers(&self) -> &HeaderMap {
        &self.parts.headers
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.parts.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// The Host header.
    pub fn host(&self) -> &str {
        self.header("host").unwrap_or("")
    }

    /// A query parameter: its first value.
    pub fn query(&self, key: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn param(&self, name: &str) -> &str {
        self.params.iter().find(|(k, _)| k == name).map_or("", |(_, v)| v.as_str())
    }

    /// A cookie's value, without the double quotes around a quoted one.
    pub fn cookie(&self, name: &str) -> Option<String> {
        for header in self.parts.headers.get_all("cookie") {
            let Ok(text) = header.to_str() else { continue };
            for pair in text.split(';') {
                let Some((key, value)) = pair.split_once('=') else { continue };
                if key.trim() == name {
                    let value = value.trim();
                    let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value);
                    return Some(value.to_string());
                }
            }
        }
        None
    }

    /// Whether the request has a body.
    pub fn can_read_body(&self) -> bool {
        !self.body.is_empty() || self.too_large.is_some()
    }

    /// The Content-Length header, as a number.
    pub fn content_length(&self) -> Option<u64> {
        self.header("content-length").and_then(|v| v.trim().parse().ok())
    }

    /// The body as JSON: a 413 if it was too large to read, `BadJson` if it isn't JSON.
    pub fn json(&self) -> Result<serde_json::Value, Fail> {
        if let Some(size) = self.too_large {
            return Err(raised(413, Some(&format!("Maximum request body size {} exceeded, actual body size {size}", super::MAX_UPLOAD))));
        }
        let text = std::str::from_utf8(&self.body).map_err(|_| Fail::BadJson)?;
        serde_json::from_str(text).map_err(|_| Fail::BadJson)
    }

    /// The body as a JSON object; any other JSON value is a 500.
    pub fn json_object(&self) -> Result<serde_json::Map<String, serde_json::Value>, Fail> {
        match self.json()? {
            serde_json::Value::Object(map) => Ok(map),
            _ => Err(Fail::Internal("the request body isn't a JSON object".into())),
        }
    }
}

pub enum RespBody {
    Bytes(Bytes),
    /// Already "prepared": a stream or an upgrade, which the middleware leaves alone.
    Prepared(axum::response::Response),
}

pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: RespBody,
}

/// Why a handler didn't return a response.
pub enum Fail {
    /// An HTTP error or redirect: sent as it is, without the security headers.
    Http(Box<Resp>),
    /// An `Error::Am`: `{"error": message}` with status 400.
    Am(String),
    /// A body that isn't valid JSON.
    BadJson,
    /// Anything else: a 500.
    Internal(String),
}

impl From<am_core::Error> for Fail {
    fn from(e: am_core::Error) -> Self {
        match e {
            am_core::Error::Am(message) => Fail::Am(message),
            other => Fail::Internal(other.to_string()),
        }
    }
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Self {
        Fail::Internal(e.to_string())
    }
}

pub type Handled = Result<Resp, Fail>;

/// Run a handler, turning a panic into a 500.
pub fn catch<T>(f: impl FnOnce() -> Result<T, Fail>) -> Result<T, Fail> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(panic) => Err(Fail::Internal(crate::daemon::panic_text(&*panic))),
    }
}

fn reason(status: u16) -> &'static str {
    StatusCode::from_u16(status).ok().and_then(|s| s.canonical_reason()).unwrap_or("")
}

impl Resp {
    pub fn new(status: u16, content_type: &str, body: impl Into<Bytes>) -> Self {
        Resp { status, headers: vec![("Content-Type".into(), content_type.into())], body: RespBody::Bytes(body.into()) }
    }

    /// An HTML page, as UTF-8.
    pub fn html(status: u16, text: impl Into<String>) -> Self {
        Resp::new(status, "text/html; charset=utf-8", text.into())
    }

    /// A JSON response: compact (`, ` and `: ` separators), with everything outside
    /// printable ASCII escaped.
    pub fn json(status: u16, data: &serde_json::Value) -> Self {
        Resp::new(status, "application/json; charset=utf-8", am_core::json::encode_ascii(data))
    }

    pub fn ok(data: &serde_json::Value) -> Handled {
        Ok(Resp::json(200, data))
    }

    pub fn prepared(response: axum::response::Response) -> Self {
        Resp { status: response.status().as_u16(), headers: vec![], body: RespBody::Prepared(response) }
    }

    pub fn is_prepared(&self) -> bool {
        matches!(self.body, RespBody::Prepared(_))
    }

    pub fn has_header(&self, name: &str) -> bool {
        self.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
    }

    pub fn set_default(&mut self, name: &str, value: &str) {
        if !self.has_header(name) {
            self.headers.push((name.into(), value.into()));
        }
    }

    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn into_response(self) -> axum::response::Response {
        let body = match self.body {
            RespBody::Prepared(response) => return response,
            RespBody::Bytes(bytes) => bytes,
        };
        let mut response = axum::response::Response::new(Body::from(body));
        *response.status_mut() = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let headers = response.headers_mut();
        for (name, value) in self.headers {
            if let (Ok(name), Ok(value)) = (axum::http::HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(&value)) {
                headers.append(name, value);
            }
        }
        response
    }
}

/// An HTTP error: `text/plain; charset=utf-8`, the text or `"<status>: <reason>"`.
pub fn raised(status: u16, text: Option<&str>) -> Fail {
    Fail::Http(Box::new(raised_resp(status, text)))
}

pub fn raised_resp(status: u16, text: Option<&str>) -> Resp {
    let body = match text {
        Some(text) => text.to_string(),
        None => format!("{status}: {}", reason(status)),
    };
    Resp::new(status, "text/plain; charset=utf-8", body)
}

pub fn not_found() -> Fail {
    raised(404, None)
}

/// A 302 redirect to `location`.
pub fn found(location: &str) -> Fail {
    Fail::Http(Box::new(raised_resp(302, None).with_header("Location", location)))
}

/// What's sent for an error a handler didn't expect.
pub fn internal_error() -> Resp {
    Resp::new(500, "text/plain; charset=utf-8", "500 Internal Server Error\n\nServer got itself in trouble")
}

// ---------------------------------------------------------------- paths and queries

fn hex_value(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

/// Decode a path. Runs of `%XX` decode as UTF-8; bytes that aren't valid UTF-8 keep
/// their original text. With `safe`, a decoded `/` or `%` stays encoded, as `%2F` and
/// `%25` (the `path_safe` that routes are matched against).
pub fn unquote_path(raw: &str, safe: bool) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            let next = raw[i..].char_indices().nth(1).map_or(raw.len(), |(n, _)| i + n);
            out.push_str(&raw[i..next]);
            i = next;
            continue;
        }
        // A run of %XX.
        let mut run: Vec<(u8, &str)> = Vec::new();
        let mut j = i;
        while j + 2 < bytes.len() && bytes[j] == b'%' {
            match (hex_value(bytes[j + 1]), hex_value(bytes[j + 2])) {
                (Some(h), Some(l)) => {
                    run.push((h * 16 + l, &raw[j..j + 3]));
                    j += 3;
                }
                _ => break,
            }
        }
        if run.is_empty() {
            out.push('%');
            i += 1;
            continue;
        }
        let mut k = 0;
        while k < run.len() {
            let byte = run[k].0;
            let width = match byte {
                0x00..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => 0,
            };
            let decoded = if width > 0 && k + width <= run.len() {
                let chunk: Vec<u8> = run[k..k + width].iter().map(|(b, _)| *b).collect();
                std::str::from_utf8(&chunk).ok().map(|s| s.to_string())
            } else {
                None
            };
            match decoded {
                Some(s) => {
                    match s.as_str() {
                        "/" if safe => out.push_str("%2F"),
                        "%" if safe => out.push_str("%25"),
                        _ => out.push_str(&s),
                    }
                    k += width;
                }
                None => {
                    out.push_str(run[k].1);
                    k += 1;
                }
            }
        }
        i = j;
    }
    out
}

/// Decode the `%2F` and `%25` left in a route parameter matched in `path_safe`.
pub fn unquote_param(value: &str) -> String {
    if !value.contains('%') {
        return value.to_string();
    }
    value.replace("%2F", "/").replace("%25", "%")
}

fn unquote_query_part(raw: &str) -> String {
    let spaced = raw.replace('+', " ");
    // Percent-decoding as for a path, but nothing stays encoded.
    unquote_path(&spaced, false)
}

/// A query string: `&`-separated, blank values kept, `+` as a space.
pub fn parse_query(raw: Option<&str>) -> Vec<(String, String)> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else { return vec![] };
    raw.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (unquote_query_part(key), unquote_query_part(value))
        })
        .collect()
}

/// Percent-encode `text` as UTF-8, except letters, digits, `_.-~` and the ASCII
/// characters in `safe`.
pub fn quote(text: &str, safe: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        let c = byte as char;
        if byte.is_ascii_alphanumeric() || "_.-~".contains(c) || (byte.is_ascii() && safe.contains(c)) {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------- files

/// An HTTP date (IMF-fixdate), as Last-Modified is written.
pub fn http_date(seconds: i64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let t = seconds as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::gmtime_r(&t, &mut tm) };
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        DAYS[tm.tm_wday as usize % 7],
        tm.tm_mday,
        MONTHS[tm.tm_mon as usize % 12],
        tm.tm_year + 1900,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Parse an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) to seconds.
pub fn parse_http_date(text: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() != 6 {
        return None;
    }
    let day: i32 = parts[1].parse().ok()?;
    let month = MONTHS.iter().position(|m| *m == parts[2])? as i32;
    let year: i32 = parts[3].parse().ok()?;
    let hms: Vec<i32> = parts[4].split(':').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    if hms.len() != 3 {
        return None;
    }
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month;
    tm.tm_mday = day;
    tm.tm_hour = hms[0];
    tm.tm_min = hms[1];
    tm.tm_sec = hms[2];
    Some(unsafe { libc::timegm(&mut tm) } as i64)
}

/// A file's Content-Type, from its suffix as it is, then lowercased; else
/// `application/octet-stream`.
pub fn guess_file_type(path: &Path) -> &'static str {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let suffix = match name.rfind('.') {
        Some(i) if i > 0 => &name[i..],
        _ => "",
    };
    super::mime_table::guess(suffix)
        .or_else(|| super::mime_table::guess(&suffix.to_lowercase()))
        .unwrap_or("application/octet-stream")
}

fn etag_matches(header: &str, etag: &str, weak: bool) -> bool {
    let header = header.trim();
    if header == "*" {
        return true;
    }
    header.split(',').map(str::trim).any(|candidate| {
        let (is_weak, value) = match candidate.strip_prefix("W/") {
            Some(rest) => (true, rest),
            None => (false, candidate),
        };
        (weak || !is_weak) && value.trim_matches('"') == etag
    })
}

/// Serve the file at `path`, with `headers` first, then the file's own (Content-Type
/// unless `headers` has one, Etag, Last-Modified). The middleware adds any security
/// headers still missing.
pub fn file_response(req: &Req, path: &Path, headers: Vec<(String, String)>) -> Handled {
    use std::os::unix::fs::MetadataExt;
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Ok(Resp { status: 403, headers, body: RespBody::Bytes(Bytes::new()) }),
        Err(_) => return Ok(Resp { status: 404, headers, body: RespBody::Bytes(Bytes::new()) }),
    };
    if !meta.is_file() {
        return Ok(Resp { status: 403, headers, body: RespBody::Bytes(Bytes::new()) });
    }
    let mtime_ns = meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128;
    let size = meta.len();
    let etag = format!("{mtime_ns:x}-{size:x}");
    let mtime = meta.mtime() as f64 + meta.mtime_nsec() as f64 * 1e-9;
    // Last-Modified rounds the mtime up to the second.
    let last_modified = http_date(mtime.ceil() as i64);
    let not_modified = match req.header("if-none-match") {
        Some(header) => etag_matches(header, &etag, true),
        None => req.header("if-modified-since").and_then(parse_http_date).is_some_and(|since| mtime <= since as f64),
    };
    let mut resp = Resp { status: 200, headers, body: RespBody::Bytes(Bytes::new()) };
    if not_modified {
        resp.status = 304;
        resp.headers.push(("Etag".into(), format!("\"{etag}\"")));
        resp.headers.push(("Last-Modified".into(), last_modified));
        return Ok(resp);
    }
    let data = std::fs::read(path)?;
    let size = data.len() as u64;
    let mut range: Option<(u64, u64)> = None;
    if let Some(header) = req.header("range") {
        match parse_range(header, size) {
            Ok(r) => range = r,
            Err(()) => {
                resp.status = 416;
                resp.headers.push(("Content-Range".into(), format!("bytes */{size}")));
                return Ok(resp);
            }
        }
    }
    if !resp.has_header("Content-Type") {
        resp.headers.push(("Content-Type".into(), guess_file_type(path).into()));
    }
    resp.headers.push(("Etag".into(), format!("\"{etag}\"")));
    resp.headers.push(("Last-Modified".into(), last_modified));
    resp.headers.push(("Accept-Ranges".into(), "bytes".into()));
    let body = match range {
        Some((start, count)) => {
            resp.status = 206;
            resp.headers.push(("Content-Range".into(), format!("bytes {start}-{}/{size}", start + count - 1)));
            Bytes::from(data).slice(start as usize..(start + count) as usize)
        }
        None => Bytes::from(data),
    };
    resp.body = RespBody::Bytes(body);
    Ok(resp)
}

/// One `bytes=` range: `Ok(None)` for no range, `Err` for 416, else the start and the count.
fn parse_range(header: &str, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = header.strip_prefix("bytes=") else { return Err(()) };
    let (start, end) = spec.split_once('-').ok_or(())?;
    let (start, end) = (start.trim(), end.trim());
    let (start, count) = if start.is_empty() {
        let suffix: u64 = end.parse().map_err(|_| ())?;
        let start = size.saturating_sub(suffix);
        (start, size - start)
    } else {
        let start: u64 = start.parse().map_err(|_| ())?;
        let stop = if end.is_empty() { size } else { end.parse::<u64>().map_err(|_| ())?.saturating_add(1).min(size) };
        if stop < start {
            return Err(());
        }
        (start, stop - start)
    };
    if start >= size {
        return Err(());
    }
    Ok(Some((start, count)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_decode_utf8_runs_and_keep_the_rest() {
        assert_eq!(unquote_path("/static/%2e%2e/config.toml", false), "/static/../config.toml");
        assert_eq!(unquote_path("/a/%2Fb/c%20d", true), "/a/%2Fb/c d");
        assert_eq!(unquote_path("/x/..%2f..%2fy", true), "/x/..%2F..%2Fy");
        assert_eq!(unquote_path("/a/%25%2F%2f", false), "/a/%//");
        assert_eq!(unquote_path("/a/%25%2F%2f", true), "/a/%25%2F%2F");
        assert_eq!(unquote_path("/a/%C3%A9", false), "/a/é");
        assert_eq!(unquote_path("/a/%C3", false), "/a/%C3");
        assert_eq!(unquote_path("/a/%zz", false), "/a/%zz");
        assert_eq!(unquote_path("/a/%e2%82", false), "/a/%e2%82");
        assert_eq!(unquote_path("/a/b%2Bc+d", false), "/a/b+c+d");
        assert_eq!(unquote_path("/a/%3F%23", false), "/a/?#");
    }

    #[test]
    fn queries_parse() {
        let q = parse_query(Some("q=1+2&token=a%2Bb&e&f=&q=3"));
        assert_eq!(q, vec![("q".into(), "1 2".into()), ("token".into(), "a+b".into()), ("e".into(), "".into()), ("f".into(), "".into()), ("q".into(), "3".into())]);
    }

    #[test]
    fn quote_escapes_all_but_unreserved_and_safe() {
        assert_eq!(quote("a b/c@d é", "/"), "a%20b/c%40d%20%C3%A9");
        assert_eq!(quote("artifacts@abc", "@"), "artifacts@abc");
    }

    #[test]
    fn http_dates_round_trip() {
        assert_eq!(http_date(784111777), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"), Some(784111777));
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-1", 10), Ok(Some((0, 2))));
        assert_eq!(parse_range("bytes=-3", 10), Ok(Some((7, 3))));
        assert_eq!(parse_range("bytes=5-", 10), Ok(Some((5, 5))));
        assert_eq!(parse_range("bytes=10-", 10), Err(()));
    }
}
