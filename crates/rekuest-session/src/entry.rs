//! One recorded fact of the agent's journal, as the outbox sends and re-sends it.
//!
//! The journal (numbering, persistence, the fold) lives in `rekuest`; the
//! entry is here because the session's [`Outbox`](crate::Outbox) retains and
//! reloads frames by it.

use serde::Serialize;
use serde_json::Value;

use crate::prelude::*;
use crate::protocol::messages::{Envelope, FromAgent};

/// One recorded fact.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JournalEntry {
    pub session_id: String,
    pub pos: u64,
    /// The state revision after this entry.
    pub global_rev: u64,
    pub timepoint: String,
    #[serde(skip)]
    pub event_time: i64,
    /// The frame `type`: `PROGRESS`, `EFFECT`, `STATE_PATCH`, …
    pub kind: String,
    /// The task this entry belongs to (for `LOCK`/`UNLOCK`: the holder).
    pub task_id: Option<String>,
    /// 1, 2, 3, … over the entries of `task_id`.
    pub step: Option<u64>,
    pub action_key: Option<String>,
    /// The state (`STATE_PATCH`) or lock key (`LOCK`/`UNLOCK`) it is about.
    pub subject: Option<String>,
    /// The frame's `id`, as sent.
    pub message_id: String,
    /// The frame, without the stream-level `seq`.
    pub payload: Value,
}

/// Who an entry is for, as subscribers filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route<'a> {
    State(&'a str),
    Lock(&'a str),
    Action(&'a str),
    /// Session-wide (`SESSION_INIT`, `STATE_SNAPSHOT`, reports about unknown tasks).
    Everyone,
}

impl JournalEntry {
    /// The envelope of `message` as this entry journaled it: its id and position come from the entry.
    pub fn envelope(&self, message: FromAgent) -> Envelope {
        Envelope {
            id: self.message_id.clone(),
            seq: None,
            pos: Some(self.pos),
            journal_session: Some(self.session_id.clone()),
            agent_ts: Some(self.event_time as f64 / 1000.0),
            task_step: self.step,
            message,
        }
    }

    /// The payload with `pos` and `journal_session`, as journal subscribers get it.
    pub fn frame(&self) -> Value {
        let mut frame = self.payload.clone();
        stamp(&mut frame, self);
        frame
    }

    pub fn route(&self) -> Route<'_> {
        match (
            self.kind.as_str(),
            self.subject.as_deref(),
            self.action_key.as_deref(),
        ) {
            ("STATE_PATCH", Some(state), _) => Route::State(state),
            ("LOCK" | "UNLOCK", Some(key), _) => Route::Lock(key),
            ("STATE_SNAPSHOT" | "SESSION_INIT", _, _) => Route::Everyone,
            (_, _, Some(key)) => Route::Action(key),
            _ => Route::Everyone,
        }
    }

    pub fn is_terminal(&self) -> bool {
        is_terminal_kind(&self.kind)
    }
}

/// Add the numbering (`pos`, `journal_session`, `agent_ts`, `task_step`)
/// to a frame, as the agent sends it. (Not `session_id`: state frames
/// already have one, and it would clash.)
pub fn stamp(frame: &mut Value, entry: &JournalEntry) {
    if let Value::Object(map) = frame {
        map.insert("pos".into(), Value::from(entry.pos));
        map.insert(
            "journal_session".into(),
            Value::String(entry.session_id.clone()),
        );
        map.insert(
            "agent_ts".into(),
            Value::from(entry.event_time as f64 / 1000.0),
        );
        if let Some(step) = entry.step {
            map.insert("task_step".into(), Value::from(step));
        }
    }
}

pub fn is_terminal_kind(kind: &str) -> bool {
    matches!(
        kind,
        "COMPLETED" | "FAILED" | "CRITICAL" | "CANCELLED" | "INTERRUPTED"
    )
}
