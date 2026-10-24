//! The rekuest agent: registers the app over the `/agi` websocket and runs
//! assignments as they arrive.
//!
//! The protocol is a [`rekuest_session::Session`], a sans-IO state machine;
//! this is its tokio driver. Connection lifecycle:
//! 1. connect, send `REGISTER` (token + declaration), wait for `INIT`
//! 2. re-send every numbered frame not covered by a `JOURNAL_ACK` (earlier
//!    sessions' first), answer inquiries
//! 3. after the *first* `INIT` only: activate (startup hooks, `SESSION_INIT`,
//!    background hooks). Activation runs beside the read loop so heartbeats
//!    keep being answered; task messages wait until it is done.
//! 4. serve: answer `HEARTBEAT`s, hand `ASSIGN`/`CANCEL`/`PAUSE`/… to the
//!    [`Executor`]
//! 5. on disconnect, reconnect with exponential backoff; running tasks keep
//!    going and report once the next connection is up. Reconnecting never
//!    re-runs startup hooks.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fakts::TokenLoader;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use rekuest_session::{to_text, Event, Session, SessionConfig, SessionError};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use crate::action::Registry;
use crate::context::Context;
use crate::definition::AgentDeclaration;
use crate::emit::Emitter;
use crate::executor::Executor;
use crate::journal::{Journal, JournalEntry};
use crate::messages::{FromAgent, ToAgent};
use crate::transport::{Connector, Direct, Ws};

pub use rekuest_session::ConnectionPolicy;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("could not get a token: {0}")]
    Token(#[from] fakts::FaktsError),
    #[error("the server refused the registration: {0}")]
    Refused(String),
    #[error("the agent was kicked: {}", .0.as_deref().unwrap_or("no reason given"))]
    Kicked(Option<String>),
    #[error("the server closed the connection with code {code}: {reason}")]
    Closed { code: u16, reason: String },
    #[error("giving up after {0} failed connection attempts")]
    Exhausted(usize),
    #[error("the agent could not start: {0}")]
    Activation(String),
}

impl From<SessionError> for AgentError {
    fn from(error: SessionError) -> Self {
        match error {
            SessionError::Refused(e) => Self::Refused(e),
            SessionError::Kicked(reason) => Self::Kicked(reason),
            SessionError::Closed { code, reason } => Self::Closed { code, reason },
            SessionError::Exhausted(n) => Self::Exhausted(n),
            SessionError::Activation(e) => Self::Activation(e),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentOptions {
    /// `ws(s)://…/agi`
    pub endpoint_url: String,
    /// Shown in the UI; the server falls back to the client id.
    pub name: Option<String>,
    pub description: Option<String>,
    /// Take over from another connection of the same agent.
    pub force: bool,
    pub policy: ConnectionPolicy,
    /// Opens the connection the websocket runs over: plain TCP by default,
    /// [`HttpProxy`](crate::transport::HttpProxy) for the mesh sidecar's
    /// proxy, or the app's own (e.g. through a mesh node it runs).
    pub connector: Arc<dyn Connector>,
    /// The local SQLite journal (write-ahead: what the server has not
    /// acknowledged is re-sent after a restart). `None` keeps it in memory
    /// only. Needs the `wal` feature.
    pub journal_path: Option<std::path::PathBuf>,
    /// Acknowledged journal entries older than this are deleted; `None` keeps them.
    pub journal_retention: Option<Duration>,
}

impl AgentOptions {
    pub fn new(endpoint_url: impl Into<String>) -> Self {
        Self {
            endpoint_url: endpoint_url.into(),
            name: None,
            description: None,
            force: false,
            policy: ConnectionPolicy::default(),
            connector: Arc::new(Direct),
            // Without the `wal` feature there is no SQLite journal to keep.
            journal_path: cfg!(feature = "wal").then(|| "agent_data.db".into()),
            journal_retention: Some(Duration::from_secs(7 * 24 * 3600)),
        }
    }
}

/// The session, shared by the driver and every task that reports into it.
///
/// Reports lock it from inside the journal's lock, so the driver never calls
/// into the executor or the journal while holding it.
struct Shared {
    session: Mutex<Session>,
    /// A report was queued: the driver sends it. A stored permit, so a
    /// report made while the driver is busy is not missed.
    wake: tokio::sync::Notify,
}

impl Shared {
    fn new(session: Session) -> Self {
        Self {
            session: Mutex::new(session),
            wake: tokio::sync::Notify::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Session> {
        self.session.lock().expect("session lock")
    }
}

impl Emitter for Shared {
    fn emit(&self, message: FromAgent, _action_key: Option<&str>) {
        self.lock().send(message);
        self.wake.notify_one();
    }

    fn emit_entry(&self, message: FromAgent, _action_key: Option<&str>, entry: &JournalEntry) {
        self.lock().send_entry(message, entry);
        self.wake.notify_one();
    }
}

/// Serves an app's actions to a rekuest server.
pub struct Agent {
    options: AgentOptions,
    executor: Executor,
    tokens: Arc<dyn TokenLoader>,
    shared: Arc<Shared>,
    /// Numbers every report of the session in front of the session's outbox.
    journal: Arc<Journal>,
    #[cfg(feature = "wal")]
    store: Option<crate::store::HistoryStore>,
    /// Earlier sessions' unacknowledged entries are loaded once.
    backlog_loaded: std::sync::atomic::AtomicBool,
}

impl Agent {
    pub fn new(
        options: AgentOptions,
        registry: Registry,
        ctx: Context,
        tokens: Arc<dyn TokenLoader>,
    ) -> Self {
        // One session per agent: REGISTER announces it, SESSION_INIT and every
        // STATE_PATCH carry it.
        let session_id = uuid::Uuid::new_v4().to_string();
        #[cfg(feature = "wal")]
        let store = options.journal_path.as_ref().and_then(|path| {
            match crate::store::HistoryStore::open(path) {
                Ok(store) => Some(store),
                Err(e) => {
                    tracing::error!(
                        "could not open the journal at {}: {e:#}; keeping it in memory",
                        path.display()
                    );
                    None
                }
            }
        });
        #[cfg(feature = "wal")]
        let outbox =
            rekuest_session::Outbox::with_limit(rekuest_session::RETAIN_LIMIT, store.is_some());
        #[cfg(not(feature = "wal"))]
        let outbox = rekuest_session::Outbox::new();
        let session = Session::with_outbox(
            SessionConfig {
                force: options.force,
                policy: options.policy.clone(),
                ..SessionConfig::new(session_id.clone(), rand::random())
            },
            outbox,
        );
        let shared = Arc::new(Shared::new(session));
        #[cfg(feature = "wal")]
        let sink = store
            .clone()
            .map(|s| Arc::new(s) as Arc<dyn crate::journal::JournalSink>);
        #[cfg(not(feature = "wal"))]
        let sink = None;
        let journal = Arc::new(Journal::new(shared.clone(), sink, Some(session_id.clone())));
        Self {
            options,
            executor: Executor::with_session(
                registry,
                ctx,
                journal.clone(),
                None,
                Some(session_id),
            ),
            tokens,
            shared,
            journal,
            #[cfg(feature = "wal")]
            store,
            backlog_loaded: Default::default(),
        }
    }

    pub fn executor(&self) -> &Executor {
        &self.executor
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// What this agent registers.
    pub fn declaration(&self) -> AgentDeclaration {
        self.executor
            .registry()
            .declaration(self.options.name.clone(), self.options.description.clone())
    }

    /// Serve until a fatal error (or until the retry budget is exhausted).
    /// Call [`Agent::shutdown`] afterwards to run shutdown hooks.
    pub async fn run(&self) -> Result<(), AgentError> {
        fakts::install_crypto_provider();
        self.load_backlog().await;
        let start = Instant::now();
        let now = || start.elapsed();
        let mut sink: Option<SplitSink<Ws, Message>> = None;
        let mut stream: Option<SplitStream<Ws>> = None;
        let (activated_tx, mut activated) = tokio::sync::mpsc::unbounded_channel();

        loop {
            let (events, frames, deadline) = {
                let mut session = self.shared.lock();
                session.handle_timeout(now());
                let events: Vec<Event> = std::iter::from_fn(|| session.poll_event()).collect();
                let frames: Vec<String> = std::iter::from_fn(|| session.poll_transmit())
                    .map(|envelope| to_text(&envelope))
                    .collect();
                (events, frames, session.poll_timeout())
            };
            let busy = !events.is_empty() || !frames.is_empty();

            if let Some(open) = sink.as_mut() {
                for frame in frames {
                    if let Err(e) = open.send(Message::Text(frame)).await {
                        self.shared
                            .lock()
                            .handle_disconnect(now(), &format!("send: {e}"));
                        break;
                    }
                }
            }

            for event in events {
                match event {
                    Event::Connect => {
                        let token = self.tokens.get_token().await?;
                        let connected = crate::transport::connect_ws(
                            &self.options.endpoint_url,
                            self.options.connector.as_ref(),
                        )
                        .await;
                        match connected {
                            Ok((ws, _)) => {
                                let (open, incoming) = ws.split();
                                sink = Some(open);
                                stream = Some(incoming);
                                let declaration = self.declaration();
                                self.shared.lock().connected(now(), token, declaration);
                            }
                            Err(e) => self.shared.lock().connect_failed(
                                now(),
                                &format!("connect to {}: {e}", self.options.endpoint_url),
                            ),
                        }
                    }
                    Event::Close => {
                        stream = None;
                        if let Some(mut open) = sink.take() {
                            let _ = open.close().await;
                        }
                    }
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
                    Event::ReloadJournal => {
                        let entries = self.read_unacked().await;
                        self.shared.lock().reloaded(entries);
                    }
                    Event::Inquiries(tasks) => self.answer_inquiries(tasks),
                    Event::Activate => {
                        let executor = self.executor.clone();
                        let done = activated_tx.clone();
                        tokio::spawn(async move {
                            let result = executor.activate().await.map_err(|e| format!("{e:#}"));
                            let _ = done.send(result);
                        });
                    }
                    Event::Message(message) => self.dispatch(message),
                    Event::Acked { session, pos } => self.record_ack(session, pos),
                    Event::MalformedAssign { task, error } => {
                        self.journal.emit(FromAgent::Critical { task, error }, None)
                    }
                    Event::Ended(error) => return Err(error.into()),
                }
            }
            if busy {
                // Acting may have queued more: REGISTER, reports, the end.
                continue;
            }

            let incoming = async {
                match stream.as_mut() {
                    Some(stream) => stream.next().await,
                    None => std::future::pending().await,
                }
            };
            let sleep = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(start + at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                frame = incoming => {
                    let mut session = self.shared.lock();
                    match frame {
                        None => session.handle_disconnect(now(), "stream ended"),
                        Some(Err(e)) => session.handle_disconnect(now(), &format!("read: {e}")),
                        Some(Ok(Message::Text(text))) => session.handle_text(now(), &text),
                        Some(Ok(Message::Close(frame))) => {
                            let (code, reason) = frame
                                .map(|f| (u16::from(f.code), f.reason.to_string()))
                                .unwrap_or((1005, String::new()));
                            session.handle_close(now(), code, &reason);
                        }
                        Some(Ok(_)) => {}
                    }
                }
                _ = self.shared.wake.notified() => {}
                Some(result) = activated.recv() => self.shared.lock().activated(result),
                _ = sleep => {}
            }
        }
    }

    /// Run shutdown hooks and flush state changes and the journal.
    pub async fn shutdown(&self) {
        self.executor.teardown().await;
        self.journal.flush(crate::executor::FLUSH_TIMEOUT).await;
    }

    /// Once: queue what earlier sessions of this agent never got acknowledged
    /// (from the local journal) ahead of this session, and start pruning.
    async fn load_backlog(&self) {
        if self
            .backlog_loaded
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        #[cfg(feature = "wal")]
        if let Some(store) = &self.store {
            let session_id = self.shared.lock().session_id().to_owned();
            match store.unacked_entries(Some(&session_id)).await {
                Ok(entries) if !entries.is_empty() => {
                    tracing::info!(
                        "re-sending {} journal entries of earlier sessions",
                        entries.len()
                    );
                    self.shared.lock().reloaded(Some(entries));
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("could not read the journal backlog: {e:#}"),
            }
            if let Some(retention) = self.options.journal_retention {
                let store = store.clone();
                tokio::spawn(async move {
                    loop {
                        let before =
                            chrono::Utc::now().timestamp_millis() - retention.as_millis() as i64;
                        match store.prune_acked(before).await {
                            Ok(0) => {}
                            Ok(n) => tracing::debug!("pruned {n} acknowledged journal entries"),
                            Err(e) => tracing::warn!("could not prune the journal: {e:#}"),
                        }
                        tokio::time::sleep(Duration::from_secs(3600)).await;
                    }
                });
            }
        }
    }

    /// The frames dropped from memory, read back from the local journal.
    async fn read_unacked(&self) -> Option<Vec<JournalEntry>> {
        #[cfg(feature = "wal")]
        if let Some(store) = &self.store {
            self.journal.flush(crate::executor::FLUSH_TIMEOUT).await;
            return match store.unacked_entries(None).await {
                Ok(entries) => Some(entries),
                Err(e) => {
                    tracing::warn!("could not read the journal to resend it: {e:#}");
                    None
                }
            };
        }
        None
    }

    fn record_ack(&self, session: String, pos: u64) {
        #[cfg(feature = "wal")]
        if let Some(store) = self.store.clone() {
            tokio::spawn(async move {
                if let Err(e) = store.set_acked(&session, pos).await {
                    tracing::warn!("could not record the journal ack: {e:#}");
                }
            });
        }
        #[cfg(not(feature = "wal"))]
        let _ = (session, pos);
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

    fn shared() -> (Arc<Shared>, Journal) {
        let shared = Arc::new(Shared::new(Session::new(SessionConfig::new("s", 1))));
        let journal = Journal::new(shared.clone(), None, Some("s".into()));
        (shared, journal)
    }

    /// Connected and past INIT: reports go out.
    fn live(shared: &Shared) {
        let mut session = shared.lock();
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
    }

    fn drain(shared: &Shared) -> Vec<Envelope> {
        let mut session = shared.lock();
        std::iter::from_fn(|| session.poll_transmit()).collect()
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
        let (shared, journal) = shared();
        live(&shared);
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
        let sent = drain(&shared);
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
        let (shared, journal) = shared();
        live(&shared);
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
        let sent = drain(&shared);
        assert_eq!(
            sent.iter()
                .map(|e| (e.pos, e.task_step))
                .collect::<Vec<_>>(),
            [(None, None), (None, None), (None, None), (Some(1), Some(1))]
        );
        assert_eq!(sent[2].seq, None, "a request has no seq");
        assert_eq!(shared.lock().outbox().retained_len(), 1);
    }
}
