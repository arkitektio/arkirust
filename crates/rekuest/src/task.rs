//! The handle an action gets to talk about its own execution.

use std::sync::Arc;

use serde_json::{Map, Value};

use crate::messages::{Assign, FromAgent, LogLevel};
use crate::outbox::Outbox;

/// The running task an action executes for.
///
/// Take it as a parameter of an `#[action]` to report progress and logs:
///
/// ```ignore
/// #[arkitekt::action]
/// async fn slow(n: i64, task: Task) -> i64 {
///     task.progress(50, "halfway");
///     n
/// }
/// ```
#[derive(Clone)]
pub struct Task {
    id: String,
    assignment: Option<Arc<Assign>>,
    outbox: Option<Arc<Outbox>>,
}

impl std::fmt::Debug for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Task {
    pub(crate) fn new(assignment: Arc<Assign>, outbox: Arc<Outbox>) -> Self {
        Self {
            id: assignment.task.clone(),
            assignment: Some(assignment),
            outbox: Some(outbox),
        }
    }

    /// A task that is not connected to an agent: reports go to `tracing`.
    /// Useful to call an action directly, e.g. in tests.
    pub fn local() -> Self {
        Self {
            id: format!("local-{}", uuid::Uuid::new_v4()),
            assignment: None,
            outbox: None,
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
        match &self.outbox {
            Some(outbox) => outbox.send(message),
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

    /// Send one set of (already shrunk) return values. Used by `#[action]`.
    #[doc(hidden)]
    pub fn yield_returns(&self, returns: Map<String, Value>) {
        self.emit(FromAgent::Yield {
            task: self.id.clone(),
            returns,
        })
    }
}
