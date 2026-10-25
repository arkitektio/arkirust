//! A connected app: fakts loaded, clients built, ready to serve.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use arkitekt_spec::{Alias, App, Context, Wiring};
use fakts::{Fakts, FaktsError, Grant};
use rekuest::{Agent, AgentOptions, ConnectionPolicy};

/// The public deployment, used unless `FAKTS_URL` (or `ARKITEKT_URL`) says otherwise.
pub const DEFAULT_ARKITEKT_URL: &str = "https://go.arkitekt.live";

/// How to connect an [`App`].
#[derive(Clone, Default)]
pub struct ConnectOptions {
    /// Server url; defaults to `$FAKTS_URL`, then `$ARKITEKT_URL`, then [`DEFAULT_ARKITEKT_URL`].
    pub url: Option<String>,
    /// Ignore the fakts cache and authorize again.
    pub no_cache: bool,
    /// Where to cache fakts instead of the per-user state directory.
    pub cache_path: Option<PathBuf>,
    /// Keep fakts somewhere other than a file (e.g. a firmware's NVS); wins
    /// over `cache_path`.
    pub store: Option<Arc<dyn fakts::FaktsStore>>,
    /// Override how the app is authorized (default: `FAKTS_MANDATE_KEY`, else
    /// `FAKTS_TOKEN`, else device code).
    pub grant: Option<Grant>,
    /// Stable id of this installation; defaults to [`device_id`].
    pub device_id: Option<String>,
    /// Take over from another running instance of this agent.
    pub force: bool,
    /// Reconnect policy of the agent.
    pub policy: Option<ConnectionPolicy>,
    /// Allow plain http to non-loopback hosts.
    pub allow_insecure_transport: bool,
    /// Reach mesh aliases through this running HTTP proxy (e.g. `arkitekt
    /// mesh proxy`); defaults to `$ARKITEKT_MESH_PROXY`.
    pub mesh_proxy: Option<String>,
    /// Join the deployment's mesh with a node the app runs itself (e.g. a
    /// firmware's); see [`fakts::MeshJoiner`]. Wins over `mesh`.
    pub mesh_joiner: Option<Arc<dyn fakts::MeshJoiner>>,
    /// Join the deployment's mesh; `ARKITEKT_MESH=1` turns it on with the defaults
    /// (the `arkitekt-meshd` sidecar), `ARKITEKT_MESH=native` runs the node
    /// in-process instead (feature `mesh-native`).
    #[cfg(feature = "mesh")]
    pub mesh: Option<fakts::MeshOptions>,
}

impl ConnectOptions {
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    pub fn no_cache(mut self, no_cache: bool) -> Self {
        self.no_cache = no_cache;
        self
    }

    pub fn store(mut self, store: Arc<dyn fakts::FaktsStore>) -> Self {
        self.store = Some(store);
        self
    }

    pub fn force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    pub fn mesh_proxy(mut self, proxy: impl Into<String>) -> Self {
        self.mesh_proxy = Some(proxy.into());
        self
    }

    pub fn mesh_joiner(mut self, joiner: Arc<dyn fakts::MeshJoiner>) -> Self {
        self.mesh_joiner = Some(joiner);
        self
    }

    #[cfg(feature = "mesh")]
    pub fn mesh(mut self, options: fakts::MeshOptions) -> Self {
        self.mesh = Some(options);
        self
    }
}

/// A connected app.
pub struct Runtime {
    app: App,
    /// `None` when the app needs no services (a served app without requirements).
    fakts: Option<Fakts>,
    context: Context,
    options: ConnectOptions,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("app", &self.app.identifier())
            .field("fakts", &self.fakts)
            .finish_non_exhaustive()
    }
}

impl Runtime {
    /// Load fakts (from cache, token or device code), then build every
    /// service's clients, in declaration order.
    pub async fn connect(app: App, options: ConnectOptions) -> anyhow::Result<Self> {
        Self::connect_as(app, options, true).await
    }

    /// Connect an app that is served locally ([`serve`](crate::serve)): only
    /// its services' requirements are requested, and an app without any
    /// authenticates nothing.
    pub async fn connect_local(app: App, options: ConnectOptions) -> anyhow::Result<Self> {
        Self::connect_as(app, options, false).await
    }

    async fn connect_as(
        app: App,
        options: ConnectOptions,
        remote_agent: bool,
    ) -> anyhow::Result<Self> {
        let device_id = options.device_id.clone().or_else(|| device_id().ok());
        let manifest = app.manifest_for(device_id, remote_agent);
        if manifest.requirements.is_empty() && !remote_agent {
            return Ok(Self {
                app,
                fakts: None,
                context: Context::default(),
                options,
            });
        }

        let url = options
            .url
            .clone()
            .or_else(|| std::env::var("FAKTS_URL").ok())
            .or_else(|| std::env::var("ARKITEKT_URL").ok())
            .unwrap_or_else(|| DEFAULT_ARKITEKT_URL.to_owned());
        let mut builder = Fakts::builder(&url, manifest)
            .no_cache(options.no_cache)
            .allow_insecure_transport(options.allow_insecure_transport);
        if let Some(path) = &options.cache_path {
            builder = builder.cache_path(path);
        }
        if let Some(store) = &options.store {
            builder = builder.store(store.clone());
        }
        if let Some(grant) = &options.grant {
            builder = builder.grant(grant.clone());
        }
        if let Some(proxy) = options.mesh_proxy.clone().or_else(|| {
            std::env::var("ARKITEKT_MESH_PROXY")
                .ok()
                .filter(|p| !p.is_empty())
        }) {
            builder = builder.mesh_proxy(proxy);
        }
        if let Some(joiner) = &options.mesh_joiner {
            builder = builder.mesh_joiner(joiner.clone());
        }
        #[cfg(feature = "mesh")]
        if let Some(mesh) = options.mesh.clone().or_else(|| {
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
        let fakts = builder
            .load()
            .await
            .with_context(|| format!("could not load the configuration from {url}"))?;

        let mut clients = Context::builder();
        clients.insert(fakts.clone());
        for service in app.services() {
            service
                .build(&FaktsWiring(&fakts), &mut clients)
                .await
                .with_context(|| format!("could not build the {} service", service.name()))?;
        }

        Ok(Self {
            app,
            fakts: Some(fakts),
            context: clients.build(),
            options,
        })
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    /// The loaded configuration (`None` if the app needed no services).
    pub fn fakts(&self) -> Option<&Fakts> {
        self.fakts.as_ref()
    }

    /// The client lookup actions receive.
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// The client of type `T`, if a service built one.
    pub fn get<T: Clone + Send + Sync + 'static>(&self) -> Option<T> {
        self.context.get::<T>()
    }

    /// The client of type `T`, or an error naming the missing service.
    pub fn require<T: Clone + Send + Sync + 'static>(&self) -> anyhow::Result<T> {
        Ok(self.context.require::<T>()?)
    }

    /// Build the rekuest agent for this app's actions.
    pub async fn agent(&self) -> anyhow::Result<Agent> {
        let fakts = self
            .fakts
            .as_ref()
            .context("this runtime was connected for local serving and has no rekuest server")?;
        let alias = fakts
            .get_alias("rekuest")
            .await
            .context("could not reach rekuest")?;
        let mut options = AgentOptions::new(alias.to_ws_path("agi"));
        if let Some(proxy) = &alias.proxy {
            options.connector = Arc::new(rekuest::HttpProxy::new(proxy));
        }
        options.name = Some(format!("{}:{}", self.app.identifier(), self.app.version()));
        options.description = self.app.manifest(None).description;
        options.force = self.options.force;
        // One journal per app, so two apps in one directory never re-send each other's backlog.
        let file: String = self
            .app
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
        if let Some(policy) = &self.options.policy {
            options.policy = policy.clone();
        }
        Ok(Agent::new(
            options,
            self.app.registry().clone(),
            self.context.clone(),
            Arc::new(fakts.clone()),
        ))
    }

    /// Serve the app's actions until the agent stops (or Ctrl-C).
    pub async fn serve(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.app.provides(), "this app declares no actions to serve");
        let agent = self.agent().await?;
        tracing::info!(
            "serving {} action(s) as {}:{}",
            self.app.registry().len(),
            self.app.identifier(),
            self.app.version()
        );
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

/// Serve an app's actions: authenticate (once, then from cache), register the
/// actions and block until stopped. Nothing connects before this is called.
pub async fn run(app: App) -> anyhow::Result<()> {
    Runtime::connect(app, ConnectOptions::default())
        .await?
        .serve()
        .await
}

/// [`run`], connected as `options` say.
pub async fn run_with(app: App, options: ConnectOptions) -> anyhow::Result<()> {
    Runtime::connect(app, options).await?.serve().await
}

/// Connect an app without serving it, e.g. to call its services from a script.
pub async fn connect(app: App) -> anyhow::Result<Runtime> {
    Runtime::connect(app, ConnectOptions::default()).await
}

/// [`connect`], as `options` say.
pub async fn connect_with(app: App, options: ConnectOptions) -> anyhow::Result<Runtime> {
    Runtime::connect(app, options).await
}

/// An app for scripts that only call services: `connect(easy("script", "0.1.0").service(..))`.
pub fn easy(identifier: impl Into<String>, version: impl Into<String>) -> App {
    App::new(identifier, version)
}

/// Services are built from the app's fakts: each requirement's alias, and
/// the `Fakts` itself (the token loader) for whoever asks for it.
struct FaktsWiring<'a>(&'a Fakts);

#[async_trait::async_trait]
impl Wiring for FaktsWiring<'_> {
    async fn alias(&self, key: &str) -> anyhow::Result<Option<Alias>> {
        match self.0.get_alias(key).await {
            Ok(alias) => Ok(Some(alias)),
            Err(FaktsError::MissingInstance { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn provided(&self, type_id: std::any::TypeId) -> Option<Box<dyn std::any::Any + Send>> {
        (type_id == std::any::TypeId::of::<Fakts>())
            .then(|| Box::new(self.0.clone()) as Box<dyn std::any::Any + Send>)
    }
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
pub(crate) async fn shutdown_signal() {
    #[cfg(feature = "signal")]
    let _ = tokio::signal::ctrl_c().await;
    #[cfg(not(feature = "signal"))]
    std::future::pending::<()>().await;
}
