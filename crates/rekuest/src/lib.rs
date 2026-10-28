//! Rekuest: runs an Arkitekt app's actions, and serves them to a rekuest
//! server.
//!
//! What an app offers is declared with `arkitekt-spec` (`#[action]`,
//! `Registry`, states, hooks). rekuest runs it: the [`Executor`] assigns
//! tasks to actions and holds their locks and states, the [`Journal`]
//! numbers every report, and the [`Driver`] keeps the session with the
//! server. The websocket [`Agent`] (tokio) and `web::Agent` (wasm) drive
//! it; any other platform drives the [`Driver`] itself (arkitekt-embassy).

#![cfg_attr(not(feature = "std"), no_std)]

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

#[cfg(feature = "tokio")]
pub mod agent;
mod agent_error;
pub mod driver;
mod emit;
pub mod executor;
pub mod journal;
pub mod link;
mod locks;
pub mod shelf;
pub mod state;
#[cfg(feature = "wal")]
pub mod store;
pub mod task;
#[cfg(feature = "tokio")]
pub mod transport;
#[cfg(all(feature = "web", target_arch = "wasm32"))]
pub mod web;

// The wire types live in `rekuest-protocol` (shared with servers).
pub use rekuest_protocol::messages;

#[cfg(feature = "tokio")]
pub use crate::agent::{Agent, AgentOptions};
pub use crate::agent_error::AgentError;
pub use crate::driver::Driver;
pub use crate::emit::{Emitter, NullEmitter};
pub use crate::executor::{Executor, StateView, TaskView};
pub use crate::journal::{Journal, JournalEntry};
pub use crate::locks::{LockTable, LockView};
pub use crate::shelf::{Memory, MemoryStructure, Shelf};
pub use crate::state::{Sink, StateHub};
pub use crate::task::AgentTask;
#[cfg(feature = "tokio")]
pub use crate::transport::{Connector, Direct, HttpProxy};
pub use rekuest_session::ConnectionPolicy;

/// What an app served by a rekuest agent requires: the rekuest server.
pub fn requirement() -> arkitekt_spec::Requirement {
    arkitekt_spec::Requirement::new("rekuest", "live.arkitekt.rekuest")
        .description("Where actions are assigned")
}

/// What a rekuest agent has to be allowed to do: the scopes the Python
/// client's `RekuestApi` asks for (read off rekuest's schema).
pub const SCOPES: &[&str] = &[
    "rekuest_agent",
    "rekuest_call",
    "rekuest_read",
    "rekuest_write",
];

/// Lets `#[action]` find the spec through rekuest. Not a public API.
#[doc(hidden)]
pub mod __private {
    pub use arkitekt_spec as spec;
}
