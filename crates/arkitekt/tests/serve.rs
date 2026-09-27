//! `arkitekt::serve`: an app with states and hooks, served next to its own routes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use arkitekt::serve::{AgentTestClient, History};
use arkitekt::{action, serve, App, ServeOptions, State, StateMut};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Default, Serialize, Deserialize, State)]
#[state(name = "Counter")]
struct Counter {
    value: i64,
}

/// Device handle provided by the startup hook.
#[derive(Clone)]
struct Device {
    step: i64,
}

/// Increment
#[action]
async fn increment(counter: StateMut<Counter>, #[inject] device: Device) -> anyhow::Result<i64> {
    Ok(counter.update(|c| {
        c.value += device.step;
        c.value
    })?)
}

#[tokio::test]
async fn serves_an_app_with_hooks() {
    let shut_down = Arc::new(AtomicBool::new(false));
    let flag = shut_down.clone();
    let app = App::new("counter", "0.1.0")
        .state(Counter::default())
        .startup(|startup| async move {
            startup.set_context(Device { step: 5 });
            startup.set_state(Counter { value: 100 })?;
            Ok(())
        })
        .shutdown(move |_| {
            let flag = flag.clone();
            async move {
                flag.store(true, Ordering::SeqCst);
                Ok(())
            }
        })
        .action(increment);

    let router = axum::Router::new().route("/health", get(|| async { "ok" }));
    let served = serve(app, router, ServeOptions::default().history(History::Memory))
        .await
        .expect("an app without services serves without authenticating");
    assert!(served.runtime.fakts().is_none());
    let (router, agent, _runtime) = served.into_parts();

    let mut client = AgentTestClient::serve(router, &agent).await.unwrap();
    assert_eq!(client.init()["states"]["states"]["Counter"]["value"]["value"], 100);
    let health = client.http().get(format!("{}/health", client.base_url())).send().await.unwrap();
    assert_eq!(health.text().await.unwrap(), "ok");

    let task = client.assign("increment", json!({}), true).await.unwrap().task_id;
    let events = client.collect_until_end_state(&task, Duration::from_secs(5)).await.unwrap();
    assert!(events.last().unwrap().is_done(), "{events:?}");
    assert_eq!(events.iter().find(|e| e.is_yield()).unwrap().returns().unwrap()["return0"], 105);

    let state = client.get("/states/Counter").await.unwrap();
    assert_eq!(state, json!({ "revision": 1, "state": { "value": 105 } }));

    agent.shutdown().await;
    assert!(shut_down.load(Ordering::SeqCst));
}
