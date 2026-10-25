//! What an Arkitekt app is, shared by everything that declares, runs or
//! serves one. The Rust twin of Python's `arkitekt-spec`.
//!
//! - [`actions`]: the action language: definitions, ports, implementations
//!   and the declaration an agent registers. Wire types, also what a server
//!   needs (without the `declare` feature, that is all there is).
//! - [`manifest`]: who an app is and which services it requires.
//! - [`wiring`]: an [`Alias`](wiring::Alias) (one way to reach a service
//!   instance) and the [`Wiring`](wiring::Wiring) a service is built from.
//!
//! With `declare` (default), the declaration layer:
//!
//! - [`App`]: identity, services and offerings, doing no I/O;
//! - [`#[action]`](action), [`Registry`], states ([`StateMut`],
//!   [`StateRef`]), hooks ([`Startup`], [`Background`]), [`PortType`] and
//!   [`Structure`];
//! - [`Task`]: what a running action reports through, implemented by a
//!   runtime ([`TaskBackend`]);
//! - [`Service`]: a client an app uses, e.g. mikro;
//! - [`rt`]: the async runtime interface (spawn, sleep, clock, randomness)
//!   a platform installs.
//!
//! A runtime runs it (`rekuest`: the executor and journal), a provider
//! serves it (a rekuest agent, or served over HTTP), a platform carries it
//! (tokio, the browser, embassy).

#![cfg_attr(not(feature = "std"), no_std)]

// Lets `#[action]` expand to `::arkitekt_spec::…` inside this crate's own tests.
extern crate self as arkitekt_spec;

extern crate alloc;

/// What std's prelude would bring, for the build without std.
#[allow(unused_imports)]
pub(crate) mod prelude {
    pub use alloc::borrow::ToOwned;
    pub use alloc::boxed::Box;
    pub use alloc::string::{String, ToString};
    pub use alloc::sync::Arc;
    pub use alloc::vec::Vec;
    pub use alloc::{format, vec};
}

/// Locks that do not poison: std's with std, spinlocks without (an embassy
/// executor runs its tasks on one core; critical sections here are short).
#[cfg(feature = "declare")]
#[doc(hidden)]
pub mod sync {
    #[cfg(feature = "std")]
    pub struct Mutex<T: ?Sized>(std::sync::Mutex<T>);

    #[cfg(feature = "std")]
    impl<T> Mutex<T> {
        pub const fn new(value: T) -> Self {
            Self(std::sync::Mutex::new(value))
        }
    }

    #[cfg(feature = "std")]
    impl<T: ?Sized> Mutex<T> {
        pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }

    #[cfg(feature = "std")]
    impl<T: Default> Default for Mutex<T> {
        fn default() -> Self {
            Self::new(T::default())
        }
    }

    #[cfg(not(feature = "std"))]
    pub use spin::Mutex;
}

/// Hash maps: std's with std, hashbrown's without.
#[cfg(feature = "declare")]
pub(crate) mod collections {
    #[cfg(not(feature = "std"))]
    pub use hashbrown::HashMap;
    #[cfg(feature = "std")]
    pub use std::collections::HashMap;
}

pub mod actions;
pub mod manifest;
pub mod wiring;

#[cfg(feature = "declare")]
mod action;
#[cfg(feature = "declare")]
mod app;
#[cfg(feature = "declare")]
mod context;
#[cfg(feature = "declare")]
mod hooks;
#[cfg(feature = "declare")]
#[doc(hidden)]
pub mod patch;
#[cfg(feature = "declare")]
mod port_type;
#[cfg(feature = "declare")]
pub mod rt;
#[cfg(feature = "declare")]
mod service;
#[cfg(feature = "declare")]
pub mod state;
#[cfg(feature = "declare")]
pub mod task;

pub use crate::actions::{
    widgets, ActionKind, AgentDeclaration, Choice, Definition, Implementation, LogLevel, Port,
    PortKind,
};
pub use crate::manifest::{Manifest, PublicSource, Requirement};
pub use crate::wiring::Alias;

#[cfg(feature = "declare")]
pub use crate::{
    action::{Action, ActionError, Concurrency, Registry},
    app::{rekuest_requirement, App},
    context::{Context, ContextBuilder, MissingClient},
    hooks::{Background, Hooks, Shutdown, Startup},
    port_type::{unwrap_reference, PortError, PortType, Structure},
    service::Service,
    state::{StateCells, StateError, StateMut, StateRef, StateType},
    task::{Task, TaskBackend, TaskGate},
    wiring::Wiring,
};
#[cfg(feature = "declare")]
pub use arkitekt_macros::{action, service, State};

/// Everything macro-generated code refers to. Not a public API.
#[cfg(feature = "declare")]
#[doc(hidden)]
pub mod __private {
    pub use anyhow;
    pub use async_trait::async_trait;
    pub use futures;
    pub use serde_json;

    pub use crate::rt;
    // So the expansion builds in a crate without std.
    pub use alloc::boxed::Box;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec::Vec;
    pub use alloc::{format, vec};
    pub use core::option::Option;
    pub use core::result::Result;

    pub use crate::action::{Action, ActionError, Concurrency};
    pub use crate::actions::{ActionKind, Definition, Port};
    pub use crate::context::{Context, ContextBuilder};
    pub use crate::manifest::Requirement;
    pub use crate::port_type::PortType;
    pub use crate::service::Service;
    pub use crate::state::{StateMut, StateRef, StateType};
    pub use crate::task::Task;
    pub use crate::wiring::{Alias, Wiring};
}
