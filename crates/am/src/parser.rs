//! The `am` command tree: every subcommand, option and help string, in the order usage
//! and help list them.

use crate::args::{flag, opt, pos, Nargs, Parser, V};

fn p(help_prog: &str) -> Parser {
    Parser::new(help_prog, None)
}

/// The text of a message or entry: words, `-` for stdin, or `-f FILE`.
fn text_args(parser: Parser) -> Parser {
    parser
        .arg(pos("text").nargs(Nargs::ZeroOrMore).help("the text, or - to read stdin"))
        .arg(opt(&["-f", "--file"]).help("read the text from a file"))
}

fn slug_arg(parser: Parser) -> Parser {
    parser.arg(pos("name").metavar("NAME").help("the project's slug"))
}

pub fn build() -> Parser {
    let project = Parser::new("am project", None).subparsers(
        "action",
        None,
        vec![
            (
                "create",
                Some("create a project; its manager waits for your first message"),
                p("").arg(pos("name").nargs(Nargs::OneOrMore)).arg(opt(&["--description"])),
            ),
            ("list", None, p("")),
            ("show", None, p("").project_opt().arg(pos("slug").nargs(Nargs::Optional))),
            (
                "set",
                Some("change settings; with no options, print them"),
                p("").project_opt()
                    .arg(opt(&["--name"]))
                    .arg(opt(&["--description"]))
                    .arg(opt(&["--status"]).help("the short status line in the sidebar"))
                    .arg(opt(&["--health-every"]).help("how often to check for stuck work: 30m, 2h, or off"))
                    .arg(opt(&["--backend"]).help("default backend for the manager; empty to clear"))
                    .arg(opt(&["--model"]).help("default model for the manager; empty to clear"))
                    .arg(opt(&["--worker-backend"]).help("default backend for workers; empty to clear"))
                    .arg(opt(&["--worker-model"]).help("default model for workers; empty to clear")),
            ),
            (
                "archive",
                Some("pause a project: nothing new runs and messages wait until it's un-archived; nothing is deleted"),
                slug_arg(p("")),
            ),
            (
                "unarchive",
                Some("resume an archived project, and deliver the messages held for it; an offloaded one is restored first"),
                slug_arg(p("")),
            ),
            (
                "offload",
                Some("back up a whole archived project, with its agents' CLI sessions, to the storage remote, and keep only a stub here"),
                slug_arg(p("")),
            ),
            ("restore", Some("bring an offloaded project back exactly; it stays archived"), slug_arg(p(""))),
        ],
    );
    let agent = Parser::new("am agent", None).subparsers(
        "action",
        None,
        vec![
            (
                "create",
                Some("create a worker"),
                p("").project_opt()
                    .arg(pos("name"))
                    .arg(opt(&["--backend"]).choices(&["claude", "codex"]))
                    .arg(opt(&["--model"]))
                    .arg(opt(&["--purpose"]).help("what this worker is for")),
            ),
            ("list", None, p("").project_opt().arg(flag(&["--all"]).help("include retired agents"))),
            (
                "set",
                Some("change an agent's backend or model; empty returns to the default"),
                p("").project_opt().arg(pos("name")).arg(opt(&["--backend"])).arg(opt(&["--model"])),
            ),
            ("retire", None, p("").project_opt().arg(pos("name"))),
            ("show", None, p("").project_opt().arg(pos("name"))),
        ],
    );
    let task = Parser::new("am task", None).subparsers(
        "action",
        None,
        vec![
            (
                "add",
                None,
                p("").project_opt()
                    .arg(pos("title").nargs(Nargs::OneOrMore))
                    .arg(opt(&["--agent"]))
                    .arg(opt(&["--notes"]))
                    .arg(opt(&["--status"]).default(V::Str("todo".into()))),
            ),
            ("list", None, p("").project_opt().arg(flag(&["--all"]).help("include finished tasks"))),
            (
                "set",
                None,
                p("").project_opt()
                    .arg(pos("id").int())
                    .arg(opt(&["--status"]))
                    .arg(opt(&["--agent"]))
                    .arg(opt(&["--notes"]))
                    .arg(opt(&["--title"]).nargs(Nargs::OneOrMore)),
            ),
            ("done", None, p("").project_opt().arg(pos("id").int())),
        ],
    );
    let attention = Parser::new("am attention", None).subparsers(
        "action",
        None,
        vec![
            (
                "add",
                None,
                p("").project_opt()
                    .arg(pos("title").nargs(Nargs::OneOrMore))
                    .arg(opt(&["--body"]).help("the details, or - for stdin"))
                    .arg(opt(&["--kind"]).default(V::Str("action".into())).choices(&["action", "alert", "result"]))
                    .arg(opt(&["--run"]).help("a command for the Run button"))
                    .arg(opt(&["--ref"]).help("a related artifact or path")),
            ),
            ("resolve", None, p("").project_opt().arg(pos("id"))),
            ("list", None, p("").project_opt().arg(flag(&["--all"]).help("include resolved items"))),
        ],
    );
    let schedule = Parser::new("am schedule", None).subparsers(
        "action",
        None,
        vec![
            (
                "set",
                Some("create or change a schedule"),
                p("").project_opt()
                    .arg(pos("name"))
                    .arg(opt(&["--every"]).help("an interval: 30m, 2h, 1d"))
                    .arg(opt(&["--cron"]).help("five-field cron, in local time: '0 9 * * 1-5'"))
                    .arg(opt(&["--to"]).help("the agent to deliver to; defaults to the manager"))
                    .arg(opt(&["--prompt"]).help("what to tell it"))
                    .arg(opt(&["-f", "--file"]).help("read the prompt from a file"))
                    .arg(flag(&["--on"]))
                    .arg(flag(&["--off"])),
            ),
            ("list", None, p("").project_opt()),
            ("rm", None, p("").project_opt().arg(pos("name"))),
            ("run", Some("deliver it now"), p("").project_opt().arg(pos("name"))),
        ],
    );
    let artifact = Parser::new("am artifact", None).subparsers(
        "action",
        None,
        vec![
            (
                "publish",
                Some("copy FILE into artifacts/ and commit it, or commit an edit to artifacts/NAME"),
                p("").project_opt()
                    .arg(pos("file"))
                    .arg(opt(&["-m", "--message"]).required().help("what changed"))
                    .arg(opt(&["--as"]).dest("as_name").help("publish under this name")),
            ),
            (
                "link",
                Some("publish an external page"),
                p("").project_opt()
                    .arg(pos("name"))
                    .arg(pos("url"))
                    .arg(opt(&["-m", "--message"]).required().help("what changed"))
                    .arg(opt(&["--title"]))
                    .arg(opt(&["--description"])),
            ),
            ("log", Some("list versions"), p("").project_opt().arg(pos("name").nargs(Nargs::Optional))),
            ("show", Some("print a version: NAME@REV"), p("").project_opt().arg(pos("spec"))),
            ("list", None, p("").project_opt()),
        ],
    );
    let memory = Parser::new("am memory", None).subparsers(
        "action",
        None,
        vec![
            ("add", None, text_args(p("").arg(pos("name")))),
            ("edit", Some("replace an entry; with no text, opens $EDITOR"), text_args(p("").arg(pos("name")))),
            ("rm", None, p("").arg(pos("name"))),
            ("list", None, p("")),
            ("show", Some("print an entry; NAME@REV for an old version"), p("").arg(pos("name"))),
            ("log", None, p("").arg(pos("name").nargs(Nargs::Optional))),
        ],
    );
    let storage = Parser::new("am storage", None).subparsers(
        "action",
        None,
        vec![
            (
                "offload",
                Some("copy to the remote, verify, delete locally, and leave a stub"),
                p("").arg(pos("path"))
                    .arg(opt(&["--note"]).help("what this data is"))
                    .arg(flag(&["--keep-local"]).help("copy without deleting the local data")),
            ),
            ("restore", Some("bring offloaded data back"), p("").arg(pos("path"))),
            ("list", None, p("")),
        ],
    );
    Parser::new("am", Some("Agent Mgr: a hierarchy of Claude Code and Codex agents.")).subparsers(
        "command",
        Some("COMMAND"),
        vec![
            ("setup", Some("create ~/agents with config.toml and the home project; safe to re-run"), p("")),
            (
                "daemon",
                Some("the web UI, runner, scheduler and health checks"),
                p("").arg(pos("action").choices(&["start", "stop", "restart", "status", "run"])),
            ),
            ("url", Some("print the web UI address, including its access token"), p("")),
            (
                "health",
                Some("the state of logins, the machine and each project's health check"),
                p("").arg(flag(&["--check"]).help("run the login and machine checks first, and record what they find"))
                    .arg(flag(&["--json"]).help("print what the Health page gets")),
            ),
            (
                "send",
                Some("send a message to an agent"),
                text_args(
                    p("").project_opt()
                        .arg(flag(&["--interrupt"]).help("stop its main turn and deliver this first"))
                        .arg(opt(&["--reply-to"]).help("the id of the item this replies to"))
                        .arg(pos("agent").help("a worker, project/agent, or a project (its manager)")),
                ),
            ),
            ("stop", Some("stop an agent's main turn"), p("").project_opt().arg(pos("agent"))),
            (
                "answer",
                Some("publish a new version of the answer to a question"),
                text_args(p("").project_opt().arg(pos("id").help("the question's id, m-..."))),
            ),
            (
                "questions",
                Some("list the user's questions and latest answers"),
                p("").project_opt().arg(opt(&["--agent"])).arg(opt(&["--limit"]).int().default(V::Int(20))),
            ),
            (
                "show",
                Some("print an item in full: m-..., m-...@v2, t-..., a-..."),
                p("").project_opt().arg(pos("id")),
            ),
            ("project", Some("create and configure projects"), project),
            ("agent", Some("create and configure agents"), agent),
            ("task", Some("the project's task list"), task),
            ("attention", Some("what needs the user"), attention),
            ("schedule", Some("timed prompts to a manager or worker"), schedule),
            ("artifact", Some("publish results for people to read"), artifact),
            ("memory", Some("shared memory, across all projects and agents"), memory),
            (
                "fix-costs",
                Some("recompute recorded costs from each turn's own output; safe to re-run"),
                p("").arg(flag(&["--dry-run"]).help("print old and new totals without changing anything")),
            ),
            ("storage", Some("move old results to shared storage"), storage),
        ],
    )
}
