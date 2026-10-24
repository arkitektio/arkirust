# The test stack

A real lok and rekuest (with takt), plus their database, redis and RustFS,
for the SDK's end-to-end tests: nothing stands in for a server. Taken from the
Python `fakts` package's integration stack (`tests/integration`).

`configs/lok.yaml` is a template: the harness picks free host ports, writes
them in (`__LOK_HOST_PORT__`, `__REKUEST_HOST_PORT__`), and mounts the result
as `LOK_CONFIG_FILE`. lok is seeded with the user `demo` in the organization
`demo`, a hub `localhost` whose rekuest the client challenges for real, and a
mandate key (`mandate_keys` in the config).

```sh
cargo test -p fakts --test real_lok -- --ignored --nocapture
```

`LOK_SERVICE_TAG` / `REKUEST_SERVICE_TAG` pick other images (default
`latest`), e.g. a local build of an unreleased server.

Every key in these configs is throwaway material for this stack only.
