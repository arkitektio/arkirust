# CHANGELOG


## v0.8.0 (2026-10-24)

### Chores

- Move the mesh to arkitektio/arkitekt-mesh
  ([`d496d15`](https://github.com/arkitektio/arkirust/commit/d496d1590cc374f43d4c3a07607e470b23efa130))

arkitekt-mesh, arkitekt-meshd and the Python bindings now live (with their history) in their own
  repository and are released from there. The workspace depends on the published arkitekt-mesh 0.7;
  fakts, arkitekt and lovekit keep their mesh features unchanged.

fakts' native mesh test runs the mesh's Go harness from a checkout of that repository
  (ARKITEKT_MESH_HARNESS, or a sibling checkout); CI checks it out. The PyPI and meshd release
  workflows, the mesh-only CI jobs, the RFCs and the mesh lab moved along.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

### Documentation

- Arkitekt-meshd is not on PyPI; point to the releases and cargo install
  ([`367e832`](https://github.com/arkitektio/arkirust/commit/367e832a86cfcf48ecfd814b49cf483d5237ba32))

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

### Features

- Build the agent stack for the ESP32-S3
  ([`6e601f7`](https://github.com/arkitektio/arkirust/commit/6e601f726a902c347cfba2c760da87d266b1273c))

Each crate names only the tokio features it uses (the workspace sets none), the TLS crypto provider
  is a feature instead of hard-wired ring, and Ctrl-C handling is behind arkitekt's `signal`
  feature. fakts, rath and rekuest with default features off now compile for xtensa-esp32s3-espidf;
  checks/esp32s3 and a CI job keep it that way, and the platform job checks the whole workspace on
  every desktop OS.

BREAKING CHANGE: fakts no longer installs ring unless its `ring` feature is on (arkitekt turns it on
  by default; rath and rekuest leave the choice to the application). Code that builds a reqwest
  client of its own must install a provider first (fakts::install_crypto_provider).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

- **fakts**: Log in with a mandate key
  ([`6f4431e`](https://github.com/arkitektio/arkirust/commit/6f4431ee094e7474558e503a17786f47d74ce1cd))

Grant::MandateKey (or FAKTS_MANDATE_KEY) logs the app in headless, as whoever granted the mandate,
  with lok's urn:fakts:grant-type:mandate-key grant. There is no refresh token: renewing logs in
  again. With the mesh on, the first login asks for a mesh key and renewals never do, so the node
  keeps the one it joined with. A refused key is an InvalidGrant, and the key itself is never cached
  or logged.

BREAKING CHANGE: AuthFakt.refresh_token is an Option (None after a mandate-key login), and
  ClientKind gains Mobile.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

### Testing

- **fakts**: Log in with a mandate key at a real lok
  ([`5d4d5a1`](https://github.com/arkitektio/arkirust/commit/5d4d5a16a6677e87c293374aa0279a1096cf2187))

testing/stack is the Python fakts package's integration stack (lok, rekuest, takt, their database,
  redis and RustFS); the test renders lok's config with free host ports, starts the stack under its
  own compose project and tears it down with its volumes. The key lok's config seeds logs in, the
  hub's rekuest answers its challenge for real, renewing is the same client, and an unknown key is
  refused. Opt-in (--ignored) locally; its own CI job, which gates the release.

The esp32s3 job installs its toolchain from the repository root: the check's rust-toolchain.toml
  names the toolchain that step installs.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

### Breaking Changes

- **fakts**: Authfakt.refresh_token is an Option (None after a mandate-key login), and ClientKind
  gains Mobile.


## v0.7.0 (2026-10-23)

### Features

- **rekuest-protocol**: Protocol hints on an action definition
  ([`4218c4f`](https://github.com/arkitektio/arkirust/commit/4218c4f485e519a7e1e1a71fcc0ef0e13c7529dc))

`Definition.protocols` names where an action means to hook in (`on_scene_annotation`): hints a
  caller filters on, never a contract, and not part of the action's hash. Left off the wire when
  empty, so a definition without hints is what a server that predates them accepts.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>


## v0.6.0 (2026-10-23)

### Features

- **rekuest-protocol**: Task bloks on the agent wire
  ([`b4f5777`](https://github.com/arkitektio/arkirust/commit/b4f5777090ea1c1fcd0523c42c1d2a467b9f89a8))

A task can show a person a blok made on demand: with its own hold (`HELD` with `blok`, an ask
  answered through the blok's returns) or while it carries on (`INFORM`). The blok is named by a
  declared `key`, an `id`, or BSX `source` the server parses, with its args, returns, dependencies,
  agent mappings, demo state, catalog and services (`TaskBlok`).

- `DECIDE` carries the answer's `values`, checked by the server against the blok's returns. -
  `BLOK_REFUSED {task, reason}` answers a blok the server cannot make; nothing of the frame is kept,
  so the sender can fix it and send again.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>


## v0.5.1 (2026-10-20)

### Bug Fixes

- Publish the state-requirements release
  ([`4973180`](https://github.com/arkitektio/arkirust/commit/4973180a28ba88bad910ed7017ffbf4ed661a9de))

The release run of 0.5.0 had its tag push rejected and published nothing; this cuts the release from
  it.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>


## v0.5.0 (2026-10-20)

### Features

- State requirements on assigns
  ([`1711660`](https://github.com/arkitektio/arkirust/commit/1711660018930ab2255696734654df6ab6adfb54))

An Assign and an AssignRequest carry state_requires (a state, an RFC 6901 path, a descriptor
  operator and a value) that the agent tests before the task runs. A FAILED report and its caller
  event carry a code (STATE_PRECONDITION), and an agent says on REGISTER which features it
  implements (state_requires). The Rust agent does not test them and so refuses any task that
  carries them.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>


## v0.4.0 (2026-10-20)

### Features

- **rekuest-protocol**: Held tasks and decisions on the agent wire
  ([`6d57063`](https://github.com/arkitektio/arkirust/commit/6d57063f977effcca6216e37c3d036d57974d224))

A task can wait for a person: HELD (FromAgent) says why, with an AssignPrompt in the agent's own
  words, and the server answers with DECIDE (approved, answer, auto). A child an agent asks for
  carries a prompt on its ASSIGN_REQUEST; while the server holds it the caller gets HELD_EVENT, and
  a refusal ends it with DENIED_EVENT. The hold effect now records the decision.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>


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
