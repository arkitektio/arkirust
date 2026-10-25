//! arkitekt-embassy on a board's embassy-net stack: the network task serving
//! fakts' host, the login, and the agent, with embassy-net's (not `Send`)
//! TCP and DNS sockets. Built for thumbv7em to prove it fits; a firmware's
//! `main` would call [`run`] once its network is up.

use alloc::sync::Arc;

use arkitekt_embassy::{serve_http, Agent, AgentOptions, NetHost, Verify};
use arkitekt_spec::{action, Context, Registry};
use embassy_net::dns::DnsSocket;
use embassy_net::tcp::client::{TcpClient, TcpClientState};
use embassy_net::Stack;
use fakts::{Fakts, Grant, MandateKeyOptions, Manifest, Requirement};

static HOST: NetHost = NetHost::new();

/// Add two numbers
#[action]
async fn add(a: i64, b: i64) -> i64 {
    a + b
}

/// The network task: every fakts and rath request goes out here.
pub async fn http(stack: Stack<'static>) -> ! {
    let state: TcpClientState<2, 4096, 4096> = TcpClientState::new();
    let tcp = TcpClient::new(stack, &state);
    let dns = DnsSocket::new(stack);
    // A firmware checks the hub against its CA (the root of its chain).
    serve_http(&HOST, &tcp, &dns, Verify::Unverified).await
}

/// Log in at `lok` with a mandate key and serve the app (the agent's own
/// socket from the same stack).
pub async fn run(stack: Stack<'static>, lok: &str, key: &str) -> Result<(), &'static str> {
    let mut manifest = Manifest::new("board", "0.1.0");
    manifest
        .requirements
        .push(Requirement::new("rekuest", "live.arkitekt.rekuest"));
    let fakts = Fakts::builder(lok, manifest)
        .host(Arc::new(&HOST))
        .grant(Grant::MandateKey(Arc::new(MandateKeyOptions::new(key))))
        .no_cache(true)
        .load()
        .await
        .map_err(|_| "login failed")?;
    let alias = fakts.get_alias("rekuest").await.map_err(|_| "no rekuest")?;
    let mut registry = Registry::new();
    registry.register(add);
    let agent = Agent::new(
        AgentOptions::new(alias.to_ws_path("agi")),
        registry,
        Context::default(),
        Arc::new(fakts),
    );
    let state: TcpClientState<1, 4096, 4096> = TcpClientState::new();
    let tcp = TcpClient::new(stack, &state);
    let dns = DnsSocket::new(stack);
    agent.run(&tcp, &dns).await.map_err(|_| "the agent stopped")
}
