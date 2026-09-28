//! Serve an app over HTTP and a websocket, without a rekuest server
//! (Python's `arkitekt.serve`).
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
//! task event and state change in one order and replays any position. See
//! [`rekuest::serve`] for every route.

use tokio::net::ToSocketAddrs;

pub use rekuest::journal::{Fold, Journal, JournalEntry, Watermark};
#[cfg(feature = "testing")]
pub use rekuest::serve::testing::{AgentTestClient, AssignmentResult, Event};
pub use rekuest::serve::{
    AuthHook, AuthRequest, EntryQuery, History, HistoryStore, LocalAgent, ServeOptions,
    SubscriptionInit, Unauthorized,
};

use crate::app::App;
use crate::runtime::{ConnectOptions, Runtime};

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
                let _ = tokio::signal::ctrl_c().await;
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
    let runtime = Runtime::connect_local(app, connect).await?;
    let (router, agent) = rekuest::serve::configure(
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
