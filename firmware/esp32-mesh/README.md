# esp32-mesh

Firmware that puts an ESP32 or ESP32-S3 on the mesh with [`arkitekt-mesh`](../../crates/mesh). It:

1. brings Wi-Fi up;
2. joins the mesh with a node identity kept in NVS;
3. fetches `http://<peer>/` every 10 s, logging whether the path is direct or via DERP and the heap figures (free, lowest ever, largest block).

It is not part of the workspace and is not published.

## Setup (once)

```sh
cargo install espup espflash ldproxy     # espflash may need a newer rustc: cargo +nightly install espflash
espup install --targets esp32,esp32s3    # Espressif's Xtensa Rust toolchain
```

## Settings

Copy `mesh.env.example` to `mesh.env`, which is gitignored and read at build time. Fill in:

- the Wi-Fi network;
- the control URL, and an auth key for the first join;
- the peer to fetch from.

To test against the Go harness from this repository, run it bound to this machine's address, which the board must be able to reach:

```sh
cd crates/mesh/tests/harness && go test -c -o /tmp/mesh-harness .
sleep infinity | MESH_HARNESS=1 MESH_HARNESS_ADDR=<this machine's LAN IP> /tmp/mesh-harness -test.run TestHarness
```

It prints `control_url`, `auth_key` and `peer_name` for `mesh.env`.

## Build and flash

```sh
. ~/export-esp.sh
cargo build --release                                          # ESP32 (the default target)
MCU=esp32s3 cargo build --release --target xtensa-esp32s3-espidf
espflash flash --monitor --partition-table partitions.csv target/xtensa-esp32-espidf/release/esp32-mesh
```

The first build downloads ESP-IDF into `.embuild/`, which takes several GB.

## Memory

The node runs with `Limits::small()`. `sdkconfig.defaults` does three things for memory:

- enables PSRAM when present (S3 dev kits have it);
- trims Wi-Fi buffers;
- raises task stacks for Rust.

Host measurements are in [docs/esp32-mesh.md](../../docs/esp32-mesh.md). The heap logs from the board are the real figures.
