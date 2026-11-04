//! The usage text and the help page for the parsers in `args.rs`.
//!
//! Everything is laid out for a width: `$COLUMNS` if it's a positive integer, else the
//! width of stdout's terminal, else 80; less 2. Widths count characters.
//!
//! # Usage
//!
//! `usage: PROG`, then the options in the order they were added, then the positionals.
//! An option shows its first option string and its values (`--project PROJECT`), in
//! brackets unless it's required; a flag shows just its option string. A value is shown
//! as the argument's metavar, else `{a,b}` for choices, else its dest, upper-cased for an
//! option; repeated by nargs as `X`, `[X]`, `[X ...]`, `X [X ...]` or `X ...` (`args.rs`).
//!
//! Usage wider than the width wraps by parts: a bracketed option, or a word. Parts fill
//! each line up to the width; one wider than a line gets a line of its own.
//!
//! - When `usage: PROG` takes at most 3/4 of the width, the parts follow PROG on its line,
//!   and the lines after it are indented to line up with the first part. Positionals start
//!   a new line when there are options.
//! - Otherwise PROG stands alone, and the parts follow on lines indented 7 spaces (under
//!   PROG). If they need more than one line, the positionals start a new line.
//!
//! # The help page
//!
//! The usage, the description, then the sections `positional arguments:` and `options:`,
//! each left out when empty, with single blank lines between. Text has its whitespace
//! collapsed and is word-wrapped (`wrap.rs`): the description to the width, at least 11.
//!
//! A section lists each argument indented 2: a positional by its metavar, an option by
//! each of its option strings with its values (`-f FILE, --file FILE`). Subcommands that
//! have help are listed under their argument, indented 4. The help text goes in a column:
//! 2 spaces past the widest entry (counting subcommands, all measured at indent 2), but at
//! most 24, or the width less 20 if that's smaller (and at least 4). It's wrapped to the
//! rest of the width (at least 11), with each further line indented to the column. An entry
//! too wide to leave 2 spaces before the column has its help start on the next line.

use crate::args::{Action, Kind, Nargs, Parser};
use crate::wrap;

fn clen(s: &str) -> usize {
    s.chars().count()
}

/// The width to lay out for: $COLUMNS, else stdout's terminal, else 80; less 2.
pub fn width() -> usize {
    let env = std::env::var("COLUMNS").ok().and_then(|c| c.trim().parse::<i64>().ok()).unwrap_or(0);
    let columns = if env > 0 {
        env as usize
    } else {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0;
        if ok && ws.ws_col > 0 { ws.ws_col as usize } else { 80 }
    };
    columns.saturating_sub(2)
}

/// How an argument's value is shown: its metavar, else `{a,b}` for choices, else its dest
/// (upper-cased for an option).
fn metavar(a: &Action) -> String {
    if let Some(m) = &a.metavar {
        m.clone()
    } else if let Some(c) = &a.choices {
        format!("{{{}}}", c.join(","))
    } else if a.option_strings.is_empty() {
        a.dest.clone()
    } else {
        a.dest.to_uppercase()
    }
}

/// An argument's values, by nargs.
fn format_args(a: &Action) -> String {
    let m = metavar(a);
    match a.nargs {
        Nargs::One => m,
        Nargs::Optional => format!("[{m}]"),
        Nargs::ZeroOrMore => format!("[{m} ...]"),
        Nargs::OneOrMore => format!("{m} [{m} ...]"),
        Nargs::Parser => format!("{m} ..."),
        Nargs::Zero => String::new(),
    }
}

/// An argument as the help page lists it: `-f FILE, --file FILE`.
fn invocation(a: &Action) -> String {
    if a.option_strings.is_empty() {
        metavar(a)
    } else if a.nargs == Nargs::Zero {
        a.option_strings.join(", ")
    } else {
        let args = format_args(a);
        a.option_strings.iter().map(|o| format!("{o} {args}")).collect::<Vec<_>>().join(", ")
    }
}

/// Usage for these arguments: those not required in brackets, with spaces just inside
/// brackets and empty brackets removed.
fn actions_usage(actions: &[&Action]) -> String {
    let parts: Vec<String> = actions
        .iter()
        .map(|a| {
            if a.option_strings.is_empty() {
                format_args(a)
            } else {
                let part = if a.nargs == Nargs::Zero {
                    a.option_strings[0].clone()
                } else {
                    format!("{} {}", a.option_strings[0], format_args(a))
                };
                if a.required { part } else { format!("[{part}]") }
            }
        })
        .collect();
    let text = parts.join(" ");
    let text = regex::Regex::new(r"([\[(]) ").unwrap().replace_all(&text, "$1").into_owned();
    let text = regex::Regex::new(r" ([\])])").unwrap().replace_all(&text, "$1").into_owned();
    let text = regex::Regex::new(r"[\[(] *[\])]").unwrap().replace_all(&text, "").into_owned();
    text.trim().to_string()
}

/// The parts usage wraps by, as `\(.*?\)+(?=\s|$)|\[.*?\]+(?=\s|$)|\S+` finds them: a
/// bracketed group that ends a word, else a run of non-whitespace.
fn usage_parts(text: &str) -> Vec<String> {
    let s: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s[i].is_whitespace() {
            i += 1;
            continue;
        }
        let mut matched = None;
        for (open, close) in [('(', ')'), ('[', ']')] {
            if s[i] != open || matched.is_some() {
                continue;
            }
            for j in i + 1..s.len() {
                if s[j] == '\n' {
                    break;
                }
                if s[j] == close {
                    let mut k = j;
                    while k < s.len() && s[k] == close {
                        k += 1;
                    }
                    if k == s.len() || s[k].is_whitespace() {
                        matched = Some(k);
                        break;
                    }
                }
            }
        }
        let end = matched.unwrap_or_else(|| {
            let mut k = i;
            while k < s.len() && !s[k].is_whitespace() {
                k += 1;
            }
            k
        });
        out.push(s[i..end].iter().collect());
        i = end;
    }
    out
}

impl Parser {
    /// The usage text alone: `usage: ...\n`.
    pub fn format_usage(&self) -> String {
        self.usage_text(width()).trim_end_matches('\n').to_string() + "\n"
    }

    /// The usage text for `width`, with two trailing newlines.
    fn usage_text(&self, width: usize) -> String {
        let prefix = "usage: ";
        let prog = self.prog.clone();
        let optionals: Vec<&Action> = self.actions.iter().filter(|a| !a.option_strings.is_empty()).collect();
        let positionals: Vec<&Action> = self.actions.iter().filter(|a| a.option_strings.is_empty()).collect();
        let all: Vec<&Action> = optionals.iter().chain(positionals.iter()).copied().collect();
        let action_usage = actions_usage(&all);
        let mut usage = [prog.clone(), action_usage].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
        let text_width = width as isize;
        if (clen(prefix) + clen(&usage)) as isize > text_width {
            let opt_parts = usage_parts(&actions_usage(&optionals));
            let pos_parts = usage_parts(&actions_usage(&positionals));
            let get_lines = |parts: &[String], indent: &str, prefix: Option<&str>| -> Vec<String> {
                let mut lines: Vec<String> = Vec::new();
                let mut line: Vec<&str> = Vec::new();
                let mut line_len = clen(prefix.unwrap_or(indent)) as isize - 1;
                for part in parts {
                    if line_len + 1 + clen(part) as isize > text_width && !line.is_empty() {
                        lines.push(format!("{indent}{}", line.join(" ")));
                        line.clear();
                        line_len = clen(indent) as isize - 1;
                    }
                    line.push(part);
                    line_len += clen(part) as isize + 1;
                }
                if !line.is_empty() {
                    lines.push(format!("{indent}{}", line.join(" ")));
                }
                if prefix.is_some() {
                    lines[0] = lines[0].chars().skip(clen(indent)).collect();
                }
                lines
            };
            let lines: Vec<String> = if (clen(prefix) + clen(&prog)) as f64 <= 0.75 * text_width as f64 {
                let indent = " ".repeat(clen(prefix) + clen(&prog) + 1);
                if !opt_parts.is_empty() {
                    let mut first = vec![prog.clone()];
                    first.extend(opt_parts.iter().cloned());
                    let mut lines = get_lines(&first, &indent, Some(prefix));
                    lines.extend(get_lines(&pos_parts, &indent, None));
                    lines
                } else if !pos_parts.is_empty() {
                    let mut first = vec![prog.clone()];
                    first.extend(pos_parts.iter().cloned());
                    get_lines(&first, &indent, Some(prefix))
                } else {
                    vec![prog.clone()]
                }
            } else {
                let indent = " ".repeat(clen(prefix));
                let parts: Vec<String> = opt_parts.iter().chain(pos_parts.iter()).cloned().collect();
                let mut lines = get_lines(&parts, &indent, None);
                if lines.len() > 1 {
                    lines = get_lines(&opt_parts, &indent, None);
                    lines.extend(get_lines(&pos_parts, &indent, None));
                }
                std::iter::once(prog.clone()).chain(lines).collect()
            };
            usage = lines.join("\n");
        }
        format!("{prefix}{usage}\n\n")
    }

    /// The help page.
    pub fn format_help(&self) -> String {
        let width = width();
        let max_help_position = 24.min((width as isize - 20).max(4) as usize);
        // The widest invocation, subcommands included, measured at the sections' indent (2).
        let mut action_max = 0;
        for a in &self.actions {
            let mut longest = clen(&invocation(a));
            for (name, _) in &a.choice_help {
                longest = longest.max(clen(name));
            }
            action_max = action_max.max(longest + 2);
        }
        let help_position = (action_max + 2).min(max_help_position);
        let mut out = self.usage_text(width);
        if let Some(d) = &self.description {
            let text = collapse(d);
            out.push_str(&wrap::fill(&text, width.max(11), ""));
            out.push_str("\n\n");
        }
        for (title, positional) in [("positional arguments", true), ("options", false)] {
            let actions: Vec<&Action> =
                self.actions.iter().filter(|a| a.option_strings.is_empty() == positional).collect();
            if actions.is_empty() {
                continue;
            }
            let mut section = String::new();
            for a in actions {
                section.push_str(&format_action(&invocation(a), a.help.as_deref(), 2, help_position, width));
                if a.kind == Kind::Subparsers {
                    for (name, help) in &a.choice_help {
                        section.push_str(&format_action(name, Some(help), 4, help_position, width));
                    }
                }
            }
            out.push_str(&format!("\n{title}:\n{section}\n"));
        }
        let mut help = out;
        while help.contains("\n\n\n") {
            help = help.replace("\n\n\n", "\n\n");
        }
        help.trim_matches('\n').to_string() + "\n"
    }
}

/// Runs of ASCII whitespace as one space, trimmed.
fn collapse(text: &str) -> String {
    text.split(|c: char| c.is_ascii_whitespace()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// One argument's entry on the help page: `header` at `indent`, its help at `help_position`.
fn format_action(header: &str, help: Option<&str>, indent: usize, help_position: usize, width: usize) -> String {
    let help_width = (width as isize - help_position as isize).max(11) as usize;
    let action_width = help_position as isize - indent as isize - 2;
    let pad = " ".repeat(indent);
    let help = help.filter(|h| !h.is_empty());
    let (mut out, indent_first) = match help {
        None => (format!("{pad}{header}\n"), 0),
        Some(_) if clen(header) as isize <= action_width => {
            (format!("{pad}{header}{}  ", " ".repeat(action_width as usize - clen(header))), 0)
        }
        Some(_) => (format!("{pad}{header}\n"), help_position),
    };
    if let Some(h) = help {
        let text = collapse(h);
        if !text.is_empty() {
            let lines = wrap::wrap(&text, help_width);
            if let Some((first, rest)) = lines.split_first() {
                out.push_str(&format!("{}{first}\n", " ".repeat(indent_first)));
                for line in rest {
                    out.push_str(&format!("{}{line}\n", " ".repeat(help_position)));
                }
            }
        } else if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}
