# CHANGELOG


## v0.3.1 (2026-10-07)

### Bug Fixes

- Attach the arkitekt-meshd binaries to every release
  ([`6cb0b0e`](https://github.com/arkitektio/arkirust/commit/6cb0b0ede44de41cbfa5fabdb0e505f4a4714a19))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_01GwdBkEZRCRUEgLvWV2pvDL

### Continuous Integration

- **release**: Semantic-release decides the version, and a release publishes everything
  ([`b4c03f1`](https://github.com/arkitektio/arkirust/commit/b4c03f1310baa36636097696839b3258690dc504))

The release PR never worked here, so versions were bumped by hand. A push to main is now the
  release: once CI is green, semantic-release bumps the workspace version from the commits, tags and
  creates the GitHub release, release-plz publishes the crates, and release.yaml (PyPI) and
  meshd.yml (binaries, onto the same GitHub release) are started on the tag.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_01GwdBkEZRCRUEgLvWV2pvDL


## v0.3.0 (2026-10-07)

### Chores

- Release v0.3.0
  ([`1c0ce76`](https://github.com/arkitektio/arkirust/commit/1c0ce76174da7dd6836c1cfb2653a5dbe95e4f05))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

- Stop tracking Python bytecode
  ([`9c57a1c`](https://github.com/arkitektio/arkirust/commit/9c57a1c8beae8960943a23448da7125d4b1254b8))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

### Continuous Integration

- **release**: Publish arkitekt-mesh to PyPI with every release
  ([`6e2487d`](https://github.com/arkitektio/arkirust/commit/6e2487d795d2991745e4615d27e9338f93ae4194))

release-plz.yml starts release.yaml on the release's vX.Y.Z tag once the crates are published: the
  tag itself starts nothing, as it is pushed with GITHUB_TOKEN. release.yaml publishes on any tag
  that carries the workspace version, and on a branch stays a dry run.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

### Features

- **mesh**: Throughput and first-connection fixes, with a bench to measure them
  ([`4bad474`](https://github.com/arkitektio/arkirust/commit/4bad474b7cc15a3ffcf875e43068b3ba7283479d))

Five causes, each measured with the new tests/bench.rs (docs/rfc7, "Results"):

- A connection had 64 KiB in flight per round trip. Limits::tcp_buffer is 1 MiB now, settable as
  --tcp-buffer on meshd, tcp_buffer= in the Python bindings and MeshOptions.tcp_buffer. - Downloads
  overflowed the kernel's UDP receive buffer: udp_buffer is 4 MiB and the node receives 64 datagrams
  per batch. - DERP uploads stalled on silent local queue drops and the relay's 32-packet client
  queue: the netstack is back-pressured and data frames are paced (derp_burst); Node::derp_dropped
  counts what was lost. - The first connection took 5 to 10 s, because a WireGuard initiation was
  only retried after 5 s: it is retried early (250 ms, 500 ms, 1 s, ...), and a session binds its
  last UDP port again after a restart. - The proxy dialed once per http:// request: it keeps a
  pooled client per host.

The mesh lab's peer can shape its link (lab.sh netem), which is what shows a window-limited
  connection at all.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>


## v0.2.0 (2026-09-30)

### Bug Fixes

- **mikro**: Keep allow_http when setting datalayer proxy options
  ([`66d2635`](https://github.com/arkitektio/arkirust/commit/66d263515a3af82f80e5b2584d893486037d3b63))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

### Code Style

- Cargo fmt
  ([`5b958f6`](https://github.com/arkitektio/arkirust/commit/5b958f6fe5c4c3e5df3fe028368136d156c2f0d5))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

### Continuous Integration

- Clang 21 for webrtc-sys
  ([`dddada0`](https://github.com/arkitektio/arkirust/commit/dddada04759c18d19b13008fdb93ce297af20378))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- Glib and X11 headers for webrtc-sys
  ([`17b9289`](https://github.com/arkitektio/arkirust/commit/17b928926d47b549cf5e47e8c52022acd6a36c55))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- Mesh workflow on Linux, macOS and Windows; clang for livekit
  ([`06e7e9f`](https://github.com/arkitektio/arkirust/commit/06e7e9fc889fc8c2b61f34e699b60d2adc7cdb43))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- **meshd**: Define __ARM_ARCH for ring in aarch64 manylinux wheels
  ([`e56d616`](https://github.com/arkitektio/arkirust/commit/e56d616809831271e938a9e055a7b1b293a86775))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- **meshd**: Sign and notarize macOS binaries, smoke-test, manual runs
  ([`3e652f0`](https://github.com/arkitektio/arkirust/commit/3e652f05e63137ec3239b48858201302a00e4c53))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- **release**: Publish arkitekt-mesh to PyPI from release.yaml
  ([`4355868`](https://github.com/arkitektio/arkirust/commit/4355868e5c9cc7ec5c684adaadc7815d2cdd881d))

Only the in-process bindings go to PyPI: Python fakts runs the node with arkitekt-mesh, so the
  arkitekt-meshd wheels (crates/meshd/python) are gone; the binary stays on the GitHub release
  (meshd.yml).

release.yaml, on a mesh-vX.Y.Z tag: tests, abi3 wheels for Linux (glibc, musl), macOS universal2 and
  Windows (x64, arm64), plus an sdist, each with LICENSE and LICENSE-THIRD-PARTY; every wheel
  installed and run on its platform (architecture asserted) and the sdist checked and built from
  source, before trusted publishing without an environment.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

### Documentation

- Mesh RFCs, ESP32 notes, README
  ([`3a35843`](https://github.com/arkitektio/arkirust/commit/3a358435ab7aba21ff3feeee7932a72d3f0c4089))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- Rfc-6 status after the first cross-platform CI runs
  ([`cb1fbff`](https://github.com/arkitektio/arkirust/commit/cb1fbff424420bbcff96c59ffb042699fd9854bf))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- The recovery wire the Rust agent speaks, and workflows as not yet implemented
  ([`1cc57eb`](https://github.com/arkitektio/arkirust/commit/1cc57eb365a9f0c3be241b64335a7cac42b66220))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

### Features

- #[service] builders and mesh routing through a tailscale sidecar
  ([`2ec8cee`](https://github.com/arkitektio/arkirust/commit/2ec8ceede86811cc6d8da2842821d9ed42848e69))

Services are declared like Python's `@registry.service`: a builder function whose
  `#[require(service, description)]` parameters are the aliases it needs (`Option<Alias>` for
  optional ones), plus an optional `Fakts`. `#[arkitekt::service]` turns it into a `Service`; mikro
  uses it (`App::service(mikro::service)`).

Aliases the server marks as `kind: "mesh"` (or with a 100.64.0.0/10 host) are reached through an
  HTTP proxy into the deployment's mesh: - fakts challenges them through the proxy and stamps it on
  the Alias; `Alias::http_client`, `Rath::from_alias`, `DataLayer::from_alias` and the agent
  websocket (HTTP CONNECT tunnel) all use it. - With the `mesh` feature (`ConnectOptions::mesh`, or
  `ARKITEKT_MESH=1`), the app asks for a mesh key on authorization, keeps it across refreshes, and
  runs a userspace tailscaled sidecar joined once per identity. - `ARKITEKT_MESH_PROXY` /
  `mesh_proxy` reuse an already running proxy.

BREAKING CHANGE: `MikroService` is gone (`mikro::service` is now a unit struct, not a function);
  `Alias`, `AgentOptions`, `DeviceCodeOptions`, `SelfFakt`, `WellKnown`, `TokenResponse` and
  `ActiveFakts` gained public fields.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

- Parse call_key on ASSIGN_REQUEST and key on EFFECT
  ([`e7e00d8`](https://github.com/arkitektio/arkirust/commit/e7e00d884e86fe8e0c9eb95f7191495369d59bee))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

- Rekuest-protocol: every agent-protocol frame, for agents and servers
  ([`2092a3a`](https://github.com/arkitektio/arkirust/commit/2092a3a587b94443fa97f640d3b97111469839ae))

The wire types move out of rekuest into their own crate, and grow to the whole protocol the Python
  server speaks: the agent's requests (probe, state revision, cancel/interrupt/ pause/resume), every
  reply, the 19 execution-event mirrors, a workflow's resume journal, paused details, and the
  RECORD/HOLD effects. FromAgent and Envelope are generic over what REGISTER declares (the agent's
  own AgentDeclaration by default). The contract is the server-generated fixture with every field of
  every frame set; all 66 round-trip.

UNLOCK's task and SHELVE/UNSHELVE's ref are optional, as in Python. The workspace moves to 0.2.0 in
  lockstep, so the arkitekt that has Alias is published before arkitekt-mikro.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

- Serve an implementation's effects, execution and codeHash
  ([`23303f5`](https://github.com/arkitektio/arkirust/commit/23303f5fe1b84cfae2a666c10a39f1f09bda41ea))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Follows the rekuest server, which replaced effect with these three.

- **arkitekt**: Mesh-relay feature
  ([`f56e0b4`](https://github.com/arkitektio/arkirust/commit/f56e0b4def55ec61e1af7947832654c4063a21a9))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- **fakts**: Native mesh backend on the shared session, TURN and forwards
  ([`1dd5d1e`](https://github.com/arkitektio/arkirust/commit/1dd5d1e7b933a43473438541f317a60496b71c4a))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- **firmware**: Esp32 mesh firmware and C6 spike
  ([`0b76fd6`](https://github.com/arkitektio/arkirust/commit/0b76fd6896914cffdc86e2731c91a62360865d59))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- **lovekit**: Lovekit client with LiveKit rooms over the mesh
  ([`ccfab6a`](https://github.com/arkitektio/arkirust/commit/ccfab6a73d3d57623ff0c832ccf5fda07bbb5640))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- **mesh**: Our own Tailscale-compatible client (arkitekt-mesh)
  ([`9ca64be`](https://github.com/arkitektio/arkirust/commit/9ca64be404632a225a20474d7d9ab2d1ac8ecb59))

UDP sockets, TURN relay and TCP forwards, sessions with a local proxy, packet filter (RFC-8), DERP
  home by latency (RFC-4), IPv6 underlay (RFC-2), PCP/NAT-PMP port mapping (RFC-3) and tailnet lock
  (RFC-5).

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- **mesh-py**: Python bindings for the mesh node (arkitekt-mesh on PyPI)
  ([`3fcecc7`](https://github.com/arkitektio/arkirust/commit/3fcecc79d3429b997fd472a73e6a9db33d286a51))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- **meshd**: Arkitekt-meshd in Rust, replacing the Go tsnet sidecar (RFC-1)
  ([`19969b4`](https://github.com/arkitektio/arkirust/commit/19969b414841f70383cc3f39ccf0a5a26f60c435))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- **rekuest**: Journal, shelf and write-ahead log for agent state
  ([`d75c609`](https://github.com/arkitektio/arkirust/commit/d75c609716271e005e62b8774b038c989cf6a509))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

- **rekuest**: Speak the agent-report contract
  ([`20d71dc`](https://github.com/arkitektio/arkirust/commit/20d71dc122d7ff7d046c4f9368d4e6a30fe229b2))

The Rust agent now matches the rekuest server 4 contract (docs/design/journal.md in the server;
  tests/fixtures/agent_wire.json is the shared set of canonical frames, round-tripped in
  tests/wire.rs):

- One EFFECT frame (NOW, RANDOM, SLEEP) replaces the journal-only ASSIGN echo and NOW/RANDOM/SLEEP
  kinds; Task::now/random/sleep record through one seam. - Probe task reports (p- ids) are never
  numbered or retained; a probe's state patch is, without a step. A report on a task this process
  never ran carries pos but no task_step. - UNLOCK and SHELVE name their task; ASSIGN_REQUEST
  carries parent_step and an optional reference. - Everything numbered is retired by JOURNAL_ACK
  alone; EVENT_ACK is ignored and the INIT journal gate is gone. - Fixes: a task's UNLOCKs always
  follow its terminal report (locks are released by whoever records it, not the aborted future);
  PAUSE/RESUME for an ended task records nothing; the SQLite writer retries a failed write instead
  of skipping it; an overflowing outbox reloads from the local journal instead of losing frames;
  resume_after 0 with a foreign session resyncs.

BREAKING CHANGE: FromAgent loses Assign/Now/Random/Sleep for Effect, Unlock gains task,
  AssignRequest is new; Outbox::set_journal and random_bytes are removed.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_01VdhhWgpfFEL4VxYhxnD5oj

### Refactoring

- Move the ESP32 firmware to arkitekt-mesh-esp32
  ([`9b7e3a7`](https://github.com/arkitektio/arkirust/commit/9b7e3a7ae41ed1970186f4badd73f61059018815))

The firmware and the ESP32-C6 check now live in github.com/arkitektio/arkitekt-mesh-esp32 and depend
  on arkitekt-mesh by git. CI here checks the feature set they build (mesh-embedded).

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1

### Testing

- Local mesh lab against our ionskale fork
  ([`511f691`](https://github.com/arkitektio/arkirust/commit/511f69106e3a969d8f4b7b5a54ece2d27598c8aa))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

Claude-Session: https://claude.ai/code/session_011edrvdGLpPzRexTAFMp1v1


## v0.1.0 (2026-09-27)

### Bug Fixes

- Publish
  ([`6c1e426`](https://github.com/arkitektio/arkirust/commit/6c1e4269b613eee276b2021c3ad9884571e05948))

### Chores

- Move repository to arkitektio org
  ([`b9f3f13`](https://github.com/arkitektio/arkirust/commit/b9f3f1372faf5066fd20246c8ff1a3aaec402a69))

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

### Features

- Split into publishable workspace crates with lockstep releases
  ([`b8ac7c8`](https://github.com/arkitektio/arkirust/commit/b8ac7c8d4a4b2ca155cf0a026f799216853f0d9c))

Restructure into fakts, rath, rekuest, rekuest-macros, arkitekt and mikro crates. Publish rath/mikro
  as arkitekt-rath/arkitekt-mikro (names taken on crates.io) while keeping their lib names. Add
  release-plz config for lockstep semantic releases and CI/release workflows on main.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>

- With state and the webserver
  ([`818df34`](https://github.com/arkitektio/arkirust/commit/818df348fef19bb296465b67bf50ccfe1f2d043f))
