# rekuest-session

The agent's side of the rekuest `/agi` protocol as a sans-IO state machine,
`no_std` + `alloc`: the `REGISTER` → `INIT` handshake, heartbeats, bounces,
kicks and close codes, reconnecting with backoff, holding task messages until
the app is activated, and the outbox that keeps every numbered frame until a
`JOURNAL_ACK` covers it and re-sends it on the next connection.

A driver feeds it time, frames and connection changes and takes out frames to
send, events to act on and the next deadline:

```rust,ignore
let mut session = Session::new(SessionConfig::new(session_id, seed));
loop {
    session.handle_timeout(now());
    while let Some(event) = session.poll_event() { /* Connect, Message, … */ }
    while let Some(frame) = session.poll_transmit() { socket.send(to_text(&frame)) }
    // wait for a frame (session.handle_text), a close, or session.poll_timeout()
}
```

Drivers: [`rekuest::Agent`](../rekuest) (tokio), `checks/wasm` (wasm32) and
`checks/embassy` (embassy, no_std). The `testing` feature has the scripted
server and app they are all tested with.
