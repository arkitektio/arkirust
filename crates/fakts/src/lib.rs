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

/// Hash maps: std's with std, hashbrown's without.
pub(crate) mod collections {
    #[cfg(not(feature = "std"))]
    pub use hashbrown::HashMap;
    #[cfg(feature = "std")]
    pub use std::collections::HashMap;
}

pub mod cache;
#[cfg(feature = "std")]
mod cache_file;
mod error;
mod fakts;
pub mod grants;
pub mod host;
#[cfg(feature = "std")]
pub mod http;
#[cfg(feature = "mesh")]
pub mod mesh;
pub mod mesh_join;
mod models;

pub use crate::cache::FaktsStore;
#[cfg(feature = "std")]
pub use crate::cache::FileStore;
pub use crate::error::{FaktsError, Result};
pub use crate::fakts::{Fakts, FaktsBuilder, Grant, TokenLoader};
pub use crate::grants::{
    ClientKind, ClientRole, DeviceCodeHook, DeviceCodeOptions, MandateKeyOptions,
};
pub use crate::host::Host;
#[cfg(feature = "std")]
pub use crate::http::AliasExt;
#[cfg(feature = "mesh")]
pub use crate::mesh::{MeshBackend, MeshOptions};
pub use crate::mesh_join::{MeshJoin, MeshJoiner, MeshRoute};
pub use crate::models::*;

/// Select the process-wide TLS provider (ring), once.
///
/// Different dependencies enable different rustls providers (`ring` via the
/// `ring` feature, `aws-lc-rs` via object_store). With both enabled, rustls
/// cannot pick one on its own and panics on the first TLS connection, so
/// every entry point that opens connections calls this first.
///
/// Without the `ring` feature this does nothing: the application installs a
/// provider itself (`CryptoProvider::install_default`) before connecting.
pub fn install_crypto_provider() {
    // On wasm the browser (fetch) does TLS: there is no provider to pick.
    #[cfg(all(
        feature = "ring",
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            // Fails only if the application already installed one, which is fine.
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }
}
