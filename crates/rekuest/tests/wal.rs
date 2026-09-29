//! The local journal against a fake server that keeps a journal: what the
//! server never acknowledged is re-sent after a restart, first and in order;
//! effects and locally minted shelve ids travel in that order too; probes
//! are never numbered.

#![cfg(feature = "wal")]

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rekuest::{action, Agent, AgentOptions, Context, Memory, MemoryStructure, Registry, Task};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

struct StaticToken;

#[async_trait::async_trait]
impl fakts::TokenLoader for StaticToken {
    async fn get_token(&self) -> fakts::Result<String> {
        Ok("token".into())
    }
    async fn refresh_token(&self, _stale: &str) -> fakts::Result<String> {
        Ok("token".into())
    }
}

#[derive(Debug)]
pub struct Frame(pub Vec<u8>);

impl MemoryStructure for Frame {
    const IDENTIFIER: &'static str = "@test/frame";
}

/// Stamp
///
/// Reads the clock, draws a random number and sleeps, as effects.
#[action]
async fn stamp(task: Task) -> i64 {
    let now = task.now();
    let bytes = task.random(4);
    task.sleep(Duration::from_millis(20)).await;
    now.timestamp() + bytes.len() as i64
}

/// Capture
///
/// Returns a frame that stays in the agent's memory.
#[action]
async fn capture() -> Memory<Frame> {
    Memory::new(Frame(vec![1, 2, 3]))
}

type Ws = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

async fn recv(ws: &mut Ws) -> Value {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("agent answered in time")
            .expect("stream open")
            .expect("frame ok");
        if let Message::Text(text) = frame {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

async fn send(ws: &mut Ws, value: Value) {
    ws.send(Message::Text(value.to_string())).await.unwrap();
}

fn assign(task: &str, interface: &str) -> Value {
    json!({
        "type": "ASSIGN", "id": format!("m-{task}"), "interface": interface, "task": task,
        "args": {}, "user": "u", "org": "o", "action": "a", "implementation": "i", "token": "tt"
    })
}

fn registry() -> Registry {
    let mut registry = Registry::new();
    registry.register(stamp).register(capture);
    registry
}

/// Start an agent on `db` against a fresh fake server; return it and the server side after INIT.
async fn connect(db: &std::path::Path) -> (Arc<Agent>, Ws) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut options = AgentOptions::new(format!("ws://{}/agi", listener.local_addr().unwrap()));
    options.name = Some("wal:1".into());
    options.journal_path = Some(db.to_owned());
    let agent = Arc::new(Agent::new(
        options,
        registry(),
        Context::default(),
        Arc::new(StaticToken),
    ));
    let runner = agent.clone();
    tokio::spawn(async move { runner.run().await });
    let (stream, _) = listener.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
    assert_eq!(recv(&mut ws).await["type"], "REGISTER");
    send(&mut ws, json!({"type": "INIT", "agent": "a1"})).await;
    (agent, ws)
}

/// Frames until `task` ends.
async fn until_end(ws: &mut Ws, task: &str) -> Vec<Value> {
    let mut frames = vec![];
    loop {
        let frame = recv(ws).await;
        let done = frame["task"] == task
            && matches!(
                frame["type"].as_str(),
                Some("COMPLETED" | "FAILED" | "CRITICAL")
            );
        frames.push(frame);
        if done {
            return frames;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unacknowledged_entries_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("agent.db");

    // First run: nothing is acknowledged.
    let (first, mut ws) = connect(&db).await;
    let session_init = recv(&mut ws).await;
    assert_eq!(session_init["type"], "SESSION_INIT");
    let first_session = session_init["journal_session"].as_str().unwrap().to_owned();
    send(&mut ws, assign("t1", "stamp")).await;
    let frames = until_end(&mut ws, "t1").await;
    let kinds: Vec<&str> = frames.iter().map(|f| f["type"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "PROGRESS",
            "EFFECT",
            "EFFECT",
            "EFFECT",
            "YIELD",
            "COMPLETED"
        ]
    );
    let effects: Vec<&str> = frames[1..4]
        .iter()
        .map(|f| f["effect"].as_str().unwrap())
        .collect();
    assert_eq!(effects, ["NOW", "RANDOM", "SLEEP"]);
    assert!(frames[1]["value"].is_f64());
    assert_eq!(
        frames[2]["value"].as_str().unwrap().len(),
        8,
        "4 bytes as hex"
    );
    assert!(frames[3]["value"].as_f64().unwrap() >= frames[1]["value"].as_f64().unwrap());
    let steps: Vec<u64> = frames
        .iter()
        .map(|f| f["task_step"].as_u64().unwrap())
        .collect();
    assert_eq!(steps, [1, 2, 3, 4, 5, 6]);

    // A probe is reported, but never numbered, journaled or resent.
    let mut probe = assign("p-1", "stamp");
    probe["probe"] = json!(true);
    send(&mut ws, probe).await;
    let probe_frames = until_end(&mut ws, "p-1").await;
    assert_eq!(probe_frames.len(), 6);
    assert!(probe_frames
        .iter()
        .all(|f| f.get("pos").is_none() && f.get("task_step").is_none()));
    let sent_positions: Vec<u64> = std::iter::once(&session_init)
        .chain(&frames)
        .map(|f| f["pos"].as_u64().unwrap())
        .collect();
    first.shutdown().await;
    drop(ws);
    let store = rekuest::store::HistoryStore::open(&db).unwrap();
    assert!(store.journal_task_entries("p-1").await.unwrap().is_empty());
    assert_eq!(store.journal_task_entries("t1").await.unwrap().len(), 6);

    // Restart on the same journal: the old session comes first, in order, then the new one.
    let (second, mut ws) = connect(&db).await;
    let mut resent = vec![];
    for _ in 0..sent_positions.len() {
        let frame = recv(&mut ws).await;
        assert_eq!(frame["journal_session"], first_session.as_str());
        resent.push(frame["pos"].as_u64().unwrap());
    }
    assert_eq!(resent, sent_positions);
    let new_init = recv(&mut ws).await;
    assert_eq!(new_init["type"], "SESSION_INIT");
    assert_ne!(new_init["journal_session"], first_session.as_str());

    // Acknowledge the old session; a third run re-sends nothing of it.
    send(&mut ws, json!({"type": "JOURNAL_ACK", "journal_session": first_session, "pos": *sent_positions.last().unwrap()})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    second.shutdown().await;
    drop(ws);

    let (_third, mut ws) = connect(&db).await;
    let frame = recv(&mut ws).await;
    assert_ne!(
        frame["journal_session"],
        first_session.as_str(),
        "acknowledged entries are not re-sent"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shelved_value_is_announced_before_it_is_referenced() {
    let dir = tempfile::tempdir().unwrap();
    let (agent, mut ws) = connect(&dir.path().join("agent.db")).await;
    assert_eq!(recv(&mut ws).await["type"], "SESSION_INIT");
    send(&mut ws, assign("t1", "capture")).await;
    let frames = until_end(&mut ws, "t1").await;
    let shelve = frames
        .iter()
        .position(|f| f["type"] == "SHELVE")
        .expect("a SHELVE");
    let yielded = frames.iter().position(|f| f["type"] == "YIELD").unwrap();
    assert!(shelve < yielded);
    let id = frames[shelve]["resource_id"].as_str().unwrap().to_owned();
    assert_eq!(frames[shelve]["ref"], id.as_str());
    assert_eq!(
        frames[shelve]["task"], "t1",
        "SHELVE names the task that shelved"
    );
    assert_eq!(
        frames[shelve]["task_step"].as_u64().unwrap() + 1,
        frames[yielded]["task_step"].as_u64().unwrap()
    );
    assert_eq!(
        frames[yielded]["returns"]["return0"],
        json!({"__identifier": "@test/frame", "object": id})
    );
    assert_eq!(agent.executor().shelf().len(), 1);

    // The server collects it: dropped, and UNSHELVE is recorded.
    send(&mut ws, json!({"type": "COLLECT", "drawers": [id]})).await;
    let unshelve = recv(&mut ws).await;
    assert_eq!(
        (unshelve["type"].as_str(), unshelve["drawer"].as_str()),
        (Some("UNSHELVE"), Some(id.as_str()))
    );
    assert!(agent.executor().shelf().is_empty());
}
