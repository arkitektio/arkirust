//! The rekuest agent: registers the app over the `/agi` websocket and runs
//! assignments as they arrive.
//!
//! Connection lifecycle:
//! 1. connect, send `REGISTER` (token + declaration), wait for `INIT`
//! 2. re-send unacknowledged terminal reports, answer inquiries
//! 3. after the *first* `INIT` only: activate (startup hooks, `SESSION_INIT`,
//!    background hooks). Activation runs beside the read loop so heartbeats
//!    keep being answered; assignments wait until it is done.
//! 4. serve: answer `HEARTBEAT`s, hand `ASSIGN`/`CANCEL`/`PAUSE`/… to the
//!    [`Executor`]
//! 5. on disconnect, reconnect with exponential backoff; running tasks keep
//!    going and report once the next connection is up. Reconnecting never
//!    re-runs startup hooks.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fakts::TokenLoader;
use futures::{SinkExt, StreamExt};
use rand::Rng;
use tokio_tungstenite::tungstenite::Message;

use crate::action::Registry;
use crate::context::Context;
use crate::definition::AgentDeclaration;
use crate::executor::Executor;
use crate::messages::{parse_to_agent, Assign, Envelope, FromAgent, ToAgent};
use crate::outbox::Outbox;

/// Close codes after which reconnecting is pointless.
const CLOSE_BLOCKED: u16 = 4003;
const CLOSE_BUSY: u16 = 4004;
const CLOSE_KICKED: u16 = 4005;

/// No frame for this long means the connection is dead (the server heartbeats).
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

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

/// Exponential backoff between reconnect attempts.
#[derive(Debug, Clone)]
pub struct ConnectionPolicy {
    pub max_retries: usize,
    pub initial: Duration,
    pub factor: f64,
    pub max: Duration,
    pub jitter: f64,
    /// A connection that stayed up this long resets the retry budget.
    pub reset_after: Duration,
}

impl Default for ConnectionPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            initial: Duration::from_secs(1),
            factor: 2.0,
            max: Duration::from_secs(60),
            jitter: 0.1,
            reset_after: Duration::from_secs(30),
        }
    }
}

impl ConnectionPolicy {
    fn delay_for(&self, attempt: usize) -> Duration {
        let base = self.initial.as_secs_f64() * self.factor.powi(attempt.saturating_sub(1) as i32);
        let base = base.min(self.max.as_secs_f64());
        let jitter = base * self.jitter * rand::thread_rng().gen_range(-1.0..=1.0);
        Duration::from_secs_f64((base + jitter).max(0.0))
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
    /// HTTP proxy to reach the endpoint through (the mesh sidecar).
    pub proxy: Option<String>,
}

impl AgentOptions {
    pub fn new(endpoint_url: impl Into<String>) -> Self {
        Self {
            endpoint_url: endpoint_url.into(),
            name: None,
            description: None,
            force: false,
            policy: ConnectionPolicy::default(),
            proxy: None,
        }
    }
}

enum SessionEnd {
    /// The connection dropped; reconnect.
    Dropped(String),
    /// The server asked us to reconnect later.
    Bounce(Option<Duration>),
}

/// Activation happens once per agent; until it is done, assignments wait.
#[derive(Default)]
enum Activation {
    #[default]
    NotStarted,
    Running(Vec<Assign>),
    Done,
    Failed(String),
}

/// Serves an app's actions to a rekuest server.
pub struct Agent {
    options: AgentOptions,
    executor: Executor,
    tokens: Arc<dyn TokenLoader>,
    outbox: Arc<Outbox>,
    activation: Arc<Mutex<Activation>>,
    activation_changed: Arc<tokio::sync::Notify>,
    session_id: String,
}

impl Agent {
    pub fn new(options: AgentOptions, registry: Registry, ctx: Context, tokens: Arc<dyn TokenLoader>) -> Self {
        let outbox = Arc::new(Outbox::new());
        // One session per agent: REGISTER announces it, SESSION_INIT and every
        // STATE_PATCH carry it.
        let session_id = uuid::Uuid::new_v4().to_string();
        Self {
            options,
            executor: Executor::with_session(registry, ctx, outbox.clone(), None, Some(session_id.clone())),
            tokens,
            outbox,
            activation: Arc::default(),
            activation_changed: Arc::default(),
            session_id,
        }
    }

    pub fn executor(&self) -> &Executor {
        &self.executor
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
        let policy = self.options.policy.clone();
        let mut attempt = 0usize;
        loop {
            let started = Instant::now();
            match self.session().await? {
                SessionEnd::Bounce(delay) => {
                    tracing::info!("server bounced the agent, reconnecting");
                    attempt = 0;
                    tokio::time::sleep(delay.unwrap_or(Duration::from_secs(1))).await;
                    continue;
                }
                SessionEnd::Dropped(reason) => tracing::warn!("agent connection lost: {reason}"),
            }
            if started.elapsed() >= policy.reset_after {
                attempt = 0;
            }
            attempt += 1;
            if attempt > policy.max_retries {
                return Err(AgentError::Exhausted(attempt - 1));
            }
            let delay = policy.delay_for(attempt);
            tracing::info!("reconnecting in {delay:?} (attempt {attempt}/{})", policy.max_retries);
            tokio::time::sleep(delay).await;
        }
    }

    /// Run shutdown hooks and flush state changes.
    pub async fn shutdown(&self) {
        self.executor.teardown().await;
    }

    /// Start activation after the first INIT; later INITs do nothing.
    fn start_activation(&self) {
        {
            let mut activation = self.activation.lock().expect("activation lock");
            if !matches!(*activation, Activation::NotStarted) {
                return;
            }
            *activation = Activation::Running(vec![]);
        }
        let executor = self.executor.clone();
        let activation = self.activation.clone();
        let changed = self.activation_changed.clone();
        tokio::spawn(async move {
            let result = executor.activate().await;
            let next = match &result {
                Ok(()) => Activation::Done,
                Err(e) => Activation::Failed(format!("{e:#}")),
            };
            let previous = std::mem::replace(&mut *activation.lock().expect("activation lock"), next);
            if let (Ok(()), Activation::Running(buffered)) = (&result, previous) {
                for assign in buffered {
                    executor.assign(assign);
                }
            }
            changed.notify_waiters();
        });
    }

    fn activation_failure(&self) -> Option<String> {
        match &*self.activation.lock().expect("activation lock") {
            Activation::Failed(e) => Some(e.clone()),
            _ => None,
        }
    }

    /// Run now if activated, else hold until activation is done.
    fn assign(&self, assign: Assign) {
        let mut activation = self.activation.lock().expect("activation lock");
        match &mut *activation {
            Activation::Done => {
                drop(activation);
                self.executor.assign(assign);
            }
            Activation::Running(buffered) => buffered.push(assign),
            Activation::Failed(_) => tracing::warn!("dropping an assignment: the agent failed to start"),
            // Cannot happen (assignments come after INIT), but never lose one.
            Activation::NotStarted => *activation = Activation::Running(vec![assign]),
        }
    }

    async fn session(&self) -> Result<SessionEnd, AgentError> {
        let token = self.tokens.get_token().await?;

        let (ws, _) = match crate::transport::connect_ws(&self.options.endpoint_url, self.options.proxy.as_deref()).await {
            Ok(ws) => ws,
            Err(e) => return Ok(SessionEnd::Dropped(format!("connect to {}: {e}", self.options.endpoint_url))),
        };
        let (mut sink, mut stream) = ws.split();

        let register = Envelope::new(FromAgent::Register {
            token,
            force: self.options.force,
            session_id: Some(self.session_id.clone()),
            declaration: self.declaration(),
        });
        if let Err(e) = sink.send(Message::Text(to_json(&register))).await {
            return Ok(SessionEnd::Dropped(format!("send REGISTER: {e}")));
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Envelope>();
        let mut initialized = false;
        let mut early: Vec<ToAgent> = vec![];

        let end = loop {
            let activation_changed = self.activation_changed.notified();
            let frame = tokio::select! {
                Some(envelope) = rx.recv() => {
                    if let Err(e) = sink.send(Message::Text(to_json(&envelope))).await {
                        break SessionEnd::Dropped(format!("send: {e}"));
                    }
                    continue;
                }
                _ = activation_changed => {
                    if let Some(error) = self.activation_failure() {
                        self.outbox.detach();
                        return Err(AgentError::Activation(error));
                    }
                    continue;
                }
                frame = tokio::time::timeout(IDLE_TIMEOUT, stream.next()) => frame,
            };

            let text = match frame {
                Err(_) => break SessionEnd::Dropped("no frame from the server within the idle timeout".into()),
                Ok(None) => break SessionEnd::Dropped("stream ended".into()),
                Ok(Some(Err(e))) => break SessionEnd::Dropped(format!("read: {e}")),
                Ok(Some(Ok(Message::Text(text)))) => text,
                Ok(Some(Ok(Message::Close(frame)))) => {
                    let (code, reason) = frame
                        .map(|f| (u16::from(f.code), f.reason.to_string()))
                        .unwrap_or((1005, String::new()));
                    if matches!(code, CLOSE_BLOCKED | CLOSE_BUSY | CLOSE_KICKED) {
                        self.outbox.detach();
                        return Err(AgentError::Closed { code, reason });
                    }
                    break SessionEnd::Dropped(format!("closed with code {code} {reason}"));
                }
                Ok(Some(Ok(_))) => continue,
            };

            let message = match parse_to_agent(&text) {
                Ok(message) => message,
                Err((raw, e)) => {
                    self.reject_frame(raw, e);
                    continue;
                }
            };

            match message {
                ToAgent::Heartbeat {} => {
                    let answer = Envelope::new(FromAgent::HeartbeatAnswer {});
                    if let Err(e) = sink.send(Message::Text(to_json(&answer))).await {
                        break SessionEnd::Dropped(format!("send: {e}"));
                    }
                }
                ToAgent::Init {
                    agent,
                    inquiries,
                    diagnostics,
                    ..
                } => {
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
                    self.outbox.attach(tx.clone());
                    self.outbox.resend_unacked();
                    self.answer_inquiries(inquiries.into_iter().map(|i| i.task));
                    self.start_activation();
                    initialized = true;
                    for message in early.drain(..) {
                        self.dispatch(message);
                    }
                }
                ToAgent::ProtocolError { error } => {
                    self.outbox.detach();
                    return Err(AgentError::Refused(error));
                }
                ToAgent::Kick { reason } => {
                    self.outbox.detach();
                    return Err(AgentError::Kicked(reason));
                }
                ToAgent::Bounce { duration } => {
                    break SessionEnd::Bounce(duration.map(Duration::from_secs));
                }
                other if !initialized => early.push(other),
                other => self.dispatch(other),
            }
        };
        self.outbox.detach();
        let _ = sink.close().await;
        Ok(end)
    }

    fn dispatch(&self, message: ToAgent) {
        match message {
            ToAgent::Assign(assign) => self.assign(*assign),
            ToAgent::Cancel { task } => self.executor.cancel(&task),
            ToAgent::Interrupt { task } => self.executor.interrupt(&task),
            ToAgent::Pause { task } => self.executor.pause(&task),
            ToAgent::Resume { task, step } => self.executor.resume(&task, step),
            ToAgent::EventAck { event, seq, .. } => self.outbox.ack(event.as_deref(), seq),
            ToAgent::Unknown => tracing::debug!("ignoring an unsupported message"),
            other => tracing::debug!("ignoring {other:?}"),
        }
    }

    /// A frame that did not parse. A broken ASSIGN is answered so the caller
    /// is not left waiting; anything else is logged.
    fn reject_frame(&self, raw: Option<serde_json::Value>, error: serde_json::Error) {
        let task = raw
            .as_ref()
            .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("ASSIGN"))
            .and_then(|v| v.get("task"))
            .and_then(|t| t.as_str());
        match task {
            Some(task) => self.outbox.send(FromAgent::Critical {
                task: task.to_owned(),
                error: format!("malformed ASSIGN: {error}"),
            }),
            None => tracing::warn!("could not parse a server frame: {error}"),
        }
    }

    fn answer_inquiries(&self, tasks: impl Iterator<Item = String>) {
        for task in tasks {
            if self.executor.is_running(&task) {
                self.outbox.send(FromAgent::Progress {
                    task,
                    progress: None,
                    message: Some("still running".into()),
                });
            } else if !self.executor.has_finished(&task) {
                self.outbox.send(FromAgent::Critical {
                    task,
                    error: "the agent restarted and lost this task".into(),
                });
            }
        }
    }
}

fn to_json(envelope: &Envelope) -> String {
    serde_json::to_string(envelope).expect("messages serialize")
}
