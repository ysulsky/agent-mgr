//! The store calls this crate makes, in one place, so `am-store`'s signatures (option
//! structs, a map of changes) meet the plain arguments this crate passes here and
//! nowhere else.

use am_core::{Error, Result, Value};
use am_store::{items, mail};

pub use am_store::items::AttentionOpts;
pub use am_store::mail::{answer_target, answered_in_turn, claim, describe_trigger, get_message, mark_interrupt, pending, publish_answer, write_note, SendOpts};
pub use am_store::projects::{agent_exists, agent_names, effective_backend, get_agent, get_project, is_archived, list_slugs, update_agent};

/// Move a claimed message back to the inbox, for a retry. With `no_side`, it's marked so
/// a side turn won't take it.
pub fn requeue(slug: &str, name: &str, msg_id: &str, no_side: bool) -> Result<()> {
    let changes = if no_side { serde_json::json!({"no_side": true}) } else { Value::Null };
    mail::requeue(slug, name, msg_id, &changes)
}

pub fn send(slug: &str, name: &str, text: &str, opts: SendOpts) -> Result<Value> {
    mail::send(slug, name, text, opts)
}

pub fn add_attention(slug: &str, title: &str, body: &str, kind: &str, source: &str, key: &str, run: Option<&str>) -> Result<Value> {
    let opts = AttentionOpts {
        body: body.into(),
        kind: Some(kind.into()),
        run: run.map(String::from),
        source: source.into(),
        key: Some(key.into()),
        ..Default::default()
    };
    items::add_attention(slug, title, opts)
}

pub fn resolve_attention_key(slug: &str, key: &str, by: &str) -> Result<()> {
    items::resolve_attention_key(slug, key, by)
}

/// True for an error meant for the person running `am` (`Error::Am`).
pub fn is_am_error(e: &Error) -> bool {
    matches!(e, Error::Am(_))
}
