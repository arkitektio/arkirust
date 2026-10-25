//! Serve an app over HTTP and a websocket, without a rekuest server: the
//! Rust port of Python's `arkitekt-fastapi` (`rekuest.contrib.fastapi`).
//!
//! ```ignore
//! let app = App::new("camera", "0.1.0").state(CameraState::default()).action(set_exposure);
//! let router = axum::Router::new().route("/health", get(|| async { "ok" }));
//! serve(app, router, ServeOptions::default()).await?.listen("0.0.0.0:8099").await
//! ```
//!
//! The actions become `POST /{interface}` and `POST /assign/{interface}`, an
//! observer websocket at `/ws` streams what happens, and `GET /states`,
//! `/tasks`, `/locks` and the state history routes show the app's state. The
//! journal (`/journal…`, or `"journal": true` on the websocket) records every
//! task event and state change in one order and replays any position.
//!
//! Or add the routes to a router yourself, for a registry without an `App`:
//!
//! ```ignore
//! let (router, agent) = arkitekt_serve::configure(axum::Router::new(), registry, Context::default(), ServeOptions::default())?;
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
mod local;
mod schema;
#[cfg(feature = "testing")]
pub mod testing;

use tokio::net::ToSocketAddrs;

pub use crate::local::{
    configure, AuthHook, AuthRequest, Broadcaster, EntryQuery, History, HistoryStore, LocalAgent,
    PatchEvent, ServeOptions, SessionBoundary, Snapshot, StateAt, SubscriptionInit, TaskBoundary,
    Unauthorized,
};
#[cfg(feature = "testing")]
pub use crate::testing::{AgentTestClient, AssignmentResult, Event};
pub use rekuest::journal::{Fold, Journal, JournalEntry, Watermark};

use arkitekt_core::ConnectOptions;
use arkitekt_spec::App;
use arkitekt_tokio::{Runtime, Tokio};

/// A served app: its router (the agent routes merged into yours), the agent
/// behind them, and the runtime holding its service clients.
pub struct Served {
    pub router: axum::Router,
    pub agent: LocalAgent,
    pub runtime: Runtime,
}

impl std::fmt::Debug for Served {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Served")
            .field("runtime", &self.runtime)
            .finish_non_exhaustive()
    }
}

impl Served {
    /// Serve on `addr` until Ctrl-C, then run shutdown hooks and flush the history.
    pub async fn listen(self, addr: impl ToSocketAddrs) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!(
            "serving {} action(s) of {}:{} on http://{}",
            self.runtime.app().registry().len(),
            self.runtime.app().identifier(),
            self.runtime.app().version(),
            listener.local_addr()?
        );
        let result = axum::serve(listener, self.router)
            .with_graceful_shutdown(async {
                arkitekt_tokio::shutdown_signal().await;
                tracing::info!("shutting down");
            })
            .await;
        self.agent.shutdown().await;
        Ok(result?)
    }

    /// Everything, for running the server yourself. Call
    /// [`LocalAgent::shutdown`] when you stop it.
    pub fn into_parts(self) -> (axum::Router, LocalAgent, Runtime) {
        (self.router, self.agent, self.runtime)
    }
}

/// Serve `app` with `router`'s own routes alongside the agent routes.
///
/// Services are connected first (authenticating only if some service needs
/// it), then startup hooks run and the session opens.
pub async fn serve(
    app: App,
    router: axum::Router,
    options: ServeOptions,
) -> anyhow::Result<Served> {
    serve_with(app, router, options, ConnectOptions::default()).await
}

/// [`serve`] with explicit connection options for the app's services.
pub async fn serve_with(
    app: App,
    router: axum::Router,
    mut options: ServeOptions,
    connect: ConnectOptions,
) -> anyhow::Result<Served> {
    if options.title == ServeOptions::default().title {
        options.title = app.identifier().to_owned();
        options.version = app.version().to_owned();
    }
    let runtime = Runtime::connect_local(Tokio::default(), app, connect).await?;
    let (router, agent) = configure(
        router,
        runtime.app().registry().clone(),
        runtime.context().clone(),
        options,
    )?;
    agent.start().await?;
    Ok(Served {
        router,
        agent,
        runtime,
    })
}
