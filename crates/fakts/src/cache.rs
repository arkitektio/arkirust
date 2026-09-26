//! On-disk cache of the active fakts, so an app authorizes once per machine.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::Result;
use crate::models::{ActiveFakts, Manifest};

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
pub fn default_cache_path(manifest: &Manifest, url: &str) -> PathBuf {
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
pub async fn read(path: &Path, key: &str) -> Option<CacheFile> {
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
pub async fn write(path: &Path, key: &str, fakts: &ActiveFakts) -> Result<()> {
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

pub async fn remove(path: &Path) {
    let _ = tokio::fs::remove_file(path).await;
}

#[cfg(unix)]
async fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await;
}

#[cfg(not(unix))]
async fn set_mode(_path: &Path, _mode: u32) {}
