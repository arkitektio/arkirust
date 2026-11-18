//! Outgoing agent frames: retention until the server acknowledges them.
//!
//! Tasks keep running across reconnects, so they report into the outbox
//! rather than into a specific socket. Nothing is sent between `REGISTER`
//! and `INIT`; after `INIT` the outbox re-sends what was not acknowledged.
//!
//! Every numbered frame (one with a journal `pos`) is retained, in
//! `(session, pos)` order, until a cumulative `JOURNAL_ACK` covers it; that
//! includes terminal reports and the requests a task makes. Frames of earlier
//! sessions (read back from the local journal after a restart) go first.
//! Unnumbered frames (a probe's, the acknowledgements) are sent once and never
//! retained. A `JOURNAL_NACK` (the server found a gap) has what is retained from
//! the position it names sent again, in order.
//!
//! Numbered frames go out through a **window** ([`SEND_WINDOW`]): at most that
//! many are sent and not yet acknowledged, and the rest wait in the retained
//! backlog, in order. The acknowledgements are the credit, so what the server
//! has to hold of this agent is bounded, and a backlog built up while it was
//! away goes out at the pace the server takes it.
//!
//! The retained frames are bounded ([`RETAIN_LIMIT`]) only when the local
//! journal persists them: beyond it, the newest are dropped from memory (the
//! lowest positions, which the server needs first, stay) and read back from
//! the journal before the next resend ([`Outbox::needs_reload`]), or once the
//! window has taken everything before them ([`Outbox::starved`]).
//! Without a local journal nothing is dropped, since a lost frame would
//! stall the server's watermark for good.

use alloc::collections::{BTreeMap, VecDeque};
use core::ops::Bound;

use serde_json::Value;

use crate::entry::JournalEntry;
use crate::prelude::*;
use crate::protocol::messages::{Envelope, FromAgent};

/// Numbered frames kept in memory for resending at most (with a local journal).
pub const RETAIN_LIMIT: usize = 65_536;

/// Numbered frames sent and not yet acknowledged at most.
pub const SEND_WINDOW: usize = 256;

/// Where a retained frame stands: its session's place in the resend order, and its position.
type Key = (i64, u64);

/// Where an agent's frames wait for a connection and for the server's ack.
pub struct Outbox {
    attached: bool,
    /// Frames for the current connection, oldest first.
    queue: VecDeque<Envelope>,
    /// Numbered frames not yet covered by a `JOURNAL_ACK`, by `(session order, pos)`.
    retained: BTreeMap<Key, Envelope>,
    /// The order sessions are re-sent in; earlier sessions come first.
    sessions: BTreeMap<String, i64>,
    next_session: i64,
    /// The highest `JOURNAL_ACK` per session.
    acked: BTreeMap<String, u64>,
    limit: usize,
    /// Retained frames are also in the local journal, so they may be dropped from memory.
    spill: bool,
    /// Some were dropped from memory and must be read back before resending.
    spilled: bool,
    /// The lowest frame dropped from memory: nothing at or beyond it goes out before the
    /// journal was read back, or it would go out ahead of what is missing.
    spilled_from: Option<Key>,
    warned: bool,
    window: usize,
    /// The last retained frame handed to the connection: what is beyond it waits.
    admitted: Option<Key>,
    /// How many retained frames are handed out and not acknowledged yet.
    out: usize,
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
            attached: false,
            queue: VecDeque::new(),
            retained: BTreeMap::new(),
            sessions: BTreeMap::new(),
            next_session: 0,
            acked: BTreeMap::new(),
            limit: limit.max(1),
            spill,
            spilled: false,
            spilled_from: None,
            warned: false,
            window: SEND_WINDOW,
            admitted: None,
            out: 0,
        }
    }

    /// With another send window than [`SEND_WINDOW`].
    pub fn with_window(mut self, window: usize) -> Self {
        self.window = window.max(1);
        self
    }

    /// Queue `envelope` on the current connection (if any). Numbered ones
    /// (with `pos` and `journal_session`) are retained until acknowledged.
    pub fn push(&mut self, envelope: Envelope) {
        let numbered = envelope.pos.is_some() && envelope.journal_session.is_some();
        self.retain(&envelope);
        if !self.attached {
            tracing::debug!("disconnected, holding {:?}", envelope.message);
        } else if numbered {
            // In its turn, and only while the window has room.
            self.fill();
        } else {
            self.queue.push_back(envelope);
        }
    }

    /// Hand the connection what waits, in order, for as long as the window has room.
    fn fill(&mut self) {
        if !self.attached {
            return;
        }
        while self.out < self.window {
            let next = match self.admitted {
                Some(at) => self
                    .retained
                    .range((Bound::Excluded(at), Bound::Unbounded))
                    .next(),
                None => self.retained.iter().next(),
            };
            let Some((key, envelope)) = next else {
                break;
            };
            if self.spilled_from.is_some_and(|from| *key >= from) {
                break; // what comes before it is in the journal only
            }
            let key = *key;
            self.queue.push_back(envelope.clone());
            self.admitted = Some(key);
            self.out += 1;
        }
    }

    /// Whether the window has room that only frames dropped from memory can fill: they must
    /// be [reloaded](Outbox::reload) from the local journal, then [`Outbox::refill`].
    pub fn starved(&self) -> bool {
        self.attached && self.spilled && self.out < self.window
    }

    /// Go on sending after a [reload](Outbox::reload) on a live connection.
    pub fn refill(&mut self) {
        self.fill();
    }

    fn key_of(&self, envelope: &Envelope) -> Option<Key> {
        let order = self.sessions.get(envelope.journal_session.as_deref()?)?;
        Some((*order, envelope.pos?))
    }

    /// Queue a journaled message: it carries the entry's id and position.
    pub fn push_entry(&mut self, message: FromAgent, entry: &JournalEntry) {
        self.push(entry.envelope(message));
    }

    /// The next frame for the connection.
    pub fn poll(&mut self) -> Option<Envelope> {
        self.queue.pop_front()
    }

    /// Hold entries read back from the local journal to be re-sent: those of
    /// earlier sessions go before anything of the current session. Entries
    /// already held or acknowledged are skipped. An entry that no longer
    /// parses ends its session's backlog: the rest could never be projected.
    pub fn reload(&mut self, entries: Vec<JournalEntry>) {
        let mut unknown: Vec<String> = vec![];
        for entry in &entries {
            if !self.sessions.contains_key(&entry.session_id)
                && !unknown.contains(&entry.session_id)
            {
                unknown.push(entry.session_id.clone());
            }
        }
        let first = self.sessions.values().min().copied().unwrap_or(0).min(0);
        for (i, session) in unknown.iter().enumerate() {
            let order = first - (unknown.len() - i) as i64;
            self.sessions.insert(session.clone(), order);
        }
        let mut broken: Vec<String> = vec![];
        for entry in entries {
            if broken.contains(&entry.session_id)
                || self.acked.get(&entry.session_id).copied().unwrap_or(0) >= entry.pos
            {
                continue;
            }
            let order = self.sessions[&entry.session_id];
            if self.retained.contains_key(&(order, entry.pos)) {
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
                        "journal entry {}/{} is unreadable ({}); not re-sending the rest of that session",
                        entry.session_id,
                        entry.pos,
                        e
                    );
                    broken.push(entry.session_id.clone());
                    continue;
                }
            };
            let envelope = entry.envelope(message);
            self.retained.insert((order, entry.pos), envelope);
        }
        self.spilled = false;
        self.spilled_from = None;
        self.bound();
    }

    /// Whether frames were dropped from memory and must be [reloaded](Outbox::reload)
    /// from the local journal before [`Outbox::resend_unacked`].
    pub fn needs_reload(&self) -> bool {
        self.spilled
    }

    /// Bind a fresh connection.
    pub fn attach(&mut self) {
        self.attached = true;
        self.queue.clear();
    }

    /// The journal sessions frames are still owed of (`REGISTER`'s `sessions`).
    pub fn sessions_owed(&self) -> Vec<String> {
        self.sessions
            .iter()
            .filter(|(_, order)| self.retained.keys().any(|(o, _)| o == *order))
            .map(|(session, _)| session.clone())
            .collect()
    }

    /// The server found a gap in `session` and takes `expected` next (`JOURNAL_NACK`): what
    /// is queued from that position on is taken back and what is retained from there goes
    /// out again, in order. If `expected` itself is no longer held, the positions that are
    /// gone (`first..=last`) are returned, for the `JOURNAL_SKIP` that must go out first.
    pub fn nack(&mut self, session: &str, expected: u64) -> Option<(u64, u64)> {
        let order = self.sessions.get(session).copied()?;
        let from: Key = (order, expected);
        let lowest = self
            .retained
            .range(from..)
            .next()
            .filter(|((o, _), _)| *o == order)
            .map(|((_, pos), _)| *pos);
        // Asked for, not in memory, but in the local journal: nothing from there on goes out
        // before the journal was read back ([`Outbox::starved`]). It is not gone.
        let in_journal = self.spilled && lowest != Some(expected);
        if in_journal {
            self.spilled_from = Some(self.spilled_from.map_or(from, |spilled| spilled.min(from)));
        }
        // Back to just before it; never forward, past what was not handed out yet.
        let before = self.retained.range(..from).next_back().map(|(key, _)| *key);
        self.admitted = match (self.admitted, before) {
            (Some(admitted), Some(before)) => Some(admitted.min(before)),
            _ => None,
        };
        let admitted = self.admitted;
        let waiting: Vec<bool> = self
            .queue
            .iter()
            .map(|envelope| match (self.key_of(envelope), admitted) {
                (Some(key), Some(admitted)) => key <= admitted,
                (Some(_), None) => false,
                (None, _) => true,
            })
            .collect();
        let mut keep = waiting.into_iter();
        self.queue.retain(|_| keep.next().unwrap_or(true));
        self.out = match admitted {
            Some(admitted) => self.retained.range(..=admitted).count(),
            None => 0,
        };
        self.fill();
        match lowest {
            Some(lowest) if lowest > expected && !in_journal => Some((expected, lowest - 1)),
            _ => None,
        }
    }

    /// The connection is gone: what was queued for it and not yet taken is
    /// dropped (numbered frames stay retained).
    pub fn detach(&mut self) {
        self.attached = false;
        self.queue.clear();
        self.admitted = None;
        self.out = 0;
    }

    /// Re-send what the server has not acknowledged, in `(session, pos)` order, from the
    /// start: through the window, like everything else.
    pub fn resend_unacked(&mut self) {
        self.queue.retain(|envelope| envelope.pos.is_none());
        self.admitted = None;
        self.out = 0;
        self.fill();
    }

    /// Everything of `session` up to `pos` is projected by the server.
    pub fn journal_ack(&mut self, session: &str, pos: u64) {
        let acked = self.acked.entry(session.to_owned()).or_default();
        *acked = (*acked).max(pos);
        if let Some(order) = self.sessions.get(session).copied() {
            let admitted = self.admitted;
            let mut taken = 0;
            self.retained.retain(|key, _| {
                let covered = key.0 == order && key.1 <= pos;
                if covered && admitted.is_some_and(|admitted| *key <= admitted) {
                    taken += 1;
                }
                !covered
            });
            self.out = self.out.saturating_sub(taken);
        }
        // The window has room again.
        self.fill();
    }

    /// The tasks whose end is retained: reported, and not acknowledged yet.
    pub fn ended_tasks(&self) -> Vec<String> {
        self.retained
            .values()
            .filter(|envelope| envelope.message.is_terminal())
            .filter_map(|envelope| envelope.message.task().map(str::to_owned))
            .collect()
    }

    /// Numbered frames not yet acknowledged.
    pub fn retained_len(&self) -> usize {
        self.retained.len()
    }

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
                if let Some((key, _)) = self.retained.pop_last() {
                    self.spilled_from = Some(self.spilled_from.map_or(key, |from| from.min(key)));
                }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Numberer;

    fn log(task: &str) -> FromAgent {
        FromAgent::Log {
            task: task.into(),
            message: "m".into(),
            level: Default::default(),
        }
    }

    fn drain(outbox: &mut Outbox) -> Vec<Envelope> {
        core::iter::from_fn(|| outbox.poll()).collect()
    }

    fn emit(outbox: &mut Outbox, journal: &mut Numberer, message: FromAgent) {
        let entry = journal.entry(&message);
        outbox.push_entry(message, &entry);
    }

    #[test]
    fn retains_until_journal_ack_terminals_included() {
        let mut outbox = Outbox::new();
        let mut journal = Numberer::new("s");
        outbox.attach();
        emit(&mut outbox, &mut journal, log("t"));
        emit(
            &mut outbox,
            &mut journal,
            FromAgent::Yield {
                task: "t".into(),
                returns: Default::default(),
            },
        );
        let first = outbox.poll().unwrap();
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
        emit(
            &mut outbox,
            &mut journal,
            FromAgent::Completed { task: "t".into() },
        );
        outbox.journal_ack("s", 1);
        assert_eq!(outbox.retained_len(), 2);

        outbox.attach();
        outbox.resend_unacked();
        let resent: Vec<Option<u64>> = drain(&mut outbox).iter().map(|e| e.pos).collect();
        assert_eq!(
            resent,
            vec![Some(2), Some(3)],
            "once each, in journal order"
        );
        assert_eq!(outbox.sessions_owed(), ["s"]);

        // A gap at 2: both go out again. A gap at 1, which is acknowledged and gone: said so.
        assert_eq!(outbox.nack("s", 2), None);
        assert_eq!(drain(&mut outbox).len(), 2);
        assert_eq!(outbox.nack("s", 1), Some((1, 1)));
        drain(&mut outbox);

        // The terminal report is retired by JOURNAL_ACK alone.
        outbox.journal_ack("s", 3);
        assert_eq!(outbox.retained_len(), 0);
    }

    #[test]
    fn unnumbered_frames_are_sent_once_and_never_retained() {
        let mut outbox = Outbox::new();
        let mut journal = Numberer::new("s");
        outbox.attach();
        outbox.push(Envelope::with_id("p".into(), log("p-1")));
        emit(&mut outbox, &mut journal, log("t"));
        let sent = drain(&mut outbox);
        assert_eq!(
            sent.iter().map(|e| e.pos).collect::<Vec<_>>(),
            [None, Some(1)]
        );
        assert_eq!(outbox.retained_len(), 1);

        // Detached, an unnumbered frame is dropped.
        outbox.detach();
        outbox.push(Envelope::with_id("q".into(), log("p-1")));
        outbox.attach();
        outbox.resend_unacked();
        assert_eq!(
            drain(&mut outbox).len(),
            1,
            "only the numbered frame is resent"
        );
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
        let mut outbox = Outbox::new();
        let mut journal = Numberer::new("now");
        emit(&mut outbox, &mut journal, log("t"));
        outbox.reload(vec![entry("a", 4), entry("a", 5), entry("b", 1)]);
        outbox.attach();
        outbox.resend_unacked();
        let order: Vec<(String, u64)> = drain(&mut outbox)
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
        let mut outbox = Outbox::new();
        let mut old = entry("a", 2);
        old.payload =
            serde_json::json!({"type": "NOW", "task": "old", "effect_id": "old:2", "value": 1.0});
        outbox.reload(vec![entry("a", 1), old, entry("a", 3), entry("b", 1)]);
        outbox.attach();
        outbox.resend_unacked();
        let order: Vec<(String, u64)> = drain(&mut outbox)
            .into_iter()
            .map(|e| (e.journal_session.unwrap(), e.pos.unwrap()))
            .collect();
        assert_eq!(order, [("a".into(), 1), ("b".into(), 1)]);
    }

    #[test]
    fn beyond_the_limit_frames_are_reloaded_not_lost() {
        let mut outbox = Outbox::with_limit(3, true);
        let mut journal = Numberer::new("s");
        let mut written = vec![];
        for _ in 0..5 {
            let message = log("t");
            let entry = journal.entry(&message);
            outbox.push_entry(message, &entry);
            written.push(entry);
        }
        assert_eq!(outbox.retained_len(), 3);
        assert!(outbox.needs_reload());
        outbox.journal_ack("s", 1);

        // What the local journal holds (acked or not).
        outbox.reload(written.clone());
        outbox.attach();
        outbox.resend_unacked();
        let resent: Vec<u64> = drain(&mut outbox).iter().map(|e| e.pos.unwrap()).collect();
        assert_eq!(resent, [2, 3, 4], "gapless from the ack, up to the limit");
        assert!(outbox.needs_reload(), "pos 5 is still only in the journal");
        outbox.journal_ack("s", 4);
        outbox.reload(written);
        outbox.resend_unacked();
        let resent: Vec<u64> = drain(&mut outbox).iter().map(|e| e.pos.unwrap()).collect();
        assert_eq!(resent, [5]);
    }

    #[test]
    fn without_a_local_journal_nothing_is_dropped() {
        let mut outbox = Outbox::with_limit(2, false);
        let mut journal = Numberer::new("s");
        for _ in 0..4 {
            emit(&mut outbox, &mut journal, log("t"));
        }
        assert_eq!(outbox.retained_len(), 4);
        assert!(!outbox.needs_reload());
    }
}
