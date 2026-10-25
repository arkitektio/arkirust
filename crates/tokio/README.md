# arkitekt-tokio

Arkitekt apps on tokio: the desktop and server platform (arkitekt-core's
`Platform`). It logs in over reqwest, keeps the login in a file, finds the
deployment (`FAKTS_URL`, `ARKITEKT_URL`) and the mesh (`ARKITEKT_MESH`,
`ARKITEKT_MESH_PROXY`) in the environment, and serves with rekuest's tokio
agent and its SQLite journal until it stops or Ctrl-C.

```rust
arkitekt_tokio::run(App::new("hello", "0.1.0").action(greet)).await
```

`arkitekt` re-exports it (`arkitekt::run`). Features: `signal`, `ring`,
`wal`, `user-dirs` (default), and `mesh`, `mesh-native`, `mesh-relay`; with
the defaults off it builds for ESP-IDF.
