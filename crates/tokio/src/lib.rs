//! Arkitekt apps on tokio: the desktop and server [`Platform`].
//!
//! It logs in over reqwest, keeps the login in a file in the user's
//! directories, finds the deployment and the mesh in the environment, and
//! serves the app's actions with rekuest's tokio agent (tungstenite, the
//! SQLite journal), until it stops or Ctrl-C.
//!
//! ```ignore
//! arkitekt_tokio::run(App::new("hello", "0.1.0").action(greet)).await
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use arkitekt_core::{ConnectOptions, Platform, DEFAULT_ARKITEKT_URL};
use arkitekt_spec::App;
use fakts::FaktsBuilder;
use rekuest::{Agent, AgentOptions};

pub use arkitekt_core::{self as core, spec};

/// An app connected on tokio.
pub type Runtime = arkitekt_core::Runtime<Tokio>;

/// tokio as the platform. `Tokio::default()` finds what it can in the
/// environment.
#[derive(Clone, Default)]
pub struct Tokio {
    /// Where to keep the login instead of the per-user state directory.
    pub cache_path: Option<PathBuf>,
    /// Reach mesh aliases through this running HTTP proxy (e.g. `arkitekt
    /// mesh proxy`); defaults to `$ARKITEKT_MESH_PROXY`.
    pub mesh_proxy: Option<String>,
    /// Join the deployment's mesh; `ARKITEKT_MESH=1` turns it on with the defaults
    /// (the `arkitekt-meshd` sidecar), `ARKITEKT_MESH=native` runs the node
    /// in-process instead (feature `mesh-native`).
    #[cfg(feature = "mesh")]
    pub mesh: Option<fakts::MeshOptions>,
}

impl Tokio {
    pub fn cache_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.cache_path = Some(path.into());
        self
    }

    pub fn mesh_proxy(mut self, proxy: impl Into<String>) -> Self {
        self.mesh_proxy = Some(proxy.into());
        self
    }

    #[cfg(feature = "mesh")]
    pub fn mesh(mut self, options: fakts::MeshOptions) -> Self {
        self.mesh = Some(options);
        self
    }
}

impl Platform for Tokio {
    /// `$FAKTS_URL`, then `$ARKITEKT_URL`, then the public deployment.
    fn url(&self) -> String {
        std::env::var("FAKTS_URL")
            .or_else(|_| std::env::var("ARKITEKT_URL"))
            .unwrap_or_else(|_| DEFAULT_ARKITEKT_URL.to_owned())
    }

    fn device_id(&self) -> Option<String> {
        device_id().ok()
    }

    fn login(&self, mut builder: FaktsBuilder) -> FaktsBuilder {
        if let Some(path) = &self.cache_path {
            builder = builder.cache_path(path);
        }
        if let Some(proxy) = self.mesh_proxy.clone().or_else(|| {
            std::env::var("ARKITEKT_MESH_PROXY")
                .ok()
                .filter(|p| !p.is_empty())
        }) {
            builder = builder.mesh_proxy(proxy);
        }
        #[cfg(feature = "mesh")]
        if let Some(mesh) = self.mesh.clone().or_else(|| {
            let backend = match std::env::var("ARKITEKT_MESH").as_deref() {
                Ok("1" | "true" | "sidecar") => fakts::MeshBackend::Sidecar,
                Ok("native") => fakts::MeshBackend::Native,
                _ => return None,
            };
            Some(fakts::MeshOptions {
                backend,
                ..Default::default()
            })
        }) {
            builder = builder.mesh(mesh);
        }
        builder
    }

    /// rekuest's tokio agent, its journal in `<identifier>.journal.db`,
    /// until it stops or Ctrl-C.
    async fn serve(&self, runtime: &Runtime) -> anyhow::Result<()> {
        let agent = agent(runtime).await?;
        let result = tokio::select! {
            result = agent.run() => result.map_err(anyhow::Error::from),
            _ = shutdown_signal() => {
                tracing::info!("shutting down");
                Ok(())
            }
        };
        agent.shutdown().await;
        result
    }
}

/// The rekuest agent for `runtime`'s actions.
pub async fn agent(runtime: &Runtime) -> anyhow::Result<Agent> {
    let fakts = runtime.agent_fakts()?;
    let alias = runtime.agent_alias().await?;
    let mut options = AgentOptions::new(alias.to_ws_path("agi"));
    if let Some(proxy) = &alias.proxy {
        options.connector = Arc::new(rekuest::HttpProxy::new(proxy));
    }
    options.name = Some(runtime.agent_name());
    options.description = runtime.agent_description();
    options.force = runtime.options().force;
    // One journal per app, so two apps in one directory never re-send each other's backlog.
    let file: String = runtime
        .app()
        .identifier()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    options.journal_path = Some(format!("{file}.journal.db").into());
    if let Some(policy) = &runtime.options().policy {
        options.policy = policy.clone();
    }
    Ok(Agent::new(
        options,
        runtime.app().registry().clone(),
        runtime.context().clone(),
        Arc::new(fakts.clone()),
    ))
}

/// Serve an app's actions: log in (once, then from the kept login), register
/// the actions and block until stopped. Nothing connects before this is called.
pub async fn run(app: App) -> anyhow::Result<()> {
    run_with(app, ConnectOptions::default()).await
}

/// [`run`], connected as `options` say.
pub async fn run_with(app: App, options: ConnectOptions) -> anyhow::Result<()> {
    connect_with(app, options).await?.serve().await
}

/// Connect an app without serving it, e.g. to call its services from a script.
pub async fn connect(app: App) -> anyhow::Result<Runtime> {
    connect_with(app, ConnectOptions::default()).await
}

/// [`connect`], as `options` say.
pub async fn connect_with(app: App, options: ConnectOptions) -> anyhow::Result<Runtime> {
    Runtime::connect(Tokio::default(), app, options).await
}

/// A stable id for this installation: `ARKITEKT_DEVICE_ID`, else the OS
/// machine id, else a UUID persisted in the user's config directory (the
/// `user-dirs` feature).
pub fn device_id() -> anyhow::Result<String> {
    if let Ok(id) = std::env::var("ARKITEKT_DEVICE_ID") {
        if !id.trim().is_empty() {
            return Ok(id.trim().to_owned());
        }
    }
    for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(id) = std::fs::read_to_string(path) {
            if !id.trim().is_empty() {
                return Ok(id.trim().to_owned());
            }
        }
    }
    persisted_device_id()
}

#[cfg(not(feature = "user-dirs"))]
fn persisted_device_id() -> anyhow::Result<String> {
    anyhow::bail!("no device id: set ARKITEKT_DEVICE_ID or ConnectOptions::device_id")
}

#[cfg(feature = "user-dirs")]
fn persisted_device_id() -> anyhow::Result<String> {
    use anyhow::Context as _;
    let path = dirs::config_dir()
        .context("no config directory")?
        .join("arkitekt")
        .join("device_id.txt");
    if let Ok(id) = std::fs::read_to_string(&path) {
        if !id.trim().is_empty() {
            return Ok(id.trim().to_owned());
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &id)?;
    Ok(id)
}

/// Resolves on Ctrl-C; never, without the `signal` feature (targets without
/// signals, where the app runs until the device resets).
pub async fn shutdown_signal() {
    #[cfg(feature = "signal")]
    let _ = tokio::signal::ctrl_c().await;
    #[cfg(not(feature = "signal"))]
    std::future::pending::<()>().await;
}

/// Lets the macros find the spec through arkitekt-tokio. Not a public API.
#[doc(hidden)]
pub mod __private {
    pub use arkitekt_spec as spec;
}
