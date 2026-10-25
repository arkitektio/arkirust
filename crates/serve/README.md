# arkitekt-serve

Serve an Arkitekt app over HTTP and a websocket, without a rekuest server:
the Rust port of Python's `arkitekt-fastapi` (`rekuest.contrib.fastapi`),
contract included (`tests/fixtures/serve/python`, recorded from Python).

```rust
let app = App::new("camera", "0.1.0").state(CameraState::default()).action(set_exposure);
let router = axum::Router::new().route("/health", get(|| async { "ok" }));
serve(app, router, ServeOptions::default()).await?.listen("0.0.0.0:8099").await
```

The actions become `POST /{interface}`, an observer websocket at `/ws`
streams what happens, and GET routes show tasks, states, locks, the state
history and the journal. `arkitekt::serve` (feature `serve`) re-exports it;
`testing` brings `AgentTestClient`.
