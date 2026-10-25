//! The action language: what an action is ([`Definition`], its [`Port`]s),
//! how an agent implements it ([`Implementation`]) and what an agent
//! registers ([`AgentDeclaration`]). The same types on both sides of the
//! wire: an agent sends them, a server validates them.

use serde::{Deserialize, Serialize};

pub mod definition;
pub mod ports;

pub use definition::{ActionKind, AgentDeclaration, Definition, Implementation};
pub use ports::{widgets, Choice, Port, PortKind};

/// How loud a task's log line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LogLevel {
    Debug,
    #[default]
    Info,
    Warn,
    Error,
    Critical,
}
