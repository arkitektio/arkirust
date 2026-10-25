//! arkitekt-embassy on a board's embassy-net stack: the network task serving
//! fakts' host, the login, and the agent, with embassy-net's (not `Send`)
//! TCP and DNS sockets. Built for thumbv7em to prove it fits; a firmware's
//! `main` would call [`run`] once its network is up.

use alloc::sync::Arc;

use arkitekt_embassy::{serve_http, Board, ConnectOptions, NetHost, Runtime, Verify};
use arkitekt_spec::{action, App};
use embassy_net::dns::DnsSocket;
use embassy_net::tcp::client::{TcpClient, TcpClientState};
use embassy_net::Stack;
use fakts::{Grant, MandateKeyOptions};

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
pub async fn run(stack: Stack<'static>, lok: &str, key: &str) -> anyhow::Result<()> {
    let state: TcpClientState<1, 4096, 4096> = TcpClientState::new();
    let tcp = TcpClient::new(stack, &state);
    let dns = DnsSocket::new(stack);
    let board = Board {
        host: &HOST,
        tcp: &tcp,
        dns: &dns,
        verify: Verify::Unverified,
    };
    let options = ConnectOptions {
        grant: Some(Grant::MandateKey(Arc::new(MandateKeyOptions::new(key)))),
        no_cache: true,
        ..ConnectOptions::default().url(lok)
    };
    let app = App::new("board", "0.1.0").action(add);
    Runtime::connect(board, app, options).await?.serve().await
}
