//! Where an executor's messages go.

use crate::journal::JournalEntry;
use crate::messages::FromAgent;
use crate::outbox::Outbox;

/// A transport's outgoing side.
///
/// `action_key` is the key of the action (its interface) a task message
/// belongs to, when the task is still managed. The websocket agent ignores it;
/// the served agent routes frames to subscribers by it.
pub trait Emitter: Send + Sync + 'static {
    fn emit(&self, message: FromAgent, action_key: Option<&str>);

    /// A report about a task this process never ran (a lifecycle request or
    /// an inquiry for a predecessor's task): the journal numbers it without
    /// a `task_step`. Defaults to [`Emitter::emit`].
    fn emit_unowned(&self, message: FromAgent, action_key: Option<&str>) {
        self.emit(message, action_key)
    }

    /// A message the [`Journal`](crate::journal::Journal) has recorded as
    /// `entry`. Called while the journal is locked, so calls arrive in `pos`
    /// order. Defaults to [`Emitter::emit`].
    fn emit_entry(&self, message: FromAgent, action_key: Option<&str>, entry: &JournalEntry) {
        let _ = entry;
        self.emit(message, action_key)
    }
}

impl Emitter for Outbox {
    fn emit(&self, message: FromAgent, _action_key: Option<&str>) {
        self.send(message)
    }

    fn emit_entry(&self, message: FromAgent, _action_key: Option<&str>, entry: &JournalEntry) {
        self.send_entry(message, entry)
    }
}

/// Drops everything (tests, local calls).
#[derive(Debug, Default, Clone, Copy)]
pub struct NullEmitter;

impl Emitter for NullEmitter {
    fn emit(&self, message: FromAgent, _action_key: Option<&str>) {
        tracing::debug!("{message:?}");
    }
}
