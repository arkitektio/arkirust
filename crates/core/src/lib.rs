//! Connect and serve an Arkitekt app, on any platform.
//!
//! An [`App`] is a declaration (`arkitekt-spec`). A [`Runtime`] connects it:
//! it logs the app in (fakts), builds every service's client, and serves the
//! app's actions with the platform's rekuest agent. What differs between
//! platforms is a [`Platform`]: arkitekt-tokio (desktop and server),
//! arkitekt-web (a browser or node), arkitekt-embassy (a microcontroller).
//!
//! ```ignore
//! let runtime = Runtime::connect(platform, app, ConnectOptions::default()).await?;
//! runtime.serve().await
//! ```

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::any::{Any, TypeId};

use anyhow::Context as _;
use arkitekt_spec::{Alias, App, Context, Wiring};
use fakts::{Fakts, FaktsBuilder, FaktsError, Grant};
use rekuest::ConnectionPolicy;

pub use arkitekt_spec as spec;
pub use fakts;
pub use rekuest;

/// The public deployment, used unless the options (or the platform) say otherwise.
pub const DEFAULT_ARKITEKT_URL: &str = "https://go.arkitekt.live";

/// Where an app runs: what connecting and serving it needs from the platform.
// No `Send` bound on `serve`: a board's and a browser's agents are not `Send`.
#[allow(async_fn_in_trait)]
pub trait Platform: Sized {
    /// The deployment to log in at when the options name none (e.g. from
    /// the environment).
    fn url(&self) -> String {
        DEFAULT_ARKITEKT_URL.into()
    }

    /// This device's stable id when the options give none.
    fn device_id(&self) -> Option<String> {
        None
    }

    /// Set up the login: the platform's HTTP, where it keeps the login, how
    /// it reaches the mesh.
    fn login(&self, builder: FaktsBuilder) -> FaktsBuilder {
        builder
    }

    /// Serve `runtime`'s actions with this platform's rekuest agent, until
    /// it stops.
    async fn serve(&self, runtime: &Runtime<Self>) -> anyhow::Result<()>;
}

/// How to connect an [`App`], on any platform.
#[derive(Clone, Default)]
pub struct ConnectOptions {
    /// The deployment; defaults to the platform's ([`Platform::url`]).
    pub url: Option<String>,
    /// Log in afresh, neither reading nor writing a kept login.
    pub no_cache: bool,
    /// Where to keep the login (a firmware's NVS, a browser's storage); the
    /// platform's default otherwise.
    pub store: Option<Arc<dyn fakts::FaktsStore>>,
    /// How the app is authorized; fakts' default otherwise (a mandate key or
    /// a token from the environment where there is one, else device code).
    pub grant: Option<Grant>,
    /// This installation's stable id; the platform's otherwise.
    pub device_id: Option<String>,
    /// Take over from another running instance of this agent.
    pub force: bool,
    /// Reconnect policy of the agent.
    pub policy: Option<ConnectionPolicy>,
    /// Allow plain http to non-loopback hosts.
    pub allow_insecure_transport: bool,
    /// Join the deployment's mesh with a node the app runs itself (e.g. a
    /// firmware's); see [`fakts::MeshJoiner`].
    pub mesh_joiner: Option<Arc<dyn fakts::MeshJoiner>>,
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

    pub fn grant(mut self, grant: Grant) -> Self {
        self.grant = Some(grant);
        self
    }

    pub fn force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    pub fn mesh_joiner(mut self, joiner: Arc<dyn fakts::MeshJoiner>) -> Self {
        self.mesh_joiner = Some(joiner);
        self
    }
}

/// A connected app on platform `P`.
pub struct Runtime<P> {
    platform: P,
    app: App,
    /// `None` when the app needs no services (a served app without requirements).
    fakts: Option<Fakts>,
    context: Context,
    options: ConnectOptions,
}

impl<P> core::fmt::Debug for Runtime<P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Runtime")
            .field("app", &self.app.identifier())
            .field("fakts", &self.fakts)
            .finish_non_exhaustive()
    }
}

impl<P: Platform> Runtime<P> {
    /// Log in (from the kept login, a token or device code), then build
    /// every service's clients, in declaration order.
    pub async fn connect(platform: P, app: App, options: ConnectOptions) -> anyhow::Result<Self> {
        Self::connect_as(platform, app, options, true).await
    }

    /// Connect an app that is served without a rekuest server: only its
    /// services' requirements are requested, and an app without any logs in
    /// nowhere.
    pub async fn connect_local(
        platform: P,
        app: App,
        options: ConnectOptions,
    ) -> anyhow::Result<Self> {
        Self::connect_as(platform, app, options, false).await
    }

    async fn connect_as(
        platform: P,
        app: App,
        options: ConnectOptions,
        remote_agent: bool,
    ) -> anyhow::Result<Self> {
        let device_id = options.device_id.clone().or_else(|| platform.device_id());
        let manifest = app.manifest_for(device_id, remote_agent);
        if manifest.requirements.is_empty() && !remote_agent {
            return Ok(Self {
                platform,
                app,
                fakts: None,
                context: Context::default(),
                options,
            });
        }

        let url = options.url.clone().unwrap_or_else(|| platform.url());
        let mut builder = platform
            .login(Fakts::builder(&url, manifest))
            .no_cache(options.no_cache)
            .allow_insecure_transport(options.allow_insecure_transport);
        if let Some(store) = &options.store {
            builder = builder.store(store.clone());
        }
        if let Some(grant) = &options.grant {
            builder = builder.grant(grant.clone());
        }
        if let Some(joiner) = &options.mesh_joiner {
            builder = builder.mesh_joiner(joiner.clone());
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
            platform,
            app,
            fakts: Some(fakts),
            context: clients.build(),
            options,
        })
    }

    /// Serve the app's actions until the agent stops.
    pub async fn serve(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.app.provides(), "this app declares no actions to serve");
        tracing::info!(
            "serving {} action(s) as {}:{}",
            self.app.registry().len(),
            self.app.identifier(),
            self.app.version()
        );
        self.platform.serve(self).await
    }
}

impl<P> Runtime<P> {
    pub fn platform(&self) -> &P {
        &self.platform
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    pub fn options(&self) -> &ConnectOptions {
        &self.options
    }

    /// The login (`None` if the app needed no services).
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

    /// The login, for the agent (a runtime connected for local serving has none).
    pub fn agent_fakts(&self) -> anyhow::Result<&Fakts> {
        self.fakts
            .as_ref()
            .context("this runtime was connected for local serving and has no rekuest server")
    }

    /// Where the agent connects: rekuest's alias.
    pub async fn agent_alias(&self) -> anyhow::Result<Alias> {
        self.agent_fakts()?
            .get_alias("rekuest")
            .await
            .context("could not reach rekuest")
    }

    /// What the agent registers as: `identifier:version`.
    pub fn agent_name(&self) -> String {
        format!("{}:{}", self.app.identifier(), self.app.version())
    }

    /// The agent's description: the app's.
    pub fn agent_description(&self) -> Option<String> {
        self.app.manifest(None).description
    }
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

    fn provided(&self, type_id: TypeId) -> Option<Box<dyn Any + Send>> {
        (type_id == TypeId::of::<Fakts>()).then(|| Box::new(self.0.clone()) as Box<dyn Any + Send>)
    }
}

/// Lets the macros find the spec through arkitekt-core. Not a public API.
#[doc(hidden)]
pub mod __private {
    pub use arkitekt_spec as spec;
}
