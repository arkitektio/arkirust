//! Outgoing agent events: numbering, and retention of terminal reports until
//! the server acknowledges them.
//!
//! Tasks keep running across reconnects, so they report into the outbox
//! rather than into a specific socket. Terminal events (`COMPLETED`,
//! `FAILED`, ...) sent while disconnected, or sent but not yet acked, are
//! re-sent after the next `INIT`.

use std::collections::BTreeMap;
use std::sync::Mutex;

use tokio::sync::mpsc::UnboundedSender;

use crate::messages::{Envelope, FromAgent};

#[derive(Default)]
struct State {
    tx: Option<UnboundedSender<Envelope>>,
    seq: u64,
    /// Terminal events not yet acknowledged, by event id (insertion ordered by seq).
    unacked: BTreeMap<String, Envelope>,
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
        let mut state = self.state.lock().expect("outbox lock");
        let mut envelope = Envelope::new(message);
        if envelope.message.is_event() {
            state.seq += 1;
            envelope.seq = Some(state.seq);
        }
        if envelope.message.is_terminal() {
            state.unacked.insert(envelope.id.clone(), envelope.clone());
        }
        match &state.tx {
            Some(tx) => {
                let _ = tx.send(envelope);
            }
            None if envelope.message.is_terminal() => {
                tracing::debug!(
                    "disconnected, will resend {:?} after reconnect",
                    envelope.message
                );
            }
            None => tracing::debug!("disconnected, dropping {:?}", envelope.message),
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

    /// Re-send every unacknowledged terminal report on the current connection.
    pub fn resend_unacked(&self) {
        let mut state = self.state.lock().expect("outbox lock");
        let pending: Vec<Envelope> = std::mem::take(&mut state.unacked).into_values().collect();
        for mut envelope in pending {
            state.seq += 1;
            envelope.seq = Some(state.seq);
            if let Some(tx) = &state.tx {
                let _ = tx.send(envelope.clone());
            }
            state.unacked.insert(envelope.id.clone(), envelope);
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

    #[cfg(test)]
    pub fn unacked_len(&self) -> usize {
        self.state.lock().expect("outbox lock").unacked.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_until_ack() {
        let outbox = Outbox::new();
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
}
