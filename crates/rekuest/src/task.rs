//! The executor's side of a [`Task`]: an [`AgentTask`] reports what its
//! action does as frames of the agent protocol, through the journal.

#[allow(unused_imports)]
use crate::prelude::*;
use crate::sync::Mutex;
use core::any::Any;
use core::time::Duration;

use arkitekt_spec::rt::{self, Latch};
use arkitekt_spec::state::{Mutation, StateCells};
use arkitekt_spec::{Task, TaskBackend, TaskGate};
use futures::future::BoxFuture;
use serde_json::{Map, Value};

use crate::emit::Emitter;
use crate::messages::{Assign, EffectKind, FromAgent, LogLevel};

/// A pause armed on a task: the next pausepoint waits until it is released.
#[derive(Default)]
pub(crate) struct Break {
    armed: Mutex<Option<Arc<Latch>>>,
}

impl Break {
    /// Arm a pause. Returns false if one was already armed.
    pub(crate) fn arm(&self) -> bool {
        let mut armed = self.armed.lock();
        if armed.is_some() {
            return false;
        }
        *armed = Some(Arc::new(Latch::new()));
        true
    }

    /// Release the pause; with `step`, arm the next one right away.
    /// Returns false if nothing was armed.
    pub(crate) fn release(&self, step: bool) -> bool {
        let mut armed = self.armed.lock();
        let Some(current) = armed.take() else {
            return false;
        };
        current.open();
        if step {
            *armed = Some(Arc::new(Latch::new()));
        }
        true
    }

    fn current(&self) -> Option<Arc<Latch>> {
        self.armed.lock().clone()
    }
}

/// A task the executor runs: an assignment, reported through the journal.
pub struct AgentTask {
    id: String,
    assignment: Arc<Assign>,
    emitter: Arc<dyn Emitter>,
    action_key: String,
    brk: Arc<Break>,
    states: Arc<StateCells>,
    locks: Vec<String>,
    gate: TaskGate,
}

impl AgentTask {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn task(
        assignment: Arc<Assign>,
        emitter: Arc<dyn Emitter>,
        action_key: String,
        brk: Arc<Break>,
        states: Arc<StateCells>,
        locks: Vec<String>,
        gate: TaskGate,
    ) -> Task {
        Task::new(Arc::new(Self {
            id: assignment.task.clone(),
            assignment,
            emitter,
            action_key,
            brk,
            states,
            locks,
            gate,
        }))
    }

    /// The assignment that started this task.
    pub fn assignment(&self) -> &Assign {
        &self.assignment
    }

    /// Reports after the task's end (a cancelled task still running up to
    /// its next `.await`) are dropped.
    fn emit(&self, message: FromAgent) {
        let Some(_pass) = self.gate.enter() else {
            tracing::debug!(task = %self.id, "dropping a report after the task ended: {message:?}");
            return;
        };
        self.emitter.emit(message, Some(&self.action_key))
    }

    /// Record an `EFFECT`: a value the task takes from outside itself.
    ///
    /// This is the replay seam: once a replay engine exists, it returns the
    /// value recorded at this task step here instead of calling `take`.
    fn effect(&self, effect: EffectKind, take: impl FnOnce() -> Value) -> Value {
        let value = take();
        self.emit(FromAgent::Effect {
            task: self.id.clone(),
            effect,
            value: value.clone(),
            // Keys are for workflow replay, which this agent does not do.
            key: None,
        });
        value
    }
}

/// The assignment of a task an executor runs (`None` for any other task).
pub fn assignment(task: &Task) -> Option<&Assign> {
    task.backend()
        .as_any()
        .downcast_ref::<AgentTask>()
        .map(AgentTask::assignment)
}

impl TaskBackend for AgentTask {
    fn id(&self) -> &str {
        &self.id
    }

    fn token(&self) -> Option<&str> {
        self.assignment.token.as_deref()
    }

    fn user(&self) -> Option<&str> {
        Some(&self.assignment.user)
    }

    fn assigner(&self) -> Option<&str> {
        Some(
            self.assignment
                .assigner
                .as_deref()
                .unwrap_or(&self.assignment.user),
        )
    }

    fn org(&self) -> Option<&str> {
        Some(&self.assignment.org)
    }

    fn log(&self, level: LogLevel, message: String) {
        self.emit(FromAgent::Log {
            task: self.id.clone(),
            message,
            level,
        })
    }

    fn progress(&self, percent: i32, message: Option<String>) {
        self.emit(FromAgent::Progress {
            task: self.id.clone(),
            progress: Some(percent),
            message,
        })
    }

    fn pausepoint(&self) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            let Some(armed) = self.brk.current() else {
                return false;
            };
            self.emit(FromAgent::Paused {
                task: self.id.clone(),
                message: None,
                details: None,
            });
            armed.wait().await;
            self.emit(FromAgent::Resumed {
                task: self.id.clone(),
            });
            true
        })
    }

    /// The current time, recorded as an `EFFECT` (`NOW`, epoch seconds).
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        let value = self.effect(EffectKind::Now, || {
            Value::from(rt::now().timestamp_micros() as f64 / 1e6)
        });
        from_epoch_seconds(&value).unwrap_or_else(rt::now)
    }

    /// `n` random bytes, recorded as an `EFFECT` (`RANDOM`, hex).
    fn random(&self, n: usize) -> Vec<u8> {
        let value = self.effect(EffectKind::Random, || {
            let mut bytes = vec![0u8; n];
            rt::fill_random(&mut bytes);
            Value::from(hex::encode(bytes))
        });
        value
            .as_str()
            .and_then(|hex| hex::decode(hex).ok())
            .unwrap_or_default()
    }

    /// Records the deadline as an `EFFECT` (`SLEEP`, epoch seconds), then
    /// sleeps until it.
    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        let value = self.effect(EffectKind::Sleep, || {
            let until = rt::now() + chrono::Duration::from_std(duration).unwrap_or_default();
            Value::from(until.timestamp_micros() as f64 / 1e6)
        });
        let left = from_epoch_seconds(&value)
            .and_then(|until| (until - rt::now()).to_std().ok())
            .unwrap_or_default();
        Box::pin(rt::sleep(left))
    }

    fn yield_returns(&self, returns: Map<String, Value>) {
        self.emit(FromAgent::Yield {
            task: self.id.clone(),
            returns,
        })
    }

    fn states(&self) -> Option<(&StateCells, Mutation)> {
        Some((
            &self.states,
            Mutation {
                task_id: Some(self.id.clone()),
                locks: Some(self.locks.clone()),
                gate: Some(self.gate.clone()),
            },
        ))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn from_epoch_seconds(value: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    // Rounded half away from zero (core has no `f64::round`).
    let scaled = value.as_f64()? * 1e6;
    let micros = if scaled < 0.0 {
        scaled - 0.5
    } else {
        scaled + 0.5
    } as i64;
    chrono::DateTime::from_timestamp_micros(micros)
}
