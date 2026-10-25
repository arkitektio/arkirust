# arkitekt-core

Connect and serve an Arkitekt app on any platform. `no_std` + `alloc`.

An `App` (arkitekt-spec) is a declaration. `Runtime<P>` connects it: it logs
the app in (fakts), builds every service's client, and serves the app's
actions with the platform's rekuest agent. A `Platform` brings what differs:

| Platform | Crate | HTTP | Agent socket | Login kept in |
| --- | --- | --- | --- | --- |
| `Tokio` | arkitekt-tokio | reqwest | tungstenite | a file in the user's directories |
| `Browser` | arkitekt-web | `fetch` | the JS `WebSocket` | the options' store, else memory |
| `Board` | arkitekt-embassy | `NetHost` (its own HTTP/1.1 + TLS 1.3) | its own websocket over embassy-net | the options' store, else memory |

```rust
let runtime = Runtime::connect(platform, app, ConnectOptions::default()).await?;
runtime.serve().await
```

Desktop apps use `arkitekt::run(app)`, which is this on tokio.
