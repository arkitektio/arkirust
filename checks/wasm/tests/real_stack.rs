//! A wasm app on the real stack, on arkitekt-web: connects (the login at
//! lok with the stack's mandate key, fakts over fetch), registers with takt
//! over the JS WebSocket (rekuest's web agent), activates, and the server
//! acknowledges its journal. Node has `fetch` and `WebSocket` (and enforces no CORS; a
//! browser would need lok and takt to allow its origin).
//!
//! Run by `cargo test -p rekuest --test real_rekuest -- --ignored`, which
//! starts the stack and passes it in `ARKIRUST_E2E_LOK` and
//! `ARKIRUST_E2E_KEY`; skipped without them.

use std::sync::Arc;
use std::time::Duration;

use arkitekt_core::ConnectOptions;
use arkitekt_spec::{action, App};
use fakts::{Grant, MandateKeyOptions};
use rekuest::ConnectionPolicy;
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
    let options = ConnectOptions {
        url: Some(lok),
        grant: Some(Grant::MandateKey(Arc::new(MandateKeyOptions::new(key)))),
        no_cache: true,
        policy: Some(ConnectionPolicy {
            initial: Duration::from_millis(200),
            ..ConnectionPolicy::default()
        }),
        ..ConnectOptions::default()
    };
    let app = App::new("test_manifest", "0.1.0").action(add);
    let runtime = arkitekt_web::connect(app, options)
        .await
        .expect("lok logs the key in");
    let alias = runtime.agent_alias().await.expect("rekuest is reachable");

    // rath over fetch, with the app's token.
    let fakts = runtime.agent_fakts().expect("logged in").clone();
    let graphql = rath::Rath::new(alias.to_http_path("graphql"), Arc::new(fakts));
    let typename = graphql
        .execute_raw(&serde_json::json!({"query": "{ __typename }"}))
        .await
        .expect("rath reaches rekuest's GraphQL");
    assert_eq!(typename["__typename"], "Query");

    let agent = Arc::new(arkitekt_web::agent(&runtime).await.expect("an agent"));
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
