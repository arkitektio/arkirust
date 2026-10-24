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
