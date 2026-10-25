//! arkitekt-embassy's TLS against a server on the public internet:
//! `public_tls <https url> <CA PEM file>` fetches the url with the server
//! checked against the CA, prints `PUBLIC_TLS <status> <bytes>` and exits 0.
//! The CA is the root that signed the last certificate the server sends
//! (for Let's Encrypt, ISRG Root X1, RSA, at the end of its cross-signed
//! chain).

use std::sync::Arc;

use arkitekt_embassy::conn::{self, TlsBuffers, Verify};
use arkitekt_embassy::Embassy;
use embassy_executor::Spawner;

#[path = "../host_net.rs"]
mod host_net;
use host_net::{HostDns, HostEntropy, HostTcp};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let args: Vec<String> = std::env::args().collect();
    let [_, url, ca] = args.as_slice() else {
        eprintln!("usage: public_tls <https url> <CA PEM file>");
        std::process::exit(2);
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter("arkitekt_embassy=debug,embedded_tls=info")
        .with_writer(std::io::stderr)
        .try_init();
    let runtime = Embassy::new(spawner.make_send(), HostEntropy);
    // Certificates are checked against the wall clock.
    runtime.set_wall_clock(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0),
    );
    let _ = arkitekt_spec::rt::install(Arc::new(runtime));

    let pem = std::fs::read_to_string(ca).expect("the CA file");
    let ca = der(&pem);
    let rest = url.strip_prefix("https://").expect("an https url");
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };

    let mut buffers = TlsBuffers::default();
    let started = std::time::Instant::now();
    let mut stream = conn::open(
        &HostTcp,
        &HostDns,
        true,
        host,
        443,
        &Verify::Ca(ca),
        &mut buffers,
    )
    .await
    .unwrap_or_else(|e| {
        eprintln!("public_tls: {e}");
        std::process::exit(1)
    });
    let handshake = started.elapsed();
    let (status, body) =
        arkitekt_embassy::http1::exchange(&mut stream, "GET", host, path, &[], None)
            .await
            .unwrap_or_else(|e| {
                eprintln!("public_tls: {e}");
                std::process::exit(1)
            });
    eprintln!("public_tls: handshake took {handshake:?}");
    println!("PUBLIC_TLS {status} {}", body.len());
    std::process::exit(0);
}

/// The DER inside a PEM certificate.
fn der(pem: &str) -> Vec<u8> {
    use base64::Engine;
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(body)
        .expect("a PEM certificate")
}
