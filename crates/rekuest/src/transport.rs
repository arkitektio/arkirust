//! Opening the agent websocket, optionally through an HTTP proxy (the mesh
//! node's local proxy).

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Response;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::Error;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Connect to `url` (`ws://` or `wss://`), tunnelling through `proxy`
/// (`http://host:port`) with `CONNECT` when given.
pub async fn connect_ws(url: &str, proxy: Option<&str>) -> Result<(Ws, Response), Error> {
    let Some(proxy) = proxy else {
        return tokio_tungstenite::connect_async(url).await;
    };
    let request = url.into_client_request()?;
    let uri = request.uri();
    let host = uri.host().ok_or(Error::Url(
        tokio_tungstenite::tungstenite::error::UrlError::NoHostName,
    ))?;
    let port = uri
        .port_u16()
        .unwrap_or(if uri.scheme_str() == Some("wss") {
            443
        } else {
            80
        });
    let stream = tunnel(proxy, host, port).await?;
    tokio_tungstenite::client_async_tls_with_config(request, stream, None, None).await
}

/// Open a TCP tunnel to `host:port` through the HTTP proxy at `proxy`.
async fn tunnel(proxy: &str, host: &str, port: u16) -> Result<TcpStream, Error> {
    let invalid =
        |what: String| Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, what));
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
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            format!("proxy {proxy} refused CONNECT {authority}: {status_line}"),
        )));
    }
    Ok(stream)
}
