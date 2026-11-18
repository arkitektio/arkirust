//! The agent websocket protocol (`/agi`).
//!
//! Every frame is a JSON object with a `type` discriminator and an `id`.
//!
//! A frame that must not be lost carries `pos`: its position in its sender's stream. The
//! agent's stream is per journal session and acknowledged with `JOURNAL_ACK`; the server's
//! (its commands) is per agent and acknowledged with `COMMAND_ACK`. Each side keeps what it
//! sent until the other acknowledged it, and sends it again after a reconnect.

use crate::prelude::*;
use alloc::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use arkitekt_spec::actions::AgentDeclaration;
pub use arkitekt_spec::actions::LogLevel;

/// What an agent says about a task it is asked about (`INQUIRE`), from memory and at once:
/// asking never touches the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskStatus {
    /// The agent has no such task. From the agent the task was given to, that is final: the
    /// server ends the task as lost.
    Unknown,
    /// Taken, not started yet (behind a lock, or while the app starts).
    Waiting,
    /// Started, and at work.
    Running,
    /// Stopped at a pause point, until it is resumed.
    Paused,
    /// Waiting for a person's decision (a hold it asked for).
    Held,
    /// The agent reported its end.
    Ended,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    #[serde(default)]
    pub level: Option<String>,
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub path: Option<String>,
}

/// What a task says about a hold or a child it asks to run, for the person who decides.
/// It is the agent's own words: whoever shows it shows what the server knows beside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssignPrompt {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A blok a task shows a person: with its own hold (`HELD`), to ask for values typed by its
/// `returns`, or on its own (`INFORM`), to show something while it carries on.
///
/// Exactly one of `key` (a blok the agent's app declares), `id` (any blok of the organization)
/// or `source` (v2 BSX, which the server parses into a blok the task owns) names it. The server
/// makes it for the task alone: it is never listed and goes when the task goes. A blok it
/// cannot make (unparsable BSX, a component the catalog lacks, args or bindings that do not
/// fit) is answered with `BLOK_REFUSED` and nothing of the frame is kept.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskBlok {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// A title for whoever sees it; the blok's own name when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What it is opened on, read by its tree under `args.<key>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Map<String, Value>>,
    /// What an answer holds (return ports, as a definition declares them). A `source` blok
    /// declares them here; a named one has its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<Vec<Value>>,
    /// A `source` blok's agent dependencies (as a declared blok's). It always has `self`,
    /// the agent that sent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<Value>>,
    /// Dependency key to agent id. A dependency left out that the sending agent satisfies is
    /// bound to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_mappings: Option<Map<String, Value>>,
    /// The blok language version a `source` blok is written in; 2, the only one, when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vocab: Option<u32>,
    /// The catalogs a `source` blok needs, as `name@version` (`ui@1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalogs: Option<Vec<String>>,
    /// A `source` blok's locals, each a port (`{key, kind?, default?, ...}`; a missing kind is
    /// inferred server-side, ANY only when nothing tells) plus `persist`: form state that feeds a
    /// call, written only through `bind`, read bare.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locals: Option<Vec<Value>>,
    /// What a preview renders a `source` blok with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub demo: Option<BlokDemo>,
    /// A `source` blok's declared reads (`{key, service, document, variables?, poll?, more?}`),
    /// read as `queries.<key>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queries: Option<Vec<Value>>,
    /// A `source` blok's declared streams (`{key, service, document, variables?, keep?,
    /// history?}`), read as `subscriptions.<key>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscriptions: Option<Vec<Value>>,
    /// The module (an app's own unversioned extras) a `source` blok renders against besides its
    /// catalogs; the server's default when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

/// What a preview renders a blok with, mirroring the roots it reads. Never what it shows live.
/// Every field is the author's JSON.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BlokDemo {
    /// Per dependency key (`self` included), its states and tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deps: Option<Value>,
    /// What it is opened on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    /// The task record it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Value>,
    /// Per declared query, the value it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queries: Option<Value>,
    /// Per declared subscription, its value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscriptions: Option<Value>,
}

/// A condition on the running agent's state that must hold for a task to run.
///
/// The agent tests it after taking the action's locks and the locks the named state
/// requires, before anything runs; a task whose state does not match ends `FAILED` with
/// [`STATE_PRECONDITION`] as its code. `path` is an RFC 6901 pointer into the state's
/// value (`/stage/busy`); a path the state does not hold fails. `operator` is a descriptor
/// operator by name (`EQUALS`, `NOT_EQUALS`, `GTE`, `LTE`, `IN`, `NOT_IN`, `CONTAINS`,
/// `EXISTS`, `MATCHES`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateRequirement {
    pub state: String,
    pub path: String,
    pub operator: String,
    #[serde(default)]
    pub value: Value,
}

/// The `FAILED` code of a task refused because its agent's state did not match its
/// [`StateRequirement`]s. Nothing of the action ran.
pub const STATE_PRECONDITION: &str = "STATE_PRECONDITION";

/// What an agent that tests [`StateRequirement`]s says on `REGISTER` (in `features`). A
/// server must not send a task with requirements to an agent that does not say it: the
/// agent would drop them and run the task unchecked.
pub const FEATURE_STATE_REQUIRES: &str = "state_requires";

/// What an agent speaking this protocol says on `REGISTER` (in `features`): positions in
/// both directions, acknowledged commands, refused gaps. A server refuses an agent that
/// does not say it.
pub const FEATURE_PROTOCOL_2: &str = "protocol_2";

/// A request to run an action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assign {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub interface: String,
    pub task: String,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub step: Option<bool>,
    #[serde(default)]
    pub probe: bool,
    #[serde(default)]
    pub capture: Option<bool>,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub args: Map<String, Value>,
    #[serde(default)]
    pub message: Option<String>,
    /// Who caused this hop: the human for a top-level task, the parent's agent user for a child.
    pub user: String,
    /// The human at the root of the task tree, who assigned it (the provenance `rcb`);
    /// None from a server that does not send it, where `user` is the best there is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigner: Option<String>,
    /// The organization the task runs in, `"<iss>#<org>"`: unique across issuers.
    pub org: String,
    pub action: String,
    pub implementation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Set when the server sends a workflow again after its agent died: what the
    /// previous run recorded, for the resumed run to replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<Journal>,
    /// What the agent's state must hold for the task to run (see [`StateRequirement`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_requires: Vec<StateRequirement>,
}

/// One value a workflow recorded, keyed as the workflow named it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedEffect {
    pub key: String,
    pub effect: String,
    #[serde(default)]
    pub value: Value,
}

/// What a resumed workflow replays: its recorded effects, and the step to continue after.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    #[serde(default)]
    pub last_step: u64,
    #[serde(default)]
    pub effects: Vec<RecordedEffect>,
}

/// The fields every `…_EVENT` mirror carries: the server tells a caller what
/// happened to a task it assigned. `event` is the event's id: a mirror seen twice (a
/// replay overlapping what arrived live) is the same event. Their order is the order of
/// the command stream they travel in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionEvent {
    pub task: String,
    pub event: String,
}

/// Server → agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ToAgent {
    Init {
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hash: Option<String>,
        #[serde(default)]
        diagnostics: Vec<Diagnostic>,
        /// The `nonce` of the `REGISTER` this answers. An agent ignores an `INIT` that is
        /// not its current connection's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nonce: Option<String>,
        /// How far the server has projected each journal session the `REGISTER` listed.
        /// The agent drops what is at or below it and sends the rest.
        #[serde(default)]
        positions: BTreeMap<String, u64>,
    },
    Assign(Box<Assign>),
    Cancel {
        task: String,
    },
    Interrupt {
        task: String,
    },
    /// Stop the task at its next pausepoint.
    Pause {
        task: String,
    },
    /// Release a paused task; with `step`, stop again at the next pausepoint.
    Resume {
        task: String,
        #[serde(default)]
        step: bool,
    },
    /// The answer to a task's own hold (`HELD`): a person decided, or the run is in
    /// auto-mode (`auto`) and the server approved it. A denial does not end the task.
    Decide {
        task: String,
        approved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer: Option<String>,
        #[serde(default)]
        auto: bool,
        /// The answer through the hold's blok, keyed by its `returns`, checked against them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        values: Option<Map<String, Value>>,
    },
    /// The blok a `HELD` or an `INFORM` carried could not be made, and why: the frame was not
    /// kept (the task is not held, nothing is shown). Fix the blok and send it again.
    BlokRefused {
        task: String,
        reason: String,
    },
    Heartbeat {},
    /// Ask about a task this agent was given: answered at once with an `INQUIRY_ANSWER`. A
    /// question, not a command: it carries no position, is not kept, and changes nothing.
    Inquire {
        task: String,
        /// Says which question an answer belongs to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<String>,
    },
    /// The answer to an `INQUIRE_REQUEST`: what the task's agent says of it (`status`), or,
    /// where the agent was not asked, what the server knows. `asked` is false then: the agent
    /// is away, has not acknowledged the assign yet, or did not answer in time.
    InquireResponse {
        request: String,
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<TaskStatus>,
        #[serde(default)]
        asked: bool,
        /// Why the agent was not asked, or why there is no answer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<u64>,
        /// When the task last reported anything, as its agent has it (epoch seconds).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reported_at: Option<f64>,
        /// What it last said of its progress, as its agent has it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Bounce {
        #[serde(default)]
        duration: Option<u64>,
    },
    Kick {
        #[serde(default)]
        reason: Option<String>,
    },
    ProtocolError {
        error: String,
    },
    /// Drop these shelved values (by the id the agent minted).
    Collect {
        #[serde(default)]
        drawers: Vec<String>,
    },
    /// Everything of `journal_session` through this position is persisted. (`through`, not
    /// `pos`: a frame's own `pos` is where it stands in its sender's stream.)
    JournalAck {
        journal_session: String,
        through: u64,
    },
    /// A frame of `journal_session` arrived ahead of `expected` and was not taken: send
    /// again from `expected`, in order. An agent that no longer holds it says so with
    /// `JOURNAL_SKIP`.
    JournalNack {
        journal_session: String,
        expected: u64,
    },
    /// The answer to an `ASSIGN_REQUEST`: the child task, or why there is none.
    /// `created` is false when the request named a child that already exists.
    AssignResponse {
        request: String,
        reference: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
        #[serde(default)]
        created: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    ProbeResponse {
        request: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        probe: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The answer to a `STATE_REVISION_REQUEST` (a workflow's guard).
    StateRevisionResponse {
        request: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        changed: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The answer to a `CANCEL_REQUEST`, `INTERRUPT_REQUEST`, `PAUSE_REQUEST` or `RESUME_REQUEST`.
    ControlResponse {
        request: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
        accepted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    BoundEvent(ExecutionEvent),
    QueuedEvent(ExecutionEvent),
    StartedEvent(ExecutionEvent),
    ProgressEvent {
        #[serde(flatten)]
        event: ExecutionEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    DelegateEvent(ExecutionEvent),
    YieldEvent {
        #[serde(flatten)]
        event: ExecutionEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        returns: Option<Map<String, Value>>,
    },
    CompletedEvent(ExecutionEvent),
    LogEvent {
        #[serde(flatten)]
        event: ExecutionEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<LogLevel>,
    },
    CancellingEvent(ExecutionEvent),
    CancelledEvent(ExecutionEvent),
    InterruptingEvent(ExecutionEvent),
    InterruptedEvent(ExecutionEvent),
    PausingEvent(ExecutionEvent),
    PausedEvent(ExecutionEvent),
    /// The child waits for a person to approve it; nothing runs until someone does.
    HeldEvent {
        #[serde(flatten)]
        event: ExecutionEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<AssignPrompt>,
    },
    /// A person refused to let the child run: it ended without ever starting.
    DeniedEvent {
        #[serde(flatten)]
        event: ExecutionEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer: Option<String>,
    },
    ResumingEvent(ExecutionEvent),
    ResumedEvent(ExecutionEvent),
    FailedEvent {
        #[serde(flatten)]
        event: ExecutionEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// The `FAILED` frame's code, passed on to the caller.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    CriticalEvent {
        #[serde(flatten)]
        event: ExecutionEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The task's agent died while it ran; how it ended is unknown.
    LostEvent {
        #[serde(flatten)]
        event: ExecutionEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_progress: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effects: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Anything this agent does not handle (yet).
    #[serde(other)]
    Unknown,
}

impl ToAgent {
    /// Whether this is a command: numbered, kept by the server until a `COMMAND_ACK`
    /// covers it, and sent again after a reconnect. Work for the agent (`ASSIGN` and what
    /// steers it), the answers to its numbered requests, and what happened to the tasks it
    /// assigned. The rest belongs to one connection (`INIT`, `HEARTBEAT`, the acks, `KICK`,
    /// `BOUNCE`) or answers a request that is not numbered either.
    ///
    /// A probe's frames are commands by kind but travel unnumbered: whoever sends them
    /// leaves `pos` out.
    pub fn is_command(&self) -> bool {
        !matches!(
            self,
            ToAgent::Init { .. }
                | ToAgent::Heartbeat {}
                | ToAgent::Inquire { .. }
                | ToAgent::InquireResponse { .. }
                | ToAgent::Bounce { .. }
                | ToAgent::Kick { .. }
                | ToAgent::ProtocolError { .. }
                | ToAgent::JournalAck { .. }
                | ToAgent::JournalNack { .. }
                | ToAgent::ProbeResponse { .. }
                | ToAgent::StateRevisionResponse { .. }
                | ToAgent::Unknown
        )
    }
}

/// Agent → server.
///
/// `D` is what a `REGISTER` declares. An agent sends its own [`AgentDeclaration`]; a
/// server parses the full declaration its registration validates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
#[allow(clippy::large_enum_variant)] // a frame is built once and sent; boxing would change the API
pub enum FromAgent<D = AgentDeclaration> {
    Register {
        token: String,
        #[serde(default)]
        force: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        /// What this agent implements beyond the base protocol ([`FEATURE_STATE_REQUIRES`]).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        features: Vec<String>,
        /// New for every connection attempt; the `INIT` that answers it says it again.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nonce: Option<String>,
        /// The highest command position this process has handled; 0 for a fresh process.
        /// The server sends its commands from the one after.
        #[serde(default)]
        commands_seen: u64,
        /// The journal sessions this agent still holds unacknowledged frames of.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sessions: Vec<String>,
        /// Every task this process holds, in whatever state: waiting, under way, or ended
        /// with its report still to come. A server that had one of the process's tasks
        /// under way and does not find it here ends it as lost, at once. Left out by an
        /// agent that does not say: the server asks about each (`INQUIRE`) instead.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tasks: Option<Vec<String>>,
        #[serde(flatten)]
        declaration: D,
    },
    HeartbeatAnswer {},
    /// The answer to an `INQUIRE`: what this agent knows of the task, at once. `step` is the
    /// last step the task reported.
    InquiryAnswer {
        task: String,
        status: TaskStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<u64>,
        /// When the task last reported anything (epoch seconds).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reported_at: Option<f64>,
        /// What it last said of its progress.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Ask about a task this agent assigned: is its agent still on it? Answered with an
    /// `INQUIRE_RESPONSE`. A read: not numbered, not kept.
    InquireRequest {
        task: String,
    },
    /// Every command through this position is handled (cumulative). The server forgets them.
    CommandAck {
        through: u64,
    },
    /// The frames `first..=last` of the journal session `session` are gone (the agent no
    /// longer holds them): the answer to a `JOURNAL_NACK` it cannot satisfy. The server
    /// records the loss and goes on after `last`. The only way a position is ever passed
    /// over. (`session`, not `journal_session`: that is the frame's own.)
    JournalSkip {
        session: String,
        first: u64,
        last: u64,
    },
    Started {
        task: String,
    },
    Log {
        task: String,
        message: String,
        #[serde(default)]
        level: LogLevel,
    },
    Progress {
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Yield {
        task: String,
        returns: Map<String, Value>,
    },
    Completed {
        task: String,
    },
    Failed {
        task: String,
        error: String,
        /// Why, when it is one the protocol names ([`STATE_PRECONDITION`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    Critical {
        task: String,
        error: String,
    },
    Cancelled {
        task: String,
    },
    Interrupted {
        task: String,
    },
    /// The task paused: asked to, or on its own (a workflow's hold), with `message`
    /// and `details` for whoever decides.
    Paused {
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
    },
    Resumed {
        task: String,
    },
    /// The task waits for a person's decision (a workflow's `hold`); the server
    /// answers with `DECIDE`. Unlike `PAUSED`, the answer comes back to the task.
    Held {
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<AssignPrompt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        /// Ask through a blok: the person answers in it, and `DECIDE` carries its `values`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blok: Option<TaskBlok>,
    },
    /// Show a person a blok while the task carries on (a chart of its progress, a result).
    Inform {
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        blok: TaskBlok,
    },
    /// One update of a state: what takes it from one revision to the next.
    ///
    /// An update is the unit a state changes by. However many operations describe it, they
    /// are applied in order and seen together or not at all: nobody who follows the state is
    /// shown it between two of them. `global_rev` is the agent's state revision once the
    /// update is applied. The session it belongs to and when it was made are the frame's own
    /// (`journal_session`, `agent_ts`).
    StateUpdate {
        state: String,
        global_rev: u64,
        ops: Vec<StateOp>,
        /// The task that made the update, if one did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
        /// Where in a task's or the agent's life the update was made, when not in its
        /// ordinary course: `cleanup` (a task being stopped, on its way out) or `shutdown`
        /// (the agent's shutdown hooks). Said by the agent's runtime, never by the app.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lifecycle: Option<String>,
        /// What the app says the update was for: its own names.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        qualifiers: Option<Vec<String>>,
    },
    /// One RFC 6902 operation on a state, as agents said it before [`FromAgent::StateUpdate`]:
    /// an update of one operation.
    StatePatch {
        session_id: String,
        global_rev: u64,
        state_name: String,
        ts: f64,
        op: String,
        path: String,
        value: Value,
        task_id: Option<String>,
        /// Where in a task's or the agent's life the change was made, when not in its
        /// ordinary course: `cleanup` (a task being stopped, on its way out) or `shutdown`
        /// (the agent's shutdown hooks). Said by the agent's runtime, never by the app.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lifecycle: Option<String>,
        /// What the app says the change was for: its own names.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        qualifiers: Option<Vec<String>>,
    },
    /// Every state at a revision.
    StateSnapshot {
        session_id: String,
        global_rev: u64,
        snapshots: Map<String, Value>,
    },
    /// The baseline of a session: every state after startup.
    SessionInit {
        session_id: String,
        states: Map<String, Value>,
    },
    Lock {
        key: String,
        task: String,
    },
    /// `task` is the task that held the lock.
    Unlock {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
    },
    /// The agent holds a value in memory under `resource_id`, an id it minted
    /// itself. The value is referenced by that id right away; the server only
    /// records the drawer.
    Shelve {
        identifier: String,
        resource_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// The task that shelved it, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
    },
    /// The agent dropped a shelved value.
    Unshelve {
        drawer: String,
    },
    /// A value the task took from outside itself (the clock, randomness, a
    /// deadline), recorded so a replay can return the same value.
    Effect {
        task: String,
        effect: EffectKind,
        value: Value,
        /// What the task calls this value; a replay matches values by key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    /// Ask the server to assign a child task (dependent work). Numbered like a report;
    /// its `ASSIGN_RESPONSE` comes back as a command. `parent_step` is the parent's step for
    /// this call; the server stores it as the child's parent step and is
    /// idempotent on (`parent`, `parent_step`). `reference` is the caller's own
    /// idempotency key; the server mints one when it is omitted.
    AssignRequest {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_step: Option<u64>,
        /// What the parent calls this child; the server is idempotent on
        /// (`parent`, `call_key`) too, checked before the step.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_key: Option<String>,
        #[serde(default)]
        args: Map<String, Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action_hash: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        implementation: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interface: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dependency: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolution: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hooks: Option<Vec<Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capture: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<bool>,
        /// Why the caller wants the child run. Unless the run is in auto-mode the
        /// server holds the child until a person approves it, and shows this.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<AssignPrompt>,
        /// What the child's agent's state must hold for it to run (see [`StateRequirement`]).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        state_requires: Vec<StateRequirement>,
        /// What to do with the child should its agent be lost while it runs: `FAIL` (it ends
        /// lost), `RESUME` (a workflow goes on from what it recorded) or `RESTART` (an action
        /// declared idempotent is run again from the start). Left out, a workflow resumes and
        /// anything else ends lost.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_lost: Option<String>,
        /// How often the child may be sent again (resumed or restarted) before it ends lost
        /// after all. Left out, the server's own limit applies.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_resumes: Option<u32>,
    },
    /// Ask the server to run an action as a probe (not a task; nothing is recorded).
    ProbeRequest {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
        #[serde(default)]
        args: Map<String, Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action_hash: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        implementation: Option<String>,
    },
    /// A workflow's guard: has `state` of `dependency` changed since `since`?
    /// Without `since`, the answer is the revision to record.
    StateRevisionRequest {
        parent: String,
        dependency: String,
        state: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<Value>,
        #[serde(default)]
        paths: Vec<String>,
    },
    /// Cancel a task this agent assigned; unconfirmed after `auto_interrupt`
    /// seconds, it is interrupted.
    CancelRequest {
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auto_interrupt: Option<f64>,
    },
    InterruptRequest {
        task: String,
    },
    PauseRequest {
        task: String,
    },
    ResumeRequest {
        task: String,
        #[serde(default)]
        step: bool,
    },
}

/// One RFC 6902 operation of a [`FromAgent::StateUpdate`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateOp {
    pub op: String,
    pub path: String,
    #[serde(default)]
    pub value: Value,
}

/// What an [`FromAgent::Effect`] recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EffectKind {
    /// The clock, in epoch seconds.
    Now,
    /// Random bytes, as hex.
    Random,
    /// A sleep's deadline, in epoch seconds.
    Sleep,
    /// A value a workflow recorded with `task.record(fn)`.
    Record,
    /// A hold's decision: whether it was approved, and the answer.
    Hold,
}

/// Tasks of a probe (`ASSIGN` with `probe: true`) have ids starting with
/// this. Their frames are never numbered, retained or journaled.
pub const PROBE_PREFIX: &str = "p-";

pub fn is_probe_task(task: &str) -> bool {
    task.starts_with(PROBE_PREFIX)
}

impl<D> FromAgent<D> {
    /// Events that end a task; kept until the server acknowledges them.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            FromAgent::Completed { .. }
                | FromAgent::Failed { .. }
                | FromAgent::Critical { .. }
                | FromAgent::Cancelled { .. }
                | FromAgent::Interrupted { .. }
        )
    }

    /// Frames that never get a journal position: the handshake, the acknowledgements, and
    /// the two requests that are answered at once and safe to lose (a probe is ephemeral, a
    /// state revision a read).
    pub fn is_unnumbered(&self) -> bool {
        matches!(
            self,
            FromAgent::Register { .. }
                | FromAgent::HeartbeatAnswer {}
                | FromAgent::InquiryAnswer { .. }
                | FromAgent::InquireRequest { .. }
                | FromAgent::CommandAck { .. }
                | FromAgent::JournalSkip { .. }
                | FromAgent::ProbeRequest { .. }
                | FromAgent::StateRevisionRequest { .. }
        )
    }

    /// The task a frame belongs to: for `STATE_UPDATE` and `STATE_PATCH` the changing task, for
    /// `LOCK`/`UNLOCK` the holder, for `SHELVE` the task that shelved.
    pub fn task(&self) -> Option<&str> {
        match self {
            FromAgent::Started { task }
            | FromAgent::Log { task, .. }
            | FromAgent::Progress { task, .. }
            | FromAgent::Yield { task, .. }
            | FromAgent::Completed { task }
            | FromAgent::Failed { task, .. }
            | FromAgent::Critical { task, .. }
            | FromAgent::Cancelled { task }
            | FromAgent::Interrupted { task }
            | FromAgent::Paused { task, .. }
            | FromAgent::Held { task, .. }
            | FromAgent::Inform { task, .. }
            | FromAgent::Resumed { task }
            | FromAgent::Effect { task, .. }
            | FromAgent::Lock { task, .. } => Some(task),
            FromAgent::Unlock { task, .. } => task.as_deref(),
            FromAgent::StatePatch { task_id, .. } => task_id.as_deref(),
            FromAgent::StateUpdate { task, .. } => task.as_deref(),
            FromAgent::Shelve { task, .. } => task.as_deref(),
            _ => None,
        }
    }

    /// A frame of a probe task (see [`is_probe_task`]).
    pub fn is_probe(&self) -> bool {
        self.task().is_some_and(is_probe_task)
    }
}

/// A [`FromAgent`] message with its `id` and (once journaled) its position. Journal fields are named so they cannot clash
/// with message fields (`STATE_PATCH` has its own `session_id` and `ts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope<D = AgentDeclaration> {
    pub id: String,
    /// Position in the agent's journal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_session: Option<String>,
    /// When the agent recorded it (seconds since the epoch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_ts: Option<f64>,
    /// The entry's step within its task (not `step`: ASSIGN has a `step` flag).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_step: Option<u64>,
    #[serde(flatten)]
    pub message: FromAgent<D>,
}

impl<D> Envelope<D> {
    /// With a fresh random id.
    #[cfg(feature = "std")]
    pub fn new(message: FromAgent<D>) -> Self {
        Self::with_id(uuid::Uuid::new_v4().to_string(), message)
    }

    /// With `id` (where there is no OS randomness, the caller mints it).
    pub fn with_id(id: String, message: FromAgent<D>) -> Self {
        Self {
            id,
            pos: None,
            journal_session: None,
            agent_ts: None,
            task_step: None,
            message,
        }
    }
}

/// A [`ToAgent`] message with its `id`, as it is on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToAgentFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// A command's position in the server's stream to this agent; none on a frame that is
    /// not a command (see [`ToAgent::is_command`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<u64>,
    #[serde(flatten)]
    pub message: ToAgent,
}

/// Parse a server frame with its `id` and (for a command) its position. Returns the raw value
/// alongside so that a frame which fails to parse can still be answered.
pub fn parse_to_agent_frame(
    text: &str,
) -> Result<ToAgentFrame, (Option<Value>, serde_json::Error)> {
    let value: Value = serde_json::from_str(text).map_err(|e| (None, e))?;
    serde_json::from_value(value.clone()).map_err(|e| (Some(value), e))
}

/// Parse a server frame. Returns the raw value alongside so that a frame
/// which fails to parse can still be answered (e.g. a malformed ASSIGN).
pub fn parse_to_agent(text: &str) -> Result<ToAgent, (Option<Value>, serde_json::Error)> {
    let value: Value = serde_json::from_str(text).map_err(|e| (None, e))?;
    serde_json::from_value(value.clone()).map_err(|e| (Some(value), e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use serde_json::json;

    #[test]
    fn parses_server_frames() {
        let init =
            parse_to_agent(r#"{"type":"INIT","id":"1","agent":"a","diagnostics":[]}"#).unwrap();
        assert!(matches!(init, ToAgent::Init { ref agent, .. } if agent == "a"));

        let assign = parse_to_agent(
            &json!({
                "type": "ASSIGN", "id": "2", "interface": "greet", "task": "t1",
                "args": {"image": {"__identifier": "@mikro/arraydataset", "object": "5"}},
                "user": "u", "org": "o", "action": "a", "implementation": "i", "token": "tok"
            })
            .to_string(),
        )
        .unwrap();
        let ToAgent::Assign(assign) = assign else {
            panic!()
        };
        assert_eq!(assign.task, "t1");
        assert_eq!(assign.args["image"]["object"], "5");
        assert_eq!(assign.assigner, None, "an older server sends no assigner");

        let delegated = parse_to_agent(
            &json!({
                "type": "ASSIGN", "interface": "greet", "task": "t2", "parent": "t1", "root": "t1",
                "user": "agent-user", "assigner": "human", "org": "lok#7", "action": "a", "implementation": "i"
            })
            .to_string(),
        )
        .unwrap();
        let ToAgent::Assign(delegated) = delegated else {
            panic!()
        };
        assert_eq!(
            (
                delegated.user.as_str(),
                delegated.assigner.as_deref(),
                delegated.org.as_str()
            ),
            ("agent-user", Some("human"), "lok#7")
        );

        assert!(matches!(
            parse_to_agent(r#"{"type":"HEARTBEAT","id":"3"}"#).unwrap(),
            ToAgent::Heartbeat {}
        ));
        assert!(matches!(
            parse_to_agent(r#"{"type":"CANCEL","id":"4","task":"t1"}"#).unwrap(),
            ToAgent::Cancel { .. }
        ));
        assert!(matches!(
            parse_to_agent(
                r#"{"type":"JOURNAL_NACK","id":"5","journal_session":"s","expected":3}"#
            )
            .unwrap(),
            ToAgent::JournalNack { expected: 3, .. }
        ));
        assert!(matches!(
            parse_to_agent(r#"{"type":"STATE_SOMETHING_NEW","id":"6"}"#).unwrap(),
            ToAgent::Unknown
        ));

        let (raw, _) = parse_to_agent(r#"{"type":"ASSIGN","id":"7","task":"t2"}"#).unwrap_err();
        assert_eq!(raw.unwrap()["task"], "t2");
    }

    #[test]
    fn serializes_agent_frames() {
        let mut env: Envelope = Envelope::new(FromAgent::Yield {
            task: "t".into(),
            returns: json!({"return0": 1}).as_object().unwrap().clone(),
        });
        env.pos = Some(4);
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["type"], "YIELD");
        assert_eq!(v["pos"], 4);
        assert_eq!(v["returns"]["return0"], 1);
        assert!(v["id"].as_str().unwrap().len() > 10);

        let v = serde_json::to_value(Envelope::<AgentDeclaration>::new(
            FromAgent::HeartbeatAnswer {},
        ))
        .unwrap();
        assert_eq!(v["type"], "HEARTBEAT_ANSWER");
        assert!(v.get("pos").is_none());

        let v = serde_json::to_value(Envelope::<AgentDeclaration>::new(FromAgent::Failed {
            task: "t".into(),
            error: "e".into(),
            code: None,
        }))
        .unwrap();
        assert_eq!(
            v,
            json!({"id": v["id"], "type": "FAILED", "task": "t", "error": "e"})
        );

        let decl = AgentDeclaration::new(Some("app:1".into()), None, vec![]);
        let v = serde_json::to_value(Envelope::new(FromAgent::Register {
            token: "tok".into(),
            force: false,
            session_id: Some("s".into()),
            features: vec![],
            nonce: None,
            commands_seen: 0,
            sessions: vec![],
            tasks: None,
            declaration: decl,
        }))
        .unwrap();
        assert_eq!(v["type"], "REGISTER");
        assert_eq!(v["name"], "app:1");
        assert_eq!(v["implementations"], json!([]));
        assert!(v["hash"].is_string());
        assert_eq!(
            v["spec_version"],
            arkitekt_spec::actions::definition::SPEC_VERSION
        );
        assert!(v.get("features").is_none());
    }

    #[test]
    fn state_requires_are_left_out_when_empty_and_kept_when_set() {
        let frame = json!({
            "type": "ASSIGN", "interface": "move", "task": "1", "user": "u", "org": "o",
            "action": "a", "implementation": "i",
        });
        let assign: ToAgent = serde_json::from_value(frame.clone()).unwrap();
        let ToAgent::Assign(assign) = assign else {
            panic!("not an assign")
        };
        assert!(assign.state_requires.is_empty());
        assert!(serde_json::to_value(&assign)
            .unwrap()
            .get("state_requires")
            .is_none());

        let mut with = frame;
        with["state_requires"] = json!([
            {"state": "stage", "path": "/busy", "operator": "EQUALS", "value": false}
        ]);
        let ToAgent::Assign(assign) = serde_json::from_value::<ToAgent>(with).unwrap() else {
            panic!("not an assign")
        };
        assert_eq!(
            assign.state_requires,
            vec![StateRequirement {
                state: "stage".into(),
                path: "/busy".into(),
                operator: "EQUALS".into(),
                value: json!(false),
            }]
        );
    }
}
