//! One ordered record of everything an agent reports.
//!
//! Task events (`PROGRESS`, `YIELD`, `COMPLETED`, …), lock changes, state
//! patches, snapshots, the session baseline and (when served) the `ASSIGN`
//! that started a task each get a position `pos` in their session. `pos`
//! starts at 1 with `SESSION_INIT` and has no gaps; `(session_id, pos)` is
//! the durable key. Every entry also carries the state revision `global_rev`
//! as of that entry, so "the world at `pos`" is the state at `global_rev`
//! plus the tasks and locks folded from the entries up to `pos`.
//!
//! The [`Journal`] is an [`Emitter`] in front of the transport. Numbering,
//! queueing for persistence and handing the message on happen under one
//! lock, so the order entries are delivered in is the order they are
//! numbered in, and the order they were made in: a task's patches come
//! before its `YIELD`, its `COMPLETED` before its `UNLOCK`.
//!
//! Nothing is recorded for a task after its end: a [`TaskGate`] is closed
//! when the end is reported, and the task's handles ([`Task`](crate::Task)
//! reports, [`StateMut`](crate::StateMut) changes) refuse to act on a
//! closed gate *before* they change anything.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{mpsc, watch};

use crate::emit::Emitter;
use crate::messages::{Assign, FromAgent};
use crate::state::apply_op;

/// Entries kept in memory for resuming subscribers.
pub const RING_CAPACITY: usize = 4096;
/// Finished tasks kept in the live fold.
const FINISHED_KEEP: usize = 1024;
/// Entries persisted per write.
const WRITE_BATCH: usize = 256;

/// Milliseconds since the epoch.
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// ISO 8601 with `Z`, microseconds only when non-zero (pydantic's format).
pub fn iso_from_ms(ms: i64) -> String {
    let Some(dt) = chrono::DateTime::from_timestamp_millis(ms) else {
        return String::new();
    };
    if dt.timestamp_subsec_micros() == 0 {
        dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    } else {
        dt.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
    }
}

// ------------------------------------------------------------------ entry --

/// One recorded fact.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JournalEntry {
    pub session_id: String,
    pub pos: u64,
    /// The state revision after this entry.
    pub global_rev: u64,
    pub timepoint: String,
    #[serde(skip)]
    pub event_time: i64,
    /// The frame `type`: `ASSIGN`, `PROGRESS`, `STATE_PATCH`, …
    pub kind: String,
    /// The task this entry belongs to (for `UNLOCK`: the task that held the lock).
    pub task_id: Option<String>,
    /// 1, 2, 3, … over the entries of `task_id`.
    pub step: Option<u64>,
    pub action_key: Option<String>,
    /// The state (`STATE_PATCH`) or lock key (`LOCK`/`UNLOCK`) it is about.
    pub subject: Option<String>,
    /// The frame's `id`, as sent.
    pub message_id: String,
    /// The frame, without the stream-level `seq`.
    pub payload: Value,
}

/// Who an entry is for, as subscribers filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route<'a> {
    State(&'a str),
    Lock(&'a str),
    Action(&'a str),
    /// Session-wide (`SESSION_INIT`, `STATE_SNAPSHOT`, reports about unknown tasks).
    Everyone,
}

impl JournalEntry {
    /// The payload with `pos` and `journal_session`, as journal subscribers get it.
    pub fn frame(&self) -> Value {
        let mut frame = self.payload.clone();
        stamp(&mut frame, self);
        frame
    }

    pub fn route(&self) -> Route<'_> {
        match (self.kind.as_str(), self.subject.as_deref(), self.action_key.as_deref()) {
            ("STATE_PATCH", Some(state), _) => Route::State(state),
            ("LOCK" | "UNLOCK", Some(key), _) => Route::Lock(key),
            ("STATE_SNAPSHOT" | "SESSION_INIT", _, _) => Route::Everyone,
            (_, _, Some(key)) => Route::Action(key),
            _ => Route::Everyone,
        }
    }

    pub fn is_terminal(&self) -> bool {
        is_terminal_kind(&self.kind)
    }
}

/// Add `pos` and `journal_session` to a frame. (Not `session_id`: state
/// frames already have one, and it would clash.)
pub fn stamp(frame: &mut Value, entry: &JournalEntry) {
    if let Value::Object(map) = frame {
        map.insert("pos".into(), Value::from(entry.pos));
        map.insert("journal_session".into(), Value::String(entry.session_id.clone()));
    }
}

pub fn is_terminal_kind(kind: &str) -> bool {
    matches!(kind, "COMPLETED" | "FAILED" | "CRITICAL" | "CANCELLED" | "INTERRUPTED")
}

fn subject_of(message: &FromAgent) -> Option<String> {
    match message {
        FromAgent::StatePatch { state_name, .. } => Some(state_name.clone()),
        FromAgent::Lock { key, .. } | FromAgent::Unlock { key } => Some(key.clone()),
        _ => None,
    }
}

// ------------------------------------------------------------------- fold --

/// A task as the journal knows it at some position.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskFold {
    pub task: String,
    pub action_key: Option<String>,
    pub interface: Option<String>,
    pub reference: Option<String>,
    /// `ASSIGNED`, `RUNNING`, `PAUSED`, or the terminal kind.
    pub status: String,
    pub done: bool,
    pub progress: Option<i64>,
    pub message: Option<String>,
    pub error: Option<String>,
    pub yields: u64,
    pub last_returns: Option<Value>,
    pub first_pos: u64,
    pub last_pos: u64,
}

/// States, tasks and locks, folded from entries in `pos` order.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Fold {
    pub states: IndexMap<String, Value>,
    pub tasks: IndexMap<String, TaskFold>,
    /// Lock key → the task holding it.
    pub locks: IndexMap<String, String>,
    /// The revision the states were last replaced at: a snapshot at N
    /// already contains patch N, which is journaled after it.
    #[serde(skip)]
    pub snapshot_rev: u64,
}

impl Fold {
    pub fn from_entries<'a>(entries: impl IntoIterator<Item = &'a JournalEntry>) -> Self {
        let mut fold = Self::default();
        for entry in entries {
            fold.apply(entry);
        }
        fold
    }

    pub fn apply(&mut self, entry: &JournalEntry) {
        let payload = &entry.payload;
        match entry.kind.as_str() {
            "SESSION_INIT" => {
                self.replace_states(payload.get("states"));
                self.snapshot_rev = 0;
            }
            "STATE_SNAPSHOT" => {
                self.replace_states(payload.get("snapshots"));
                self.snapshot_rev = entry.global_rev;
            }
            "STATE_PATCH" if entry.global_rev > self.snapshot_rev => {
                if let Some(doc) = entry.subject.as_ref().and_then(|s| self.states.get_mut(s)) {
                    apply_op(
                        doc,
                        payload["op"].as_str().unwrap_or_default(),
                        payload["path"].as_str().unwrap_or_default(),
                        payload.get("value").unwrap_or(&Value::Null),
                    );
                }
            }
            "LOCK" => {
                if let (Some(key), Some(task)) = (&entry.subject, &entry.task_id) {
                    self.locks.insert(key.clone(), task.clone());
                }
            }
            "UNLOCK" => {
                if let Some(key) = &entry.subject {
                    self.locks.shift_remove(key);
                }
            }
            _ => {}
        }

        let Some(task_id) = &entry.task_id else { return };
        if matches!(entry.kind.as_str(), "STATE_PATCH" | "LOCK" | "UNLOCK") {
            if let Some(task) = self.tasks.get_mut(task_id) {
                task.last_pos = task.last_pos.max(entry.pos);
            }
            return;
        }
        let task = self.tasks.entry(task_id.clone()).or_insert_with(|| TaskFold {
            task: task_id.clone(),
            action_key: entry.action_key.clone(),
            interface: None,
            reference: None,
            status: "ASSIGNED".into(),
            done: false,
            progress: None,
            message: None,
            error: None,
            yields: 0,
            last_returns: None,
            first_pos: entry.pos,
            last_pos: entry.pos,
        });
        task.last_pos = entry.pos;
        if task.action_key.is_none() {
            task.action_key = entry.action_key.clone();
        }
        if task.done {
            return;
        }
        let text = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_owned);
        match entry.kind.as_str() {
            "ASSIGN" => {
                task.interface = text("interface");
                task.reference = text("reference");
            }
            "PROGRESS" => {
                if let Some(progress) = payload.get("progress").and_then(Value::as_i64) {
                    task.progress = Some(progress);
                }
                if let Some(message) = text("message") {
                    task.message = Some(message);
                }
                task.status = "RUNNING".into();
            }
            "LOG" => {
                task.message = text("message");
                task.status = "RUNNING".into();
            }
            "YIELD" => {
                task.yields += 1;
                task.last_returns = payload.get("returns").cloned();
                task.status = "RUNNING".into();
            }
            "STARTED" | "RESUMED" => task.status = "RUNNING".into(),
            "PAUSED" => task.status = "PAUSED".into(),
            kind if is_terminal_kind(kind) => {
                task.status = kind.to_owned();
                task.done = true;
                task.error = text("error");
            }
            _ => {}
        }
    }

    fn replace_states(&mut self, states: Option<&Value>) {
        if let Some(Value::Object(states)) = states {
            for (name, value) in states {
                self.states.insert(name.clone(), value.clone());
            }
        }
    }
}

// ------------------------------------------------------------------- gate --

#[derive(Default)]
struct GateState {
    closed: bool,
    active: usize,
}

/// Closes a task: after [`TaskGate::close`] returns, nothing more is done
/// for the task. Reports and state changes [`enter`](TaskGate::enter) the
/// gate for their (synchronous) duration; closing refuses new entries and
/// waits for the ones in flight. Entering twice on one thread is fine.
#[derive(Clone, Default)]
pub struct TaskGate {
    inner: Arc<(Mutex<GateState>, Condvar)>,
}

impl std::fmt::Debug for TaskGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskGate").field("closed", &self.is_closed()).finish()
    }
}

/// Held while acting for an open task.
pub struct GatePass<'a> {
    gate: &'a TaskGate,
}

impl Drop for GatePass<'_> {
    fn drop(&mut self) {
        let (lock, done) = &*self.gate.inner;
        let mut state = lock.lock().expect("gate lock");
        state.active -= 1;
        if state.active == 0 {
            done.notify_all();
        }
    }
}

impl TaskGate {
    /// `None` once the gate is closed (or closing).
    pub fn enter(&self) -> Option<GatePass<'_>> {
        let mut state = self.inner.0.lock().expect("gate lock");
        if state.closed {
            return None;
        }
        state.active += 1;
        Some(GatePass { gate: self })
    }

    /// Refuse new entries, then wait until those in flight are done.
    pub fn close(&self) {
        let (lock, done) = &*self.inner;
        let mut state = lock.lock().expect("gate lock");
        state.closed = true;
        while state.active > 0 {
            state = done.wait(state).expect("gate lock");
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.0.lock().expect("gate lock").closed
    }
}

// ---------------------------------------------------------------- journal --

/// Where entries are persisted (the served app keeps them in SQLite).
#[async_trait]
pub trait JournalSink: Send + Sync + 'static {
    /// Entries in `pos` order; a batch never spans sessions.
    async fn write_entries(&self, entries: &[Arc<JournalEntry>]) -> anyhow::Result<()>;
}

/// The last position of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Watermark {
    pub session_id: String,
    pub pos: u64,
    pub global_rev: u64,
}

/// What [`Journal::locked`] hands out: a consistent view at the watermark.
pub struct JournalView<'a> {
    state: &'a JournalState,
}

impl JournalView<'_> {
    /// `None` before the first session.
    pub fn watermark(&self) -> Option<Watermark> {
        self.state.session.as_ref().map(|session| Watermark {
            session_id: session.clone(),
            pos: self.state.pos,
            global_rev: self.state.global_rev,
        })
    }

    /// States, tasks and locks at the watermark (finished tasks are only kept for a while).
    pub fn fold(&self) -> &Fold {
        &self.state.fold
    }

    /// Entries in `(after, until]` if they are still in memory.
    pub fn recent(&self, after: u64, until: u64) -> Option<Vec<Arc<JournalEntry>>> {
        recent(&self.state.ring, after, until)
    }
}

fn recent(ring: &VecDeque<Arc<JournalEntry>>, after: u64, until: u64) -> Option<Vec<Arc<JournalEntry>>> {
    if after >= until {
        return Some(vec![]);
    }
    let first = ring.front()?.pos;
    if first > after + 1 {
        return None;
    }
    Some(ring.iter().filter(|e| e.pos > after && e.pos <= until).cloned().collect())
}

#[derive(Default)]
struct JournalState {
    session: Option<String>,
    pos: u64,
    global_rev: u64,
    ring: VecDeque<Arc<JournalEntry>>,
    fold: Fold,
    finished: VecDeque<String>,
    /// The last step of each task.
    steps: std::collections::HashMap<String, u64>,
    writer: Option<mpsc::UnboundedSender<Arc<JournalEntry>>>,
}

/// Numbers, records and hands on every message. See the module docs.
pub struct Journal {
    inner: Arc<dyn Emitter>,
    state: Mutex<JournalState>,
    sink: Option<Arc<dyn JournalSink>>,
    /// `(session, pos)` persisted so far.
    durable: watch::Sender<(String, u64)>,
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock().expect("journal lock");
        f.debug_struct("Journal")
            .field("session", &state.session)
            .field("pos", &state.pos)
            .finish_non_exhaustive()
    }
}

impl Journal {
    /// A journal in front of `inner`. Without `session`, entries start with
    /// the first `SESSION_INIT`; messages before it are passed on unrecorded.
    pub fn new(inner: Arc<dyn Emitter>, sink: Option<Arc<dyn JournalSink>>, session: Option<String>) -> Self {
        Self {
            inner,
            state: Mutex::new(JournalState {
                session,
                ..Default::default()
            }),
            sink,
            durable: watch::channel((String::new(), 0)).0,
        }
    }

    /// The last position, `None` before the first session.
    pub fn watermark(&self) -> Option<Watermark> {
        self.locked(|view| view.watermark())
    }

    /// Run `f` with the journal locked: nothing is recorded meanwhile, so
    /// what `f` reads is consistent with the watermark, and a subscription
    /// made in `f` gets exactly the entries after it.
    pub fn locked<R>(&self, f: impl FnOnce(&JournalView<'_>) -> R) -> R {
        let state = self.state.lock().expect("journal lock");
        f(&JournalView { state: &state })
    }

    /// Record the assignment that starts a task (secrets removed).
    pub fn record_assign(&self, assign: &Assign, action_key: &str) {
        let mut assign = assign.clone();
        assign.token = None;
        // The frame's `id` is the entry's.
        assign.id = None;
        self.emit(FromAgent::Assign(Box::new(assign)), Some(action_key));
    }

    /// Wait (bounded) until everything up to `pos` of the current session is persisted.
    pub async fn flush_to(&self, pos: u64, timeout: Duration) -> bool {
        let Some(session) = self.state.lock().expect("journal lock").session.clone() else {
            return true;
        };
        if self.sink.is_none() || pos == 0 {
            return true;
        }
        let mut durable = self.durable.subscribe();
        let reached = async move { durable.wait_for(|(s, p)| *s == session && *p >= pos).await.is_ok() };
        match tokio::time::timeout(timeout, reached).await {
            Ok(true) => true,
            _ => {
                tracing::warn!("the journal was not persisted up to {pos} within {timeout:?}");
                false
            }
        }
    }

    /// Wait (bounded) until every entry so far is persisted.
    pub async fn flush(&self, timeout: Duration) -> bool {
        let pos = self.state.lock().expect("journal lock").pos;
        self.flush_to(pos, timeout).await
    }

    fn append(
        &self,
        state: &mut JournalState,
        kind: &str,
        task_id: Option<String>,
        action_key: Option<String>,
        subject: Option<String>,
        mut payload: Value,
    ) -> Option<Arc<JournalEntry>> {
        let session = state.session.clone()?;
        state.pos += 1;
        let step = task_id.as_ref().map(|task| {
            let step = state.steps.entry(task.clone()).or_default();
            *step += 1;
            *step
        });
        let message_id = uuid::Uuid::new_v4().to_string();
        if let Value::Object(map) = &mut payload {
            map.insert("id".into(), Value::String(message_id.clone()));
        }
        let event_time = now_ms();
        let action_key = action_key.or_else(|| {
            task_id
                .as_ref()
                .and_then(|t| state.fold.tasks.get(t))
                .and_then(|t| t.action_key.clone())
        });
        let entry = Arc::new(JournalEntry {
            session_id: session,
            pos: state.pos,
            global_rev: state.global_rev,
            timepoint: iso_from_ms(event_time),
            event_time,
            kind: kind.to_owned(),
            task_id,
            step,
            action_key,
            subject,
            message_id,
            payload,
        });

        state.fold.apply(&entry);
        if entry.is_terminal() {
            if let Some(task) = &entry.task_id {
                state.finished.push_back(task.clone());
                if state.finished.len() > FINISHED_KEEP {
                    if let Some(old) = state.finished.pop_front() {
                        state.fold.tasks.shift_remove(&old);
                        state.steps.remove(&old);
                    }
                }
            }
        }
        state.ring.push_back(entry.clone());
        if state.ring.len() > RING_CAPACITY {
            state.ring.pop_front();
        }
        self.persist(state, entry.clone());
        Some(entry)
    }

    fn persist(&self, state: &mut JournalState, entry: Arc<JournalEntry>) {
        let Some(sink) = &self.sink else { return };
        if state.writer.is_none() {
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                tracing::warn!("no async runtime; the journal is not persisted");
                return;
            };
            let (tx, rx) = mpsc::unbounded_channel();
            runtime.spawn(write_loop(sink.clone(), rx, self.durable.clone()));
            state.writer = Some(tx);
        }
        if let Some(writer) = &state.writer {
            let _ = writer.send(entry);
        }
    }
}

async fn write_loop(
    sink: Arc<dyn JournalSink>,
    mut rx: mpsc::UnboundedReceiver<Arc<JournalEntry>>,
    durable: watch::Sender<(String, u64)>,
) {
    let mut carry: Option<Arc<JournalEntry>> = None;
    loop {
        let first = match carry.take() {
            Some(entry) => entry,
            None => match rx.recv().await {
                Some(entry) => entry,
                None => return,
            },
        };
        let mut batch = vec![first];
        while batch.len() < WRITE_BATCH {
            match rx.try_recv() {
                Ok(entry) if entry.session_id == batch[0].session_id => batch.push(entry),
                Ok(entry) => {
                    carry = Some(entry);
                    break;
                }
                Err(_) => break,
            }
        }
        if let Err(e) = sink.write_entries(&batch).await {
            tracing::warn!("could not persist the journal: {e:#}");
        }
        let last = batch.last().expect("a batch is never empty");
        durable.send_replace((last.session_id.clone(), last.pos));
    }
}

impl Emitter for Journal {
    fn emit(&self, mut message: FromAgent, action_key: Option<&str>) {
        let mut state = self.state.lock().expect("journal lock");
        match &message {
            FromAgent::Register { .. } | FromAgent::HeartbeatAnswer {} => {
                drop(state);
                return self.inner.emit(message, action_key);
            }
            FromAgent::SessionInit { session_id, .. } => {
                if state.session.as_deref() != Some(session_id.as_str()) {
                    state.session = Some(session_id.clone());
                    state.pos = 0;
                    state.ring.clear();
                    state.fold = Fold::default();
                    state.finished.clear();
                    state.steps.clear();
                }
                state.global_rev = 0;
            }
            FromAgent::StatePatch { global_rev, .. } | FromAgent::StateSnapshot { global_rev, .. } => {
                state.global_rev = *global_rev;
            }
            _ => {}
        }

        let task_id = match &message {
            FromAgent::StatePatch { task_id, .. } => task_id.clone(),
            FromAgent::Lock { task, .. } => Some(task.clone()),
            FromAgent::Unlock { key } => state.fold.locks.get(key).cloned(),
            other => other.task().map(str::to_owned),
        };
        // An effect is addressed by its task and step, which is the next one.
        if let Some(task) = task_id.as_ref().filter(|_| state.session.is_some()) {
            let next = state.steps.get(task).copied().unwrap_or(0) + 1;
            if let Some(effect_id) = message.effect_id_mut() {
                if effect_id.is_empty() {
                    *effect_id = format!("{task}:{next}");
                }
            }
        }
        let payload = serde_json::to_value(&message).unwrap_or(Value::Null);
        let kind = payload.get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
        match self.append(&mut state, &kind, task_id, action_key.map(str::to_owned), subject_of(&message), payload) {
            Some(entry) => self.inner.emit_entry(message, action_key, &entry),
            None => self.inner.emit(message, action_key),
        }
    }
}

/// The wire `type` of a message.
pub fn kind_of(message: &FromAgent) -> String {
    match serde_json::to_value(message) {
        Ok(Value::Object(map)) => map.get("type").and_then(Value::as_str).unwrap_or_default().to_owned(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Map};
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct Recorder(StdMutex<Vec<(String, Option<u64>)>>);

    impl Emitter for Recorder {
        fn emit(&self, message: FromAgent, _: Option<&str>) {
            self.0.lock().unwrap().push((kind_of(&message), None));
        }
        fn emit_entry(&self, _: FromAgent, _: Option<&str>, entry: &JournalEntry) {
            self.0.lock().unwrap().push((entry.kind.clone(), Some(entry.pos)));
        }
    }

    fn session_init(id: &str) -> FromAgent {
        let mut states = Map::new();
        states.insert("Camera".into(), json!({"exposure": 1}));
        FromAgent::SessionInit {
            session_id: id.into(),
            states,
        }
    }

    fn patch(rev: u64, task: &str) -> FromAgent {
        FromAgent::StatePatch {
            session_id: "s".into(),
            global_rev: rev,
            state_name: "Camera".into(),
            ts: 0.0,
            op: "replace".into(),
            path: "/exposure".into(),
            value: json!(rev + 1),
            old_value: Value::Null,
            task_id: Some(task.into()),
        }
    }

    #[test]
    fn numbers_and_folds() {
        let recorder = Arc::new(Recorder::default());
        let journal = Journal::new(recorder.clone(), None, None);

        journal.emit(FromAgent::Progress { task: "early".into(), progress: None, message: None }, None);
        journal.emit(session_init("s"), None);
        journal.record_assign(
            &Assign {
                id: None,
                interface: "set".into(),
                task: "t".into(),
                root: None,
                parent: None,
                resolution: None,
                step: None,
                probe: false,
                capture: None,
                reference: Some("r".into()),
                args: Map::new(),
                message: None,
                user: "u".into(),
                org: "o".into(),
                action: "a".into(),
                implementation: "i".into(),
                token: Some("secret".into()),
            },
            "set",
        );
        journal.emit(FromAgent::Lock { key: "cam".into(), task: "t".into() }, None);
        journal.emit(patch(1, "t"), None);
        journal.emit(FromAgent::Yield { task: "t".into(), returns: Map::new() }, Some("set"));
        journal.emit(FromAgent::Completed { task: "t".into() }, Some("set"));
        journal.emit(FromAgent::Unlock { key: "cam".into() }, None);

        let seen = recorder.0.lock().unwrap().clone();
        assert_eq!(seen[0], ("PROGRESS".into(), None), "nothing is recorded before the session");
        let kinds: Vec<(String, Option<u64>)> = seen[1..].to_vec();
        assert_eq!(
            kinds,
            ["SESSION_INIT", "ASSIGN", "LOCK", "STATE_PATCH", "YIELD", "COMPLETED", "UNLOCK"]
                .iter()
                .enumerate()
                .map(|(i, k)| (k.to_string(), Some(i as u64 + 1)))
                .collect::<Vec<_>>()
        );

        journal.locked(|view| {
            let wm = view.watermark().unwrap();
            assert_eq!((wm.pos, wm.global_rev), (7, 1));
            let fold = view.fold();
            assert_eq!(fold.states["Camera"], json!({"exposure": 2}));
            let task = &fold.tasks["t"];
            assert_eq!((task.status.as_str(), task.done, task.yields), ("COMPLETED", true, 1));
            assert_eq!(task.reference.as_deref(), Some("r"));
            assert!(fold.locks.is_empty());
            let recent = view.recent(0, 7).unwrap();
            assert_eq!(recent.len(), 7);
            assert!(recent[1].payload.get("token").is_none(), "secrets are not recorded");
            assert_eq!(recent[6].task_id.as_deref(), Some("t"), "UNLOCK names the holder");
            assert_eq!(recent[6].action_key.as_deref(), Some("set"));
            assert_eq!(recent[3].global_rev, 1);
            assert_eq!(view.recent(2, 4).unwrap().iter().map(|e| e.pos).collect::<Vec<_>>(), vec![3, 4]);
        });

        // A new session starts over.
        journal.emit(session_init("s2"), None);
        assert_eq!(journal.watermark().unwrap(), Watermark { session_id: "s2".into(), pos: 1, global_rev: 0 });
    }

    #[tokio::test]
    async fn a_snapshot_is_not_patched_twice() {
        use crate::state::{Mutation, StateDeclaration, StateHub, StateType, SNAPSHOT_INTERVAL};
        #[derive(Clone, serde::Serialize, serde::Deserialize)]
        struct Tags {
            tags: Vec<u64>,
        }
        impl StateType for Tags {
            const NAME: &'static str = "Tags";
            const REQUIRED_LOCKS: &'static [&'static str] = &[];
            fn ports() -> Vec<crate::Port> {
                vec![]
            }
        }
        let hub = StateHub::new(vec![StateDeclaration::of(Some(Tags { tags: vec![] }))]);
        hub.apply_initial_values().unwrap();
        let journal = Arc::new(Journal::new(Arc::new(crate::emit::NullEmitter), None, None));
        hub.start_session("s".into(), journal.clone(), None).await.unwrap();
        let tags = hub.state_mut::<Tags>(Mutation::default()).unwrap();
        for i in 0..SNAPSHOT_INTERVAL + 2 {
            tags.update(|t| t.tags.push(i)).unwrap();
        }
        journal.locked(|view| {
            assert_eq!(view.fold().states["Tags"], hub.value("Tags").unwrap());
            let replayed = Fold::from_entries(view.recent(0, view.watermark().unwrap().pos).unwrap().iter().map(|e| &**e));
            assert_eq!(replayed.states["Tags"]["tags"].as_array().unwrap().len(), SNAPSHOT_INTERVAL as usize + 2);
        });
    }

    #[test]
    fn gate_refuses_after_close_and_allows_nesting() {
        let gate = TaskGate::default();
        {
            let outer = gate.enter().unwrap();
            let inner = gate.enter().unwrap();
            drop((inner, outer));
        }
        gate.close();
        assert!(gate.enter().is_none());
        assert!(gate.is_closed());
    }

    #[test]
    fn gate_close_waits_for_in_flight() {
        let gate = TaskGate::default();
        let pass_gate = gate.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            let _pass = pass_gate.enter().unwrap();
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        });
        entered_rx.recv().unwrap();
        let closer = {
            let gate = gate.clone();
            std::thread::spawn(move || {
                let start = std::time::Instant::now();
                gate.close();
                start.elapsed()
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert!(closer.join().unwrap() >= Duration::from_millis(50));
    }

    #[tokio::test]
    async fn persists_in_order() {
        #[derive(Default)]
        struct Collect(StdMutex<Vec<u64>>);
        #[async_trait]
        impl JournalSink for Collect {
            async fn write_entries(&self, entries: &[Arc<JournalEntry>]) -> anyhow::Result<()> {
                self.0.lock().unwrap().extend(entries.iter().map(|e| e.pos));
                Ok(())
            }
        }
        let sink = Arc::new(Collect::default());
        let journal = Arc::new(Journal::new(Arc::new(crate::emit::NullEmitter), Some(sink.clone()), Some("s".into())));
        let mut handles = vec![];
        for t in 0..8 {
            let journal = journal.clone();
            handles.push(tokio::spawn(async move {
                for i in 0..50 {
                    journal.emit(
                        FromAgent::Log { task: format!("t{t}"), message: i.to_string(), level: Default::default() },
                        None,
                    );
                    tokio::task::yield_now().await;
                }
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        assert!(journal.flush(Duration::from_secs(2)).await);
        assert_eq!(*sink.0.lock().unwrap(), (1..=400).collect::<Vec<u64>>());
    }
}
