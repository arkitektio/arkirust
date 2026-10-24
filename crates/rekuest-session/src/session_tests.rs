//! The session with frames as strings and time as numbers: no socket.

use core::time::Duration;
use std::vec::Vec;

use serde_json::{json, Value};

use crate::prelude::*;
use crate::protocol::definition::AgentDeclaration;
use crate::protocol::messages::{FromAgent, ToAgent};
use crate::session::{Event, Session, SessionConfig, SessionError, IDLE_TIMEOUT};
use crate::testing::{Echo, FakeServer, Numberer, ServerOutput};
use crate::{to_text, ConnectionPolicy, Outbox};

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

fn declaration() -> AgentDeclaration {
    serde_json::from_value(json!({"name": "test"})).unwrap()
}

fn config() -> SessionConfig {
    SessionConfig {
        policy: ConnectionPolicy {
            jitter: 0.0,
            ..Default::default()
        },
        ..SessionConfig::new("s", 7)
    }
}

fn events(session: &mut Session) -> Vec<Event> {
    core::iter::from_fn(|| session.poll_event()).collect()
}

fn sent(session: &mut Session) -> Vec<Value> {
    core::iter::from_fn(|| session.poll_transmit())
        .map(|e| serde_json::from_str(&to_text(&e)).unwrap())
        .collect()
}

fn types(frames: &[Value]) -> Vec<&str> {
    frames.iter().map(|f| f["type"].as_str().unwrap()).collect()
}

/// Connected and registered (REGISTER taken), at `now`; earlier events are dropped.
fn registering(session: &mut Session, now: Duration) {
    events(session);
    session.handle_timeout(now);
    assert_eq!(events(session), [Event::Connect]);
    session.connected(now, "token".into(), declaration());
    assert_eq!(types(&sent(session)), ["REGISTER"]);
}

fn init(session: &mut Session, now: Duration) {
    session.handle_text(
        now,
        r#"{"type":"INIT","id":"i","agent":"a","inquiries":[],"diagnostics":[]}"#,
    );
}

fn live(session: &mut Session, now: Duration) {
    registering(session, now);
    init(session, now);
    let events = events(session);
    assert!(events.contains(&Event::Activate) || !events.is_empty());
}

fn assign(task: &str) -> String {
    json!({
        "type": "ASSIGN", "id": "x", "interface": "echo", "task": task, "args": {},
        "user": "u", "org": "o", "action": "a", "implementation": "i", "token": "t",
    })
    .to_string()
}

fn log(task: &str) -> FromAgent {
    FromAgent::Log {
        task: task.into(),
        message: "m".into(),
        level: Default::default(),
    }
}

#[test]
fn registers_with_the_session_and_declaration() {
    let mut session = Session::new(SessionConfig {
        force: true,
        ..config()
    });
    assert_eq!(
        session.poll_timeout(),
        Some(Duration::ZERO),
        "connects at once"
    );
    session.handle_timeout(Duration::ZERO);
    assert_eq!(events(&mut session), [Event::Connect]);
    session.connected(Duration::ZERO, "tok".into(), declaration());
    let register = &sent(&mut session)[0];
    assert_eq!(
        (
            &register["type"],
            &register["token"],
            &register["force"],
            &register["session_id"],
            &register["name"]
        ),
        (
            &json!("REGISTER"),
            &json!("tok"),
            &json!(true),
            &json!("s"),
            &json!("test")
        )
    );
    assert_eq!(register["id"].as_str().unwrap().len(), 36, "a uuid");
}

#[test]
fn frames_before_init_wait_for_it_heartbeats_excepted() {
    let mut session = Session::new(config());
    registering(&mut session, secs(0));
    session.handle_text(secs(1), r#"{"type":"COLLECT","id":"c","drawers":["x"]}"#);
    session.handle_text(secs(1), r#"{"type":"HEARTBEAT","id":"h"}"#);
    assert_eq!(types(&sent(&mut session)), ["HEARTBEAT_ANSWER"]);
    assert_eq!(events(&mut session), []);

    init(&mut session, secs(2));
    let events = events(&mut session);
    assert!(matches!(events[0], Event::Registered { ref agent, .. } if agent == "a"));
    assert_eq!(events[1], Event::Activate);
    assert_eq!(
        events[2],
        Event::Message(ToAgent::Collect {
            drawers: vec!["x".into()]
        })
    );
}

#[test]
fn task_messages_wait_for_activation_in_order() {
    let mut session = Session::new(config());
    live(&mut session, secs(0));
    session.handle_text(secs(1), &assign("t1"));
    session.handle_text(secs(1), r#"{"type":"CANCEL","id":"c","task":"t1"}"#);
    session.handle_text(secs(1), r#"{"type":"COLLECT","id":"d","drawers":["x"]}"#);
    assert_eq!(
        events(&mut session),
        [Event::Message(ToAgent::Collect {
            drawers: vec!["x".into()]
        })],
        "the collect goes on, the task messages are held"
    );
    session.activated(Ok(()));
    let released = events(&mut session);
    assert!(
        matches!(
            &released[..],
            [Event::Message(ToAgent::Assign(a)), Event::Message(ToAgent::Cancel { task })]
                if a.task == "t1" && task == "t1"
        ),
        "the cancel cannot overtake its assignment: {released:?}"
    );
    session.handle_text(secs(2), &assign("t2"));
    assert!(matches!(
        &events(&mut session)[..],
        [Event::Message(ToAgent::Assign(a))] if a.task == "t2"
    ));
}

#[test]
fn a_close_before_registration_is_a_bounce() {
    let mut session = Session::new(config());
    registering(&mut session, secs(0));
    session.handle_disconnect(secs(1), "gone");
    registering(&mut session, secs(2));
    session.handle_close(secs(3), 3004, "too early");
    assert_eq!(events(&mut session), [Event::Close]);
    assert_eq!(session.poll_timeout(), Some(secs(4)));
    registering(&mut session, secs(4));
    session.handle_disconnect(secs(5), "gone");
    assert_eq!(
        session.poll_timeout(),
        Some(secs(6)),
        "the budget was reset"
    );
}

#[test]
fn activation_runs_once_across_reconnects() {
    let mut session = Session::new(config());
    live(&mut session, secs(0));
    session.activated(Ok(()));
    session.handle_disconnect(secs(1), "gone");
    let delay = session.poll_timeout().unwrap();
    registering(&mut session, delay);
    init(&mut session, delay);
    assert!(!events(&mut session).contains(&Event::Activate));
}

#[test]
fn an_activation_failure_ends_the_session_even_while_disconnected() {
    let mut session = Session::new(config());
    live(&mut session, secs(0));
    session.handle_disconnect(secs(1), "gone");
    events(&mut session);
    session.activated(Err("startup hook failed".into()));
    assert_eq!(
        events(&mut session),
        [
            Event::Close,
            Event::Ended(SessionError::Activation("startup hook failed".into()))
        ]
    );
    assert!(session.is_ended());
    assert_eq!(session.poll_timeout(), None);
}

#[test]
fn unacknowledged_frames_are_resent_in_order_after_a_reconnect() {
    let mut session = Session::new(config());
    let mut journal = Numberer::new("s");
    live(&mut session, secs(0));
    for _ in 0..3 {
        let message = log("t");
        let entry = journal.entry(&message);
        session.send_entry(message, &entry);
    }
    assert_eq!(sent(&mut session).len(), 3);
    session.handle_text(
        secs(1),
        r#"{"type":"JOURNAL_ACK","id":"a","journal_session":"s","pos":1}"#,
    );
    assert_eq!(
        events(&mut session),
        [Event::Acked {
            session: "s".into(),
            pos: 1
        }]
    );

    session.handle_close(secs(2), 1006, "abnormal");
    assert_eq!(events(&mut session), [Event::Close]);
    // Reported while disconnected: kept for the next connection.
    let message = log("t");
    let entry = journal.entry(&message);
    session.send_entry(message, &entry);
    assert!(sent(&mut session).is_empty());

    let at = session.poll_timeout().unwrap();
    registering(&mut session, at);
    assert!(
        sent(&mut session).is_empty(),
        "nothing between REGISTER and INIT"
    );
    init(&mut session, at);
    let resent: Vec<(u64, u64)> = sent(&mut session)
        .iter()
        .map(|f| (f["pos"].as_u64().unwrap(), f["seq"].as_u64().unwrap()))
        .collect();
    assert_eq!(resent, [(2, 1), (3, 2), (4, 3)]);
}

#[test]
fn a_spilled_outbox_is_reloaded_before_resending_heartbeats_go_on() {
    let mut session = Session::with_outbox(config(), Outbox::with_limit(2, true));
    let mut journal = Numberer::new("s");
    let mut written = vec![];
    for _ in 0..3 {
        let message = log("t");
        let entry = journal.entry(&message);
        session.send_entry(message, &entry);
        written.push(entry);
    }
    registering(&mut session, secs(0));
    session.handle_text(secs(0), &assign("t9"));
    init(&mut session, secs(0));
    assert!(events(&mut session).contains(&Event::ReloadJournal));
    session.handle_text(secs(1), r#"{"type":"HEARTBEAT","id":"h"}"#);
    assert_eq!(types(&sent(&mut session)), ["HEARTBEAT_ANSWER"]);
    assert!(!session.is_live());

    session.reloaded(Some(written));
    let positions: Vec<u64> = sent(&mut session)
        .iter()
        .map(|f| f["pos"].as_u64().unwrap())
        .collect();
    assert_eq!(
        positions,
        [1, 2],
        "up to the limit, the rest stays in the journal"
    );
    assert_eq!(events(&mut session), [Event::Activate]);
    session.activated(Ok(()));
    assert!(matches!(
        &events(&mut session)[..],
        [Event::Message(ToAgent::Assign(a))] if a.task == "t9"
    ));
}

#[test]
fn fatal_close_codes_kicks_and_refusals_end_the_session() {
    for (frame, close, expected) in [
        (
            None,
            Some((4003, "blocked")),
            SessionError::Closed {
                code: 4003,
                reason: "blocked".into(),
            },
        ),
        (
            Some(r#"{"type":"KICK","id":"k","reason":"bye"}"#),
            None,
            SessionError::Kicked(Some("bye".into())),
        ),
        (
            Some(r#"{"type":"PROTOCOL_ERROR","id":"p","error":"bad"}"#),
            None,
            SessionError::Refused("bad".into()),
        ),
    ] {
        let mut session = Session::new(config());
        registering(&mut session, secs(0));
        if let Some(frame) = frame {
            session.handle_text(secs(1), frame);
        }
        if let Some((code, reason)) = close {
            session.handle_close(secs(1), code, reason);
        }
        assert_eq!(events(&mut session), [Event::Close, Event::Ended(expected)]);
    }
}

#[test]
fn backoff_doubles_and_gives_up() {
    let mut session = Session::new(SessionConfig {
        policy: ConnectionPolicy {
            max_retries: 3,
            jitter: 0.0,
            ..Default::default()
        },
        ..config()
    });
    let mut now = secs(0);
    let mut waits = vec![];
    loop {
        session.handle_timeout(now);
        assert_eq!(events(&mut session), [Event::Connect]);
        session.connect_failed(now, "refused");
        if session.is_ended() {
            assert_eq!(
                events(&mut session),
                [Event::Close, Event::Ended(SessionError::Exhausted(3))]
            );
            break;
        }
        assert_eq!(events(&mut session), [Event::Close]);
        let next = session.poll_timeout().unwrap();
        waits.push((next - now).as_secs());
        now = next;
    }
    assert_eq!(waits, [1, 2, 4]);
}

#[test]
fn a_connection_that_held_resets_the_budget_and_bounce_its_own_delay() {
    let mut session = Session::new(config());
    registering(&mut session, secs(0));
    session.handle_disconnect(secs(1), "gone");
    assert_eq!(session.poll_timeout(), Some(secs(2)), "attempt 1: 1 s");
    registering(&mut session, secs(2));
    session.handle_disconnect(secs(3), "gone");
    assert_eq!(session.poll_timeout(), Some(secs(5)), "attempt 2: 2 s");
    registering(&mut session, secs(5));
    session.handle_disconnect(secs(100), "gone");
    assert_eq!(
        session.poll_timeout(),
        Some(secs(101)),
        "held 95 s: back to 1 s"
    );

    registering(&mut session, secs(101));
    session.handle_text(secs(102), r#"{"type":"BOUNCE","id":"b","duration":7}"#);
    assert_eq!(events(&mut session), [Event::Close]);
    assert_eq!(session.poll_timeout(), Some(secs(109)));
}

#[test]
fn only_frames_from_the_server_keep_a_connection_alive() {
    let mut session = Session::new(config());
    live(&mut session, secs(0));
    session.activated(Ok(()));
    let mut journal = Numberer::new("s");
    // A chatty agent, a silent server.
    for t in 1..=IDLE_TIMEOUT.as_secs() {
        let message = log("t");
        let entry = journal.entry(&message);
        session.send_entry(message, &entry);
        sent(&mut session);
        session.handle_timeout(secs(t));
    }
    assert!(events(&mut session).contains(&Event::Close));
    assert!(!session.is_live());

    let mut session = Session::new(config());
    live(&mut session, secs(0));
    session.handle_text(secs(100), r#"{"type":"HEARTBEAT","id":"h"}"#);
    session.handle_timeout(secs(130));
    assert!(session.is_live());
    assert_eq!(session.poll_timeout(), Some(secs(100) + IDLE_TIMEOUT));
}

#[test]
fn a_malformed_assign_is_reported_other_garbage_ignored() {
    let mut session = Session::new(config());
    live(&mut session, secs(0));
    session.handle_text(secs(1), r#"{"type":"ASSIGN","id":"x","task":"t7"}"#);
    session.handle_text(secs(1), "not json");
    assert!(matches!(
        &events(&mut session)[..],
        [Event::MalformedAssign { task, .. }] if task == "t7"
    ));
}

#[test]
fn inquiries_follow_the_resend() {
    let mut session = Session::new(config());
    registering(&mut session, secs(0));
    session.handle_text(
        secs(0),
        r#"{"type":"INIT","id":"i","agent":"a","inquiries":[{"task":"old"}],"diagnostics":[]}"#,
    );
    let events = events(&mut session);
    assert_eq!(events[1], Event::Inquiries(vec!["old".into()]));
}

/// The shared scenario, driven synchronously: the reference the async
/// controls (tokio, wasm, embassy) are held to.
#[test]
fn the_scripted_scenario() {
    let mut session = Session::new(SessionConfig {
        policy: ConnectionPolicy {
            initial: Duration::from_millis(10),
            ..Default::default()
        },
        ..SessionConfig::new("s", 1)
    });
    let mut server = FakeServer::scripted();
    let mut app = Echo::new("s");
    let mut now = Duration::ZERO;
    let mut open = false;
    for _ in 0..1000 {
        if server.done() {
            break;
        }
        session.handle_timeout(now);
        while let Some(event) = session.poll_event() {
            match event {
                Event::Connect => {
                    server.connected();
                    open = true;
                    session.connected(now, "token".into(), declaration());
                }
                Event::Close => open = false,
                Event::Ended(e) => panic!("ended: {e}"),
                other => app.handle(&mut session, &other),
            }
        }
        while let Some(envelope) = session.poll_transmit() {
            if open {
                server.receive(&to_text(&envelope));
            }
        }
        while let Some(output) = server.poll() {
            match output {
                ServerOutput::Frame(text) if open => session.handle_text(now, &text),
                ServerOutput::Frame(_) => {}
                ServerOutput::Drop => {
                    open = false;
                    session.handle_disconnect(now, "dropped");
                }
            }
        }
        now += Duration::from_millis(5);
    }
    server.check();
}
