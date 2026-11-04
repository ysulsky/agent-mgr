//! Python 3.11's argparse, for the parsers `cli.py` builds: the same parsing algorithm
//! (option prefixes, `--opt=value`, `-Pvalue`, `--`, positionals consumed in chunks
//! between options, subparsers), the same help and usage text, and the same errors.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nargs {
    /// `nargs=None`: exactly one.
    One,
    /// `?`
    Optional,
    /// `*`
    ZeroOrMore,
    /// `+`
    OneOrMore,
    /// A subparsers action: a command and everything after it.
    Parser,
    /// `nargs=0`: flags and help.
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

/// `add_argument(name)`: a positional (required unless `?`, or `*` with a default).
pub fn pos(dest: &str) -> Action {
    let mut a = Action::new(Kind::Store, dest);
    a.required = true;
    a
}

/// `add_argument(*option_strings)`: an option taking one value; dest from the first long
/// option string (`--reply-to` -> `reply_to`).
pub fn opt(strings: &[&str]) -> Action {
    let long = strings.iter().find(|s| s.starts_with("--")).unwrap_or(&strings[0]);
    let mut a = Action::new(Kind::Store, &long.trim_start_matches('-').replace('-', "_"));
    a.option_strings = strings.iter().map(|s| s.to_string()).collect();
    a
}

/// `add_argument(*option_strings, action="store_true")`.
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
    /// `ArgumentParser(prog=..., description=...)`, with `-h/--help`.
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

    /// `parents=[project_opt]`.
    pub fn project_opt(self) -> Self {
        self.arg(opt(&["--project", "-P"]).help("the project; defaults to the current one"))
    }

    /// `add_subparsers(dest=..., required=True, metavar=...)` with its parsers, each
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
pub struct Namespace(pub HashMap<String, V>);

impl Namespace {
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
    /// `_parse_optional`: `Ok(None)` for a positional.
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

    /// `_get_option_tuples`: what an abbreviated or attached option could be.
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

    /// `parse_args`: the namespace, or how to exit.
    pub fn parse_args(&self, args: &[String]) -> Result<Namespace, Exit> {
        let mut ns = Namespace::default();
        let extras = self.parse_known_args(args, &mut ns)?;
        if !extras.is_empty() {
            return Err(self.error(&format!("unrecognized arguments: {}", extras.join(" "))));
        }
        Ok(ns)
    }

    /// `parse_known_args` into `ns`: the arguments it didn't recognize.
    pub fn parse_known_args(&self, args: &[String], ns: &mut Namespace) -> Result<Vec<String>, Exit> {
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

/// `_get_action_name`.
pub fn action_name(a: &Action) -> String {
    if !a.option_strings.is_empty() {
        a.option_strings.join("/")
    } else if let Some(m) = &a.metavar {
        m.clone()
    } else {
        a.dest.clone()
    }
}

/// `_get_nargs_pattern`, anchored at the start.
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

/// `re.match(pattern, text)`: the length of each group.
fn match_groups(pattern: &str, text: &str) -> Option<Vec<usize>> {
    let re = regex::Regex::new(&format!("^{pattern}")).ok()?;
    let caps = re.captures(text)?;
    Some((1..caps.len()).map(|i| caps.get(i).map_or(0, |m| m.len())).collect())
}

impl Parser {
    /// `parser.error(message)`: usage and the message on stderr, status 2.
    pub fn error(&self, message: &str) -> Exit {
        Exit { code: 2, stdout: String::new(), stderr: format!("{}{}: error: {message}\n", self.format_usage(), self.prog) }
    }

    fn arg_error(&self, a: &Action, message: &str) -> Exit {
        self.error(&format!("argument {}: {message}", action_name(a)))
    }

    /// `_match_argument`: how many of the strings an option takes.
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
        ns: &mut Namespace,
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
        ns: &mut Namespace,
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
                        return Err(self.arg_error(act, &format!("ignored explicit argument {}", am_core::py::repr(&exp))));
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
                    return Err(self.arg_error(act, &format!("ignored explicit argument {}", am_core::py::repr(&exp))));
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

    /// `_get_value` and `_check_value` for one string.
    fn convert(&self, a: &Action, s: &str) -> Result<V, Exit> {
        let v = if a.int {
            match am_core::py::parse_int(s) {
                Some(i) => V::Int(i),
                None => return Err(self.arg_error(a, &format!("invalid int value: {}", am_core::py::repr(s)))),
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
                let listed: Vec<String> = choices.iter().map(|c| am_core::py::repr(c)).collect();
                return Err(self.arg_error(
                    a,
                    &format!("invalid choice: {} (choose from {})", am_core::py::repr(s), listed.join(", ")),
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
        ns: &mut Namespace,
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
                let mut sub = Namespace::default();
                let unknown = parser.parse_known_args(&strings[1..], &mut sub)?;
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
