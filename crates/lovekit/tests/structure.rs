//! The lovekit service and structures against what Python's lovekit declares.

use arkitekt::fakts::Manifest;
use arkitekt::rekuest::PortType;
use arkitekt::App;
use lovekit::{SoloBroadcast, Stream};

#[test]
fn manifest_requires_lovekit_and_livekit() {
    #[arkitekt::action]
    async fn noop() {}

    let manifest: Manifest = App::new("app", "1")
        .service(lovekit::service)
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
            ("lovekit", "live.arkitekt.lovekit"),
            ("livekit", "io.livekit.livekit"),
        ]
    );
}

#[test]
fn structures_are_pythons() {
    for (port, identifier, query) in [
        (
            <Stream as PortType>::port("stream"),
            "@lovekit/stream",
            "SearchStreams",
        ),
        (
            <SoloBroadcast as PortType>::port("broadcast"),
            "@lovekit/solo_broadcast",
            "SearchSoloBroadcast",
        ),
    ] {
        let port = serde_json::to_value(port).unwrap();
        assert_eq!(port["kind"], "STRUCTURE");
        assert_eq!(port["identifier"], identifier);
        assert_eq!(port["widget"]["kind"], "SEARCH");
        assert_eq!(port["widget"]["ward"], "lovekit");
        assert!(port["widget"]["query"].as_str().unwrap().contains(query));
    }
}

#[test]
fn stream_kinds_travel_as_the_schema_spells_them() {
    assert_eq!(
        serde_json::to_value(lovekit::StreamKind::Video).unwrap(),
        "VIDEO"
    );
    let stream: Stream =
        serde_json::from_value(serde_json::json!({"id": "1", "title": "cam", "kind": "AUDIO"}))
            .unwrap();
    assert_eq!(stream.kind, lovekit::StreamKind::Audio);
}
