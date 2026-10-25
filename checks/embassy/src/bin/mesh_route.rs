//! The mesh on a board, on the host (RFC-9 phase 7b): an embassy mesh node
//! (arkitekt-mesh-embassy) dialing control and DERP through this crate's
//! TLS, and one [`Routed`] TCP under `serve_http`, so fakts' requests go
//! over the mesh for tailnet names and over the network for the rest, and
//! [`MeshHandoff`] answering fakts' join once the node is on the mesh.
//!
//! Against the mesh lab of arkitekt-mesh (testing/mesh-lab):
//!
//! ```sh
//! eval "$(../arkitekt-mesh/testing/mesh-lab/lab.sh env)"
//! cargo run --bin mesh_route
//! ```

#[path = "../host_net.rs"]
mod host_net;

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;

use arkitekt_embassy::conn::{self, Transport, Verify};
use arkitekt_embassy::route::{MeshHandoff, Routed};
use arkitekt_embassy::{serve_http, NetHost};
use embassy_executor::Spawner;
use embassy_futures::select::{select3, Either3};
use embedded_io_adapters::futures_03::FromFutures;
use fakts::host::Request;
use fakts::{Host, MeshJoin, MeshJoiner, MeshRoute};
use host_net::{HostDns, HostEntropy, HostTcp};
use mesh_embassy::{Config, Dial, DialError, Mesh, Tls, Udp};

static HOST: NetHost = NetHost::new();
static TCP: HostTcp = HostTcp;

/// The host's UDP for the mesh node.
struct HostUdp(async_io::Async<UdpSocket>);

impl Udp for HostUdp {
    type Error = std::io::Error;

    async fn send_to(&self, data: &[u8], to: SocketAddr) -> Result<(), std::io::Error> {
        self.0.send_to(data, to).await.map(|_| ())
    }

    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr), std::io::Error> {
        self.0.recv_from(buf).await
    }

    fn local_port(&self) -> u16 {
        self.0.get_ref().local_addr().map(|a| a.port()).unwrap_or(0)
    }
}

/// The mesh node's control and DERP streams, through this crate's TLS (as
/// on a board).
struct BoardDial {
    ca: Vec<u8>,
}

impl Dial for BoardDial {
    type Conn<'a> = Transport<'a, FromFutures<async_io::Async<std::net::TcpStream>>>;

    async fn dial(&self, host: &str, port: u16, tls: Tls<'_>) -> Result<Self::Conn<'_>, DialError> {
        let (secure, name, verify) = match tls {
            Tls::None => (false, host, Verify::Unverified),
            Tls::Verify(name) => (true, name, Verify::Ca(self.ca.clone())),
            Tls::Unverified(name) => (true, name, Verify::Unverified),
        };
        // Its record buffers go with it: the node reconnects.
        conn::open_owned(&TCP, &HostDns, secure, name, port, &verify)
            .await
            .map_err(DialError)
    }
}

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        eprintln!("{name}: load the mesh lab's environment (testing/mesh-lab/lab.sh env)");
        std::process::exit(2)
    })
}

/// The lab CA as DER.
fn ca() -> Vec<u8> {
    let pem = std::fs::read_to_string(var("ARKITEKT_MESH_CA_FILE")).expect("the CA file");
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64).expect("PEM")
}

async fn get(url: &str) -> (u16, String) {
    let reply = (&HOST)
        .send(Request::get(url))
        .await
        .unwrap_or_else(|e| panic!("GET {url}: {e:?}"));
    (
        reply.status,
        String::from_utf8_lossy(&reply.body).into_owned(),
    )
}

async fn scenario(mesh: &Mesh, lab_url: &str, peer: &str) {
    // fakts hands the login's key over; the mesh task answers once joined.
    let handoff = MeshHandoff::new();
    let join = MeshJoin {
        coord_url: Some(lab_url.to_owned()),
        auth_key: Some(String::from("from the login")),
        hostname: String::from("mesh-route-check"),
    };
    let (route, ()) = embassy_futures::join::join(handoff.join(join), async {
        let join = handoff.next_join().await;
        assert_eq!(join.auth_key.as_deref(), Some("from the login"));
        mesh.joined().await;
        handoff.joined();
    })
    .await;
    assert_eq!(route.unwrap(), Some(MeshRoute::Direct));
    println!(
        "joined as {:?}: fakts reaches the mesh directly",
        mesh.addresses()
    );

    // A tailnet name: over the mesh.
    let (status, body) = get(&format!("http://{peer}/")).await;
    assert_eq!(status, 200, "{body}");
    println!("GET http://{peer}/ (over the mesh): {status}");
    // Not the tailnet: over the network (TLS against the lab CA).
    let (status, body) = get(&format!("{lab_url}/key?v=142")).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("publicKey"), "{body}");
    println!("GET {lab_url}/key (over the network): {status}");
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("arkitekt_embassy=info,mesh_embassy=info")
        .with_writer(std::io::stderr)
        .try_init();
    let (url, key, peer) = (
        var("ARKITEKT_TEST_MESH_URL"),
        var("ARKITEKT_TEST_MESH_KEY"),
        var("ARKITEKT_TEST_MESH_PEER"),
    );
    // arkitekt-embassy's runtime: the clock certificates are checked against.
    let runtime = arkitekt_embassy::runtime::Embassy::new(spawner.make_send(), HostEntropy);
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64;
    runtime.set_wall_clock(unix);
    let _ = arkitekt_spec::rt::install(Arc::new(runtime));

    let ca = ca();
    let mut config = Config::small(
        url.clone(),
        mesh_core::keys::PrivateKey::generate(),
        mesh_core::keys::PrivateKey::generate(),
    );
    config.auth_key = Some(key);
    config.hostname = format!("mesh-route-{}", std::process::id());
    config.ephemeral = true;
    let mesh = Mesh::new(config);
    let udp = HostUdp(async_io::Async::<UdpSocket>::bind(([0, 0, 0, 0], 0)).unwrap());
    let dial = BoardDial { ca: ca.clone() };
    let routed = Routed {
        mesh: &mesh,
        tcp: &TCP,
        dns: &HostDns,
    };
    let run = select3(
        mesh.run(&udp, &dial),
        serve_http(&HOST, &routed, &routed, Verify::Ca(ca)),
        scenario(&mesh, &url, &peer),
    );
    match embassy_time::with_timeout(embassy_time::Duration::from_secs(60), run).await {
        Ok(Either3::Third(())) => println!("ok"),
        Ok(Either3::First(e)) => panic!("the mesh node ended: {e:?}"),
        Ok(Either3::Second(never)) => match never {},
        Err(_) => {
            eprintln!("FAILED: timed out");
            std::process::exit(1)
        }
    }
    std::process::exit(0);
}
