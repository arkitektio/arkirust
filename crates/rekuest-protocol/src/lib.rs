//! The rekuest agent protocol: every frame between an agent and the rekuest server.
//!
//! Both sides use these types. An agent emits [`messages::FromAgent`] in an
//! [`messages::Envelope`] and parses [`messages::ToAgent`]; a server does the opposite.
//! `tests/fixtures/agent_wire_examples.json` is generated from the Python server's models
//! and is the contract: every frame in it must parse and serialize back unchanged.

#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

/// What std's prelude would bring.
mod prelude {
    pub use alloc::boxed::Box;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec::Vec;
}

pub mod messages;

// The action language is the spec's; these paths stay for servers.
pub use arkitekt_spec::actions::{definition, ports};
