//! Outgoing agent frames: retention until the server acknowledges them.
//!
//! Tasks keep running across reconnects, so they report into the outbox
//! rather than into a specific socket. Nothing is sent between `REGISTER`
//! and `INIT`; after `INIT` the outbox re-sends what was not acknowledged.
//!
//! Every numbered frame (one with a journal `pos`) is retained, in
//! `(session, pos)` order, until a cumulative `JOURNAL_ACK` covers it; that
//! includes terminal reports (`EVENT_ACK` is not needed). Frames of earlier
//! sessions (read back from the local journal after a restart) go first.
//! Unnumbered frames (probes, requests) are sent once and never retained.
//!
//! The retained frames are bounded ([`RETAIN_LIMIT`]) only when the local
//! journal persists them: beyond it, the newest are dropped from memory (the
//! lowest positions, which the server needs first, stay) and read back from
//! the journal before the next resend ([`Outbox::needs_reload`]).
//! Without a local journal nothing is dropped, since a lost frame would
//! stall the server's watermark for good.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::journal::JournalEntry;
use crate::messages::{Envelope, FromAgent};

/// Numbered frames kept in memory for resending at most (with a local journal).
pub const RETAIN_LIMIT: usize = 65_536;

struct State {
    tx: Option<UnboundedSender<Envelope>>,
    seq: u64,
    /// Numbered frames not yet covered by a `JOURNAL_ACK`, by `(session order, pos)`.
    retained: BTreeMap<(i64, u64), Envelope>,
    /// The order sessions are re-sent in; earlier sessions come first.
    sessions: HashMap<String, i64>,
    next_session: i64,
    /// The highest `JOURNAL_ACK` per session.
    acked: HashMap<String, u64>,
    limit: usize,
    /// Retained frames are also in the local journal, so they may be dropped from memory.
    spill: bool,
    /// Some were dropped from memory and must be read back before resending.
    spilled: bool,
    warned: bool,
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
        let order = self.session_order(session);
        self.retained.insert((order, pos), envelope.clone());
        self.bound();
    }

    fn bound(&mut self) {
        if self.retained.len() <= self.limit {
            return;
        }
        if self.spill {
            while self.retained.len() > self.limit {
                self.retained.pop_last();
            }
            if !self.spilled {
                tracing::warn!(
                    "the server has not acknowledged {} frames; keeping the rest in the local journal only",
                    self.limit
                );
            }
            self.spilled = true;
        } else if !self.warned {
            self.warned = true;
            tracing::warn!(
                "the server has not acknowledged {} frames; they are kept in memory",
                self.limit
            );
        }
    }

    fn deliver(&mut self, mut envelope: Envelope) {
        if envelope.message.is_event() {
            self.seq += 1;
            envelope.seq = Some(self.seq);
        }
        self.retain(&envelope);
        match &self.tx {
            Some(tx) => {
                let _ = tx.send(envelope);
            }
            None => tracing::debug!("disconnected, holding {:?}", envelope.message),
        }
    }
}

/// Where an agent's frames wait for a connection and for the server's ack.
pub struct Outbox {
    state: Mutex<State>,
}

impl Default for Outbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Outbox {
    /// An outbox without a local journal: it keeps every unacknowledged frame.
    pub fn new() -> Self {
        Self::with_limit(RETAIN_LIMIT, false)
    }

    /// With `spill`, the retained frames are also in the local journal:
    /// beyond `limit`, the newest are dropped from memory and read back
    /// (see [`Outbox::reload`]) before the next resend.
    pub fn with_limit(limit: usize, spill: bool) -> Self {
        Self {
            state: Mutex::new(State {
                tx: None,
                seq: 0,
                retained: BTreeMap::new(),
                sessions: HashMap::new(),
                next_session: 0,
                acked: HashMap::new(),
                limit: limit.max(1),
                spill,
                spilled: false,
                warned: false,
            }),
        }
    }

    /// Queue a message on the current connection (if any), unnumbered.
    pub fn send(&self, message: FromAgent) {
        self.state
            .lock()
            .expect("outbox lock")
            .deliver(Envelope::new(message));
    }

    /// Queue a journaled message: it carries the entry's id and position.
    pub fn send_entry(&self, message: FromAgent, entry: &JournalEntry) {
        self.state
            .lock()
            .expect("outbox lock")
            .deliver(Envelope::journaled(message, entry));
    }

    #[cfg_attr(not(feature = "wal"), allow(dead_code))]
    /// Hold entries read back from the local journal to be re-sent: those of
    /// earlier sessions go before anything of the current session. Entries
    /// already held or acknowledged are skipped. An entry that no longer
    /// parses ends its session's backlog: the rest could never be projected.
    pub fn reload(&self, entries: Vec<JournalEntry>) {
        let mut state = self.state.lock().expect("outbox lock");
        let mut unknown: Vec<String> = vec![];
        for entry in &entries {
            if !state.sessions.contains_key(&entry.session_id)
                && !unknown.contains(&entry.session_id)
            {
                unknown.push(entry.session_id.clone());
            }
        }
        let first = state.sessions.values().min().copied().unwrap_or(0).min(0);
        for (i, session) in unknown.iter().enumerate() {
            let order = first - (unknown.len() - i) as i64;
            state.sessions.insert(session.clone(), order);
        }
        let mut broken: Vec<String> = vec![];
        for entry in entries {
            if broken.contains(&entry.session_id)
                || state.acked.get(&entry.session_id).copied().unwrap_or(0) >= entry.pos
            {
                continue;
            }
            let order = state.sessions[&entry.session_id];
            if state.retained.contains_key(&(order, entry.pos)) {
                continue;
            }
            let mut payload = entry.payload.clone();
            if let Value::Object(map) = &mut payload {
                map.remove("id");
            }
            let message: FromAgent = match serde_json::from_value(payload) {
                Ok(message) => message,
                Err(e) => {
                    tracing::warn!(
                        "journal entry {}/{} is unreadable ({e}); not re-sending the rest of that session",
                        entry.session_id,
                        entry.pos
                    );
                    broken.push(entry.session_id.clone());
                    continue;
                }
            };
            let envelope = Envelope::journaled(message, &entry);
            state.retained.insert((order, entry.pos), envelope);
        }
        state.spilled = false;
        state.bound();
    }

    #[cfg_attr(not(feature = "wal"), allow(dead_code))]
    /// Whether frames were dropped from memory and must be [reloaded](Outbox::reload)
    /// from the local journal before [`Outbox::resend_unacked`].
    pub fn needs_reload(&self) -> bool {
        self.state.lock().expect("outbox lock").spilled
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

    /// Re-send what the server has not acknowledged, in `(session, pos)` order.
    pub fn resend_unacked(&self) {
        let mut state = self.state.lock().expect("outbox lock");
        let retained: Vec<Envelope> = std::mem::take(&mut state.retained).into_values().collect();
        for envelope in retained {
            state.deliver(envelope);
        }
    }

    /// Everything of `session` up to `pos` is projected by the server.
    pub fn journal_ack(&self, session: &str, pos: u64) {
        let mut state = self.state.lock().expect("outbox lock");
        let acked = state.acked.entry(session.to_owned()).or_default();
        *acked = (*acked).max(pos);
        if let Some(order) = state.sessions.get(session).copied() {
            state
                .retained
                .retain(|(o, p), _| !(*o == order && *p <= pos));
        }
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

    fn log(task: &str) -> FromAgent {
        FromAgent::Log {
            task: task.into(),
            message: "m".into(),
            level: Default::default(),
        }
    }

    fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Envelope>) -> Vec<Envelope> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    #[test]
    fn retains_until_journal_ack_terminals_included() {
        let outbox = Arc::new(Outbox::new());
        let journal = Journal::new(outbox.clone(), None, Some("s".into()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        outbox.attach(tx);
        journal.emit(log("t"), None);
        journal.emit(
            FromAgent::Yield {
                task: "t".into(),
                returns: Default::default(),
            },
            None,
        );
        let first = rx.try_recv().unwrap();
        assert_eq!(
            (first.pos, first.journal_session.as_deref(), first.task_step),
            (Some(1), Some("s"), Some(1))
        );
        let json = serde_json::to_value(&first).unwrap();
        assert_eq!(
            (json["pos"].as_u64(), json["journal_session"].as_str()),
            (Some(1), Some("s"))
        );

        // Disconnected: kept, not dropped.
        outbox.detach();
        journal.emit(FromAgent::Completed { task: "t".into() }, None);
        outbox.journal_ack("s", 1);
        assert_eq!(outbox.retained_len(), 2);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        outbox.attach(tx);
        outbox.resend_unacked();
        let resent: Vec<(Option<u64>, Option<u64>)> =
            drain(&mut rx).iter().map(|e| (e.pos, e.seq)).collect();
        assert_eq!(
            resent,
            vec![(Some(2), Some(1)), (Some(3), Some(2))],
            "once each, in journal order"
        );

        // The terminal report is retired by JOURNAL_ACK alone.
        outbox.journal_ack("s", 3);
        assert_eq!(outbox.retained_len(), 0);
    }

    #[test]
    fn effects_are_numbered_and_sent_at_once() {
        let outbox = Arc::new(Outbox::new());
        let journal = Journal::new(outbox.clone(), None, Some("s".into()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        outbox.attach(tx);
        journal.emit(log("t"), None);
        journal.emit(
            FromAgent::Effect {
                task: "t".into(),
                effect: crate::messages::EffectKind::Now,
                value: 1.5.into(),
            },
            None,
        );
        let sent = drain(&mut rx);
        assert_eq!((sent[1].pos, sent[1].task_step), (Some(2), Some(2)));
        let json = serde_json::to_value(&sent[1]).unwrap();
        assert_eq!(
            (
                json["type"].as_str(),
                json["effect"].as_str(),
                json["value"].as_f64()
            ),
            (Some("EFFECT"), Some("NOW"), Some(1.5))
        );
    }

    #[test]
    fn probes_and_requests_are_never_numbered_or_retained() {
        let outbox = Arc::new(Outbox::new());
        let journal = Journal::new(outbox.clone(), None, Some("s".into()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        outbox.attach(tx);
        journal.emit(log("p-1"), None);
        journal.emit(FromAgent::Completed { task: "p-1".into() }, None);
        journal.emit(
            FromAgent::AssignRequest {
                reference: None,
                parent_step: Some(3),
                args: Default::default(),
                action: Some("a".into()),
                action_hash: None,
                implementation: None,
                agent: None,
                interface: None,
                parent: Some("t".into()),
                dependency: None,
                method: None,
                resolution: None,
                hooks: None,
                capture: None,
                step: None,
            },
            None,
        );
        journal.emit(log("t"), None);
        let sent = drain(&mut rx);
        assert_eq!(
            sent.iter()
                .map(|e| (e.pos, e.task_step))
                .collect::<Vec<_>>(),
            [(None, None), (None, None), (None, None), (Some(1), Some(1))]
        );
        assert_eq!(sent[2].seq, None, "a request has no seq");
        assert_eq!(outbox.retained_len(), 1);

        outbox.detach();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        outbox.attach(tx);
        outbox.resend_unacked();
        assert_eq!(drain(&mut rx).len(), 1, "only the numbered frame is resent");
    }

    fn entry(session: &str, pos: u64) -> JournalEntry {
        JournalEntry {
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
        }
    }

    #[test]
    fn earlier_sessions_are_resent_first() {
        let outbox = Arc::new(Outbox::new());
        let journal = Journal::new(outbox.clone(), None, Some("now".into()));
        journal.emit(log("t"), None);
        outbox.reload(vec![entry("a", 4), entry("a", 5), entry("b", 1)]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        outbox.attach(tx);
        outbox.resend_unacked();
        let order: Vec<(String, u64)> = drain(&mut rx)
            .into_iter()
            .map(|e| (e.journal_session.unwrap(), e.pos.unwrap()))
            .collect();
        assert_eq!(
            order,
            [
                ("a".into(), 4),
                ("a".into(), 5),
                ("b".into(), 1),
                ("now".into(), 1)
            ]
        );
        outbox.journal_ack("a", 5);
        assert_eq!(outbox.retained_len(), 2);
    }

    #[test]
    fn an_unreadable_entry_ends_its_sessions_backlog() {
        let outbox = Outbox::new();
        let mut old = entry("a", 2);
        old.payload =
            serde_json::json!({"type": "NOW", "task": "old", "effect_id": "old:2", "value": 1.0});
        outbox.reload(vec![entry("a", 1), old, entry("a", 3), entry("b", 1)]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        outbox.attach(tx);
        outbox.resend_unacked();
        let order: Vec<(String, u64)> = drain(&mut rx)
            .into_iter()
            .map(|e| (e.journal_session.unwrap(), e.pos.unwrap()))
            .collect();
        assert_eq!(order, [("a".into(), 1), ("b".into(), 1)]);
    }

    #[test]
    fn beyond_the_limit_frames_are_reloaded_not_lost() {
        let outbox = Arc::new(Outbox::with_limit(3, true));
        let journal = Journal::new(outbox.clone(), None, Some("s".into()));
        for _ in 0..5 {
            journal.emit(log("t"), None);
        }
        assert_eq!(outbox.retained_len(), 3);
        assert!(outbox.needs_reload());
        outbox.journal_ack("s", 1);

        // What the local journal holds (acked or not).
        let entries: Vec<JournalEntry> = journal.locked(|view| {
            view.recent(0, 5)
                .unwrap()
                .iter()
                .map(|e| (**e).clone())
                .collect()
        });
        outbox.reload(entries);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        outbox.attach(tx);
        outbox.resend_unacked();
        let resent: Vec<u64> = drain(&mut rx).iter().map(|e| e.pos.unwrap()).collect();
        assert_eq!(resent, [2, 3, 4], "gapless from the ack, up to the limit");
        assert!(outbox.needs_reload(), "pos 5 is still only in the journal");
        outbox.journal_ack("s", 4);
        outbox.reload(journal.locked(|view| {
            view.recent(0, 5)
                .unwrap()
                .iter()
                .map(|e| (**e).clone())
                .collect()
        }));
        outbox.resend_unacked();
        let resent: Vec<u64> = drain(&mut rx).iter().map(|e| e.pos.unwrap()).collect();
        assert_eq!(resent, [5]);
    }

    #[test]
    fn without_a_local_journal_nothing_is_dropped() {
        let outbox = Arc::new(Outbox::with_limit(2, false));
        let journal = Journal::new(outbox.clone(), None, Some("s".into()));
        for _ in 0..4 {
            journal.emit(log("t"), None);
        }
        assert_eq!(outbox.retained_len(), 4);
        assert!(!outbox.needs_reload());
    }
}
