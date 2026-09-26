//! Rekuest: expose typed Rust functions as Arkitekt actions.
//!
//! Write a plain async function, annotate it with [`action`], and hand it to
//! an [`Agent`] (or, more commonly, to an `arkitekt::App`). The function's
//! parameters and return type become the action's ports through
//! [`PortType`]; service clients and the running [`Task`] are injected.
//!
//! ```ignore
//! /// Greet someone
//! ///
//! /// # Arguments
//! /// * `name` - Who to greet
//! #[rekuest::action]
//! async fn greet(name: String, #[port(default = 1)] times: i64) -> String {
//!     name.repeat(times as usize)
//! }
//! ```

// Lets `#[action]` expand to `::rekuest::…` inside this crate's own tests.
extern crate self as rekuest;

mod action;
pub mod agent;
mod context;
mod definition;
pub mod messages;
mod outbox;
mod port_type;
mod ports;
mod task;

pub use crate::action::{Action, ActionError, Registry};
pub use crate::agent::{Agent, AgentError, AgentOptions, ConnectionPolicy};
pub use crate::context::{Context, ContextBuilder, MissingClient};
pub use crate::definition::{ActionKind, AgentDeclaration, Definition, Implementation};
pub use crate::messages::LogLevel;
pub use crate::port_type::{unwrap_reference, PortError, PortType, Structure};
pub use crate::ports::{widgets, Choice, Port, PortKind};
pub use crate::task::Task;
pub use rekuest_macros::action;

/// Everything `#[action]`-generated code refers to. Not a public API.
#[doc(hidden)]
pub mod __private {
    pub use futures;
    pub use serde_json;
    pub use tokio;

    pub use crate::action::{Action, ActionError};
    pub use crate::context::Context;
    pub use crate::definition::{ActionKind, Definition};
    pub use crate::port_type::PortType;
    pub use crate::ports::Port;
    pub use crate::task::Task;
}
