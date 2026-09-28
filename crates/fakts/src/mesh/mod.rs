//! The mesh: a userspace tailnet node joined to the deployment's mesh
//! (ionscale), exposing a local HTTP proxy that mesh aliases are reached
//! through.
//!
//! No root and no TUN device, and nothing shared with a system tailscale: the
//! node has its own state directory, keyed by the app's identity
//! (`sub`/`organization`/`hub`), so it is joined once with the key from the
//! first token and re-used on every later start.
//!
//! The node is always our own Tailscale-compatible client (the
//! `arkitekt-mesh` crate). It is new and unaudited, and it is tested
//! against tailscale's own Go implementation. Two backends run it:
//!
//! - [`MeshBackend::Sidecar`] (the default): in a child process, the
//!   `arkitekt-meshd` binary. The Python client can run the same binary the
//!   same way (`pip install arkitekt-meshd`).
//! - [`MeshBackend::Native`] (feature `mesh-native`): in this process, with no
//!   extra binary. Only this backend offers the TURN relay and TCP forwards
//!   (feature `mesh-relay`).
//!
//! Either way the app gets the same local HTTP proxy, so everything that
//! reaches mesh aliases works unchanged. Each backend keeps its node in a
//! state directory of its own, so switching joins the mesh again as a new
//! node, which needs a fresh mesh key.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{FaktsError, Result};

#[cfg(feature = "mesh-native")]
mod native;
mod sidecar;

#[cfg(feature = "mesh-native")]
pub use native::NativeNode;
/// One ICE server entry for a WebRTC client: the node's TURN relay
/// ([`Fakts::mesh_turn`](crate::Fakts::mesh_turn)).
#[cfg(feature = "mesh-relay")]
pub use mesh::driver::TurnInfo;
pub use sidecar::Sidecar;

/// What runs the mesh node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MeshBackend {
    /// The `arkitekt-meshd` sidecar (the same node, in a child process).
    #[default]
    Sidecar,
    /// The `arkitekt-mesh` client in this process; needs the `mesh-native` feature.
    Native,
}

impl std::str::FromStr for MeshBackend {
    type Err = FaktsError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "sidecar" | "meshd" => Ok(Self::Sidecar),
            "native" => Ok(Self::Native),
            other => Err(FaktsError::Mesh(format!(
                "unknown mesh backend {other:?} (expected `sidecar` or `native`)"
            ))),
        }
    }
}

/// Where to find `arkitekt-meshd` and keep the node's state.
#[derive(Debug, Clone)]
pub struct MeshOptions {
    /// What runs the node.
    pub backend: MeshBackend,
    /// The `arkitekt-meshd` binary (default: `$ARKITEKT_MESHD`, next to the
    /// app's executable, `<data dir>/arkitekt/bin`, then `PATH`).
    pub meshd: Option<PathBuf>,
    /// Where node state lives (default: `<state dir>/arkitekt/mesh`).
    pub state_root: Option<PathBuf>,
    /// The node's hostname (default: `<app identifier>-<device id>`).
    pub hostname: Option<String>,
    /// How long joining and connecting may take.
    pub timeout: Duration,
}

impl Default for MeshOptions {
    fn default() -> Self {
        Self {
            backend: MeshBackend::default(),
            meshd: None,
            state_root: None,
            hostname: None,
            timeout: Duration::from_secs(90),
        }
    }
}

impl MeshOptions {
    pub(crate) fn state_root(&self) -> PathBuf {
        self.state_root.clone().unwrap_or_else(|| {
            dirs::state_dir()
                .or_else(dirs::data_local_dir)
                .unwrap_or_else(|| PathBuf::from(".arkitekt"))
                .join("arkitekt")
                .join("mesh")
        })
    }

    /// The state directory of the node called `name`; each backend has its own.
    pub(crate) fn node_dir(&self, name: &str) -> PathBuf {
        let suffix = match self.backend {
            MeshBackend::Sidecar => "",
            MeshBackend::Native => "-native",
        };
        self.state_root().join(format!("{name}{suffix}"))
    }
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

/// How to reach the mesh: the coordination server, and the key to join with
/// if the node is not joined yet.
pub(crate) struct Join {
    pub coord_url: Option<String>,
    pub auth_key: Option<String>,
}

#[cfg(feature = "mesh-relay")]
fn no_relay() -> FaktsError {
    FaktsError::Mesh(
        "the arkitekt-meshd sidecar offers no TURN relay or forwards to the app; \
         use the native backend (MeshBackend::Native)"
            .into(),
    )
}

/// The error for a node that is not on the mesh and has no key to join with.
pub(crate) fn needs_login() -> FaktsError {
    FaktsError::Mesh(
        "this node is not on the mesh and no mesh key was granted; \
         authorize the app again (no_cache) and allow mesh access"
            .into(),
    )
}

/// Create the node's state directory, private to this user.
pub(crate) async fn prepare_statedir(statedir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(statedir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(statedir, std::fs::Permissions::from_mode(0o700)).await;
    }
    Ok(())
}

/// A running mesh node; stopped when dropped.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // one per process
pub enum MeshNode {
    Sidecar(Sidecar),
    #[cfg(feature = "mesh-native")]
    Native(NativeNode),
}

impl MeshNode {
    /// The local HTTP proxy into the mesh, e.g. `http://127.0.0.1:41234`.
    pub fn proxy_url(&self) -> &str {
        match self {
            Self::Sidecar(node) => node.proxy_url(),
            #[cfg(feature = "mesh-native")]
            Self::Native(node) => node.proxy_url(),
        }
    }

    pub fn statedir(&self) -> &Path {
        match self {
            Self::Sidecar(node) => node.statedir(),
            #[cfg(feature = "mesh-native")]
            Self::Native(node) => node.statedir(),
        }
    }

    /// The node's TURN relay (started once), as an ICE server.
    #[cfg(feature = "mesh-relay")]
    pub async fn turn(&mut self) -> Result<mesh::driver::TurnInfo> {
        match self {
            Self::Native(node) => node
                .session()
                .turn()
                .await
                .map_err(|e| FaktsError::Mesh(format!("the TURN relay did not start: {e}"))),
            Self::Sidecar(_) => Err(no_relay()),
        }
    }

    /// A local port forwarding TCP to `host:port` on the mesh.
    #[cfg(feature = "mesh-relay")]
    pub async fn forward(&mut self, host: &str, port: u16) -> Result<std::net::SocketAddr> {
        match self {
            Self::Native(node) => {
                node.session().forward(host, port).await.map_err(|e| {
                    FaktsError::Mesh(format!("could not forward to {host}:{port}: {e}"))
                })
            }
            Self::Sidecar(_) => Err(no_relay()),
        }
    }

    /// Whether a node was already joined in `statedir`.
    pub(crate) fn has_state(options: &MeshOptions, statedir: &Path) -> bool {
        match options.backend {
            MeshBackend::Sidecar => Sidecar::has_state(statedir),
            #[cfg(feature = "mesh-native")]
            MeshBackend::Native => NativeNode::has_state(statedir),
            #[cfg(not(feature = "mesh-native"))]
            MeshBackend::Native => false,
        }
    }

    /// Start the node in `statedir` and wait until it is connected.
    pub(crate) async fn start(
        options: &MeshOptions,
        statedir: PathBuf,
        hostname: &str,
        join: Join,
    ) -> Result<Self> {
        match options.backend {
            MeshBackend::Sidecar => Sidecar::start(options, statedir, hostname, join)
                .await
                .map(Self::Sidecar),
            #[cfg(feature = "mesh-native")]
            MeshBackend::Native => NativeNode::start(options, statedir, hostname, join)
                .await
                .map(Self::Native),
            #[cfg(not(feature = "mesh-native"))]
            MeshBackend::Native => Err(FaktsError::Mesh(
                "the native mesh backend needs fakts' `mesh-native` feature".into(),
            )),
        }
    }
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
    fn backends_keep_separate_state() {
        let mut options = MeshOptions {
            state_root: Some("/state".into()),
            ..Default::default()
        };
        assert_eq!(options.node_dir("app-x"), Path::new("/state/app-x"));
        options.backend = MeshBackend::Native;
        assert_eq!(options.node_dir("app-x"), Path::new("/state/app-x-native"));
        assert_eq!(
            "native".parse::<MeshBackend>().unwrap(),
            MeshBackend::Native
        );
        assert!("tsnet".parse::<MeshBackend>().is_err());
    }
}
