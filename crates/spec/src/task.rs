//! The running task an action executes for: [`Task`], a handle to whatever
//! runs the action ([`TaskBackend`]: rekuest's executor, or [`Task::local`]).

#[allow(unused_imports)]
use crate::prelude::*;
use crate::sync::Mutex;
use core::any::Any;
use core::time::Duration;

use futures::future::BoxFuture;
use serde_json::{Map, Value};

use crate::actions::LogLevel;
use crate::state::{Mutation, StateCells, StateError, StateMut, StateRef, StateType};

/// What a [`Task`] reports through: implemented by a runtime for each task
/// it runs.
pub trait TaskBackend: Send + Sync + 'static {
    fn id(&self) -> &str;

    /// The token requests made for this task should carry.
    fn token(&self) -> Option<&str> {
        None
    }

    fn user(&self) -> Option<&str> {
        None
    }

    fn org(&self) -> Option<&str> {
        None
    }

    fn log(&self, level: LogLevel, message: String);

    /// Progress in percent (0-100).
    fn progress(&self, percent: i32, message: Option<String>);

    /// See [`Task::pausepoint`].
    fn pausepoint(&self) -> BoxFuture<'_, bool>;

    /// See [`Task::now`].
    fn now(&self) -> chrono::DateTime<chrono::Utc>;

    /// See [`Task::random`].
    fn random(&self, n: usize) -> Vec<u8>;

    /// See [`Task::sleep`].
    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()>;

    /// One set of (already shrunk) return values.
    fn yield_returns(&self, returns: Map<String, Value>);

    /// The states the task may use, and how it changes them; `None` when it
    /// runs on no agent.
    fn states(&self) -> Option<(&StateCells, Mutation)> {
        None
    }

    /// The backend itself, for what only its runtime knows about a task
    /// (rekuest: the assignment).
    fn as_any(&self) -> &dyn Any;
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
    backend: Arc<dyn TaskBackend>,
}

impl core::fmt::Debug for Task {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Task")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}

impl Task {
    pub fn new(backend: Arc<dyn TaskBackend>) -> Self {
        Self { backend }
    }

    /// A task that is not connected to an agent: reports go to `tracing`,
    /// pausepoints never pause, and there are no states.
    /// Useful to call an action directly, e.g. in tests.
    pub fn local() -> Self {
        Self::new(Arc::new(Local {
            id: format!("local-{}", crate::rt::uuid()),
        }))
    }

    pub fn backend(&self) -> &Arc<dyn TaskBackend> {
        &self.backend
    }

    pub fn id(&self) -> &str {
        self.backend.id()
    }

    /// The token requests made for this task should carry.
    pub fn token(&self) -> Option<&str> {
        self.backend.token()
    }

    pub fn user(&self) -> Option<&str> {
        self.backend.user()
    }

    pub fn org(&self) -> Option<&str> {
        self.backend.org()
    }

    pub fn log(&self, message: impl Into<String>) {
        self.log_level(LogLevel::Info, message)
    }

    pub fn log_level(&self, level: LogLevel, message: impl Into<String>) {
        self.backend.log(level, message.into())
    }

    /// Report progress in percent (0-100) with an optional message.
    pub fn progress(&self, percent: i32, message: impl Into<String>) {
        let message = message.into();
        self.backend.progress(
            percent.clamp(0, 100),
            (!message.is_empty()).then_some(message),
        )
    }

    /// A point where the task may be paused. If a pause was requested (or the
    /// task is being stepped), this reports `PAUSED`, waits until it is
    /// resumed, reports `RESUMED` and returns true. Otherwise it returns false
    /// immediately.
    pub async fn pausepoint(&self) -> bool {
        self.backend.pausepoint().await
    }

    /// The current time, recorded as an `EFFECT` (`NOW`).
    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.backend.now()
    }

    /// `n` random bytes, recorded as an `EFFECT` (`RANDOM`).
    pub fn random(&self, n: usize) -> Vec<u8> {
        self.backend.random(n)
    }

    /// Sleep for `duration`, its deadline recorded as an `EFFECT` (`SLEEP`).
    pub async fn sleep(&self, duration: Duration) {
        self.backend.sleep(duration).await
    }

    /// A handle to change a state, holding this task's locks. Used by `#[action]`.
    #[doc(hidden)]
    pub fn state_mut<T: StateType>(&self) -> Result<StateMut<T>, StateError> {
        let (states, mutation) = self.backend.states().ok_or(StateError::NoAgent)?;
        states.state_mut(mutation)
    }

    /// A read-only handle to a state. Used by `#[action]`.
    #[doc(hidden)]
    pub fn state_ref<T: StateType>(&self) -> Result<StateRef<T>, StateError> {
        let (states, _) = self.backend.states().ok_or(StateError::NoAgent)?;
        states.state_ref()
    }

    /// Send one set of (already shrunk) return values. Used by `#[action]`.
    #[doc(hidden)]
    pub fn yield_returns(&self, returns: Map<String, Value>) {
        self.backend.yield_returns(returns)
    }
}

/// [`Task::local`]'s backend.
struct Local {
    id: String,
}

impl TaskBackend for Local {
    fn id(&self) -> &str {
        &self.id
    }

    fn log(&self, level: LogLevel, message: String) {
        tracing::info!(task = %self.id, ?level, "{message}");
    }

    fn progress(&self, percent: i32, message: Option<String>) {
        tracing::info!(task = %self.id, percent, "{}", message.unwrap_or_default());
    }

    fn pausepoint(&self) -> BoxFuture<'_, bool> {
        Box::pin(core::future::ready(false))
    }

    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        crate::rt::now()
    }

    fn random(&self, n: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; n];
        crate::rt::fill_random(&mut bytes);
        bytes
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        Box::pin(crate::rt::sleep(duration))
    }

    fn yield_returns(&self, returns: Map<String, Value>) {
        tracing::info!(task = %self.id, "yield {returns:?}");
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

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
    inner: Arc<Gate>,
}

#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    /// Woken when the last pass is dropped (with std; without it, `close`
    /// spins: one executor thread finishes its passes synchronously).
    #[cfg(feature = "std")]
    done: std::sync::Condvar,
}

impl core::fmt::Debug for TaskGate {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TaskGate")
            .field("closed", &self.is_closed())
            .finish()
    }
}

/// Held while acting for an open task.
pub struct GatePass<'a> {
    gate: &'a TaskGate,
}

impl Drop for GatePass<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.inner.state.lock();
        state.active -= 1;
        #[cfg(feature = "std")]
        if state.active == 0 {
            self.gate.inner.done.notify_all();
        }
    }
}

impl TaskGate {
    /// `None` once the gate is closed (or closing).
    pub fn enter(&self) -> Option<GatePass<'_>> {
        let mut state = self.inner.state.lock();
        if state.closed {
            return None;
        }
        state.active += 1;
        Some(GatePass { gate: self })
    }

    /// Refuse new entries, then wait until those in flight are done.
    pub fn close(&self) {
        let mut state = self.inner.state.lock();
        state.closed = true;
        #[cfg(feature = "std")]
        while state.active > 0 {
            state = self
                .inner
                .done
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        #[cfg(not(feature = "std"))]
        while state.active > 0 {
            drop(state);
            core::hint::spin_loop();
            state = self.inner.state.lock();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.state.lock().closed
    }
}
