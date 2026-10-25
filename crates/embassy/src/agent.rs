//! The rekuest agent on embassy: rekuest's [`Driver`] over the [`ws`](crate::ws)
//! client, on a TCP connection from embedded-nal-async (`TcpClient` and
//! `DnsSocket` on embassy-net), TLS 1.3 for `wss://`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::time::Duration;

use arkitekt_spec::{AgentDeclaration, Context, Registry};
use embassy_futures::select::{select, select3, Either3};
use embassy_time::{Instant, Timer};
use embedded_io_async::{Read, Write};
use embedded_nal_async::{Dns, TcpConnect};
use fakts::TokenLoader;
use rekuest::driver::Driver;
use rekuest::executor::Executor;
use rekuest::journal::Journal;
use rekuest_session::{ConnectionPolicy, Event, Outbox, SessionConfig, SessionError};

use crate::conn::{TlsBuffers, Verify};
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
    /// `ws://…/agi` or `wss://…/agi`
    pub endpoint_url: String,
    /// Shown in the UI; the server falls back to the client id.
    pub name: Option<String>,
    pub description: Option<String>,
    /// Take over from another connection of the same agent.
    pub force: bool,
    pub policy: ConnectionPolicy,
    /// How a `wss://` server is checked.
    pub verify: Verify,
}

impl AgentOptions {
    /// `wss://` servers unverified: set [`verify`](Self::verify) to their CA.
    pub fn new(endpoint_url: impl Into<String>) -> Self {
        Self {
            endpoint_url: endpoint_url.into(),
            name: None,
            description: None,
            force: false,
            policy: ConnectionPolicy::default(),
            verify: Verify::Unverified,
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
        arkitekt_spec::rt::fill_random(&mut seed);
        let config = SessionConfig {
            force: options.force,
            policy: options.policy.clone(),
            ..SessionConfig::new(
                arkitekt_spec::rt::uuid().to_string(),
                u64::from_le_bytes(seed),
            )
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
        let now = move || Duration::from_micros(start.elapsed().as_micros());
        self.driver.with_session(|session| session.restart(now()));
        // The TLS records of whichever connection is open.
        let mut buffers = TlsBuffers::default();

        // Disconnected: wait for the next attempt, then connect.
        loop {
            let polled = self.driver.poll(now());
            let mut connect = false;
            for event in polled.events {
                match self.driver.act(event) {
                    Some(Event::Connect) => connect = true,
                    // Nothing was dropped from memory: there is no journal file.
                    Some(Event::ReloadJournal) => {
                        self.driver.with_session(|session| session.reloaded(None))
                    }
                    Some(Event::Ended(error)) => return Err(AgentError::Session(error)),
                    _ => {}
                }
            }
            if connect {
                let token = self.tokens.get_token().await.map_err(AgentError::Token)?;
                match self.open(tcp, dns, &mut buffers).await {
                    Ok(socket) => {
                        let declaration = self.declaration();
                        self.driver
                            .with_session(|session| session.connected(now(), token, declaration));
                        self.serve(socket, start).await?;
                    }
                    Err(e) => self.driver.with_session(|session| {
                        session.connect_failed(
                            now(),
                            &format!("connect to {}: {e}", self.options.endpoint_url),
                        )
                    }),
                }
                continue;
            }
            let deadline = deadline(start, polled.deadline);
            select(Timer::at(deadline), self.driver.woken()).await;
        }
    }

    /// Run shutdown hooks and flush state changes.
    pub async fn shutdown(&self) {
        self.executor().teardown().await;
    }

    async fn open<'a, 'b, T: TcpConnect, D: Dns>(
        &self,
        tcp: &'a T,
        dns: &D,
        buffers: &'b mut TlsBuffers,
    ) -> Result<WebSocket<crate::conn::Transport<'b, T::Connection<'a>>>, String>
    where
        'a: 'b,
    {
        let url = &self.options.endpoint_url;
        let (secure, host, port, path) =
            crate::http::split_url(url).ok_or_else(|| format!("not a websocket url: {url}"))?;
        let transport =
            crate::conn::open(tcp, dns, secure, &host, port, &self.options.verify, buffers).await?;
        let authority = crate::http::authority(&host, port, secure);
        WebSocket::connect(transport, &authority, &path, arkitekt_spec::rt::fill_random)
            .await
            .map_err(|e| format!("{e}"))
    }

    /// One connection, until the session is done with it.
    async fn serve<S: Read + Write>(
        &self,
        mut socket: WebSocket<S>,
        start: Instant,
    ) -> Result<(), AgentError> {
        let now = move || Duration::from_micros(start.elapsed().as_micros());
        loop {
            let polled = self.driver.poll(now());
            let busy = !polled.events.is_empty() || !polled.frames.is_empty();
            for frame in &polled.frames {
                if let Err(e) = socket.send_text(frame).await {
                    self.driver.with_session(|session| {
                        session.handle_disconnect(now(), &format!("send: {e}"))
                    });
                    break;
                }
            }
            let mut closed = false;
            for event in polled.events {
                match self.driver.act(event) {
                    Some(Event::Close) => closed = true,
                    Some(Event::ReloadJournal) => {
                        self.driver.with_session(|session| session.reloaded(None))
                    }
                    Some(Event::Ended(error)) => {
                        socket.close().await;
                        return Err(AgentError::Session(error));
                    }
                    _ => {}
                }
            }
            if closed {
                socket.close().await;
                return Ok(());
            }
            if busy {
                continue;
            }

            let deadline = deadline(start, polled.deadline);
            let ping = match select3(socket.next(), Timer::at(deadline), self.driver.woken()).await
            {
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
            if let Some(payload) = ping {
                let _ = socket.pong(&payload).await;
            }
        }
    }
}

fn deadline(start: Instant, at: Option<Duration>) -> Instant {
    match at {
        Some(at) => start + embassy_time::Duration::from_micros(at.as_micros() as u64),
        None => Instant::MAX,
    }
}
