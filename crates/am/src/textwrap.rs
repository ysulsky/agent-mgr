//! Python 3.11's `textwrap.wrap`, as argparse's help formatter uses it: default options,
//! so long words break and words break after hyphens (`re-run` -> `re-` `run`).

fn is_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0b' | '\x0c' | '\r' | ' ')
}

/// `\w`.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `[^\d\W]`: a word character that isn't a digit.
fn is_letter(c: char) -> bool {
    is_word(c) && !c.is_numeric()
}

/// `[\w!"'&.,?]`.
fn is_word_punct(c: char) -> bool {
    is_word(c) || matches!(c, '!' | '"' | '\'' | '&' | '.' | ',' | '?')
}

/// `(?<=wp)-{2,}(?=\w)` at `i`: the length of the em-dash, if there is one.
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

/// `TextWrapper._split`: whitespace runs, em-dashes, and words split after hyphens.
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
            // `nws+?` then one of: a hyphen to break after, the end of the word, an em-dash.
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

/// `textwrap.wrap(text, width)`, with the text already whitespace-collapsed by argparse.
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
            // _handle_long_word, breaking long words (and after hyphens).
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

/// `textwrap.fill(text, width, initial_indent=indent, subsequent_indent=indent)`.
pub fn fill(text: &str, width: usize, indent: &str) -> String {
    let inner = width.saturating_sub(len(indent)).max(1);
    wrap(text, inner).iter().map(|l| format!("{indent}{l}")).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_python() {
        let s: Vec<char> = "safe to re-run, five-field cron '0 9 * * 1-5' -- x a--b".chars().collect();
        let chunks = split(&s);
        assert!(chunks.contains(&"re-".to_string()) && chunks.contains(&"run,".to_string()));
        assert!(chunks.contains(&"five-".to_string()) && chunks.contains(&"field".to_string()));
        assert!(chunks.contains(&"1-5'".to_string()));
        assert!(chunks.contains(&"a".to_string()) && chunks.contains(&"--".to_string()));
    }

    #[test]
    fn wraps_like_python() {
        assert_eq!(wrap("create ~/agents with config.toml and the home project; safe to re-run", 30), vec![
            "create ~/agents with",
            "config.toml and the home",
            "project; safe to re-run"
        ]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    }
}
