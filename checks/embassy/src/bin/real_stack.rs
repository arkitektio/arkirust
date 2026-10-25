//! An embassy agent on the real stack, from the host: arkitekt-embassy's
//! network task, login and agent on embassy's executor, over the host's TCP
//! as embedded-nal-async (async-io, its own reactor thread). Logs in at the
//! lok in `ARKIRUST_E2E_LOK` with `ARKIRUST_E2E_KEY`, registers with takt,
//! and waits until the server acknowledges the journal; prints
//! `ARKIRUST_E2E_EMBASSY_ACKED` and exits 0.
//!
//! The same code runs on a board over embassy-net (`board.rs`); only the
//! IP stack differs.

use std::sync::Arc;
use std::time::Duration;

use arkitekt_embassy::conn::TlsBuffers;
use arkitekt_embassy::{serve_http, Agent, AgentOptions, Embassy, NetHost, Verify};
use embassy_executor::Spawner;
use fakts::{Fakts, Grant, MandateKeyOptions, Manifest, Requirement};
use rekuest::{action, Context, Registry};

#[path = "../host_net.rs"]
mod host_net;
use host_net::{HostDns, HostEntropy, HostTcp};

static HOST: NetHost = NetHost::new();

/// Add two numbers
#[action]
async fn add(a: i64, b: i64) -> i64 {
    a + b
}

#[embassy_executor::task]
async fn http(verify: Verify) {
    serve_http(&HOST, &HostTcp, &HostDns, verify).await
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

#[embassy_executor::task]
async fn agent(agent: &'static Agent) {
    if let Err(e) = agent.run(&HostTcp, &HostDns).await {
        eprintln!("embassy agent: stopped: {e}");
        std::process::exit(1);
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let (Ok(lok), Ok(key)) = (
        std::env::var("ARKIRUST_E2E_LOK"),
        std::env::var("ARKIRUST_E2E_KEY"),
    ) else {
        eprintln!("set ARKIRUST_E2E_LOK and ARKIRUST_E2E_KEY (rekuest's real_rekuest test does)");
        std::process::exit(2);
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter("rekuest_session=info,rekuest=info,fakts=info,arkitekt_embassy=debug")
        .with_writer(std::io::stderr)
        .try_init();
    let runtime = Embassy::new(spawner.make_send(), HostEntropy);
    runtime.set_wall_clock(chrono_now_micros());
    let _ = rekuest::rt::install(Arc::new(runtime));
    // With the gateway's https port and CA, rekuest is reached over TLS.
    let tls = match (
        std::env::var("ARKIRUST_E2E_TLS_PORT"),
        std::env::var("ARKIRUST_E2E_CA_PEM"),
    ) {
        (Ok(port), Ok(pem)) => Some((port.parse::<u16>().expect("a port"), der(&pem))),
        _ => None,
    };
    let verify = match &tls {
        Some((_, ca)) => Verify::Ca(ca.clone()),
        None => Verify::Unverified,
    };
    spawner.spawn(http(verify.clone()).expect("the http task"));

    let mut manifest = Manifest::new("test_manifest", "0.1.0");
    manifest
        .requirements
        .push(Requirement::new("rekuest", "live.arkitekt.rekuest"));
    let fakts = Fakts::builder(lok, manifest)
        .host(Arc::new(&HOST))
        .grant(Grant::MandateKey(Arc::new(MandateKeyOptions::new(key))))
        .no_cache(true)
        .load()
        .await
        .unwrap_or_else(|e| {
            eprintln!("embassy agent: lok did not log the key in: {e}");
            std::process::exit(1)
        });
    let alias = fakts.get_alias("rekuest").await.unwrap_or_else(|e| {
        eprintln!("embassy agent: rekuest is not reachable: {e}");
        std::process::exit(1)
    });

    // rath through the same host, with the agent's token: an answer far
    // larger than the header buffer (the body streams into memory).
    let graphql_url = match &tls {
        Some((port, _)) => format!("https://localhost:{port}/graphql"),
        None => alias.to_http_path("graphql"),
    };
    let graphql = rath::Rath::with_host(Arc::new(&HOST), graphql_url, Arc::new(fakts.clone()));
    let schema = graphql
        .execute_raw(&serde_json::json!({"query": "{ __schema { types { name description } } }"}))
        .await
        .unwrap_or_else(|e| {
            eprintln!("embassy agent: rath failed: {e}");
            std::process::exit(1)
        });
    let size = serde_json::to_string(&schema).map(|s| s.len()).unwrap_or(0);
    // Larger than a fixed response buffer would hold: the body streams.
    if size <= 8 * 1024 {
        eprintln!("embassy agent: the schema answer was only {size} bytes");
        std::process::exit(1);
    }
    println!("ARKIRUST_E2E_EMBASSY_RATH {size}");

    if let Some((port, ca)) = &tls {
        // The right CA but the wrong name (the certificate is for localhost):
        // refused, so the server really is checked.
        let mut buffers = TlsBuffers::default();
        let wrong = arkitekt_embassy::conn::open(
            &HostTcp,
            &HostDns,
            true,
            "127.0.0.1",
            *port,
            &Verify::Ca(ca.clone()),
            &mut buffers,
        )
        .await;
        if wrong.is_ok() {
            eprintln!("embassy agent: TLS to 127.0.0.1 was accepted with a localhost certificate");
            std::process::exit(1);
        }
        println!("ARKIRUST_E2E_EMBASSY_TLS_REFUSES_A_WRONG_NAME");
    }

    let endpoint = match &tls {
        Some((port, _)) => format!("wss://localhost:{port}/agi"),
        None => alias.to_ws_path("agi"),
    };
    let mut options = AgentOptions::new(endpoint);
    options.verify = verify;
    options.name = Some("arkirust-e2e-embassy".into());
    options.policy.initial = Duration::from_millis(200);
    let mut registry = Registry::new();
    registry.register(add);
    let the_agent: &'static Agent = Box::leak(Box::new(Agent::new(
        options,
        registry,
        Context::default(),
        Arc::new(fakts),
    )));
    spawner.spawn(agent(the_agent).expect("the agent task"));

    for _ in 0..240 {
        let activated = the_agent.journal().watermark().is_some_and(|w| w.pos >= 1);
        let acked = the_agent
            .driver()
            .with_session(|session| session.outbox().retained_len() == 0);
        if activated && acked {
            println!("ARKIRUST_E2E_EMBASSY_ACKED");
            std::process::exit(0);
        }
        embassy_time::Timer::after_millis(250).await;
    }
    eprintln!("embassy agent: no journal ack within 60 s");
    std::process::exit(1);
}

/// The host's clock (a board asks SNTP).
fn chrono_now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}
