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
use futures::future::{select, Either};
use futures::StreamExt;
use rekuest_session::{Event, Outbox, SessionConfig};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{CloseEvent, MessageEvent, WebSocket};

use crate::driver::Driver;
use crate::executor::Executor;
use crate::journal::Journal;
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
enum Incoming {
    Open,
    Text(String),
    Closed { code: u16, reason: String },
    Error,
}

/// An open (or opening) JS WebSocket and its callbacks.
struct Socket {
    ws: WebSocket,
    incoming: futures::channel::mpsc::UnboundedReceiver<Incoming>,
    _callbacks: [Closure<dyn FnMut(web_sys::Event)>; 3],
    _on_message: Closure<dyn FnMut(MessageEvent)>,
}

impl Socket {
    fn open(url: &str) -> Result<Self, String> {
        let ws = WebSocket::new(url).map_err(|e| format!("{e:?}"))?;
        let (tx, incoming) = futures::channel::mpsc::unbounded();
        let on_open = {
            let tx = tx.clone();
            Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                let _ = tx.unbounded_send(Incoming::Open);
            })
        };
        let on_close = {
            let tx = tx.clone();
            Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
                let (code, reason) = event
                    .dyn_ref::<CloseEvent>()
                    .map(|e| (e.code(), e.reason()))
                    .unwrap_or((1006, String::new()));
                let _ = tx.unbounded_send(Incoming::Closed { code, reason });
            })
        };
        let on_error = {
            let tx = tx.clone();
            Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                let _ = tx.unbounded_send(Incoming::Error);
            })
        };
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            if let Some(text) = event.data().as_string() {
                let _ = tx.unbounded_send(Incoming::Text(text));
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
            Some(Incoming::Open) => Ok(()),
            Some(Incoming::Closed { code, reason }) => Err(format!("closed with {code} {reason}")),
            Some(Incoming::Error) | None => Err("the connection failed".into()),
            Some(Incoming::Text(_)) => Err("a frame before the connection opened".into()),
        }
    }
}

impl Drop for Socket {
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
        let now = clock();
        self.driver.with_session(|session| session.restart(now()));
        let mut socket: Option<Socket> = None;

        loop {
            let polled = self.driver.poll(now());
            let busy = !polled.events.is_empty() || !polled.frames.is_empty();
            if let Some(open) = &socket {
                for frame in &polled.frames {
                    if let Err(e) = open.ws.send_with_str(frame) {
                        self.driver.with_session(|session| {
                            session.handle_disconnect(now(), &format!("send: {e:?}"))
                        });
                        break;
                    }
                }
            }
            for event in polled.events {
                match self.driver.act(event) {
                    Some(Event::Connect) => {
                        let token = self.tokens.get_token().await?;
                        let opened = match Socket::open(&self.options.endpoint_url) {
                            Ok(mut opening) => opening.opened().await.map(|()| opening),
                            Err(e) => Err(e),
                        };
                        match opened {
                            Ok(open) => {
                                socket = Some(open);
                                let declaration = self.declaration();
                                self.driver.with_session(|session| {
                                    session.connected(now(), token, declaration)
                                });
                            }
                            Err(e) => self.driver.with_session(|session| {
                                session.connect_failed(
                                    now(),
                                    &format!("connect to {}: {e}", self.options.endpoint_url),
                                )
                            }),
                        }
                    }
                    Some(Event::Close) => socket = None,
                    // Nothing was dropped from memory: there is no journal file.
                    Some(Event::ReloadJournal) => {
                        self.driver.with_session(|session| session.reloaded(None))
                    }
                    Some(Event::Ended(error)) => return Err(error.into()),
                    _ => {}
                }
            }
            if busy {
                continue;
            }

            let wait = polled
                .deadline
                .map(|at| at.saturating_sub(now()))
                .unwrap_or(Duration::from_secs(3600));
            let timer = gloo_timers::future::TimeoutFuture::new(
                wait.as_millis().min(u32::MAX as u128) as u32,
            );
            let woken = std::pin::pin!(self.driver.woken());
            let quiet = select(timer, woken);
            let Some(open) = socket.as_mut() else {
                quiet.await;
                continue;
            };
            match select(open.incoming.next(), quiet).await {
                Either::Left((incoming, _)) => self.driver.with_session(|session| match incoming {
                    Some(Incoming::Text(text)) => session.handle_text(now(), &text),
                    Some(Incoming::Closed { code, reason }) => {
                        session.handle_close(now(), code, &reason)
                    }
                    Some(Incoming::Error) | None => {
                        session.handle_disconnect(now(), "socket error")
                    }
                    Some(Incoming::Open) => {}
                }),
                Either::Right(_) => {}
            }
        }
    }

    /// Run shutdown hooks and flush state changes.
    pub async fn shutdown(&self) {
        self.executor().teardown().await;
    }
}
