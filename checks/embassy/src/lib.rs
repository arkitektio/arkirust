//! A minimal embassy driver for [`rekuest_session::Session`].
//!
//! The socket is a channel from a [`FakeServer`] in the same task; timers
//! are embassy-time's. A real one would put embassy-net and a websocket
//! where the channel is: the session does not change.

#![no_std]

extern crate alloc;

pub mod agent;

use alloc::string::String;
use alloc::vec;
use core::time::Duration;

use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Instant, Timer};
use rekuest_session::protocol::definition::AgentDeclaration;
use rekuest_session::testing::{Echo, FakeServer, ServerOutput};
use rekuest_session::{to_text, ConnectionPolicy, Event, Session, SessionConfig, SessionError};

/// What the fake socket hands the agent.
enum Incoming {
    Frame(String),
    Dropped,
}

/// Run the scripted scenario; the server, to [`FakeServer::check`].
pub async fn run(seed: u64) -> Result<FakeServer, SessionError> {
    let start = Instant::now();
    let now = || Duration::from_micros(start.elapsed().as_micros());
    let socket: Channel<NoopRawMutex, Incoming, 16> = Channel::new();
    let mut server = FakeServer::scripted();
    let mut session = Session::new(SessionConfig {
        policy: ConnectionPolicy {
            initial: Duration::from_millis(10),
            ..Default::default()
        },
        ..SessionConfig::new("s", seed)
    });
    let mut app = Echo::new("s");
    let mut open = false;

    loop {
        session.handle_timeout(now());
        while let Some(event) = session.poll_event() {
            match event {
                Event::Connect => {
                    server.connected();
                    open = true;
                    session.connected(now(), "token".into(), declaration());
                }
                Event::Close => {
                    open = false;
                    socket.clear();
                }
                Event::Ended(error) => return Err(error),
                other => app.handle(&mut session, &other),
            }
        }
        while let Some(envelope) = session.poll_transmit() {
            if open {
                server.receive(&to_text(&envelope));
            }
        }
        while let Some(output) = server.poll() {
            let incoming = match output {
                ServerOutput::Frame(text) => Incoming::Frame(text),
                ServerOutput::Drop => Incoming::Dropped,
            };
            if socket.try_send(incoming).is_err() {
                panic!("the fake socket is full");
            }
        }
        if server.done() {
            return Ok(server);
        }

        let deadline = match session.poll_timeout() {
            Some(at) => start + embassy_time::Duration::from_micros(at.as_micros() as u64),
            None => Instant::MAX,
        };
        match select(socket.receive(), Timer::at(deadline)).await {
            Either::First(Incoming::Frame(text)) if open => session.handle_text(now(), &text),
            Either::First(Incoming::Frame(_)) => {}
            Either::First(Incoming::Dropped) => {
                open = false;
                session.handle_disconnect(now(), "dropped");
            }
            Either::Second(()) => {}
        }
    }
}

fn declaration() -> AgentDeclaration {
    AgentDeclaration {
        name: Some("embassy-control".into()),
        description: None,
        implementations: vec![],
        states: vec![],
        locks: vec![],
        bloks: vec![],
        hash: None,
    }
}
