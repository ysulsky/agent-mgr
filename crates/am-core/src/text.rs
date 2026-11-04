//! Short forms of text: a first line and an excerpt.

use crate::format::is_space;

/// The first non-blank line, stripped, cut to `limit` characters.
pub fn first_line(text: &str, limit: usize) -> String {
    for line in crate::format::split_lines(text) {
        let line = line.trim_matches(is_space);
        if !line.is_empty() {
            return cut(line, limit);
        }
    }
    String::new()
}

/// Whitespace collapsed, cut to `limit` characters with an ellipsis. 240 is the usual
/// limit.
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
