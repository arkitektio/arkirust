# arkitekt-embassy

Arkitekt apps on [embassy](https://embassy.dev), without std: the rekuest
agent, fakts' login and rath's GraphQL over embedded-nal-async TCP
(embassy-net's `TcpClient` and `DnsSocket` on a board).

- `Embassy`: rekuest's runtime on an embassy executor; the board's RNG and
  wall clock (SNTP, an RTC) come in through it.
- `NetHost` + `serve_http`: fakts' (and rath's) `Host`. One network task owns
  the TCP client and makes every exchange; callers wait on embassy-sync
  channels, so their futures are `Send` although the sockets are not.
- `Agent`: rekuest's driver over a websocket client of its own.

Plain `http://` and `ws://` for now; `https://` and `wss://` are refused until
an embedded TLS client builds (reqwless 0.14's embedded-tls 0.18 does not).

`checks/embassy` runs it on the host against the real stack (`real_stack`)
and builds the board wiring for a Cortex-M4F (`board.rs`).
