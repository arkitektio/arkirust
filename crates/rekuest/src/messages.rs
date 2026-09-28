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
        /// The server persists journal positions and answers with `JOURNAL_ACK`.
        #[serde(default)]
        journal: bool,
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
        #[serde(default)]
        error: Option<String>,
    },
    Unshelved {
        #[serde(default)]
        error: Option<String>,
    },
    /// Everything of `journal_session` up to `pos` is persisted.
    JournalAck {
        journal_session: String,
        pos: u64,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FromAgent {
    Register {
        token: String,
        force: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(flatten)]
        declaration: AgentDeclaration,
    },
    HeartbeatAnswer {},
    Started {
        task: String,
    },
    Log {
        task: String,
        message: String,
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
    Paused {
        task: String,
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
    Unlock {
        key: String,
    },
    /// The agent holds a value in memory under `resource_id`, an id it minted
    /// itself. The value is referenced by that id right away; the server only
    /// records the drawer.
    Shelve {
        #[serde(rename = "ref")]
        reference: String,
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
        #[serde(rename = "ref")]
        reference: String,
        drawer: String,
    },
    /// The assignment a task started from, as the journal records it (no token).
    /// Only sent to a server that keeps a journal.
    Assign(Box<Assign>),
    /// The task read the clock (a durable-action effect).
    Now {
        task: String,
        /// `{task}:{step}`, filled in by the journal.
        #[serde(default)]
        effect_id: String,
        value: f64,
    },
    /// The task drew random bytes (hex).
    Random {
        task: String,
        #[serde(default)]
        effect_id: String,
        value: String,
    },
    /// The task slept until a deadline (epoch seconds).
    Sleep {
        task: String,
        #[serde(default)]
        effect_id: String,
        until: f64,
    },
}

impl FromAgent {
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
    /// Task events carry a `seq`; registration, heartbeats, state, lock,
    /// shelve and journal-only messages do not.
    pub fn is_event(&self) -> bool {
        !matches!(
            self,
            FromAgent::Register { .. }
                | FromAgent::HeartbeatAnswer {}
                | FromAgent::StatePatch { .. }
                | FromAgent::StateSnapshot { .. }
                | FromAgent::SessionInit { .. }
                | FromAgent::Lock { .. }
                | FromAgent::Unlock { .. }
                | FromAgent::Shelve { .. }
                | FromAgent::Unshelve { .. }
        ) && !self.is_journal_only()
    }

    /// Kinds only a journal-capable server understands, and legacy
    /// subscribers never see: the assignment echo and the effects.
    pub fn is_journal_only(&self) -> bool {
        matches!(
            self,
            FromAgent::Assign(_)
                | FromAgent::Now { .. }
                | FromAgent::Random { .. }
                | FromAgent::Sleep { .. }
        )
    }

    /// Effects: their `effect_id` is `{task}:{step}`.
    pub fn effect_id_mut(&mut self) -> Option<&mut String> {
        match self {
            FromAgent::Now { effect_id, .. }
            | FromAgent::Random { effect_id, .. }
            | FromAgent::Sleep { effect_id, .. } => Some(effect_id),
            _ => None,
        }
    }

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
            | FromAgent::Paused { task }
            | FromAgent::Resumed { task }
            | FromAgent::Now { task, .. }
            | FromAgent::Random { task, .. }
            | FromAgent::Sleep { task, .. } => Some(task),
            FromAgent::Assign(assign) => Some(assign.task.as_str()),
            FromAgent::Shelve { task, .. } => task.as_deref(),
            _ => None,
        }
    }
}

/// A [`FromAgent`] message with its `id`, (for events) `seq`, and (once
/// journaled) its position. Journal fields are named so they cannot clash
/// with message fields (`STATE_PATCH` has its own `session_id` and `ts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// Position in the agent's journal; see [`crate::journal`].
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
    pub message: FromAgent,
}

impl Envelope {
    pub fn new(message: FromAgent) -> Self {
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

    /// The envelope of a journaled message: its id and position come from the entry.
    pub fn journaled(message: FromAgent, entry: &crate::journal::JournalEntry) -> Self {
        Self {
            id: entry.message_id.clone(),
            seq: None,
            pos: Some(entry.pos),
            journal_session: Some(entry.session_id.clone()),
            agent_ts: Some(entry.event_time as f64 / 1000.0),
            task_step: entry.step,
            message,
        }
    }
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
        let mut env = Envelope::new(FromAgent::Yield {
            task: "t".into(),
            returns: json!({"return0": 1}).as_object().unwrap().clone(),
        });
        env.seq = Some(4);
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["type"], "YIELD");
        assert_eq!(v["seq"], 4);
        assert_eq!(v["returns"]["return0"], 1);
        assert!(v["id"].as_str().unwrap().len() > 10);

        let v = serde_json::to_value(Envelope::new(FromAgent::HeartbeatAnswer {})).unwrap();
        assert_eq!(v["type"], "HEARTBEAT_ANSWER");
        assert!(v.get("seq").is_none());

        let v = serde_json::to_value(Envelope::new(FromAgent::Failed {
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
