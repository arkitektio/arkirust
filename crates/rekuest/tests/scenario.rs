//! The std control: the tokio `Agent`, with a real executor and action, runs
//! the scripted scenario every driver of the session runs (`checks/wasm`,
//! `checks/embassy`), over a localhost websocket.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arkitekt_spec::{action, Context, Registry};
use futures::{SinkExt, StreamExt};
use rekuest::{Agent, AgentOptions, ConnectionPolicy};
use rekuest_session::testing::{FakeServer, ServerOutput};
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

/// Hand `x` back
#[action]
async fn echo(x: i64) -> i64 {
    x
}

/// Serve `server` on `listener`, one connection at a time; a `Drop` ends one.
async fn serve(listener: TcpListener, server: Arc<Mutex<FakeServer>>) {
    loop {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        server.lock().unwrap().connected();
        'connection: while let Some(Ok(frame)) = ws.next().await {
            let Message::Text(text) = frame else { continue };
            let outputs: Vec<ServerOutput> = {
                let mut server = server.lock().unwrap();
                server.receive(&text);
                std::iter::from_fn(|| server.poll()).collect()
            };
            for output in outputs {
                match output {
                    ServerOutput::Frame(frame) => ws.send(Message::Text(frame)).await.unwrap(),
                    ServerOutput::Drop => break 'connection,
                }
            }
        }
    }
}

#[tokio::test]
async fn the_scripted_scenario() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/agi", listener.local_addr().unwrap());
    let server = Arc::new(Mutex::new(FakeServer::scripted()));
    tokio::spawn(serve(listener, server.clone()));

    let mut options = AgentOptions::new(url);
    options.journal_path = None;
    options.policy = ConnectionPolicy {
        initial: Duration::from_millis(10),
        ..Default::default()
    };
    let mut registry = Registry::new();
    registry.register(echo);
    let agent = Agent::new(options, registry, Context::default(), Arc::new(StaticToken));
    let running = tokio::spawn(async move { agent.run().await });

    let deadline = Instant::now() + Duration::from_secs(10);
    while !server.lock().unwrap().done() {
        assert!(
            !running.is_finished(),
            "the agent stopped: {:?}",
            running.await
        );
        assert!(
            Instant::now() < deadline,
            "the scenario did not finish in 10 s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.lock().unwrap().check();
    running.abort();
}
