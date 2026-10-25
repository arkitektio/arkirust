//! The rekuest agent: registers the app over the `/agi` websocket and runs
//! assignments as they arrive.
//!
//! The protocol is a [`rekuest_session::Session`], a sans-IO state machine,
//! and [`Driver`] wires it to the executor; this adds the tokio websocket.
//! Connection lifecycle:
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

use std::sync::Arc;
use std::time::Duration;

use fakts::TokenLoader;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use rekuest_session::{Event, SessionConfig};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use crate::driver::Driver;
use crate::executor::Executor;
use crate::journal::{Journal, JournalEntry};
use crate::transport::{Connector, Direct, Ws};
use arkitekt_spec::AgentDeclaration;
use arkitekt_spec::Context;
use arkitekt_spec::Registry;

pub use crate::agent_error::AgentError;
pub use rekuest_session::ConnectionPolicy;

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

/// Serves an app's actions to a rekuest server.
pub struct Agent {
    options: AgentOptions,
    driver: Driver,
    tokens: Arc<dyn TokenLoader>,
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
        #[cfg(feature = "wal")]
        let sink = store
            .clone()
            .map(|s| Arc::new(s) as Arc<dyn crate::journal::JournalSink>);
        #[cfg(not(feature = "wal"))]
        let sink = None;
        let config = SessionConfig {
            force: options.force,
            policy: options.policy.clone(),
            ..SessionConfig::new(session_id, rand::random())
        };
        Self {
            options,
            driver: Driver::new(registry, ctx, config, outbox, sink),
            tokens,
            #[cfg(feature = "wal")]
            store,
            backlog_loaded: Default::default(),
        }
    }

    pub fn executor(&self) -> &Executor {
        self.driver.executor()
    }

    pub fn journal(&self) -> &Journal {
        self.driver.journal()
    }

    /// What this agent registers.
    pub fn declaration(&self) -> AgentDeclaration {
        self.driver
            .declaration(self.options.name.clone(), self.options.description.clone())
    }

    /// Serve until a fatal error (or until the retry budget is exhausted).
    /// Call [`Agent::shutdown`] afterwards to run shutdown hooks.
    pub async fn run(&self) -> Result<(), AgentError> {
        fakts::install_crypto_provider();
        self.load_backlog().await;
        let start = Instant::now();
        let now = || start.elapsed();
        // A run after an earlier one ended (or stopped while connecting) starts over.
        self.driver.with_session(|session| session.restart(now()));
        let mut sink: Option<SplitSink<Ws, Message>> = None;
        let mut stream: Option<SplitStream<Ws>> = None;

        loop {
            let polled = self.driver.poll(now());
            let busy = !polled.events.is_empty() || !polled.frames.is_empty();

            if let Some(open) = sink.as_mut() {
                for frame in polled.frames {
                    if let Err(e) = open.send(Message::Text(frame)).await {
                        self.driver.with_session(|session| {
                            session.handle_disconnect(now(), &format!("send: {e}"))
                        });
                        break;
                    }
                }
            }

            for event in polled.events {
                let Some(event) = self.driver.act(event) else {
                    continue;
                };
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
                                self.driver.with_session(|session| {
                                    session.connected(now(), token, declaration)
                                });
                            }
                            Err(e) => self.driver.with_session(|session| {
                                session.connect_failed(
                                    now(),
                                    &format!("connect to {}: {e}", self.options.endpoint_url),
                                )
                            }),
                        }
                    }
                    Event::Close => {
                        stream = None;
                        if let Some(mut open) = sink.take() {
                            let _ = open.close().await;
                        }
                    }
                    Event::ReloadJournal => {
                        let entries = self.read_unacked().await;
                        self.driver
                            .with_session(|session| session.reloaded(entries));
                    }
                    Event::Acked { session, pos } => self.record_ack(session, pos),
                    Event::Ended(error) => return Err(error.into()),
                    _ => {}
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
                match polled.deadline {
                    Some(at) => tokio::time::sleep_until(start + at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                frame = incoming => self.driver.with_session(|session| match frame {
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
                }),
                _ = self.driver.woken() => {}
                _ = sleep => {}
            }
        }
    }

    /// Run shutdown hooks and flush state changes and the journal.
    pub async fn shutdown(&self) {
        self.executor().teardown().await;
        self.journal().flush(crate::executor::FLUSH_TIMEOUT).await;
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
            let session_id = self
                .driver
                .with_session(|session| session.session_id().to_owned());
            match store.unacked_entries(Some(&session_id)).await {
                Ok(entries) if !entries.is_empty() => {
                    tracing::info!(
                        "re-sending {} journal entries of earlier sessions",
                        entries.len()
                    );
                    self.driver
                        .with_session(|session| session.reloaded(Some(entries)));
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
            self.journal().flush(crate::executor::FLUSH_TIMEOUT).await;
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
}
