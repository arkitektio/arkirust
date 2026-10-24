//! Where the active fakts are kept between runs, so an app authorizes once
//! per machine: a file by default ([`FileStore`]), or whatever a target has
//! instead (e.g. a firmware's NVS) through [`FaktsStore`].

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::models::{ActiveFakts, Manifest};
use crate::prelude::*;

#[cfg(feature = "std")]
pub use crate::cache_file::*;

/// Keeps the active fakts (credential, instances, mesh key) between runs.
///
/// `key` identifies the app and server they were granted for: a store hands
/// back only what it saved under the same key. The fakts hold a live
/// credential, so a store keeps them private to the app.
#[async_trait]
pub trait FaktsStore: Send + Sync {
    /// What was saved under `key`; `None` when nothing (readable) was, or it
    /// was saved under another key.
    async fn load(&self, key: &str) -> Option<ActiveFakts>;
    /// Save `fakts` under `key`, replacing what was there.
    async fn save(&self, key: &str, fakts: &ActiveFakts) -> Result<()>;
    /// Forget what was saved: the server refused its credential.
    async fn clear(&self);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheFile {
    pub fakts: ActiveFakts,
    /// Unix timestamp of the write.
    pub created: i64,
    /// `manifest.hash() + url`; a mismatch invalidates the cache.
    pub hash: String,
}

pub fn cache_key(manifest: &Manifest, url: &str) -> String {
    format!("{}{}", manifest.hash(), url)
}
