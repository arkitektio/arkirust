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

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fakts::{
    Fakts, FaktsError, Grant, MandateKeyOptions, Manifest, MeshJoin, MeshJoiner, Requirement,
    TokenLoader,
};

/// The key lok's config seeds (`mandate_keys`), for the user `demo` in `demo`.
const MANDATE_KEY: &str = "Y22joLbkjm4vtXMj_T4FD3U99Mb71pTFnUe-8KToAQI";

fn stack_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testing/stack")
}

/// `n` distinct free ports: every listener is held until all are picked.
fn free_ports(n: usize) -> Vec<u16> {
    let listeners: Vec<TcpListener> = (0..n)
        .map(|_| TcpListener::bind("127.0.0.1:0").expect("a free port"))
        .collect();
    listeners
        .iter()
        .map(|l| l.local_addr().expect("bound").port())
        .collect()
}

/// The running stack, taken down (volumes included) however the test ends.
struct Stack {
    project: String,
    env: Vec<(&'static str, String)>,
    lok: u16,
    rekuest: u16,
    _config: tempfile::TempDir,
}

impl Stack {
    fn up() -> Self {
        let ports = free_ports(3);
        let (lok, rekuest, rustfs) = (ports[0], ports[1], ports[2]);

        // lok advertises rekuest's address and anchors its issuer to its own,
        // so both ports go into its config before it starts.
        let template = std::fs::read_to_string(stack_dir().join("configs/lok.yaml"))
            .expect("the stack's lok config");
        let config = tempfile::Builder::new()
            .prefix("arkirust-stack-")
            .tempdir()
            .expect("a temp dir");
        let config_file = config.path().join("lok.yaml");
        std::fs::write(
            &config_file,
            template
                .replace("__LOK_HOST_PORT__", &lok.to_string())
                .replace("__REKUEST_HOST_PORT__", &rekuest.to_string()),
        )
        .expect("the rendered config");
        // Readable by the docker daemon, which mounts it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(config.path(), std::fs::Permissions::from_mode(0o755));
            let _ = std::fs::set_permissions(&config_file, std::fs::Permissions::from_mode(0o644));
        }

        let stack = Self {
            project: format!("arkirust-e2e-{}", std::process::id()),
            env: vec![
                ("LOK_HOST_PORT", lok.to_string()),
                ("REKUEST_HOST_PORT", rekuest.to_string()),
                ("RUSTFS_HOST_PORT", rustfs.to_string()),
                (
                    "LOK_CONFIG_FILE",
                    config_file.to_string_lossy().into_owned(),
                ),
            ],
            lok,
            rekuest,
            _config: config,
        };
        // A locally built tag has nothing to pull: keep going with the local image.
        stack.compose(&["pull", "--ignore-pull-failures", "--quiet"]);
        let up = stack.compose(&["up", "--detach"]);
        assert!(
            up.status.success(),
            "the stack did not start: {}",
            String::from_utf8_lossy(&up.stderr)
        );
        stack
    }

    fn compose(&self, args: &[&str]) -> Output {
        Command::new("docker")
            .args(["compose", "--project-name", &self.project])
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (*k, v.as_str())))
            .current_dir(stack_dir())
            .output()
            .expect("docker runs")
    }

    /// Wait until `service` answers `GET /ht` on its host port.
    async fn wait_healthy(&self, service: &str, port: u16) {
        let url = format!("http://localhost:{port}/ht");
        let deadline = Instant::now() + Duration::from_secs(300);
        let http = reqwest::Client::new();
        loop {
            if let Ok(resp) = http.get(&url).timeout(Duration::from_secs(5)).send().await {
                if resp.status().is_success() {
                    return;
                }
            }
            if Instant::now() > deadline {
                let logs = self.compose(&["logs", "--no-log-prefix", "--tail", "50", service]);
                panic!(
                    "{service} never became healthy at {url}:\n{}",
                    String::from_utf8_lossy(&logs.stdout)
                );
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.compose(&["down", "--volumes", "--remove-orphans", "--timeout", "1"]);
    }
}

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
