//! Where the active fakts are kept between runs, so an app authorizes once
//! per machine: a file by default ([`FileStore`]), or whatever a target has
//! instead (e.g. a firmware's NVS) through [`FaktsStore`].

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::models::{ActiveFakts, Manifest};

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

/// A JSON file, readable by its owner only.
#[derive(Debug, Clone)]
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The per-user default for this app and server (see [`default_cache_path`]).
    #[cfg(feature = "user-dirs")]
    pub fn default_for(manifest: &Manifest, url: &str) -> Self {
        Self::new(default_cache_path(manifest, url))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl FaktsStore for FileStore {
    async fn load(&self, key: &str) -> Option<ActiveFakts> {
        read(&self.path, key).await.map(|file| file.fakts)
    }

    async fn save(&self, key: &str, fakts: &ActiveFakts) -> Result<()> {
        write(&self.path, key, fakts).await
    }

    async fn clear(&self) {
        remove(&self.path).await;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheFile {
    pub fakts: ActiveFakts,
    /// Unix timestamp of the write.
    pub created: i64,
    /// `manifest.hash() + url`; a mismatch invalidates the cache.
    pub hash: String,
}

/// Default cache location:
/// `{state_dir}/arkitekt/cache/{identifier}-{version}-{sha(url)[:6]}_fakts_cache.rs.json`
///
/// The `.rs` keeps it apart from the Python client's cache in the same
/// directory: the two formats differ, and sharing a file would make each
/// client discard (and so re-authorize over) the other's rotating credential.
#[cfg(feature = "user-dirs")]
pub fn default_cache_path(manifest: &Manifest, url: &str) -> PathBuf {
    use sha2::{Digest, Sha256};

    let base = dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from(".arkitekt"));
    let url_hash = hex::encode(Sha256::digest(url.as_bytes()));
    base.join("arkitekt").join("cache").join(format!(
        "{}-{}-{}_fakts_cache.rs.json",
        manifest.identifier,
        manifest.version,
        &url_hash[..6]
    ))
}

pub fn cache_key(manifest: &Manifest, url: &str) -> String {
    format!("{}{}", manifest.hash(), url)
}

/// Read the cache, returning `None` when it is missing, unreadable or stale.
async fn read(path: &Path, key: &str) -> Option<CacheFile> {
    let raw = tokio::fs::read(path).await.ok()?;
    let file: CacheFile = match serde_json::from_slice(&raw) {
        Ok(file) => file,
        Err(e) => {
            tracing::warn!("ignoring unreadable fakts cache {}: {e}", path.display());
            return None;
        }
    };
    (file.hash == key).then_some(file)
}

/// Write the cache with owner-only permissions (0600 file, 0700 directory).
async fn write(path: &Path, key: &str, fakts: &ActiveFakts) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
        set_mode(parent, 0o700).await;
    }
    let file = CacheFile {
        fakts: fakts.clone(),
        created: chrono::Utc::now().timestamp(),
        hash: key.to_owned(),
    };
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, serde_json::to_vec_pretty(&file)?).await?;
    set_mode(&tmp, 0o600).await;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

async fn remove(path: &Path) {
    let _ = tokio::fs::remove_file(path).await;
}

#[cfg(unix)]
async fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await;
}

#[cfg(not(unix))]
async fn set_mode(_path: &Path, _mode: u32) {}
