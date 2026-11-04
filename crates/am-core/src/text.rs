//! `core.first_line` and `core.excerpt`.

use crate::py::is_space;

/// `core.first_line`: the first non-blank line, stripped, cut to `limit` characters.
pub fn first_line(text: &str, limit: usize) -> String {
    for line in crate::py::splitlines(text) {
        let line = line.trim_matches(is_space);
        if !line.is_empty() {
            return cut(line, limit);
        }
    }
    String::new()
}

/// `core.excerpt`: whitespace collapsed, cut to `limit` characters with an ellipsis.
/// Python's default limit is 240.
pub fn excerpt(text: &str, limit: usize) -> String {
    let flat = text.split(is_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    cut(&flat, limit)
}

fn cut(s: &str, limit: usize) -> String {
    if s.chars().count() > limit {
        let mut out: String = s.chars().take(limit.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_and_first_line() {
        assert_eq!(excerpt("  a \n\t b  ", 240), "a b");
        assert_eq!(excerpt("abcdef", 4), "abc…");
        assert_eq!(first_line("\n  \n  hello there \nnext", 200), "hello there");
        assert_eq!(first_line("", 200), "");
    }
}
