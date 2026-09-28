//! The native backend: the mesh node ([`mesh`], our own Tailscale-compatible
//! client) in this process, serving the same local proxy as meshd. The node
//! and its state directory are an [`mesh::driver::Session`], which meshd and
//! the Python bindings run too.

use std::path::{Path, PathBuf};

use mesh::driver::{Session, SessionError, SessionOptions};

use super::{needs_login, Join, MeshOptions};
use crate::error::{FaktsError, Result};

/// A running in-process node; stopped when dropped.
#[derive(Debug)]
pub struct NativeNode {
    session: Session,
    proxy_url: String,
}

impl NativeNode {
    /// The local HTTP proxy into the mesh, e.g. `http://127.0.0.1:41234`.
    pub fn proxy_url(&self) -> &str {
        &self.proxy_url
    }

    pub fn statedir(&self) -> &Path {
        self.session.statedir()
    }

    /// The session, for what else it offers (the node, a TURN relay, forwards).
    pub fn session(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Whether a node was already joined in `statedir`.
    pub(crate) fn has_state(statedir: &Path) -> bool {
        Session::has_state(statedir)
    }

    /// Start the node in `statedir` and wait until it is on the mesh.
    pub(crate) async fn start(
        options: &MeshOptions,
        statedir: PathBuf,
        hostname: &str,
        join: Join,
    ) -> Result<Self> {
        let mut session_options = SessionOptions::new(statedir, hostname);
        session_options.control_url = join.coord_url;
        session_options.auth_key = join.auth_key;
        session_options.timeout = options.timeout;
        let mut session = Session::start(session_options).await.map_err(|e| match e {
            SessionError::NeedsLogin => needs_login(),
            SessionError::Io(e) => e.into(),
            e => FaktsError::Mesh(e.to_string()),
        })?;
        let proxy_url = session
            .serve_proxy("127.0.0.1:0")
            .await
            .map_err(|e| FaktsError::Mesh(format!("could not serve the mesh proxy: {e}")))?;
        tracing::info!("connected to the mesh as {hostname}; proxy at {proxy_url}");
        Ok(Self { session, proxy_url })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn without_a_key_or_state_it_asks_to_authorize_again() {
        let dir = tempfile::tempdir().unwrap();
        let join = Join {
            coord_url: Some("http://127.0.0.1:1".into()),
            auth_key: None,
        };
        let err = NativeNode::start(&MeshOptions::default(), dir.path().join("n"), "app", join)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no mesh key was granted"), "{err}");
    }

    /// Joins a real mesh: `ARKITEKT_TEST_MESH_URL=https://… ARKITEKT_TEST_MESH_KEY=…
    /// [ARKITEKT_TEST_MESH_PEER=http://peer/] cargo test -p fakts --features mesh-native
    /// -- --ignored native`.
    #[tokio::test]
    #[ignore]
    async fn native_joins_and_rejoins_without_a_key() {
        let url = std::env::var("ARKITEKT_TEST_MESH_URL").expect("ARKITEKT_TEST_MESH_URL");
        let key = std::env::var("ARKITEKT_TEST_MESH_KEY").expect("ARKITEKT_TEST_MESH_KEY");
        let dir = tempfile::tempdir().unwrap();
        let statedir = dir.path().join("node");
        let options = MeshOptions {
            timeout: Duration::from_secs(60),
            ..Default::default()
        };
        let join = Join {
            coord_url: Some(url),
            auth_key: Some(key),
        };
        let node = NativeNode::start(&options, statedir.clone(), "arkitekt-test", join)
            .await
            .unwrap();
        if let Ok(peer) = std::env::var("ARKITEKT_TEST_MESH_PEER") {
            let client = reqwest::Client::builder()
                .proxy(reqwest::Proxy::all(node.proxy_url()).unwrap())
                .build()
                .unwrap();
            let status = client.get(&peer).send().await.unwrap().status();
            println!("{peer}: {status}");
        }
        drop(node);
        tokio::time::sleep(Duration::from_secs(3)).await;

        // The joined node restarts with neither key nor url.
        let join = Join {
            coord_url: None,
            auth_key: None,
        };
        NativeNode::start(&options, statedir, "arkitekt-test", join)
            .await
            .unwrap();
    }
}
