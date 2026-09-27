//! Fans the agent's messages out to websocket subscribers.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tokio::sync::mpsc::UnboundedSender;

use crate::emit::Emitter;
use crate::messages::{Envelope, FromAgent};

/// What a subscriber asked for; `None` means everything of that kind.
#[derive(Debug, Clone, Default)]
pub(crate) struct Filters {
    pub action_keys: Option<HashSet<String>>,
    pub state_keys: Option<HashSet<String>>,
    pub lock_keys: Option<HashSet<String>>,
}

struct Subscriber {
    id: u64,
    filters: Filters,
    tx: UnboundedSender<String>,
}

/// Routes each message to the subscribers whose filters match, as the Python
/// FastAPI transport does:
/// * `STATE_PATCH` by state name, `LOCK`/`UNLOCK` by lock key;
/// * everything else by the action key of its (still managed) task. Messages
///   without one, such as `SESSION_INIT` and `STATE_SNAPSHOT`, reach nobody.
#[derive(Default)]
pub struct Broadcaster {
    subscribers: Mutex<Vec<Subscriber>>,
    next_id: AtomicU64,
    seq: AtomicU64,
}

impl std::fmt::Debug for Broadcaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Broadcaster")
            .field("subscribers", &self.subscribers.lock().expect("subscribers").len())
            .finish()
    }
}

fn allows(filter: &Option<HashSet<String>>, key: &str) -> bool {
    filter.as_ref().is_none_or(|keys| keys.contains(key))
}

impl Broadcaster {
    pub(crate) fn subscribe(&self, filters: Filters, tx: UnboundedSender<String>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.subscribers
            .lock()
            .expect("subscribers")
            .push(Subscriber { id, filters, tx });
        id
    }

    pub(crate) fn unsubscribe(&self, id: u64) {
        self.subscribers.lock().expect("subscribers").retain(|s| s.id != id);
    }
}

impl Emitter for Broadcaster {
    fn emit(&self, message: FromAgent, action_key: Option<&str>) {
        let matches: Box<dyn Fn(&Filters) -> bool> = match &message {
            FromAgent::StatePatch { state_name, .. } => {
                let name = state_name.clone();
                Box::new(move |f: &Filters| allows(&f.state_keys, &name))
            }
            FromAgent::Lock { key, .. } | FromAgent::Unlock { key } => {
                let key = key.clone();
                Box::new(move |f: &Filters| allows(&f.lock_keys, &key))
            }
            _ => match action_key {
                Some(key) => {
                    let key = key.to_owned();
                    Box::new(move |f: &Filters| allows(&f.action_keys, &key))
                }
                None => return,
            },
        };

        let mut envelope = Envelope::new(message);
        if envelope.message.is_event() {
            envelope.seq = Some(self.seq.fetch_add(1, Ordering::SeqCst) + 1);
        }
        let text = serde_json::to_string(&envelope).expect("messages serialize");
        self.subscribers
            .lock()
            .expect("subscribers")
            .retain(|s| !matches(&s.filters) || s.tx.send(text.clone()).is_ok());
    }
}
