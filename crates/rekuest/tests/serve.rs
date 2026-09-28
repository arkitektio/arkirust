//! The served agent against the contract recorded from Python
//! (`fixtures/serve/python`, see `record.sh`).

#![cfg(feature = "serve")]

mod twin;

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rekuest::serve::{configure, AuthRequest, History, ServeOptions, Unauthorized};
use rekuest::Context;
use serde_json::{json, Map, Value};
use tokio_tungstenite::tungstenite::Message;

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/serve/python/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

fn is_time(s: &str) -> bool {
    s.len() >= 20 && s.as_bytes()[4] == b'-' && s.as_bytes()[10] == b'T' && s.ends_with('Z')
}

/// Replace what differs between runs: ids, uuids, times.
fn normalize(value: Value) -> Value {
    fn walk(value: Value, key: Option<&str>) -> Value {
        if matches!(
            key,
            Some("id" | "ts" | "timepoint" | "start_time" | "end_time")
        ) && !value.is_null()
        {
            return json!(format!("<{}>", key.unwrap()));
        }
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(k, v)| {
                        let v = walk(v, Some(&k));
                        (if is_uuid(&k) { "<uuid>".to_owned() } else { k }, v)
                    })
                    .collect::<Map<_, _>>(),
            ),
            Value::Array(items) => Value::Array(items.into_iter().map(|v| walk(v, None)).collect()),
            Value::String(s) if is_uuid(&s) => json!("<uuid>"),
            Value::String(s) if is_time(&s) => json!("<time>"),
            other => other,
        }
    }
    walk(value, None)
}

fn assert_matches(name: &str, actual: Value) {
    let expected = normalize(fixture(name));
    let actual = normalize(actual);
    assert_eq!(
        actual,
        expected,
        "\n{name} differs\nexpected: {}\nactual:   {}",
        serde_json::to_string_pretty(&expected).unwrap(),
        serde_json::to_string_pretty(&actual).unwrap()
    );
}

async fn response(r: reqwest::Response) -> Value {
    let status = r.status().as_u16();
    let mut headers = Map::new();
    for name in ["www-authenticate", "content-type"] {
        if let Some(v) = r.headers().get(name) {
            headers.insert(name.into(), json!(v.to_str().unwrap()));
        }
    }
    let text = r.text().await.unwrap();
    let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
    json!({ "status": status, "headers": headers, "body": body })
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_frame(ws: &mut Ws) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            _ => continue,
        }
    }
}

/// Frames of `task` up to its end, plus what follows within a moment (UNLOCK).
async fn until_terminal(ws: &mut Ws, task: &str) -> Vec<Value> {
    let mut frames = vec![];
    loop {
        let frame = next_frame(ws).await;
        let done = frame["task"] == task
            && matches!(
                frame["type"].as_str(),
                Some("COMPLETED" | "FAILED" | "CRITICAL" | "CANCELLED")
            );
        frames.push(frame);
        if done {
            break;
        }
    }
    while let Ok(Some(Ok(Message::Text(text)))) =
        tokio::time::timeout(Duration::from_millis(300), ws.next()).await
    {
        frames.push(serde_json::from_str(&text).unwrap());
    }
    frames
}

async fn start() -> (String, rekuest::serve::LocalAgent) {
    let options = ServeOptions::default()
        .history(History::Memory)
        .auth(|request| match request {
            AuthRequest::Http { headers, .. } => {
                match headers.get("authorization").and_then(|h| h.to_str().ok()) {
                    Some("Bearer good") => Ok("tester".into()),
                    _ => Err(Unauthorized("bad".into())),
                }
            }
            AuthRequest::WebSocket(init) => match init.token.as_deref() {
                Some("good") => Ok("tester".into()),
                _ => Err(Unauthorized("bad".into())),
            },
        });
    let (router, agent) = configure(
        axum::Router::new(),
        twin::registry(),
        Context::default(),
        options,
    )
    .unwrap();
    agent.start().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (base, agent)
}

#[tokio::test]
async fn matches_the_python_contract() {
    let (base, agent) = start().await;
    let http = reqwest::Client::new();
    let ws_url = base.replace("http", "ws") + "/ws";
    let auth = ("authorization", "Bearer good");

    // Auth: 401 without WWW-Authenticate, 1008 on the socket.
    let r = http
        .post(format!("{base}/assign/count_up"))
        .json(&json!({"args": {"until": 1}}))
        .send()
        .await
        .unwrap();
    assert_matches("auth_http_401", response(r).await);
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    ws.send(Message::Text(
        json!({"type": "INIT", "token": "bad"}).to_string(),
    ))
    .await
    .unwrap();
    match ws.next().await {
        Some(Ok(Message::Close(Some(frame)))) => {
            assert_eq!(
                (u16::from(frame.code), &*frame.reason),
                (1008, "unauthorized")
            )
        }
        other => panic!("expected a 1008 close, got {other:?}"),
    }

    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    ws.send(Message::Text(
        json!({"type": "INIT", "token": "good"}).to_string(),
    ))
    .await
    .unwrap();
    assert_matches("ws_init", next_frame(&mut ws).await);

    for (name, path) in [
        ("schemas_implementations", "/schemas/implementations"),
        ("schemas_states", "/schemas/states"),
        ("schemas_locks", "/schemas/locks"),
        ("state_camera_initial", "/states/CameraState"),
    ] {
        assert_matches(
            name,
            response(http.get(format!("{base}{path}")).send().await.unwrap()).await,
        );
    }

    // LOCK, STATE_PATCH, YIELD, COMPLETED, UNLOCK.
    let r = http
        .post(format!("{base}/assign/set_exposure"))
        .header(auth.0, auth.1)
        .json(&json!({"args": {"exposure_ms": 20.0}}))
        .send()
        .await
        .unwrap();
    let r = response(r).await;
    let task = r["body"]["task"].as_str().unwrap().to_owned();
    assert_matches("assign_set_exposure", r);
    assert_matches(
        "frames_set_exposure",
        json!(until_terminal(&mut ws, &task).await),
    );

    let r = http
        .post(format!("{base}/assign"))
        .header(auth.0, auth.1)
        .json(&json!({"interface": "add_tag", "args": {"tag": "a"}}))
        .send()
        .await
        .unwrap();
    let task = response(r).await["body"]["task"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_matches(
        "frames_add_tag",
        json!(until_terminal(&mut ws, &task).await),
    );

    // The per-action route answers `task_id`; a generator yields twice.
    let r = response(
        http.post(format!("{base}/count_up"))
            .json(&json!({"args": {"until": 2}}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let task = r["body"]["task_id"].as_str().unwrap().to_owned();
    assert_matches("impl_count_up", r);
    assert_matches(
        "frames_count_up",
        json!(until_terminal(&mut ws, &task).await),
    );

    let r = http
        .post(format!("{base}/assign/explode"))
        .header(auth.0, auth.1)
        .json(&json!({"args": {}}))
        .send()
        .await
        .unwrap();
    let task = response(r).await["body"]["task"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_matches(
        "frames_explode",
        json!(until_terminal(&mut ws, &task).await),
    );

    // Step: PAUSED at the first pausepoint, released by /resume.
    let r = http
        .post(format!("{base}/assign/pausable"))
        .header(auth.0, auth.1)
        .json(&json!({"args": {}, "step": true}))
        .send()
        .await
        .unwrap();
    let task = response(r).await["body"]["task"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut early = vec![];
    loop {
        let frame = next_frame(&mut ws).await;
        let paused = frame["type"] == "PAUSED";
        early.push(frame);
        if paused {
            break;
        }
    }
    assert_matches(
        "tasks_paused",
        response(http.get(format!("{base}/tasks")).send().await.unwrap()).await,
    );
    let r = http
        .post(format!("{base}/resume"))
        .json(&json!({"task": task}))
        .send()
        .await
        .unwrap();
    assert_matches("resume", response(r).await);
    early.extend(until_terminal(&mut ws, &task).await);
    assert_matches("frames_pausable", json!(early));

    // Views and history after the run.
    let session = agent.executor().states().revision().0;
    for (name, path) in [
        ("tasks_after", "/tasks".to_owned()),
        ("task_unknown", "/tasks/nope".to_owned()),
        ("states_after", "/states".to_owned()),
        (
            "states_filtered",
            "/states?state_keys=CameraState,Other".to_owned(),
        ),
        ("state_camera_after", "/states/CameraState".to_owned()),
        ("locks_after", "/locks".to_owned()),
        (
            "checkout_1",
            "/states/checkout?global_revision_id=1".to_owned(),
        ),
        (
            "checkout_unknown_key",
            "/states/checkout?global_revision_id=1&state_keys=Nope".to_owned(),
        ),
        ("checkout_missing_rev", "/states/checkout".to_owned()),
        (
            "segments_0_2",
            "/states/segments?from_global_revision_id=0&to_global_revision_id=2".to_owned(),
        ),
        (
            "active_session_boundaries",
            "/active_session_boundaries".to_owned(),
        ),
        (
            "session_boundaries",
            format!("/session_boundaries/{session}"),
        ),
        (
            "task_boundaries_unknown",
            "/task_boundaries/nope".to_owned(),
        ),
        ("state_at_global_1", format!("/state_at_global/{session}/1")),
        (
            "state_at_global_1_camera",
            format!("/state_at_global/{session}/1?state_id=CameraState"),
        ),
        (
            "current_state_at_global_2",
            "/current_state_at_global/2".to_owned(),
        ),
        ("forward_events_0", format!("/forward_events/{session}/0")),
        (
            "snapshots_around_1",
            format!("/snapshots_around/{session}/1"),
        ),
    ] {
        assert_matches(
            name,
            response(http.get(format!("{base}{path}")).send().await.unwrap()).await,
        );
    }

    agent.shutdown().await;
}
