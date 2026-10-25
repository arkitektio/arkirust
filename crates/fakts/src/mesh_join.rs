//! Joining the mesh with a node the app runs itself
//! ([`FaktsBuilder::mesh_joiner`](crate::FaktsBuilder::mesh_joiner)), e.g. a
//! firmware, which has neither the `arkitekt-meshd` sidecar nor a state
//! directory for the native backend.

use crate::error::Result;
use crate::models::Manifest;
use crate::prelude::*;

/// What fakts knows about joining, once the app is logged in.
pub struct MeshJoin {
    /// The coordination server: the one the mesh key was issued for, else the
    /// deployment's.
    pub coord_url: Option<String>,
    /// The key lok granted for this login, if any. A node that already joined
    /// does not need it; one that never did cannot join without it.
    pub auth_key: Option<String>,
    /// The node's hostname: `<app identifier>-<device id>`, as a DNS label.
    pub hostname: String,
}

impl core::fmt::Debug for MeshJoin {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MeshJoin")
            .field("coord_url", &self.coord_url)
            .field("auth_key", &self.auth_key.as_ref().map(|_| "***"))
            .field("hostname", &self.hostname)
            .finish()
    }
}

/// How mesh aliases are reached, once a [`MeshJoiner`] joined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshRoute {
    /// Through this local HTTP proxy into the mesh
    /// (`http://127.0.0.1:<port>`): a sidecar's or a native node's.
    Proxy(String),
    /// The platform reaches mesh addresses itself: its TCP sends tailnet
    /// addresses and names over the mesh (on embassy, arkitekt-embassy's
    /// `route::Routed`). Mesh aliases go through the ordinary [`Host`],
    /// without a proxy.
    ///
    /// [`Host`]: crate::Host
    Direct,
}

/// Runs the app's mesh node. With one set, fakts asks for a mesh key at login
/// and reaches mesh aliases the way the joiner says.
#[async_trait::async_trait]
pub trait MeshJoiner: Send + Sync {
    /// Join (or rejoin) the mesh and say how it is reached, or `None` to
    /// stay off the mesh, e.g. with no key and no node that joined before.
    /// Mesh aliases are then skipped.
    async fn join(&self, join: MeshJoin) -> Result<Option<MeshRoute>>;
}

/// A DNS label: lowercase alphanumerics and dashes, at most 63 characters.
pub(crate) fn hostname_label(raw: &str) -> String {
    let mut label: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    label.truncate(63);
    let label = label.trim_matches('-').to_owned();
    if label.is_empty() {
        "arkitekt-app".into()
    } else {
        label
    }
}

/// `<app identifier>-<first 8 alphanumerics of the device id>`.
pub(crate) fn default_hostname(manifest: &Manifest) -> String {
    let device: String = manifest
        .device_id
        .as_deref()
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    hostname_label(&format!("{}-{device}", manifest.identifier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostnames_are_dns_labels() {
        assert_eq!(hostname_label("My App_v2"), "my-app-v2");
        assert_eq!(hostname_label("--x--"), "x");
        assert_eq!(hostname_label("!!!"), "arkitekt-app");
        assert_eq!(hostname_label(&"a".repeat(80)).len(), 63);
    }

    #[test]
    fn the_hostname_carries_the_device() {
        let mut manifest = Manifest::new("live.arkitekt.esp32", "2");
        manifest.device_id = Some("a1b2c3d4-e5f6-5a0b".into());
        assert_eq!(default_hostname(&manifest), "live-arkitekt-esp32-a1b2c3d4");
    }
}
