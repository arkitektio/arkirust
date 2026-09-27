//! Agent state: typed values that every change of is published as JSON patches.
//!
//! A state is a plain struct (`#[derive(State)]`). Actions take a
//! [`StateMut<T>`] to change it or a [`StateRef<T>`] to read it; every change
//! made through [`StateMut::update`] is diffed into RFC 6902 operations,
//! numbered with an agent-wide `global_rev`, and published as `STATE_PATCH`.
//!
//! Anyone observing the agent (the rekuest server, or subscribers of a served
//! app) therefore always knows every state. Reading state is not what actions
//! are for: do not write "getter" actions that only return a state.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::sync::mpsc;

use crate::emit::Emitter;
use crate::messages::FromAgent;
use crate::ports::Port;

/// Publish a full snapshot of every state every this many revisions.
pub const SNAPSHOT_INTERVAL: u64 = 60;

/// A type that can be an agent state. Implemented by `#[derive(State)]`.
pub trait StateType: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {
    /// The state's interface, e.g. `CameraState`.
    const NAME: &'static str;
    /// Locks an action must hold to change this state.
    const REQUIRED_LOCKS: &'static [&'static str];
    /// One (return) port per field.
    fn ports() -> Vec<Port>;
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum StateError {
    #[error("Cannot modify state '{state}' without required locks: {locks:?}")]
    MissingLocks { state: String, locks: Vec<String> },
    #[error("state '{0}' is not declared on this app")]
    Undeclared(String),
    #[error("state '{0}' has no value yet")]
    Uninitialized(String),
    #[error("state '{0}' is read-only here")]
    ReadOnly(String),
    #[error("state '{state}' could not be serialized: {message}")]
    Serialize { state: String, message: String },
    #[error("this task is not running on an agent, so it has no states")]
    NoAgent,
}

/// Who changes a state: which task (if any) and which locks it holds.
/// `locks: None` means unrestricted (initialization).
#[derive(Debug, Clone, Default)]
pub struct Mutation {
    pub task_id: Option<String>,
    pub locks: Option<Vec<String>>,
}

impl Mutation {
    /// A change made outside a task (background workers, hooks): no locks.
    pub fn unlocked() -> Self {
        Self {
            task_id: None,
            locks: Some(vec![]),
        }
    }
}

/// A state's declaration, as registered.
#[derive(Clone)]
pub struct StateDeclaration {
    pub name: String,
    pub ports: Vec<Port>,
    pub required_locks: Vec<String>,
    make_cell: Arc<dyn Fn() -> Arc<dyn AnyCell> + Send + Sync>,
    initial: Option<Arc<dyn Fn() -> Box<dyn Any + Send> + Send + Sync>>,
}

impl std::fmt::Debug for StateDeclaration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateDeclaration")
            .field("name", &self.name)
            .field("required_locks", &self.required_locks)
            .finish_non_exhaustive()
    }
}

impl StateDeclaration {
    pub fn of<T: StateType>(initial: Option<T>) -> Self {
        Self {
            name: T::NAME.to_owned(),
            ports: T::ports().into_iter().map(Port::into_return).collect(),
            required_locks: T::REQUIRED_LOCKS.iter().map(|s| s.to_string()).collect(),
            make_cell: Arc::new(|| Arc::new(StateCell::<T>::new()) as Arc<dyn AnyCell>),
            initial: initial.map(|value| {
                Arc::new(move || Box::new(value.clone()) as Box<dyn Any + Send>)
                    as Arc<dyn Fn() -> Box<dyn Any + Send> + Send + Sync>
            }),
        }
    }

    /// The `StateImplementationInput` sent in REGISTER.
    pub fn to_declaration_json(&self) -> Value {
        serde_json::json!({
            "interface": self.name,
            "definition": { "ports": self.ports, "name": self.name },
        })
    }
}

/// Type-erased access to a state cell.
pub(crate) trait AnyCell: Send + Sync + 'static {
    fn as_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
    fn set_boxed(&self, value: Box<dyn Any + Send>) -> Result<(), StateError>;
    fn value_json(&self) -> Option<Value>;
    fn bind(&self, hub: Arc<HubInner>);
}

pub(crate) struct StateCell<T> {
    value: Mutex<Option<T>>,
    /// The JSON of `value` as of the last published change.
    json: Mutex<Value>,
    hub: Mutex<Option<Arc<HubInner>>>,
}

impl<T: StateType> StateCell<T> {
    fn new() -> Self {
        Self {
            value: Mutex::new(None),
            json: Mutex::new(Value::Null),
            hub: Mutex::new(None),
        }
    }

    fn to_json(value: &T) -> Result<Value, StateError> {
        serde_json::to_value(value).map_err(|e| StateError::Serialize {
            state: T::NAME.to_owned(),
            message: e.to_string(),
        })
    }

    /// Replace the value without publishing (initialization).
    fn set(&self, value: T) -> Result<(), StateError> {
        let json = Self::to_json(&value)?;
        *self.value.lock().expect("state lock") = Some(value);
        *self.json.lock().expect("state lock") = json;
        Ok(())
    }

    fn get(&self) -> Result<T, StateError> {
        self.value
            .lock()
            .expect("state lock")
            .clone()
            .ok_or_else(|| StateError::Uninitialized(T::NAME.to_owned()))
    }

    fn read<R>(&self, f: impl FnOnce(&T) -> R) -> Result<R, StateError> {
        let guard = self.value.lock().expect("state lock");
        let value = guard
            .as_ref()
            .ok_or_else(|| StateError::Uninitialized(T::NAME.to_owned()))?;
        Ok(f(value))
    }

    fn update<R>(&self, mutation: &Mutation, f: impl FnOnce(&mut T) -> R) -> Result<R, StateError> {
        if let Some(held) = &mutation.locks {
            let missing: Vec<String> = T::REQUIRED_LOCKS
                .iter()
                .filter(|l| !held.iter().any(|h| h == *l))
                .map(|l| l.to_string())
                .collect();
            if !missing.is_empty() {
                return Err(StateError::MissingLocks {
                    state: T::NAME.to_owned(),
                    locks: T::REQUIRED_LOCKS.iter().map(|s| s.to_string()).collect(),
                });
            }
        }

        // The value lock is held until the operations are queued, so changes
        // to one state are published in the order they were made.
        let mut guard = self.value.lock().expect("state lock");
        let value = guard
            .as_mut()
            .ok_or_else(|| StateError::Uninitialized(T::NAME.to_owned()))?;
        let result = f(value);
        let new_json = Self::to_json(value)?;

        let mut json = self.json.lock().expect("state lock");
        let ops = diff_ops(&json, &new_json);
        *json = new_json;
        drop(json);

        if !ops.is_empty() {
            if let Some(hub) = self.hub.lock().expect("state lock").as_ref() {
                let ts = now_ts();
                for (op, path, value) in ops {
                    hub.queue(QueuedPatch {
                        state: T::NAME.to_owned(),
                        op,
                        path,
                        value,
                        task_id: mutation.task_id.clone(),
                        ts,
                    });
                }
            }
        }
        drop(guard);
        Ok(result)
    }
}

impl<T: StateType> AnyCell for StateCell<T> {
    fn as_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn set_boxed(&self, value: Box<dyn Any + Send>) -> Result<(), StateError> {
        let value = value
            .downcast::<T>()
            .map_err(|_| StateError::Undeclared(T::NAME.to_owned()))?;
        self.set(*value)
    }

    fn value_json(&self) -> Option<Value> {
        self.value
            .lock()
            .expect("state lock")
            .is_some()
            .then(|| self.json.lock().expect("state lock").clone())
    }

    fn bind(&self, hub: Arc<HubInner>) {
        *self.hub.lock().expect("state lock") = Some(hub);
    }
}

/// `(op, path, value)` for each RFC 6902 operation turning `old` into `new`.
fn diff_ops(old: &Value, new: &Value) -> Vec<(String, String, Value)> {
    let patch = json_patch::diff(old, new);
    let Ok(Value::Array(ops)) = serde_json::to_value(&patch) else {
        return vec![];
    };
    ops.into_iter()
        .filter_map(|op| {
            let kind = op.get("op")?.as_str()?.to_owned();
            let path = op.get("path")?.as_str()?.to_owned();
            let value = if kind == "remove" {
                Value::Null
            } else {
                op.get("value").cloned().unwrap_or(Value::Null)
            };
            Some((kind, path, value))
        })
        .collect()
}

fn now_ts() -> f64 {
    let now = chrono::Utc::now();
    now.timestamp() as f64 + f64::from(now.timestamp_subsec_micros()) / 1e6
}

/// A change waiting for its revision number.
#[derive(Debug, Clone)]
pub(crate) struct QueuedPatch {
    state: String,
    op: String,
    path: String,
    value: Value,
    task_id: Option<String>,
    ts: f64,
}

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
    async fn dump_snapshot(&self, session_id: &str, global_rev: u64, snapshots: &Map<String, Value>) -> anyhow::Result<()>;
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
    sink: Option<mpsc::UnboundedSender<SinkWrite>>,
}

/// Numbers and publishes changes. Publishing is synchronous, so a change is
/// reported before whatever the task does next (e.g. its YIELD); only
/// persisting to the sink happens in the background.
pub(crate) struct HubInner {
    state: Mutex<HubState>,
    /// Sink writes queued but not yet done.
    pending: std::sync::atomic::AtomicUsize,
    /// Woken whenever `pending` drops to zero.
    drained: tokio::sync::Notify,
}

impl HubInner {
    fn queue(&self, patch: QueuedPatch) {
        let mut state = self.state.lock().expect("hub lock");
        let Some(emitter) = state.emitter.clone() else {
            return;
        };
        state.global_rev += 1;
        let rev = state.global_rev;
        if let Some(doc) = state.shrunk.get_mut(&patch.state) {
            apply_op(doc, &patch.op, &patch.path, &patch.value);
        }

        if rev.is_multiple_of(SNAPSHOT_INTERVAL) {
            let snapshots: Map<String, Value> =
                state.shrunk.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
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

    fn persist(&self, state: &HubState, write: SinkWrite) {
        if let Some(tx) = &state.sink {
            self.pending.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if tx.send(write).is_err() {
                self.done();
            }
        }
    }

    fn done(&self) {
        if self.pending.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            self.drained.notify_waiters();
        }
    }
}

/// All states of an agent.
pub struct StateHub {
    declarations: Vec<StateDeclaration>,
    cells: HashMap<String, Arc<dyn AnyCell>>,
    inner: Arc<HubInner>,
}

impl std::fmt::Debug for StateHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateHub")
            .field("states", &self.declarations.iter().map(|d| &d.name).collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl StateHub {
    pub fn new(declarations: Vec<StateDeclaration>) -> Self {
        let cells: HashMap<String, Arc<dyn AnyCell>> = declarations
            .iter()
            .map(|d| (d.name.clone(), (d.make_cell)()))
            .collect();
        Self {
            declarations,
            cells,
            inner: Arc::new(HubInner {
                state: Mutex::new(HubState {
                    session_id: String::new(),
                    global_rev: 0,
                    shrunk: IndexMap::new(),
                    emitter: None,
                    sink: None,
                }),
                pending: Default::default(),
                drained: tokio::sync::Notify::new(),
            }),
        }
    }

    pub fn declarations(&self) -> &[StateDeclaration] {
        &self.declarations
    }

    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }

    fn cell<T: StateType>(&self) -> Result<Arc<StateCell<T>>, StateError> {
        let cell = self
            .cells
            .get(T::NAME)
            .ok_or_else(|| StateError::Undeclared(T::NAME.to_owned()))?;
        cell.clone()
            .as_any()
            .downcast::<StateCell<T>>()
            .map_err(|_| StateError::Undeclared(T::NAME.to_owned()))
    }

    /// Set a state's value without publishing (startup).
    pub fn init<T: StateType>(&self, value: T) -> Result<(), StateError> {
        self.cell::<T>()?.set(value)
    }

    pub fn state_mut<T: StateType>(&self, mutation: Mutation) -> Result<StateMut<T>, StateError> {
        Ok(StateMut {
            cell: self.cell::<T>()?,
            mutation,
        })
    }

    pub fn state_ref<T: StateType>(&self) -> Result<StateRef<T>, StateError> {
        Ok(StateRef { cell: self.cell::<T>()? })
    }

    /// Apply the declared initial values.
    pub(crate) fn apply_initial_values(&self) -> Result<(), StateError> {
        for declaration in &self.declarations {
            if let Some(initial) = &declaration.initial {
                self.cells[&declaration.name].set_boxed(initial())?;
            }
        }
        Ok(())
    }

    /// The current session id and revision.
    pub fn revision(&self) -> (String, u64) {
        let state = self.inner.state.lock().expect("hub lock");
        (state.session_id.clone(), state.global_rev)
    }

    /// The published value of a state (None if it has none yet).
    pub fn value(&self, name: &str) -> Option<Value> {
        self.inner.state.lock().expect("hub lock").shrunk.get(name).cloned()
    }

    /// Open a session: check every state has a value, publish SESSION_INIT
    /// (and persist the baseline), then publish every change from revision 1.
    pub(crate) async fn start_session(
        &self,
        session_id: String,
        emitter: Arc<dyn Emitter>,
        sink: Option<Arc<dyn Sink>>,
    ) -> Result<(), StateError> {
        let missing: Vec<&str> = self
            .declarations
            .iter()
            .filter(|d| self.cells[&d.name].value_json().is_none())
            .map(|d| d.name.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(StateError::Uninitialized(format!(
                "Registered states are missing initialization values from startup hooks: {}",
                missing.join(", ")
            )));
        }

        let baseline: Map<String, Value> = self
            .declarations
            .iter()
            .map(|d| (d.name.clone(), self.cells[&d.name].value_json().unwrap_or(Value::Null)))
            .collect();

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
            let (tx, mut rx) = mpsc::unbounded_channel::<SinkWrite>();
            let inner = self.inner.clone();
            tokio::spawn(async move {
                while let Some(write) = rx.recv().await {
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
            let mut state = self.inner.state.lock().expect("hub lock");
            state.session_id = session_id;
            state.global_rev = 0;
            state.shrunk = baseline.into_iter().collect();
            state.emitter = Some(emitter);
            state.sink = sink_tx;
        }
        for cell in self.cells.values() {
            cell.bind(self.inner.clone());
        }
        Ok(())
    }

    /// Wait (bounded) until every change has been persisted.
    pub async fn flush(&self, timeout: std::time::Duration) {
        let wait = async {
            loop {
                let drained = self.inner.drained.notified();
                if self.inner.pending.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                    return;
                }
                drained.await;
            }
        };
        if tokio::time::timeout(timeout, wait).await.is_err() {
            tracing::warn!("state history was still being persisted after {timeout:?}");
        }
    }
}

/// Apply one RFC 6902 operation to `doc`, ignoring failures (the cache is
/// rebuilt from the cells at the next session).
pub(crate) fn apply_op(doc: &mut Value, op: &str, path: &str, value: &Value) {
    let mut operation = serde_json::json!({ "op": op, "path": path });
    if op != "remove" {
        operation["value"] = value.clone();
    }
    match serde_json::from_value::<json_patch::Patch>(Value::Array(vec![operation])) {
        Ok(patch) => {
            if let Err(e) = json_patch::patch(doc, &patch) {
                tracing::warn!("could not apply {op} {path}: {e}");
            }
        }
        Err(e) => tracing::warn!("invalid patch {op} {path}: {e}"),
    }
}

/// A handle that changes a state. Take it as an action parameter:
/// `camera: StateMut<CameraState>`.
pub struct StateMut<T: StateType> {
    cell: Arc<StateCell<T>>,
    mutation: Mutation,
}

impl<T: StateType> Clone for StateMut<T> {
    fn clone(&self) -> Self {
        Self {
            cell: self.cell.clone(),
            mutation: self.mutation.clone(),
        }
    }
}

impl<T: StateType> std::fmt::Debug for StateMut<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateMut").field("state", &T::NAME).finish()
    }
}

impl<T: StateType> StateMut<T> {
    /// A copy of the current value.
    pub fn get(&self) -> T {
        self.cell.get().expect("states are initialized before actions run")
    }

    /// Read without copying.
    pub fn read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.cell.read(f).expect("states are initialized before actions run")
    }

    /// Change the state. The difference is published as `STATE_PATCH`es,
    /// attributed to the task this handle belongs to.
    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> R) -> Result<R, StateError> {
        self.cell.update(&self.mutation, f)
    }

    /// Replace the whole value.
    pub fn set(&self, value: T) -> Result<(), StateError> {
        self.update(|current| *current = value)
    }
}

/// A read-only handle: `camera: StateRef<CameraState>`.
pub struct StateRef<T: StateType> {
    cell: Arc<StateCell<T>>,
}

impl<T: StateType> Clone for StateRef<T> {
    fn clone(&self) -> Self {
        Self { cell: self.cell.clone() }
    }
}

impl<T: StateType> std::fmt::Debug for StateRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateRef").field("state", &T::NAME).finish()
    }
}

impl<T: StateType> StateRef<T> {
    pub fn get(&self) -> T {
        self.cell.get().expect("states are initialized before actions run")
    }

    pub fn read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.cell.read(f).expect("states are initialized before actions run")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit::Emitter;
    use serde::Deserialize;
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
        hub.start_session("s".into(), recorder.clone(), None).await.unwrap();

        let task = Mutation {
            task_id: Some("t1".into()),
            locks: Some(vec!["camera".into()]),
        };
        let camera = hub.state_mut::<Camera>(task).unwrap();
        camera.update(|c| c.exposure_ms = 20.0).unwrap();
        camera.update(|c| c.tags.push("a".into())).unwrap();
        hub.flush(std::time::Duration::from_secs(1)).await;

        let messages = recorder.0.lock().unwrap().clone();
        assert!(matches!(&messages[0], FromAgent::SessionInit { states, .. } if states["Camera"]["exposure_ms"] == 10.0));
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
                (1, "replace".into(), "/exposure_ms".into(), serde_json::json!(20.0)),
                (2, "add".into(), "/tags/0".into(), serde_json::json!("a")),
            ]
        );
        assert_eq!(hub.value("Camera").unwrap()["tags"], serde_json::json!(["a"]));
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
        assert_eq!(unlocked.get().exposure_ms, 1.0, "a refused change is not applied");
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
        hub.start_session("s".into(), recorder.clone(), None).await.unwrap();
        let camera = hub.state_mut::<Camera>(Mutation::default()).unwrap();
        for i in 1..=SNAPSHOT_INTERVAL {
            camera.update(|c| c.exposure_ms = i as f64).unwrap();
        }
        hub.flush(std::time::Duration::from_secs(2)).await;
        let messages = recorder.0.lock().unwrap().clone();
        let snapshot_at = messages
            .iter()
            .position(|m| matches!(m, FromAgent::StateSnapshot { global_rev: 60, .. }))
            .expect("a snapshot at 60");
        // The snapshot precedes the patch with the same revision and already contains it.
        assert!(matches!(&messages[snapshot_at + 1], FromAgent::StatePatch { global_rev: 60, .. }));
        assert!(matches!(&messages[snapshot_at], FromAgent::StateSnapshot { snapshots, .. } if snapshots["Camera"]["exposure_ms"] == 60.0));
    }
}
