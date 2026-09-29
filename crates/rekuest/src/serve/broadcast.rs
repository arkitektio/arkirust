//! Fans the agent's messages out to websocket subscribers.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tokio::sync::mpsc::UnboundedSender;

use crate::emit::Emitter;
use crate::journal::{stamp, JournalEntry, Route};
use crate::messages::{Envelope, FromAgent};

/// What a subscriber asked for; `None` means everything of that kind.
#[derive(Debug, Clone, Default)]
pub(crate) struct Filters {
    pub action_keys: Option<HashSet<String>>,
    pub state_keys: Option<HashSet<String>>,
    pub lock_keys: Option<HashSet<String>>,
}

impl Filters {
    /// Whether a journal subscriber gets an entry routed this way.
    pub(crate) fn admits(&self, route: Route<'_>) -> bool {
        match route {
            Route::State(name) => allows(&self.state_keys, name),
            Route::Lock(key) => allows(&self.lock_keys, key),
            Route::Action(key) => allows(&self.action_keys, key),
            Route::Everyone => true,
        }
    }
}

struct Subscriber {
    id: u64,
    filters: Filters,
    tx: UnboundedSender<String>,
    /// Opted into the journal: frames carry `pos`, and session-wide entries
    /// (`SESSION_INIT`, `STATE_SNAPSHOT`) are included.
    journal: bool,
}

/// Routes each message to the subscribers whose filters match, as the Python
/// FastAPI transport does:
/// * `STATE_PATCH` by state name, `LOCK`/`UNLOCK` by lock key;
/// * everything else by the action key of its (still managed) task. Messages
///   without one, such as `SESSION_INIT` and `STATE_SNAPSHOT`, reach nobody.
///
/// Journal subscribers get the same frames plus `pos`/`journal_session`, and
/// also the session-wide ones.
#[derive(Default)]
pub struct Broadcaster {
    subscribers: Mutex<Vec<Subscriber>>,
    next_id: AtomicU64,
    /// Numbered under the subscribers lock, so frames go out in `seq` order.
    seq: Mutex<u64>,
}

impl std::fmt::Debug for Broadcaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Broadcaster")
            .field(
                "subscribers",
                &self.subscribers.lock().expect("subscribers").len(),
            )
            .finish()
    }
}

fn allows(filter: &Option<HashSet<String>>, key: &str) -> bool {
    filter.as_ref().is_none_or(|keys| keys.contains(key))
}

/// How the Python transport routes a message; `None` reaches nobody.
fn legacy_route<'a>(message: &'a FromAgent, action_key: Option<&'a str>) -> Option<Route<'a>> {
    match message {
        // Python's subscribers never see these.
        FromAgent::Shelve { .. }
        | FromAgent::Unshelve { .. }
        | FromAgent::Effect { .. }
        | FromAgent::AssignRequest { .. } => None,
        FromAgent::StatePatch { state_name, .. } => Some(Route::State(state_name)),
        FromAgent::Lock { key, .. } | FromAgent::Unlock { key, .. } => Some(Route::Lock(key)),
        _ => action_key.map(Route::Action),
    }
}

impl Broadcaster {
    pub(crate) fn subscribe(
        &self,
        filters: Filters,
        tx: UnboundedSender<String>,
        journal: bool,
    ) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.subscribers
            .lock()
            .expect("subscribers")
            .push(Subscriber {
                id,
                filters,
                tx,
                journal,
            });
        id
    }

    pub(crate) fn unsubscribe(&self, id: u64) {
        self.subscribers
            .lock()
            .expect("subscribers")
            .retain(|s| s.id != id);
    }

    fn deliver(&self, message: FromAgent, action_key: Option<&str>, entry: Option<&JournalEntry>) {
        let mut subscribers = self.subscribers.lock().expect("subscribers");
        let mut envelope = Envelope::new(message);
        if let Some(entry) = entry {
            envelope.id = entry.message_id.clone();
        }
        // Numbered even when nobody is listening, as in Python.
        if envelope.message.is_event() {
            let mut seq = self.seq.lock().expect("seq");
            *seq += 1;
            envelope.seq = Some(*seq);
        }
        let legacy = legacy_route(&envelope.message, action_key);
        let journal_route = entry.map(JournalEntry::route).or(legacy);

        let mut plain: Option<String> = None;
        let mut stamped: Option<String> = None;
        subscribers.retain(|s| {
            let text = if s.journal {
                if !journal_route.is_some_and(|r| s.filters.admits(r)) {
                    return true;
                }
                stamped.get_or_insert_with(|| {
                    let mut frame = serde_json::to_value(&envelope).expect("messages serialize");
                    if let Some(entry) = entry {
                        stamp(&mut frame, entry);
                    }
                    frame.to_string()
                })
            } else {
                if !legacy.is_some_and(|r| s.filters.admits(r)) {
                    return true;
                }
                plain.get_or_insert_with(|| {
                    let mut frame = serde_json::to_value(&envelope).expect("messages serialize");
                    // Python's UNLOCK does not name the holder.
                    if let (FromAgent::Unlock { .. }, Some(map)) =
                        (&envelope.message, frame.as_object_mut())
                    {
                        map.remove("task");
                    }
                    frame.to_string()
                })
            };
            s.tx.send(text.clone()).is_ok()
        });
    }
}

impl Emitter for Broadcaster {
    fn emit(&self, message: FromAgent, action_key: Option<&str>) {
        self.deliver(message, action_key, None)
    }

    fn emit_entry(&self, message: FromAgent, action_key: Option<&str>, entry: &JournalEntry) {
        self.deliver(message, action_key, Some(entry))
    }
}
