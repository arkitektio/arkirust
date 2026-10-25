//! The agent in a browser or in node (wasm32, the `web` feature): the
//! [`Driver`] over the JS `WebSocket`, on the JS event loop ([`rt::Js`]).
//!
//! The same as [`Agent`](crate::agent) on tokio, minus what a browser does
//! not have: no local journal file (unacknowledged frames are kept in
//! memory) and no choice of connection (the browser opens it, TLS included).
//!
//! ```ignore
//! let fakts = Fakts::builder(url, manifest).grant(grant).no_cache(true).load().await?;
//! let alias = fakts.get_alias("rekuest").await?;
//! let agent = rekuest::web::Agent::new(
//!     AgentOptions::new(alias.to_ws_path("agi")),
//!     registry,
//!     Context::default(),
//!     Arc::new(fakts),
//! );
//! wasm_bindgen_futures::spawn_local(async move { agent.run().await.ok(); });
//! ```

use std::sync::Arc;
use std::time::Duration;

use fakts::TokenLoader;
use futures::StreamExt;
use rekuest_session::{Outbox, SessionConfig};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{CloseEvent, MessageEvent, WebSocket};

use crate::driver::Driver;
use crate::executor::Executor;
use crate::journal::Journal;
use crate::link::{self, Dial, Link, Socket};
use arkitekt_spec::AgentDeclaration;
use arkitekt_spec::Context;
use arkitekt_spec::Registry;

pub use crate::agent_error::AgentError;
pub use rekuest_session::ConnectionPolicy;

#[derive(Debug, Clone)]
pub struct AgentOptions {
    /// `ws(s)://…/agi`
    pub endpoint_url: String,
    /// Shown in the UI; the server falls back to the client id.
    pub name: Option<String>,
    pub description: Option<String>,
    /// Take over from another connection of the same agent.
    pub force: bool,
    pub policy: ConnectionPolicy,
}

impl AgentOptions {
    pub fn new(endpoint_url: impl Into<String>) -> Self {
        Self {
            endpoint_url: endpoint_url.into(),
            name: None,
            description: None,
            force: false,
            policy: ConnectionPolicy::default(),
        }
    }
}

/// What the socket's callbacks hand the agent.
enum Event {
    Open,
    Text(String),
    Closed { code: u16, reason: String },
    Error,
}

/// An open (or opening) JS WebSocket and its callbacks.
struct JsSocket {
    ws: WebSocket,
    incoming: futures::channel::mpsc::UnboundedReceiver<Event>,
    _callbacks: [Closure<dyn FnMut(web_sys::Event)>; 3],
    _on_message: Closure<dyn FnMut(MessageEvent)>,
}

impl JsSocket {
    fn open(url: &str) -> Result<Self, String> {
        let ws = WebSocket::new(url).map_err(|e| format!("{e:?}"))?;
        let (tx, incoming) = futures::channel::mpsc::unbounded();
        let on_open = {
            let tx = tx.clone();
            Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                let _ = tx.unbounded_send(Event::Open);
            })
        };
        let on_close = {
            let tx = tx.clone();
            Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
                let (code, reason) = event
                    .dyn_ref::<CloseEvent>()
                    .map(|e| (e.code(), e.reason()))
                    .unwrap_or((1006, String::new()));
                let _ = tx.unbounded_send(Event::Closed { code, reason });
            })
        };
        let on_error = {
            let tx = tx.clone();
            Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                let _ = tx.unbounded_send(Event::Error);
            })
        };
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            if let Some(text) = event.data().as_string() {
                let _ = tx.unbounded_send(Event::Text(text));
            }
        });
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        Ok(Self {
            ws,
            incoming,
            _callbacks: [on_open, on_close, on_error],
            _on_message: on_message,
        })
    }

    /// Wait until the socket is open, or say why it is not.
    async fn opened(&mut self) -> Result<(), String> {
        match self.incoming.next().await {
            Some(Event::Open) => Ok(()),
            Some(Event::Closed { code, reason }) => Err(format!("closed with {code} {reason}")),
            Some(Event::Error) | None => Err("the connection failed".into()),
            Some(Event::Text(_)) => Err("a frame before the connection opened".into()),
        }
    }
}

impl Socket for JsSocket {
    /// The browser answers pings itself. Cancel-safe: frames wait in the channel.
    async fn next(&mut self) -> Result<link::Incoming, String> {
        loop {
            return match self.incoming.next().await {
                Some(Event::Text(text)) => Ok(link::Incoming::Text(text)),
                Some(Event::Closed { code, reason }) => Ok(link::Incoming::Closed { code, reason }),
                Some(Event::Error) | None => Err("socket error".into()),
                Some(Event::Open) => continue,
            };
        }
    }

    async fn send_text(&mut self, text: &str) -> Result<(), String> {
        self.ws.send_with_str(text).map_err(|e| format!("{e:?}"))
    }

    async fn pong(&mut self, _payload: &[u8]) -> Result<(), String> {
        Ok(())
    }

    async fn close(&mut self) {
        let _ = self.ws.close();
    }
}

/// Opens JS WebSockets.
struct Browser;

impl Dial for Browser {
    type Socket<'a> = JsSocket;

    async fn dial(&mut self, url: &str) -> Result<JsSocket, String> {
        let mut opening = JsSocket::open(url)?;
        opening.opened().await?;
        Ok(opening)
    }
}

impl Drop for JsSocket {
    fn drop(&mut self) {
        self.ws.set_onopen(None);
        self.ws.set_onclose(None);
        self.ws.set_onerror(None);
        self.ws.set_onmessage(None);
        let _ = self.ws.close();
    }
}

/// Milliseconds on the JS clock, as the session's monotonic `now`.
fn clock() -> impl Fn() -> Duration {
    let start = js_sys::Date::now();
    move || Duration::from_secs_f64(((js_sys::Date::now() - start) / 1000.0).max(0.0))
}

/// Serves an app's actions to a rekuest server from wasm.
pub struct Agent {
    options: AgentOptions,
    driver: Driver,
    tokens: Arc<dyn TokenLoader>,
}

impl Agent {
    pub fn new(
        options: AgentOptions,
        registry: Registry,
        ctx: Context,
        tokens: Arc<dyn TokenLoader>,
    ) -> Self {
        let seed = (js_sys::Math::random() * u64::MAX as f64) as u64;
        let config = SessionConfig {
            force: options.force,
            policy: options.policy.clone(),
            ..SessionConfig::new(uuid::Uuid::new_v4().to_string(), seed)
        };
        Self {
            driver: Driver::new(registry, ctx, config, Outbox::new(), None),
            options,
            tokens,
        }
    }

    pub fn executor(&self) -> &Executor {
        self.driver.executor()
    }

    /// The session, executor and journal underneath.
    pub fn driver(&self) -> &Driver {
        &self.driver
    }

    pub fn journal(&self) -> &Journal {
        self.driver.journal()
    }

    /// What this agent registers.
    pub fn declaration(&self) -> AgentDeclaration {
        self.driver
            .declaration(self.options.name.clone(), self.options.description.clone())
    }

    /// Serve until a fatal error (or until the retry budget is exhausted).
    /// Call [`Agent::shutdown`] afterwards to run shutdown hooks.
    pub async fn run(&self) -> Result<(), AgentError> {
        let link = Link {
            driver: &self.driver,
            endpoint: &self.options.endpoint_url,
            tokens: self.tokens.as_ref(),
            // No journal file: what is unacknowledged is kept in memory.
            backlog: None,
            name: self.options.name.clone(),
            description: self.options.description.clone(),
        };
        link::run(&link, &mut Browser, &clock()).await
    }

    /// Run shutdown hooks and flush state changes.
    pub async fn shutdown(&self) {
        self.executor().teardown().await;
    }
}
