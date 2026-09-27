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
//!     let app = App::new("hello", "0.1.0").action(greet);
//!     run(app).await
//! }
//! ```
//!
//! The pieces mirror the Python libraries:
//!
//! | Rust              | Python                         |
//! |-------------------|--------------------------------|
//! | [`fakts`]         | `fakts` (configuration + auth) |
//! | [`rath`]          | `rath` (GraphQL client)        |
//! | [`rekuest`]       | `rekuest` (actions + agent)    |
//! | [`App`]           | `arkitekt.App`                 |
//! | [`Service`]       | `@registry.service` builders   |
//! | [`Runtime`]       | `connect(app)` / `run(app)`    |

mod app;
mod runtime;
#[cfg(feature = "serve")]
pub mod serve;
mod service;

pub use crate::app::{connect, easy, run, App};
pub use crate::runtime::{device_id, ConnectOptions, Runtime, DEFAULT_ARKITEKT_URL};
pub use crate::service::Service;
#[cfg(feature = "serve")]
pub use crate::serve::{serve, serve_with, ServeOptions, Served};

pub use fakts;
pub use rath;
pub use rekuest;

pub use async_trait::async_trait;
pub use fakts::{Alias, Fakts, Requirement};
pub use rekuest_macros::service;
pub use rekuest::{
    action, widgets, Background, Context, ContextBuilder, LogLevel, PortType, Shutdown, Startup, State, StateMut,
    StateRef, StateType, Structure, Task,
};

/// Re-exports for generated code. Not a public API.
#[doc(hidden)]
pub mod __private {
    pub use crate::service::Service;
    pub use anyhow;
    pub use async_trait::async_trait;
    pub use fakts;
    pub use rekuest;
}
