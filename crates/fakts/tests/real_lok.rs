//! The mandate-key login against a real lok: the test stack (`testing/stack`)
//! with lok, rekuest and takt, and nothing standing in for a server. lok is
//! seeded with a mandate key (`mandate_keys` in its config) and a hub whose
//! rekuest the client challenges for real.
//!
//! It pulls and starts the whole stack, so it is opt-in:
//!
//! ```sh
//! cargo test -p fakts --test real_lok -- --ignored --nocapture
//! ```

use std::sync::{Arc, Mutex};

use fakts::{
    Fakts, FaktsError, Grant, MandateKeyOptions, Manifest, MeshJoin, MeshJoiner, Requirement,
    TokenLoader,
};

#[path = "../../../testing/stack/harness.rs"]
mod harness;

use harness::{Stack, MANDATE_KEY};

fn manifest() -> Manifest {
    let mut manifest = Manifest::new("test_manifest", "0.1.0");
    manifest.scopes = vec!["openid".into(), "profile".into(), "email".into()];
    manifest
        .requirements
        .push(Requirement::new("rekuest", "live.arkitekt.rekuest"));
    manifest
}

async fn login(stack: &Stack, key: &str) -> fakts::Result<Fakts> {
    Fakts::builder(format!("http://localhost:{}", stack.lok), manifest())
        .grant(Grant::MandateKey(Arc::new(MandateKeyOptions::new(key))))
        .no_cache(true)
        .load()
        .await
}

/// Remembers what fakts handed it, and stays off the mesh.
#[derive(Default)]
struct Recorder(Mutex<Vec<MeshJoin>>);

#[async_trait::async_trait]
impl MeshJoiner for Recorder {
    async fn join(&self, join: MeshJoin) -> fakts::Result<Option<String>> {
        self.0.lock().unwrap().push(join);
        Ok(None)
    }
}

#[tokio::test]
#[ignore = "starts the docker test stack (testing/stack): run with --ignored"]
async fn a_mandate_key_logs_in_at_lok_and_renews_as_the_same_client() {
    fakts::install_crypto_provider();
    let stack = Stack::up();
    stack.wait_healthy("lok", stack.lok).await;
    stack.wait_healthy("rekuest", stack.rekuest).await;

    let fakts = login(&stack, MANDATE_KEY)
        .await
        .expect("lok logs the key in");

    let active = fakts.active().await;
    assert!(!active.auth.client_id.is_empty());
    assert_eq!(active.auth.refresh_token, None);
    assert_eq!(
        fakts.instance("rekuest").await.unwrap().service,
        "live.arkitekt.rekuest"
    );

    // The hub's rekuest answers its challenge for real.
    let alias = fakts.get_alias("rekuest").await.unwrap();
    assert_eq!(
        alias.challenge_path(),
        format!("http://localhost:{}/ht", stack.rekuest)
    );

    // Renewing logs in again with the key, and is the same client.
    let first = fakts.get_token().await.unwrap();
    assert!(!fakts.refresh_token(&first).await.unwrap().is_empty());
    assert_eq!(fakts.active().await.auth.client_id, active.auth.client_id);

    // An app with its own mesh node asks for a mesh key; this deployment has
    // no mesh, so lok grants none, and the app stays direct.
    let joiner = Arc::new(Recorder::default());
    let fakts = Fakts::builder(format!("http://localhost:{}", stack.lok), manifest())
        .grant(Grant::MandateKey(Arc::new(MandateKeyOptions::new(
            MANDATE_KEY,
        ))))
        .no_cache(true)
        .mesh_joiner(joiner.clone())
        .load()
        .await
        .expect("lok logs the key in with a mesh key requested");
    {
        let joins = joiner.0.lock().unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!(joins[0].auth_key, None);
        assert_eq!(joins[0].hostname, "test-manifest");
    }
    assert_eq!(fakts.mesh_proxy(), None);
    assert_eq!(fakts.get_alias("rekuest").await.unwrap().proxy, None);

    // A key lok does not know is refused, as an invalid grant.
    let err = login(&stack, "an-unknown-key-of-the-right-length-0123456789")
        .await
        .unwrap_err();
    assert!(matches!(err, FaktsError::InvalidGrant(_)), "{err}");
}
