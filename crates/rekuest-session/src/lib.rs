//! The rekuest agent session, sans-IO.
//!
//! [`Session`] is the agent's side of the `/agi` protocol as a state
//! machine: the `REGISTER` → `INIT` handshake, heartbeats, bounces, kicks
//! and close codes, reconnecting with backoff, holding assignments until the
//! app is activated, and the [`Outbox`] that keeps every numbered frame
//! until a `JOURNAL_ACK` covers it and re-sends it on the next connection.
//!
//! It does no I/O and owns no clock, runtime or randomness: a driver feeds
//! it time (`now`, monotonic since the driver started), frames and
//! connection changes, and takes out frames to send ([`Session::poll_transmit`]),
//! [`Event`]s to act on and the next deadline ([`Session::poll_timeout`]).
//! `rekuest::Agent` is the tokio driver; the same session runs on wasm and
//! on embassy (`checks/wasm`, `checks/embassy`).
//!
//! `no_std` + `alloc`.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

/// What std's prelude would bring.
mod prelude {
    pub use alloc::borrow::ToOwned;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec::Vec;
    pub use alloc::{format, vec};
}

pub mod entry;
pub mod outbox;
pub mod policy;
mod rng;
pub mod session;
#[cfg(test)]
mod session_tests;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use rekuest_protocol as protocol;

pub use crate::entry::{is_terminal_kind, stamp, JournalEntry, Route};
pub use crate::outbox::{Outbox, RETAIN_LIMIT};
pub use crate::policy::ConnectionPolicy;
pub use crate::session::{to_text, Event, Session, SessionConfig, SessionError};
