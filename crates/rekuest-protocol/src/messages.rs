//! The agent websocket protocol (`/agi`).
//!
//! Every frame is a JSON object with a `type` discriminator and an `id`.
//! Agent events that take part in the ack stream also carry a per-connection
//! monotonic `seq`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::definition::AgentDeclaration;

/// A task the server asks about after (re)connecting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Inquiry {
    pub task: String,
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
    pub user: String,
    pub org: String,
    pub action: String,
    pub implementation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Set when the server sends a workflow again after its agent died: what the
    /// previous run recorded, for the resumed run to replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<Journal>,
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
/// happened to a task it assigned. `event` is the event's id, `seq` its order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionEvent {
    pub task: String,
    pub event: String,
    pub seq: u64,
}

/// Server → agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ToAgent {
    Init {
        agent: String,
        #[serde(default)]
        inquiries: Vec<Inquiry>,
        #[serde(default)]
        hash: Option<String>,
        #[serde(default)]
        diagnostics: Vec<Diagnostic>,
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
    Heartbeat {},
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
    EventAck {
        #[serde(default)]
        event: Option<String>,
        #[serde(default)]
        task: Option<String>,
        #[serde(default)]
        seq: Option<u64>,
    },
    /// Drop these shelved values (by the id the agent minted).
    Collect {
        #[serde(default)]
        drawers: Vec<String>,
    },
    /// The server's answer to a `SHELVE`; the agent minted the id already.
    Shelved {
        #[serde(rename = "ref")]
        reference: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drawer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Unshelved {
        #[serde(rename = "ref")]
        reference: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Everything of `journal_session` up to `pos` is persisted.
    JournalAck {
        journal_session: String,
        pos: u64,
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
    ResumingEvent(ExecutionEvent),
    ResumedEvent(ExecutionEvent),
    FailedEvent {
        #[serde(flatten)]
        event: ExecutionEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LogLevel {
    Debug,
    #[default]
    Info,
    Warn,
    Error,
    Critical,
}

/// Agent → server.
///
/// `D` is what a `REGISTER` declares. An agent sends its own [`AgentDeclaration`]; a
/// server parses the full declaration its registration validates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FromAgent<D = AgentDeclaration> {
    Register {
        token: String,
        #[serde(default)]
        force: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(flatten)]
        declaration: D,
    },
    HeartbeatAnswer {},
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
    /// One RFC 6902 operation on a state. `old_value` is always null, as in Python.
    StatePatch {
        session_id: String,
        global_rev: u64,
        state_name: String,
        ts: f64,
        op: String,
        path: String,
        value: Value,
        old_value: Value,
        task_id: Option<String>,
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
        #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
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
        #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
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
    /// Ask the server to assign a child task (dependent work). Not numbered
    /// (no `pos`): the server answers it. `parent_step` is the parent's step for
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
    /// A hold a person resumed.
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

    /// Events that carry a `seq`.
    /// Task events carry a `seq`; registration, heartbeats, requests, state,
    /// lock and shelve messages do not.
    pub fn is_event(&self) -> bool {
        !matches!(
            self,
            FromAgent::Register { .. }
                | FromAgent::HeartbeatAnswer {}
                | FromAgent::AssignRequest { .. }
                | FromAgent::ProbeRequest { .. }
                | FromAgent::StateRevisionRequest { .. }
                | FromAgent::CancelRequest { .. }
                | FromAgent::InterruptRequest { .. }
                | FromAgent::PauseRequest { .. }
                | FromAgent::ResumeRequest { .. }
                | FromAgent::StatePatch { .. }
                | FromAgent::StateSnapshot { .. }
                | FromAgent::SessionInit { .. }
                | FromAgent::Lock { .. }
                | FromAgent::Unlock { .. }
                | FromAgent::Shelve { .. }
                | FromAgent::Unshelve { .. }
        )
    }

    /// Frames that never get a journal position: they have their own reply.
    pub fn is_unnumbered(&self) -> bool {
        matches!(
            self,
            FromAgent::Register { .. }
                | FromAgent::HeartbeatAnswer {}
                | FromAgent::AssignRequest { .. }
                | FromAgent::ProbeRequest { .. }
                | FromAgent::StateRevisionRequest { .. }
                | FromAgent::CancelRequest { .. }
                | FromAgent::InterruptRequest { .. }
                | FromAgent::PauseRequest { .. }
                | FromAgent::ResumeRequest { .. }
        )
    }

    /// The task a frame belongs to: for `STATE_PATCH` the changing task, for
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
            | FromAgent::Resumed { task }
            | FromAgent::Effect { task, .. }
            | FromAgent::Lock { task, .. } => Some(task),
            FromAgent::Unlock { task, .. } => task.as_deref(),
            FromAgent::StatePatch { task_id, .. } => task_id.as_deref(),
            FromAgent::Shelve { task, .. } => task.as_deref(),
            _ => None,
        }
    }

    /// A frame of a probe task (see [`is_probe_task`]).
    pub fn is_probe(&self) -> bool {
        self.task().is_some_and(is_probe_task)
    }
}

/// A [`FromAgent`] message with its `id`, (for events) `seq`, and (once
/// journaled) its position. Journal fields are named so they cannot clash
/// with message fields (`STATE_PATCH` has its own `session_id` and `ts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope<D = AgentDeclaration> {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
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
    pub fn new(message: FromAgent<D>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            seq: None,
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
    #[serde(flatten)]
    pub message: ToAgent,
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
    use serde_json::json;

    #[test]
    fn parses_server_frames() {
        let init = parse_to_agent(
            r#"{"type":"INIT","id":"1","agent":"a","inquiries":[{"task":"t"}],"diagnostics":[]}"#,
        )
        .unwrap();
        assert!(matches!(init, ToAgent::Init { ref inquiries, .. } if inquiries[0].task == "t"));

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

        assert!(matches!(
            parse_to_agent(r#"{"type":"HEARTBEAT","id":"3"}"#).unwrap(),
            ToAgent::Heartbeat {}
        ));
        assert!(matches!(
            parse_to_agent(r#"{"type":"CANCEL","id":"4","task":"t1"}"#).unwrap(),
            ToAgent::Cancel { .. }
        ));
        assert!(matches!(
            parse_to_agent(r#"{"type":"EVENT_ACK","id":"5","event":"e","seq":3}"#).unwrap(),
            ToAgent::EventAck { seq: Some(3), .. }
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
        env.seq = Some(4);
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["type"], "YIELD");
        assert_eq!(v["seq"], 4);
        assert_eq!(v["returns"]["return0"], 1);
        assert!(v["id"].as_str().unwrap().len() > 10);

        let v = serde_json::to_value(Envelope::<AgentDeclaration>::new(
            FromAgent::HeartbeatAnswer {},
        ))
        .unwrap();
        assert_eq!(v["type"], "HEARTBEAT_ANSWER");
        assert!(v.get("seq").is_none());

        let v = serde_json::to_value(Envelope::<AgentDeclaration>::new(FromAgent::Failed {
            task: "t".into(),
            error: "e".into(),
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
            declaration: decl,
        }))
        .unwrap();
        assert_eq!(v["type"], "REGISTER");
        assert_eq!(v["name"], "app:1");
        assert_eq!(v["implementations"], json!([]));
        assert!(v["hash"].is_string());
    }
}
