//! The rekuest agent: registers the app's actions over the `/agi` websocket
//! and runs assignments as they arrive.
//!
//! Connection lifecycle:
//! 1. connect, send `REGISTER` (token + declaration), wait for `INIT`
//! 2. re-send unacknowledged terminal reports, answer inquiries
//! 3. serve: answer `HEARTBEAT`s, spawn a tokio task per `ASSIGN`, abort on
//!    `CANCEL`/`INTERRUPT`
//! 4. on disconnect, reconnect with exponential backoff; running tasks keep
//!    going and report once the next connection is up.

use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fakts::TokenLoader;
use futures::{FutureExt, SinkExt, StreamExt};
use rand::Rng;
use tokio::task::AbortHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::action::{ActionError, Registry};
use crate::context::Context;
use crate::definition::AgentDeclaration;
use crate::messages::{parse_to_agent, Assign, Envelope, FromAgent, ToAgent};
use crate::outbox::Outbox;
use crate::task::Task;

/// Close codes after which reconnecting is pointless.
const CLOSE_BLOCKED: u16 = 4003;
const CLOSE_BUSY: u16 = 4004;
const CLOSE_KICKED: u16 = 4005;

/// How many finished task ids are remembered to ignore duplicate assigns.
const FINISHED_MEMORY: usize = 2048;

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
}

impl AgentOptions {
    pub fn new(endpoint_url: impl Into<String>) -> Self {
        Self {
            endpoint_url: endpoint_url.into(),
            name: None,
            description: None,
            force: false,
            policy: ConnectionPolicy::default(),
        }
    }
}

enum SessionEnd {
    /// The connection dropped; reconnect.
    Dropped(String),
    /// The server asked us to reconnect later.
    Bounce(Option<Duration>),
}

type Fatal = AgentError;

#[derive(Default)]
struct Tasks {
    running: HashMap<String, AbortHandle>,
    finished: VecDeque<String>,
    finished_set: HashSet<String>,
}

impl Tasks {
    fn finish(&mut self, task: &str) -> bool {
        let was_running = self.running.remove(task).is_some();
        if self.finished_set.insert(task.to_owned()) {
            self.finished.push_back(task.to_owned());
            if self.finished.len() > FINISHED_MEMORY {
                if let Some(old) = self.finished.pop_front() {
                    self.finished_set.remove(&old);
                }
            }
        }
        was_running
    }
}

/// Serves a [`Registry`] of actions to a rekuest server.
pub struct Agent {
    options: AgentOptions,
    registry: Arc<Registry>,
    ctx: Context,
    tokens: Arc<dyn TokenLoader>,
    outbox: Arc<Outbox>,
    tasks: Arc<Mutex<Tasks>>,
    session_id: String,
}

impl Agent {
    pub fn new(
        options: AgentOptions,
        registry: Registry,
        ctx: Context,
        tokens: Arc<dyn TokenLoader>,
    ) -> Self {
        Self {
            options,
            registry: Arc::new(registry),
            ctx,
            tokens,
            outbox: Arc::new(Outbox::new()),
            tasks: Arc::default(),
            session_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// What this agent registers.
    pub fn declaration(&self) -> AgentDeclaration {
        AgentDeclaration::new(
            self.options.name.clone(),
            self.options.description.clone(),
            self.registry.implementations(),
        )
    }

    /// Serve until a fatal error (or until the retry budget is exhausted).
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
            tracing::info!(
                "reconnecting in {delay:?} (attempt {attempt}/{})",
                policy.max_retries
            );
            tokio::time::sleep(delay).await;
        }
    }

    async fn session(&self) -> Result<SessionEnd, Fatal> {
        let token = self.tokens.get_token().await?;

        let (ws, _) = match tokio_tungstenite::connect_async(&self.options.endpoint_url).await {
            Ok(ws) => ws,
            Err(e) => {
                return Ok(SessionEnd::Dropped(format!(
                    "connect to {}: {e}",
                    self.options.endpoint_url
                )))
            }
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
            let frame = tokio::select! {
                Some(envelope) = rx.recv() => {
                    if let Err(e) = sink.send(Message::Text(to_json(&envelope))).await {
                        break SessionEnd::Dropped(format!("send: {e}"));
                    }
                    continue;
                }
                frame = tokio::time::timeout(IDLE_TIMEOUT, stream.next()) => frame,
            };

            let text = match frame {
                Err(_) => {
                    break SessionEnd::Dropped(
                        "no frame from the server within the idle timeout".into(),
                    )
                }
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
                        self.registry.len()
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
            ToAgent::Cancel { task } => {
                self.abort(&task, FromAgent::Cancelled { task: task.clone() })
            }
            ToAgent::Interrupt { task } => {
                self.abort(&task, FromAgent::Interrupted { task: task.clone() })
            }
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
        let state = self.tasks.lock().expect("tasks lock");
        for task in tasks {
            if state.running.contains_key(&task) {
                self.outbox.send(FromAgent::Progress {
                    task,
                    progress: None,
                    message: Some("still running".into()),
                });
            } else if !state.finished_set.contains(&task) {
                self.outbox.send(FromAgent::Critical {
                    task,
                    error: "the agent restarted and lost this task".into(),
                });
            }
        }
    }

    fn assign(&self, assign: Assign) {
        let mut state = self.tasks.lock().expect("tasks lock");
        let task_id = assign.task.clone();
        if state.running.contains_key(&task_id) || state.finished_set.contains(&task_id) {
            tracing::debug!("ignoring duplicate ASSIGN for task {task_id}");
            return;
        }
        let Some(action) = self.registry.get(&assign.interface) else {
            self.outbox.send(FromAgent::Critical {
                task: task_id,
                error: format!(
                    "this agent has no action with interface '{}'",
                    assign.interface
                ),
            });
            return;
        };

        let assign = Arc::new(assign);
        let args = assign.args.clone();
        let token = assign.token.clone();
        let task = Task::new(assign, self.outbox.clone());
        let outbox = self.outbox.clone();
        let tasks = self.tasks.clone();
        let ctx = self.ctx.clone();
        let id = task_id.clone();

        let run = async move {
            outbox.send(FromAgent::Progress {
                task: id.clone(),
                progress: Some(0),
                message: Some("Queued for running".into()),
            });
            let body = rath::with_task_token(token, action.run(args, ctx, task));
            let event = match AssertUnwindSafe(body).catch_unwind().await {
                Ok(Ok(())) => FromAgent::Completed { task: id.clone() },
                Ok(Err(ActionError::Failed(error))) => FromAgent::Failed {
                    task: id.clone(),
                    error,
                },
                Ok(Err(ActionError::Critical(error))) => FromAgent::Critical {
                    task: id.clone(),
                    error,
                },
                Err(panic) => FromAgent::Critical {
                    task: id.clone(),
                    error: format!("the action panicked: {}", panic_message(&panic)),
                },
            };
            // A cancelled task was already reported by `abort`.
            if tasks.lock().expect("tasks lock").finish(&id) {
                outbox.send(event);
            }
        };

        // The lock is held across the spawn so the task cannot finish before
        // it is registered as running.
        let handle = tokio::spawn(run);
        state.running.insert(task_id, handle.abort_handle());
    }

    fn abort(&self, task: &str, report: FromAgent) {
        let mut state = self.tasks.lock().expect("tasks lock");
        match state.running.get(task).cloned() {
            Some(handle) => {
                handle.abort();
                state.finish(task);
                self.outbox.send(report);
            }
            None => tracing::debug!("cannot stop task {task}: it is not running"),
        }
    }
}

fn to_json(envelope: &Envelope) -> String {
    serde_json::to_string(envelope).expect("messages serialize")
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}
