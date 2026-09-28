//! ESP32 spike: the mesh core (control handshake, WireGuard, DERP and disco
//! codecs, path selection, the TCP stack) built for ESP-IDF. The board
//! supplies the sockets; the core only turns bytes and time into bytes.
use std::time::Instant;

use mesh::control::noise::ClientHandshake;
use mesh::control::types::CAPABILITY_VERSION;
use mesh::keys::{MachinePublic, NodeIdentity, NodePublic, PrivateKey};
use mesh::netstack::Netstack;
use mesh::paths::Paths;
use mesh::wg::Tunnel;

/// Everything a board-side driver would hold for one node.
pub struct Core {
    pub tunnel: Tunnel,
    pub paths: Paths,
    pub netstack: Netstack,
}

pub fn core(identity: &NodeIdentity) -> Core {
    let now = Instant::now();
    Core {
        tunnel: Tunnel::new(identity.node.0.clone()),
        paths: Paths::new(PrivateKey::generate(), identity.node_public()),
        netstack: Netstack::new(&[], now),
    }
}

/// The first bytes a board sends to control, after `POST /ts2021`.
pub fn control_hello(identity: &NodeIdentity, control: MachinePublic) -> Vec<u8> {
    ClientHandshake::start(&identity.machine.0, &control, CAPABILITY_VERSION)
        .expect("valid keys")
        .1
}

pub fn peer(core: &mut Core, key: NodePublic) {
    core.tunnel.add_peer(key);
}

/// The whole node under tokio, as on a board with ESP-IDF's std: join,
/// dial a peer, send a request.
pub async fn join_and_get(config: mesh::driver::Config, peer: &str) -> std::io::Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // No ring on ESP-IDF: a pure-Rust TLS provider (alpha).
    let _ = rustls_rustcrypto::provider().install_default();
    let node = mesh::driver::Node::start(config).await.map_err(std::io::Error::other)?;
    let mut stream = node.dial(peer, 80).await?;
    stream.write_all(b"GET / HTTP/1.0\r\n\r\n").await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    Ok(response)
}
