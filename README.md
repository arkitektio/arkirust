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

## Running

```sh
# connects to $FAKTS_URL, or the public deployment at https://go.arkitekt.live
cargo run -p arkitekt-examples --bin hello
cargo run -p arkitekt-examples --bin upload_demo    # action: upload a fake image to mikro
cargo run -p arkitekt-examples --bin create_image
cargo run -p arkitekt-examples --bin script         # services only, no agent
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

Not implemented yet:

- [ ] GraphQL subscriptions over websocket
- [ ] Agent state, locks, memory structures, pause/resume, calling other actions
- [ ] The redeem grant for headless deployment
- [ ] Other mikro data types (tables, files, meshes, …) and multiscale pyramids
- [ ] Other services (unlok, fluss, kabinet, …)

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```
