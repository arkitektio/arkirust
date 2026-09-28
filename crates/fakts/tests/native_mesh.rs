//! The native mesh backend end to end: a (mock) fakts server grants a real
//! mesh key; fakts joins tailscale's test control server with our node and
//! reaches a Go tsnet peer through the mesh alias.
#![cfg(feature = "mesh-native")]

#[path = "../../mesh/tests/common/mod.rs"]
mod common;

use std::sync::Arc;
use std::time::Duration;

use fakts::mesh::{MeshBackend, MeshOptions};
use fakts::{DeviceCodeOptions, Fakts, Grant, Manifest, Requirement};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn fakts_server(h: &common::Harness) -> MockServer {
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
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-1", "refresh_token": "refresh-1", "token_type": "Bearer",
            "expires_in": 3600, "scope": "openid", "client_id": "client-1",
            "self": {"deployment_name": "test", "alias": {"id": "self", "host": "127.0.0.1", "port": server.address().port()},
                     "sub": "12", "organization": 3, "hub": 7},
            "instances": {"hub": {"service": "live.arkitekt.hub", "identifier": "2", "aliases": [
                {"id": "mesh", "host": h.ready.peer_name, "port": 80, "challenge": "ht", "kind": "mesh"}
            ]}},
            "statuses": {"hub": "granted"},
            "mesh": {"ionscale_auth_key": h.ready.auth_key, "ionscale_coord_url": h.ready.control_url}
        })))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn fakts_reaches_a_mesh_peer_with_the_native_node() {
    let Some(h) = common::start().await else {
        return;
    };
    let server = fakts_server(&h).await;
    let state = tempfile::tempdir().unwrap();

    let mut manifest = Manifest::new("native-mesh-test", "0.1.0");
    manifest
        .requirements
        .push(Requirement::new("hub", "live.arkitekt.hub"));
    let grant = Grant::DeviceCode(Arc::new(DeviceCodeOptions {
        hook: Arc::new(|_: &str, _: &str| {}),
        ..Default::default()
    }));
    let fakts = Fakts::builder(server.uri(), manifest)
        .grant(grant)
        .no_cache(true)
        .mesh(MeshOptions {
            backend: MeshBackend::Native,
            state_root: Some(state.path().into()),
            timeout: Duration::from_secs(60),
            ..Default::default()
        })
        .load()
        .await
        .unwrap();
    let proxy = fakts
        .mesh_proxy()
        .expect("the node serves a proxy")
        .to_owned();
    assert!(proxy.starts_with("http://127.0.0.1:"), "{proxy}");

    // The alias is challenged and used through the node.
    let alias = fakts.get_alias("hub").await.unwrap();
    assert_eq!(alias.proxy(), Some(proxy.as_str()));
    let body = alias
        .http_client()
        .unwrap()
        .get(alias.to_http_path(""))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "hello from peer");

    // For clients that cannot use the proxy: a forward and a TURN relay.
    #[cfg(feature = "mesh-relay")]
    {
        let local = fakts.mesh_forward(&alias).await.unwrap();
        assert!(local.ip().is_loopback());
        let body = reqwest::get(format!("http://{local}/"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "hello from peer");
        let turn = fakts.mesh_turn().await.unwrap();
        assert!(turn.urls[0].starts_with("turn:127.0.0.1:"), "{turn:?}");
        assert_eq!(fakts.mesh_turn().await.unwrap(), turn, "one relay per node");
    }
}
