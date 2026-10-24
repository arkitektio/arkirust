//! An embassy agent on the real stack, from the host: arkitekt-embassy's
//! network task, login and agent on embassy's executor, over the host's TCP
//! as embedded-nal-async (async-io, its own reactor thread). Logs in at the
//! lok in `ARKIRUST_E2E_LOK` with `ARKIRUST_E2E_KEY`, registers with takt,
//! and waits until the server acknowledges the journal; prints
//! `ARKIRUST_E2E_EMBASSY_ACKED` and exits 0.
//!
//! The same code runs on a board over embassy-net (`board.rs`); only the
//! IP stack differs.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use arkitekt_embassy::{serve_http, Agent, AgentOptions, Embassy, Entropy, NetHost};
use embassy_executor::Spawner;
use embedded_io_adapters::futures_03::FromFutures;
use embedded_nal_async::{AddrType, Dns, TcpConnect};
use fakts::{Fakts, Grant, MandateKeyOptions, Manifest, Requirement};
use rekuest::{action, Context, Registry};

static HOST: NetHost = NetHost::new();

/// The host's TCP as embedded-nal-async.
struct HostTcp;

impl TcpConnect for HostTcp {
    type Error = std::io::Error;
    type Connection<'a> = FromFutures<async_io::Async<std::net::TcpStream>>;

    async fn connect<'a>(
        &'a self,
        remote: SocketAddr,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        Ok(FromFutures::new(
            async_io::Async::<std::net::TcpStream>::connect(remote).await?,
        ))
    }
}

/// The host's resolver as embedded-nal-async.
struct HostDns;

impl Dns for HostDns {
    type Error = std::io::Error;

    async fn get_host_by_name(
        &self,
        host: &str,
        addr_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        (host, 0)
            .to_socket_addrs()?
            .map(|a| a.ip())
            .find(|ip| match addr_type {
                AddrType::IPv4 => ip.is_ipv4(),
                AddrType::IPv6 => ip.is_ipv6(),
                AddrType::Either => true,
            })
            .ok_or_else(|| std::io::Error::other(format!("{host} does not resolve")))
    }

    async fn get_host_by_address(
        &self,
        _addr: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        Err(std::io::Error::other("no reverse lookups"))
    }
}

struct HostEntropy;

impl Entropy for HostEntropy {
    fn fill(&self, bytes: &mut [u8]) {
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), bytes)
    }
}

/// Add two numbers
#[action]
async fn add(a: i64, b: i64) -> i64 {
    a + b
}

#[embassy_executor::task]
async fn http() {
    serve_http(&HOST, &HostTcp, &HostDns).await
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
    spawner.spawn(http().expect("the http task"));

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

    let mut options = AgentOptions::new(alias.to_ws_path("agi"));
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
