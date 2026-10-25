//! The state hub: numbers every state change and publishes it.
//!
//! States themselves are the declaration's ([`arkitekt_spec::state`]):
//! typed cells, changed through [`StateMut`]. The hub gives each change an
//! agent-wide `global_rev`, emits it as `STATE_PATCH` (with a full
//! `STATE_SNAPSHOT` every [`SNAPSHOT_INTERVAL`] revisions) and persists the
//! history to a [`Sink`] in the background.

#[allow(unused_imports)]
use crate::prelude::*;
use crate::sync::Mutex;

use crate::collections::IndexMap;
use async_trait::async_trait;
use serde_json::{Map, Value};

pub use arkitekt_spec::state::{
    Mutation, StateCells, StateDeclaration, StateError, StateMut, StatePatch, StatePublisher,
    StateRef, StateType,
};

use crate::emit::Emitter;
use crate::messages::FromAgent;

/// Publish a full snapshot of every state every this many revisions.
pub const SNAPSHOT_INTERVAL: u64 = 60;

/// A published patch, as handed to a [`Sink`].
#[derive(Debug, Clone, PartialEq)]
pub struct PublishedPatch {
    pub session_id: String,
    pub global_rev: u64,
    pub state_name: String,
    pub ts: f64,
    pub op: String,
    pub path: String,
    pub value: Value,
    pub task_id: Option<String>,
}

/// Where state history goes (the served app keeps it in SQLite).
#[async_trait]
pub trait Sink: Send + Sync + 'static {
    /// Start a session; returns its id.
    async fn create_session(&self) -> anyhow::Result<String>;
    async fn dump_snapshot(
        &self,
        session_id: &str,
        global_rev: u64,
        snapshots: &Map<String, Value>,
    ) -> anyhow::Result<()>;
    async fn write_patch(&self, patch: &PublishedPatch) -> anyhow::Result<()>;
    async fn is_caught_up_to(&self, global_rev: u64) -> anyhow::Result<bool>;
}

enum SinkWrite {
    Snapshot {
        session_id: String,
        global_rev: u64,
        snapshots: Map<String, Value>,
    },
    Patch(PublishedPatch),
}

struct HubState {
    session_id: String,
    global_rev: u64,
    /// The published value of every state, in declaration order.
    shrunk: IndexMap<String, Value>,
    /// Set when the session starts; changes before that are not published.
    emitter: Option<Arc<dyn Emitter>>,
    sink: Option<async_channel::Sender<SinkWrite>>,
}

/// Numbers and publishes changes. Publishing is synchronous, so a change is
/// reported before whatever the task does next (e.g. its YIELD); only
/// persisting to the sink happens in the background.
pub(crate) struct HubInner {
    state: Mutex<HubState>,
    /// Sink writes queued but not yet done.
    pending: core::sync::atomic::AtomicUsize,
    /// Set whenever `pending` drops to zero.
    drained: arkitekt_spec::rt::Watch<()>,
}

impl StatePublisher for HubInner {
    fn publish(&self, patch: StatePatch) {
        let mut state = self.state.lock();
        let Some(emitter) = state.emitter.clone() else {
            return;
        };
        state.global_rev += 1;
        let rev = state.global_rev;
        if let Some(doc) = state.shrunk.get_mut(&patch.state) {
            apply_op(doc, &patch.op, &patch.path, &patch.value);
        }

        if rev.is_multiple_of(SNAPSHOT_INTERVAL) {
            let snapshots: Map<String, Value> = state
                .shrunk
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            emitter.emit(
                FromAgent::StateSnapshot {
                    session_id: state.session_id.clone(),
                    global_rev: rev,
                    snapshots: snapshots.clone(),
                },
                None,
            );
            self.persist(
                &state,
                SinkWrite::Snapshot {
                    session_id: state.session_id.clone(),
                    global_rev: rev,
                    snapshots,
                },
            );
        }

        let published = PublishedPatch {
            session_id: state.session_id.clone(),
            global_rev: rev,
            state_name: patch.state,
            ts: patch.ts,
            op: patch.op,
            path: patch.path,
            value: patch.value,
            task_id: patch.task_id,
        };
        emitter.emit(
            FromAgent::StatePatch {
                session_id: published.session_id.clone(),
                global_rev: rev,
                state_name: published.state_name.clone(),
                ts: published.ts,
                op: published.op.clone(),
                path: published.path.clone(),
                value: published.value.clone(),
                old_value: Value::Null,
                task_id: published.task_id.clone(),
            },
            None,
        );
        self.persist(&state, SinkWrite::Patch(published));
    }
}

impl HubInner {
    fn persist(&self, state: &HubState, write: SinkWrite) {
        if let Some(tx) = &state.sink {
            self.pending
                .fetch_add(1, core::sync::atomic::Ordering::SeqCst);
            if tx.try_send(write).is_err() {
                self.done();
            }
        }
    }

    fn done(&self) {
        if self
            .pending
            .fetch_sub(1, core::sync::atomic::Ordering::SeqCst)
            == 1
        {
            self.drained.set(());
        }
    }
}

/// All states of an agent.
pub struct StateHub {
    cells: Arc<StateCells>,
    inner: Arc<HubInner>,
}

impl core::fmt::Debug for StateHub {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StateHub")
            .field(
                "states",
                &self
                    .cells
                    .declarations()
                    .iter()
                    .map(|d| &d.name)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl StateHub {
    pub fn new(declarations: Vec<StateDeclaration>) -> Self {
        Self {
            cells: Arc::new(StateCells::new(declarations)),
            inner: Arc::new(HubInner {
                state: Mutex::new(HubState {
                    session_id: String::new(),
                    global_rev: 0,
                    shrunk: IndexMap::default(),
                    emitter: None,
                    sink: None,
                }),
                pending: Default::default(),
                drained: arkitekt_spec::rt::Watch::new(()),
            }),
        }
    }

    /// The states' values, typed (what hooks and tasks change).
    pub fn cells(&self) -> &Arc<StateCells> {
        &self.cells
    }

    pub fn declarations(&self) -> &[StateDeclaration] {
        self.cells.declarations()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Set a state's value without publishing (startup).
    pub fn init<T: StateType>(&self, value: T) -> Result<(), StateError> {
        self.cells.init(value)
    }

    pub fn state_mut<T: StateType>(&self, mutation: Mutation) -> Result<StateMut<T>, StateError> {
        self.cells.state_mut(mutation)
    }

    pub fn state_ref<T: StateType>(&self) -> Result<StateRef<T>, StateError> {
        self.cells.state_ref()
    }

    /// Apply the declared initial values.
    pub(crate) fn apply_initial_values(&self) -> Result<(), StateError> {
        self.cells.apply_initial_values()
    }

    /// The current session id and revision.
    pub fn revision(&self) -> (String, u64) {
        let state = self.inner.state.lock();
        (state.session_id.clone(), state.global_rev)
    }

    /// The published value of a state (None if it has none yet).
    pub fn value(&self, name: &str) -> Option<Value> {
        self.inner.state.lock().shrunk.get(name).cloned()
    }

    /// Open a session: check every state has a value, publish SESSION_INIT
    /// (and persist the baseline), then publish every change from revision 1.
    pub(crate) async fn start_session(
        &self,
        session_id: String,
        emitter: Arc<dyn Emitter>,
        sink: Option<Arc<dyn Sink>>,
    ) -> Result<(), StateError> {
        let missing = self.cells.uninitialized();
        if !missing.is_empty() {
            return Err(StateError::Uninitialized(format!(
                "Registered states are missing initialization values from startup hooks: {}",
                missing.join(", ")
            )));
        }

        let baseline: Map<String, Value> = self.cells.values();

        // Sent even without states (`states: {}`), as in Python: it opens the session.
        emitter.emit(
            FromAgent::SessionInit {
                session_id: session_id.clone(),
                states: baseline.clone(),
            },
            None,
        );
        if let Some(sink) = &sink {
            if let Err(e) = sink.dump_snapshot(&session_id, 0, &baseline).await {
                tracing::warn!("could not persist the session baseline: {e:#}");
            }
        }

        let sink_tx = sink.map(|sink| {
            let (tx, rx) = async_channel::unbounded::<SinkWrite>();
            let inner = self.inner.clone();
            arkitekt_spec::rt::spawn(async move {
                while let Ok(write) = rx.recv().await {
                    let result = match &write {
                        SinkWrite::Snapshot {
                            session_id,
                            global_rev,
                            snapshots,
                        } => sink.dump_snapshot(session_id, *global_rev, snapshots).await,
                        SinkWrite::Patch(patch) => sink.write_patch(patch).await,
                    };
                    if let Err(e) = result {
                        tracing::warn!("could not persist state history: {e:#}");
                    }
                    inner.done();
                }
            });
            tx
        });

        {
            let mut state = self.inner.state.lock();
            state.session_id = session_id;
            state.global_rev = 0;
            state.shrunk = baseline.into_iter().collect();
            state.emitter = Some(emitter);
            state.sink = sink_tx;
        }
        self.cells.bind(self.inner.clone());
        Ok(())
    }

    /// Wait (bounded) until every change has been persisted.
    pub async fn flush(&self, timeout: core::time::Duration) {
        let wait = self.inner.drained.wait_for(|()| {
            self.inner
                .pending
                .load(core::sync::atomic::Ordering::SeqCst)
                == 0
        });
        if arkitekt_spec::rt::timeout(timeout, wait).await.is_err() {
            tracing::warn!("state history was still being persisted after {timeout:?}");
        }
    }
}

/// Apply one RFC 6902 operation to `doc`, ignoring failures (the cache is
/// rebuilt from the cells at the next session).
pub(crate) fn apply_op(doc: &mut Value, op: &str, path: &str, value: &Value) {
    if let Err(e) = arkitekt_spec::patch::apply(doc, op, path, value) {
        tracing::warn!("could not apply {op} {path}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit::Emitter;
    use arkitekt_spec::actions::Port;
    use arkitekt_spec::TaskGate;
    use serde::{Deserialize, Serialize};
    use std::sync::Mutex as StdMutex;

    #[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
    struct Camera {
        exposure_ms: f64,
        tags: Vec<String>,
    }

    impl StateType for Camera {
        const NAME: &'static str = "Camera";
        const REQUIRED_LOCKS: &'static [&'static str] = &["camera"];
        fn ports() -> Vec<Port> {
            vec![]
        }
    }

    #[derive(Default)]
    struct Recorder(StdMutex<Vec<FromAgent>>);

    impl Emitter for Recorder {
        fn emit(&self, message: FromAgent, _: Option<&str>) {
            self.0.lock().unwrap().push(message);
        }
    }

    #[tokio::test]
    async fn publishes_numbered_patches() {
        let hub = StateHub::new(vec![StateDeclaration::of(Some(Camera {
            exposure_ms: 10.0,
            tags: vec![],
        }))]);
        hub.apply_initial_values().unwrap();
        let recorder = Arc::new(Recorder::default());
        hub.start_session("s".into(), recorder.clone(), None)
            .await
            .unwrap();

        let task = Mutation {
            task_id: Some("t1".into()),
            locks: Some(vec!["camera".into()]),
            gate: None,
        };
        let camera = hub.state_mut::<Camera>(task).unwrap();
        camera.update(|c| c.exposure_ms = 20.0).unwrap();
        camera.update(|c| c.tags.push("a".into())).unwrap();
        hub.flush(core::time::Duration::from_secs(1)).await;

        let messages = recorder.0.lock().unwrap().clone();
        assert!(
            matches!(&messages[0], FromAgent::SessionInit { states, .. } if states["Camera"]["exposure_ms"] == 10.0)
        );
        let patches: Vec<(u64, String, String, Value)> = messages
            .iter()
            .filter_map(|m| match m {
                FromAgent::StatePatch {
                    global_rev,
                    op,
                    path,
                    value,
                    task_id,
                    ..
                } => {
                    assert_eq!(task_id.as_deref(), Some("t1"));
                    Some((*global_rev, op.clone(), path.clone(), value.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            patches,
            vec![
                (
                    1,
                    "replace".into(),
                    "/exposure_ms".into(),
                    serde_json::json!(20.0)
                ),
                (2, "add".into(), "/tags/0".into(), serde_json::json!("a")),
            ]
        );
        assert_eq!(
            hub.value("Camera").unwrap()["tags"],
            serde_json::json!(["a"])
        );
        assert_eq!(hub.revision().1, 2);
    }

    #[tokio::test]
    async fn enforces_required_locks() {
        let hub = StateHub::new(vec![StateDeclaration::of(Some(Camera {
            exposure_ms: 1.0,
            tags: vec![],
        }))]);
        hub.apply_initial_values().unwrap();
        hub.start_session("s".into(), Arc::new(Recorder::default()), None)
            .await
            .unwrap();
        let unlocked = hub.state_mut::<Camera>(Mutation::unlocked()).unwrap();
        let err = unlocked.update(|c| c.exposure_ms = 2.0).unwrap_err();
        assert!(matches!(err, StateError::MissingLocks { .. }));
        assert_eq!(
            unlocked.get().exposure_ms,
            1.0,
            "a refused change is not applied"
        );
    }

    #[tokio::test]
    async fn refuses_changes_after_the_task_ended() {
        let hub = StateHub::new(vec![StateDeclaration::of(Some(Camera {
            exposure_ms: 1.0,
            tags: vec![],
        }))]);
        hub.apply_initial_values().unwrap();
        let recorder = Arc::new(Recorder::default());
        hub.start_session("s".into(), recorder.clone(), None)
            .await
            .unwrap();
        let gate = TaskGate::default();
        let camera = hub
            .state_mut::<Camera>(Mutation {
                task_id: Some("t".into()),
                locks: Some(vec!["camera".into()]),
                gate: Some(gate.clone()),
            })
            .unwrap();
        camera.update(|c| c.exposure_ms = 2.0).unwrap();
        gate.close();
        assert_eq!(
            camera.update(|c| c.exposure_ms = 3.0).unwrap_err(),
            StateError::TaskClosed
        );
        assert_eq!(
            camera.get().exposure_ms,
            2.0,
            "a refused change is not applied"
        );
        assert_eq!(hub.revision().1, 1);
    }

    #[tokio::test]
    async fn refuses_to_start_without_values() {
        let hub = StateHub::new(vec![StateDeclaration::of::<Camera>(None)]);
        let err = hub
            .start_session("s".into(), Arc::new(Recorder::default()), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing initialization values"));
    }

    #[tokio::test]
    async fn snapshots_every_interval() {
        let hub = StateHub::new(vec![StateDeclaration::of(Some(Camera {
            exposure_ms: 0.0,
            tags: vec![],
        }))]);
        hub.apply_initial_values().unwrap();
        let recorder = Arc::new(Recorder::default());
        hub.start_session("s".into(), recorder.clone(), None)
            .await
            .unwrap();
        let camera = hub.state_mut::<Camera>(Mutation::default()).unwrap();
        for i in 1..=SNAPSHOT_INTERVAL {
            camera.update(|c| c.exposure_ms = i as f64).unwrap();
        }
        hub.flush(core::time::Duration::from_secs(2)).await;
        let messages = recorder.0.lock().unwrap().clone();
        let snapshot_at = messages
            .iter()
            .position(|m| matches!(m, FromAgent::StateSnapshot { global_rev: 60, .. }))
            .expect("a snapshot at 60");
        // The snapshot precedes the patch with the same revision and already contains it.
        assert!(matches!(
            &messages[snapshot_at + 1],
            FromAgent::StatePatch { global_rev: 60, .. }
        ));
        assert!(
            matches!(&messages[snapshot_at], FromAgent::StateSnapshot { snapshots, .. } if snapshots["Camera"]["exposure_ms"] == 60.0)
        );
    }
}
