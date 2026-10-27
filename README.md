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
        .service(mikro::service)
        .action(create_rusty_image);

    run(app).await
}
```

## Crates

| Crate                                 | Python twin         | What it does                                                                                     |
| ------------------------------------- | ------------------- | ------------------------------------------------------------------------------------------------ |
| [`fakts`](crates/fakts)               | `fakts`             | Discovers the server, authorizes the app once (OAuth2 device code), caches and refreshes tokens, resolves service aliases |
| [`rath`](crates/rath)                 | `rath`              | Authenticated GraphQL client that retries on 401 and tags requests with the current task         |
| [`arkitekt-spec`](crates/spec)       | `arkitekt-spec`     | What an app is: the action language (definitions, ports), `App`, `#[action]`, `Registry`, states, hooks, `Task`, `Service`, and `rt` (the runtime interface); no_std. Without its `declare` feature only the wire types |
| [`arkitekt-macros`](crates/macros)    | `@register`         | `#[action]`, `#[derive(State)]`, `#[service]` (used through `arkitekt-spec`)                     |
| [`rekuest`](crates/rekuest)           | `arkitekt-runtime` + `rekuest` | Runs the declared actions (executor, journal, state hub) and serves them: the `/agi` websocket agent |
| [`arkitekt-core`](crates/core)        | `arkitekt` (`Runtime`) | `Runtime<P: Platform>`: logs an app in, builds its services, serves it with the platform's agent; no_std |
| [`arkitekt-tokio`](crates/tokio)      |                     | The desktop and server platform: reqwest, tungstenite, a login file, the SQLite journal, Ctrl-C  |
| [`arkitekt-web`](crates/web)          |                     | The browser and node platform (wasm32): `fetch` and the JS `WebSocket`                           |
| [`arkitekt-embassy`](crates/embassy)  |                     | The microcontroller platform (`Board`, no_std): embassy-net, its own HTTP/1.1, websocket and TLS 1.3 |
| [`arkitekt-serve`](crates/serve)      | `arkitekt-fastapi`  | Serves an app over HTTP and a websocket without a rekuest server (Python's contract, recorded) |
| [`arkitekt`](crates/arkitekt)         | `arkitekt`          | `run(app)` / `connect(app)` on tokio (default; `web` for the browser's); re-exports everything above |
| [`mikro`](crates/mikro)               | `mikro`             | Mikro client: array datasets stored as zarr v3 on the S3 datalayer                                |
| [`lovekit`](crates/lovekit)           | `lovekit`           | Lovekit client: LiveKit broadcasts and stream tokens; with `livekit`, joins rooms and publishes video, also through the mesh |

Apps normally depend on `arkitekt` (and service crates such as `mikro`) only.

On crates.io, `rath`, `mikro` and `lovekit` are published as `arkitekt-rath`,
`arkitekt-mikro` and `arkitekt-lovekit` (the plain names were taken); they are
still imported as `rath` / `mikro` / `lovekit`:

```toml
[dependencies]
arkitekt = "0.1"
mikro = { package = "arkitekt-mikro", version = "0.1" }
```

All crates are released in lockstep from Conventional Commits: every push to
main with a `feat`, `fix` or `perf` is a release (semantic-release, see
`releaserc.toml`). The mesh client (`arkitekt-mesh`, `arkitekt-meshd` and its
Python bindings) lives in its own repository,
[arkitektio/arkitekt-mesh](https://github.com/arkitektio/arkitekt-mesh), and is
released from there.

## Concepts

**App.** `App::new(identifier, version)` declares who the app is, which
services it uses and which actions it offers. The fakts manifest is derived
from that declaration: each service contributes its requirements, and apps
that offer actions also require `rekuest`.

**Services.** A service is a builder function, as with Python's
`@registry.service`. Each `#[require]` parameter is a fakts requirement keyed by
its name, resolved to an `Alias` before the body runs; any other parameter is
provided by type by the login (`Fakts`, the token loader):

```rust
/// Mikro: the user's images, files and metadata.
#[arkitekt::service(name = "mikro")]
pub fn service(
    #[require("live.arkitekt.mikro", "Where the user's images and their metadata live")] mikro: Alias,
    #[require("live.arkitekt.s3", "Where the user's files are stored")] s3: Alias,
    fakts: Fakts,
) -> anyhow::Result<Mikro> {
    let rath = Rath::from_alias(&mikro, "graphql", Arc::new(fakts))?;
    Ok(Mikro::new(rath, DataLayer::from_alias(&s3)))
}
```

`Option<Alias>` makes a requirement optional. The returned client is looked up
by type: actions take it as an `#[inject]` parameter, and scripts call
`runtime.require::<Mikro>()`. Build clients from the alias (`Rath::from_alias`,
`alias.http_client()`) so they go through the mesh when needed.

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

## The mesh

A deployment can serve some instances only over its private mesh (an ionscale
tailnet). Build with the `mesh` feature and run with `ARKITEKT_MESH=1` (or
`ConnectOptions::mesh(..)`):

* When authorizing, the app asks for a mesh key. The approver can allow it.
* The app then runs [`arkitekt-meshd`](https://github.com/arkitektio/arkitekt-mesh/tree/main/crates/meshd) as a sidecar: a userspace
  tailnet node (our own client, below) that needs no root and runs next to a
  system tailscale. It joins
  the mesh once and keeps its node under `~/.local/state/arkitekt/mesh/`. It
  stops when the app exits.
* Aliases the server marks as mesh-only are challenged and used through the
  sidecar's local HTTP proxy. This covers GraphQL, the agent websocket and S3.
* `arkitekt-meshd` must be installed. Download it from the
  [arkitekt-mesh releases](https://github.com/arkitektio/arkitekt-mesh/releases)
  or run `cargo install arkitekt-meshd`. The app finds it through
  `ARKITEKT_MESHD`, next to its own executable, in `~/.local/share/arkitekt/bin`,
  or on `PATH`. The Python client runs the same binary, or the same node
  in-process (`pip install arkitekt-mesh`).

### Without the sidecar: `mesh-native`

With the `mesh-native` feature, `ARKITEKT_MESH=native` (or
`MeshOptions { backend: MeshBackend::Native, .. }`) runs the node inside the
app with [`arkitekt-mesh`](https://github.com/arkitektio/arkitekt-mesh), our
own Tailscale-compatible client (ts2021 control, WireGuard, DERP and disco
path discovery). You don't need a separate binary or a Go toolchain, and the
app gets the same local proxy.

The same limits apply to any new implementation:

* It has not been audited. The protocols are tested against tailscale's own Go
  implementation, and the cryptographic primitives come from the
  RustCrypto crates.
* It has no MagicDNS. Mesh hostnames are looked up in the node's netmap by
  hostname or FQDN.
* It dials out only. The node accepts no inbound TCP connections; a UDP
  socket it binds (`Node::bind_udp`) receives only what peers send to it.

Each backend keeps its own node state. The native node lives in
`…/mesh/<app>-native/` (`identity.json`). Switching backends therefore joins
the mesh as a new node, which needs a fresh mesh key: authorize again with
`no_cache`.

To check a real deployment, join its mesh with a key and reach a peer:

```sh
ARKITEKT_TEST_MESH_URL=https://… ARKITEKT_TEST_MESH_KEY=tskey-… \
ARKITEKT_TEST_MESH_PEER=http://<peer>/ \
  cargo test -p fakts --features mesh-native -- --ignored native
```

How the client works, what it lacks compared with tailscale (the RFCs), its
tests and the local mesh lab are documented in
[arkitekt-mesh](https://github.com/arkitektio/arkitekt-mesh). fakts' own
end-to-end test (`tests/native_mesh.rs`) runs the node against that
repository's Go harness: check it out next to this one, or set
`ARKITEKT_MESH_HARNESS` to its `crates/mesh/tests/harness`; without it (or
without Go) the test is skipped.

### WebRTC media over the mesh (LiveKit)

WebRTC media is UDP and cannot go through the HTTP proxy. With fakts'
`mesh-relay` feature, the native node offers two more ways in, without root
or a TUN device:

* `Fakts::mesh_forward(&alias)`: a local 127.0.0.1 port that forwards TCP to
  a mesh alias, e.g. for LiveKit's signaling websocket.
* `Fakts::mesh_turn()`: a TURN server on 127.0.0.1 whose allocations are UDP
  sockets on the mesh. Hand it to the WebRTC client as its only ICE server,
  with a relay-only transport policy. All media then goes through the relay
  and over the mesh to the SFU. The relay only reaches mesh peers.

In Python, `lovekit`'s `aconnect_room(token)` does both for a mesh-only
LiveKit alias, via `Fakts.amesh_forward`/`amesh_turn`. The SFU must advertise
its mesh address on its UDP port (LiveKit `rtc.node_ip` with
`rtc.udp_port`), and the mesh ACLs must let apps reach it on the signaling
(TCP) and media (UDP) ports.

If a proxy into the mesh is already running (e.g. `arkitekt mesh proxy`), set
`ARKITEKT_MESH_PROXY=http://localhost:1055` instead.

The mesh proxy is passed explicitly to each mesh alias. It is never read from
`HTTP_PROXY` or `ALL_PROXY`, and it is HTTP only, never SOCKS. Direct aliases
still follow `HTTP(S)_PROXY`/`ALL_PROXY`, but the datalayer cannot use a SOCKS
proxy. With `ALL_PROXY=socks5h://…` set for another tool, exclude the
deployment with `NO_PROXY`.

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
| `GET /journal/{session}`, `/journal/{session}/at/{pos}`, `/tasks/{id}/events` | the journal: every task event and state change in one order, and the app as of any position |
| `GET /schemas/…`, `/openapi.json`, `/docs` | the declaration and an API page |

* **Service connections:** an app without services never authenticates. One
  that uses services (e.g. mikro) authenticates for exactly those.
* **Auth:** `ServeOptions::auth(|request| ...)` protects `/assign` and the
  websocket.
* **Testing:** the `testing` feature adds `AgentTestClient`.
* **Journal:** send `{"type": "INIT", "journal": true}` on the websocket to get
  a `pos` on every frame and a snapshot consistent with it; add
  `"resume_after": N` after a reconnect to receive exactly what you missed.
  See [docs/journal.md](docs/journal.md).

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
example `~/.local/state/arkitekt/cache/*_fakts_cache.rs.json`. To skip the
interactive step, for instance in CI, a container or on a device, set
`FAKTS_MANDATE_KEY` to a mandate key (issued in kontrol, by a hub or by the app
that provisions the device): the app logs in as whoever granted it, and logs in
again whenever its access token runs out. `FAKTS_TOKEN=client_id:refresh_token`
still works too.

## Status

Implemented:

- [x] Fakts v2: discovery, device code, rotating refresh tokens, cache, and alias challenges (including Ed25519)
- [x] The headless mandate-key login (`FAKTS_MANDATE_KEY`, `Grant::MandateKey`), with a mesh key on the first login
- [x] GraphQL client with token refresh and task attribution
- [x] `#[action]` with typed ports, defaults, docs, structures, tuples, `Result` and generators
- [x] Rekuest agent: register, heartbeat, assign, cancel, interrupt, progress, log, reports acked by seq, reconnect with backoff
- [x] Mikro: create, fetch and read array datasets through zarr v3 on S3
- [x] States (JSON-patch published, with history), locks, pause/resume/step, startup/background/shutdown hooks
- [x] Serving over HTTP + websocket without a rekuest server, contract-tested against the Python implementation
- [x] A journal of task events and state changes in one order: resumable subscriptions, replay at any position
- [x] A local write-ahead journal: what the server has not acknowledged is re-sent after a restart
- [x] Memory structures (`Memory<T>`): kept in the agent's memory under ids it mints itself, no round trip
- [x] Durable-action groundwork: `task.now()`, `task.random_bytes(n)`, `task.sleep(d)` are recorded as effects
- [x] The recovery wire: `ASSIGN_REQUEST.call_key` and keyed `EFFECT` frames parse; actions register as `PLAIN` with no `effects` claim, so a Rust task whose agent dies ends `LOST`

Not implemented yet:

- [ ] GraphQL subscriptions over websocket
- [ ] Workflows (`WORKFLOW` actions that call other actions and are resumed from their journal), and an `effects` claim per action
- [ ] Structure-typed fields inside states
- [ ] Other mikro data types (tables, files, meshes, …) and multiscale pyramids
- [ ] Other services (unlok, fluss, kabinet, …)

## Targets

The SDK builds for Linux, macOS and Windows (x86-64 and ARM), and its agent
stack (`fakts`, `rath`, `rekuest`) for the ESP32-S3 (ESP-IDF std); CI checks
all of them (`checks/esp32s3` for the board). What a target lacks is behind
features:

* `ring` (`arkitekt` and `fakts`, on by default): ring as the TLS crypto
  provider. Without it, install a rustls provider before connecting, e.g.
  `rustls_rustcrypto::provider().install_default()` on ESP-IDF, where ring's C
  does not build.
* `signal` (`arkitekt`, on by default): stop on Ctrl-C.
* `user-dirs` (`arkitekt` and `fakts`, on by default): default places in the
  user's directories, for the fakts cache and the persisted device id.
  Without it (ESP-IDF has no users) give `ConnectOptions` a `store` and a
  `device_id`.
* `wal` (`arkitekt` and `rekuest`, on by default): the SQLite journal. Without
  it the journal stays in memory.

Each crate names only the tokio features it uses, so a firmware can run the
SDK on a current-thread runtime. Three traits take what a target does
differently:

* `fakts::FaktsStore` keeps the credential (a file by default, e.g. NVS on a
  board).
* `fakts::MeshJoiner` runs the app's own mesh node (`ConnectOptions::mesh_joiner`),
  where neither the sidecar nor the native backend's state directory fits:
  fakts asks for a mesh key at login, hands it over, and reaches mesh aliases
  through the proxy the joiner returns.
* `rekuest::Connector` opens the connection the agent's websocket runs over
  (plain TCP by default; `HttpProxy` through a mesh proxy). TLS runs on top
  of either.

The agent protocol itself does no I/O: `rekuest-session` is the `/agi`
session as a `no_std` state machine (handshake, heartbeats, backoff,
activation gating, the outbox that re-sends until `JOURNAL_ACK`), and
`rekuest-protocol` its frames, also `no_std`. `rekuest::Agent` drives it on
tokio; `checks/wasm` (wasm32, on node) and `checks/embassy` (on the host
and built for a Cortex-M4F) drive it with a minimal driver each, and CI runs
the same scripted scenario on all of them.

**Spec and runtime.** As in Python, what an app *is* and what *runs* it are
separate crates. `arkitekt-spec` holds the action language (definitions,
ports: the same types a server validates) and the declaration (`App`,
`#[action]`, `Registry`, states, hooks, `Service`), and `Task`, which an
action reports through and a runtime implements (`TaskBackend`). rekuest is
that runtime: its executor runs each task as an `AgentTask`, and its journal
and state hub number what they report. A server takes `arkitekt-spec`
without its `declare` feature: only the wire types.

Above the session, rekuest names no runtime either: the executor, journal,
states and `#[action]` code spawn, sleep and wait through `arkitekt_spec::rt`
(`Runtime`: spawn, sleep, blocking work), which is tokio with the `tokio`
feature and whatever the app installs (`rt::install`) without it.
`rekuest::Driver` is an agent without its socket (session + executor +
journal), and `rekuest::link::run` keeps it connected over any websocket:
a `Dial` opens a cancel-safe `Socket`, an optional `Backlog` keeps the
journal beyond memory. Every agent is that loop: `Agent` with tungstenite and
the SQLite journal, `web::Agent` with the JS `WebSocket`, arkitekt-embassy's
with its own client over embassy-net; `checks/wasm` runs a real `#[action]`
through the driver on wasm32. Features of `rekuest`: `tokio` (the
`Agent`, `transport`, fakts tokens) and `rath` (task tokens on GraphQL calls),
both on by default and turned on by `arkitekt`; `wal` needs tokio. Serving an
app over HTTP without a rekuest server is `arkitekt-serve` (Python's
arkitekt-fastapi), on rekuest's executor and journal.
Action futures must be `Send` everywhere, so a browser action that holds a JS
value across an `.await` wraps it (e.g. `send_wrapper`); wasm has one thread.

In a browser or node (wasm32), `rekuest`'s `web` feature has `web::Agent`:
the same driver over the JS `WebSocket`, on the JS event loop (`rt::Js`),
with tokens from `fakts`, which logs in over `fetch` there (as `rath` makes
its requests); there is no cache file and no mesh. The e2e job runs it under
node against the real stack (`checks/wasm/tests/real_stack.rs`). Node does
not enforce CORS; a browser needs lok and takt to allow its origin.
`arkitekt-web` is the browser's platform: `arkitekt_web::run(app, options)`.

Without its `std` feature (on by default; `tokio`, `web`, `rath`, `wal` and
`serve` imply it) rekuest is `no_std` + `alloc`, for a microcontroller: locks
spin, maps are hashbrown's, the task local is a global slot (one executor
thread), a panicking action aborts instead of failing its task, and the
runtime brings what std would (`Runtime::wall_clock_micros`,
`Runtime::fill_random`). `checks/embassy` runs a real `#[action]` through
`Driver` on an embassy runtime that way, on the host and built for a Cortex-M4F.

fakts and rath have the same `std` feature. Without it, fakts' login
(discovery, grants, refresh, alias challenges) runs against a `fakts::Host`
the app implements (HTTP exchanges, sleep, the clock, randomness; with std
that is `fakts::http::Reqwest`), and rath sends through the same host; typed
`graphql_client` operations stay std-only (`execute_raw` works everywhere).
An embassy app implements `fakts::Host` and `arkitekt_spec::rt::Runtime` on
one struct.

`arkitekt-embassy` (no_std) is that for embassy:
`Embassy` (the runtime; the board's RNG and clock come in through it),
`NetHost` + `serve_http` (fakts' and rath's host: a small HTTP/1.1 client
over embedded-nal-async, one network task, so the futures are `Send` although
embassy-net's sockets are not) and `Agent` (the driver over its own websocket
client), and `Board`, the platform: `Runtime::connect(Board { host, tcp, dns,
verify }, app, options)` connects the same `App` as on the desktop, and
`serve()` runs the agent. `https://` and `wss://` are TLS 1.3 or 1.2
(rustls's unbuffered client, RustCrypto), the server's chain and name checked
against a CA the app gives (`Verify::Ca`, the root, DER). The e2e job runs it on
the host against the real stack (`checks/embassy`'s `real_stack`, over async-io TCP): the app connected on
`Board`, rath (over
https) and the agent (over wss), verified against the test gateway's internal
CA, and a wrong name refused; the board wiring over embassy-net builds for a
Cortex-M4F.

## Development

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features
```

The served-agent contract is recorded from the Python implementation
(`crates/rekuest/tests/fixtures/serve/`). Run `record.sh python` to re-record
it. To check a running Rust server against it, run `record.sh rust <url>`
followed by `compare.py python rust`.
