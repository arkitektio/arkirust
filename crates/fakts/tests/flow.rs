//! The fakts v2 flow against a mock server: discovery, device code, alias
//! challenge, refresh rotation and cache reuse.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fakts::{DeviceCodeOptions, Fakts, Grant, Manifest, Requirement, TokenLoader};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn token_response(server: &MockServer, access: &str, refresh: &str) -> serde_json::Value {
    let port = server.address().port();
    let mut response = json!({
        "access_token": access, "refresh_token": refresh, "token_type": "Bearer",
        "expires_in": 3600, "scope": "openid", "client_id": "client-1",
        "self": {"deployment_name": "test", "alias": {"id": "self", "host": "127.0.0.1", "port": port},
                 "sub": "12", "organization": 3, "hub": 7},
        "instances": {"mikro": {"service": "live.arkitekt.mikro", "identifier": "1", "aliases": [
            {"id": "dead", "host": "127.0.0.1", "port": 1, "challenge": "ht"},
            {"id": "lan", "host": "127.0.0.1", "port": port, "path": "mikro", "challenge": "ht"}
        ]},
        "hub": {"service": "live.arkitekt.hub", "identifier": "2", "aliases": [
            {"id": "mesh", "host": "meshhub.test", "port": port, "path": "mikro", "challenge": "ht", "kind": "mesh"}
        ]}},
        "statuses": {"mikro": "granted", "hub": "granted"}
    });
    // Only the first (device code) token carries the mesh key.
    if refresh == "refresh-1" {
        response["mesh"] =
            json!({"ionscale_auth_key": "mesh-key", "ionscale_coord_url": "https://mesh.test"});
    }
    response
}

async fn mount_server() -> MockServer {
    let server = MockServer::start().await;
    let uri = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/fakts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "test", "protocol_version": "2", "base_url": format!("{uri}/f/"),
            "token_endpoint": format!("{uri}/token/"),
            "device_authorization_endpoint": format!("{uri}/device/"),
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/device/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "granted", "device_code": "dev-1", "user_code": "ABCD", "client_id": "client-1",
            "verification_uri_complete": format!("{uri}/configure/ABCD"), "expires_in": 30, "interval": 1
        })))
        .expect(1)
        .mount(&server)
        .await;
    // First poll: pending. Then granted.
    Mock::given(method("POST"))
        .and(path("/token/"))
        .and(body_string_contains("device_code=dev-1"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({"error": "authorization_pending"})),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token/"))
        .and(body_string_contains("device_code=dev-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_response(
            &server,
            "access-1",
            "refresh-1",
        )))
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token/"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=refresh-1"))
        .and(body_string_contains("client_id=client-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_response(
            &server,
            "access-2",
            "refresh-2",
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/mikro/ht"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    server
}

fn manifest() -> Manifest {
    let mut manifest = Manifest::new("flow-test", "0.1.0");
    manifest
        .requirements
        .push(Requirement::new("mikro", "live.arkitekt.mikro"));
    manifest
        .requirements
        .push(Requirement::new("hub", "live.arkitekt.hub"));
    manifest
}

/// A forwarding HTTP proxy that sends every request (absolute-form or
/// `CONNECT`) to `127.0.0.1` on the requested port, whatever the host: the
/// mesh hostnames in these tests only resolve through it. Counts requests.
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
                seen.fetch_add(1, Ordering::SeqCst);
                let text = String::from_utf8_lossy(&head).to_string();
                let target = text.split_whitespace().nth(1).unwrap().to_owned();
                let authority = target
                    .strip_prefix("http://")
                    .map(|rest| rest.split('/').next().unwrap())
                    .unwrap_or(&target);
                let port: u16 = authority.rsplit(':').next().unwrap().parse().unwrap();
                let mut upstream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                if text.starts_with("CONNECT") {
                    client.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
                } else {
                    upstream.write_all(&head).await.unwrap();
                }
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    (url, count)
}

#[tokio::test]
async fn device_code_refresh_and_cache() {
    let server = mount_server().await;
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("fakts.json");

    let prompts = Arc::new(Mutex::new(vec![]));
    let seen = prompts.clone();
    let grant = Grant::DeviceCode(Arc::new(DeviceCodeOptions {
        hook: Arc::new(move |uri: &str, code: &str| {
            seen.lock().unwrap().push(format!("{uri} {code}"))
        }),
        ..Default::default()
    }));

    let fakts = Fakts::builder(server.uri(), manifest())
        .grant(grant.clone())
        .cache_path(&cache)
        .load()
        .await
        .unwrap();

    assert_eq!(prompts.lock().unwrap().len(), 1);
    assert!(prompts.lock().unwrap()[0].ends_with("/configure/ABCD ABCD"));
    assert_eq!(fakts.get_token().await.unwrap(), "access-1");

    // The unreachable alias is skipped, the next one answers its challenge.
    let alias = fakts.get_alias("mikro").await.unwrap();
    assert_eq!(alias.id, "lan");
    assert_eq!(
        alias.to_http_path("graphql"),
        format!("http://127.0.0.1:{}/mikro/graphql", server.address().port())
    );
    assert!(fakts.get_alias("kabinet").await.is_err());
    assert_eq!(alias.proxy(), None);
    // Without the mesh, the mesh-only alias is skipped.
    assert!(fakts.get_alias("hub").await.is_err());

    // The cache is private to the user.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&cache).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // A stale token that is no longer current is not refreshed again.
    assert_eq!(
        fakts.refresh_token("something-else").await.unwrap(),
        "access-1"
    );
    // Refresh rotates, and the rotated refresh token is persisted.
    assert_eq!(fakts.refresh_token("access-1").await.unwrap(), "access-2");
    let cached: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
    assert_eq!(cached["fakts"]["auth"]["refresh_token"], "refresh-2");
    // The mesh key outlives the refresh that did not carry it.
    assert_eq!(cached["fakts"]["mesh"]["ionscale_auth_key"], "mesh-key");
    let active = fakts.active().await;
    assert!(active.mesh.is_some());
    let me = active.self_.unwrap();
    assert_eq!(
        (
            me.sub.as_deref(),
            me.organization.as_deref(),
            me.hub.as_deref()
        ),
        (Some("12"), Some("3"), Some("7"))
    );

    // A second load comes from the cache: no new device code (the mock expects exactly one).
    let again = Fakts::builder(server.uri(), manifest())
        .grant(grant)
        .cache_path(&cache)
        .load()
        .await
        .unwrap();
    assert_eq!(again.get_token().await.unwrap(), "access-2");
    assert_eq!(prompts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn refuses_protocol_v1() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/fakts"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"name": "old", "token_endpoint": "x"})),
        )
        .mount(&server)
        .await;
    let err = Fakts::builder(server.uri(), manifest())
        .no_cache(true)
        .load()
        .await
        .unwrap_err();
    assert!(err.to_string().contains("protocol 1"), "{err}");
}

#[tokio::test]
async fn mesh_aliases_go_through_the_proxy() {
    let server = mount_server().await;
    let (proxy, count) = start_proxy().await;
    let grant = Grant::DeviceCode(Arc::new(DeviceCodeOptions {
        hook: Arc::new(|_: &str, _: &str| {}),
        ..Default::default()
    }));
    let fakts = Fakts::builder(server.uri(), manifest())
        .grant(grant)
        .no_cache(true)
        .mesh_proxy(&proxy)
        .load()
        .await
        .unwrap();

    // Direct aliases stay direct.
    let direct = fakts.get_alias("mikro").await.unwrap();
    assert_eq!(direct.proxy(), None);
    assert_eq!(count.load(Ordering::SeqCst), 0);

    // `meshhub.test` only resolves through the proxy.
    let mesh = fakts.get_alias("hub").await.unwrap();
    assert_eq!(mesh.id, "mesh");
    assert_eq!(mesh.proxy(), Some(proxy.as_str()));
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // Clients built from the alias use the proxy too.
    let resp = mesh
        .http_client()
        .unwrap()
        .get(mesh.to_http_path("ht"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    assert_eq!(count.load(Ordering::SeqCst), 2);
}
