# arkitekt-embassy

Arkitekt apps on [embassy](https://embassy.dev), without std: the rekuest
agent, fakts' login and rath's GraphQL over embedded-nal-async TCP and TLS
(embassy-net's `TcpClient` and `DnsSocket` on a board).

- `Embassy`: rekuest's runtime on an embassy executor; the board's RNG and
  wall clock (SNTP, an RTC) come in through it.
- `NetHost` + `serve_http`: fakts' (and rath's) `Host`. One network task owns
  the TCP client and makes every exchange; callers wait on embassy-sync
  channels, so their futures are `Send` although the sockets are not.
- `Agent`: rekuest's link over a websocket client of its own.
- `Board`: the platform (arkitekt-core): `Runtime::connect(board, app,
  options)` logs the app in through `NetHost` and `serve()` runs the agent.

`https://` and `wss://` are TLS 1.3 or 1.2: rustls's unbuffered client with
the RustCrypto provider, without std (`tls`). Its reads are cancel-safe
(every buffer is the stream's), which the agent's and the mesh's
`select`s over a read need. The server's certificate chain and name are
checked (webpki) against the CA given as `Verify::Ca`, or any of those in
`Verify::Roots`; `Verify::Unverified` skips that and logs it. HTTP is a
small HTTP/1.1 client of its own; `serve_http` keeps the last connection
open for the next request to its host.

A CA (DER) is trusted wherever the chain reaches it: webpki stops at the
first certificate a trusted root signed. For Let's Encrypt, ISRG Root X1
(RSA) signs the end of every chain, ECDSA (YE1 → Root YE → ISRG Root X2 →
X1) or RSA (R10, R11, …); with Root YE trusted as well, the ECDSA chain
stops after YE1: two signatures to check, not four (one of them RSA-4096).
On a board each one costs a second or so.

Sessions are resumed (TLS 1.3 tickets): `tls::config` makes one config per
`Verify` and hands it out again, since rustls resumes a session only under
the verifier it was made with. A server shaken hands with before costs a
key exchange, not its chain again (about 0.4 s on an ESP32-S3, not 3.3 s).

The RustCrypto provider takes its randomness from getrandom; on a board
(`target_os = "none"`) this crate registers the runtime's RNG for it.

`checks/embassy` runs it on the host against the real stack (`real_stack`,
over https and wss, verified), against any public server (`public_tls <url>
<CA PEM>`: tried with Let's Encrypt, GlobalSign and Sectigo chains, ECDSA
and RSA), and builds the board wiring for a Cortex-M4F (`board.rs`).
