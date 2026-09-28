//! Test a served app over real HTTP and websocket connections
//! (the Rust twin of Python's `AsyncAgentTestClient`).
//!
//! ```ignore
//! let (router, agent) = configure(Router::new(), registry, Context::default(), ServeOptions::default().history(History::Memory))?;
//! let mut client = AgentTestClient::serve(router, &agent).await?;
//! let task = client.assign("count_up", json!({"until": 2}), false).await?.task_id;
//! let events = client.collect_until_end_state(&task, Duration::from_secs(5)).await?;
//! assert_eq!(events.last().unwrap().event_type(), "COMPLETED");
//! ```

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use super::LocalAgent;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// One websocket frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub data: Value,
}

impl Event {
    pub fn event_type(&self) -> &str {
        self.data["type"].as_str().unwrap_or("UNKNOWN")
    }

    pub fn task(&self) -> Option<&str> {
        self.data["task"].as_str()
    }

    pub fn is_done(&self) -> bool {
        self.event_type() == "COMPLETED"
    }

    pub fn is_end_state(&self) -> bool {
        matches!(
            self.event_type(),
            "COMPLETED" | "FAILED" | "CRITICAL" | "CANCELLED" | "INTERRUPTED"
        )
    }

    pub fn is_yield(&self) -> bool {
        self.event_type() == "YIELD"
    }

    pub fn is_error(&self) -> bool {
        matches!(self.event_type(), "FAILED" | "CRITICAL")
    }

    pub fn is_progress(&self) -> bool {
        self.event_type() == "PROGRESS"
    }

    pub fn is_log(&self) -> bool {
        self.event_type() == "LOG"
    }

    /// `returns` of a YIELD.
    pub fn returns(&self) -> Option<&Value> {
        self.data.get("returns")
    }
}

/// The answer to an assign.
#[derive(Debug, Clone)]
pub struct AssignmentResult {
    pub status: String,
    pub task_id: String,
    pub response: Value,
}

/// An HTTP client plus one websocket subscription.
pub struct AgentTestClient {
    base_url: String,
    http: reqwest::Client,
    ws: Ws,
    init: Value,
}

impl std::fmt::Debug for AgentTestClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTestClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl AgentTestClient {
    /// Start `agent`, serve `router` on a free local port and connect to it.
    pub async fn serve(router: axum::Router, agent: &LocalAgent) -> anyhow::Result<Self> {
        agent.start().await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base_url = format!("http://{}", listener.local_addr()?);
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Self::connect(&base_url, "/ws", None).await
    }

    /// Connect to an app served at `base_url`, subscribing to everything.
    pub async fn connect(
        base_url: &str,
        ws_path: &str,
        token: Option<&str>,
    ) -> anyhow::Result<Self> {
        let base_url = base_url.trim_end_matches('/').to_owned();
        let ws_url = format!("{}{ws_path}", base_url.replacen("http", "ws", 1));
        let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await?;
        let mut init = json!({ "type": "INIT" });
        if let Some(token) = token {
            init["token"] = json!(token);
        }
        ws.send(Message::Text(init.to_string())).await?;
        let mut client = Self {
            base_url,
            http: reqwest::Client::new(),
            ws,
            init: Value::Null,
        };
        client.init = client
            .receive_event(Duration::from_secs(5))
            .await
            .ok_or_else(|| anyhow::anyhow!("the server sent no INIT frame"))?
            .data;
        Ok(client)
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// The INIT snapshot received on connect.
    pub fn init(&self) -> &Value {
        &self.init
    }

    pub async fn get(&self, path: &str) -> anyhow::Result<Value> {
        Ok(self
            .http
            .get(format!("{}{path}", self.base_url))
            .send()
            .await?
            .json()
            .await?)
    }

    pub async fn post(&self, path: &str, body: Value) -> anyhow::Result<Value> {
        Ok(self
            .http
            .post(format!("{}{path}", self.base_url))
            .json(&body)
            .send()
            .await?
            .json()
            .await?)
    }

    /// Start a task through the action's own route (`POST /{interface}`) or
    /// through `POST /assign/{interface}`.
    pub async fn assign(
        &self,
        interface: &str,
        args: Value,
        use_implementation_route: bool,
    ) -> anyhow::Result<AssignmentResult> {
        let path = if use_implementation_route {
            format!("/{interface}")
        } else {
            format!("/assign/{interface}")
        };
        let response = self
            .http
            .post(format!("{}{path}", self.base_url))
            .json(&json!({ "args": args, "interface": interface }))
            .send()
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "assign failed: HTTP {}",
            response.status()
        );
        let body: Value = response.json().await?;
        // The action route answers `task_id`, `/assign` answers `task`.
        let task_id = body["task_id"]
            .as_str()
            .or(body["task"].as_str())
            .unwrap_or_default()
            .to_owned();
        Ok(AssignmentResult {
            status: body["status"].as_str().unwrap_or_default().to_owned(),
            task_id,
            response: body,
        })
    }

    /// The next frame, or `None` after `timeout`.
    pub async fn receive_event(&mut self, timeout: Duration) -> Option<Event> {
        loop {
            match tokio::time::timeout(timeout, self.ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    return serde_json::from_str(&text).ok().map(|data| Event { data });
                }
                Ok(Some(Ok(_))) => continue,
                _ => return None,
            }
        }
    }

    /// Frames of `task` up to and including its end.
    pub async fn collect_until_end_state(
        &mut self,
        task: &str,
        timeout: Duration,
    ) -> anyhow::Result<Vec<Event>> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut events = vec![];
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let event = self
                .receive_event(left)
                .await
                .ok_or_else(|| anyhow::anyhow!("task {task} did not end within {timeout:?}"))?;
            if event.task() != Some(task) {
                continue;
            }
            let end = event.is_end_state();
            events.push(event);
            if end {
                return Ok(events);
            }
        }
    }

    /// Like [`collect_until_end_state`](Self::collect_until_end_state), but
    /// returns what was collected when the time is up.
    pub async fn collect_until_done(&mut self, task: &str, timeout: Duration) -> Vec<Event> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut events = vec![];
        while let Some(event) = self
            .receive_event(deadline.saturating_duration_since(tokio::time::Instant::now()))
            .await
        {
            if event.task() != Some(task) {
                continue;
            }
            let end = event.is_end_state();
            events.push(event);
            if end {
                break;
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::{configure, History, ServeOptions};
    use crate::{action, Context, Registry};

    /// Double
    #[action]
    async fn double(x: i64) -> i64 {
        x * 2
    }

    #[tokio::test]
    async fn assigns_and_collects() {
        let mut registry = Registry::new();
        registry.register(double);
        let (router, agent) = configure(
            axum::Router::new(),
            registry,
            Context::default(),
            ServeOptions::default().history(History::Memory),
        )
        .unwrap();
        let mut client = AgentTestClient::serve(router, &agent).await.unwrap();
        assert_eq!(client.init()["type"], "INIT");

        for use_route in [true, false] {
            let task = client
                .assign("double", json!({"x": 21}), use_route)
                .await
                .unwrap()
                .task_id;
            let events = client
                .collect_until_end_state(&task, Duration::from_secs(5))
                .await
                .unwrap();
            let returns = events
                .iter()
                .find(|e| e.is_yield())
                .and_then(|e| e.returns())
                .unwrap();
            assert_eq!(returns["return0"], 42);
            assert!(events.last().unwrap().is_done());
        }
    }
}
