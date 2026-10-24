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
//! `https://` and `wss://` are TLS 1.3 (embedded-tls), the server checked
//! against a CA you give ([`conn::Verify`]).

#![no_std]

extern crate alloc;

pub mod agent;
pub mod conn;
pub mod http;
pub mod http1;
pub mod runtime;
pub mod ws;

pub use crate::agent::{Agent, AgentError, AgentOptions};
pub use crate::conn::Verify;
pub use crate::http::{serve_http, NetHost};
pub use crate::runtime::{Embassy, Entropy};
