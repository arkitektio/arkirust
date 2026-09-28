//! The datalayer store actually reaches plain-http endpoints (local and mesh
//! deployments), directly or through the mesh proxy.

use std::time::Duration;

use arkitekt::fakts::Alias;
use mikro::{DataLayer, Grant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use zarrs::storage::AsyncReadableStorageTraits;

fn grant() -> Grant {
    Grant {
        access_key: "a".into(),
        secret_key: "s".into(),
        session_token: "t".into(),
        bucket: "b".into(),
        key: "k".into(),
    }
}

/// Answers one request with a 404 and returns its request head.
async fn one_request() -> (u16, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 8192];
        let n = conn.read(&mut buf).await.unwrap();
        let _ = conn
            .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await;
        String::from_utf8_lossy(&buf[..n]).into_owned()
    });
    (port, server)
}

async fn served(server: tokio::task::JoinHandle<String>) -> String {
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the request never arrived")
        .unwrap()
}

#[tokio::test]
async fn plain_http_endpoints_are_reached() {
    let (port, server) = one_request().await;
    let store = DataLayer::new(format!("http://127.0.0.1:{port}"))
        .store(&grant())
        .unwrap();
    // A 404 is "not found", not an error.
    assert!(store
        .get(&"zarr.json".try_into().unwrap())
        .await
        .unwrap()
        .is_none());
    assert!(served(server).await.starts_with("GET /b/k/zarr.json "));
}

#[tokio::test]
async fn mesh_aliases_go_through_the_proxy() {
    let (proxy_port, proxy) = one_request().await;
    let alias: Alias = serde_json::from_value(serde_json::json!({
        "id": "s3", "host": "100.64.0.9", "port": 8333, "ssl": false,
        "path": null, "challenge": "ht", "kind": "mesh",
    }))
    .unwrap();
    let alias = Alias {
        proxy: Some(format!("http://127.0.0.1:{proxy_port}")),
        ..alias
    };
    let store = DataLayer::from_alias(&alias).store(&grant()).unwrap();
    let _ = store.get(&"zarr.json".try_into().unwrap()).await;
    // The proxy gets the absolute-form request for the mesh host.
    let head = served(proxy).await;
    assert!(
        head.starts_with("GET http://100.64.0.9:8333/b/k/zarr.json "),
        "{head}"
    );
}
