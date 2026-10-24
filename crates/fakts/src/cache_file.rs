//! The fakts cache as a file (with std).

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::cache::{CacheFile, FaktsStore};
use crate::error::Result;
use crate::models::ActiveFakts;
#[cfg(feature = "user-dirs")]
use crate::models::Manifest;

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

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
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

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
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

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
async fn remove(path: &Path) {
    let _ = tokio::fs::remove_file(path).await;
}

#[cfg(unix)]
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
async fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await;
}

#[cfg(not(any(unix, all(target_arch = "wasm32", target_os = "unknown"))))]
async fn set_mode(_path: &Path, _mode: u32) {}

/// No filesystem on wasm: nothing is read, a write is refused (give the
/// builder a [`FaktsStore`] of your own, e.g. on `localStorage`).
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
async fn read(_path: &Path, _key: &str) -> Option<CacheFile> {
    None
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
async fn write(path: &Path, _key: &str, _fakts: &ActiveFakts) -> Result<()> {
    Err(crate::FaktsError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("no filesystem to cache the fakts at {}", path.display()),
    )))
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
async fn remove(_path: &Path) {}
