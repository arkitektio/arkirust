# Arkitekt goes Rust

Arkirust brings the [Arkitekt](https://arkitekt.live) Python libraries to Rust.
It follows the same design: an app is a declaration, services turn the app's
configuration into typed clients, and plain functions become actions that
anyone in your Arkitekt deployment can run.

```rust
use arkitekt::{action, run, App, Task};
use mikro::{axes_for, ArrayDataset, Mikro};

/// Create Rusty Image
///
/// Creates an image filled with random noise.
///
/// # Arguments
/// * `name` - The name of the new image
/// * `size` - Edge length in pixels
#[action]
async fn create_rusty_image(
    name: String,
    #[port(default = 512)] size: i64,
    #[inject] mikro: Mikro,
    task: Task,
) -> anyhow::Result<ArrayDataset> {
    task.progress(10, "generating noise");
    let data = ndarray::ArrayD::from_shape_fn(vec![1, size as usize, size as usize], |_| rand::random::<u16>());
    Ok(mikro.create_array_dataset(&name, &data, axes_for(&["c", "y", "x"])).await?)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // An App is a declaration; `run` authenticates, registers and blocks.
    let app = App::new("rusty-images", "0.1.0")
        .service(mikro::service())
        .action(create_rusty_image);

    run(app).await
}
```

## Crates

| Crate                                 | Python twin         | What it does                                                                                     |
| ------------------------------------- | ------------------- | ------------------------------------------------------------------------------------------------ |
| [`fakts`](crates/fakts)               | `fakts`             | Discovers the server, authorizes the app once (OAuth2 device code), caches and refreshes tokens, resolves service aliases |
| [`rath`](crates/rath)                 | `rath`              | Authenticated GraphQL client that retries on 401 and tags requests with the current task         |
| [`rekuest`](crates/rekuest)           | `rekuest`           | Ports, structures, the `/agi` websocket agent, and `Task`                                         |
| [`rekuest-macros`](crates/rekuest-macros) | `@register`     | `#[action]`: turns a function into an action definition                                          |
| [`arkitekt`](crates/arkitekt)         | `arkitekt`          | `App`, `Service`, `Runtime`; re-exports everything above                                          |
| [`mikro`](crates/mikro)               | `mikro`             | Mikro client: array datasets stored as zarr v3 on the S3 datalayer                                |

Apps normally depend on `arkitekt` (and service crates such as `mikro`) only.

On crates.io, `rath` and `mikro` are published as `arkitekt-rath` and
`arkitekt-mikro` (the plain names were taken); they are still imported as
`rath` / `mikro`:

```toml
[dependencies]
arkitekt = "0.1"
mikro = { package = "arkitekt-mikro", version = "0.1" }
```

All crates are released in lockstep from Conventional Commits via
[release-plz](https://release-plz.dev) (see `release-plz.toml`).

## Concepts

**App.** `App::new(identifier, version)` declares who the app is, which
services it uses and which actions it offers. The fakts manifest is derived
from that declaration: each service contributes its requirements, and apps
that offer actions also require `rekuest`.

**Services.** A service implements `arkitekt::Service`. It names the fakts
instances it needs (for mikro, `mikro` → `live.arkitekt.mikro` and
`s3` → `live.arkitekt.s3`) and, once the app is connected, builds its client
from the resolved aliases. Clients are looked up by type: actions take them
as `#[inject]` parameters, and scripts call `runtime.require::<Mikro>()`.

**Actions.** `#[action]` reads everything from the signature:

* Every parameter becomes a port, typed through `PortType`. Supported types
  are `String`, `bool`, integers, floats, `chrono::DateTime<Utc>`,
  `Option<T>`, `Vec<T>`, `HashMap<String, T>` and any `Structure`.
* The return value becomes `return0`. A tuple fans out into `return0..n`.
  A `Result` is unwrapped, and an `Err` reports the task as `CRITICAL`, like an
  exception in Python. Arguments that cannot be converted report `FAILED`.
* Returning `impl Stream<Item = T>` makes a generator, which yields once per item.
* A `Task` parameter gives access to `task.progress(..)` and `task.log(..)`.
* `#[port(default = …, description = "…", label = "…")]` refines a port.
* The doc comment supplies the description. Port descriptions come from its
  `# Arguments` section, and the return description from `# Returns`.

The generated definitions match what the Python library registers for the
same function. `crates/rekuest/tests/actions.rs` checks this against a
fixture dumped from Python.

**Structures.** A type that lives on a service, such as mikro's
`ArrayDataset`, implements `Structure`. It travels by reference as
`{"__identifier": "@mikro/arraydataset", "object": "<id>"}` and is expanded
back through the service's client before your function runs.

**States.** An app can hold state, for example what a piece of hardware is
doing right now:

```rust
#[derive(Clone, Default, Serialize, Deserialize, arkitekt::State)]
#[state(name = "CameraState", locks = ["camera"])]
struct CameraState {
    connected: bool,
    exposure_ms: f64,
}

/// Set Exposure
#[action]
async fn set_exposure(exposure_ms: f64, camera: StateMut<CameraState>) -> anyhow::Result<f64> {
    camera.update(|c| c.exposure_ms = exposure_ms)?;
    Ok(exposure_ms)
}

let app = App::new("camera", "0.1.0")
    .state(CameraState::default())
    .startup(|startup| async move { startup.set_state(connect_camera().await?)?; Ok(()) })
    .action(set_exposure);
```

* How changes are published:
  * Every change made through `StateMut::update` is published as JSON-patch
    `STATE_PATCH` events, numbered with a revision per app.
  * Whoever observes the app always knows the current state: the rekuest
    server, or subscribers of a served app, which also get `GET /states`.
  * **Do not write "getter" actions** that only return a state. Reading state
    is what that observation is for; actions are for doing something.
    `StateRef<T>` exists for actions that need to *consult* a state while
    doing their work.
* Locks:
  * `locks = [...]` on a state means an action must hold those locks to
    change it. Actions taking the state hold them automatically.
  * `#[action(locks = ["stage"])]` declares further locks.
  * Assignments of one action run one at a time unless you set
    `#[action(concurrency = "parallel")]`.
* Pausing:
  * `task.pausepoint().await` marks where a task may be paused, and stepped
    through with `PAUSE`/`RESUME`.
* Hooks:
  * `.startup(..)` runs once before any action.
  * `.background(..)` runs for the app's lifetime.
  * `.shutdown(..)` runs when the app stops.

## Serving without a rekuest server

Like Python's `arkitekt.serve(app, fastapi_app)`, an app can be served
directly over HTTP, next to your own axum routes. Enable the `serve` feature
of `arkitekt`:

```rust
let router = axum::Router::new().route("/health", get(|| async { "ok" }));
arkitekt::serve(app, router, ServeOptions::default()).await?.listen("0.0.0.0:8099").await
```

The HTTP and websocket contract is the Python one (`rekuest.contrib.fastapi`),
so clients written against a Python app work unchanged:

| Route | What it does |
|---|---|
| `POST /{interface}`, `POST /assign/{interface}` | start a task |
| `POST /cancel`, `/pause`, `/resume`, `/step` | control a task |
| `WS /ws` | send `{"type": "INIT"}` and receive a snapshot, then every YIELD / COMPLETED / LOG / STATE_PATCH / LOCK frame |
| `GET /tasks`, `/states`, `/states/{name}`, `/locks` | what the app is doing right now |
| `GET /states/checkout`, `/states/segments`, `/forward_events/…`, … | state history, kept in SQLite (`agent_data.db`) |
| `GET /schemas/…`, `/openapi.json`, `/docs` | the declaration and an API page |

* **Service connections:** an app without services never authenticates. One
  that uses services (e.g. mikro) authenticates for exactly those.
* **Auth:** `ServeOptions::auth(|request| ...)` protects `/assign` and the
  websocket.
* **Testing:** the `testing` feature adds `AgentTestClient`.

Because the served app tracks every state itself, do not add "getter" actions
that only return state: read `GET /states` or subscribe to the websocket.

## Running

```sh
# connects to $FAKTS_URL, or the public deployment at https://go.arkitekt.live
cargo run -p arkitekt-examples --bin hello
cargo run -p arkitekt-examples --bin upload_demo    # action: upload a fake image to mikro
cargo run -p arkitekt-examples --bin create_image
cargo run -p arkitekt-examples --bin script         # services only, no agent
cargo run -p arkitekt-examples --bin camera_serve   # states + hooks, served on :8099 (no server needed)
FAKTS_URL=https://your.arkitekt.server cargo run -p arkitekt-examples --bin hello
```

On the first run the app prints a URL where you approve it. After that, the
credentials are cached (mode 0600) under your user state directory, for
example `~/.local/state/arkitekt/cache/*_fakts_cache.rs.json`. Set `FAKTS_TOKEN=client_id:refresh_token`
to skip the interactive step, for instance in CI.

## Status

Implemented:

- [x] Fakts v2: discovery, device code, rotating refresh tokens, cache, and alias challenges (including Ed25519)
- [x] GraphQL client with token refresh and task attribution
- [x] `#[action]` with typed ports, defaults, docs, structures, tuples, `Result` and generators
- [x] Rekuest agent: register, heartbeat, assign, cancel, interrupt, progress, log, reports acked by seq, reconnect with backoff
- [x] Mikro: create, fetch and read array datasets through zarr v3 on S3
- [x] States (JSON-patch published, with history), locks, pause/resume/step, startup/background/shutdown hooks
- [x] Serving over HTTP + websocket without a rekuest server, contract-tested against the Python implementation

Not implemented yet:

- [ ] GraphQL subscriptions over websocket
- [ ] Memory structures and calling other actions from an action
- [ ] Structure-typed fields inside states
- [ ] The redeem grant for headless deployment
- [ ] Other mikro data types (tables, files, meshes, …) and multiscale pyramids
- [ ] Other services (unlok, fluss, kabinet, …)

## Development

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features
```

The served-agent contract is recorded from the Python implementation
(`crates/rekuest/tests/fixtures/serve/`). Run `record.sh python` to re-record
it. To check a running Rust server against it, run `record.sh rust <url>`
followed by `compare.py python rust`.
