//! The rekuest agent on embassy: rekuest's [`Driver`] over the [`ws`](crate::ws)
//! client, on a TCP connection from embedded-nal-async (`TcpClient` and
//! `DnsSocket` on embassy-net).
//!
//! `ws://` only for now: a `wss://` endpoint (every hub behind TLS) needs the
//! websocket over embedded-tls, which is not here yet.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::net::SocketAddr;
use core::time::Duration;

use embassy_futures::select::{select3, Either3};
use embassy_time::{Instant, Timer};
use embedded_nal_async::{AddrType, Dns, TcpConnect};
use fakts::TokenLoader;
use rekuest::driver::Driver;
use rekuest::executor::Executor;
use rekuest::journal::Journal;
use rekuest::{AgentDeclaration, Context, Registry};
use rekuest_session::{ConnectionPolicy, Event, Outbox, SessionConfig, SessionError};

use crate::ws::{Incoming, WebSocket};

#[derive(Debug)]
pub enum AgentError {
    /// No token for the registration.
    Token(fakts::FaktsError),
    /// The session ended for good (refused, kicked, out of retries, …).
    Session(SessionError),
}

impl core::fmt::Display for AgentError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Token(e) => write!(f, "could not get a token: {e}"),
            Self::Session(e) => write!(f, "{e}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentOptions {
    /// `ws://…/agi`
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

/// Serves an app's actions to a rekuest server from a board.
pub struct Agent {
    options: AgentOptions,
    driver: Driver,
    tokens: Arc<dyn TokenLoader>,
}

impl Agent {
    /// Needs rekuest's runtime installed ([`Embassy`](crate::Embassy)).
    pub fn new(
        options: AgentOptions,
        registry: Registry,
        ctx: Context,
        tokens: Arc<dyn TokenLoader>,
    ) -> Self {
        let mut seed = [0u8; 8];
        rekuest::rt::fill_random(&mut seed);
        let config = SessionConfig {
            force: options.force,
            policy: options.policy.clone(),
            ..SessionConfig::new(rekuest::rt::uuid().to_string(), u64::from_le_bytes(seed))
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

    pub fn journal(&self) -> &Journal {
        self.driver.journal()
    }

    /// The session, executor and journal underneath.
    pub fn driver(&self) -> &Driver {
        &self.driver
    }

    /// What this agent registers.
    pub fn declaration(&self) -> AgentDeclaration {
        self.driver
            .declaration(self.options.name.clone(), self.options.description.clone())
    }

    /// Serve until a fatal error (or until the retry budget is exhausted).
    pub async fn run<T: TcpConnect, D: Dns>(&self, tcp: &T, dns: &D) -> Result<(), AgentError> {
        let start = Instant::now();
        let now = || Duration::from_micros(start.elapsed().as_micros());
        self.driver.with_session(|session| session.restart(now()));
        let mut socket: Option<WebSocket<T::Connection<'_>>> = None;

        loop {
            let polled = self.driver.poll(now());
            let busy = !polled.events.is_empty() || !polled.frames.is_empty();
            if let Some(open) = socket.as_mut() {
                for frame in &polled.frames {
                    if let Err(e) = open.send_text(frame).await {
                        self.driver.with_session(|session| {
                            session.handle_disconnect(now(), &format!("send: {e}"))
                        });
                        break;
                    }
                }
            }
            for event in polled.events {
                match self.driver.act(event) {
                    Some(Event::Connect) => {
                        let token = self.tokens.get_token().await.map_err(AgentError::Token)?;
                        match connect(tcp, dns, &self.options.endpoint_url).await {
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
                    Some(Event::Close) => {
                        if let Some(mut open) = socket.take() {
                            open.close().await;
                        }
                    }
                    // Nothing was dropped from memory: there is no journal file.
                    Some(Event::ReloadJournal) => {
                        self.driver.with_session(|session| session.reloaded(None))
                    }
                    Some(Event::Ended(error)) => return Err(AgentError::Session(error)),
                    _ => {}
                }
            }
            if busy {
                continue;
            }

            let deadline = match polled.deadline {
                Some(at) => start + embassy_time::Duration::from_micros(at.as_micros() as u64),
                None => Instant::MAX,
            };
            let Some(open) = socket.as_mut() else {
                embassy_futures::select::select(Timer::at(deadline), self.driver.woken()).await;
                continue;
            };
            let ping = match select3(open.next(), Timer::at(deadline), self.driver.woken()).await {
                Either3::First(Ok(Incoming::Text(text))) => {
                    self.driver
                        .with_session(|session| session.handle_text(now(), &text));
                    None
                }
                Either3::First(Ok(Incoming::Ping(payload))) => Some(payload),
                Either3::First(Ok(Incoming::Closed { code, reason })) => {
                    self.driver
                        .with_session(|session| session.handle_close(now(), code, &reason));
                    None
                }
                Either3::First(Err(e)) => {
                    self.driver.with_session(|session| {
                        session.handle_disconnect(now(), &format!("read: {e}"))
                    });
                    None
                }
                Either3::Second(()) | Either3::Third(()) => None,
            };
            if let (Some(payload), Some(open)) = (ping, socket.as_mut()) {
                let _ = open.pong(&payload).await;
            }
        }
    }

    /// Run shutdown hooks and flush state changes.
    pub async fn shutdown(&self) {
        self.executor().teardown().await;
    }
}

/// Resolve, connect and upgrade.
async fn connect<'a, T: TcpConnect, D: Dns>(
    tcp: &'a T,
    dns: &D,
    url: &str,
) -> Result<WebSocket<T::Connection<'a>>, String> {
    let (secure, host, port, path) =
        crate::http::split_url(url).ok_or_else(|| format!("not a websocket url: {url}"))?;
    if secure {
        return Err(
            "wss:// is not supported by arkitekt-embassy yet (no websocket over TLS)".into(),
        );
    }
    let ip = match host.parse() {
        Ok(ip) => ip,
        Err(_) => dns
            .get_host_by_name(&host, AddrType::Either)
            .await
            .map_err(|e| format!("resolve {host}: {e:?}"))?,
    };
    let connection = tcp
        .connect(SocketAddr::new(ip, port))
        .await
        .map_err(|e| format!("{e:?}"))?;
    let authority = if port == 80 {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    WebSocket::connect(connection, &authority, &path, rekuest::rt::fill_random)
        .await
        .map_err(|e| format!("{e}"))
}
