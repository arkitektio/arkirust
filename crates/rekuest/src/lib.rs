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

#![cfg_attr(not(feature = "std"), no_std)]

// Lets `#[action]` expand to `::rekuest::…` inside this crate's own tests.
extern crate self as rekuest;

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
pub(crate) mod sync {
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

    #[cfg(feature = "std")]
    pub struct RwLock<T: ?Sized>(std::sync::RwLock<T>);

    #[cfg(feature = "std")]
    impl<T> RwLock<T> {
        pub const fn new(value: T) -> Self {
            Self(std::sync::RwLock::new(value))
        }
    }

    #[cfg(feature = "std")]
    impl<T: ?Sized> RwLock<T> {
        pub fn read(&self) -> std::sync::RwLockReadGuard<'_, T> {
            self.0
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        pub fn write(&self) -> std::sync::RwLockWriteGuard<'_, T> {
            self.0
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }

    #[cfg(not(feature = "std"))]
    pub use spin::{Mutex, RwLock};
}

/// Hash maps: std's with std, hashbrown's without.
#[allow(unused_imports)]
pub(crate) mod collections {
    #[cfg(not(feature = "std"))]
    pub use hashbrown::{HashMap, HashSet};
    #[cfg(feature = "std")]
    pub use std::collections::{HashMap, HashSet};

    /// Insertion-ordered maps: indexmap's, hashed by hashbrown's hasher
    /// without std (indexmap's default hasher needs std).
    #[cfg(feature = "std")]
    pub use indexmap::IndexMap;
    #[cfg(not(feature = "std"))]
    pub type IndexMap<K, V> = indexmap::IndexMap<K, V, hashbrown::DefaultHashBuilder>;
}

mod action;
#[cfg(feature = "tokio")]
pub mod agent;
#[cfg(any(feature = "tokio", feature = "web"))]
mod agent_error;
mod context;
pub mod driver;
mod emit;
pub mod executor;
mod hooks;
pub mod journal;
mod locks;
mod patch;
mod port_type;
pub mod rt;
#[cfg(feature = "serve")]
pub mod serve;
pub mod shelf;
pub mod state;
#[cfg(feature = "wal")]
pub mod store;
mod task;
#[cfg(feature = "tokio")]
pub mod transport;
#[cfg(all(feature = "web", target_arch = "wasm32"))]
pub mod web;

// The wire types live in `rekuest-protocol` (shared with servers); these paths stay.
pub use rekuest_protocol::{definition, messages, ports};

pub use crate::action::{Action, ActionError, Concurrency, Registry};
#[cfg(feature = "tokio")]
pub use crate::agent::{Agent, AgentOptions};
#[cfg(any(feature = "tokio", feature = "web"))]
pub use crate::agent_error::AgentError;
pub use crate::context::{Context, ContextBuilder, MissingClient};
pub use crate::definition::{ActionKind, AgentDeclaration, Definition, Implementation};
pub use crate::driver::Driver;
pub use crate::emit::{Emitter, NullEmitter};
pub use crate::executor::{Executor, StateView, TaskView};
pub use crate::hooks::{Background, Shutdown, Startup};
pub use crate::journal::{Journal, JournalEntry, TaskGate};
pub use crate::locks::{LockTable, LockView};
pub use crate::messages::LogLevel;
pub use crate::port_type::{unwrap_reference, PortError, PortType, Structure};
pub use crate::ports::{widgets, Choice, Port, PortKind};
pub use crate::shelf::{Memory, MemoryStructure, Shelf};
pub use crate::state::{Mutation, Sink, StateError, StateHub, StateMut, StateRef, StateType};
pub use crate::task::Task;
#[cfg(feature = "tokio")]
pub use crate::transport::{Connector, Direct, HttpProxy};
pub use rekuest_macros::{action, State};
pub use rekuest_session::ConnectionPolicy;

/// Everything `#[action]`-generated code refers to. Not a public API.
#[doc(hidden)]
pub mod __private {
    pub use futures;
    pub use serde_json;
    #[cfg(feature = "tokio")]
    pub use tokio;

    pub use crate::rt;
    // So the expansion builds in a crate without std.
    pub use alloc::boxed::Box;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec::Vec;
    pub use alloc::{format, vec};

    pub use crate::action::{Action, ActionError, Concurrency};
    pub use crate::context::Context;
    pub use crate::definition::{ActionKind, Definition};
    pub use crate::port_type::PortType;
    pub use crate::ports::Port;
    pub use crate::state::{StateMut, StateRef, StateType};
    pub use crate::task::Task;
}
