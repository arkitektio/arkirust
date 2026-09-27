//! The handle an action gets to talk about its own execution.

use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tokio::sync::Notify;

use crate::emit::Emitter;
use crate::messages::{Assign, FromAgent, LogLevel};
use crate::state::{Mutation, StateError, StateHub, StateMut, StateRef, StateType};

/// A pause armed on a task: the next pausepoint waits until it is released.
#[derive(Default)]
pub(crate) struct Break {
    armed: Mutex<Option<Arc<Notify>>>,
}

impl Break {
    /// Arm a pause. Returns false if one was already armed.
    pub(crate) fn arm(&self) -> bool {
        let mut armed = self.armed.lock().expect("break lock");
        if armed.is_some() {
            return false;
        }
        *armed = Some(Arc::new(Notify::new()));
        true
    }

    /// Release the pause; with `step`, arm the next one right away.
    /// Returns false if nothing was armed.
    pub(crate) fn release(&self, step: bool) -> bool {
        let mut armed = self.armed.lock().expect("break lock");
        let Some(current) = armed.take() else {
            return false;
        };
        current.notify_one();
        if step {
            *armed = Some(Arc::new(Notify::new()));
        }
        true
    }

    fn current(&self) -> Option<Arc<Notify>> {
        self.armed.lock().expect("break lock").clone()
    }
}

/// The running task an action executes for.
///
/// Take it as a parameter of an `#[action]` to report progress and logs, and
/// to offer points where the task can be paused:
///
/// ```ignore
/// #[arkitekt::action]
/// async fn slow(n: i64, task: Task) -> i64 {
///     task.progress(50, "halfway");
///     task.pausepoint().await;
///     n
/// }
/// ```
#[derive(Clone)]
pub struct Task {
    id: String,
    assignment: Option<Arc<Assign>>,
    emitter: Option<Arc<dyn Emitter>>,
    action_key: Option<String>,
    brk: Arc<Break>,
    hub: Option<Arc<StateHub>>,
    locks: Vec<String>,
}

impl std::fmt::Debug for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task").field("id", &self.id).finish_non_exhaustive()
    }
}

impl Task {
    pub(crate) fn new(
        assignment: Arc<Assign>,
        emitter: Arc<dyn Emitter>,
        action_key: String,
        brk: Arc<Break>,
        hub: Arc<StateHub>,
        locks: Vec<String>,
    ) -> Self {
        Self {
            id: assignment.task.clone(),
            assignment: Some(assignment),
            emitter: Some(emitter),
            action_key: Some(action_key),
            brk,
            hub: Some(hub),
            locks,
        }
    }

    /// A task that is not connected to an agent: reports go to `tracing`,
    /// pausepoints never pause, and there are no states.
    /// Useful to call an action directly, e.g. in tests.
    pub fn local() -> Self {
        Self {
            id: format!("local-{}", uuid::Uuid::new_v4()),
            assignment: None,
            emitter: None,
            action_key: None,
            brk: Arc::default(),
            hub: None,
            locks: vec![],
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The assignment that started this task (`None` for local tasks).
    pub fn assignment(&self) -> Option<&Assign> {
        self.assignment.as_deref()
    }

    /// The token requests made for this task should carry.
    pub fn token(&self) -> Option<&str> {
        self.assignment.as_ref()?.token.as_deref()
    }

    pub fn user(&self) -> Option<&str> {
        self.assignment.as_ref().map(|a| a.user.as_str())
    }

    pub fn org(&self) -> Option<&str> {
        self.assignment.as_ref().map(|a| a.org.as_str())
    }

    fn emit(&self, message: FromAgent) {
        match &self.emitter {
            Some(emitter) => emitter.emit(message, self.action_key.as_deref()),
            None => tracing::info!(task = %self.id, "{message:?}"),
        }
    }

    pub fn log(&self, message: impl Into<String>) {
        self.log_level(LogLevel::Info, message)
    }

    pub fn log_level(&self, level: LogLevel, message: impl Into<String>) {
        self.emit(FromAgent::Log {
            task: self.id.clone(),
            message: message.into(),
            level,
        })
    }

    /// Report progress in percent (0-100) with an optional message.
    pub fn progress(&self, percent: i32, message: impl Into<String>) {
        let message = message.into();
        self.emit(FromAgent::Progress {
            task: self.id.clone(),
            progress: Some(percent.clamp(0, 100)),
            message: (!message.is_empty()).then_some(message),
        })
    }

    /// A point where the task may be paused. If a pause was requested (or the
    /// task is being stepped), this reports `PAUSED`, waits until it is
    /// resumed, reports `RESUMED` and returns true. Otherwise it returns false
    /// immediately.
    pub async fn pausepoint(&self) -> bool {
        let Some(armed) = self.brk.current() else {
            return false;
        };
        self.emit(FromAgent::Paused { task: self.id.clone() });
        armed.notified().await;
        self.emit(FromAgent::Resumed { task: self.id.clone() });
        true
    }

    /// A handle to change a state, holding this task's locks. Used by `#[action]`.
    #[doc(hidden)]
    pub fn state_mut<T: StateType>(&self) -> Result<StateMut<T>, StateError> {
        self.hub.as_ref().ok_or(StateError::NoAgent)?.state_mut(Mutation {
            task_id: Some(self.id.clone()),
            locks: Some(self.locks.clone()),
        })
    }

    /// A read-only handle to a state. Used by `#[action]`.
    #[doc(hidden)]
    pub fn state_ref<T: StateType>(&self) -> Result<StateRef<T>, StateError> {
        self.hub.as_ref().ok_or(StateError::NoAgent)?.state_ref()
    }

    /// Send one set of (already shrunk) return values. Used by `#[action]`.
    #[doc(hidden)]
    pub fn yield_returns(&self, returns: Map<String, Value>) {
        self.emit(FromAgent::Yield {
            task: self.id.clone(),
            returns,
        })
    }
}
