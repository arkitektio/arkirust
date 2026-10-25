//! The rekuest agent: registers the app over the `/agi` websocket and runs
//! assignments as they arrive.
//!
//! The protocol is a [`rekuest_session::Session`], a sans-IO state machine,
//! [`Driver`] wires it to the executor and [`link::run`](crate::link::run)
//! keeps it up; this adds the tokio websocket and the SQLite journal.
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

use async_trait::async_trait;
use fakts::TokenLoader;
use futures::{SinkExt, StreamExt};
use rekuest_session::SessionConfig;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use crate::driver::Driver;
use crate::executor::Executor;
use crate::journal::{Journal, JournalEntry};
use crate::link::{self, Backlog, Dial, Incoming, Link, Socket};
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

    /// The session, executor and journal underneath.
    pub fn driver(&self) -> &Driver {
        &self.driver
    }

    /// Serve until a fatal error (or until the retry budget is exhausted).
    /// Call [`Agent::shutdown`] afterwards to run shutdown hooks.
    pub async fn run(&self) -> Result<(), AgentError> {
        fakts::install_crypto_provider();
        let start = Instant::now();
        let link = Link {
            driver: &self.driver,
            endpoint: &self.options.endpoint_url,
            tokens: self.tokens.as_ref(),
            backlog: Some(self),
            name: self.options.name.clone(),
            description: self.options.description.clone(),
        };
        let mut dial = Tungstenite {
            connector: self.options.connector.clone(),
        };
        link::run(&link, &mut dial, &|| start.elapsed()).await
    }

    /// Run shutdown hooks and flush state changes and the journal.
    pub async fn shutdown(&self) {
        self.executor().teardown().await;
        self.journal().flush(crate::executor::FLUSH_TIMEOUT).await;
    }
}

/// The SQLite journal (`wal`): earlier runs' backlog, reloads and acks.
#[async_trait]
impl Backlog for Agent {
    /// Once per agent: what earlier sessions never got acknowledged; also
    /// starts pruning what is acknowledged.
    async fn earlier(&self, session_id: &str) -> Vec<JournalEntry> {
        if self
            .backlog_loaded
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return vec![];
        }
        #[cfg(feature = "wal")]
        if let Some(store) = &self.store {
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
            return match store.unacked_entries(Some(session_id)).await {
                Ok(entries) => entries,
                Err(e) => {
                    tracing::warn!("could not read the journal backlog: {e:#}");
                    vec![]
                }
            };
        }
        let _ = session_id;
        vec![]
    }

    /// The frames dropped from memory, read back from the local journal.
    async fn unacked(&self) -> Option<Vec<JournalEntry>> {
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

    /// Recorded in the background: the link goes on meanwhile.
    async fn acked(&self, session: String, pos: u64) {
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

/// Websockets through tungstenite, over a stream `connector` opens.
struct Tungstenite {
    connector: Arc<dyn Connector>,
}

impl Dial for Tungstenite {
    type Socket<'a>
        = Ws
    where
        Self: 'a;

    async fn dial(&mut self, url: &str) -> Result<Ws, String> {
        crate::transport::connect_ws(url, self.connector.as_ref())
            .await
            .map(|(ws, _)| ws)
            .map_err(|e| e.to_string())
    }
}

impl Socket for Ws {
    /// tungstenite answers pings itself; binary frames and pongs are skipped.
    async fn next(&mut self) -> Result<Incoming, String> {
        loop {
            return match StreamExt::next(self).await {
                None => Ok(Incoming::Closed {
                    code: 1006,
                    reason: "stream ended".into(),
                }),
                Some(Err(e)) => Err(e.to_string()),
                Some(Ok(Message::Text(text))) => Ok(Incoming::Text(text)),
                Some(Ok(Message::Close(frame))) => {
                    let (code, reason) = frame
                        .map(|f| (u16::from(f.code), f.reason.to_string()))
                        .unwrap_or((1005, String::new()));
                    Ok(Incoming::Closed { code, reason })
                }
                Some(Ok(_)) => continue,
            };
        }
    }

    async fn send_text(&mut self, text: &str) -> Result<(), String> {
        self.send(Message::Text(text.to_owned()))
            .await
            .map_err(|e| e.to_string())
    }

    async fn pong(&mut self, payload: &[u8]) -> Result<(), String> {
        self.send(Message::Pong(payload.to_vec()))
            .await
            .map_err(|e| e.to_string())
    }

    async fn close(&mut self) {
        let _ = SinkExt::close(self).await;
    }
}
