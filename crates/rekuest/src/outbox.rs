//! Outgoing agent events: numbering, and retention until the server
//! acknowledges them.
//!
//! Tasks keep running across reconnects, so they report into the outbox
//! rather than into a specific socket. Nothing is sent between `REGISTER`
//! and `INIT`; after `INIT` the outbox re-sends what was not acknowledged.
//!
//! * A server that keeps a journal (`INIT` with `journal: true`) gets every
//!   journaled message, in `(session, pos)` order, until a cumulative
//!   `JOURNAL_ACK` covers it. It drops duplicates by position. Messages of
//!   earlier sessions (read back from the local journal after a restart)
//!   go first. Journal-only kinds (`ASSIGN`, effects) go only to such a server.
//! * Any other server gets today's behaviour: terminal events (`COMPLETED`,
//!   `FAILED`, ...) are kept until `EVENT_ACK` and re-sent after `INIT`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::journal::JournalEntry;
use crate::messages::{Envelope, FromAgent};

/// Journaled messages kept for resending at most. If the server's acks stall
/// (it can never fill a gap), the oldest are dropped rather than growing forever.
pub const RETAIN_LIMIT: usize = 65_536;

#[derive(Default)]
struct State {
    tx: Option<UnboundedSender<Envelope>>,
    seq: u64,
    /// Terminal events not yet acknowledged, by event id.
    unacked: BTreeMap<String, Envelope>,
    /// Whether the server keeps a journal; unknown until its first `INIT`.
    journal: Option<bool>,
    /// Journaled messages not yet covered by a `JOURNAL_ACK`, by `(session order, pos)`.
    retained: BTreeMap<(i64, u64), Envelope>,
    /// The order sessions are re-sent in; earlier sessions come first.
    sessions: HashMap<String, i64>,
    next_session: i64,
}

impl State {
    fn session_order(&mut self, session: &str) -> i64 {
        if let Some(order) = self.sessions.get(session) {
            return *order;
        }
        let order = self.next_session;
        self.next_session += 1;
        self.sessions.insert(session.to_owned(), order);
        order
    }

    fn retain(&mut self, envelope: &Envelope) {
        let (Some(pos), Some(session)) = (envelope.pos, envelope.journal_session.as_deref()) else {
            return;
        };
        if self.journal == Some(false) {
            return;
        }
        let order = self.session_order(session);
        self.retained.insert((order, pos), envelope.clone());
        if self.retained.len() > RETAIN_LIMIT {
            if let Some((oldest, _)) = self.retained.pop_first() {
                tracing::warn!(
                    "the server has not acknowledged the journal for a while; dropping {oldest:?} from the resend buffer"
                );
            }
        }
    }

    fn deliver(&mut self, mut envelope: Envelope) {
        if envelope.message.is_event() {
            self.seq += 1;
            envelope.seq = Some(self.seq);
        }
        if envelope.message.is_terminal() {
            self.unacked.insert(envelope.id.clone(), envelope.clone());
        }
        self.retain(&envelope);
        self.transmit(envelope);
    }

    fn transmit(&self, envelope: Envelope) {
        if envelope.message.is_journal_only() && self.journal != Some(true) {
            return;
        }
        match &self.tx {
            Some(tx) => {
                let _ = tx.send(envelope);
            }
            None => tracing::debug!("disconnected, holding {:?}", envelope.message),
        }
    }
}

#[derive(Default)]
pub struct Outbox {
    state: Mutex<State>,
}

impl Outbox {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a message on the current connection (if any).
    pub fn send(&self, message: FromAgent) {
        self.state.lock().expect("outbox lock").deliver(Envelope::new(message));
    }

    /// Queue a journaled message: it carries the entry's id and position.
    pub fn send_entry(&self, message: FromAgent, entry: &JournalEntry) {
        self.state
            .lock()
            .expect("outbox lock")
            .deliver(Envelope::journaled(message, entry));
    }

    #[cfg_attr(not(feature = "wal"), allow(dead_code))]
    /// Hold entries of earlier sessions (read back from the local journal)
    /// to be re-sent, before anything of the current session.
    pub fn preload(&self, entries: Vec<JournalEntry>) {
        let mut state = self.state.lock().expect("outbox lock");
        let mut order = -(entries.len() as i64) - 1;
        let mut last_session: Option<String> = None;
        for entry in entries {
            if last_session.as_deref() != Some(entry.session_id.as_str()) {
                order += 1;
                state.sessions.insert(entry.session_id.clone(), order);
                last_session = Some(entry.session_id.clone());
            }
            let mut payload = entry.payload.clone();
            if let Value::Object(map) = &mut payload {
                map.remove("id");
            }
            let message: FromAgent = match serde_json::from_value(payload) {
                Ok(message) => message,
                Err(e) => {
                    tracing::warn!("skipping unreadable journal entry {}/{}: {e}", entry.session_id, entry.pos);
                    continue;
                }
            };
            let envelope = Envelope::journaled(message, &entry);
            state.retained.insert((order, entry.pos), envelope);
        }
    }

    /// Bind a fresh connection. Numbering restarts per connection.
    pub fn attach(&self, tx: UnboundedSender<Envelope>) {
        let mut state = self.state.lock().expect("outbox lock");
        state.tx = Some(tx);
        state.seq = 0;
    }

    pub fn detach(&self) {
        self.state.lock().expect("outbox lock").tx = None;
    }

    /// Whether the server acknowledges journal positions (from its `INIT`).
    pub fn set_journal(&self, journal: bool) {
        let mut state = self.state.lock().expect("outbox lock");
        state.journal = Some(journal);
        if !journal {
            state.retained.clear();
        }
    }

    /// Re-send what the server has not acknowledged, in the original order.
    pub fn resend_unacked(&self) {
        let mut state = self.state.lock().expect("outbox lock");
        let retained: Vec<Envelope> = std::mem::take(&mut state.retained).into_values().collect();
        let mut unacked: Vec<Envelope> = std::mem::take(&mut state.unacked)
            .into_values()
            .filter(|e| e.pos.is_none() || !retained.iter().any(|r| r.id == e.id))
            .collect();
        unacked.sort_by_key(|e| (e.pos, e.seq));
        // Terminals the journal does not cover first, then the journal in order.
        for envelope in unacked.into_iter().chain(retained) {
            state.deliver(envelope);
        }
    }

    /// Drop acknowledged reports, matched by event id, else by seq.
    pub fn ack(&self, event: Option<&str>, seq: Option<u64>) {
        let mut state = self.state.lock().expect("outbox lock");
        if let Some(event) = event {
            if state.unacked.remove(event).is_some() {
                return;
            }
        }
        if let Some(seq) = seq {
            state.unacked.retain(|_, e| e.seq != Some(seq));
        }
    }

    /// Everything of `session` up to `pos` is persisted by the server.
    pub fn journal_ack(&self, session: &str, pos: u64) {
        let mut state = self.state.lock().expect("outbox lock");
        if let Some(order) = state.sessions.get(session).copied() {
            state.retained.retain(|(o, p), _| !(*o == order && *p <= pos));
        }
        state
            .unacked
            .retain(|_, e| !(e.journal_session.as_deref() == Some(session) && e.pos.is_some_and(|p| p <= pos)));
    }

    #[cfg(test)]
    pub fn unacked_len(&self) -> usize {
        self.state.lock().expect("outbox lock").unacked.len()
    }

    #[cfg(test)]
    pub fn retained_len(&self) -> usize {
        self.state.lock().expect("outbox lock").retained.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit::Emitter;
    use crate::journal::Journal;
    use std::sync::Arc;

    #[test]
    fn retains_until_ack() {
        let outbox = Outbox::new();
        outbox.set_journal(false);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        outbox.attach(tx);
        outbox.send(FromAgent::Started { task: "t".into() });
        outbox.send(FromAgent::Completed { task: "t".into() });
        let started = rx.try_recv().unwrap();
        let completed = rx.try_recv().unwrap();
        assert_eq!((started.seq, completed.seq), (Some(1), Some(2)));
        assert_eq!(outbox.unacked_len(), 1);

        // Reconnect: the report is re-sent with fresh numbering.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        outbox.attach(tx);
        outbox.resend_unacked();
        let again = rx.try_recv().unwrap();
        assert_eq!(again.id, completed.id);
        assert_eq!(again.seq, Some(1));

        outbox.ack(Some(&again.id), None);
        assert_eq!(outbox.unacked_len(), 0);
    }

    #[test]
    fn journal_mode_resends_everything_in_order() {
        let outbox = Arc::new(Outbox::new());
        let journal = Journal::new(outbox.clone(), None, Some("s".into()));
        outbox.set_journal(true);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        outbox.attach(tx);
        journal.emit(FromAgent::Log { task: "t".into(), message: "a".into(), level: Default::default() }, None);
        journal.emit(FromAgent::Yield { task: "t".into(), returns: Default::default() }, None);
        let first = rx.try_recv().unwrap();
        assert_eq!((first.pos, first.journal_session.as_deref(), first.task_step), (Some(1), Some("s"), Some(1)));
        let json = serde_json::to_value(&first).unwrap();
        assert_eq!((json["pos"].as_u64(), json["journal_session"].as_str()), (Some(1), Some("s")));

        // Disconnected: kept, not dropped.
        outbox.detach();
        journal.emit(FromAgent::Completed { task: "t".into() }, None);
        outbox.journal_ack("s", 1);
        assert_eq!(outbox.retained_len(), 2);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        outbox.attach(tx);
        outbox.resend_unacked();
        let resent: Vec<(Option<u64>, Option<u64>)> = std::iter::from_fn(|| rx.try_recv().ok()).map(|e| (e.pos, e.seq)).collect();
        assert_eq!(resent, vec![(Some(2), Some(1)), (Some(3), Some(2))], "once each, in journal order");

        outbox.journal_ack("s", 3);
        assert_eq!((outbox.retained_len(), outbox.unacked_len()), (0, 0));
    }

    #[test]
    fn journal_only_kinds_wait_for_a_journal_server() {
        let outbox = Arc::new(Outbox::new());
        let journal = Journal::new(outbox.clone(), None, Some("s".into()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        outbox.attach(tx);
        journal.emit(FromAgent::Now { task: "t".into(), effect_id: String::new(), value: 1.0 }, None);
        assert!(rx.try_recv().is_err(), "not sent while the server's journal support is unknown");
        outbox.set_journal(true);
        outbox.resend_unacked();
        let sent = rx.try_recv().unwrap();
        assert!(matches!(&sent.message, FromAgent::Now { effect_id, .. } if effect_id == "t:1"));

        let old = Arc::new(Outbox::new());
        let journal = Journal::new(old.clone(), None, Some("s".into()));
        old.set_journal(false);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        old.attach(tx);
        journal.emit(FromAgent::Now { task: "t".into(), effect_id: String::new(), value: 1.0 }, None);
        journal.emit(FromAgent::Log { task: "t".into(), message: "a".into(), level: Default::default() }, None);
        let only: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok()).map(|e| crate::journal::kind_of(&e.message)).collect();
        assert_eq!(only, ["LOG"], "an old server never gets journal-only kinds");
    }

    #[test]
    fn earlier_sessions_are_resent_first() {
        let outbox = Arc::new(Outbox::new());
        let journal = Journal::new(outbox.clone(), None, Some("now".into()));
        journal.emit(FromAgent::Log { task: "t".into(), message: "current".into(), level: Default::default() }, None);
        let old = |session: &str, pos: u64| JournalEntry {
            session_id: session.into(),
            pos,
            global_rev: 0,
            timepoint: String::new(),
            event_time: 0,
            kind: "LOG".into(),
            task_id: Some("old".into()),
            step: Some(pos),
            action_key: None,
            subject: None,
            message_id: format!("{session}-{pos}"),
            payload: serde_json::json!({"type": "LOG", "id": "x", "task": "old", "message": "m", "level": "INFO"}),
        };
        outbox.preload(vec![old("a", 4), old("a", 5), old("b", 1)]);
        outbox.set_journal(true);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        outbox.attach(tx);
        outbox.resend_unacked();
        let order: Vec<(String, u64)> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|e| (e.journal_session.unwrap(), e.pos.unwrap()))
            .collect();
        assert_eq!(
            order,
            [("a".into(), 4), ("a".into(), 5), ("b".into(), 1), ("now".into(), 1)]
        );
        outbox.journal_ack("a", 5);
        assert_eq!(outbox.retained_len(), 2);
    }
}
