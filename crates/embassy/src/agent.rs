//! The rekuest agent on embassy: rekuest's [link](rekuest::link) over the
//! [`ws`](crate::ws) client, on a TCP connection from embedded-nal-async
//! (`TcpClient` and `DnsSocket` on embassy-net), TLS 1.3 for `wss://`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::time::Duration;

use arkitekt_spec::{AgentDeclaration, Context, Registry};
use embassy_time::Instant;
use embedded_io_async::{Read, Write};
use embedded_nal_async::{Dns, TcpConnect};
use fakts::TokenLoader;
use rekuest::driver::Driver;
use rekuest::executor::Executor;
use rekuest::journal::Journal;
use rekuest::link::{self, Dial, Link, Socket};
use rekuest_session::{ConnectionPolicy, Outbox, SessionConfig};

use crate::conn::{TlsBuffers, Transport, Verify};
use crate::ws::{Incoming, WebSocket};

pub use rekuest::AgentError;

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
        let link = Link {
            driver: &self.driver,
            endpoint: &self.options.endpoint_url,
            tokens: self.tokens.as_ref(),
            // No journal file: what is unacknowledged is kept in memory.
            backlog: None,
            name: self.options.name.clone(),
            description: self.options.description.clone(),
        };
        let mut dial = Board {
            tcp,
            dns,
            verify: &self.options.verify,
            buffers: TlsBuffers::default(),
        };
        link::run(&link, &mut dial, &|| {
            Duration::from_micros(start.elapsed().as_micros())
        })
        .await
    }

    /// Run shutdown hooks and flush state changes.
    pub async fn shutdown(&self) {
        self.executor().teardown().await;
    }
}

/// Websockets over the board's TCP (and TLS), into one set of TLS buffers:
/// one connection at a time.
struct Board<'n, T, D> {
    tcp: &'n T,
    dns: &'n D,
    verify: &'n Verify,
    buffers: TlsBuffers,
}

impl<'n, T: TcpConnect, D: Dns> Dial for Board<'n, T, D> {
    type Socket<'a>
        = WebSocket<Transport<'a, T::Connection<'n>>>
    where
        Self: 'a;

    async fn dial<'a>(&'a mut self, url: &str) -> Result<Self::Socket<'a>, String> {
        let (secure, host, port, path) =
            crate::http::split_url(url).ok_or_else(|| format!("not a websocket url: {url}"))?;
        let transport = crate::conn::open(
            self.tcp,
            self.dns,
            secure,
            &host,
            port,
            self.verify,
            &mut self.buffers,
        )
        .await?;
        let authority = crate::http::authority(&host, port, secure);
        WebSocket::connect(transport, &authority, &path, arkitekt_spec::rt::fill_random)
            .await
            .map_err(|e| format!("{e}"))
    }
}

impl<S: Read + Write> Socket for WebSocket<S> {
    async fn next(&mut self) -> Result<link::Incoming, String> {
        Ok(
            match WebSocket::next(self).await.map_err(|e| format!("{e}"))? {
                Incoming::Text(text) => link::Incoming::Text(text),
                Incoming::Ping(payload) => link::Incoming::Ping(payload),
                Incoming::Closed { code, reason } => link::Incoming::Closed { code, reason },
            },
        )
    }

    async fn send_text(&mut self, text: &str) -> Result<(), String> {
        WebSocket::send_text(self, text)
            .await
            .map_err(|e| format!("{e}"))
    }

    async fn pong(&mut self, payload: &[u8]) -> Result<(), String> {
        WebSocket::pong(self, payload)
            .await
            .map_err(|e| format!("{e}"))
    }

    async fn close(&mut self) {
        WebSocket::close(self).await
    }
}
