//! What one TLS connection (`conn::open_owned`: rustls's unbuffered client,
//! RustCrypto) costs in heap: its peak during the handshake and what it
//! holds once open, measured with a counting allocator on the host.
//!
//! ```sh
//! cargo run --release --bin tls_memory -- https://letsencrypt.org/ /etc/ssl/certs/ISRG_Root_X1.pem
//! ```

#[path = "../host_net.rs"]
mod host_net;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;

use arkitekt_embassy::conn::{self, Verify};
use embassy_executor::Spawner;
use embedded_io_async::{Read, Write};
use host_net::{HostDns, HostEntropy, HostTcp};

struct Counting;

static NOW: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = NOW.fetch_add(layout.size() as isize, Ordering::SeqCst) + layout.size() as isize;
        PEAK.fetch_max(now, Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        NOW.fetch_sub(layout.size() as isize, Ordering::SeqCst);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn kib(bytes: isize) -> f64 {
    bytes as f64 / 1024.0
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut args = std::env::args().skip(1);
    let (Some(url), Some(ca)) = (args.next(), args.next()) else {
        eprintln!("usage: tls_memory <https url> <CA PEM>");
        std::process::exit(2);
    };
    let runtime = arkitekt_embassy::runtime::Embassy::new(spawner.make_send(), HostEntropy);
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64;
    runtime.set_wall_clock(unix);
    let _ = arkitekt_spec::rt::install(Arc::new(runtime));
    let pem = std::fs::read_to_string(&ca).expect("the CA");
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let der = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64).expect("PEM");
    let rest = url.strip_prefix("https://").expect("an https url");
    let (host, path) = rest
        .split_once('/')
        .map(|(h, p)| (h, format!("/{p}")))
        .unwrap_or((rest, "/".into()));

    let verify = Verify::Ca(der);
    {
        let before = NOW.load(Ordering::SeqCst);
        PEAK.store(before, Ordering::SeqCst);
        let config = arkitekt_embassy::tls::config(&verify).unwrap();
        println!(
            "TLS_MEMORY config alone: {:.1} KiB held, peak {:.1} KiB",
            kib(NOW.load(Ordering::SeqCst) - before),
            kib(PEAK.load(Ordering::SeqCst) - before)
        );
        let before = NOW.load(Ordering::SeqCst);
        let conn = rustls::client::UnbufferedClientConnection::new(
            config.clone(),
            rustls::pki_types::ServerName::try_from(host.to_string()).unwrap(),
        )
        .unwrap();
        println!(
            "TLS_MEMORY a new connection, nothing sent: {:.1} KiB held, peak {:.1} KiB",
            kib(NOW.load(Ordering::SeqCst) - before),
            kib(PEAK.load(Ordering::SeqCst) - before)
        );
        drop(conn);
        drop(config);
    }
    // The host's reactor and resolver start on first use: not the TLS's.
    drop(
        conn::open_owned(&HostTcp, &HostDns, false, host, 443, &verify)
            .await
            .expect("a TCP connection"),
    );
    let before = NOW.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    let mut stream = conn::open_owned(&HostTcp, &HostDns, true, host, 443, &verify)
        .await
        .expect("the TLS connection");
    let handshake_peak = PEAK.load(Ordering::SeqCst) - before;
    let open = NOW.load(Ordering::SeqCst) - before;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    PEAK.store(NOW.load(Ordering::SeqCst), Ordering::SeqCst);
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 4096];
    let mut total = 0;
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => total += n,
        }
    }
    let traffic_peak = PEAK.load(Ordering::SeqCst) - before;
    println!("TLS_MEMORY handshake peak {:.1} KiB, held open {:.1} KiB, peak while reading {total} bytes {:.1} KiB",
        kib(handshake_peak), kib(open), kib(traffic_peak));
    drop(stream);
    std::process::exit(0);
}
