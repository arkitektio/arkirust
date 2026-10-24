//! The agent's side of one `/agi` session, across connections.
//!
//! Connection lifecycle:
//! 1. [`Event::Connect`]: the driver opens a socket and gets a token, then
//!    calls [`Session::connected`], which queues `REGISTER`
//! 2. `INIT`: re-send every numbered frame not covered by a `JOURNAL_ACK`
//!    (earlier sessions' first; when frames were dropped from memory,
//!    [`Event::ReloadJournal`] first), answer inquiries
//! 3. after the *first* `INIT` only: [`Event::Activate`]. Messages about
//!    tasks (`ASSIGN`, `CANCEL`, `PAUSE`, `DECIDE`, …) wait, in order, until
//!    the driver reports [`Session::activated`]; heartbeats, acks and
//!    answers go on meanwhile
//! 4. serve: answer `HEARTBEAT`s, hand the rest on as [`Event::Message`]
//! 5. on disconnect, [`Event::Connect`] again after a backoff; numbered
//!    frames reported meanwhile are kept and re-sent. Reconnecting never
//!    activates again.
//!
//! Frames that arrive between `REGISTER` and `INIT` (or while the journal
//! is reloaded) are held and handed on after it, heartbeats excepted.

use alloc::collections::VecDeque;
use core::time::Duration;

use serde_json::Value;

use crate::entry::JournalEntry;
use crate::outbox::Outbox;
use crate::policy::ConnectionPolicy;
use crate::prelude::*;
use crate::protocol::definition::AgentDeclaration;
use crate::protocol::messages::{parse_to_agent, Diagnostic, Envelope, FromAgent, ToAgent};
use crate::rng::Rng;

/// Close codes after which reconnecting is pointless.
pub const CLOSE_BLOCKED: u16 = 4003;
pub const CLOSE_BUSY: u16 = 4004;
pub const CLOSE_KICKED: u16 = 4005;
/// A frame came before `REGISTER` was through: the server wants a fresh connection.
pub const CLOSE_BEFORE_REGISTRATION: u16 = 3004;

/// No frame from the server for this long means the connection is dead (the server heartbeats).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// How long to wait after a `BOUNCE` that names no duration.
const BOUNCE_DELAY: Duration = Duration::from_secs(1);

pub struct SessionConfig {
    /// One per agent run: `REGISTER` announces it, the journal numbers by it.
    pub session_id: String,
    /// Take over from another connection of the same agent.
    pub force: bool,
    pub policy: ConnectionPolicy,
    pub idle_timeout: Duration,
    /// Seeds backoff jitter and message ids: pass real randomness.
    pub seed: u64,
}

impl SessionConfig {
    pub fn new(session_id: impl Into<String>, seed: u64) -> Self {
        Self {
            session_id: session_id.into(),
            force: false,
            policy: ConnectionPolicy::default(),
            idle_timeout: IDLE_TIMEOUT,
            seed,
        }
    }
}

/// Why a session ended for good.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionError {
    /// `PROTOCOL_ERROR`: the server refused the registration.
    Refused(String),
    Kicked(Option<String>),
    /// A close code after which reconnecting is pointless.
    Closed {
        code: u16,
        reason: String,
    },
    /// Giving up after this many failed connection attempts.
    Exhausted(usize),
    /// The app could not start.
    Activation(String),
}

impl core::fmt::Display for SessionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Refused(e) => write!(f, "the server refused the registration: {e}"),
            Self::Kicked(reason) => write!(
                f,
                "the agent was kicked: {}",
                reason.as_deref().unwrap_or("no reason given")
            ),
            Self::Closed { code, reason } => {
                write!(
                    f,
                    "the server closed the connection with code {code}: {reason}"
                )
            }
            Self::Exhausted(n) => write!(f, "giving up after {n} failed connection attempts"),
            Self::Activation(e) => write!(f, "the agent could not start: {e}"),
        }
    }
}

/// What the driver has to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Open a connection, then [`Session::connected`] or [`Session::connect_failed`].
    Connect,
    /// Close the connection (if one is open); the session is done with it.
    Close,
    /// `INIT` arrived.
    Registered {
        agent: String,
        diagnostics: Vec<Diagnostic>,
    },
    /// Frames were dropped from memory: read the unacknowledged entries back
    /// from the local journal and hand them to [`Session::reloaded`].
    ReloadJournal,
    /// The server asks about these tasks: report the running ones, fail the
    /// ones this process never ran.
    Inquiries(Vec<String>),
    /// Start the app (startup hooks, `SESSION_INIT`, …), then
    /// [`Session::activated`]. Once per session.
    Activate,
    /// A server message for the app: task messages (`ASSIGN`, `CANCEL`,
    /// `INTERRUPT`, `PAUSE`, `RESUME`, `DECIDE`, `BLOK_REFUSED`; only once
    /// activated), `COLLECT`, answers to requests, …
    Message(ToAgent),
    /// Everything of `session` up to `pos` is persisted by the server.
    Acked { session: String, pos: u64 },
    /// An `ASSIGN` that did not parse: report the task as critical.
    MalformedAssign { task: String, error: String },
    /// The session is over; nothing else will happen.
    Ended(SessionError),
}

/// Activation happens once per session; until it is done, task messages wait.
enum Activation {
    NotStarted,
    Running(Vec<ToAgent>),
    Done,
}

enum Phase {
    /// No connection; the next attempt is at `at`.
    Waiting {
        at: Duration,
    },
    /// The driver is opening a connection.
    Connecting,
    /// `REGISTER` sent; frames before `INIT` are held.
    Registering {
        early: Vec<ToAgent>,
    },
    /// `INIT` arrived; waiting for the journal backlog.
    Reloading {
        early: Vec<ToAgent>,
        inquiries: Vec<String>,
    },
    Live,
    Ended,
}

/// See the module docs.
pub struct Session {
    config: SessionConfig,
    phase: Phase,
    outbox: Outbox,
    activation: Activation,
    /// Frames of the connection itself (`REGISTER`, `HEARTBEAT_ANSWER`), sent before the outbox's.
    control: VecDeque<Envelope>,
    events: VecDeque<Event>,
    /// Failed attempts since the last connection that held.
    attempt: usize,
    /// When the current attempt started.
    attempt_started: Duration,
    last_inbound: Duration,
    rng: Rng,
}

impl Session {
    /// A session that connects right away, keeping every unacknowledged frame in memory.
    pub fn new(config: SessionConfig) -> Self {
        Self::with_outbox(config, Outbox::new())
    }

    /// With `outbox`, e.g. one that may spill to a local journal ([`Outbox::with_limit`]).
    pub fn with_outbox(config: SessionConfig, outbox: Outbox) -> Self {
        let rng = Rng::new(config.seed);
        Self {
            config,
            phase: Phase::Waiting { at: Duration::ZERO },
            outbox,
            activation: Activation::NotStarted,
            control: VecDeque::new(),
            events: VecDeque::new(),
            attempt: 0,
            attempt_started: Duration::ZERO,
            last_inbound: Duration::ZERO,
            rng,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.config.session_id
    }

    pub fn outbox(&self) -> &Outbox {
        &self.outbox
    }

    /// Whether the session ended for good.
    pub fn is_ended(&self) -> bool {
        matches!(self.phase, Phase::Ended)
    }

    /// Whether frames reach the server now (after `INIT`, until the connection drops).
    pub fn is_live(&self) -> bool {
        matches!(self.phase, Phase::Live)
    }

    /// A fresh message id.
    pub fn mint_id(&mut self) -> String {
        self.rng.uuid().to_string()
    }

    // ---------------------------------------------------------------- inputs

    /// The connection is open: register over it.
    pub fn connected(&mut self, now: Duration, token: String, declaration: AgentDeclaration) {
        if !matches!(self.phase, Phase::Connecting) {
            return;
        }
        let register = FromAgent::Register {
            token,
            force: self.config.force,
            session_id: Some(self.config.session_id.clone()),
            features: vec![],
            declaration,
        };
        let id = self.mint_id();
        self.control.push_back(Envelope::with_id(id, register));
        self.last_inbound = now;
        self.phase = Phase::Registering { early: vec![] };
    }

    /// Opening the connection failed.
    pub fn connect_failed(&mut self, now: Duration, reason: &str) {
        if matches!(self.phase, Phase::Connecting) {
            self.dropped(now, reason);
        }
    }

    /// A text frame from the server.
    pub fn handle_text(&mut self, now: Duration, text: &str) {
        if !self.is_connected() {
            return;
        }
        self.last_inbound = now;
        let message = match parse_to_agent(text) {
            Ok(message) => message,
            Err((raw, error)) => return self.reject_frame(raw, error),
        };
        match message {
            ToAgent::Heartbeat {} => {
                let id = self.mint_id();
                self.control
                    .push_back(Envelope::with_id(id, FromAgent::HeartbeatAnswer {}));
            }
            ToAgent::Init {
                agent,
                inquiries,
                diagnostics,
                ..
            } => {
                self.events
                    .push_back(Event::Registered { agent, diagnostics });
                let inquiries: Vec<String> = inquiries.into_iter().map(|i| i.task).collect();
                let early = match core::mem::replace(&mut self.phase, Phase::Live) {
                    Phase::Registering { early } => early,
                    Phase::Reloading { early, .. } => early,
                    _ => vec![],
                };
                if self.outbox.needs_reload() {
                    self.phase = Phase::Reloading { early, inquiries };
                    self.events.push_back(Event::ReloadJournal);
                } else {
                    self.go_live(early, inquiries);
                }
            }
            ToAgent::ProtocolError { error } => self.end(SessionError::Refused(error)),
            ToAgent::Kick { reason } => self.end(SessionError::Kicked(reason)),
            ToAgent::Bounce { duration } => {
                tracing::info!("server bounced the agent, reconnecting");
                self.attempt = 0;
                let delay = duration.map(Duration::from_secs).unwrap_or(BOUNCE_DELAY);
                self.disconnect(now + delay);
            }
            other => match &mut self.phase {
                Phase::Registering { early } | Phase::Reloading { early, .. } => early.push(other),
                _ => self.dispatch(other),
            },
        }
    }

    /// The server closed the connection.
    pub fn handle_close(&mut self, now: Duration, code: u16, reason: &str) {
        if !self.is_connected() {
            return;
        }
        if code == CLOSE_BEFORE_REGISTRATION {
            tracing::info!("the server wants a fresh registration, reconnecting");
            self.attempt = 0;
            return self.disconnect(now + BOUNCE_DELAY);
        }
        if matches!(code, CLOSE_BLOCKED | CLOSE_BUSY | CLOSE_KICKED) {
            return self.end(SessionError::Closed {
                code,
                reason: reason.to_owned(),
            });
        }
        self.dropped(now, &format!("closed with code {code} {reason}"));
    }

    /// The connection broke (read or write error, the stream ended).
    pub fn handle_disconnect(&mut self, now: Duration, reason: &str) {
        if self.is_connected() {
            self.dropped(now, reason);
        }
    }

    /// Time passed: reconnect when the backoff is over, drop a silent connection.
    pub fn handle_timeout(&mut self, now: Duration) {
        match self.phase {
            Phase::Waiting { at } if now >= at => {
                self.phase = Phase::Connecting;
                self.attempt_started = now;
                self.events.push_back(Event::Connect);
            }
            _ if self.is_connected() && now >= self.last_inbound + self.config.idle_timeout => {
                self.dropped(now, "no frame from the server within the idle timeout");
            }
            _ => {}
        }
    }

    /// The entries [`Event::ReloadJournal`] asked for, or `None` if they could not be read.
    pub fn reloaded(&mut self, entries: Option<Vec<JournalEntry>>) {
        if let Some(entries) = entries {
            self.outbox.reload(entries);
        }
        if let Phase::Reloading { .. } = self.phase {
            let Phase::Reloading { early, inquiries } =
                core::mem::replace(&mut self.phase, Phase::Live)
            else {
                unreachable!()
            };
            self.go_live(early, inquiries);
        }
    }

    /// The app started (or failed to): held assignments go out, or the session ends.
    pub fn activated(&mut self, result: Result<(), String>) {
        if self.is_ended() {
            return;
        }
        match result {
            Ok(()) => {
                if let Activation::Running(held) =
                    core::mem::replace(&mut self.activation, Activation::Done)
                {
                    for message in held {
                        self.events.push_back(Event::Message(message));
                    }
                }
            }
            Err(error) => {
                self.activation = Activation::Done;
                self.end(SessionError::Activation(error));
            }
        }
    }

    /// Queue an unnumbered message (requests, probes) for the current connection.
    pub fn send(&mut self, message: FromAgent) {
        let id = self.mint_id();
        self.outbox.push(Envelope::with_id(id, message));
    }

    /// Queue a message the journal recorded as `entry`; kept until acknowledged.
    pub fn send_entry(&mut self, message: FromAgent, entry: &JournalEntry) {
        self.outbox.push_entry(message, entry);
    }

    // --------------------------------------------------------------- outputs

    /// The next frame to send on the open connection.
    pub fn poll_transmit(&mut self) -> Option<Envelope> {
        self.control.pop_front().or_else(|| self.outbox.poll())
    }

    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// When to call [`Session::handle_timeout`] next.
    pub fn poll_timeout(&self) -> Option<Duration> {
        match self.phase {
            Phase::Waiting { at } => Some(at),
            _ if self.is_connected() => Some(self.last_inbound + self.config.idle_timeout),
            _ => None,
        }
    }

    // -------------------------------------------------------------- internal

    fn is_connected(&self) -> bool {
        matches!(
            self.phase,
            Phase::Registering { .. } | Phase::Reloading { .. } | Phase::Live
        )
    }

    /// After `INIT` (and the backlog): attach, re-send, answer inquiries,
    /// activate once, then what arrived early.
    fn go_live(&mut self, early: Vec<ToAgent>, inquiries: Vec<String>) {
        self.phase = Phase::Live;
        self.outbox.attach();
        self.outbox.resend_unacked();
        if !inquiries.is_empty() {
            self.events.push_back(Event::Inquiries(inquiries));
        }
        if matches!(self.activation, Activation::NotStarted) {
            self.activation = Activation::Running(vec![]);
            self.events.push_back(Event::Activate);
        }
        for message in early {
            self.dispatch(message);
        }
    }

    fn dispatch(&mut self, message: ToAgent) {
        match message {
            message if is_for_tasks(&message) => match &mut self.activation {
                Activation::Done => self.events.push_back(Event::Message(message)),
                Activation::Running(held) => held.push(message),
                // Cannot happen (task messages come after INIT), but never lose one.
                Activation::NotStarted => self.activation = Activation::Running(vec![message]),
            },
            ToAgent::JournalAck {
                journal_session,
                pos,
            } => {
                self.outbox.journal_ack(&journal_session, pos);
                self.events.push_back(Event::Acked {
                    session: journal_session,
                    pos,
                });
            }
            // Frames are retired by JOURNAL_ACK; the server stops sending
            // EVENT_ACK once it gets positions.
            ToAgent::EventAck { .. } => {}
            ToAgent::Unknown => tracing::debug!("ignoring an unsupported message"),
            other => self.events.push_back(Event::Message(other)),
        }
    }

    /// A frame that did not parse. A broken `ASSIGN` is answered so the
    /// caller is not left waiting; anything else is logged.
    fn reject_frame(&mut self, raw: Option<Value>, error: serde_json::Error) {
        let task = raw
            .as_ref()
            .filter(|v| v.get("type").and_then(Value::as_str) == Some("ASSIGN"))
            .and_then(|v| v.get("task"))
            .and_then(Value::as_str);
        match task {
            Some(task) => self.events.push_back(Event::MalformedAssign {
                task: task.to_owned(),
                error: format!("malformed ASSIGN: {error}"),
            }),
            None => tracing::warn!("could not parse a server frame: {}", error),
        }
    }

    /// Leave the connection and wait until `at`.
    fn disconnect(&mut self, at: Duration) {
        self.outbox.detach();
        self.control.clear();
        self.events.push_back(Event::Close);
        self.phase = Phase::Waiting { at };
    }

    /// The connection (or the attempt) failed: back off, or give up.
    fn dropped(&mut self, now: Duration, reason: &str) {
        tracing::warn!("agent connection lost: {}", reason);
        let policy = &self.config.policy;
        if now.saturating_sub(self.attempt_started) >= policy.reset_after {
            self.attempt = 0;
        }
        self.attempt += 1;
        if self.attempt > policy.max_retries {
            return self.end(SessionError::Exhausted(self.attempt - 1));
        }
        let sample = self.rng.jitter();
        let delay = self.config.policy.delay_for(self.attempt, sample);
        tracing::info!(
            "reconnecting in {:?} (attempt {}/{})",
            delay,
            self.attempt,
            self.config.policy.max_retries
        );
        self.disconnect(now + delay);
    }

    fn end(&mut self, error: SessionError) {
        if self.is_ended() {
            return;
        }
        self.outbox.detach();
        self.control.clear();
        self.events.push_back(Event::Close);
        self.events.push_back(Event::Ended(error));
        self.phase = Phase::Ended;
    }
}

/// Messages the app's tasks act on: held until activation, in order.
fn is_for_tasks(message: &ToAgent) -> bool {
    matches!(
        message,
        ToAgent::Assign(_)
            | ToAgent::Cancel { .. }
            | ToAgent::Interrupt { .. }
            | ToAgent::Pause { .. }
            | ToAgent::Resume { .. }
            | ToAgent::Decide { .. }
            | ToAgent::BlokRefused { .. }
    )
}

/// An envelope as a text frame.
pub fn to_text(envelope: &Envelope) -> String {
    serde_json::to_string(envelope).unwrap_or_default()
}
