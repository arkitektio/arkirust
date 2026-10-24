//! The agent against a real rekuest: the test stack (`testing/stack`) with
//! lok, rekuest and takt. The agent logs in with the stack's mandate key,
//! registers over `/agi`, activates, and the server acknowledges its
//! journal (`JOURNAL_ACK`), which the agent records in its local journal.
//!
//! It pulls and starts the whole stack, so it is opt-in:
//!
//! ```sh
//! cargo test -p rekuest --test real_rekuest -- --ignored --nocapture
//! ```
//!
//! The second test runs the same from wasm: `checks/wasm`'s real-stack test
//! under node (`wasm-pack test --node`, which must be installed); the third
//! from embassy: `checks/embassy`'s `real_stack` (no std below the binary).

use std::sync::Arc;
use std::time::{Duration, Instant};

use fakts::{Fakts, Grant, MandateKeyOptions, Manifest, Requirement};
use rekuest::store::HistoryStore;
use rekuest::{action, Agent, AgentOptions, Context, Registry};

#[path = "../../../testing/stack/harness.rs"]
mod harness;

use harness::{Stack, MANDATE_KEY};

/// Add two numbers
#[action]
async fn add(a: i64, b: i64) -> i64 {
    a + b
}

async fn eventually<T>(what: &str, mut check: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(value) = check().await {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "{what} did not happen within 60 s"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test]
#[ignore = "starts the docker test stack (testing/stack): run with --ignored"]
async fn the_agent_registers_and_the_server_acknowledges_its_journal() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("rekuest=debug,rekuest_session=debug,fakts=info")
        .try_init();
    fakts::install_crypto_provider();
    let stack = Stack::up();
    stack.wait_healthy("lok", stack.lok).await;
    stack.wait_healthy("rekuest", stack.rekuest).await;

    let mut manifest = Manifest::new("test_manifest", "0.1.0");
    manifest
        .requirements
        .push(Requirement::new("rekuest", "live.arkitekt.rekuest"));
    let fakts = Fakts::builder(format!("http://localhost:{}", stack.lok), manifest)
        .grant(Grant::MandateKey(Arc::new(MandateKeyOptions::new(
            MANDATE_KEY,
        ))))
        .no_cache(true)
        .load()
        .await
        .expect("lok logs the key in");
    let alias = fakts
        .get_alias("rekuest")
        .await
        .expect("rekuest is reachable");

    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("journal.db");
    let mut options = AgentOptions::new(alias.to_ws_path("agi"));
    options.name = Some("arkirust-e2e".into());
    options.journal_path = Some(journal.clone());
    let mut registry = Registry::new();
    registry.register(add);
    let agent = Arc::new(Agent::new(
        options,
        registry,
        Context::default(),
        Arc::new(fakts),
    ));
    let running = tokio::spawn({
        let agent = agent.clone();
        async move { agent.run().await }
    });

    // Activated: SESSION_INIT is the session's first entry.
    eventually("activation", async || {
        agent.journal().watermark().filter(|w| w.pos >= 1)
    })
    .await;
    assert!(agent.journal().flush(Duration::from_secs(5)).await);

    // The server persisted it and said so; the agent recorded the ack.
    let store = HistoryStore::open(&journal).unwrap();
    eventually("the journal ack", async || {
        store
            .unacked_entries(None)
            .await
            .unwrap()
            .is_empty()
            .then_some(())
    })
    .await;

    assert!(!running.is_finished(), "the agent is still serving");
    running.abort();
}

#[tokio::test]
#[ignore = "starts the docker test stack and runs checks/wasm under node: run with --ignored"]
async fn a_wasm_agent_registers_and_the_server_acknowledges_its_journal() {
    fakts::install_crypto_provider();
    let stack = Stack::up();
    stack.wait_healthy("lok", stack.lok).await;
    stack.wait_healthy("rekuest", stack.rekuest).await;

    let checks = harness::stack_dir().join("../../checks/wasm");
    let output = tokio::process::Command::new("wasm-pack")
        .args([
            "test",
            "--node",
            "--test",
            "real_stack",
            "--",
            "--nocapture",
        ])
        .env(
            "ARKIRUST_E2E_LOK",
            format!("http://localhost:{}", stack.lok),
        )
        .env("ARKIRUST_E2E_KEY", MANDATE_KEY)
        .current_dir(checks)
        .output()
        .await
        .expect("wasm-pack runs (is it installed?)");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("ARKIRUST_E2E_WASM_ACKED"),
        "the wasm agent did not get its journal acknowledged:\n{stdout}\n{stderr}"
    );
}

#[tokio::test]
#[ignore = "starts the docker test stack and runs checks/embassy's real_stack: run with --ignored"]
async fn an_embassy_agent_registers_and_the_server_acknowledges_its_journal() {
    fakts::install_crypto_provider();
    let stack = Stack::up();
    stack.wait_healthy("lok", stack.lok).await;
    stack.wait_healthy("rekuest", stack.rekuest).await;

    let checks = harness::stack_dir().join("../../checks/embassy");
    let output = tokio::process::Command::new(env!("CARGO"))
        .args(["run", "--quiet", "--bin", "real_stack"])
        .env(
            "ARKIRUST_E2E_LOK",
            format!("http://localhost:{}", stack.lok),
        )
        .env("ARKIRUST_E2E_KEY", MANDATE_KEY)
        .current_dir(checks)
        .output()
        .await
        .expect("cargo runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success()
            && stdout.contains("ARKIRUST_E2E_EMBASSY_RATH")
            && stdout.contains("ARKIRUST_E2E_EMBASSY_ACKED"),
        "the embassy agent did not get its journal acknowledged:\n{stdout}\n{stderr}"
    );
}
