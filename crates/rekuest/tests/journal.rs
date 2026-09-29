//! The journal: one gap-free order of everything a served agent reports,
//! resumable over the websocket and replayable at any position.

#![cfg(feature = "serve")]

mod twin;

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rekuest::serve::{configure, History, LocalAgent, ServeOptions};
use rekuest::{action, Context, StateMut};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use twin::CameraState;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Changes the camera until cancelled.
#[action]
async fn spin(camera: StateMut<CameraState>) -> anyhow::Result<i64> {
    let mut n = 0i64;
    loop {
        camera.update(|c| c.exposure_ms += 1.0)?;
        n += 1;
        if n % 3 == 0 {
            tokio::task::yield_now().await;
        }
    }
}

async fn start() -> (String, LocalAgent) {
    let mut registry = twin::registry();
    registry.register(spin);
    let options = ServeOptions::default().history(History::Memory);
    let (router, agent) =
        configure(axum::Router::new(), registry, Context::default(), options).unwrap();
    agent.start().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (base, agent)
}

async fn connect(base: &str, init: Value) -> (Ws, Value) {
    let (mut ws, _) = tokio_tungstenite::connect_async(base.replace("http", "ws") + "/ws")
        .await
        .unwrap();
    ws.send(Message::Text(init.to_string())).await.unwrap();
    let first = next_frame(&mut ws).await;
    assert_eq!(first["type"], "INIT");
    (ws, first)
}

async fn next_frame(ws: &mut Ws) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next())
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

/// Frames up to `task`'s end, plus what follows within a moment (UNLOCK).
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
    frames.extend(drain(ws, Duration::from_millis(300)).await);
    frames
}

async fn drain(ws: &mut Ws, quiet: Duration) -> Vec<Value> {
    let mut frames = vec![];
    while let Ok(Some(Ok(Message::Text(text)))) = tokio::time::timeout(quiet, ws.next()).await {
        frames.push(serde_json::from_str(&text).unwrap());
    }
    frames
}

async fn assign(http: &reqwest::Client, base: &str, interface: &str, args: Value) -> String {
    let r: Value = http
        .post(format!("{base}/assign/{interface}"))
        .json(&json!({ "args": args }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    r["task"].as_str().unwrap().to_owned()
}

async fn get(http: &reqwest::Client, url: String) -> Value {
    http.get(url).send().await.unwrap().json().await.unwrap()
}

fn positions(frames: &[Value]) -> Vec<u64> {
    frames
        .iter()
        .map(|f| f["pos"].as_u64().expect("every journal frame has a pos"))
        .collect()
}

fn kinds(frames: &[Value]) -> Vec<&str> {
    frames.iter().map(|f| f["type"].as_str().unwrap()).collect()
}

fn assert_contiguous(pos: &[u64]) {
    for pair in pos.windows(2) {
        assert_eq!(
            pair[1],
            pair[0] + 1,
            "positions have no gaps and no repeats: {pos:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_order_for_tasks_and_state_and_replay_at_any_position() {
    let (base, agent) = start().await;
    let http = reqwest::Client::new();

    let (mut ws, init) = connect(&base, json!({"type": "INIT", "journal": true})).await;
    let (mut legacy, legacy_init) = connect(&base, json!({"type": "INIT"})).await;
    let session = init["journal"]["session_id"].as_str().unwrap().to_owned();
    assert_eq!(init["journal"]["pos"], 1, "SESSION_INIT is the first entry");
    assert_eq!(
        init["journal"]["states"]["CameraState"]["exposure_ms"],
        10.0
    );
    assert!(legacy_init.get("journal").is_none());

    let task = assign(&http, &base, "set_exposure", json!({"exposure_ms": 20.0})).await;
    let frames = until_terminal(&mut ws, &task).await;
    assert_eq!(
        kinds(&frames),
        [
            "PROGRESS",
            "LOCK",
            "STATE_PATCH",
            "YIELD",
            "COMPLETED",
            "UNLOCK"
        ]
    );
    let pos = positions(&frames);
    assert_eq!(pos[0], 2);
    assert_contiguous(&pos);
    assert!(frames.iter().all(|f| f["journal_session"] == session));

    // The legacy subscriber gets Python's frames: no positions.
    let legacy_frames = until_terminal(&mut legacy, &task).await;
    assert_eq!(
        kinds(&legacy_frames),
        [
            "PROGRESS",
            "LOCK",
            "STATE_PATCH",
            "YIELD",
            "COMPLETED",
            "UNLOCK"
        ]
    );
    assert!(legacy_frames
        .iter()
        .all(|f| f.get("pos").is_none() && f.get("journal_session").is_none()));
    assert_eq!(
        legacy_frames.iter().map(|f| &f["id"]).collect::<Vec<_>>(),
        frames.iter().map(|f| &f["id"]).collect::<Vec<_>>(),
        "the same messages, with the same ids"
    );

    // The stored journal is the same sequence.
    let listing = get(&http, format!("{base}/journal/current")).await;
    let entries = listing["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 7);
    assert_contiguous(
        &entries
            .iter()
            .map(|e| e["pos"].as_u64().unwrap())
            .collect::<Vec<_>>(),
    );
    assert_eq!(entries[0]["kind"], "SESSION_INIT");
    let unlock = &entries[6];
    assert_eq!(
        (unlock["kind"].as_str(), unlock["task_id"].as_str()),
        (Some("UNLOCK"), Some(task.as_str()))
    );

    // Time travel: before the patch, between the end and the unlock, after.
    let lock_pos = frames[1]["pos"].as_u64().unwrap();
    let done_pos = frames[4]["pos"].as_u64().unwrap();
    let at = get(&http, format!("{base}/journal/{session}/at/{lock_pos}")).await;
    assert_eq!(at["states"]["CameraState"]["exposure_ms"], 10.0);
    assert_eq!(at["global_rev"], 0);
    assert_eq!(at["tasks"][&task]["status"], "RUNNING");
    assert_eq!(at["locks"]["camera"], task.as_str());
    let at = get(&http, format!("{base}/journal/{session}/at/{done_pos}")).await;
    assert_eq!(at["states"]["CameraState"]["exposure_ms"], 20.0);
    assert_eq!(at["global_rev"], 1);
    assert_eq!(at["tasks"][&task]["status"], "COMPLETED");
    assert_eq!(at["tasks"][&task]["last_returns"]["return0"], 20.0);
    assert_eq!(
        at["locks"]["camera"],
        task.as_str(),
        "UNLOCK comes after the end"
    );
    let at = get(
        &http,
        format!("{base}/journal/{session}/at/{}", done_pos + 1),
    )
    .await;
    assert!(at["locks"].as_object().unwrap().is_empty());

    let now = chrono::Utc::now().timestamp_millis() + 1000;
    let at = get(
        &http,
        format!("{base}/journal/{session}/at?timestamp={now}"),
    )
    .await;
    assert_eq!(at["pos"], done_pos + 1);

    let events = get(&http, format!("{base}/tasks/{task}/events")).await;
    assert_eq!(events["task"]["status"], "COMPLETED");
    assert_eq!(
        events["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "PROGRESS",
            "LOCK",
            "STATE_PATCH",
            "YIELD",
            "COMPLETED",
            "UNLOCK"
        ]
    );

    let info = get(&http, format!("{base}/session_info")).await;
    assert_eq!(info["current_pos"], 7);

    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delivery_order_is_journal_order_under_concurrency() {
    let (base, agent) = start().await;
    let http = reqwest::Client::new();
    let (mut ws, init) = connect(&base, json!({"type": "INIT", "journal": true})).await;
    let start_pos = init["journal"]["pos"].as_u64().unwrap();

    let mut tasks = vec![];
    for i in 0..20 {
        let (http, base) = (http.clone(), base.clone());
        tasks.push(tokio::spawn(async move {
            if i % 2 == 0 {
                assign(&http, &base, "add_tag", json!({"tag": format!("t{i}")})).await
            } else {
                assign(&http, &base, "count_up", json!({"until": 5})).await
            }
        }));
    }
    let mut ids = vec![];
    for task in tasks {
        ids.push(task.await.unwrap());
    }

    let mut frames = vec![];
    let mut ended = 0;
    while ended < ids.len() {
        let frame = next_frame(&mut ws).await;
        if matches!(
            frame["type"].as_str(),
            Some("COMPLETED" | "FAILED" | "CRITICAL")
        ) {
            ended += 1;
        }
        frames.push(frame);
    }
    frames.extend(drain(&mut ws, Duration::from_millis(300)).await);
    let pos = positions(&frames);
    assert_eq!(pos[0], start_pos + 1);
    assert_contiguous(&pos);

    // Per task: the queued PROGRESS first, the end last (before its UNLOCK); patches carry increasing revisions.
    for id in &ids {
        let own: Vec<&Value> = frames
            .iter()
            .filter(|f| f["task"] == id.as_str() || f["task_id"] == id.as_str())
            .collect();
        assert_eq!(own[0]["type"], "PROGRESS");
        let end = own.iter().position(|f| f["type"] == "COMPLETED").unwrap();
        assert!(
            own[end + 1..].iter().all(|f| f["type"] == "UNLOCK"),
            "nothing of a task after its end but its UNLOCKs"
        );
        let steps: Vec<u64> = own
            .iter()
            .map(|f| f["task_step"].as_u64().unwrap())
            .collect();
        assert_eq!(
            steps,
            (1..=own.len() as u64).collect::<Vec<_>>(),
            "gapless task steps"
        );
    }
    let revs: Vec<u64> = frames
        .iter()
        .filter(|f| f["type"] == "STATE_PATCH")
        .map(|f| f["global_rev"].as_u64().unwrap())
        .collect();
    assert_eq!(revs, (1..=revs.len() as u64).collect::<Vec<_>>());

    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_after_a_disconnect() {
    let (base, agent) = start().await;
    let http = reqwest::Client::new();

    let (mut ws, init) = connect(&base, json!({"type": "INIT", "journal": true})).await;
    let session = init["journal"]["session_id"].as_str().unwrap().to_owned();
    let task = assign(&http, &base, "add_tag", json!({"tag": "a"})).await;
    let seen = until_terminal(&mut ws, &task).await;
    let last = *positions(&seen).last().unwrap();
    drop(ws);

    // Missed while away.
    let missed = assign(&http, &base, "count_up", json!({"until": 3})).await;
    while !agent
        .journal()
        .locked(|v| v.fold().tasks.get(&missed).is_some_and(|t| t.done))
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let watermark = agent.journal().watermark().unwrap().pos;

    let (mut ws, init) = connect(
        &base,
        json!({"type": "INIT", "journal": true, "resume_after": last, "session_id": session}),
    )
    .await;
    assert_eq!(init["journal"]["resync"], false);
    assert_eq!(init["journal"]["pos"], watermark);
    assert_eq!(init["journal"]["tasks"][&missed]["yields"], 3);
    let mut frames = vec![];
    for _ in last..watermark {
        frames.push(next_frame(&mut ws).await);
    }
    assert_eq!(kinds(&frames)[0], "PROGRESS");
    assert_eq!(frames[0]["task"], missed.as_str());

    // Then live, continuing the same order.
    let live = assign(&http, &base, "explode", json!({})).await;
    frames.extend(until_terminal(&mut ws, &live).await);
    let pos = positions(&frames);
    assert_eq!(pos[0], last + 1);
    assert_contiguous(&pos);

    // Another session's position cannot be resumed.
    let (_, init) = connect(
        &base,
        json!({"type": "INIT", "journal": true, "resume_after": 1, "session_id": "elsewhere"}),
    )
    .await;
    assert_eq!(init["journal"]["resync"], true);
    // Not even from the start: 0 of another session is not 0 of this one.
    let (mut other, init) = connect(
        &base,
        json!({"type": "INIT", "journal": true, "resume_after": 0, "session_id": "elsewhere"}),
    )
    .await;
    assert_eq!(init["journal"]["resync"], true);
    assert!(
        drain(&mut other, Duration::from_millis(200))
            .await
            .is_empty(),
        "no replay on resync"
    );

    // Nor a position without its session (it may be from before a restart).
    let (mut ws, init) = connect(
        &base,
        json!({"type": "INIT", "journal": true, "resume_after": 1}),
    )
    .await;
    assert_eq!(init["journal"]["resync"], true);
    assert!(
        drain(&mut ws, Duration::from_millis(200)).await.is_empty(),
        "no backlog on resync"
    );

    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_from_storage_beyond_memory() {
    let (base, agent) = start().await;
    let http = reqwest::Client::new();
    let n = rekuest::journal::RING_CAPACITY as i64 + 100;
    let task = assign(&http, &base, "count_up", json!({"until": n})).await;
    while !agent
        .journal()
        .locked(|v| v.fold().tasks.get(&task).is_some_and(|t| t.done))
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let watermark = agent.journal().watermark().unwrap().pos;

    let (mut ws, init) = connect(
        &base,
        json!({"type": "INIT", "journal": true, "resume_after": 0}),
    )
    .await;
    assert_eq!(init["journal"]["resync"], false);
    let mut frames = vec![];
    for _ in 0..watermark {
        frames.push(next_frame(&mut ws).await);
    }
    let pos = positions(&frames);
    assert_eq!(pos[0], 1);
    assert_eq!(*pos.last().unwrap(), watermark);
    assert_contiguous(&pos);

    agent.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_of_a_cancelled_task_after_its_end() {
    let (base, agent) = start().await;
    let http = reqwest::Client::new();
    let (mut ws, init) = connect(&base, json!({"type": "INIT", "journal": true})).await;
    let session = init["journal"]["session_id"].as_str().unwrap().to_owned();

    let task = assign(&http, &base, "spin", json!({})).await;
    let mut patches = 0;
    while patches < 50 {
        if next_frame(&mut ws).await["type"] == "STATE_PATCH" {
            patches += 1;
        }
    }
    http.post(format!("{base}/cancel"))
        .json(&json!({"task": task}))
        .send()
        .await
        .unwrap();
    let frames = until_terminal(&mut ws, &task).await;
    let cancelled = frames
        .iter()
        .position(|f| f["type"] == "CANCELLED")
        .unwrap();
    assert_eq!(
        kinds(&frames[cancelled + 1..]),
        ["UNLOCK"],
        "only the lock release follows the end"
    );

    agent.journal().flush(Duration::from_secs(5)).await;
    let listing = get(
        &http,
        format!("{base}/journal/{session}?kinds=STATE_PATCH&limit=100000"),
    )
    .await;
    let revs: Vec<u64> = listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["global_rev"].as_u64().unwrap())
        .collect();
    assert_eq!(
        revs,
        (1..=revs.len() as u64).collect::<Vec<_>>(),
        "no revision was skipped"
    );

    // The live state is exactly the recorded one.
    let last = agent.journal().watermark().unwrap();
    assert_eq!(agent.executor().states().revision().1, last.global_rev);
    let at = get(&http, format!("{base}/journal/{session}/at/{}", last.pos)).await;
    assert_eq!(
        at["states"]["CameraState"],
        agent.executor().states().value("CameraState").unwrap()
    );
    assert_eq!(at["tasks"][&task]["status"], "CANCELLED");

    agent.shutdown().await;
}
