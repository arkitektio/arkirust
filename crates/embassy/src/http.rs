//! fakts' [`Host`] on embassy: HTTP through one network task.
//!
//! A board's TCP sockets borrow its network stack and are not `Send`; fakts'
//! and rath's futures must be (an action awaits them). So the exchanges are
//! made by [`serve_http`], a task that owns the TCP client, and
//! [`NetHost::send`] only hands it a request and waits for the reply, over
//! embassy-sync channels (critical-section mutexes, so `Send`).
//!
//! ```ignore
//! static HOST: NetHost = NetHost::new();
//! spawner.spawn(http_task(stack))?; // calls serve_http(&HOST, &TcpClient, &DnsSocket)
//! let fakts = Fakts::builder(url, manifest).host(Arc::new(&HOST)).load().await?;
//! ```

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;

use async_trait::async_trait;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embedded_nal_async::{Dns, TcpConnect};
use fakts::host::{Host, HostError, Method, Reply, Request};

use crate::conn::{TlsBuffers, Verify};

pub use crate::http1::{MAX_BODY, MAX_HEADERS};

/// When a request names no timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// See the module docs. `static`, and served by [`serve_http`].
pub struct NetHost {
    /// One exchange at a time: the reply channel is shared.
    turn: Mutex<CriticalSectionRawMutex, ()>,
    requests: Channel<CriticalSectionRawMutex, (u32, Request), 1>,
    replies: Channel<CriticalSectionRawMutex, (u32, Result<Reply, HostError>), 1>,
    next: AtomicU32,
}

impl Default for NetHost {
    fn default() -> Self {
        Self::new()
    }
}

impl NetHost {
    pub const fn new() -> Self {
        Self {
            turn: Mutex::new(()),
            requests: Channel::new(),
            replies: Channel::new(),
            next: AtomicU32::new(0),
        }
    }
}

#[async_trait]
impl Host for &'static NetHost {
    async fn send(&self, request: Request) -> Result<Reply, HostError> {
        let _turn = self.turn.lock().await;
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.requests.send((id, request)).await;
        loop {
            // A reply for a caller that gave up before it came is dropped.
            let (of, reply) = self.replies.receive().await;
            if of == id {
                return reply;
            }
        }
    }

    async fn sleep(&self, duration: Duration) {
        embassy_time::Timer::after(embassy_time::Duration::from_micros(
            duration.as_micros() as u64
        ))
        .await
    }

    fn now_unix(&self) -> i64 {
        rekuest::rt::now().timestamp()
    }

    fn fill_random(&self, bytes: &mut [u8]) {
        rekuest::rt::fill_random(bytes)
    }
}

/// Make `host`'s exchanges over `tcp` and `dns`, forever. Run it as a task.
/// `https://` is TLS 1.3, the server checked as `verify` says.
pub async fn serve_http<T: TcpConnect, D: Dns>(
    host: &'static NetHost,
    tcp: &T,
    dns: &D,
    verify: Verify,
) -> ! {
    let mut buffers = TlsBuffers::default();
    loop {
        let (id, request) = host.requests.receive().await;
        let timeout = request.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let reply = match embassy_time::with_timeout(
            embassy_time::Duration::from_micros(timeout.as_micros() as u64),
            exchange(tcp, dns, &verify, &mut buffers, &request),
        )
        .await
        {
            Ok(reply) => reply,
            Err(_) => Err(HostError(format!("{} did not answer in time", request.url))),
        };
        host.replies.send((id, reply)).await;
    }
}

/// One request on a connection of its own. Redirects are never followed:
/// alias challenges need that, and the fakts server does not redirect.
async fn exchange<T: TcpConnect, D: Dns>(
    tcp: &T,
    dns: &D,
    verify: &Verify,
    buffers: &mut TlsBuffers,
    request: &Request,
) -> Result<Reply, HostError> {
    let url = request.full_url();
    let failed = |e: String| HostError(format!("{url}: {e}"));
    let (secure, name, port, path) =
        split_url(&url).ok_or_else(|| HostError(format!("not an http url: {url}")))?;
    let mut connection = crate::conn::open(tcp, dns, secure, &name, port, verify, buffers)
        .await
        .map_err(failed)?;
    let body = request.encoded_body();
    let headers: Vec<(&str, &str)> = request
        .headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let (status, body) = crate::http1::exchange(
        &mut connection,
        match request.method {
            Method::Get => "GET",
            Method::Post => "POST",
        },
        &authority(&name, port, secure),
        &path,
        &headers,
        body.as_ref().map(|(t, b)| (*t, b.as_slice())),
    )
    .await
    .map_err(failed)?;
    Ok(Reply { status, body })
}

/// `host[:port]`, the port left out when it is the scheme's.
pub(crate) fn authority(host: &str, port: u16, secure: bool) -> String {
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    if port == if secure { 443 } else { 80 } {
        host
    } else {
        format!("{host}:{port}")
    }
}

/// Why a URL could not be split.
pub(crate) fn split_url(url: &str) -> Option<(bool, String, u16, String)> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("wss://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("ws://") {
        (false, rest)
    } else {
        (false, url.strip_prefix("http://")?)
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.ends_with(']') || authority.starts_with('[') => {
            (host, port.parse().ok()?)
        }
        _ => (authority, if secure { 443 } else { 80 }),
    };
    Some((
        secure,
        host.trim_matches(['[', ']']).to_string(),
        port,
        path,
    ))
}
