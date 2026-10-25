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

pub use arkitekt_core::{manifest, ConnectOptions, Platform, DEFAULT_ARKITEKT_URL};
#[cfg(feature = "serve")]
pub use arkitekt_serve::{self as serve, serve, serve_with, ServeOptions, Served};
#[cfg(feature = "tokio")]
pub use arkitekt_tokio::{connect, connect_with, device_id, run, run_with, Runtime, Tokio};

pub use arkitekt_core as core;
pub use arkitekt_spec as spec;
#[cfg(feature = "tokio")]
pub use arkitekt_tokio as tokio_platform;
#[cfg(all(feature = "web", target_arch = "wasm32", target_os = "unknown"))]
pub use arkitekt_web as web;
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

/// An app for scripts that only call services: `connect(easy("script", "0.1.0").service(..))`.
pub fn easy(identifier: impl Into<String>, version: impl Into<String>) -> App {
    App::new(identifier, version)
}

/// Lets the macros find the spec through arkitekt. Not a public API.
#[doc(hidden)]
pub mod __private {
    pub use arkitekt_spec as spec;
}
