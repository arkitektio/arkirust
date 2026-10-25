//! Arkitekt for Rust.
//!
//! Declare an [`App`], compose the services it uses, offer actions, and run it:
//!
//! ```ignore
//! use arkitekt::{action, run, App};
//!
//! /// Greet someone
//! ///
//! /// # Arguments
//! /// * `name` - Who to greet
//! #[action]
//! async fn greet(name: String) -> String {
//!     format!("Hello {name}")
//! }
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     run(App::new("hello", "0.1.0").action(greet)).await
//! }
//! ```
//!
//! The pieces mirror the Python libraries:
//!
//! | Rust              | Python                                      |
//! |-------------------|---------------------------------------------|
//! | [`spec`]          | `arkitekt-spec` (the action language, `App`) |
//! | [`rekuest`]       | `arkitekt-runtime` + `rekuest` (the agent)  |
//! | [`fakts`]         | `fakts` (configuration + auth)              |
//! | [`rath`]          | `rath` (GraphQL client)                     |
//! | [`App`]           | `arkitekt.App`                              |
//! | [`Service`]       | `@registry.service` builders                |
//! | [`Runtime`]       | `connect(app)` / `run(app)`                 |

mod runtime;
#[cfg(feature = "serve")]
pub mod serve;

pub use crate::runtime::{
    connect, connect_with, device_id, easy, run, run_with, ConnectOptions, Runtime,
    DEFAULT_ARKITEKT_URL,
};
#[cfg(feature = "serve")]
pub use crate::serve::{serve, serve_with, ServeOptions, Served};

pub use arkitekt_spec as spec;
pub use fakts;
pub use rath;
pub use rekuest;

pub use arkitekt_spec::{
    action, service, widgets, Action, Alias, App, Background, Context, ContextBuilder, LogLevel,
    PortType, Registry, Service, Shutdown, Startup, State, StateMut, StateRef, StateType,
    Structure, Task,
};
pub use async_trait::async_trait;
pub use fakts::{Fakts, Requirement};

/// Lets the macros find the spec through arkitekt. Not a public API.
#[doc(hidden)]
pub mod __private {
    pub use arkitekt_spec as spec;
}
