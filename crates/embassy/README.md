# arkitekt-embassy

Arkitekt apps on [embassy](https://embassy.dev), without std: the rekuest
agent, fakts' login and rath's GraphQL over embedded-nal-async TCP and TLS
(embassy-net's `TcpClient` and `DnsSocket` on a board).

- `Embassy`: rekuest's runtime on an embassy executor; the board's RNG and
  wall clock (SNTP, an RTC) come in through it.
- `NetHost` + `serve_http`: fakts' (and rath's) `Host`. One network task owns
  the TCP client and makes every exchange; callers wait on embassy-sync
  channels, so their futures are `Send` although the sockets are not.
- `Agent`: rekuest's driver over a websocket client of its own.

`https://` and `wss://` are TLS 1.3 (embedded-tls 0.19, AES-128-GCM): the
server's certificate chain and name are checked against the CA given as
`Verify::Ca` (the root, DER, e.g. ISRG Root X1 for Let's Encrypt);
`Verify::Unverified` skips that and logs it. HTTP is a small HTTP/1.1 client
of its own (one exchange per connection).

`checks/embassy` runs it on the host against the real stack (`real_stack`,
over https and wss, verified)
and builds the board wiring for a Cortex-M4F (`board.rs`).
