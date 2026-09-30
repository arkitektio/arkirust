//! Serve an app over HTTP and a websocket, without a rekuest server.
//!
//! This is the Rust port of Python's `rekuest.contrib.fastapi`: the app's
//! actions become REST commands, an observer websocket streams what the
//! agent reports, and GET routes expose tasks, states, locks and the state
//! history (kept in SQLite). The HTTP and websocket contract is the Python
//! one, quirks included, so clients written against a Python app work
//! unchanged.
//!
//! ```ignore
//! let (router, agent) = rekuest::serve::configure(axum::Router::new(), registry, Context::default(), ServeOptions::default())?;
//! agent.start().await?;
//! axum::serve(listener, router).await?;
//! agent.shutdown().await;
//! ```
//!
//! Routes (paths configurable in [`ServeOptions`]):
//!
//! | route | |
//! |---|---|
//! | `WS /ws` | send `{"type":"INIT", "action_keys"?, "state_keys"?, "lock_keys"?, "token"?}`, get an `INIT` snapshot, then every matching frame |
//! | `POST /assign`, `/assign/{interface}` | start a task (authenticated), `{"status":"submitted","task"}` |
//! | `POST /{interface}` | start a task of that action, `{"status":"submitted","task_id"}` |
//! | `POST /cancel`, `/pause`, `/resume`, `/step` | control a task |
//! | `GET /tasks`, `/tasks/{id}`, `/states`, `/states/{interface}`, `/locks` | what is going on |
//! | `GET /schemas/{implementations,states,locks,bloks}` | the declaration |
//! | `GET /session_info`, `/states/checkout`, `/states/segments`, … | state history |
//! | `GET /journal`, `/journal/{session}`, `/journal/{session}/at/{pos}`, `/tasks/{id}/events` | the journal: every report in order, and the world at any position |
//! | `GET /openapi.json`, `/docs` | API description |
//!
//! A websocket client that sends `"journal": true` in its INIT opts into the
//! journal: its INIT gets a `journal` object (the watermark and the states,
//! tasks and locks exactly as of it), every frame after carries `pos`,
//! `journal_session`, `agent_ts` and (for a task's frames) `task_step`, and
//! with `"resume_after": N` and the `session_id` it first gets the entries it
//! missed after `N` (another session's position answers `resync: true`).
//! Without the flag, frames are exactly Python's.

// Handler helpers return a ready `Response` as their error, as axum handlers do.
#![allow(clippy::result_large_err)]

mod broadcast;
mod schema;
#[cfg(feature = "testing")]
pub mod testing;

use std::collections::HashSet;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::action::Registry;
use crate::context::Context;
use crate::executor::{Executor, FLUSH_TIMEOUT};
use crate::journal::{Fold, Journal, JournalEntry, JournalSink};
use crate::messages::Assign;
use crate::state::Sink;

pub use crate::store::{
    EntryQuery, HistoryStore, PatchEvent, SessionBoundary, Snapshot, StateAt, TaskBoundary,
};
pub use broadcast::Broadcaster;

/// The first frame a websocket client sends.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SubscriptionInit {
    #[serde(rename = "type", default)]
    pub type_: Option<String>,
    #[serde(default)]
    pub action_keys: Option<Vec<String>>,
    #[serde(default)]
    pub state_keys: Option<Vec<String>>,
    #[serde(default)]
    pub lock_keys: Option<Vec<String>>,
    #[serde(default)]
    pub token: Option<String>,
    /// Opt into the journal (`pos` on every frame, a consistent INIT).
    #[serde(default)]
    pub journal: bool,
    /// With `journal`: first replay the entries after this position.
    #[serde(default)]
    pub resume_after: Option<u64>,
    /// With `resume_after`: the session the position belongs to. Required
    /// unless resuming from 0; a mismatch answers `resync: true`.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// What an auth hook authenticates.
#[derive(Debug)]
pub enum AuthRequest<'a> {
    /// An HTTP command (`/assign`).
    Http {
        headers: &'a HeaderMap,
        uri: &'a Uri,
    },
    /// A websocket subscription (its first frame).
    WebSocket(&'a SubscriptionInit),
}

/// Rejects a request: 401 over HTTP, close code 1008 over the websocket.
#[derive(Debug, Clone, thiserror::Error)]
#[error("unauthorized: {0}")]
pub struct Unauthorized(pub String);

/// Returns the user a request acts for.
pub type AuthHook = Arc<dyn Fn(AuthRequest<'_>) -> Result<String, Unauthorized> + Send + Sync>;

/// Where state history is kept.
#[derive(Debug, Clone)]
pub enum History {
    /// An SQLite file (`agent_data.db` by default, like Python).
    Sqlite(std::path::PathBuf),
    /// In memory, gone when the process exits.
    Memory,
    /// A store you opened yourself.
    Store(HistoryStore),
}

/// How [`configure`] serves an app.
#[derive(Clone)]
pub struct ServeOptions {
    pub auth: Option<AuthHook>,
    pub history: History,
    pub ws_path: String,
    pub tasks_path: String,
    pub assign_path: String,
    pub states_path: String,
    pub locks_path: String,
    pub add_implementations: bool,
    pub add_schema: bool,
    pub add_states: bool,
    pub add_state_details: bool,
    pub add_locks: bool,
    pub add_tasks: bool,
    pub add_task_details: bool,
    /// Serve the journal routes (`/journal…`, `/tasks/{id}/events`).
    pub add_journal: bool,
    /// Serve `/openapi.json` and a Swagger page at `/docs`.
    pub openapi: bool,
    pub title: String,
    pub version: String,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            auth: None,
            history: History::Sqlite("agent_data.db".into()),
            ws_path: "/ws".into(),
            tasks_path: "/tasks".into(),
            assign_path: "/assign".into(),
            states_path: "/states".into(),
            locks_path: "/locks".into(),
            add_implementations: true,
            add_schema: true,
            add_states: true,
            add_state_details: true,
            add_locks: true,
            add_tasks: true,
            add_task_details: true,
            add_journal: true,
            openapi: true,
            title: "Arkitekt App".into(),
            version: "0.1.0".into(),
        }
    }
}

impl ServeOptions {
    /// Authenticate `/assign` and the websocket with `hook`.
    pub fn auth<F>(mut self, hook: F) -> Self
    where
        F: Fn(AuthRequest<'_>) -> Result<String, Unauthorized> + Send + Sync + 'static,
    {
        self.auth = Some(Arc::new(hook));
        self
    }

    pub fn history(mut self, history: History) -> Self {
        self.history = history;
        self
    }
}

impl std::fmt::Debug for ServeOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServeOptions")
            .field("auth", &self.auth.is_some())
            .field("history", &self.history)
            .finish_non_exhaustive()
    }
}

/// The in-process agent behind a served app. Cheap to clone.
#[derive(Clone)]
pub struct LocalAgent {
    executor: Executor,
    broadcaster: Arc<Broadcaster>,
    journal: Arc<Journal>,
    store: HistoryStore,
}

impl std::fmt::Debug for LocalAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalAgent")
            .field("executor", &self.executor)
            .finish()
    }
}

impl LocalAgent {
    /// Run startup hooks, open the session and start background hooks.
    pub async fn start(&self) -> anyhow::Result<()> {
        self.executor.activate().await
    }

    /// Cancel background hooks, run shutdown hooks and flush the history.
    pub async fn shutdown(&self) {
        self.executor.teardown().await;
        self.journal.flush(FLUSH_TIMEOUT).await;
    }

    pub fn executor(&self) -> &Executor {
        &self.executor
    }

    pub fn history(&self) -> &HistoryStore {
        &self.store
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    fn current_session(&self) -> Option<String> {
        self.executor
            .is_activated()
            .then(|| self.executor.states().revision().0)
            .filter(|s| !s.is_empty())
    }
}

struct Shared {
    agent: LocalAgent,
    auth: Option<AuthHook>,
    openapi: Value,
}

type S = State<Arc<Shared>>;

/// Add the agent routes to `router` and build the agent behind them.
/// Call [`LocalAgent::start`] before serving and [`LocalAgent::shutdown`] after.
pub fn configure(
    router: Router,
    registry: Registry,
    ctx: Context,
    options: ServeOptions,
) -> anyhow::Result<(Router, LocalAgent)> {
    let store = match &options.history {
        History::Sqlite(path) => HistoryStore::open(path)?,
        History::Memory => HistoryStore::memory()?,
        History::Store(store) => store.clone(),
    };
    let broadcaster = Arc::new(Broadcaster::default());
    let journal = Arc::new(Journal::new(
        broadcaster.clone(),
        Some(Arc::new(store.clone()) as Arc<dyn JournalSink>),
        None,
    ));
    let executor = Executor::new(
        registry,
        ctx,
        journal.clone(),
        Some(Arc::new(store.clone()) as Arc<dyn Sink>),
    );
    let agent = LocalAgent {
        executor,
        broadcaster,
        journal,
        store,
    };
    let openapi = build_openapi(&agent, &options);
    let shared = Arc::new(Shared {
        agent: agent.clone(),
        auth: options.auth.clone(),
        openapi,
    });

    let o = &options;
    let mut routes: Router<Arc<Shared>> = Router::new()
        .route(&o.ws_path, get(ws_endpoint))
        .route(&o.assign_path, post(assign_base))
        .route(
            &format!("{}/{{interface}}", o.assign_path),
            post(assign_interface),
        )
        .route("/cancel", post(cancel))
        .route("/pause", post(pause))
        .route("/resume", post(resume))
        .route("/step", post(step));

    let mut taken: HashSet<String> = [
        o.ws_path.clone(),
        o.assign_path.clone(),
        "/cancel".into(),
        "/pause".into(),
        "/resume".into(),
        "/step".into(),
    ]
    .into();

    if o.add_tasks {
        routes = routes.route(&o.tasks_path, get(list_tasks));
        taken.insert(o.tasks_path.clone());
    }
    if o.add_task_details {
        routes = routes.route(&format!("{}/{{task_id}}", o.tasks_path), get(get_task));
    }
    if o.add_journal {
        routes = routes
            .route("/journal", get(journal_info))
            .route("/journal/{session_id}", get(journal_entries))
            .route("/journal/{session_id}/at", get(journal_at_time))
            .route("/journal/{session_id}/at/{pos}", get(journal_at))
            .route(
                &format!("{}/{{task_id}}/events", o.tasks_path),
                get(task_events),
            );
        taken.insert("/journal".into());
    }
    if o.add_states {
        routes = routes.route(&o.states_path, get(list_states));
        taken.insert(o.states_path.clone());
    }
    if o.add_state_details {
        let fixed = ["checkout", "segments", "session_boundaries"];
        for declaration in agent.executor.registry().states() {
            if fixed.contains(&declaration.name.as_str()) {
                tracing::warn!(
                    "state '{}' would shadow a history route; not adding its route",
                    declaration.name
                );
                continue;
            }
            let name = declaration.name.clone();
            routes = routes.route(
                &format!("{}/{}", o.states_path, name),
                get(move |state: S| current_state(state, name)),
            );
        }
        routes = routes
            .route("/session_info", get(session_info))
            .route("/task_boundaries/{correlation_id}", get(task_boundaries))
            .route("/active_session_boundaries", get(active_session_boundaries))
            .route(
                &format!("{}/session_boundaries", o.states_path),
                get(state_session_boundaries),
            )
            .route("/session_boundaries/{session_id}", get(session_boundaries))
            .route(&format!("{}/checkout", o.states_path), get(checkout))
            .route(&format!("{}/segments", o.states_path), get(segments))
            .route(
                "/state_at_global/{session_id}/{target_revision}",
                get(state_at_global),
            )
            .route(
                "/current_state_at_global/{target_revision}",
                get(current_state_at_global),
            )
            .route(
                "/forward_events/{session_id}/{after_global_revision}",
                get(forward_events),
            )
            .route(
                "/snapshots_around/{session_id}/{target_revision}",
                get(snapshots_around),
            );
        taken.extend(
            [
                "/session_info",
                "/active_session_boundaries",
                "/task_boundaries",
                "/session_boundaries",
                "/state_at_global",
                "/current_state_at_global",
                "/forward_events",
                "/snapshots_around",
            ]
            .map(String::from),
        );
    }
    if o.add_locks {
        routes = routes.route(&o.locks_path, get(list_locks));
        taken.insert(o.locks_path.clone());
    }
    if o.add_implementations {
        for action in agent.executor.registry().iter() {
            let path = format!("/{}", action.interface());
            if taken.contains(&path) || path.starts_with("/schemas") {
                tracing::warn!("action route {path} would shadow an agent route; not adding it");
                continue;
            }
            let interface = action.interface();
            routes = routes.route(
                &path,
                post(move |state: S, body: Bytes| implementation_endpoint(state, interface, body)),
            );
        }
    }
    if o.add_schema {
        routes = routes
            .route("/schemas/implementations", get(schema_implementations))
            .route("/schemas/states", get(schema_states))
            .route("/schemas/locks", get(schema_locks))
            .route("/schemas/bloks", get(schema_bloks));
    }
    if o.openapi {
        routes = routes
            .route("/openapi.json", get(openapi_json))
            .route("/docs", get(docs_page));
    }

    Ok((router.merge(routes.with_state(shared)), agent))
}

// ---------------------------------------------------------------- helpers --

fn internal_error() -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}

fn detail(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "detail": message }))).into_response()
}

fn parse_object(body: &Bytes) -> Option<Map<String, Value>> {
    match serde_json::from_slice(body) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

/// `normalize_filter_values`: repeated or comma separated, trimmed, unique, in order.
fn normalize_filter(values: &[String]) -> Option<Vec<String>> {
    let mut out: Vec<String> = vec![];
    for value in values {
        for key in value.split(',') {
            let key = key.trim();
            if !key.is_empty() && !out.iter().any(|k| k == key) {
                out.push(key.to_owned());
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

fn query_values(query: &[(String, String)], name: &str) -> Vec<String> {
    query
        .iter()
        .filter(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
        .collect()
}

fn query_one(query: &[(String, String)], name: &str) -> Option<String> {
    query
        .iter()
        .rev()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}

/// FastAPI's 422 body for one invalid parameter.
fn invalid(
    location: &str,
    name: &str,
    kind: &str,
    msg: &str,
    input: Value,
    ctx: Option<Value>,
) -> Response {
    let mut error = json!({ "type": kind, "loc": [location, name], "msg": msg, "input": input });
    if let Some(ctx) = ctx {
        error["ctx"] = ctx;
    }
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({ "detail": [error] })),
    )
        .into_response()
}

/// An integer parameter with FastAPI's validation (`required`, `ge`).
fn int_param(
    location: &str,
    name: &str,
    raw: Option<String>,
    default: Option<i64>,
    ge: Option<i64>,
) -> Result<i64, Response> {
    let Some(raw) = raw else {
        return default.ok_or_else(|| {
            invalid(
                location,
                name,
                "missing",
                "Field required",
                Value::Null,
                None,
            )
        });
    };
    let value: i64 = raw.trim().parse().map_err(|_| {
        invalid(
            location,
            name,
            "int_parsing",
            "Input should be a valid integer, unable to parse string as an integer",
            json!(raw),
            None,
        )
    })?;
    if let Some(ge) = ge {
        if value < ge {
            return Err(invalid(
                location,
                name,
                "greater_than_equal",
                &format!("Input should be greater than or equal to {ge}"),
                json!(raw),
                Some(json!({ "ge": ge })),
            ));
        }
    }
    Ok(value)
}

// --------------------------------------------------------------- commands --

const ALLOWED_ASSIGN_FIELDS: &[&str] = &[
    "action",
    "resolution",
    "implementation",
    "agent",
    "action_hash",
    "actionHash",
    "interface",
    "hooks",
    "args",
    "reference",
    "capture",
    "dependencies",
    "step",
];

/// `build_assign_input` + `build_assign_message`. `None` is a 500 in Python.
fn build_assign(
    mut payload: Map<String, Value>,
    interface: Option<String>,
    user: String,
) -> Option<Assign> {
    if let Some(interface) = interface {
        payload.insert("interface".into(), Value::String(interface));
    }
    for field in ["cached", "log", "ephemeral", "parent"] {
        payload.remove(field);
    }
    payload.entry("capture").or_insert(Value::Bool(false));
    if payload
        .keys()
        .any(|k| !ALLOWED_ASSIGN_FIELDS.contains(&k.as_str()))
    {
        return None;
    }
    let args = match payload.get("args")? {
        Value::Object(args) => args.clone(),
        _ => return None,
    };
    let optional_str = |key: &str| -> Option<Option<String>> {
        match payload.get(key) {
            None | Some(Value::Null) => Some(None),
            Some(Value::String(s)) => Some(Some(s.clone())),
            _ => None,
        }
    };
    let interface = optional_str("interface")??;
    let reference = optional_str("reference")?;
    let implementation = optional_str("implementation")?;
    let capture = payload.get("capture")?.as_bool()?;
    let step = match payload.get("step") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        _ => return None,
    };
    Some(Assign {
        id: None,
        interface,
        task: uuid::Uuid::new_v4().to_string(),
        root: None,
        parent: None,
        resolution: None,
        step,
        probe: false,
        capture: Some(capture),
        reference,
        args,
        message: None,
        user,
        org: "fastapi".into(),
        action: "api_call".into(),
        implementation: implementation.unwrap_or_else(|| "fastapi".into()),
        token: None,
        resume: None,
    })
}

fn http_user(shared: &Shared, headers: &HeaderMap, uri: &Uri) -> Result<String, Response> {
    match &shared.auth {
        None => Ok("anonymous".into()),
        // No `WWW-Authenticate`: it would make browsers pop their own login dialog.
        Some(hook) => hook(AuthRequest::Http { headers, uri })
            .map_err(|_| detail(StatusCode::UNAUTHORIZED, "Not authorized")),
    }
}

async fn submit(
    shared: &Shared,
    payload: Map<String, Value>,
    interface: Option<String>,
    user: String,
) -> Result<String, Response> {
    let assign = build_assign(payload, interface, user).ok_or_else(internal_error)?;
    let task = assign.task.clone();
    shared.agent.executor.assign(assign);
    Ok(task)
}

async fn assign_base(State(shared): S, headers: HeaderMap, uri: Uri, body: Bytes) -> Response {
    let user = match http_user(&shared, &headers, &uri) {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(mut payload) = parse_object(&body) else {
        return internal_error();
    };
    let interface = payload
        .remove("interface")
        .and_then(|v| v.as_str().map(str::to_owned));
    match submit(&shared, payload, interface, user).await {
        Ok(task) => Json(json!({ "status": "submitted", "task": task })).into_response(),
        Err(response) => response,
    }
}

async fn assign_interface(
    State(shared): S,
    Path(interface): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    let user = match http_user(&shared, &headers, &uri) {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(payload) = parse_object(&body) else {
        return internal_error();
    };
    match submit(&shared, payload, Some(interface), user).await {
        Ok(task) => Json(json!({ "status": "submitted", "task": task })).into_response(),
        Err(response) => response,
    }
}

async fn implementation_endpoint(State(shared): S, interface: String, body: Bytes) -> Response {
    let Some(payload) = parse_object(&body) else {
        return internal_error();
    };
    match submit(&shared, payload, Some(interface), "fastapi".into()).await {
        Ok(task) => Json(json!({ "status": "submitted", "task_id": task })).into_response(),
        Err(response) => response,
    }
}

/// `{"task": "..."}` with only the allowed extra keys.
fn control_task(body: &Bytes, allowed: &[&str]) -> Option<(String, Map<String, Value>)> {
    let payload = parse_object(body)?;
    if payload
        .keys()
        .any(|k| k != "task" && !allowed.contains(&k.as_str()))
    {
        return None;
    }
    let task = payload.get("task")?.as_str()?.to_owned();
    Some((task, payload))
}

async fn cancel(State(shared): S, body: Bytes) -> Response {
    let Some((task, _)) = control_task(&body, &[]) else {
        return internal_error();
    };
    shared.agent.executor.cancel(&task);
    Json(json!({ "status": "cancelling", "task": task })).into_response()
}

async fn pause(State(shared): S, body: Bytes) -> Response {
    let Some((task, _)) = control_task(&body, &[]) else {
        return internal_error();
    };
    shared.agent.executor.pause(&task);
    Json(json!({ "status": "pausing", "task": task })).into_response()
}

async fn resume(State(shared): S, body: Bytes) -> Response {
    // `step` is accepted but, as in Python, `/resume` always resumes freely.
    let Some((task, _)) = control_task(&body, &["step"]) else {
        return internal_error();
    };
    shared.agent.executor.resume(&task, false);
    Json(json!({ "status": "resuming", "task": task })).into_response()
}

async fn step(State(shared): S, body: Bytes) -> Response {
    let Some(task) =
        parse_object(&body).and_then(|p| p.get("task").and_then(|t| t.as_str().map(str::to_owned)))
    else {
        return internal_error();
    };
    shared.agent.executor.resume(&task, true);
    Json(json!({ "status": "stepping", "task": task })).into_response()
}

// -------------------------------------------------------------- websocket --

async fn ws_endpoint(State(shared): S, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_socket(shared, socket))
}

fn key_set(values: &Option<Vec<String>>) -> Option<HashSet<String>> {
    let set: HashSet<String> = values
        .iter()
        .flatten()
        .filter(|v| !v.is_empty())
        .cloned()
        .collect();
    (!set.is_empty()).then_some(set)
}

/// The INIT snapshot uses the raw lists (no splitting, empty strings kept), as in Python.
fn raw_set(values: &Option<Vec<String>>) -> Option<HashSet<String>> {
    values
        .as_ref()
        .filter(|v| !v.is_empty())
        .map(|v| v.iter().cloned().collect())
}

async fn handle_socket(shared: Arc<Shared>, mut socket: WebSocket) {
    let init = loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => break text,
            Some(Ok(Message::Binary(_) | Message::Ping(_) | Message::Pong(_))) => continue,
            _ => return,
        }
    };
    let init: SubscriptionInit = match serde_json::from_str::<Value>(&init) {
        Ok(value @ Value::Object(_)) => match serde_json::from_value(value) {
            Ok(init) => init,
            Err(e) => {
                tracing::warn!("invalid websocket init: {e}");
                return;
            }
        },
        _ => {
            tracing::warn!("websocket init must be a JSON object");
            return;
        }
    };

    if let Some(hook) = &shared.auth {
        if hook(AuthRequest::WebSocket(&init)).is_err() {
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: 1008,
                    reason: "unauthorized".into(),
                })))
                .await;
            return;
        }
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let filters = broadcast::Filters {
        action_keys: key_set(&init.action_keys),
        state_keys: key_set(&init.state_keys),
        lock_keys: key_set(&init.lock_keys),
    };
    let agent = &shared.agent;
    let (id, journal) = if init.journal {
        let (id, opening) = journal_opening(agent, &init, filters, tx).await;
        (id, Some(opening))
    } else {
        (agent.broadcaster.subscribe(filters, tx, false), None)
    };

    let tasks = agent
        .executor
        .task_views(raw_set(&init.action_keys).as_ref());
    let mut first = json!({
        "type": "INIT",
        "tasks": { "count": tasks.len(), "tasks": tasks },
        "states": state_collection(agent, raw_set(&init.state_keys).as_ref()),
        "locks": lock_collection(agent, raw_set(&init.lock_keys).as_ref()),
    });
    let backlog = match journal {
        Some((info, backlog)) => {
            first["journal"] = info;
            backlog
        }
        None => vec![],
    };

    let mut open = socket
        .send(Message::Text(first.to_string().into()))
        .await
        .is_ok();
    for entry in backlog {
        if !open {
            break;
        }
        open = socket
            .send(Message::Text(entry.frame().to_string().into()))
            .await
            .is_ok();
    }
    if open {
        loop {
            tokio::select! {
                frame = rx.recv() => match frame {
                    Some(text) => {
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                incoming = socket.recv() => match incoming {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {} // anything after INIT is ignored
                },
            }
        }
    }
    agent.broadcaster.unsubscribe(id);
}

/// Subscribe a journal client at the current watermark. Returns the
/// subscription, the INIT's `journal` object (the world exactly as of the
/// watermark) and the entries to replay (those after `resume_after`).
async fn journal_opening(
    agent: &LocalAgent,
    init: &SubscriptionInit,
    filters: broadcast::Filters,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
) -> (u64, (Value, Vec<Arc<JournalEntry>>)) {
    // Under the journal lock: live frames after the watermark queue up in
    // `tx`, so nothing is missed or sent twice.
    let (id, watermark, fold, recent) = agent.journal.locked(|view| {
        let watermark = view.watermark();
        let recent = match (&watermark, init.resume_after) {
            (Some(wm), Some(after)) if after <= wm.pos => view.recent(after, wm.pos),
            _ => None,
        };
        let id = agent.broadcaster.subscribe(filters.clone(), tx, true);
        (id, watermark, view.fold().clone(), recent)
    });

    let Some(wm) = watermark else {
        let info = json!({ "session_id": null, "pos": 0, "global_rev": 0, "resync": init.resume_after.is_some() });
        return (id, (info, vec![]));
    };

    // A position means nothing without its session (the agent may have
    // restarted since): resuming needs the session, except from the start.
    // A session that is not this one always resyncs, even from 0.
    let resync = init.resume_after.is_some_and(|after| {
        let session_ok = match &init.session_id {
            Some(session) => *session == wm.session_id,
            None => after == 0,
        };
        after > wm.pos || !session_ok
    });
    let backlog: Vec<Arc<JournalEntry>> = match (init.resume_after, resync) {
        (Some(after), false) => match recent {
            Some(recent) => recent,
            None => {
                agent.journal.flush_to(wm.pos, FLUSH_TIMEOUT).await;
                let query = EntryQuery {
                    after,
                    until: Some(wm.pos),
                    ..Default::default()
                };
                match agent.store.journal_entries(&wm.session_id, query).await {
                    Ok(entries) => entries.into_iter().map(Arc::new).collect(),
                    Err(e) => {
                        tracing::error!("could not read the journal to resume a subscriber: {e:#}");
                        vec![]
                    }
                }
            }
        },
        _ => vec![],
    };
    let backlog = backlog
        .into_iter()
        .filter(|e| filters.admits(e.route()))
        .collect();

    let mut info = world_json(
        &fold,
        raw_set(&init.action_keys).as_ref(),
        raw_set(&init.state_keys).as_ref(),
        raw_set(&init.lock_keys).as_ref(),
    );
    info["session_id"] = json!(wm.session_id);
    info["pos"] = json!(wm.pos);
    info["global_rev"] = json!(wm.global_rev);
    info["resync"] = json!(resync);
    (id, (info, backlog))
}

/// States, tasks and locks of a fold, optionally only some keys.
fn world_json(
    fold: &Fold,
    action_keys: Option<&HashSet<String>>,
    state_keys: Option<&HashSet<String>>,
    lock_keys: Option<&HashSet<String>>,
) -> Value {
    let states: Map<String, Value> = fold
        .states
        .iter()
        .filter(|(name, _)| state_keys.is_none_or(|k| k.contains(*name)))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let tasks: Map<String, Value> = fold
        .tasks
        .iter()
        .filter(|(_, task)| {
            action_keys.is_none_or(|k| task.action_key.as_ref().is_some_and(|a| k.contains(a)))
        })
        .map(|(id, task)| (id.clone(), json!(task)))
        .collect();
    let locks: Map<String, Value> = fold
        .locks
        .iter()
        .filter(|(key, _)| lock_keys.is_none_or(|k| k.contains(*key)))
        .map(|(key, task)| (key.clone(), json!(task)))
        .collect();
    json!({ "states": states, "tasks": tasks, "locks": locks })
}

// ------------------------------------------------------------------ views --

fn state_collection(agent: &LocalAgent, keys: Option<&HashSet<String>>) -> Value {
    let states = agent.executor.state_views(keys);
    json!({
        "current_session": agent.current_session(),
        "current_global_revision": agent.executor.states().revision().1,
        "count": states.len(),
        "states": states,
        "recent_patches": [],
    })
}

fn lock_collection(agent: &LocalAgent, keys: Option<&HashSet<String>>) -> Value {
    let locks = agent.executor.lock_views(keys);
    json!({ "count": locks.len(), "locks": locks })
}

async fn list_tasks(State(shared): S, Query(query): Query<Vec<(String, String)>>) -> Response {
    let keys = normalize_filter(&query_values(&query, "action_keys"))
        .map(|k| k.into_iter().collect::<HashSet<_>>());
    let tasks = shared.agent.executor.task_views(keys.as_ref());
    Json(json!({ "count": tasks.len(), "tasks": tasks })).into_response()
}

async fn get_task(State(shared): S, Path(task_id): Path<String>) -> Response {
    match shared.agent.executor.task_view(&task_id) {
        Some(view) => Json(view).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Task not found", "task_id": task_id })),
        )
            .into_response(),
    }
}

async fn list_states(State(shared): S, Query(query): Query<Vec<(String, String)>>) -> Response {
    let keys = normalize_filter(&query_values(&query, "state_keys"))
        .map(|k| k.into_iter().collect::<HashSet<_>>());
    Json(state_collection(&shared.agent, keys.as_ref())).into_response()
}

async fn current_state(State(shared): S, interface: String) -> Response {
    match shared.agent.executor.states().value(&interface) {
        Some(state) if shared.agent.executor.is_activated() => Json(json!({
            "revision": shared.agent.executor.states().revision().1,
            "state": state,
        }))
        .into_response(),
        _ => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "State not initialized", "interface": interface })),
        )
            .into_response(),
    }
}

async fn list_locks(State(shared): S, Query(query): Query<Vec<(String, String)>>) -> Response {
    let keys = normalize_filter(&query_values(&query, "lock_keys"))
        .map(|k| k.into_iter().collect::<HashSet<_>>());
    Json(lock_collection(&shared.agent, keys.as_ref())).into_response()
}

// ---------------------------------------------------------------- schemas --

async fn schema_implementations(State(shared): S) -> Response {
    let implementations: Map<String, Value> = shared
        .agent
        .executor
        .registry()
        .implementations()
        .iter()
        .map(|i| (i.interface.clone(), schema::api_implementation(i)))
        .collect();
    Json(json!({ "count": implementations.len(), "implementations": implementations }))
        .into_response()
}

async fn schema_states(State(shared): S) -> Response {
    let states: Map<String, Value> = shared
        .agent
        .executor
        .registry()
        .states()
        .iter()
        .map(|s| (s.name.clone(), schema::api_state(s)))
        .collect();
    Json(json!({ "count": states.len(), "states": states })).into_response()
}

async fn schema_locks(State(shared): S) -> Response {
    let locks: Map<String, Value> = shared
        .agent
        .executor
        .registry()
        .lock_keys()
        .iter()
        .map(|k| (k.clone(), schema::api_lock(k)))
        .collect();
    Json(json!({ "count": locks.len(), "locks": locks })).into_response()
}

async fn schema_bloks() -> Response {
    Json(json!({ "count": 0, "bloks": {} })).into_response()
}

// ---------------------------------------------------------------- history --

fn resolve_session(agent: &LocalAgent, session_id: Option<String>) -> Result<String, Response> {
    session_id
        .filter(|s| !s.is_empty())
        .or_else(|| agent.current_session())
        .ok_or_else(|| detail(StatusCode::NOT_FOUND, "No active session"))
}

fn resolve_state_keys(
    agent: &LocalAgent,
    query: &[(String, String)],
) -> Result<Option<Vec<String>>, Response> {
    let keys = normalize_filter(&query_values(query, "state_keys"));
    if let Some(keys) = &keys {
        let known: HashSet<&str> = agent
            .executor
            .registry()
            .states()
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        let missing: Vec<&str> = keys
            .iter()
            .map(String::as_str)
            .filter(|k| !known.contains(k))
            .collect();
        if !missing.is_empty() {
            return Err(detail(
                StatusCode::UNPROCESSABLE_ENTITY,
                &format!("Unknown state keys: {}", missing.join(", ")),
            ));
        }
    }
    Ok(keys)
}

fn storage_error(e: anyhow::Error) -> Response {
    tracing::error!("state history: {e:#}");
    internal_error()
}

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(response) => return response,
        }
    };
}

async fn session_info(State(shared): S) -> Response {
    let pos = shared.agent.journal.watermark().map(|wm| wm.pos);
    Json(json!({ "current_session": shared.agent.current_session(), "current_pos": pos }))
        .into_response()
}

async fn task_boundaries(
    State(shared): S,
    Path(correlation_id): Path<String>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let state_id = query_one(&query, "state_id");
    match shared
        .agent
        .store
        .task_boundaries(&correlation_id, state_id.as_deref())
        .await
    {
        Ok(Some(boundary)) => Json(boundary).into_response(),
        Ok(None) => detail(StatusCode::NOT_FOUND, "Task boundaries not found"),
        Err(e) => storage_error(e),
    }
}

async fn session_boundary_response(
    agent: &LocalAgent,
    session_id: &str,
    state_id: Option<String>,
) -> Response {
    match agent
        .store
        .session_boundaries(session_id, state_id.as_deref())
        .await
    {
        Ok(Some(boundary)) => Json(boundary).into_response(),
        Ok(None) => detail(StatusCode::NOT_FOUND, "Session boundaries not found"),
        Err(e) => storage_error(e),
    }
}

async fn active_session_boundaries(
    State(shared): S,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let session = tri!(resolve_session(&shared.agent, None));
    session_boundary_response(&shared.agent, &session, query_one(&query, "state_id")).await
}

async fn state_session_boundaries(
    State(shared): S,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let session = tri!(resolve_session(
        &shared.agent,
        query_one(&query, "session_id")
    ));
    session_boundary_response(&shared.agent, &session, query_one(&query, "state_id")).await
}

async fn session_boundaries(
    State(shared): S,
    Path(session_id): Path<String>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    session_boundary_response(&shared.agent, &session_id, query_one(&query, "state_id")).await
}

async fn checkout(State(shared): S, Query(query): Query<Vec<(String, String)>>) -> Response {
    let agent = &shared.agent;
    let revision = tri!(int_param(
        "query",
        "global_revision_id",
        query_one(&query, "global_revision_id"),
        None,
        Some(0)
    ));
    let recent = tri!(int_param(
        "query",
        "recent_patch_count",
        query_one(&query, "recent_patch_count"),
        Some(5),
        Some(0)
    ));
    let keys = tri!(resolve_state_keys(agent, &query));
    let session = tri!(resolve_session(agent, query_one(&query, "session_id")));

    let mut states = Map::new();
    for declaration in agent.executor.registry().states() {
        if keys
            .as_ref()
            .is_some_and(|k| !k.contains(&declaration.name))
        {
            continue;
        }
        let value = match agent
            .store
            .state_at(revision, Some(&declaration.name), Some(&session))
            .await
        {
            Ok(Some(StateAt::Single(snapshot))) => Some(snapshot.data),
            Ok(_) => None,
            Err(e) => return storage_error(e),
        };
        states.insert(
            declaration.name.clone(),
            json!({
                "interface": declaration.name,
                "name": declaration.name,
                "initialized": value.is_some(),
                "value": value,
            }),
        );
    }
    let recent_patches = if recent == 0 {
        vec![]
    } else {
        let events = match agent
            .store
            .between(0, revision, keys.clone(), Some(&session))
            .await
        {
            Ok(events) => events,
            Err(e) => return storage_error(e),
        };
        let skip = events.len().saturating_sub(recent as usize);
        events.into_iter().skip(skip).collect()
    };
    Json(json!({
        "current_session": session,
        "current_global_revision": revision,
        "count": states.len(),
        "states": states,
        "recent_patches": recent_patches,
    }))
    .into_response()
}

async fn segments(State(shared): S, Query(query): Query<Vec<(String, String)>>) -> Response {
    let agent = &shared.agent;
    let from = tri!(int_param(
        "query",
        "from_global_revision_id",
        query_one(&query, "from_global_revision_id"),
        None,
        Some(0)
    ));
    let to = tri!(int_param(
        "query",
        "to_global_revision_id",
        query_one(&query, "to_global_revision_id"),
        None,
        Some(0)
    ));
    let keys = tri!(resolve_state_keys(agent, &query));
    let session = tri!(resolve_session(agent, query_one(&query, "session_id")));
    match agent.store.between(from, to, keys, Some(&session)).await {
        Ok(patches) => Json(json!({
            "from_global_revision": from,
            "to_global_revision": to,
            "patches": patches,
        }))
        .into_response(),
        Err(e) => storage_error(e),
    }
}

async fn state_at_response(
    agent: &LocalAgent,
    revision: i64,
    state_id: Option<String>,
    session: &str,
) -> Response {
    match agent
        .store
        .state_at(revision, state_id.as_deref(), Some(session))
        .await
    {
        Ok(Some(at)) => Json(at).into_response(),
        Ok(None) => detail(
            StatusCode::NOT_FOUND,
            "No state found for the requested revision",
        ),
        Err(e) => storage_error(e),
    }
}

async fn state_at_global(
    State(shared): S,
    Path((session_id, target_revision)): Path<(String, String)>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let revision = tri!(int_param(
        "path",
        "target_revision",
        Some(target_revision),
        None,
        None
    ));
    state_at_response(
        &shared.agent,
        revision,
        query_one(&query, "state_id"),
        &session_id,
    )
    .await
}

async fn current_state_at_global(
    State(shared): S,
    Path(target_revision): Path<String>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let revision = tri!(int_param(
        "path",
        "target_revision",
        Some(target_revision),
        None,
        None
    ));
    let session = tri!(resolve_session(&shared.agent, None));
    state_at_response(
        &shared.agent,
        revision,
        query_one(&query, "state_id"),
        &session,
    )
    .await
}

async fn forward_events(
    State(shared): S,
    Path((session_id, after)): Path<(String, String)>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let after = tri!(int_param(
        "path",
        "after_global_revision",
        Some(after),
        None,
        None
    ));
    let count = tri!(int_param(
        "query",
        "count",
        query_one(&query, "count"),
        Some(100),
        Some(1)
    ));
    let state_id = query_one(&query, "state_id");
    match shared
        .agent
        .store
        .forward_events(after, state_id.as_deref(), Some(&session_id), count)
        .await
    {
        Ok(events) => Json(events).into_response(),
        Err(e) => storage_error(e),
    }
}

async fn snapshots_around(
    State(shared): S,
    Path((session_id, target_revision)): Path<(String, String)>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let revision = tri!(int_param(
        "path",
        "target_revision",
        Some(target_revision),
        None,
        None
    ));
    let before = tri!(int_param(
        "query",
        "before",
        query_one(&query, "before"),
        Some(1),
        Some(0)
    ));
    let after = tri!(int_param(
        "query",
        "after",
        query_one(&query, "after"),
        Some(1),
        Some(0)
    ));
    let state_id = query_one(&query, "state_id");
    match shared
        .agent
        .store
        .snapshots_around(
            revision,
            state_id.as_deref(),
            Some(&session_id),
            before,
            after,
        )
        .await
    {
        Ok(snapshots) => Json(snapshots).into_response(),
        Err(e) => storage_error(e),
    }
}

// ---------------------------------------------------------------- journal --

fn opt_filter(query: &[(String, String)], name: &str) -> Option<Vec<String>> {
    normalize_filter(&query_values(query, name))
}

/// `current` resolves to the running session.
fn journal_session(agent: &LocalAgent, session_id: String) -> Result<String, Response> {
    if session_id == "current" {
        resolve_session(agent, None)
    } else {
        Ok(session_id)
    }
}

async fn journal_info(State(shared): S) -> Response {
    match shared.agent.journal.watermark() {
        Some(wm) => Json(wm).into_response(),
        None => detail(StatusCode::NOT_FOUND, "No active session"),
    }
}

async fn journal_entries(
    State(shared): S,
    Path(session_id): Path<String>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let agent = &shared.agent;
    let session = tri!(journal_session(agent, session_id));
    let after = tri!(int_param(
        "query",
        "after",
        query_one(&query, "after"),
        Some(0),
        Some(0)
    ));
    let limit = tri!(int_param(
        "query",
        "limit",
        query_one(&query, "limit"),
        Some(1000),
        Some(1)
    ));
    let until = match query_one(&query, "until") {
        Some(raw) => Some(tri!(int_param("query", "until", Some(raw), None, Some(0))) as u64),
        None => None,
    };
    if agent
        .journal
        .watermark()
        .is_some_and(|wm| wm.session_id == session)
    {
        agent.journal.flush(FLUSH_TIMEOUT).await;
    }
    let entries = agent
        .store
        .journal_entries(
            &session,
            EntryQuery {
                after: after as u64,
                until,
                limit: Some(limit as u64),
                kinds: opt_filter(&query, "kinds"),
                task_id: query_one(&query, "task_id"),
                action_keys: opt_filter(&query, "action_keys"),
                state_keys: opt_filter(&query, "state_keys"),
                lock_keys: opt_filter(&query, "lock_keys"),
            },
        )
        .await;
    match entries {
        Ok(entries) => {
            let last = entries.last().map(|e| e.pos);
            Json(json!({ "session_id": session, "after": after, "last_pos": last, "entries": entries })).into_response()
        }
        Err(e) => storage_error(e),
    }
}

async fn world_response(agent: &LocalAgent, session: &str, pos: u64) -> Response {
    if agent
        .journal
        .watermark()
        .is_some_and(|wm| wm.session_id == session)
    {
        agent.journal.flush_to(pos, FLUSH_TIMEOUT).await;
    }
    match agent.store.journal_world(session, pos).await {
        Ok(Some((entry, fold))) => {
            let mut world = world_json(&fold, None, None, None);
            world["session_id"] = json!(session);
            world["pos"] = json!(entry.pos);
            world["global_rev"] = json!(entry.global_rev);
            world["timepoint"] = json!(entry.timepoint);
            world["entry"] = json!(entry);
            Json(world).into_response()
        }
        Ok(None) => detail(StatusCode::NOT_FOUND, "No journal entry at that position"),
        Err(e) => storage_error(e),
    }
}

async fn journal_at(State(shared): S, Path((session_id, pos)): Path<(String, String)>) -> Response {
    let session = tri!(journal_session(&shared.agent, session_id));
    let pos = tri!(int_param("path", "pos", Some(pos), None, Some(1)));
    world_response(&shared.agent, &session, pos as u64).await
}

/// `?timestamp=` as epoch milliseconds or RFC 3339.
fn parse_timestamp(raw: &str) -> Option<i64> {
    raw.trim().parse::<i64>().ok().or_else(|| {
        chrono::DateTime::parse_from_rfc3339(raw.trim())
            .ok()
            .map(|t| t.timestamp_millis())
    })
}

async fn journal_at_time(
    State(shared): S,
    Path(session_id): Path<String>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let agent = &shared.agent;
    let session = tri!(journal_session(agent, session_id));
    let Some(raw) = query_one(&query, "timestamp") else {
        return invalid(
            "query",
            "timestamp",
            "missing",
            "Field required",
            Value::Null,
            None,
        );
    };
    let Some(ms) = parse_timestamp(&raw) else {
        return invalid(
            "query",
            "timestamp",
            "datetime_parsing",
            "Input should be a valid datetime",
            json!(raw),
            None,
        );
    };
    if agent
        .journal
        .watermark()
        .is_some_and(|wm| wm.session_id == session)
    {
        agent.journal.flush(FLUSH_TIMEOUT).await;
    }
    match agent.store.journal_pos_at_time(&session, ms).await {
        Ok(Some(pos)) => world_response(agent, &session, pos).await,
        Ok(None) => detail(
            StatusCode::NOT_FOUND,
            "No journal entry at or before that time",
        ),
        Err(e) => storage_error(e),
    }
}

async fn task_events(State(shared): S, Path(task_id): Path<String>) -> Response {
    shared.agent.journal.flush(FLUSH_TIMEOUT).await;
    match shared.agent.store.journal_task_entries(&task_id).await {
        Ok(entries) if entries.is_empty() => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Task not found", "task_id": task_id })),
        )
            .into_response(),
        Ok(entries) => {
            let fold = Fold::from_entries(&entries);
            Json(
                json!({ "task_id": task_id, "task": fold.tasks.get(&task_id), "entries": entries }),
            )
            .into_response()
        }
        Err(e) => storage_error(e),
    }
}

// ---------------------------------------------------------------- openapi --

fn build_openapi(agent: &LocalAgent, options: &ServeOptions) -> Value {
    let mut paths = Map::new();
    // The response models FastAPI documents for the agent routes.
    let mut components: Map<String, Value> =
        serde_json::from_str(include_str!("openapi_models.json")).expect("bundled schemas parse");
    let registry = agent.executor.registry();

    let command =
        |summary: &str, tag: &str| json!({ "post": { "tags": [tag], "summary": summary } });
    paths.insert(
        options.assign_path.clone(),
        command("Assign Base Action", "Agent"),
    );
    paths.insert(
        format!("{}/{{interface}}", options.assign_path),
        command("Assign Action", "Agent"),
    );
    for (path, summary) in [
        ("/cancel", "Cancel Action"),
        ("/pause", "Pause Action"),
        ("/resume", "Resume Action"),
        ("/step", "Step Action"),
    ] {
        paths.insert(path.into(), command(summary, "Agent"));
    }
    let listing =
        |summary: &str, tags: &[&str]| json!({ "get": { "tags": tags, "summary": summary } });
    paths.insert(
        options.tasks_path.clone(),
        listing("List tasks", &["Tasks"]),
    );
    paths.insert(
        format!("{}/{{task_id}}", options.tasks_path),
        listing("Get task details", &["Tasks", "Task Details"]),
    );
    paths.insert(
        options.states_path.clone(),
        listing("List states", &["States"]),
    );
    paths.insert(
        options.locks_path.clone(),
        listing("List locks", &["Locks"]),
    );
    if options.add_journal {
        paths.insert(
            "/journal".into(),
            listing("The journal's current position", &["Journal"]),
        );
        paths.insert(
            "/journal/{session_id}".into(),
            listing("Journal entries in order", &["Journal"]),
        );
        paths.insert(
            "/journal/{session_id}/at/{pos}".into(),
            listing("States, tasks and locks at a position", &["Journal"]),
        );
        paths.insert(
            "/journal/{session_id}/at".into(),
            listing("States, tasks and locks at a time", &["Journal"]),
        );
        paths.insert(
            format!("{}/{{task_id}}/events", options.tasks_path),
            listing("Every journal entry of a task", &["Journal", "Tasks"]),
        );
    }

    if options.add_implementations {
        for implementation in registry.implementations() {
            let definition = &implementation.definition;
            let name = &definition.name;
            let path = format!("/{}", implementation.interface);
            let mut operation = json!({
                "summary": name,
                "description": definition.description.clone().unwrap_or_else(|| format!("Execute {name} action")),
                "operationId": format!("implementation_endpoint_{}_post", implementation.interface),
                "requestBody": {
                    "content": { "application/json": { "schema": { "$ref": format!("#/components/schemas/{name}Request") } } },
                    "required": true,
                },
                "responses": {
                    "200": {
                        "description": "Successful Response",
                        "content": { "application/json": { "schema": { "$ref": format!("#/components/schemas/{name}Response") } } },
                    }
                },
            });
            if !definition.collections.is_empty() {
                operation["tags"] = json!(definition.collections);
            }
            paths.insert(path, json!({ "post": operation }));
            components.insert(format!("{name}Request"), schema::request_schema(definition));
            components.insert(
                format!("{name}Response"),
                schema::schema_from_ports(&definition.returns, &format!("{name}Response")),
            );
        }
    }
    for declaration in registry.states() {
        let title = format!("{}State", declaration.name);
        components.insert(
            title.clone(),
            schema::schema_from_ports(&declaration.ports, &title),
        );
    }

    json!({
        "openapi": "3.1.0",
        "info": { "title": options.title, "version": options.version },
        "paths": paths,
        "components": { "schemas": components },
    })
}

async fn openapi_json(State(shared): S) -> Response {
    Json(shared.openapi.clone()).into_response()
}

async fn docs_page() -> Html<&'static str> {
    Html(
        r##"<!doctype html>
<html>
<head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Agent API</title>
<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui.css">
</head>
<body>
<div id="swagger-ui"></div>
<script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
<script>SwaggerUIBundle({ url: "openapi.json", dom_id: "#swagger-ui" });</script>
</body>
</html>"##,
    )
}
