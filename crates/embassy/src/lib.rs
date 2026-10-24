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
//! Plain `http://` and `ws://` only for now, so no hub behind https is
//! reachable yet: reqwless 0.14 brings embedded-tls 0.18, which does not
//! build against a released `der`; embedded-tls 0.19 does, so TLS needs an
//! HTTP/1.1 client of our own over it.

#![no_std]

extern crate alloc;

pub mod agent;
pub mod http;
pub mod runtime;
pub mod ws;

pub use crate::agent::{Agent, AgentError, AgentOptions};
pub use crate::http::{serve_http, NetHost};
pub use crate::runtime::{Embassy, Entropy};
