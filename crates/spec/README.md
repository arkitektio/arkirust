# arkitekt-spec

What an Arkitekt app is, shared by everything that declares, runs or serves
one: the Rust twin of Python's `arkitekt-spec`. `no_std` + `alloc`.

| Module | What |
| --- | --- |
| `actions` | The action language: `Definition`, `Port`, `Implementation`, `AgentDeclaration`, widgets. What an agent registers and a server validates. |
| `manifest` | `Manifest`, `Requirement`: who an app is and which services it needs. |
| `wiring` | `Alias` (one way to reach a service instance) and `Wiring`, what a service's client is built from. |

With the `declare` feature (default):

| Item | What |
| --- | --- |
| `App` | Identity, services and offerings. Does no I/O; a runtime runs it. |
| `#[action]`, `Registry`, `PortType`, `Structure` | Typed functions as actions. |
| `#[derive(State)]`, `StateMut`, `StateRef` | Agent state, every change diffed into JSON patches. |
| `Startup`, `Background` | Hooks. |
| `Task`, `TaskBackend` | What a running action reports through; a runtime implements the backend (rekuest's executor), `Task::local()` reports to `tracing`. |
| `#[service]`, `Service` | A client the app uses, built from a `Wiring` (fakts provides one). |
| `rt` | The async runtime interface (spawn, sleep, clock, randomness) a platform installs. `tokio` and `js` features bring `rt::Tokio` and `rt::Js`. |

A server needs only the wire types:

```toml
arkitekt-spec = { version = "0.15", default-features = false }
```

Apps use it through `arkitekt` (or `arkitekt-embassy`), which re-export it.
