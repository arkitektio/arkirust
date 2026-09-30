//! The rekuest agent protocol: every frame between an agent and the rekuest server.
//!
//! Both sides use these types. An agent emits [`messages::FromAgent`] in an
//! [`messages::Envelope`] and parses [`messages::ToAgent`]; a server does the opposite.
//! `tests/fixtures/agent_wire_examples.json` is generated from the Python server's models
//! and is the contract: every frame in it must parse and serialize back unchanged.

pub mod definition;
pub mod messages;
pub mod ports;
