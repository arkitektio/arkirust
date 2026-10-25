//! Arkitekt apps on embassy, without std: rekuest's agent, fakts' login and
//! rath's GraphQL over embedded-nal-async TCP (embassy-net's `TcpClient` and
//! `DnsSocket` on a board).
//!
//! - [`Embassy`]: rekuest's runtime on an embassy executor (the board's
//!   clock and RNG come in through it);
//! - [`NetHost`] and [`serve_http`]: fakts' (and rath's) `Host`, HTTP made
//!   by one network task;
//! - [`Agent`]: the rekuest agent over a websocket ([`ws`]).
//!
//! `checks/embassy` runs it on the host against the real stack.
//!
//! `https://` and `wss://` are TLS 1.3 or 1.2 (rustls's unbuffered client,
//! RustCrypto, cancel-safe reads: [`tls`]), the server checked against a CA
//! you give ([`conn::Verify`]).

#![no_std]

extern crate alloc;

pub mod agent;
pub mod conn;
pub mod http;
pub mod http1;
pub mod platform;
pub mod runtime;
pub mod tls;
pub mod ws;

pub use crate::agent::{Agent, AgentError, AgentOptions};
pub use crate::conn::Verify;
pub use crate::http::{serve_http, NetHost};
pub use crate::platform::{Board, Runtime};
pub use crate::runtime::{Embassy, Entropy};
pub use arkitekt_core::{ConnectOptions, Platform};

pub use arkitekt_spec as spec;
pub use arkitekt_spec::{action, App, Context, Registry, State, StateMut, StateRef, Task};

/// Lets the macros find the spec through arkitekt-embassy. Not a public API.
#[doc(hidden)]
pub mod __private {
    pub use arkitekt_spec as spec;
}
