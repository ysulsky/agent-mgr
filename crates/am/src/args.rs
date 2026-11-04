//! Command-line parsing for `am`. Each command and subcommand has a [`Parser`] with options,
//! positionals and at most one set of subcommands; parsing gives a [`Parsed`], or an
//! [`Exit`] for help and usage errors.
//!
//! # Classifying arguments
//!
//! Every argument is first classified against the parser's own option strings:
//!
//! - `--` ends the options: every argument after it is a positional, even one starting
//!   with `-`. Each positional drops the first `--` from the arguments it takes.
//! - An argument that doesn't start with `-`, the empty string, and `-` alone are
//!   positionals.
//! - An exact option string (`--project`, `-P`) is that option. An exact match wins over a
//!   prefix.
//! - `--opt=value` and `-x=value`: the text before the first `=` names the option, and the
//!   rest is its value, which may be empty or start with `-`.
//! - `-xvalue`: a single-dash option followed by more text takes that text as its value.
//!   For a flag, the text is more single-letter flags (`-ab` is `-a -b`; a letter that isn't
//!   an option is unrecognized), but `-a=x`, or text starting with `-`, is an error.
//! - A `--` option may be abbreviated to any prefix that matches only one option string:
//!   `--desc` for `--description`, `--desc=x` too. A prefix that matches more than one is an
//!   error: `ambiguous option: --h could match --help, --health-every`.
//! - Anything else starting with `-` is a positional if it looks like a negative number
//!   (`-5`, `-1.5`, `-.5`) or contains a space, and otherwise an unrecognized option.
//!
//! # Consuming them
//!
//! Options and positionals may come in any order. Parsing alternates: the arguments before
//! the next option go to the positionals not yet filled, in order, as many positionals as
//! can take them; then the option takes its values; and so on. A `?` or `*` positional
//! reached in such a run is done, even if it got nothing: `am memory add foo -f FILE bar`
//! leaves `bar` unrecognized. Positionals left at the end take the remaining arguments,
//! and arguments no positional can take are unrecognized.
//!
//! An option's values are the arguments right after it that are neither options nor `--`,
//! so a value that looks like an option has to be attached: `--opt=-x`. Given more than
//! once, an option's last value wins.
//!
//! # Nargs
//!
//! How many values an argument takes, and what lands in the namespace:
//!
//! | [`Nargs`] | in usage | takes | value |
//! |---|---|---|---|
//! | `One` | `X` | exactly one | the value |
//! | `Optional` | `[X]` | zero or one | the value, or the default |
//! | `ZeroOrMore` | `[X ...]` | any number | a list; with none, a positional's default if it has one |
//! | `OneOrMore` | `X [X ...]` | at least one | a list |
//! | `Zero` | | none | flags: `true` (`false` when absent); help: shows it |
//! | `Parser` | `X ...` | a subcommand and every argument after it | the subcommand's name |
//!
//! A positional is required unless it's `?`, or `*` with a default; an option only when
//! marked `.required()`. A `*` positional counts as given once parsing reaches it,
//! with or without values, so it's only missing when an earlier positional is too
//! (`required: name, text`). Arguments not given get their defaults (`V::None` unless set).
//!
//! An integer argument (`.int()`) takes a decimal integer: an optional sign,
//! surrounding whitespace, and single `_`s between digits allowed. An argument with
//! choices must be one of them; a subcommand's name must be one of the subcommands.
//!
//! # Subcommands
//!
//! A subcommand takes everything after its name, options included: `am send -h` is
//! `send`'s help, not `am`'s. Its parser parses those arguments the same way, and its
//! values join the namespace. The arguments it doesn't recognize are passed up and
//! reported by the top parser.
//!
//! # Help and errors
//!
//! `-h`/`--help` prints the parser's help page (`help.rs`) on stdout and exits 0, as soon as
//! parsing reaches it: an earlier error still wins, but missing required arguments don't
//! matter.
//!
//! A usage error prints the parser's usage, then `PROG: error: MESSAGE`, on stderr, and
//! exits 2. PROG is the command path of the parser that found the error (`am send: error:
//! ...`), except that unrecognized arguments always get `am`'s usage and `am: error: ...`.
//! The messages, with values quoted by `am_core::format::quote`:
//!
//! - `ambiguous option: --w could match --worker-backend, --worker-model`
//! - `unrecognized arguments: --bogus extra`
//! - `the following arguments are required: agent, text`
//! - `argument --description: expected one argument` (or `expected at most one argument`,
//!   `expected at least one argument`)
//! - `argument --all: ignored explicit argument 'x'`, for a value given to a flag
//! - `argument id: invalid int value: 'x'`
//! - `argument --backend: invalid choice: 'x' (choose from 'claude', 'codex')`
//!
//! An argument is named by its option strings joined with `/` (`--project/-P`), else its
//! metavar (`COMMAND`), else its dest. Errors found while classifying (ambiguous options)
//! come first, then the others in argument order, then missing arguments, then
//! unrecognized ones.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nargs {
    /// Exactly one.
    One,
    /// `?`
    Optional,
    /// `*`
    ZeroOrMore,
    /// `+`
    OneOrMore,
    /// A subparsers action: a command and everything after it.
    Parser,
    /// None: flags and help.
    Zero,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Store,
    StoreTrue,
    Help,
    Subparsers,
}

/// A parsed value, as it lands in the namespace.
#[derive(Clone, Debug, PartialEq)]
pub enum V {
    None,
    Bool(bool),
    Str(String),
    Int(i64),
    List(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct Action {
    pub option_strings: Vec<String>,
    pub dest: String,
    pub nargs: Nargs,
    pub kind: Kind,
    pub required: bool,
    pub choices: Option<Vec<String>>,
    pub int: bool,
    pub metavar: Option<String>,
    pub help: Option<String>,
    pub default: V,
    /// Subparsers: (name, parser), and the help of those added with `help=`.
    pub parsers: Vec<(String, Parser)>,
    pub choice_help: Vec<(String, String)>,
}

impl Action {
    fn new(kind: Kind, dest: &str) -> Self {
        Action {
            option_strings: vec![],
            dest: dest.into(),
            nargs: Nargs::One,
            kind,
            required: false,
            choices: None,
            int: false,
            metavar: None,
            help: None,
            default: V::None,
            parsers: vec![],
            choice_help: vec![],
        }
    }

    pub fn help(mut self, h: &str) -> Self {
        self.help = Some(h.into());
        self
    }
    pub fn nargs(mut self, n: Nargs) -> Self {
        self.nargs = n;
        if !self.is_optional() {
            self.required = !matches!(n, Nargs::Optional | Nargs::ZeroOrMore) || n == Nargs::ZeroOrMore;
        }
        self
    }
    pub fn choices(mut self, c: &[&str]) -> Self {
        self.choices = Some(c.iter().map(|s| s.to_string()).collect());
        self
    }
    pub fn int(mut self) -> Self {
        self.int = true;
        self
    }
    pub fn metavar(mut self, m: &str) -> Self {
        self.metavar = Some(m.into());
        self
    }
    pub fn default(mut self, d: V) -> Self {
        self.default = d;
        if !self.is_optional() && self.nargs == Nargs::ZeroOrMore {
            self.required = false;
        }
        self
    }
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
    pub fn dest(mut self, d: &str) -> Self {
        self.dest = d.into();
        self
    }
    fn is_optional(&self) -> bool {
        !self.option_strings.is_empty()
    }
}

/// A positional (required unless `?`, or `*` with a default).
pub fn pos(dest: &str) -> Action {
    let mut a = Action::new(Kind::Store, dest);
    a.required = true;
    a
}

/// An option taking one value; its dest is the first long option string without the
/// dashes, `-` as `_` (`--reply-to` -> `reply_to`).
pub fn opt(strings: &[&str]) -> Action {
    let long = strings.iter().find(|s| s.starts_with("--")).unwrap_or(&strings[0]);
    let mut a = Action::new(Kind::Store, &long.trim_start_matches('-').replace('-', "_"));
    a.option_strings = strings.iter().map(|s| s.to_string()).collect();
    a
}

/// A flag: `true` if given, else `false`.
pub fn flag(strings: &[&str]) -> Action {
    let mut a = opt(strings);
    a.kind = Kind::StoreTrue;
    a.nargs = Nargs::Zero;
    a.default = V::Bool(false);
    a
}

#[derive(Clone, Debug)]
pub struct Parser {
    pub prog: String,
    pub description: Option<String>,
    pub actions: Vec<Action>,
}

impl Parser {
    /// A parser for `prog`, with `-h/--help`.
    pub fn new(prog: &str, description: Option<&str>) -> Self {
        let mut help = Action::new(Kind::Help, "help");
        help.option_strings = vec!["-h".into(), "--help".into()];
        help.nargs = Nargs::Zero;
        help.help = Some("show this help message and exit".into());
        Parser { prog: prog.into(), description: description.map(String::from), actions: vec![help] }
    }

    pub fn arg(mut self, a: Action) -> Self {
        self.actions.push(a);
        self
    }

    /// The shared `--project/-P` option.
    pub fn project_opt(self) -> Self {
        self.arg(opt(&["--project", "-P"]).help("the project; defaults to the current one"))
    }

    /// Required subcommands, stored in `dest` and shown as `metavar` if given, each
    /// `(name, help, parser)`. Each parser's prog is this one's plus the name.
    pub fn subparsers(mut self, dest: &str, metavar: Option<&str>, parsers: Vec<(&str, Option<&str>, Parser)>) -> Self {
        let mut a = Action::new(Kind::Subparsers, dest);
        a.nargs = Nargs::Parser;
        a.required = true;
        a.metavar = metavar.map(String::from);
        a.choices = Some(parsers.iter().map(|(n, _, _)| n.to_string()).collect());
        for (name, help, mut parser) in parsers {
            parser.prog = format!("{} {}", self.prog, name);
            fix_progs(&mut parser);
            if let Some(h) = help {
                a.choice_help.push((name.into(), h.into()));
            }
            a.parsers.push((name.into(), parser));
        }
        self.actions.push(a);
        self
    }

    fn option_index(&self) -> HashMap<&str, usize> {
        let mut map = HashMap::new();
        for (i, a) in self.actions.iter().enumerate() {
            for s in &a.option_strings {
                map.insert(s.as_str(), i);
            }
        }
        map
    }
}

/// A nested parser's sub-parsers take their prog from it.
fn fix_progs(parser: &mut Parser) {
    let prog = parser.prog.clone();
    for a in parser.actions.iter_mut() {
        for (name, p) in a.parsers.iter_mut() {
            p.prog = format!("{prog} {name}");
            fix_progs(p);
        }
    }
}

/// The parsed arguments.
#[derive(Debug, Default, Clone)]
pub struct Parsed(pub HashMap<String, V>);

impl Parsed {
    pub fn get(&self, k: &str) -> &V {
        self.0.get(k).unwrap_or(&V::None)
    }
    pub fn str(&self, k: &str) -> Option<&str> {
        match self.get(k) {
            V::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn flag(&self, k: &str) -> bool {
        matches!(self.get(k), V::Bool(true))
    }
    pub fn int(&self, k: &str) -> Option<i64> {
        match self.get(k) {
            V::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn list(&self, k: &str) -> Option<&[String]> {
        match self.get(k) {
            V::List(l) => Some(l),
            _ => None,
        }
    }
}

/// How parsing ended without a namespace: help on stdout (status 0), or a usage error on
/// stderr (status 2).
#[derive(Debug)]
pub struct Exit {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// `(action index or None for an unknown option, option string, explicit argument)`;
/// `sep` is whether that argument came after `=` (`Some(true)`), attached (`Some(false)`).
#[derive(Clone, Debug)]
struct OptTuple {
    action: Option<usize>,
    option_string: String,
    sep: Option<bool>,
    explicit: Option<String>,
}

fn looks_negative(s: &str) -> bool {
    let rest = &s[1..];
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit());
    digits(rest) || rest.split_once('.').is_some_and(|(a, b)| a.bytes().all(|c| c.is_ascii_digit()) && digits(b))
}

impl Parser {
    /// Classifies one argument: `Ok(None)` for a positional.
    fn parse_optional(&self, arg: &str) -> Result<Option<OptTuple>, Exit> {
        if arg.is_empty() || !arg.starts_with('-') {
            return Ok(None);
        }
        let index = self.option_index();
        if let Some(&i) = index.get(arg) {
            return Ok(Some(OptTuple { action: Some(i), option_string: arg.into(), sep: None, explicit: None }));
        }
        if arg.chars().count() == 1 {
            return Ok(None);
        }
        if let Some((head, tail)) = arg.split_once('=') {
            if let Some(&i) = index.get(head) {
                return Ok(Some(OptTuple { action: Some(i), option_string: head.into(), sep: Some(true), explicit: Some(tail.into()) }));
            }
        }
        let tuples = self.option_tuples(arg);
        if tuples.len() > 1 {
            let options: Vec<&str> = tuples.iter().map(|t| t.option_string.as_str()).collect();
            return Err(self.error(&format!("ambiguous option: {arg} could match {}", options.join(", "))));
        }
        if let Some(t) = tuples.into_iter().next() {
            return Ok(Some(t));
        }
        if looks_negative(arg) || arg.contains(' ') {
            return Ok(None);
        }
        Ok(Some(OptTuple { action: None, option_string: arg.into(), sep: None, explicit: None }))
    }

    /// What an abbreviated or attached option could be.
    fn option_tuples(&self, arg: &str) -> Vec<OptTuple> {
        let mut out = Vec::new();
        let chars: Vec<char> = arg.chars().collect();
        let strings: Vec<(usize, &String)> =
            self.actions.iter().enumerate().flat_map(|(i, a)| a.option_strings.iter().map(move |s| (i, s))).collect();
        if chars.len() > 1 && chars[1] == '-' {
            let (prefix, explicit) = match arg.split_once('=') {
                Some((p, e)) => (p, Some(e.to_string())),
                None => (arg, None),
            };
            for (i, s) in &strings {
                if s.starts_with(prefix) {
                    out.push(OptTuple { action: Some(*i), option_string: (*s).clone(), sep: explicit.as_ref().map(|_| true), explicit: explicit.clone() });
                }
            }
        } else {
            let short: String = chars[..2].iter().collect();
            let rest: String = chars[2..].iter().collect();
            for (i, s) in &strings {
                if **s == short {
                    out.push(OptTuple { action: Some(*i), option_string: (*s).clone(), sep: Some(false), explicit: Some(rest.clone()) });
                } else if s.starts_with(arg) {
                    out.push(OptTuple { action: Some(*i), option_string: (*s).clone(), sep: None, explicit: None });
                }
            }
        }
        out
    }

    /// The namespace, or how to exit; arguments left over are an error.
    pub fn parse_args(&self, args: &[String]) -> Result<Parsed, Exit> {
        let mut ns = Parsed::default();
        let extras = self.parse_known(args, &mut ns)?;
        if !extras.is_empty() {
            return Err(self.error(&format!("unrecognized arguments: {}", extras.join(" "))));
        }
        Ok(ns)
    }

    /// Parses into `ns`, returning the arguments it didn't recognize.
    pub fn parse_known(&self, args: &[String], ns: &mut Parsed) -> Result<Vec<String>, Exit> {
        for a in &self.actions {
            if a.kind != Kind::Help && !ns.0.contains_key(&a.dest) {
                ns.0.insert(a.dest.clone(), a.default.clone());
            }
        }
        // The pattern: O for an option, A for an argument, - for `--` (and A after it).
        let mut options: HashMap<usize, OptTuple> = HashMap::new();
        let mut pattern = String::new();
        let mut after_dashes = false;
        for (i, arg) in args.iter().enumerate() {
            if after_dashes {
                pattern.push('A');
            } else if arg == "--" {
                pattern.push('-');
                after_dashes = true;
            } else if let Some(t) = self.parse_optional(arg)? {
                options.insert(i, t);
                pattern.push('O');
            } else {
                pattern.push('A');
            }
        }
        let mut seen = vec![false; self.actions.len()];
        let mut extras: Vec<String> = Vec::new();
        let mut positionals: Vec<usize> =
            (0..self.actions.len()).filter(|&i| self.actions[i].option_strings.is_empty()).collect();
        let max_option = options.keys().copied().max();
        let mut start = 0usize;
        if let Some(max_option) = max_option {
            while start <= max_option {
                let next = *options.keys().filter(|&&k| k >= start).min().unwrap();
                if start != next {
                    let end = self.consume_positionals(args, &pattern, start, &mut positionals, &mut seen, ns, &mut extras)?;
                    if end > start {
                        start = end;
                        continue;
                    }
                    start = end;
                }
                if !options.contains_key(&start) {
                    extras.extend(args[start..next].iter().cloned());
                    start = next;
                }
                start = self.consume_optional(args, &pattern, start, &options, &mut seen, ns, &mut extras)?;
            }
        }
        let stop = self.consume_positionals(args, &pattern, start, &mut positionals, &mut seen, ns, &mut extras)?;
        extras.extend(args[stop..].iter().cloned());
        if let Some(V::List(unknown)) = ns.0.remove("\0unrecognized") {
            extras.extend(unknown);
        }
        let missing: Vec<String> =
            self.actions.iter().enumerate().filter(|(i, a)| a.required && !seen[*i]).map(|(_, a)| action_name(a)).collect();
        if !missing.is_empty() {
            return Err(self.error(&format!("the following arguments are required: {}", missing.join(", "))));
        }
        Ok(extras)
    }
}

/// How errors name an argument: its option strings joined with `/`, else its metavar,
/// else its dest.
pub fn action_name(a: &Action) -> String {
    if !a.option_strings.is_empty() {
        a.option_strings.join("/")
    } else if let Some(m) = &a.metavar {
        m.clone()
    } else {
        a.dest.clone()
    }
}

/// The regex for an argument's nargs over the pattern of `O`, `A` and `-`; a positional
/// may also take the `--` around its values.
fn nargs_pattern(a: &Action) -> &'static str {
    let optional = !a.option_strings.is_empty();
    match (a.nargs, optional) {
        (Nargs::One, false) => "(-*A-*)",
        (Nargs::One, true) => "(A)",
        (Nargs::Optional, false) => "(-*A?-*)",
        (Nargs::Optional, true) => "(A?)",
        (Nargs::ZeroOrMore, false) => "(-*[A-]*)",
        (Nargs::ZeroOrMore, true) => "(A*)",
        (Nargs::OneOrMore, false) => "(-*A[A-]*)",
        (Nargs::OneOrMore, true) => "(AA*)",
        (Nargs::Parser, _) => "(-*A[-AO]*)",
        (Nargs::Zero, false) => "(-*-*)",
        (Nargs::Zero, true) => "()",
    }
}

/// Matches `pattern` at the start of `text`: the length of each group.
fn match_groups(pattern: &str, text: &str) -> Option<Vec<usize>> {
    let re = regex::Regex::new(&format!("^{pattern}")).ok()?;
    let caps = re.captures(text)?;
    Some((1..caps.len()).map(|i| caps.get(i).map_or(0, |m| m.len())).collect())
}

impl Parser {
    /// A usage error: usage and `PROG: error: MESSAGE` on stderr, status 2.
    pub fn error(&self, message: &str) -> Exit {
        Exit { code: 2, stdout: String::new(), stderr: format!("{}{}: error: {message}\n", self.format_usage(), self.prog) }
    }

    fn arg_error(&self, a: &Action, message: &str) -> Exit {
        self.error(&format!("argument {}: {message}", action_name(a)))
    }

    /// How many of the strings an option takes, or the error if they don't fit its nargs.
    fn match_argument(&self, a: &Action, pattern: &str) -> Result<usize, Exit> {
        match match_groups(nargs_pattern(a), pattern) {
            Some(groups) => Ok(groups[0]),
            None => Err(self.arg_error(
                a,
                match a.nargs {
                    Nargs::One => "expected one argument",
                    Nargs::Optional => "expected at most one argument",
                    Nargs::OneOrMore => "expected at least one argument",
                    _ => "expected 0 arguments",
                },
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn consume_positionals(
        &self,
        args: &[String],
        pattern: &str,
        mut start: usize,
        positionals: &mut Vec<usize>,
        seen: &mut [bool],
        ns: &mut Parsed,
        extras: &mut Vec<String>,
    ) -> Result<usize, Exit> {
        let selected = &pattern[start..];
        let mut counts = Vec::new();
        for i in (1..=positionals.len()).rev() {
            let joined: String = positionals[..i].iter().map(|&p| nargs_pattern(&self.actions[p])).collect();
            if let Some(groups) = match_groups(&joined, selected) {
                counts = groups;
                break;
            }
        }
        for (k, count) in counts.iter().enumerate() {
            let strings = args[start..start + count].to_vec();
            start += count;
            self.take_action(positionals[k], strings, seen, ns, extras)?;
        }
        positionals.drain(..counts.len());
        Ok(start)
    }

    #[allow(clippy::too_many_arguments)]
    fn consume_optional(
        &self,
        args: &[String],
        pattern: &str,
        start: usize,
        options: &HashMap<usize, OptTuple>,
        seen: &mut [bool],
        ns: &mut Parsed,
        extras: &mut Vec<String>,
    ) -> Result<usize, Exit> {
        let t = options[&start].clone();
        let (mut action, mut option_string, mut sep, mut explicit) = (t.action, t.option_string, t.sep, t.explicit);
        let mut tuples: Vec<(usize, Vec<String>)> = Vec::new();
        let stop;
        loop {
            let Some(a) = action else {
                extras.push(args[start].clone());
                return Ok(start + 1);
            };
            let act = &self.actions[a];
            if let Some(exp) = explicit.clone() {
                let count = self.match_argument(act, "A")?;
                let second_is_dash = option_string.chars().nth(1) == Some('-');
                if count == 0 && !second_is_dash && !exp.is_empty() {
                    if sep == Some(true) || exp.starts_with('-') {
                        return Err(self.arg_error(act, &format!("ignored explicit argument {}", am_core::format::quote(&exp))));
                    }
                    tuples.push((a, vec![]));
                    let first = exp.chars().next().unwrap();
                    option_string = format!("-{first}");
                    let rest: String = exp.chars().skip(1).collect();
                    if let Some(&next) = self.option_index().get(option_string.as_str()) {
                        action = Some(next);
                        if rest.is_empty() {
                            sep = None;
                            explicit = None;
                        } else if let Some(r) = rest.strip_prefix('=') {
                            sep = Some(true);
                            explicit = Some(r.to_string());
                        } else {
                            sep = Some(false);
                            explicit = Some(rest);
                        }
                    } else {
                        extras.push(format!("-{exp}"));
                        stop = start + 1;
                        break;
                    }
                } else if count == 1 {
                    stop = start + 1;
                    tuples.push((a, vec![exp]));
                    break;
                } else {
                    return Err(self.arg_error(act, &format!("ignored explicit argument {}", am_core::format::quote(&exp))));
                }
            } else {
                let from = start + 1;
                let count = self.match_argument(act, &pattern[from..])?;
                stop = from + count;
                tuples.push((a, args[from..stop].to_vec()));
                break;
            }
        }
        for (a, strings) in tuples {
            self.take_action(a, strings, seen, ns, extras)?;
        }
        Ok(stop)
    }

    /// Converts one string (to an integer for an int argument) and checks its choices.
    fn convert(&self, a: &Action, s: &str) -> Result<V, Exit> {
        let v = if a.int {
            match am_core::format::parse_int(s) {
                Some(i) => V::Int(i),
                None => return Err(self.arg_error(a, &format!("invalid int value: {}", am_core::format::quote(s)))),
            }
        } else {
            V::Str(s.to_string())
        };
        self.check(a, s)?;
        Ok(v)
    }

    fn check(&self, a: &Action, s: &str) -> Result<(), Exit> {
        if let Some(choices) = &a.choices {
            if !choices.iter().any(|c| c == s) {
                let listed: Vec<String> = choices.iter().map(|c| am_core::format::quote(c)).collect();
                return Err(self.arg_error(
                    a,
                    &format!("invalid choice: {} (choose from {})", am_core::format::quote(s), listed.join(", ")),
                ));
            }
        }
        Ok(())
    }

    fn take_action(
        &self,
        index: usize,
        mut strings: Vec<String>,
        seen: &mut [bool],
        ns: &mut Parsed,
        extras: &mut Vec<String>,
    ) -> Result<(), Exit> {
        seen[index] = true;
        let a = &self.actions[index];
        if a.option_strings.is_empty() && a.nargs != Nargs::Parser {
            if let Some(i) = strings.iter().position(|s| s == "--") {
                strings.remove(i);
            }
        }
        match a.kind {
            Kind::Help => Err(Exit { code: 0, stdout: self.format_help(), stderr: String::new() }),
            Kind::StoreTrue => {
                ns.0.insert(a.dest.clone(), V::Bool(true));
                Ok(())
            }
            Kind::Subparsers => {
                let name = strings[0].clone();
                self.check(a, &name)?;
                ns.0.insert(a.dest.clone(), V::Str(name.clone()));
                let parser = &a.parsers.iter().find(|(n, _)| *n == name).unwrap().1;
                let mut sub = Parsed::default();
                let unknown = parser.parse_known(&strings[1..], &mut sub)?;
                ns.0.extend(sub.0);
                ns.0.entry("\0unrecognized".into()).or_insert(V::List(vec![]));
                if let Some(V::List(l)) = ns.0.get_mut("\0unrecognized") {
                    l.extend(unknown);
                }
                let _ = extras;
                Ok(())
            }
            Kind::Store => {
                let value = match a.nargs {
                    Nargs::Optional if strings.is_empty() => a.default.clone(),
                    Nargs::ZeroOrMore if strings.is_empty() && a.option_strings.is_empty() => match &a.default {
                        V::None => V::List(vec![]),
                        d => d.clone(),
                    },
                    Nargs::One | Nargs::Optional if strings.len() == 1 => self.convert(a, &strings[0])?,
                    _ => {
                        for s in &strings {
                            self.convert(a, s)?;
                        }
                        V::List(strings)
                    }
                };
                ns.0.insert(a.dest.clone(), value);
                Ok(())
            }
        }
    }
}
