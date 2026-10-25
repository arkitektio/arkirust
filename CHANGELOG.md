# CHANGELOG


## v0.15.0 (2026-10-25)

### Features

- **embassy**: Publish arkitekt-embassy with the other crates
  ([`7dbffa2`](https://github.com/arkitektio/arkirust/commit/7dbffa260f3149bb7939e851437aba957f0e3f5d))

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

- **embassy**: Tls 1.3 on embassy: https and wss, verified against a CA
  ([`b456cf8`](https://github.com/arkitektio/arkirust/commit/b456cf88135dfb2ca5004eec9acd788adf665753))

arkitekt-embassy drops reqwless (its embedded-tls 0.18 does not build against a released der) for a
  small HTTP/1.1 client of its own (src/http1.rs: one exchange per connection; Content-Length,
  chunked and until-close bodies) over a transport that is plain TCP or embedded-tls 0.19
  (src/conn.rs). The server's chain and name are checked against the CA given as Verify::Ca (the
  root, DER; RSA and ECDSA chains); Verify::Unverified skips both and logs it. The agent's websocket
  runs over the same transport (wss://); it now serves each connection in a scope of its own, so the
  TLS record buffers are borrowed only while one is open.

The test stack's gateway also serves https for localhost with Caddy's internal CA (Stack::tls_root
  hands out its root). The embassy real-stack run calls rath over https and runs the agent over wss,
  both verified against it, and checks that the right CA with the wrong name is refused.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

### Testing

- **rekuest**: Wait for the lock on time, not on a count of yields
  ([`f147e70`](https://github.com/arkitektio/arkirust/commit/f147e704ef49c66b90fe95419c8ac6e535959fd4))

unlock_always_follows_the_cancelled_report gave each even round 1000 yields to take the lock; a
  loaded CI runner took longer and the test failed (t48 never locked). It now waits up to 5 s and
  fails loudly if not.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>


## v0.14.0 (2026-10-24)

### Bug Fixes

- **embassy**: Bodies of any size; rath on embassy and wasm against the real stack
  ([`5202a9d`](https://github.com/arkitektio/arkirust/commit/5202a9ddb52a8e7b95f8a5a52cc1a7f29a895d9c))

serve_http read a response's headers and body into one fixed buffer, so a real GraphQL answer did
  not fit; the body now streams into memory (only the headers must fit, 8 KiB), and exchanges ask
  for Connection: close (one connection each, so a body without a length ends where it does).

The real-stack runs now call rath too, with the agent's token: on embassy through NetHost, an
  introspection answer larger than the header buffer; on wasm over fetch.

The TLS note was wrong: embedded-tls 0.19 builds (for a Cortex-M too); it is reqwless 0.14 that
  brings 0.18, which does not. TLS needs an HTTP client of our own over 0.19.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

### Features

- **embassy**: Arkitekt-embassy, the agent on embassy, run against the real stack
  ([`7de8938`](https://github.com/arkitektio/arkirust/commit/7de89384e49118eb35ac3fe7c6d2be8a38bf68e6))

New crate arkitekt-embassy (no_std, publish = false until its name is reserved): - Embassy:
  rekuest's rt::Runtime on an embassy executor (a pool of boxed tasks, embassy-time; the board's RNG
  and wall clock come in through it); - NetHost + serve_http: fakts' Host (and so rath's) as
  reqwless over embedded-nal-async, made by one network task: callers wait on embassy-sync channels,
  so the futures are Send although embassy-net's sockets are not; replies of callers that gave up
  are dropped by id; - Agent: rekuest's Driver over a websocket client of its own (RFC 6455: accept
  checked, pings answered, fragments joined, buffers that grow, and cancel-safe as long as the
  stream's read is).

checks/embassy: board.rs wires it over a real embassy-net stack (TcpClient, DnsSocket) and builds
  for thumbv7em; bin/real_stack runs it on the host over async-io TCP. rekuest's real_rekuest starts
  the stack and runs it: the embassy agent logs in at lok, registers with takt and gets its journal
  acknowledged, beside the native and wasm agents.

Plain http/ws only: reqwless 0.14's TLS (embedded-tls 0.18) does not build against a released der,
  so https:// and wss:// are refused for now.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

- **fakts,rath**: Fakts and rath without std: the login and GraphQL on a Host
  ([`fd8209f`](https://github.com/arkitektio/arkirust/commit/fd8209fc997e1c52a38a3be59e87e186a7565023))

fakts' login (discovery, device code, mandate key, refresh, alias challenges) runs against
  fakts::Host: HTTP exchanges as plain data (Request/Reply), sleep, the clock, randomness. With the
  new std feature (default) that is fakts::http::Reqwest, reqwest natively and over fetch on wasm,
  keeping each request's semantics (challenges follow no redirects and time out after 3 s, 20 s
  through the mesh; discovery after 10 s): a test checks that a challenge answered with a redirect
  does not count. Without std, fakts is no_std + alloc and the app gives the builder a Host. The
  reqwest-taking grants functions and Fakts::http stay (std) and wrap the *_with ones. The file
  cache, environment variables, the mesh and ring need std.

arkitekt-rath sends through a fakts::Host too (Rath::with_host); with std (default) it builds a
  reqwest one and has typed graphql_client operations. The task token is a poll-scoped local without
  std as well, so rekuest's rath feature no longer needs std.

Both build for thumbv7em-none-eabihf without std; CI lints them there.

BREAKING CHANGE: fakts and arkitekt-rath have a std feature; dependencies without default features
  turn it on (the workspace's rath entry now has default-features = false). FaktsError::Transport
  and RathError::Transport are new variants.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

- **rekuest**: Rekuest without std: a real #[action] on embassy
  ([`8acd92d`](https://github.com/arkitektio/arkirust/commit/8acd92d674655579f902c08438661f511b360520))

rekuest's new std feature (default; tokio, web, rath, wal and serve imply it). Without it the crate
  is no_std + alloc: locks are spin's (crate::sync, which never poisons with std either), maps
  hashbrown's (indexmap hashed by it), the task local a global slot, TaskGate::close spins instead
  of waiting on a Condvar, and a panicking action aborts. The runtime brings the wall clock and
  randomness (Runtime::wall_clock_micros, Runtime::fill_random, defaulted with std; rt::now,
  rt::uuid, rt::fill_random). The state diff is json-patch's algorithm in src/patch.rs (json-patch
  needs std; it stays a dev-dependency the diff is checked against). #[action] and #[derive(State)]
  expand without naming std.

checks/embassy runs the scripted scenario with a real #[action] through Driver on an embassy
  rt::Runtime (boxed-task pool, embassy-time), on the host and built for thumbv7em-none-eabihf; CI
  clippy-builds rekuest for it.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

### Testing

- **rekuest**: The wasm agent's real-stack run calls rath over fetch
  ([`9977471`](https://github.com/arkitektio/arkirust/commit/99774711d08dfb559de20c2c3259d6b7e98d8ab2))

The edit meant for 5202a9d did not apply; this is it.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>


## v0.13.0 (2026-10-24)

### Bug Fixes

- **rekuest**: Wake outside the primitives' locks; sync actions covered on wasm and tokio
  ([`a53afb2`](https://github.com/arkitektio/arkirust/commit/a53afb27449fa9c3ef2efc6c46dd78a1a5607715))

FifoMutex, Latch, Watch and Signal wake their waiters after releasing their lock (an executor may
  poll inside wake), and remember a re-polled task's waker once. A sync #[action] runs on wasm
  (checks/wasm's echo, inline) and its panic becomes the task's critical error (tests/actions.rs).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

### Features

- **rekuest**: The agent in a browser or node: web::Agent over the JS WebSocket
  ([`af45fd3`](https://github.com/arkitektio/arkirust/commit/af45fd325220c488fca734aa9e3478ad8aae8d63))

rekuest's new web feature (wasm32): web::Agent is the Driver over the JS WebSocket on the JS event
  loop (rt::Js, the default runtime there), with its tokens from fakts. fakts and arkitekt-rath
  build for wasm32: reqwest over fetch (no rustls, no proxy, no redirect policy there), every
  exchange made whole in fakts::http (send + body, its future wrapped on wasm, where fetch futures
  are not Send), timers and Instant from JS (web-time), no cache file. rath's task token is a
  poll-scoped local on any runtime.

checks/wasm/tests/real_stack.rs: a wasm agent under node logs in at the real lok and registers with
  the real takt, and the server acknowledges its journal; real_rekuest starts the stack and runs it
  (the e2e job installs node and wasm-pack). The stack harness names each stack's compose project
  apart, so tests in one binary run side by side.

Also: async-trait 0.1.92 (the async-trait expansion tripped clippy 1.99's double_must_use, which
  failed the controls job and held back 0.13.0).

BREAKING CHANGE: rath::CURRENT_TASK_TOKEN is private; use with_task_token.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

- **rekuest**: The agent runs on any async runtime; a real #[action] on wasm
  ([`743a9f4`](https://github.com/arkitektio/arkirust/commit/743a9f459bde09547c8ba3c8f4fc42ae2ebdea41))

The executor, journal, state hub, locks, pausepoints, shelf and the #[action] expansion no longer
  name tokio: they spawn, sleep and wait through rekuest::rt (Runtime: spawn, sleep, blocking work),
  tokio with the new tokio feature, else whatever the app installs (rt::install). tokio's primitives
  give way to runtime-free ones: FifoMutex (strictly first come, first served, as the named locks
  promise), Latch, Signal (a stored wake-up, as Notify::notify_one), Watch, TaskLocal (scoped per
  poll, so blocking work does not see it, as before), async-channel.

rekuest::Driver is an agent without its socket (session, executor, journal, activation, inquiries);
  Agent is it with a tokio websocket, and checks/wasm runs the scripted scenario with a real
  #[action] through it on wasm32 (node), on a JS-event-loop runtime. CI clippy-builds rekuest for
  wasm32 without default features.

BREAKING CHANGE: rekuest's new features tokio (Agent, transport, fakts) and rath (task tokens on
  GraphQL calls) are on by default; a dependency with default features off turns them on (arkitekt
  and checks/esp32s3 do). wal and serve imply tokio.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

### Breaking Changes

- **rekuest**: Rath::current_task_token is private; use with_task_token.


## v0.12.0 (2026-10-24)

### Features

- **rekuest**: The agent session as a sans-IO core, driven on tokio, wasm and embassy
  ([`1834b8c`](https://github.com/arkitektio/arkirust/commit/1834b8cecfe182f13c02ddcc6876786edbdb883c))

rekuest-session (new, no_std + alloc) holds the /agi session as a state machine: handshake,
  heartbeats, bounce/kick/close codes, backoff, activation gating and the outbox that keeps numbered
  frames until JOURNAL_ACK. Time, frames and randomness come from the driver. rekuest::Agent is now
  its tokio driver; checks/wasm (node) and checks/embassy (host + thumbv7em) drive it with a minimal
  driver each against the shared scripted scenario, in CI.

rekuest-protocol is no_std + alloc too (Envelope::new behind its default std feature,
  Envelope::with_id without it). The public API of rekuest and the macros are unchanged.

Behaviour changes, each tested: - every task message (ASSIGN, CANCEL, PAUSE, DECIDE, ...) waits for
  activation in order, so a CANCEL can no longer overtake its ASSIGN - close code 3004 (before
  registration) is a bounce, as in Python - the idle timeout counts frames from the server only - an
  activation failure ends the run even while disconnected

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

### Testing

- **rekuest**: A real agent registers with takt and gets its journal acknowledged
  ([`a8e236d`](https://github.com/arkitektio/arkirust/commit/a8e236d409a5a6710b34356b51a722c3da53086d))

The test stack now routes rekuest's address through a Caddy gateway (/agi to takt, the rest to the
  server) as a hub does, and rekuest trusts the tokens lok issues under the run's address
  (rekuest.yaml is rendered like lok.yaml). Both e2e tests share testing/stack/harness.rs, which
  prints the services' logs when a test fails.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

- **rekuest**: The tokio Agent runs the shared scripted scenario; run() starts over
  ([`afcc62a`](https://github.com/arkitektio/arkirust/commit/afcc62a93de686bd414c41902e8bd466fc09239c))

The std control: tests/scenario.rs runs the real Agent, executor and action against
  rekuest_session::testing::FakeServer over a localhost websocket. FakeServer::check now asserts
  protocol invariants (registration, the heartbeat answer, t1 up to YIELD and COMPLETED, every
  numbered frame of the first connection re-sent in order, seq from 1 per connection), so a real
  executor's extra frames pass as Echo's do.

Session::restart: Agent::run called again after the session ended, or after it stopped while
  connecting, connects again with a fresh retry budget, keeping the outbox and the activation (as
  run() did before the core).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>


## v0.11.1 (2026-10-24)

### Bug Fixes

- **fakts**: Give mesh aliases 20 s to answer their challenge
  ([`76fbf5e`](https://github.com/arkitektio/arkirust/commit/76fbf5ef7cd736693f057777d575ece61687d117))

The first request to a mesh peer sets up its tunnel (a WireGuard handshake, through DERP at first),
  about 5 s on an ESP32; with 3 s the alias was skipped and a mesh-only service unreachable. Direct
  aliases keep 3 s.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

### Continuous Integration

- Publish to crates.io on a manual run
  ([`f077d59`](https://github.com/arkitektio/arkirust/commit/f077d5919654dfff14dac0f888a008342679d783))

A release whose tag push failed (GitHub's commit_refs error) left 0.11.0 tagged but unpublished; a
  workflow_dispatch run publishes what crates.io lacks.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>


## v0.11.0 (2026-10-24)

### Features

- **fakts,arkitekt**: Meshjoiner, for a mesh node the app runs itself
  ([`93913e5`](https://github.com/arkitektio/arkirust/commit/93913e57269c753f22919ef7f8c5f53df28b4031))

fakts asks for a mesh key at login, hands it with the coordination server and the node's hostname to
  the joiner, and reaches mesh aliases through the proxy it returns (ConnectOptions::mesh_joiner).
  For targets where neither the meshd sidecar nor the native backend's state directory fits, e.g. a
  firmware keeping its node in NVS.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>


## v0.10.0 (2026-10-24)

### Features

- **arkitekt**: Build without SQLite (wal feature)
  ([`bb1b0a9`](https://github.com/arkitektio/arkirust/commit/bb1b0a91c866e8133a4628052a769fedfa7ee5be))

The workspace takes rekuest without its wal default, and arkitekt turns it on with a wal feature of
  its own (on by default), so an app on a target SQLite's C does not build for uses arkitekt with
  default features off. checks/esp32s3 now checks arkitekt too.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

- **fakts,arkitekt**: User-dirs feature for default paths
  ([`40d8276`](https://github.com/arkitektio/arkirust/commit/40d82761d09b0efafc35f78c470da719c74f0035))

The fakts cache file, the mesh node's state and the persisted device id live in the user's
  directories (dirs). dirs compiles for ESP-IDF but does not link there (no getuid/getpwuid_r), so
  it is the user-dirs feature now, on by default in fakts and arkitekt and implied by the mesh
  features. Without it an app passes its own store and device id; fakts without a store logs in on
  every start.

Found linking an ESP32-S3 firmware: checks/esp32s3 only type-checks.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>


## v0.9.0 (2026-10-24)

### Features

- **fakts**: Keep the fakts in a store of the app's choosing
  ([`ad91dfe`](https://github.com/arkitektio/arkirust/commit/ad91dfe7264aa263ce43f49a1b1990b3788137ae))

FaktsStore (load, save, clear under the app's cache key) replaces the hard-wired cache file, so a
  target without a filesystem keeps its credential where it can (a firmware's NVS). FileStore is the
  old file cache and stays the default; FaktsBuilder::store and ConnectOptions::store take another
  one.

BREAKING CHANGE: fakts::cache's read/write/remove are private; use FileStore.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

- **rekuest**: The agent's websocket runs over a connector
  ([`2db6cb1`](https://github.com/arkitektio/arkirust/commit/2db6cb1b0f23aad15da73aed2e25562ea3ca5bfa))

AgentOptions.connector (rekuest::Connector) opens the stream the websocket and its TLS run over:
  Direct (plain TCP, the default), HttpProxy (the mesh sidecar's CONNECT proxy, what arkitekt picks
  for a mesh alias), or the app's own, e.g. a connection through a mesh node it runs itself. Without
  the wal feature, AgentOptions::new no longer names a SQLite journal file.

BREAKING CHANGE: AgentOptions.proxy is gone (use connector: Arc::new(HttpProxy::new(proxy))), and
  connect_ws takes a &dyn Connector.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>

### Breaking Changes

- **rekuest**: Agentoptions.proxy is gone (use connector: Arc::new(HttpProxy::new(proxy))), and
  connect_ws takes a &dyn Connector.


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
