//! A wasm agent on the real stack: logs in at lok with the stack's mandate
//! key (fakts over fetch), registers with takt over the JS WebSocket
//! (`rekuest::web::Agent`), activates, and the server acknowledges its
//! journal. Node has `fetch` and `WebSocket` (and enforces no CORS; a
//! browser would need lok and takt to allow its origin).
//!
//! Run by `cargo test -p rekuest --test real_rekuest -- --ignored`, which
//! starts the stack and passes it in `ARKIRUST_E2E_LOK` and
//! `ARKIRUST_E2E_KEY`; skipped without them.

use std::sync::Arc;
use std::time::Duration;

use arkitekt_spec::{action, Context, Registry};
use fakts::{Fakts, Grant, MandateKeyOptions, Manifest, Requirement};
use rekuest::web::{Agent, AgentOptions};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

/// Add two numbers
#[action]
async fn add(a: i64, b: i64) -> i64 {
    a + b
}

/// `process.env[name]` under node.
fn env(name: &str) -> Option<String> {
    let process = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("process")).ok()?;
    let env = js_sys::Reflect::get(&process, &JsValue::from_str("env")).ok()?;
    js_sys::Reflect::get(&env, &JsValue::from_str(name))
        .ok()?
        .as_string()
}

async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..240 {
        if check() {
            return;
        }
        gloo_timers::future::TimeoutFuture::new(250).await;
    }
    panic!("{what} did not happen within 60 s");
}

#[wasm_bindgen_test]
async fn a_wasm_agent_registers_and_the_server_acknowledges_its_journal() {
    let (Some(lok), Some(key)) = (env("ARKIRUST_E2E_LOK"), env("ARKIRUST_E2E_KEY")) else {
        return;
    };
    let mut manifest = Manifest::new("test_manifest", "0.1.0");
    manifest
        .requirements
        .push(Requirement::new("rekuest", "live.arkitekt.rekuest"));
    let fakts = Fakts::builder(lok, manifest)
        .grant(Grant::MandateKey(Arc::new(MandateKeyOptions::new(key))))
        .no_cache(true)
        .load()
        .await
        .expect("lok logs the key in");
    let alias = fakts
        .get_alias("rekuest")
        .await
        .expect("rekuest is reachable");

    // rath over fetch, with the agent's token.
    let graphql = rath::Rath::new(alias.to_http_path("graphql"), Arc::new(fakts.clone()));
    let typename = graphql
        .execute_raw(&serde_json::json!({"query": "{ __typename }"}))
        .await
        .expect("rath reaches rekuest's GraphQL");
    assert_eq!(typename["__typename"], "Query");

    let mut options = AgentOptions::new(alias.to_ws_path("agi"));
    options.name = Some("arkirust-e2e-wasm".into());
    options.policy.initial = Duration::from_millis(200);
    let mut registry = Registry::new();
    registry.register(add);
    let agent = Arc::new(Agent::new(
        options,
        registry,
        Context::default(),
        Arc::new(fakts),
    ));
    wasm_bindgen_futures::spawn_local({
        let agent = agent.clone();
        async move {
            if let Err(e) = agent.run().await {
                panic!("the agent stopped: {e}");
            }
        }
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
    // The test that started the stack looks for this line.
    web_sys_log("ARKIRUST_E2E_WASM_ACKED");
}

fn web_sys_log(line: &str) {
    let console = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("console")).unwrap();
    let log: js_sys::Function = js_sys::Reflect::get(&console, &JsValue::from_str("log"))
        .unwrap()
        .into();
    log.call1(&console, &JsValue::from_str(line)).unwrap();
}
