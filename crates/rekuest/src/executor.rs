//! Running assignments, independent of how they arrive.
//!
//! The [`Executor`] owns everything an agent does locally: the managed
//! tasks, per-action serialization, locks, states, pauses and hooks. A
//! transport (the websocket [`Agent`](crate::Agent), or a served app) feeds it
//! commands and receives its messages through an [`Emitter`].

use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use futures::FutureExt;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::Value;
use tokio::task::AbortHandle;

use crate::action::{ActionError, Concurrency, Registry};
use crate::context::Context;
use crate::emit::Emitter;
use crate::hooks::{Background, Startup};
use crate::journal::TaskGate;
use crate::locks::{LockTable, LockView};
use crate::messages::{Assign, FromAgent};
use crate::shelf::Shelf;
use crate::state::{Sink, StateHub};
use crate::task::{Break, Task};

/// How many finished task ids are remembered to ignore duplicate assigns.
const FINISHED_MEMORY: usize = 2048;
/// How long one startup or shutdown hook may take.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(20);
/// How long teardown waits for state changes to be published and persisted.
pub const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// A task as the served app shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskView {
    pub task: String,
    pub action_key: String,
    pub interface: Option<String>,
    pub user: Option<String>,
    /// The assigning org (Python calls this field `app`).
    pub app: Option<String>,
    pub action: Option<String>,
    pub running: bool,
    pub actor_id: Option<String>,
}

/// A state as the served app shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StateView {
    pub interface: String,
    pub name: String,
    pub initialized: bool,
    pub value: Option<Value>,
}

struct Managed {
    assign: Arc<Assign>,
    action_key: String,
    actor_id: String,
    brk: Arc<Break>,
    abort: Option<AbortHandle>,
    gate: TaskGate,
}

#[derive(Default)]
struct Tasks {
    managed: IndexMap<String, Managed>,
    finished: VecDeque<String>,
    finished_set: HashSet<String>,
}

impl Tasks {
    /// Forget a task. Returns true if it was still managed (i.e. its end has not been reported).
    fn finish(&mut self, task: &str) -> bool {
        let was_managed = self.managed.shift_remove(task).is_some();
        if self.finished_set.insert(task.to_owned()) {
            self.finished.push_back(task.to_owned());
            if self.finished.len() > FINISHED_MEMORY {
                if let Some(old) = self.finished.pop_front() {
                    self.finished_set.remove(&old);
                }
            }
        }
        was_managed
    }
}

/// `interface or action or task`, as Python keys tasks for subscribers.
pub fn action_key(assign: &Assign) -> String {
    [&assign.interface, &assign.action, &assign.task]
        .into_iter()
        .find(|s| !s.is_empty())
        .cloned()
        .unwrap_or_default()
}

struct Inner {
    registry: Registry,
    ctx: RwLock<Context>,
    emitter: Arc<dyn Emitter>,
    sink: Option<Arc<dyn Sink>>,
    hub: Arc<StateHub>,
    locks: LockTable,
    shelf: Shelf,
    serial: HashMap<String, Arc<tokio::sync::Mutex<()>>>,
    actor_ids: HashMap<String, String>,
    tasks: Mutex<Tasks>,
    background: Mutex<Vec<AbortHandle>>,
    activated: tokio::sync::OnceCell<()>,
    /// The session to open when there is no sink to create one.
    session_id: Option<String>,
}

/// Runs assignments for a [`Registry`]. Cheap to clone.
#[derive(Clone)]
pub struct Executor {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Executor")
            .field("registry", &self.inner.registry)
            .finish_non_exhaustive()
    }
}

impl Executor {
    pub fn new(registry: Registry, ctx: Context, emitter: Arc<dyn Emitter>, sink: Option<Arc<dyn Sink>>) -> Self {
        Self::with_session(registry, ctx, emitter, sink, None)
    }

    /// Like [`Executor::new`], with the session id to open (the websocket
    /// agent registers with it, and its states must be published under it).
    /// A sink, if given, creates the session instead.
    pub fn with_session(
        registry: Registry,
        ctx: Context,
        emitter: Arc<dyn Emitter>,
        sink: Option<Arc<dyn Sink>>,
        session_id: Option<String>,
    ) -> Self {
        let hub = Arc::new(StateHub::new(registry.states().to_vec()));
        let locks = LockTable::new(registry.lock_keys());
        let serial = registry
            .iter()
            .filter(|a| a.concurrency() == Concurrency::Serial)
            .map(|a| (a.interface(), Arc::new(tokio::sync::Mutex::new(()))))
            .collect();
        let actor_ids = registry
            .iter()
            .map(|a| (a.interface(), uuid::Uuid::new_v4().to_string()))
            .collect();
        // Memory structures shrink onto (and expand from) the shelf in the context.
        let shelf = Shelf::new(emitter.clone());
        let ctx = {
            let mut builder = ctx.to_builder();
            builder.insert(shelf.clone());
            builder.build()
        };
        Self {
            inner: Arc::new(Inner {
                registry,
                ctx: RwLock::new(ctx),
                emitter,
                sink,
                hub,
                locks,
                shelf,
                serial,
                actor_ids,
                tasks: Mutex::default(),
                background: Mutex::default(),
                activated: tokio::sync::OnceCell::new(),
                session_id,
            }),
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.inner.registry
    }

    pub fn context(&self) -> Context {
        self.inner.ctx.read().expect("ctx lock").clone()
    }

    pub fn states(&self) -> &Arc<StateHub> {
        &self.inner.hub
    }

    pub fn locks(&self) -> &LockTable {
        &self.inner.locks
    }

    /// Values kept in memory for memory-structure ports.
    pub fn shelf(&self) -> &Shelf {
        &self.inner.shelf
    }

    pub fn is_activated(&self) -> bool {
        self.inner.activated.initialized()
    }

    /// Start the agent's session, once: run startup hooks, open the session
    /// (SESSION_INIT), then start background hooks. Later calls do nothing.
    pub async fn activate(&self) -> anyhow::Result<()> {
        self.inner
            .activated
            .get_or_try_init(|| self.do_activate())
            .await
            .map(|_| ())
    }

    async fn do_activate(&self) -> anyhow::Result<()> {
        let inner = &self.inner;
        let session_id = match &inner.sink {
            Some(sink) => sink.create_session().await?,
            None => inner
                .session_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        };

        inner.hub.apply_initial_values()?;
        let startup = Startup {
            ctx: self.context(),
            hub: inner.hub.clone(),
            contexts: Arc::default(),
        };
        for hook in &inner.registry.hooks().startup {
            tokio::time::timeout(HOOK_TIMEOUT, hook(startup.clone()))
                .await
                .map_err(|_| anyhow::anyhow!("a startup hook took longer than {HOOK_TIMEOUT:?}"))??;
        }
        let edits = std::mem::take(&mut *startup.contexts.lock().expect("context edits"));
        if !edits.is_empty() {
            let mut builder = self.context().to_builder();
            for edit in edits {
                edit(&mut builder);
            }
            *inner.ctx.write().expect("ctx lock") = builder.build();
        }

        inner
            .hub
            .start_session(session_id, inner.emitter.clone(), inner.sink.clone())
            .await?;

        let background = Background {
            ctx: self.context(),
            hub: inner.hub.clone(),
        };
        let mut handles = inner.background.lock().expect("background lock");
        for hook in &inner.registry.hooks().background {
            let run = hook(background.clone());
            handles.push(
                tokio::spawn(async move {
                    if let Err(e) = run.await {
                        tracing::error!("a background hook failed: {e:#}");
                    }
                })
                .abort_handle(),
            );
        }
        Ok(())
    }

    /// Stop: cancel background hooks, run shutdown hooks (in reverse order),
    /// then wait (bounded) until state changes are published and persisted.
    pub async fn teardown(&self) {
        for handle in self.inner.background.lock().expect("background lock").drain(..) {
            handle.abort();
        }
        if self.is_activated() {
            let shutdown = Background {
                ctx: self.context(),
                hub: self.inner.hub.clone(),
            };
            for hook in self.inner.registry.hooks().shutdown.iter().rev() {
                match tokio::time::timeout(HOOK_TIMEOUT, hook(shutdown.clone())).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::error!("a shutdown hook failed: {e:#}"),
                    Err(_) => tracing::error!("a shutdown hook took longer than {HOOK_TIMEOUT:?}"),
                }
            }
        }
        self.inner.hub.flush(FLUSH_TIMEOUT).await;
        if let Some(sink) = &self.inner.sink {
            let (_, rev) = self.inner.hub.revision();
            let caught_up = async {
                loop {
                    match sink.is_caught_up_to(rev).await {
                        Ok(true) => return,
                        Ok(false) => tokio::time::sleep(Duration::from_millis(50)).await,
                        Err(e) => {
                            tracing::warn!("could not check the sink: {e:#}");
                            return;
                        }
                    }
                }
            };
            if tokio::time::timeout(FLUSH_TIMEOUT, caught_up).await.is_err() {
                tracing::warn!("the state sink did not catch up within {FLUSH_TIMEOUT:?}");
            }
        }
    }

    fn emit(&self, message: FromAgent, action_key: Option<&str>) {
        self.inner.emitter.emit(message, action_key);
    }

    pub fn is_running(&self, task: &str) -> bool {
        self.inner.tasks.lock().expect("tasks lock").managed.contains_key(task)
    }

    pub fn has_finished(&self, task: &str) -> bool {
        self.inner.tasks.lock().expect("tasks lock").finished_set.contains(task)
    }

    /// Start running an assignment.
    pub fn assign(&self, assign: Assign) {
        let inner = &self.inner;
        let mut tasks = inner.tasks.lock().expect("tasks lock");
        let task_id = assign.task.clone();
        if tasks.managed.contains_key(&task_id) || tasks.finished_set.contains(&task_id) {
            tracing::debug!("ignoring duplicate ASSIGN for task {task_id}");
            return;
        }
        let Some(action) = inner.registry.get(&assign.interface) else {
            self.emit(
                FromAgent::Critical {
                    task: task_id,
                    error: format!("this agent has no action with interface '{}'", assign.interface),
                },
                None,
            );
            return;
        };

        let key = action_key(&assign);
        let actor_id = inner.actor_ids.get(&assign.interface).cloned().unwrap_or_default();
        let brk = Arc::new(Break::default());
        if assign.step == Some(true) {
            brk.arm();
        }
        let assign = Arc::new(assign);
        let locks = action.locks();
        let gate = TaskGate::default();
        let task = Task::new(
            assign.clone(),
            inner.emitter.clone(),
            key.clone(),
            brk.clone(),
            inner.hub.clone(),
            locks.clone(),
            gate.clone(),
        );
        tasks.managed.insert(
            task_id.clone(),
            Managed {
                assign: assign.clone(),
                action_key: key.clone(),
                actor_id,
                brk,
                abort: None,
                gate: gate.clone(),
            },
        );

        let this = self.clone();
        let serial = inner.serial.get(&assign.interface).cloned();
        let id = task_id.clone();
        let run = async move {
            let inner = &this.inner;
            if let Some(_pass) = gate.enter() {
                this.emit(
                    FromAgent::Progress {
                        task: id.clone(),
                        progress: Some(0),
                        message: Some("Queued for running".into()),
                    },
                    Some(&key),
                );
            }
            let _serial = match serial {
                Some(serial) => Some(serial.lock_owned().await),
                None => None,
            };
            let held = inner.locks.acquire(&locks, &id, inner.emitter.clone()).await;

            let body = rath::with_task_token(
                assign.token.clone(),
                action.run(assign.args.clone(), this.context(), task),
            );
            let event = match AssertUnwindSafe(body).catch_unwind().await {
                Ok(Ok(())) => FromAgent::Completed { task: id.clone() },
                Ok(Err(ActionError::Failed(error))) => FromAgent::Failed { task: id.clone(), error },
                Ok(Err(ActionError::Critical(error))) => FromAgent::Critical { task: id.clone(), error },
                Err(panic) => FromAgent::Critical {
                    task: id.clone(),
                    error: format!("the action panicked: {}", panic_message(&panic)),
                },
            };
            // Reported while still holding the locks: UNLOCK follows the end, as in Python.
            // A cancelled task was already reported (and forgotten) by `stop`.
            {
                let mut tasks = inner.tasks.lock().expect("tasks lock");
                if tasks.managed.contains_key(&id) {
                    gate.close();
                    this.emit(event, Some(&key));
                    tasks.finish(&id);
                }
            }
            drop(held);
        };

        // The lock is held across the spawn so the task cannot finish before
        // its abort handle is stored.
        let handle = tokio::spawn(run);
        if let Some(managed) = tasks.managed.get_mut(&task_id) {
            managed.abort = Some(handle.abort_handle());
        }
    }

    /// Abort a running task and report `CANCELLED`.
    pub fn cancel(&self, task: &str) {
        self.stop(task, |task| FromAgent::Cancelled { task })
    }

    /// Abort a running task and report `INTERRUPTED`.
    pub fn interrupt(&self, task: &str) {
        self.stop(task, |task| FromAgent::Interrupted { task })
    }

    fn stop(&self, task: &str, report: impl FnOnce(String) -> FromAgent) {
        let mut tasks = self.inner.tasks.lock().expect("tasks lock");
        let Some(managed) = tasks.managed.get(task) else {
            tracing::debug!("cannot stop task {task}: it is not running");
            return;
        };
        let key = managed.action_key.clone();
        if let Some(abort) = &managed.abort {
            abort.abort();
        }
        // Waits for a report or state change the task is making right now;
        // after this, the task can report nothing more.
        managed.gate.close();
        self.emit(report(task.to_owned()), Some(&key));
        tasks.finish(task);
    }

    /// Pause the task at its next pausepoint.
    pub fn pause(&self, task: &str) {
        match self.break_of(task) {
            Some(brk) => {
                if !brk.arm() {
                    tracing::warn!("task {task} is already pausing");
                }
            }
            None => self.not_managed(task),
        }
    }

    /// Release a paused task; with `step`, it pauses again at the next pausepoint.
    pub fn resume(&self, task: &str, step: bool) {
        match self.break_of(task) {
            Some(brk) => {
                if !brk.release(step) {
                    tracing::warn!("task {task} was not paused");
                }
            }
            None => self.not_managed(task),
        }
    }

    fn break_of(&self, task: &str) -> Option<Arc<Break>> {
        self.inner
            .tasks
            .lock()
            .expect("tasks lock")
            .managed
            .get(task)
            .map(|m| m.brk.clone())
    }

    fn not_managed(&self, task: &str) {
        self.emit(
            FromAgent::Critical {
                task: task.to_owned(),
                error: "Actors is no longer running and not managed. Probablry there was a restart".into(),
            },
            None,
        );
    }

    /// Tasks currently managed, optionally only those of some action keys.
    pub fn task_views(&self, action_keys: Option<&HashSet<String>>) -> IndexMap<String, TaskView> {
        self.inner
            .tasks
            .lock()
            .expect("tasks lock")
            .managed
            .iter()
            .filter(|(_, m)| action_keys.is_none_or(|keys| keys.contains(&m.action_key)))
            .map(|(id, m)| {
                (
                    id.clone(),
                    TaskView {
                        task: id.clone(),
                        action_key: m.action_key.clone(),
                        interface: Some(m.assign.interface.clone()),
                        user: Some(m.assign.user.clone()),
                        app: Some(m.assign.org.clone()),
                        action: Some(m.assign.action.clone()),
                        running: true,
                        actor_id: Some(m.actor_id.clone()),
                    },
                )
            })
            .collect()
    }

    pub fn task_view(&self, task: &str) -> Option<TaskView> {
        self.task_views(None).shift_remove(task)
    }

    /// Declared states, optionally only some interfaces.
    pub fn state_views(&self, keys: Option<&HashSet<String>>) -> IndexMap<String, StateView> {
        self.inner
            .hub
            .declarations()
            .iter()
            .filter(|d| keys.is_none_or(|k| k.contains(&d.name)))
            .map(|d| {
                let value = self.inner.hub.value(&d.name);
                (
                    d.name.clone(),
                    StateView {
                        interface: d.name.clone(),
                        name: d.name.clone(),
                        initialized: value.is_some(),
                        value,
                    },
                )
            })
            .collect()
    }

    pub fn lock_views(&self, keys: Option<&HashSet<String>>) -> IndexMap<String, LockView> {
        self.inner.locks.views(keys)
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}
