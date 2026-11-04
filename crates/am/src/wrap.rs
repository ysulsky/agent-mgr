//! Word wrapping for the help page (`help.rs`).
//!
//! Each whitespace character counts as a space. The text is split into chunks: runs of
//! whitespace, words, and em-dashes (two or more `-` between words). A word also splits
//! after a hyphen between letters, so `re-run` can wrap as `re-` `run`, but `1-5` can't.
//! Each line takes whole chunks while they fit in the width. A chunk wider than the width
//! is broken to fill the rest of the line, after its last hyphen in that space if there's
//! one with something other than hyphens before it. Whitespace is dropped at the end of
//! every line and at the start of every line but the first. Widths count characters.

fn is_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0b' | '\x0c' | '\r' | ' ')
}

/// A word character: alphanumeric or `_`.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A word character that isn't a digit.
fn is_letter(c: char) -> bool {
    is_word(c) && !c.is_numeric()
}

/// A word character or one of `!"'&.,?`: what can come before an em-dash.
fn is_word_punct(c: char) -> bool {
    is_word(c) || matches!(c, '!' | '"' | '\'' | '&' | '.' | ',' | '?')
}

/// The length of the em-dash at `i`, if there is one: two or more `-` after a word or
/// punctuation character and before a word character.
fn em_dash_at(s: &[char], i: usize) -> Option<usize> {
    if i == 0 || !is_word_punct(s[i - 1]) {
        return None;
    }
    let mut j = i;
    while j < s.len() && s[j] == '-' {
        j += 1;
    }
    (j - i >= 2 && j < s.len() && is_word(s[j])).then_some(j - i)
}

/// The chunks: whitespace runs, em-dashes, and words, split after hyphens. A hyphen splits
/// a word when it has two letters, or a letter, `-` and a letter, before it, and a letter
/// after it followed by a letter or by `-` and a letter.
fn split(s: &[char]) -> Vec<String> {
    let at = |k: usize| s.get(k).copied();
    let letter = |k: usize| at(k).is_some_and(is_letter);
    let mut chunks = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let start = i;
        if is_ws(s[i]) {
            while i < s.len() && is_ws(s[i]) {
                i += 1;
            }
        } else if let Some(n) = em_dash_at(s, i) {
            i += n;
        } else {
            // The shortest run of non-whitespace that ends with a hyphen to break after,
            // at the end of the word, or just before an em-dash.
            let mut j = i + 1;
            loop {
                if at(j) == Some('-') {
                    let behind = (j >= 2 && letter(j - 2) && letter(j - 1))
                        || (j >= 3 && letter(j - 3) && at(j - 2) == Some('-') && letter(j - 1));
                    let ahead = letter(j + 1) && (letter(j + 2) || (at(j + 2) == Some('-') && letter(j + 3)));
                    if behind && ahead {
                        j += 1;
                        break;
                    }
                }
                if j >= s.len() || is_ws(s[j]) {
                    break;
                }
                if is_word_punct(s[j - 1]) && em_dash_at(s, j).is_some() {
                    break;
                }
                j += 1;
            }
            i = j;
        }
        chunks.push(s[start..i].iter().collect());
    }
    chunks
}

fn len(s: &str) -> usize {
    s.chars().count()
}

/// `text` in lines of at most `width` characters. The help page collapses its whitespace
/// first.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().map(|c| if is_ws(c) { ' ' } else { c }).collect();
    let mut chunks = split(&chars);
    chunks.reverse();
    let mut lines: Vec<String> = Vec::new();
    while !chunks.is_empty() {
        let mut cur: Vec<String> = Vec::new();
        let mut cur_len = 0;
        if chunks.last().is_some_and(|c| c.trim().is_empty()) && !lines.is_empty() {
            chunks.pop();
        }
        while let Some(chunk) = chunks.last() {
            let l = len(chunk);
            if cur_len + l <= width {
                cur_len += l;
                cur.push(chunks.pop().unwrap());
            } else {
                break;
            }
        }
        if chunks.last().is_some_and(|c| len(c) > width) {
            // A chunk wider than a line: fill the line with its start, breaking after a
            // hyphen if there's one to break after.
            let space_left = if width < 1 { 1 } else { width - cur_len };
            let chunk: Vec<char> = chunks.last().unwrap().chars().collect();
            let mut end = space_left;
            if chunk.len() > space_left {
                if let Some(h) = chunk[..space_left].iter().rposition(|c| *c == '-') {
                    if h > 0 && chunk[..h].iter().any(|c| *c != '-') {
                        end = h + 1;
                    }
                }
            }
            cur.push(chunk[..end].iter().collect());
            *chunks.last_mut().unwrap() = chunk[end..].iter().collect();
            cur_len = cur.iter().map(|c| len(c)).sum();
        }
        if cur.last().is_some_and(|c| c.trim().is_empty()) {
            cur_len -= len(cur.last().unwrap());
            cur.pop();
        }
        let _ = cur_len;
        if !cur.is_empty() {
            lines.push(cur.concat());
        }
    }
    lines
}

/// The wrapped lines joined with newlines, each starting with `indent`, which counts
/// toward `width`.
pub fn fill(text: &str, width: usize, indent: &str) -> String {
    let inner = width.saturating_sub(len(indent)).max(1);
    wrap(text, inner).iter().map(|l| format!("{indent}{l}")).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_words() {
        let s: Vec<char> = "safe to re-run, five-field cron '0 9 * * 1-5' -- x a--b".chars().collect();
        let chunks = split(&s);
        assert!(chunks.contains(&"re-".to_string()) && chunks.contains(&"run,".to_string()));
        assert!(chunks.contains(&"five-".to_string()) && chunks.contains(&"field".to_string()));
        assert!(chunks.contains(&"1-5'".to_string()));
        assert!(chunks.contains(&"a".to_string()) && chunks.contains(&"--".to_string()));
    }

    #[test]
    fn wraps_long_text() {
        assert_eq!(wrap("create ~/agents with config.toml and the home project; safe to re-run", 30), vec![
            "create ~/agents with",
            "config.toml and the home",
            "project; safe to re-run"
        ]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    }
}
