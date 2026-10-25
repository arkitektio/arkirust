# arkitekt-web

Arkitekt apps in a browser or node (wasm32): arkitekt-core's `Platform` over
`fetch` (the login, rath) and the JS `WebSocket` (rekuest's web agent), on
the JS event loop.

```rust
let options = ConnectOptions::default().url("https://go.arkitekt.live");
wasm_bindgen_futures::spawn_local(async move {
    arkitekt_web::run(app, options).await.ok();
});
```

Nothing is kept unless the options give a store. Node enforces no CORS; a
browser needs lok and takt to allow its origin. On other targets the crate
is empty.
