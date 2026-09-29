//! The canonical agent-socket frames (`tests/fixtures/agent_wire.json`, a
//! copy of the server's fixture): every frame parses and re-serializes
//! without losing or adding a field.

use rekuest::messages::{Envelope, ToAgentFrame};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/agent_wire.json")).unwrap()
}

fn frames<'a>(fixture: &'a Value, group: &str) -> Vec<(&'a str, &'a Value)> {
    let frames = fixture[group].as_array().unwrap();
    assert!(!frames.is_empty(), "{group} has frames");
    frames
        .iter()
        .map(|f| (f["name"].as_str().unwrap(), &f["frame"]))
        .collect()
}

#[test]
fn from_agent_frames_round_trip() {
    let fixture = fixture();
    for group in ["numbered_from_agent", "unnumbered_from_agent"] {
        for (name, frame) in frames(&fixture, group) {
            let envelope: Envelope = serde_json::from_value(frame.clone())
                .unwrap_or_else(|e| panic!("{name} does not parse: {e}"));
            assert_eq!(
                envelope.pos.is_some(),
                group == "numbered_from_agent",
                "{name}: numbering"
            );
            assert_eq!(
                envelope.message.is_probe(),
                name.starts_with("probe"),
                "{name}: probe"
            );
            let back = serde_json::to_value(&envelope).unwrap();
            assert_eq!(&back, frame, "{name} round-trips");
        }
    }
}

#[test]
fn to_agent_frames_round_trip() {
    let fixture = fixture();
    for (name, frame) in frames(&fixture, "to_agent") {
        let parsed: ToAgentFrame = serde_json::from_value(frame.clone())
            .unwrap_or_else(|e| panic!("{name} does not parse: {e}"));
        assert!(
            !matches!(parsed.message, rekuest::messages::ToAgent::Unknown),
            "{name} is understood"
        );
        let back = serde_json::to_value(&parsed).unwrap();
        assert_eq!(&back, frame, "{name} round-trips");
    }
}

#[test]
fn child_call_step_belongs_to_the_request() {
    let fixture = fixture();
    let frame = frames(&fixture, "unnumbered_from_agent")
        .into_iter()
        .find(|(name, _)| *name == "child_call")
        .map(|(_, frame)| frame.clone())
        .expect("child_call frame");
    let envelope: Envelope = serde_json::from_value(frame).unwrap();
    assert_eq!(envelope.task_step, None, "the envelope stays unnumbered");
    match envelope.message {
        rekuest::messages::FromAgent::AssignRequest {
            parent_step,
            reference,
            parent,
            ..
        } => {
            assert_eq!(parent_step, Some(8));
            assert_eq!(
                reference.as_deref(),
                Some("c0ffee00c0ffee00c0ffee00c0ffee00")
            );
            assert_eq!(parent.as_deref(), Some("42"));
        }
        other => panic!("not an ASSIGN_REQUEST: {other:?}"),
    }
}
