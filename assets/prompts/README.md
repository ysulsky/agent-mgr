# Prompts

Every piece of text Agent Mgr sends to an agent is a template here. The code only picks a template and fills it in, with `am_turns::prompts::render_kv("notes/vanished", &[("turn_id", ...)])`.

- `roles/`: the system prompt. Every agent gets `common.md`, then `home.md` (the home manager only) and `manager.md`, or `worker.md`. `worker-purpose.md` is the line that fills `{purpose}` in `worker.md`. `memory.md`, the shared memory index, comes last when memory has entries. It is in the system prompt, not the turn prompt, so it is cached; changing memory makes every session cache its context again once.
- `turn/`: the pieces of the prompt that starts a turn, in the order they appear: `side-turn.md` for a fork of a busy worker, `notes.md` for the notes below, `history.md` and `history-entry.md` for a new session's digest of recent turns, `recent-question*.md` for a manager's view of the user's last three messages in full, `message*.md` for each message block, and `answer-card*.md` or `no-user-message.md` to close a manager's turn.
- `notes/`: what the runner tells an agent at the start of its next turn, after its last one was interrupted, hit its time limit, failed, vanished or lost its session, after its backend or model changed, or, for a long-lived process, after shared memory changed (`memory-changed.md`).
- `messages/`: messages Agent Mgr itself sends: the health check, the disk and memory alerts to the home manager, a busy worker's side-turn exchange passed back to its main session, and the stand-in for a turn that ended without a reply.

## Syntax

`{name}` is a placeholder, filled with the value of the same name. A placeholder with no value is an error. Nothing else is special: other braces and `$` are plain text, and a value is inserted as it is, never read as a template. A file's final newline isn't part of the text.

Templates are read from disk at runtime (see `am_core::assets`), so editing one needs no rebuild.
