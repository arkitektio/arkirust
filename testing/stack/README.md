# The test stack

A real lok and rekuest (with takt behind a Caddy gateway that sends `/agi`
to takt, as a hub's does, over http and over https for `localhost` with
Caddy's internal CA: `Stack::tls_root`), plus their database, redis and RustFS,
for the SDK's end-to-end tests: nothing stands in for a server. Taken from the
Python `fakts` package's integration stack (`tests/integration`).

`configs/lok.yaml` and `configs/rekuest.yaml` are templates: the harness picks
free host ports, writes them in (`__LOK_HOST_PORT__`, `__REKUEST_HOST_PORT__`),
and mounts the results as `LOK_CONFIG_FILE` and `REKUEST_CONFIG_FILE` (rekuest
trusts the tokens lok issues under its run's address). lok is seeded with the user `demo` in the organization
`demo`, a hub `localhost` whose rekuest the client challenges for real, and a
mandate key (`mandate_keys` in the config).

```sh
cargo test -p fakts --test real_lok -- --ignored --nocapture
cargo test -p rekuest --test real_rekuest -- --ignored --nocapture
```

Both include `harness.rs` (start, wait for health, logs on failure, tear down).

`LOK_SERVICE_TAG` / `REKUEST_SERVICE_TAG` pick other images (default
`latest`), e.g. a local build of an unreleased server.

Every key in these configs is throwaway material for this stack only.
