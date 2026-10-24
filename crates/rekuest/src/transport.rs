//! Opening the agent websocket over a stream a [`Connector`] opens: a plain
//! TCP socket ([`Direct`]), a tunnel through an HTTP proxy ([`HttpProxy`], the
//! mesh sidecar's), or anything else that carries bytes, such as a
//! connection through a mesh node the app runs itself. TLS (`wss://`) runs
//! on top of whichever it is.

use std::fmt::Debug;
use std::io;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Response;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::Error;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// A byte stream the agent's websocket can run over.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// Opens the connection to the agent endpoint's host and port.
#[async_trait]
pub trait Connector: Send + Sync + Debug {
    async fn connect(&self, host: &str, port: u16) -> io::Result<Box<dyn Stream>>;
}

/// A plain TCP connection.
#[derive(Debug, Clone, Copy, Default)]
pub struct Direct;

#[async_trait]
impl Connector for Direct {
    async fn connect(&self, host: &str, port: u16) -> io::Result<Box<dyn Stream>> {
        Ok(Box::new(TcpStream::connect((host, port)).await?))
    }
}

/// A tunnel through an HTTP proxy (`http://host:port`) with `CONNECT`, e.g.
/// the mesh sidecar's local proxy.
#[derive(Debug, Clone)]
pub struct HttpProxy {
    proxy: String,
}

impl HttpProxy {
    pub fn new(proxy: impl Into<String>) -> Self {
        Self {
            proxy: proxy.into(),
        }
    }
}

#[async_trait]
impl Connector for HttpProxy {
    async fn connect(&self, host: &str, port: u16) -> io::Result<Box<dyn Stream>> {
        Ok(Box::new(tunnel(&self.proxy, host, port).await?))
    }
}

pub type Ws = WebSocketStream<MaybeTlsStream<Box<dyn Stream>>>;

/// Connect to `url` (`ws://` or `wss://`) over a stream `connector` opens.
pub async fn connect_ws(url: &str, connector: &dyn Connector) -> Result<(Ws, Response), Error> {
    let request = url.into_client_request()?;
    let uri = request.uri();
    let host = uri.host().ok_or(Error::Url(
        tokio_tungstenite::tungstenite::error::UrlError::NoHostName,
    ))?;
    // An IPv6 literal comes bracketed; a socket wants the bare address.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = uri
        .port_u16()
        .unwrap_or(if uri.scheme_str() == Some("wss") {
            443
        } else {
            80
        });
    let stream = connector.connect(host, port).await?;
    tokio_tungstenite::client_async_tls_with_config(request, stream, None, None).await
}

/// Open a TCP tunnel to `host:port` through the HTTP proxy at `proxy`.
async fn tunnel(proxy: &str, host: &str, port: u16) -> io::Result<TcpStream> {
    let invalid = |what: String| io::Error::new(io::ErrorKind::InvalidInput, what);
    let proxy_uri: Uri = proxy
        .parse()
        .map_err(|e| invalid(format!("invalid proxy url {proxy}: {e}")))?;
    let proxy_host = proxy_uri
        .host()
        .ok_or_else(|| invalid(format!("proxy url {proxy} has no host")))?;
    let proxy_port = proxy_uri.port_u16().unwrap_or(80);

    let mut stream = TcpStream::connect((proxy_host, proxy_port)).await?;
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    stream
        .write_all(format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes())
        .await?;

    // Read the response head byte by byte: whatever follows it belongs to
    // the tunnelled connection.
    let mut head = Vec::with_capacity(128);
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 8192 {
            return Err(invalid("proxy sent an oversized CONNECT response".into()));
        }
        let byte = stream.read_u8().await?;
        head.push(byte);
    }
    let status_line = String::from_utf8_lossy(&head);
    let status_line = status_line.lines().next().unwrap_or_default();
    let status = status_line.split_whitespace().nth(1).unwrap_or_default();
    if status != "200" {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("proxy {proxy} refused CONNECT {authority}: {status_line}"),
        ));
    }
    Ok(stream)
}
