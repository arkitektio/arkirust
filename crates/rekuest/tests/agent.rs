//! The agent against an in-process fake of the rekuest `/agi` socket.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rekuest::{action, Agent, AgentError, AgentOptions, Context, Registry, Task};
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
        json!({"type": "INIT", "id": "i", "agent": "agent-1", "inquiries": [{"task": "lost"}]}),
    )
    .await;

    // An inquiry about a task this agent never saw is answered as lost.
    let lost = recv_type(&mut ws, "CRITICAL").await;
    assert_eq!(lost["task"], "lost");

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
    assert!(completed["seq"].as_u64().unwrap() > yielded["seq"].as_u64().unwrap());

    // Bad input fails; unknown interfaces are critical.
    send(&mut ws, assign("t2", "add", json!({"a": "x", "b": 3}))).await;
    assert_eq!(recv_type(&mut ws, "FAILED").await["task"], "t2");
    send(&mut ws, assign("t3", "nope", json!({}))).await;
    assert_eq!(recv_type(&mut ws, "CRITICAL").await["task"], "t3");

    // A long task can be cancelled.
    send(&mut ws, assign("t4", "sleepy", json!({}))).await;
    let log = recv_type(&mut ws, "LOG").await;
    assert_eq!(log["message"], "going to sleep");
    send(&mut ws, json!({"type": "CANCEL", "id": "c", "task": "t4"})).await;
    assert_eq!(recv_type(&mut ws, "CANCELLED").await["task"], "t4");

    // Unacknowledged terminal reports are re-sent after a reconnect.
    send(
        &mut ws,
        json!({"type": "EVENT_ACK", "id": "a", "event": completed["id"]}),
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
    let mut resent = vec![];
    for _ in 0..4 {
        let frame = recv(&mut ws).await;
        resent.push(frame["task"].as_str().unwrap().to_owned());
    }
    resent.sort();
    // t1's COMPLETED was acked; the other terminal reports come again.
    assert_eq!(resent, vec!["lost", "t2", "t3", "t4"]);

    // Being kicked is fatal.
    send(&mut ws, json!({"type": "KICK", "id": "k", "reason": "bye"})).await;
    let result = tokio::time::timeout(Duration::from_secs(5), agent_task)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(AgentError::Kicked(Some(ref r))) if r == "bye"));
}
