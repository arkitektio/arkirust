//! The ArrayDataset port against what Python's mikro registers.

use arkitekt::fakts::Manifest;
use arkitekt::rekuest::PortType;
use arkitekt::App;
use mikro::ArrayDataset;
use serde_json::Value;

#[test]
fn arraydataset_port_matches_python() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/python_declaration.json")).unwrap();
    // `rescale(image: ArrayDataset, ...)` in the fixture.
    let python = &fixture["implementations"][1]["definition"]["args"][0];
    let rust = serde_json::to_value(<ArrayDataset as PortType>::port("image")).unwrap();

    assert_eq!(rust["kind"], python["kind"]);
    assert_eq!(rust["identifier"], python["identifier"]);
    // Python spells unset widget fields as explicit nulls.
    let mut python_widget = python["widget"].clone();
    python_widget
        .as_object_mut()
        .unwrap()
        .retain(|_, v| !v.is_null());
    assert_eq!(rust["widget"], python_widget);
}

#[test]
fn manifest_requires_mikro_s3_and_rekuest() {
    #[arkitekt::action]
    async fn noop() {}

    let manifest: Manifest = App::new("app", "1")
        .service(mikro::service)
        .action(noop)
        .manifest(None);
    let keys: Vec<(&str, &str)> = manifest
        .requirements
        .iter()
        .map(|r| (r.key.as_str(), r.service.as_str()))
        .collect();
    assert_eq!(
        keys,
        vec![
            ("rekuest", "live.arkitekt.rekuest"),
            ("mikro", "live.arkitekt.mikro"),
            ("s3", "live.arkitekt.s3"),
        ]
    );

    // A script that only uses mikro does not ask for rekuest.
    let script = arkitekt::easy("script", "1")
        .service(mikro::service)
        .manifest(None);
    assert!(script.requirements.iter().all(|r| r.key != "rekuest"));
}
