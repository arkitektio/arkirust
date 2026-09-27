//! Where an executor's messages go.

use crate::messages::FromAgent;
use crate::outbox::Outbox;

/// A transport's outgoing side.
///
/// `action_key` is the key of the action (its interface) a task message
/// belongs to, when the task is still managed. The websocket agent ignores it;
/// the served agent routes frames to subscribers by it.
pub trait Emitter: Send + Sync + 'static {
    fn emit(&self, message: FromAgent, action_key: Option<&str>);
}

impl Emitter for Outbox {
    fn emit(&self, message: FromAgent, _action_key: Option<&str>) {
        self.send(message)
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
