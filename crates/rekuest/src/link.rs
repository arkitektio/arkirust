//! The agent's link to its server: the one loop every agent runs, whatever
//! its socket.
//!
//! A [`Dial`] opens a websocket ([`Socket`]) to the endpoint: tungstenite on
//! tokio, the JS `WebSocket` in a browser, arkitekt-embassy's own client on a
//! board. [`run`] keeps the session going over it: it connects when the
//! session says so, sends what the [`Driver`] polls, hands it what comes in,
//! and waits for a frame, the session's deadline or a report (whichever is
//! first). A [`Backlog`] keeps the journal beyond memory (the tokio agent's
//! SQLite file); without one, what is unacknowledged lives in memory only.
//!
//! Nothing here is `Send`-bound: a board's and a browser's sockets are not.
//! A link runs in the platform's own task (an embassy task, `spawn_local`,
//! a tokio task when its socket is `Send`).

#[allow(unused_imports)]
use crate::prelude::*;
use core::pin::pin;
use core::time::Duration;

use arkitekt_spec::rt;
use async_trait::async_trait;
use fakts::TokenLoader;
use futures::future::{select, Either};
use rekuest_session::Event;

use crate::agent_error::AgentError;
use crate::driver::Driver;
use crate::journal::JournalEntry;

/// What came in on a [`Socket`].
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Text(String),
    /// The server pinged; answered with [`Socket::pong`]. A socket whose
    /// library answers pings itself never yields this.
    Ping(Vec<u8>),
    /// The server closed the connection (1006: it just ended).
    Closed {
        code: u16,
        reason: String,
    },
}

/// An open websocket to the agent endpoint.
// No `Send` bound on the futures (board and browser sockets are not `Send`);
// a concrete socket's futures stay `Send` when it is.
#[allow(async_fn_in_trait)]
pub trait Socket {
    /// The next text frame, ping or close.
    ///
    /// Must be cancel-safe: [`run`] races it against the session's deadline
    /// and new reports, and drops it when they win. Dropping it must lose no
    /// frame (keep partial frames in the socket, not in the future).
    async fn next(&mut self) -> Result<Incoming, String>;

    async fn send_text(&mut self, text: &str) -> Result<(), String>;

    async fn pong(&mut self, payload: &[u8]) -> Result<(), String>;

    /// Send a close frame (normal closure).
    async fn close(&mut self);
}

/// Opens [`Socket`]s to the agent endpoint.
#[allow(async_fn_in_trait)]
pub trait Dial {
    type Socket<'a>: Socket
    where
        Self: 'a;

    /// Connect to `url` (`ws://` or `wss://`).
    async fn dial<'a>(&'a mut self, url: &str) -> Result<Self::Socket<'a>, String>;
}

/// Where the journal is kept beyond memory, so that what the server has not
/// acknowledged survives (the tokio agent's SQLite file).
#[async_trait]
pub trait Backlog: Send + Sync {
    /// What earlier runs never got acknowledged (all but `session_id`'s).
    async fn earlier(&self, session_id: &str) -> Vec<JournalEntry>;

    /// Every unacknowledged entry: the session dropped some from memory
    /// (flush the journal first: entries still being written count).
    async fn unacked(&self) -> Option<Vec<JournalEntry>>;

    /// The server acknowledged `session` up to `pos`.
    async fn acked(&self, session: String, pos: u64);
}

/// What a link connects as.
pub struct Link<'a> {
    pub driver: &'a Driver,
    /// `ws(s)://…/agi`
    pub endpoint: &'a str,
    pub tokens: &'a dyn TokenLoader,
    pub backlog: Option<&'a dyn Backlog>,
    /// Shown in the UI; the server falls back to the client id.
    pub name: Option<String>,
    pub description: Option<String>,
}

/// Keep `link`'s session up over sockets `dial` opens, until it ends for
/// good (refused, kicked, out of retries, …). `now` is a monotonic clock.
pub async fn run<D: Dial, N: Fn() -> Duration>(
    link: &Link<'_>,
    dial: &mut D,
    now: &N,
) -> Result<(), AgentError> {
    let driver = link.driver;
    if let Some(backlog) = link.backlog {
        let session_id = driver.with_session(|session| session.session_id().to_owned());
        let entries = backlog.earlier(&session_id).await;
        if !entries.is_empty() {
            tracing::info!(
                "re-sending {} journal entries of earlier sessions",
                entries.len()
            );
            driver.with_session(|session| session.reloaded(Some(entries)));
        }
    }
    // A run after an earlier one ended (or stopped while connecting) starts over.
    driver.with_session(|session| session.restart(now()));

    // Disconnected: wait for the next attempt, then connect.
    loop {
        let polled = driver.poll(now());
        let mut connect = false;
        for event in polled.events {
            match driver.act(event) {
                Some(Event::Connect) => connect = true,
                Some(other) => handle(link, other).await?,
                None => {}
            }
        }
        if connect {
            let token = link.tokens.get_token().await?;
            match dial.dial(link.endpoint).await {
                Ok(socket) => {
                    let declaration =
                        driver.declaration(link.name.clone(), link.description.clone());
                    driver.with_session(|session| session.connected(now(), token, declaration));
                    serve(link, socket, now).await?;
                }
                Err(e) => driver.with_session(|session| {
                    session.connect_failed(now(), &format!("connect to {}: {e}", link.endpoint))
                }),
            }
            continue;
        }
        quiet(driver, polled.deadline, now).await;
    }
}

/// One connection, until the session is done with it.
async fn serve<S: Socket, N: Fn() -> Duration>(
    link: &Link<'_>,
    mut socket: S,
    now: &N,
) -> Result<(), AgentError> {
    let driver = link.driver;
    loop {
        let polled = driver.poll(now());
        let busy = !polled.events.is_empty() || !polled.frames.is_empty();
        for frame in &polled.frames {
            if let Err(e) = socket.send_text(frame).await {
                driver.with_session(|session| {
                    session.handle_disconnect(now(), &format!("send: {e}"))
                });
                break;
            }
        }
        let mut closed = false;
        for event in polled.events {
            match driver.act(event) {
                Some(Event::Close) => closed = true,
                Some(Event::Ended(error)) => {
                    socket.close().await;
                    return Err(error.into());
                }
                Some(other) => handle(link, other).await?,
                None => {}
            }
        }
        if closed {
            socket.close().await;
            return Ok(());
        }
        if busy {
            // Acting may have queued more: REGISTER, reports, the end.
            continue;
        }

        let incoming = {
            let next = pin!(socket.next());
            let quiet = pin!(quiet(driver, polled.deadline, now));
            match select(next, quiet).await {
                Either::Left((incoming, _)) => Some(incoming),
                Either::Right(_) => None,
            }
        };
        let ping = match incoming {
            Some(Ok(Incoming::Text(text))) => {
                driver.with_session(|session| session.handle_text(now(), &text));
                None
            }
            Some(Ok(Incoming::Ping(payload))) => Some(payload),
            Some(Ok(Incoming::Closed { code, reason })) => {
                driver.with_session(|session| session.handle_close(now(), code, &reason));
                None
            }
            Some(Err(e)) => {
                driver.with_session(|session| {
                    session.handle_disconnect(now(), &format!("read: {e}"))
                });
                None
            }
            None => None,
        };
        if let Some(payload) = ping {
            let _ = socket.pong(&payload).await;
        }
    }
}

/// The events a link handles in either state (the driver acted on the rest).
async fn handle(link: &Link<'_>, event: Event) -> Result<(), AgentError> {
    match event {
        Event::ReloadJournal => {
            let entries = match link.backlog {
                Some(backlog) => backlog.unacked().await,
                // Nothing was dropped from memory: there is no journal file.
                None => None,
            };
            link.driver
                .with_session(|session| session.reloaded(entries));
        }
        Event::Acked { session, pos } => {
            if let Some(backlog) = link.backlog {
                backlog.acked(session, pos).await;
            }
        }
        Event::Ended(error) => return Err(error.into()),
        _ => {}
    }
    Ok(())
}

/// Until the session's deadline or a new report, whichever is first.
async fn quiet<N: Fn() -> Duration>(driver: &Driver, deadline: Option<Duration>, now: &N) {
    let timer = pin!(async {
        match deadline {
            Some(at) => rt::sleep(at.saturating_sub(now())).await,
            None => core::future::pending().await,
        }
    });
    let woken = pin!(driver.woken());
    select(timer, woken).await;
}
