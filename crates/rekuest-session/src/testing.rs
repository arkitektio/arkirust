//! For drivers' tests: a journal stand-in, a minimal app and a scripted server.
//!
//! Every driver (`rekuest::Agent`, `checks/wasm`, `checks/embassy`) runs the
//! same [`FakeServer::scripted`] scenario against its own I/O: register,
//! run one assignment, lose the connection before acknowledging, reconnect,
//! get the reports again in order, acknowledge. The server and the app are
//! pure, so the driver under test is the only thing that differs.

use alloc::collections::{BTreeMap, VecDeque};

use serde_json::{json, Value};

use crate::entry::JournalEntry;
use crate::prelude::*;
use crate::protocol::messages::{FromAgent, ToAgent};
use crate::session::{Event, Session};

/// Numbers messages as the journal does (pos, task step), without recording them.
pub struct Numberer {
    session: String,
    pos: u64,
    steps: BTreeMap<String, u64>,
}

impl Numberer {
    pub fn new(session: &str) -> Self {
        Self {
            session: session.into(),
            pos: 0,
            steps: BTreeMap::new(),
        }
    }

    pub fn entry(&mut self, message: &FromAgent) -> JournalEntry {
        self.pos += 1;
        let task_id = message.task().map(str::to_owned);
        let step = task_id.as_ref().map(|task| {
            let step = self.steps.entry(task.clone()).or_default();
            *step += 1;
            *step
        });
        let payload = serde_json::to_value(message).unwrap_or(Value::Null);
        JournalEntry {
            session_id: self.session.clone(),
            pos: self.pos,
            global_rev: 0,
            timepoint: String::new(),
            event_time: 0,
            kind: payload
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            task_id,
            step,
            action_key: None,
            subject: None,
            message_id: format!("{}-{}", self.session, self.pos),
            payload,
        }
    }
}

/// A minimal app: activates at once and answers every assignment with
/// `YIELD {"echo": args}` and `COMPLETED`, numbered.
pub struct Echo {
    journal: Numberer,
}

impl Echo {
    pub fn new(session: &str) -> Self {
        Self {
            journal: Numberer::new(session),
        }
    }

    /// Act on `event`; the I/O events (`Connect`, `Close`, `Ended`, …) are the driver's.
    pub fn handle(&mut self, session: &mut Session, event: &Event) {
        match event {
            Event::Activate => session.activated(Ok(())),
            Event::Message(ToAgent::Assign(assign)) => {
                let mut returns = serde_json::Map::new();
                returns.insert("echo".into(), Value::Object(assign.args.clone()));
                for message in [
                    FromAgent::Yield {
                        task: assign.task.clone(),
                        returns,
                    },
                    FromAgent::Completed {
                        task: assign.task.clone(),
                    },
                ] {
                    let entry = self.journal.entry(&message);
                    session.send_entry(message, &entry);
                }
            }
            _ => {}
        }
    }
}

/// What the server does next.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerOutput {
    /// A text frame to the agent.
    Frame(String),
    /// Drop the connection (no close frame).
    Drop,
}

/// A scripted `/agi` server. Feed it every frame the agent sends
/// ([`FakeServer::receive`]) and tell it about new connections
/// ([`FakeServer::connected`]); take what it does from [`FakeServer::poll`].
pub struct FakeServer {
    connections: usize,
    /// Every frame received, per connection (1, 2, …).
    pub received: Vec<(usize, Value)>,
    out: VecDeque<ServerOutput>,
    stage: Stage,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Stage {
    /// Waiting for REGISTER, then assign `t1`.
    First,
    /// `t1` assigned; drop once its COMPLETED arrives, unacknowledged.
    Assigned,
    /// Waiting for REGISTER again, then the resent reports.
    Second,
    Done,
}

impl Default for FakeServer {
    fn default() -> Self {
        Self::scripted()
    }
}

impl FakeServer {
    /// The shared scenario, see the module docs.
    pub fn scripted() -> Self {
        Self {
            connections: 0,
            received: vec![],
            out: VecDeque::new(),
            stage: Stage::First,
        }
    }

    /// The agent opened connection number `n` (1, 2, …).
    pub fn connected(&mut self) {
        self.connections += 1;
    }

    pub fn receive(&mut self, text: &str) {
        let frame: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let kind = frame["type"].as_str().unwrap_or_default().to_owned();
        self.received.push((self.connections, frame.clone()));
        match (self.stage, kind.as_str()) {
            (Stage::First, "REGISTER") => {
                self.init();
                self.send(json!({"type": "HEARTBEAT", "id": "hb"}));
                self.send(json!({
                    "type": "ASSIGN", "id": "a1", "interface": "echo", "task": "t1",
                    "args": {"x": 1}, "user": "u", "org": "o", "action": "a",
                    "implementation": "i", "token": "tok",
                }));
                self.stage = Stage::Assigned;
            }
            (Stage::Assigned, "COMPLETED") => {
                self.out.push_back(ServerOutput::Drop);
                self.stage = Stage::Second;
            }
            (Stage::Second, "REGISTER") => self.init(),
            (Stage::Second, "COMPLETED") => {
                let pos = frame["pos"].as_u64().unwrap_or_default();
                let session = frame["journal_session"].as_str().unwrap_or_default();
                self.send(json!({"type": "JOURNAL_ACK", "id": "ack", "journal_session": session, "pos": pos}));
                self.stage = Stage::Done;
            }
            _ => {}
        }
    }

    pub fn poll(&mut self) -> Option<ServerOutput> {
        self.out.pop_front()
    }

    /// The scenario ran to its end.
    pub fn done(&self) -> bool {
        self.stage == Stage::Done
    }

    /// Panics unless the agent behaved: registered twice with the same
    /// session, answered the heartbeat, and sent `t1`'s reports once per
    /// connection, in journal order, with `seq` restarting.
    pub fn check(&self) {
        assert!(self.done(), "the scenario did not finish: {:?}", self.stage);
        let of = |connection: usize, kind: &str| -> Vec<&Value> {
            self.received
                .iter()
                .filter(|(c, f)| *c == connection && f["type"] == kind)
                .map(|(_, f)| f)
                .collect()
        };
        let registers = [of(1, "REGISTER"), of(2, "REGISTER")];
        assert_eq!(registers[0].len(), 1);
        assert_eq!(registers[1].len(), 1);
        assert_eq!(
            registers[0][0]["session_id"], registers[1][0]["session_id"],
            "one session across reconnects"
        );
        assert_eq!(of(1, "HEARTBEAT_ANSWER").len(), 1);
        for connection in [1, 2] {
            let reports: Vec<(Value, Value, Value)> = self
                .received
                .iter()
                .filter(|(c, f)| *c == connection && f["task"] == "t1")
                .map(|(_, f)| (f["type"].clone(), f["pos"].clone(), f["seq"].clone()))
                .collect();
            assert_eq!(
                reports,
                vec![
                    (json!("YIELD"), json!(1), json!(1)),
                    (json!("COMPLETED"), json!(2), json!(2)),
                ],
                "connection {connection}"
            );
        }
        assert_eq!(of(1, "YIELD")[0]["returns"], json!({"echo": {"x": 1}}));
    }

    fn init(&mut self) {
        self.send(json!({"type": "INIT", "id": "init", "agent": "agent-1", "inquiries": [], "diagnostics": []}));
    }

    fn send(&mut self, frame: Value) {
        self.out.push_back(ServerOutput::Frame(frame.to_string()));
    }
}
