//! Fakts: configuration discovery for Arkitekt apps (protocol v2).
//!
//! An app describes itself with a [`Manifest`] (who it is, which services it
//! needs). [`Fakts::builder`] discovers the server via
//! `/.well-known/fakts`, authorizes the app once through the OAuth2 device
//! code flow, and caches the result. The loaded [`Fakts`] then hands out
//! access tokens (refreshing rotating refresh tokens as needed) and resolves
//! each requirement key to a reachable [`Alias`].
//!
//! ```no_run
//! # async fn demo() -> fakts::Result<()> {
//! use fakts::{Fakts, Manifest, Requirement, TokenLoader};
//!
//! let mut manifest = Manifest::new("my-app", "0.1.0");
//! manifest.requirements.push(Requirement::new("mikro", "live.arkitekt.mikro"));
//!
//! let fakts = Fakts::builder("http://127.0.0.1", manifest).load().await?;
//! let alias = fakts.get_alias("mikro").await?;
//! let token = fakts.get_token().await?;
//! println!("{} {}", alias.to_http_path("graphql"), token);
//! # Ok(()) }
//! ```

pub mod cache;
mod error;
mod fakts;
pub mod grants;
#[cfg(feature = "mesh")]
pub mod mesh;
mod models;

pub use crate::error::{FaktsError, Result};
pub use crate::fakts::{Fakts, FaktsBuilder, Grant, TokenLoader};
pub use crate::grants::{ClientKind, ClientRole, DeviceCodeHook, DeviceCodeOptions};
#[cfg(feature = "mesh")]
pub use crate::mesh::{MeshBackend, MeshOptions};
pub use crate::models::*;

/// Select the process-wide TLS provider (ring), once.
///
/// Different dependencies enable different rustls providers (`ring` via
/// reqwest and tungstenite, `aws-lc-rs` via object_store). With both enabled,
/// rustls cannot pick one on its own and panics on the first TLS connection,
/// so every entry point that opens connections calls this first.
pub fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Fails only if the application already installed one, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}
