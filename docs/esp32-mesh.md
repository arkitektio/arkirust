# The mesh on ESP32

This spike asks whether an ESP32 can join the deployment's mesh from Rust, using the same code as the `mesh-native` backend: our own client, [`arkitekt-mesh`](../crates/mesh).

The spike lives in [`spikes/esp32-mesh/`](../spikes/esp32-mesh). It is outside the workspace and is not published.

## Result so far

**`arkitekt-mesh` type-checks for ESP32-C6 (`riscv32imac-esp-espidf`, ESP-IDF std) with no patches.** That covers both:

- the sans-IO core (`default-features = false`): keys, the control Noise handshake, WireGuard, the DERP/disco/STUN codecs, path selection and the smoltcp TCP stack;
- the tokio driver (`features = ["tokio"]`, without the default `ring`), including joining, dialing and TLS through a pure-Rust provider (`rustls-rustcrypto`).

It has **not been linked against ESP-IDF or run on a board**. So flash and RAM size, and whether it works at runtime, are still open.

```sh
cd spikes/esp32-mesh
cargo check     # nightly + build-std, from rust-toolchain.toml / .cargo/config.toml
```

## What made it portable

- **The core is sans-IO.** It takes bytes and time and returns bytes, with no tokio and no sockets. A board without tokio (an embassy or bare ESP-IDF event loop) can drive it directly.
- **The TLS provider is the app's choice.** The driver uses the process-wide rustls provider. `ring`, whose C does not build for ESP-IDF, is only a default fallback feature.
- **The driver's tokio features are minimal:** `rt`, `net`, `time`, `sync`, `macros`, `io-util`. It does not use `signal`, `process` or `fs`, which have no ESP-IDF support.

## Memory (measured on the host)

`cargo test -p arkitekt-mesh --release --test memory -- --nocapture` joins a
test tailnet with a counting allocator. The rows below use `Limits::small()`,
the preset for boards. The heap figures are the same on any 64-bit host. On
the ESP32's 32-bit target, pointers and sizes are half as wide, so expect
somewhat less there.

| peers | start peak | idle | during a transfer |
|---:|---:|---:|---:|
| 1 | 134 KiB | 126 KiB | 71 KiB above idle, of which about 48 KiB are the test's own buffers |
| 101 | 264 KiB | 184 KiB | the same |
| 501 | 515 KiB | 298 KiB | the same |
| 1001 | 821 KiB | 451 KiB | the same |

- **Per peer:** about 330 bytes while idle. The netmap keeps only what routing,
  naming and path discovery need. WireGuard and path state exist only for
  peers in use.
- **Start peak:** the map response is streamed. Each peer is compacted as its
  JSON arrives, and unused members such as `UserProfiles` and packet filters
  are skipped unbuffered. The frame is never held whole, unlike microlink's
  512 KB HTTP/2 and JSON buffers. What remains at 1,000 peers is the old and
  new peer lists during a full replace. After the first map, control sends
  deltas.
- **TLS:** about 27 KiB per connection with rustls. A node holds two, one to
  control and one to its home DERP.
- **Regression budgets:** the test fails at 700 bytes per idle peer or more,
  and with `Limits::small()` at 128 KiB transfer peak or 192 KiB idle.

## Open questions

These can only be answered by linking and then running on a board.

- **Heap on the board.** The host numbers suggest the fit:
  - An ESP32-C6 (512 KB SRAM, no PSRAM) should hold a node with a few dozen
    peers under `Limits::small()`.
  - An ESP32-S3 or ESP32-P4 with PSRAM handles large tailnets.

  Still to measure on the chip: tokio's own footprint and task stacks, and
  what `rustls-rustcrypto` allocates.
- **tokio on ESP-IDF.** The eventfd VFS must be registered before tokio starts (`esp_idf_svc::io::vfs::initialize_eventfd`), and the pthread stacks need to be larger.
- **Keys.** `NodeIdentity` is plain serde. A board would keep it in NVS rather than a file.
- **Crypto maturity.** `rustls-rustcrypto` is an alpha release, and `arkitekt-mesh` itself has not been audited.
- **The rest of arkitekt.** reqwest, tungstenite, rusqlite and zarrs are all out of scope here. An ESP32 agent would be a thin client on `Node::dial`, not the full `arkitekt` crate.

## Next steps

1. Add `esp-idf-svc` and a `main` that brings up Wi-Fi and then calls `join_and_get`. Build it with `ldproxy`/`espflash` for an S3 or P4 with PSRAM, and record the flash and heap figures.
   - Build from `spikes/` on disk, not tmpfs: ESP-IDF plus `.embuild` takes several GB.
2. Run it against ionscale with an auth key, and dial a known peer.
3. If the heap is too tight, make the buffers configurable, and cap the netmap to the peers the board talks to.
