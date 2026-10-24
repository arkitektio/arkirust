//! An agent without its socket: the session, the executor and the journal,
//! wired together, on whatever [`rt::Runtime`](crate::rt::Runtime) the
//! process runs.
//!
//! A driver owns the I/O. It feeds the session what the socket brings
//! ([`Driver::with_session`]: `handle_text`, `handle_close`, …), sends what
//! [`Driver::poll`] hands out, waits for a frame, the deadline or
//! [`Driver::woken`], and passes every event to [`Driver::act`], which runs
//! the app's side (assignments, cancels, activation, inquiries) and hands
//! back the ones that are the driver's: `Connect`, `Close`,
//! `ReloadJournal`, `Acked` and `Ended`.
//!
//! [`Agent`](crate::Agent) is this with a tokio websocket.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rekuest_session::{to_text, Event, Outbox, Session, SessionConfig};

use crate::action::Registry;
use crate::context::Context;
use crate::definition::AgentDeclaration;
use crate::emit::Emitter;
use crate::executor::Executor;
use crate::journal::{Journal, JournalEntry, JournalSink};
use crate::messages::{FromAgent, ToAgent};
use crate::rt::{self, Signal};

/// The session, shared by the driver and every task that reports into it.
///
/// Reports lock it from inside the journal's lock, so nothing calls into the
/// executor or the journal while holding it.
struct Shared {
    session: Mutex<Session>,
    /// A report was queued (or activation finished): the driver should poll.
    wake: Signal,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Session> {
        self.session.lock().expect("session lock")
    }
}

impl Emitter for Shared {
    fn emit(&self, message: FromAgent, _action_key: Option<&str>) {
        self.lock().send(message);
        self.wake.raise();
    }

    fn emit_entry(&self, message: FromAgent, _action_key: Option<&str>, entry: &JournalEntry) {
        self.lock().send_entry(message, entry);
        self.wake.raise();
    }
}

/// What the driver has to do after a [`Driver::poll`].
#[derive(Debug)]
pub struct Polled {
    /// Events to pass to [`Driver::act`], in order.
    pub events: Vec<Event>,
    /// Text frames to send on the open connection, in order.
    pub frames: Vec<String>,
    /// When to poll again at the latest (`now` as the session counts it).
    pub deadline: Option<Duration>,
}

/// See the module docs. Cheap to clone.
#[derive(Clone)]
pub struct Driver {
    executor: Executor,
    journal: Arc<Journal>,
    shared: Arc<Shared>,
}

impl Driver {
    /// A driver for `registry`'s actions. `outbox` holds what is unacknowledged
    /// ([`Outbox::with_limit`] when `sink` persists the journal).
    pub fn new(
        registry: Registry,
        ctx: Context,
        config: SessionConfig,
        outbox: Outbox,
        sink: Option<Arc<dyn JournalSink>>,
    ) -> Self {
        let session_id = config.session_id.clone();
        let shared = Arc::new(Shared {
            session: Mutex::new(Session::with_outbox(config, outbox)),
            wake: Signal::new(),
        });
        let journal = Arc::new(Journal::new(shared.clone(), sink, Some(session_id.clone())));
        Self {
            executor: Executor::with_session(
                registry,
                ctx,
                journal.clone(),
                None,
                Some(session_id),
            ),
            journal,
            shared,
        }
    }

    pub fn executor(&self) -> &Executor {
        &self.executor
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// What this agent registers.
    pub fn declaration(
        &self,
        name: Option<String>,
        description: Option<String>,
    ) -> AgentDeclaration {
        self.executor.registry().declaration(name, description)
    }

    /// Feed or read the session. Do not call into the executor or the
    /// journal from `f`.
    pub fn with_session<R>(&self, f: impl FnOnce(&mut Session) -> R) -> R {
        f(&mut self.shared.lock())
    }

    /// Advance the session to `now` and take what it has for the driver.
    pub fn poll(&self, now: Duration) -> Polled {
        let mut session = self.shared.lock();
        session.handle_timeout(now);
        Polled {
            events: std::iter::from_fn(|| session.poll_event()).collect(),
            frames: std::iter::from_fn(|| session.poll_transmit())
                .map(|envelope| to_text(&envelope))
                .collect(),
            deadline: session.poll_timeout(),
        }
    }

    /// Resolves once there is something to poll for besides the socket and
    /// the deadline: a report, the end of activation.
    pub async fn woken(&self) {
        self.shared.wake.wait().await
    }

    /// Run the app's side of `event`; the driver's events come back.
    pub fn act(&self, event: Event) -> Option<Event> {
        match event {
            Event::Registered { agent, diagnostics } => {
                tracing::info!(
                    "registered as agent {agent} with {} actions",
                    self.executor.registry().len()
                );
                for d in diagnostics {
                    tracing::warn!(
                        "registration {}: {} ({}){}",
                        d.level.as_deref().unwrap_or("WARNING"),
                        d.message,
                        d.code,
                        d.path.map(|p| format!(" at {p}")).unwrap_or_default()
                    );
                }
            }
            Event::Inquiries(tasks) => self.answer_inquiries(tasks),
            Event::Activate => {
                let executor = self.executor.clone();
                let shared = self.shared.clone();
                rt::spawn(async move {
                    let result = executor.activate().await.map_err(|e| format!("{e:#}"));
                    shared.lock().activated(result);
                    shared.wake.raise();
                });
            }
            Event::Message(message) => self.dispatch(message),
            Event::MalformedAssign { task, error } => {
                self.journal.emit(FromAgent::Critical { task, error }, None)
            }
            other => return Some(other),
        }
        None
    }

    fn dispatch(&self, message: ToAgent) {
        match message {
            ToAgent::Assign(assign) => self.executor.assign(*assign),
            ToAgent::Cancel { task } => self.executor.cancel(&task),
            ToAgent::Interrupt { task } => self.executor.interrupt(&task),
            ToAgent::Pause { task } => self.executor.pause(&task),
            ToAgent::Resume { task, step } => self.executor.resume(&task, step),
            ToAgent::Collect { drawers } => {
                for drawer in drawers {
                    self.executor.shelf().remove(&drawer);
                }
            }
            ToAgent::Shelved { error: Some(e), .. } | ToAgent::Unshelved { error: Some(e), .. } => {
                tracing::warn!("the server could not record a drawer: {e}")
            }
            other => tracing::debug!("ignoring {other:?}"),
        }
    }

    fn answer_inquiries(&self, tasks: Vec<String>) {
        for task in tasks {
            if self.executor.is_running(&task) {
                self.journal.emit(
                    FromAgent::Progress {
                        task,
                        progress: None,
                        message: Some("still running".into()),
                    },
                    None,
                );
            } else if !self.executor.has_finished(&task) {
                // A predecessor's task: numbered, but with no task step.
                self.journal.emit_unowned(
                    FromAgent::Critical {
                        task,
                        error: "the agent restarted and lost this task".into(),
                    },
                    None,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::Envelope;

    fn driver() -> Driver {
        Driver::new(
            Registry::new(),
            Context::default(),
            SessionConfig::new("s", 1),
            Outbox::new(),
            None,
        )
    }

    /// Connected and past INIT: reports go out.
    fn live(driver: &Driver) {
        driver.with_session(|session| {
            session.handle_timeout(Duration::ZERO);
            session.connected(
                Duration::ZERO,
                "t".into(),
                serde_json::from_value(serde_json::json!({})).unwrap(),
            );
            session.handle_text(
                Duration::ZERO,
                r#"{"type":"INIT","id":"i","agent":"a","inquiries":[],"diagnostics":[]}"#,
            );
            session.poll_transmit();
        });
    }

    fn drain(driver: &Driver) -> Vec<Envelope> {
        driver.with_session(|session| std::iter::from_fn(|| session.poll_transmit()).collect())
    }

    fn log(task: &str) -> FromAgent {
        FromAgent::Log {
            task: task.into(),
            message: "m".into(),
            level: Default::default(),
        }
    }

    #[test]
    fn effects_are_numbered_and_sent_at_once() {
        let driver = driver();
        live(&driver);
        let journal = driver.journal();
        journal.emit(log("t"), None);
        journal.emit(
            FromAgent::Effect {
                task: "t".into(),
                effect: crate::messages::EffectKind::Now,
                value: 1.5.into(),
                key: None,
            },
            None,
        );
        let sent = drain(&driver);
        assert_eq!((sent[1].pos, sent[1].task_step), (Some(2), Some(2)));
        let json = serde_json::to_value(&sent[1]).unwrap();
        assert_eq!(
            (
                json["type"].as_str(),
                json["effect"].as_str(),
                json["value"].as_f64()
            ),
            (Some("EFFECT"), Some("NOW"), Some(1.5))
        );
    }

    #[test]
    fn probes_and_requests_are_never_numbered_or_retained() {
        let driver = driver();
        live(&driver);
        let journal = driver.journal();
        journal.emit(log("p-1"), None);
        journal.emit(FromAgent::Completed { task: "p-1".into() }, None);
        journal.emit(
            FromAgent::AssignRequest {
                reference: None,
                parent_step: Some(3),
                call_key: None,
                args: Default::default(),
                action: Some("a".into()),
                action_hash: None,
                implementation: None,
                agent: None,
                interface: None,
                parent: Some("t".into()),
                dependency: None,
                method: None,
                resolution: None,
                hooks: None,
                capture: None,
                step: None,
                prompt: None,
                state_requires: vec![],
            },
            None,
        );
        journal.emit(log("t"), None);
        let sent = drain(&driver);
        assert_eq!(
            sent.iter()
                .map(|e| (e.pos, e.task_step))
                .collect::<Vec<_>>(),
            [(None, None), (None, None), (None, None), (Some(1), Some(1))]
        );
        assert_eq!(sent[2].seq, None, "a request has no seq");
        assert_eq!(
            driver.with_session(|session| session.outbox().retained_len()),
            1
        );
    }
}
