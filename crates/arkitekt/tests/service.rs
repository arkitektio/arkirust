//! `#[arkitekt::service]`: requirements come from the `#[require]` parameters.

use arkitekt::{Alias, App, Fakts, Requirement, Service};

#[derive(Debug, Clone)]
pub struct Client;

/// A client for testing.
#[arkitekt::service(name = "thing")]
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
    let manifest = App::new("app", "0.1.0")
        .service(thing)
        .service(plain)
        .manifest(None);
    let keys: Vec<_> = manifest
        .requirements
        .iter()
        .map(|r| r.key.as_str())
        .collect();
    assert_eq!(keys, ["api", "files", "endpoint"]);
}
