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
| [`rekuest`](crates/rekuest)           | `rekuest`           | Ports, structures, the `/agi` websocket agent, and `Task`                                         |
| [`rekuest-macros`](crates/rekuest-macros) | `@register`     | `#[action]`: turns a function into an action definition                                          |
| [`arkitekt`](crates/arkitekt)         | `arkitekt`          | `App`, `Service`, `Runtime`; re-exports everything above                                          |
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

All crates are released in lockstep from Conventional Commits via
[release-plz](https://release-plz.dev) (see `release-plz.toml`).

## Concepts

**App.** `App::new(identifier, version)` declares who the app is, which
services it uses and which actions it offers. The fakts manifest is derived
from that declaration: each service contributes its requirements, and apps
that offer actions also require `rekuest`.

**Services.** A service is a builder function, as with Python's
`@registry.service`. Each `#[require]` parameter is a fakts requirement keyed by
its name, resolved to an `Alias` before the body runs:

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
* The app then runs [`arkitekt-meshd`](crates/meshd/) as a sidecar: a userspace
  tailnet node (our own client, below) that needs no root and runs next to a
  system tailscale. It joins
  the mesh once and keeps its node under `~/.local/state/arkitekt/mesh/`. It
  stops when the app exits.
* Aliases the server marks as mesh-only are challenged and used through the
  sidecar's local HTTP proxy. This covers GraphQL, the agent websocket and S3.
* `arkitekt-meshd` must be installed. Download it from the `meshd-v*` GitHub
  releases or run `pip install arkitekt-meshd`. The app finds it through
  `ARKITEKT_MESHD`, next to its own executable, in `~/.local/share/arkitekt/bin`,
  or on `PATH`. The Python client runs the same binary, or the same node
  in-process (`pip install arkitekt-mesh`).

### Without the sidecar: `mesh-native`

With the `mesh-native` feature, `ARKITEKT_MESH=native` (or
`MeshOptions { backend: MeshBackend::Native, .. }`) runs the node inside the
app with [`arkitekt-mesh`](crates/mesh/). This is our own Tailscale-compatible
client, written from the protocol up. It speaks:

* ts2021 control (Noise, then HTTP/2), the protocol ionscale, headscale and
  Tailscale serve. It is tested against tailscale's own test control server,
  and against our ionskale fork in the local mesh lab (`testing/mesh-lab`),
* WireGuard,
* DERP relays,
* disco path discovery with STUN, so peers get direct UDP paths when the
  network allows and relay through DERP when it doesn't.

You don't need a separate binary or a Go toolchain, and the app gets the same
local proxy.

The same limits apply to any new implementation:

* It has not been audited. The protocols are tested against tailscale's own Go
  implementation (see below), and the cryptographic primitives come from the
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

The crate's end-to-end tests (`cargo test -p arkitekt-mesh`) run it against
tailscale's test control server, a DERP/STUN server and a `tsnet` peer. They
build these from `crates/mesh/tests/harness` and need Go; without Go they are
skipped.

For running the same stack on ESP32, see [docs/esp32-mesh.md](docs/esp32-mesh.md).

Go appears only in the tests, as the reference tailscale the client is
checked against. The client used to run as a Go tsnet sidecar; that build is
gone (see [RFC-1](docs/rfc1-rust-only-mesh.md)). What the Rust node lacked compared with tsnet
is tracked as RFCs, each listing what is done and what is not:

| RFC | What | Status |
|---|---|---|
| [RFC-2](docs/rfc2-ipv6-underlay.md) | Direct paths over IPv6 | Implemented |
| [RFC-3](docs/rfc3-nat-port-mapping.md) | NAT port mapping | PCP and NAT-PMP; no UPnP |
| [RFC-4](docs/rfc4-derp-home-by-latency.md) | Home DERP region by measured latency | Implemented |
| [RFC-5](docs/rfc5-tailnet-lock.md) | Tailnet lock | Verifying node; no signing or fork resolution |
| [RFC-6](docs/rfc6-cross-platform.md) | macOS and Windows | Tests green on all three; release signing open |
| [RFC-7](docs/rfc7-production-and-throughput.md) | Production and throughput | Proposed |
| [RFC-8](docs/rfc8-packet-filter.md) | Enforcing the tailnet's ACLs | Implemented |

The same node runs:

* **as the sidecar:** [`crates/meshd`](crates/meshd/) is `arkitekt-meshd`
  (`--turn` and `--forward` also expose the relay and forwards).
* **in Python:** [`crates/mesh-py`](crates/mesh-py/) is the `arkitekt-mesh`
  package on PyPI. With it installed, Python fakts runs the node in-process
  (`MeshOptions.backend`, `ARKITEKT_MESH=native`).

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
- [x] A journal of task events and state changes in one order: resumable subscriptions, replay at any position
- [x] A local write-ahead journal: what the server has not acknowledged is re-sent after a restart
- [x] Memory structures (`Memory<T>`): kept in the agent's memory under ids it mints itself, no round trip
- [x] Durable-action groundwork: `task.now()`, `task.random_bytes(n)`, `task.sleep(d)` are recorded as effects

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
