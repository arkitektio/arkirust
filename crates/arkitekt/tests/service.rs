//! `#[arkitekt::service]`: requirements come from the `#[require]` parameters.

use arkitekt::{Alias, App, Fakts, Requirement, Service};

#[derive(Debug, Clone)]
pub struct Client;

/// A client for testing.
#[arkitekt::service(name = "thing", scopes = ["thing_write", "thing_read"])]
pub async fn thing(
    #[require("live.arkitekt.thing", "Where things live")] api: Alias,
    #[require("live.arkitekt.s3")] files: Option<Alias>,
    fakts: Fakts,
) -> anyhow::Result<Client> {
    let _ = (api, files, fakts);
    Ok(Client)
}

#[arkitekt::service]
pub fn plain(#[require("live.arkitekt.plain")] endpoint: Alias) -> Client {
    let _ = endpoint;
    Client
}

#[test]
fn requirements_from_parameters() {
    assert_eq!(thing.name(), "thing");
    assert_eq!(
        thing.requirements(),
        vec![
            Requirement::new("api", "live.arkitekt.thing").description("Where things live"),
            Requirement::new("files", "live.arkitekt.s3").optional(true),
        ]
    );
    assert_eq!(plain.name(), "plain");
    assert_eq!(
        plain.requirements(),
        vec![Requirement::new("endpoint", "live.arkitekt.plain")]
    );
}

#[test]
fn manifest_collects_service_requirements() {
    let manifest = arkitekt::manifest(
        &App::new("app", "0.1.0").service(thing).service(plain),
        None,
    );
    let keys: Vec<_> = manifest
        .requirements
        .iter()
        .map(|r| r.key.as_str())
        .collect();
    assert_eq!(keys, ["api", "files", "endpoint"]);
}

/// As the Python client's: `openid`, the app's own, and its services'.
#[test]
fn manifest_scopes_are_openid_the_apps_and_its_services() {
    assert_eq!(thing.scopes(), ["thing_write", "thing_read"]);
    assert!(plain.scopes().is_empty());
    let manifest = arkitekt::manifest(
        &App::new("app", "0.1.0")
            .scopes(["thing_read", "own"])
            .service(thing)
            .service(plain),
        None,
    );
    assert_eq!(
        manifest.scopes,
        ["openid", "own", "thing_read", "thing_write"]
    );
}

/// A rekuest agent asks for rekuest's scopes, as `run(app)` does in Python.
#[test]
fn an_agent_asks_for_rekuests_scopes() {
    #[arkitekt::action]
    async fn noop() {}

    let agent = arkitekt::manifest(&App::new("app", "0.1.0").action(noop), None);
    assert_eq!(
        agent.scopes,
        ["openid", "rekuest_agent", "rekuest_read", "rekuest_write"]
    );
    let no_actions = arkitekt::manifest(&App::new("app", "0.1.0"), None);
    assert_eq!(no_actions.scopes, ["openid"]);
}

/// Desktop apps spawn these: they must stay `Send` through the platform.
#[test]
fn the_tokio_platforms_futures_are_send() {
    fn is_send<T: Send>(_: T) {}
    is_send(arkitekt::run(App::new("a", "1")));
    is_send(arkitekt::connect_with(
        App::new("a", "1"),
        arkitekt::ConnectOptions::default(),
    ));
}
