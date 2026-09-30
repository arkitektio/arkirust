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
use crate::journal::TaskGate;
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

    /// Take `keys` (sorted, unknown keys skipped) for `task` into `held`,
    /// waiting as needed. Each lock is taken and reported (`LOCK`) inside the
    /// task's gate; once the gate is closed nothing more is taken, and this
    /// returns false.
    ///
    /// The locks live in `held`, not in the caller's future: aborting the
    /// task does not release them. Whoever reports the task's end releases
    /// them afterwards ([`HeldLocks::release`]), so `UNLOCK` follows the end.
    pub(crate) async fn acquire(
        &self,
        keys: &[String],
        task: &str,
        gate: &TaskGate,
        held: &HeldLocks,
        emitter: &dyn Emitter,
    ) -> bool {
        let mut keys: Vec<&String> = keys
            .iter()
            .filter(|k| self.slots.contains_key(*k))
            .collect();
        keys.sort();
        keys.dedup();
        for key in keys {
            let slot = self.slots[key].clone();
            let guard = slot.mutex.clone().lock_owned().await;
            let Some(_pass) = gate.enter() else {
                return false;
            };
            *slot.holder.lock().expect("lock holder") = Some(task.to_owned());
            held.inner
                .lock()
                .expect("held locks")
                .0
                .push((key.clone(), slot, guard));
            emitter.emit(
                FromAgent::Lock {
                    key: key.clone(),
                    task: task.to_owned(),
                },
                None,
            );
        }
        true
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

type Held = Vec<(String, Arc<Slot>, OwnedMutexGuard<()>)>;

struct HeldInner(Held);

impl Drop for HeldInner {
    /// Frees what was never released (the executor went away) without reporting.
    fn drop(&mut self) {
        while let Some((_, slot, guard)) = self.0.pop() {
            *slot.holder.lock().expect("lock holder") = None;
            drop(guard);
        }
    }
}

/// Locks held by one assignment. Shared between the task's future and the
/// executor, so they outlive an aborted future until the end is reported.
#[derive(Clone)]
pub(crate) struct HeldLocks {
    inner: Arc<Mutex<HeldInner>>,
}

impl Default for HeldLocks {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HeldInner(vec![]))),
        }
    }
}

impl HeldLocks {
    /// Release every lock (in reverse order), reporting `UNLOCK` for `task`.
    pub(crate) fn release(&self, task: &str, emitter: &dyn Emitter) {
        let held = std::mem::take(&mut self.inner.lock().expect("held locks").0);
        for (key, slot, guard) in held.into_iter().rev() {
            *slot.holder.lock().expect("lock holder") = None;
            emitter.emit(
                FromAgent::Unlock {
                    key,
                    task: Some(task.to_owned()),
                },
                None,
            );
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
            for keys in [
                vec!["a".to_owned(), "b".to_owned()],
                vec!["b".to_owned(), "a".to_owned()],
            ] {
                let table = table.clone();
                handles.push(tokio::spawn(async move {
                    let (task, held) = (format!("t{round}"), HeldLocks::default());
                    assert!(
                        table
                            .acquire(&keys, &task, &TaskGate::default(), &held, &NullEmitter)
                            .await
                    );
                    tokio::task::yield_now().await;
                    held.release(&task, &NullEmitter);
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
        let held = HeldLocks::default();
        table
            .acquire(
                &["zzz".to_owned(), "a".to_owned()],
                "t",
                &TaskGate::default(),
                &held,
                &NullEmitter,
            )
            .await;
        assert_eq!(table.views(None)["a"].task_id.as_deref(), Some("t"));
        drop(held);
        assert_eq!(
            table.views(None)["a"].task_id,
            None,
            "dropping frees silently"
        );
    }

    #[tokio::test]
    async fn a_closed_gate_takes_nothing() {
        let table = LockTable::new(["a".to_owned()]);
        let (gate, held) = (TaskGate::default(), HeldLocks::default());
        gate.close();
        assert!(
            !table
                .acquire(&["a".to_owned()], "t", &gate, &held, &NullEmitter)
                .await
        );
        assert_eq!(table.views(None)["a"].task_id, None);
    }
}
