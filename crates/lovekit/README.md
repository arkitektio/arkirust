# lovekit

The Rust client for Arkitekt's lovekit service, the twin of the Python
`lovekit` package. It manages LiveKit broadcasts and streams and hands out
the tokens to join them.

```rust
let app = arkitekt::App::new("camera", "0.1.0").service(lovekit::service);
// in an action: #[inject] lovekit: lovekit::Lovekit
let broadcast = lovekit.ensure_solo_broadcast(Some("microscope"), None).await?;
let token = lovekit.ensure_stream(Some(&broadcast.id), lovekit::StreamKind::Video, Some("camera")).await?;
```

## Streaming (feature `livekit`)

```rust
let (room, _events) = lovekit.connect_room(&token).await?;
let camera = lovekit::VideoPublisher::publish(&room, "camera", 640, 480).await?;
camera.push_rgba(&rgba_frame, timestamp_us);
```

The feature builds WebRTC (`webrtc-sys`), which needs `clang++` on Linux.

## A media server on the mesh (feature `mesh`)

When the `io.livekit.livekit` alias is only reachable over the deployment's
mesh, `connect_room` joins it through the mesh node, the same way Python's
`lovekit.aconnect_room` does:
- **signaling** goes through a local TCP forward (`Fakts::mesh_forward`);
- **media** (WebRTC, UDP) goes through the node's TURN relay on 127.0.0.1.
  The relay is the room's only ICE server, with a relay-only policy, and it
  sends every packet as UDP over the mesh (`Fakts::mesh_turn`).

This needs the native mesh backend (`ARKITEKT_MESH=native`), and no root or
TUN device. The server must advertise its mesh address on its UDP port (see
`testing/mesh-lab/livekit/livekit.yaml`).

`testing/mesh-lab` has a LiveKit server that is only on the mesh
(`lab.sh livekit`). `tests/lab_livekit.rs` streams video through it and
checks the subscriber decodes the frames.
