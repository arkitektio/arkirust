//! fakts' [`Host`] on embassy: HTTP through one network task.
//!
//! A board's TCP sockets borrow its network stack and are not `Send`; fakts'
//! and rath's futures must be (an action awaits them). So the exchanges are
//! made by [`serve_http`], a task that owns the TCP client, and
//! [`NetHost::send`] only hands it a request and waits for the reply, over
//! embassy-sync channels (critical-section mutexes, so `Send`).
//!
//! The last connection stays open for the next request to its host (HTTP
//! keep-alive): a TLS handshake costs a board seconds, a request on an open
//! connection milliseconds.
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

use crate::conn::{Transport, Verify};

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
        arkitekt_spec::rt::now().timestamp()
    }

    fn fill_random(&self, bytes: &mut [u8]) {
        arkitekt_spec::rt::fill_random(bytes)
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
    let mut kept: Option<Kept<'_, T>> = None;
    loop {
        let (id, request) = host.requests.receive().await;
        let timeout = request.timeout.unwrap_or(DEFAULT_TIMEOUT);
        // The kept connection goes into the exchange and comes back if it
        // can carry the next one; a timeout drops it with the exchange.
        let reply = match embassy_time::with_timeout(
            embassy_time::Duration::from_micros(timeout.as_micros() as u64),
            exchange(tcp, dns, &verify, kept.take(), &request),
        )
        .await
        {
            Ok((reply, again)) => {
                kept = again;
                reply
            }
            Err(_) => Err(HostError(format!("{} did not answer in time", request.url))),
        };
        host.replies.send((id, reply)).await;
    }
}

/// A connection left open, and where to.
struct Kept<'a, T: TcpConnect + 'a> {
    secure: bool,
    name: String,
    port: u16,
    connection: Transport<'a, T::Connection<'a>>,
}

/// One request: on the kept connection if it goes to the same place (once
/// more on a new one if that had closed before answering), else on a new
/// one. Returns the connection to keep. Redirects are never followed:
/// alias challenges need that, and the fakts server does not redirect.
async fn exchange<'a, T: TcpConnect, D: Dns>(
    tcp: &'a T,
    dns: &D,
    verify: &Verify,
    kept: Option<Kept<'a, T>>,
    request: &Request,
) -> (Result<Reply, HostError>, Option<Kept<'a, T>>) {
    let url = request.full_url();
    let failed = |e: String| HostError(format!("{url}: {e}"));
    let Some((secure, name, port, path)) = split_url(&url) else {
        return (Err(HostError(format!("not an http url: {url}"))), kept);
    };
    let body = request.encoded_body();
    let body = body.as_ref().map(|(t, b)| (*t, b.as_slice()));
    let headers: Vec<(&str, &str)> = request
        .headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let method = match request.method {
        Method::Get => "GET",
        Method::Post => "POST",
    };
    let host = authority(&name, port, secure);
    let mut reused = kept.filter(|k| k.secure == secure && k.name == name && k.port == port);
    loop {
        let fresh = reused.is_none();
        tracing::debug!(
            "{url}: on {} connection",
            if fresh { "a new" } else { "the kept" }
        );
        let mut kept = match reused.take() {
            Some(kept) => kept,
            None => match crate::conn::open_owned(tcp, dns, secure, &name, port, verify).await {
                Ok(connection) => Kept {
                    secure,
                    name: name.clone(),
                    port,
                    connection,
                },
                Err(e) => return (Err(failed(e)), None),
            },
        };
        let exchanged = crate::http1::exchange_kept(
            &mut kept.connection,
            true,
            method,
            &host,
            &path,
            &headers,
            body,
        )
        .await;
        match exchanged {
            Ok((status, body, reusable)) => {
                return (Ok(Reply { status, body }), reusable.then_some(kept));
            }
            // The server closed the kept connection (idle): once more.
            Err(e) if !fresh && e.unanswered => {
                tracing::debug!(
                    "{url}: the kept connection had closed ({}): a new one",
                    e.reason
                );
            }
            Err(e) => return (Err(failed(e.reason)), None),
        }
    }
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
