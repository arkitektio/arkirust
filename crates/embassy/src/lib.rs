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
//! Plain `http://` and `ws://` only for now: TLS (every hub behind https)
//! waits for an embedded TLS client that builds (reqwless 0.14's
//! embedded-tls 0.18 does not, against a released `der`).

#![no_std]

extern crate alloc;

pub mod agent;
pub mod http;
pub mod runtime;
pub mod ws;

pub use crate::agent::{Agent, AgentError, AgentOptions};
pub use crate::http::{serve_http, NetHost};
pub use crate::runtime::{Embassy, Entropy};
