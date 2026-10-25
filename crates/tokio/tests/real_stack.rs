//! The tokio platform against the real stack (`testing/stack`): an `App`
//! connected the way `arkitekt::run` connects it (the login with the stack's
//! mandate key, its services, rekuest's alias), served by the platform's
//! agent, until the server acknowledges its journal.
//!
//! It pulls and starts the whole stack, so it is opt-in:
//!
//! ```sh
//! cargo test -p arkitekt-tokio --test real_stack -- --ignored --nocapture
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use arkitekt_core::ConnectOptions;
use arkitekt_spec::{action, App};
use fakts::{Grant, MandateKeyOptions};

#[path = "../../../testing/stack/harness.rs"]
mod harness;

use harness::{Stack, MANDATE_KEY};

/// Add two numbers
#[action]
async fn add(a: i64, b: i64) -> i64 {
    a + b
}

async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !check() {
        assert!(
            Instant::now() < deadline,
            "{what} did not happen within 60 s"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test]
#[ignore = "starts the docker test stack (testing/stack): run with --ignored"]
async fn a_tokio_app_registers_and_the_server_acknowledges_its_journal() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("rekuest=info,rekuest_session=info,fakts=info")
        .try_init();
    fakts::install_crypto_provider();
    let stack = Stack::up();
    stack.wait_healthy("lok", stack.lok).await;
    stack.wait_healthy("rekuest", stack.rekuest).await;
    // The agent keeps its journal in `<identifier>.journal.db` where it runs.
    let dir = tempfile::tempdir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();

    let options = ConnectOptions {
        url: Some(format!("http://localhost:{}", stack.lok)),
        grant: Some(Grant::MandateKey(Arc::new(MandateKeyOptions::new(
            MANDATE_KEY,
        )))),
        no_cache: true,
        ..ConnectOptions::default()
    };
    let app = App::new("test_manifest", "0.1.0").action(add);
    let runtime = arkitekt_tokio::connect_with(app, options)
        .await
        .expect("lok logs the key in");
    let agent = Arc::new(arkitekt_tokio::agent(&runtime).await.expect("an agent"));
    let running = tokio::spawn({
        let agent = agent.clone();
        async move { agent.run().await }
    });

    // Activated: SESSION_INIT is the session's first entry.
    eventually("activation", || {
        agent.journal().watermark().is_some_and(|w| w.pos >= 1)
    })
    .await;
    // The server persisted it and said so: nothing is waiting for an ack.
    eventually("the journal ack", || {
        agent
            .driver()
            .with_session(|session| session.outbox().retained_len() == 0)
    })
    .await;
    assert!(!running.is_finished(), "the agent is still serving");
    running.abort();
}
