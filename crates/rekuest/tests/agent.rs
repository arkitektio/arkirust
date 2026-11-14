//! The agent against an in-process fake of the rekuest `/agi` socket.

use std::sync::Arc;
use std::time::Duration;

use arkitekt_spec::{action, Context, Registry, Task};
use futures::{SinkExt, StreamExt};
use rekuest::{Agent, AgentError, AgentOptions};
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

/// Add two numbers
#[action]
async fn add(a: i64, b: i64) -> i64 {
    a + b
}

/// Sleeps forever
#[action]
async fn sleepy(task: Task) {
    task.log("going to sleep");
    tokio::time::sleep(Duration::from_secs(3600)).await;
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

/// Receive frames until one of `type`, skipping others (e.g. LOG, PROGRESS).
async fn recv_type(ws: &mut Ws, ty: &str) -> Value {
    loop {
        let frame = recv(ws).await;
        if frame["type"] == ty {
            return frame;
        }
    }
}

async fn send(ws: &mut Ws, value: Value) {
    ws.send(Message::Text(value.to_string())).await.unwrap();
}

fn assign(task: &str, interface: &str, args: Value) -> Value {
    json!({
        "type": "ASSIGN", "id": format!("m-{task}"), "interface": interface, "task": task,
        "args": args, "user": "u", "org": "o", "action": "a", "implementation": "i", "token": "tt"
    })
}

#[tokio::test]
async fn serves_assignments() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/agi", listener.local_addr().unwrap());

    let mut registry = Registry::new();
    registry.register(add).register(sleepy);
    let mut options = AgentOptions::new(url);
    options.name = Some("test:1".into());
    options.journal_path = None;
    let agent = Agent::new(options, registry, Context::default(), Arc::new(StaticToken));
    let agent_task = tokio::spawn(async move { agent.run().await });

    let (stream, _) = listener.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();

    // Registration.
    let register = recv(&mut ws).await;
    assert_eq!(register["type"], "REGISTER");
    assert_eq!(register["token"], "token");
    assert_eq!(register["name"], "test:1");
    assert_eq!(register["implementations"].as_array().unwrap().len(), 2);
    assert!(register["hash"].is_string());
    send(
        &mut ws,
        json!({"type": "INIT", "id": "i", "agent": "agent-1"}),
    )
    .await;

    // Asked about a task this agent never saw, it says so at once: an answer, not a report.
    send(
        &mut ws,
        json!({"type": "INQUIRE", "id": "q", "task": "lost", "request": "q-1"}),
    )
    .await;
    let answer = recv_type(&mut ws, "INQUIRY_ANSWER").await;
    assert_eq!(
        (&answer["task"], &answer["status"], &answer["request"]),
        (&json!("lost"), &json!("UNKNOWN"), &json!("q-1"))
    );
    assert!(answer.get("pos").is_none());

    // Heartbeats are answered.
    send(&mut ws, json!({"type": "HEARTBEAT", "id": "h"})).await;
    recv_type(&mut ws, "HEARTBEAT_ANSWER").await;

    // A function runs, yields and completes.
    send(&mut ws, assign("t1", "add", json!({"a": 2, "b": 3}))).await;
    let yielded = recv_type(&mut ws, "YIELD").await;
    assert_eq!(yielded["task"], "t1");
    assert_eq!(yielded["returns"], json!({"return0": 5}));
    let completed = recv_type(&mut ws, "COMPLETED").await;
    assert_eq!(completed["task"], "t1");
    assert!(completed["pos"].as_u64().unwrap() > yielded["pos"].as_u64().unwrap());

    // Asked about a task it finished, it says so.
    send(
        &mut ws,
        json!({"type": "INQUIRE", "id": "q2", "task": "t1", "request": "q-2"}),
    )
    .await;
    assert_eq!(
        recv_type(&mut ws, "INQUIRY_ANSWER").await["status"],
        "ENDED"
    );

    // Bad input fails; unknown interfaces are critical.
    send(&mut ws, assign("t2", "add", json!({"a": "x", "b": 3}))).await;
    assert_eq!(recv_type(&mut ws, "FAILED").await["task"], "t2");
    send(&mut ws, assign("t3", "nope", json!({}))).await;
    assert_eq!(recv_type(&mut ws, "CRITICAL").await["task"], "t3");

    // A long task can be cancelled.
    send(&mut ws, assign("t4", "sleepy", json!({}))).await;
    let log = recv_type(&mut ws, "LOG").await;
    assert_eq!(log["message"], "going to sleep");
    // Asked about a task under way, it says where it stands and what it last said.
    send(
        &mut ws,
        json!({"type": "INQUIRE", "id": "q3", "task": "t4", "request": "q-3"}),
    )
    .await;
    let running = recv_type(&mut ws, "INQUIRY_ANSWER").await;
    assert_eq!(
        (&running["status"], &running["message"]),
        (&json!("RUNNING"), &json!("going to sleep"))
    );
    assert!(
        running["reported_at"].as_f64().unwrap() > 0.0 && running["step"].as_u64().unwrap() > 0
    );
    send(&mut ws, json!({"type": "CANCEL", "id": "c", "task": "t4"})).await;
    assert_eq!(recv_type(&mut ws, "CANCELLED").await["task"], "t4");

    // Everything not covered by a JOURNAL_ACK is re-sent after a reconnect
    // (terminal reports included).
    let acked = completed["pos"].as_u64().unwrap();
    send(
        &mut ws,
        json!({"type": "JOURNAL_ACK", "id": "a", "journal_session": completed["journal_session"], "through": acked}),
    )
    .await;
    ws.close(None).await.unwrap();
    drop(ws);

    let (stream, _) = listener.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
    assert_eq!(recv(&mut ws).await["type"], "REGISTER");
    send(
        &mut ws,
        json!({"type": "INIT", "id": "i2", "agent": "agent-1"}),
    )
    .await;
    let (mut positions, mut terminals) = (vec![], vec![]);
    loop {
        let frame = recv(&mut ws).await;
        positions.push(frame["pos"].as_u64().unwrap());
        let task = frame["task"].as_str().unwrap_or_default().to_owned();
        if matches!(
            frame["type"].as_str(),
            Some("COMPLETED" | "FAILED" | "CRITICAL" | "CANCELLED")
        ) {
            terminals.push(task.clone());
        }
        if frame["type"] == "CANCELLED" && task == "t4" {
            break;
        }
    }
    let last = *positions.last().unwrap();
    assert_eq!(
        positions,
        (acked + 1..=last).collect::<Vec<_>>(),
        "gapless, in order"
    );
    // t1 was acked; the other terminal reports come again.
    assert_eq!(terminals, vec!["t2", "t3", "t4"]);

    // Being kicked is fatal.
    send(&mut ws, json!({"type": "KICK", "id": "k", "reason": "bye"})).await;
    let result = tokio::time::timeout(Duration::from_secs(5), agent_task)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(AgentError::Kicked(Some(ref r))) if r == "bye"));
}
