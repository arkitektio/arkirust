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
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;

use async_trait::async_trait;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embedded_nal_async::{Dns, TcpConnect};
use fakts::host::{Host, HostError, Method, Reply, Request};
use reqwless::client::HttpClient;
use reqwless::request::RequestBuilder;

/// A response's headers must fit; its body is read into memory as it comes.
pub const HEADER_BUFFER: usize = 8 * 1024;
/// Refuse bodies larger than this.
pub const MAX_BODY: usize = 16 * 1024 * 1024;
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
///
/// Plain `http://` only for now: `https://` is answered with an error (the
/// TLS reqwless 0.14 brings, embedded-tls 0.18, does not build).
pub async fn serve_http<T: TcpConnect, D: Dns>(host: &'static NetHost, tcp: &T, dns: &D) -> ! {
    let mut rx = vec![0u8; HEADER_BUFFER];
    loop {
        let (id, request) = host.requests.receive().await;
        if request.url.starts_with("https://") {
            let refused = HostError(format!(
                "{}: https is not supported by arkitekt-embassy yet",
                request.url
            ));
            host.replies.send((id, Err(refused))).await;
            continue;
        }
        let timeout = request.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let mut client = HttpClient::new(tcp, dns);
        let reply = match embassy_time::with_timeout(
            embassy_time::Duration::from_micros(timeout.as_micros() as u64),
            exchange(&mut client, &request, &mut rx),
        )
        .await
        {
            Ok(reply) => reply,
            Err(_) => Err(HostError(format!("{} did not answer in time", request.url))),
        };
        host.replies.send((id, reply)).await;
    }
}

async fn exchange<T: TcpConnect, D: Dns>(
    client: &mut HttpClient<'_, T, D>,
    request: &Request,
    rx: &mut [u8],
) -> Result<Reply, HostError> {
    let failed = |e: reqwless::Error| HostError(format!("{}: {e:?}", request.url));
    let url = request.full_url();
    let method = match request.method {
        Method::Get => reqwless::request::Method::GET,
        Method::Post => reqwless::request::Method::POST,
    };
    let body = request.encoded_body();
    let mut headers: Vec<(&str, &str)> = request
        .headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    if let Some((content_type, _)) = &body {
        headers.push(("content-type", content_type));
    }
    // One connection per exchange: a body without a length then ends where
    // the connection does.
    headers.push(("connection", "close"));
    let handle = client.request(method, &url).await.map_err(failed)?;
    let mut handle = handle.headers(&headers);
    // Redirects are never followed here: alias challenges need that, and
    // the fakts server does not redirect.
    match &body {
        Some((_, bytes)) => {
            let mut with_body = handle.body(bytes.as_slice());
            let response = with_body.send(rx).await.map_err(failed)?;
            let status = response.status.0;
            let body = read_body(response.body().reader()).await.map_err(failed)?;
            Ok(Reply { status, body })
        }
        None => {
            let response = handle.send(rx).await.map_err(failed)?;
            let status = response.status.0;
            let body = read_body(response.body().reader()).await.map_err(failed)?;
            Ok(Reply { status, body })
        }
    }
}

/// The whole body, however large (GraphQL answers can be many KiB).
async fn read_body<R: embedded_io_async::Read<Error = reqwless::Error>>(
    mut reader: R,
) -> Result<Vec<u8>, reqwless::Error> {
    let mut body = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            return Ok(body);
        }
        body.extend_from_slice(&chunk[..n]);
        if body.len() > MAX_BODY {
            return Err(reqwless::Error::BufferTooSmall);
        }
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
