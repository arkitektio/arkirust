//! `connect_ws` runs over what a connector opens: a tunnel through an HTTP
//! proxy with `CONNECT`, or a stream of the app's own.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::{SinkExt, StreamExt};
use rekuest::transport::{connect_ws, Connector, HttpProxy, Stream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

/// A websocket server that echoes one message.
async fn start_echo() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        if let Some(Ok(msg)) = ws.next().await {
            ws.send(msg).await.unwrap();
        }
    });
    port
}

/// A `CONNECT` proxy that sends every tunnel to 127.0.0.1 on the requested
/// port (so `meshhub.test` only resolves through it). Counts tunnels.
async fn start_proxy() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    tokio::spawn(async move {
        loop {
            let (mut client, _) = listener.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut head = vec![];
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(client.read_u8().await.unwrap());
                }
                let text = String::from_utf8_lossy(&head).to_string();
                assert!(text.starts_with("CONNECT meshhub.test:"), "{text}");
                seen.fetch_add(1, Ordering::SeqCst);
                let authority = text.split_whitespace().nth(1).unwrap();
                let port: u16 = authority.rsplit(':').next().unwrap().parse().unwrap();
                let mut upstream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                client
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    (url, count)
}

#[tokio::test]
async fn tunnels_through_the_proxy() {
    let port = start_echo().await;
    let (proxy, count) = start_proxy().await;

    let (mut ws, _) = connect_ws(
        &format!("ws://meshhub.test:{port}/agi"),
        &HttpProxy::new(proxy),
    )
    .await
    .unwrap();
    ws.send(Message::text("hello")).await.unwrap();
    let echoed = ws.next().await.unwrap().unwrap();
    assert_eq!(echoed, Message::text("hello"));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn refused_connect_is_an_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        let mut head = vec![];
        while !head.ends_with(b"\r\n\r\n") {
            head.push(client.read_u8().await.unwrap());
        }
        client
            .write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n")
            .await
            .unwrap();
    });
    let Err(err) = connect_ws("ws://meshhub.test:80/agi", &HttpProxy::new(proxy)).await else {
        panic!("a refused CONNECT connected");
    };
    assert!(err.to_string().contains("403"), "{err}");
}

/// Resolves every host itself, as a mesh node's `dial` does: here to the
/// echo server on 127.0.0.1. Records what it was asked for.
#[derive(Debug, Default)]
struct OwnConnector {
    asked: Mutex<Vec<(String, u16)>>,
}

#[async_trait::async_trait]
impl Connector for OwnConnector {
    async fn connect(&self, host: &str, port: u16) -> io::Result<Box<dyn Stream>> {
        self.asked.lock().unwrap().push((host.to_owned(), port));
        Ok(Box::new(TcpStream::connect(("127.0.0.1", port)).await?))
    }
}

#[tokio::test]
async fn runs_over_a_stream_of_the_apps_own() {
    let port = start_echo().await;
    let connector = OwnConnector::default();

    let (mut ws, _) = connect_ws(&format!("ws://meshhub.test:{port}/agi"), &connector)
        .await
        .unwrap();
    ws.send(Message::text("hello")).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap(), Message::text("hello"));
    assert_eq!(
        *connector.asked.lock().unwrap(),
        vec![("meshhub.test".to_owned(), port)]
    );
}
