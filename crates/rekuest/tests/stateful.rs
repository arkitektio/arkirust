//! States, locks, pause and hooks: the declaration against the Python
//! oracle, and the protocol against a fake rekuest server.

mod twin;

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rekuest::{Agent, AgentOptions, Context};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// Drop what the backend treats as unset: nulls and empty lists.
fn normalize(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, v)| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()))
                .map(|(k, v)| (k, normalize(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(normalize).collect()),
        other => other,
    }
}

#[test]
fn declaration_matches_python() {
    let python: Value =
        serde_json::from_str(include_str!("fixtures/python_state_declaration.json")).unwrap();
    let rust = serde_json::to_value(twin::registry().declaration(Some("twin:0.1.0".into()), None))
        .unwrap();

    assert_eq!(
        normalize(rust["states"].clone()),
        normalize(python["states"].clone())
    );
    assert_eq!(rust["locks"], python["locks"]);

    let by_interface = |v: &Value| -> Vec<(String, Value)> {
        v["implementations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    i["interface"].as_str().unwrap().to_owned(),
                    normalize(i.clone()),
                )
            })
            .collect()
    };
    let (python, rust) = (by_interface(&python), by_interface(&rust));
    assert_eq!(python.len(), rust.len());
    for ((pi, p), (ri, r)) in python.iter().zip(&rust) {
        assert_eq!(pi, ri);
        // Python's docstring-less generator fixture fields that Rust fills differently are
        // compared on what matters for the protocol.
        for field in ["locks", "manipulates", "interface"] {
            assert_eq!(p.get(field), r.get(field), "{pi}.{field}");
        }
        for field in ["stateful", "kind", "args", "returns", "key", "name"] {
            assert_eq!(
                p["definition"].get(field),
                r["definition"].get(field),
                "{pi}.definition.{field}"
            );
        }
    }
}

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

/// Frames up to and including one of type `until`, skipping PROGRESS.
async fn frames_until(ws: &mut Ws, until: &str) -> Vec<Value> {
    let mut frames = vec![];
    loop {
        let frame = recv(ws).await;
        let done = frame["type"] == until;
        if frame["type"] != "PROGRESS" {
            frames.push(frame);
        }
        if done {
            return frames;
        }
    }
}

fn kinds(frames: &[Value]) -> Vec<&str> {
    frames.iter().map(|f| f["type"].as_str().unwrap()).collect()
}

async fn send(ws: &mut Ws, value: Value) {
    ws.send(Message::Text(value.to_string())).await.unwrap();
}

fn assign(task: &str, interface: &str, args: Value, step: bool) -> Value {
    json!({
        "type": "ASSIGN", "id": format!("m-{task}"), "interface": interface, "task": task,
        "args": args, "user": "u", "org": "o", "action": "a", "implementation": "i", "step": step
    })
}

#[tokio::test]
async fn states_locks_and_pauses_over_the_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/agi", listener.local_addr().unwrap());
    let mut options = AgentOptions::new(url);
    options.journal_path = None;
    let agent = Arc::new(Agent::new(
        options,
        twin::registry(),
        Context::default(),
        Arc::new(StaticToken),
    ));
    let runner = agent.clone();
    tokio::spawn(async move { runner.run().await });

    let (stream, _) = listener.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();

    let register = recv(&mut ws).await;
    assert_eq!(register["states"][0]["interface"], "CameraState");
    assert_eq!(register["locks"][0]["key"], "camera");
    send(&mut ws, json!({"type": "INIT", "id": "i", "agent": "a"})).await;

    // Activation: the startup hook's value is the session baseline.
    let init = recv(&mut ws).await;
    assert_eq!(init["type"], "SESSION_INIT");
    assert_eq!(init["states"]["CameraState"]["connected"], true);
    assert!(
        init.get("seq").is_none(),
        "SESSION_INIT is not a task event"
    );
    assert_eq!(
        init["session_id"], register["session_id"],
        "states are published under the registered session"
    );

    // A stateful action: LOCK, STATE_PATCH, YIELD, COMPLETED, then UNLOCK.
    send(
        &mut ws,
        assign("t1", "set_exposure", json!({"exposure_ms": 20.0}), false),
    )
    .await;
    let frames = frames_until(&mut ws, "UNLOCK").await;
    assert_eq!(
        kinds(&frames),
        ["LOCK", "STATE_PATCH", "YIELD", "COMPLETED", "UNLOCK"]
    );
    let patch = &frames[1];
    assert_eq!(
        (
            &patch["op"],
            &patch["path"],
            &patch["value"],
            &patch["global_rev"],
            &patch["task_id"]
        ),
        (
            &json!("replace"),
            &json!("/exposure_ms"),
            &json!(20.0),
            &json!(1),
            &json!("t1")
        )
    );
    assert!(patch["old_value"].is_null());
    assert_eq!(patch["session_id"], init["session_id"]);

    send(&mut ws, assign("t2", "add_tag", json!({"tag": "a"}), false)).await;
    let frames = frames_until(&mut ws, "UNLOCK").await;
    assert_eq!(
        (&frames[1]["op"], &frames[1]["path"]),
        (&json!("add"), &json!("/tags/0"))
    );
    assert_eq!(frames[1]["global_rev"], 2);

    // Stepping: PAUSED at the first pausepoint, RESUMED on resume.
    send(&mut ws, assign("t3", "pausable", json!({}), true)).await;
    let paused = frames_until(&mut ws, "PAUSED").await;
    assert_eq!(kinds(&paused), ["PAUSED"]);
    send(
        &mut ws,
        json!({"type": "RESUME", "id": "r", "task": "t3", "step": false}),
    )
    .await;
    let frames = frames_until(&mut ws, "COMPLETED").await;
    assert_eq!(kinds(&frames), ["RESUMED", "YIELD", "COMPLETED"]);

    // Pausing a task that is not running is critical.
    send(&mut ws, json!({"type": "PAUSE", "id": "p", "task": "nope"})).await;
    let critical = frames_until(&mut ws, "CRITICAL").await;
    assert_eq!(critical[0]["task"], "nope");

    // State stays consistent with what was published.
    let hub = agent.executor().states();
    assert_eq!(hub.value("CameraState").unwrap()["tags"], json!(["a"]));
}
