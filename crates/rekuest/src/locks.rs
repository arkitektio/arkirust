//! Named locks an action holds while it runs (e.g. a piece of hardware).
//!
//! Locks are local and FIFO: an assignment waits until it gets them, it is
//! never refused. Taking and releasing a lock is reported as `LOCK`/`UNLOCK`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde::Serialize;
use tokio::sync::OwnedMutexGuard;

use crate::emit::Emitter;
use crate::messages::FromAgent;

struct Slot {
    mutex: Arc<tokio::sync::Mutex<()>>,
    holder: Mutex<Option<String>>,
}

/// Every lock declared by an app.
pub struct LockTable {
    slots: IndexMap<String, Arc<Slot>>,
}

impl std::fmt::Debug for LockTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.slots.keys()).finish()
    }
}

/// One lock as the served app shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LockView {
    pub interface: String,
    pub key: String,
    pub task_id: Option<String>,
}

impl LockTable {
    pub fn new(keys: impl IntoIterator<Item = String>) -> Self {
        Self {
            slots: keys
                .into_iter()
                .map(|key| {
                    (
                        key,
                        Arc::new(Slot {
                            mutex: Arc::new(tokio::sync::Mutex::new(())),
                            holder: Mutex::new(None),
                        }),
                    )
                })
                .collect(),
        }
    }

    /// Take `keys` (sorted, unknown keys skipped) for `task`, waiting as needed.
    pub(crate) async fn acquire(&self, keys: &[String], task: &str, emitter: Arc<dyn Emitter>) -> HeldLocks {
        let mut keys: Vec<&String> = keys.iter().filter(|k| self.slots.contains_key(*k)).collect();
        keys.sort();
        keys.dedup();
        let mut held = HeldLocks {
            held: vec![],
            emitter: emitter.clone(),
        };
        for key in keys {
            let slot = self.slots[key].clone();
            let guard = slot.mutex.clone().lock_owned().await;
            *slot.holder.lock().expect("lock holder") = Some(task.to_owned());
            emitter.emit(
                FromAgent::Lock {
                    key: key.clone(),
                    task: task.to_owned(),
                },
                None,
            );
            held.held.push((key.clone(), slot, guard));
        }
        held
    }

    pub fn views(&self, filter: Option<&HashSet<String>>) -> IndexMap<String, LockView> {
        self.slots
            .iter()
            .filter(|(key, _)| filter.is_none_or(|f| f.contains(*key)))
            .map(|(key, slot)| {
                (
                    key.clone(),
                    LockView {
                        interface: key.clone(),
                        key: key.clone(),
                        task_id: slot.holder.lock().expect("lock holder").clone(),
                    },
                )
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// Locks held by one assignment; released (in reverse order) when dropped,
/// which also covers a cancelled task.
pub(crate) struct HeldLocks {
    held: Vec<(String, Arc<Slot>, OwnedMutexGuard<()>)>,
    emitter: Arc<dyn Emitter>,
}

impl Drop for HeldLocks {
    fn drop(&mut self) {
        while let Some((key, slot, guard)) = self.held.pop() {
            *slot.holder.lock().expect("lock holder") = None;
            self.emitter.emit(FromAgent::Unlock { key }, None);
            drop(guard);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit::NullEmitter;

    #[tokio::test]
    async fn opposite_orders_do_not_deadlock() {
        let table = Arc::new(LockTable::new(["a".to_owned(), "b".to_owned()]));
        let mut handles = vec![];
        for round in 0..25 {
            for keys in [vec!["a".to_owned(), "b".to_owned()], vec!["b".to_owned(), "a".to_owned()]] {
                let table = table.clone();
                handles.push(tokio::spawn(async move {
                    let held = table.acquire(&keys, &format!("t{round}"), Arc::new(NullEmitter)).await;
                    tokio::task::yield_now().await;
                    drop(held);
                }));
            }
        }
        for handle in handles {
            tokio::time::timeout(std::time::Duration::from_secs(5), handle)
                .await
                .expect("no deadlock")
                .unwrap();
        }
        assert!(table.views(None).values().all(|v| v.task_id.is_none()));
    }

    #[tokio::test]
    async fn unknown_keys_are_skipped() {
        let table = LockTable::new(["a".to_owned()]);
        let held = table.acquire(&["zzz".to_owned(), "a".to_owned()], "t", Arc::new(NullEmitter)).await;
        assert_eq!(table.views(None)["a"].task_id.as_deref(), Some("t"));
        drop(held);
        assert_eq!(table.views(None)["a"].task_id, None);
    }
}
