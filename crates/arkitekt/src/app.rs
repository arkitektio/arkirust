//! The app declaration.

use std::future::Future;
use std::sync::Arc;

use fakts::{Manifest, PublicSource};

use rekuest::{Action, Background, Registry, Startup, StateType};

use crate::runtime::{ConnectOptions, Runtime};
use crate::service::{rekuest_requirement, Service};

/// What an app is: its identity, the services it uses and the actions it offers.
///
/// An `App` holds no connection state; [`App::connect`] or [`App::run`]
/// turn it into a [`Runtime`].
///
/// ```ignore
/// App::new("hello", "0.1.0")
///     .description("Says hello")
///     .service(mikro::service)
///     .action(greet)
///     .run()
///     .await
/// ```
#[derive(Clone)]
pub struct App {
    pub(crate) identifier: String,
    pub(crate) version: String,
    pub(crate) description: Option<String>,
    pub(crate) logo: Option<String>,
    pub(crate) scopes: Vec<String>,
    pub(crate) public_sources: Vec<PublicSource>,
    pub(crate) services: Vec<Arc<dyn Service>>,
    pub(crate) registry: Registry,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("identifier", &self.identifier)
            .field("version", &self.version)
            .field(
                "services",
                &self.services.iter().map(|s| s.name()).collect::<Vec<_>>(),
            )
            .field("actions", &self.registry)
            .finish()
    }
}

impl App {
    pub fn new(identifier: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            identifier: identifier.into(),
            version: version.into(),
            description: None,
            logo: None,
            scopes: vec!["openid".into()],
            public_sources: vec![],
            services: vec![],
            registry: Registry::new(),
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn logo(mut self, logo: impl Into<String>) -> Self {
        self.logo = Some(logo.into());
        self
    }

    pub fn scopes<I: IntoIterator<Item = S>, S: Into<String>>(mut self, scopes: I) -> Self {
        self.scopes = scopes.into_iter().map(Into::into).collect();
        self
    }

    pub fn public_source(mut self, kind: impl Into<String>, url: impl Into<String>) -> Self {
        self.public_sources.push(PublicSource {
            kind: kind.into(),
            url: url.into(),
        });
        self
    }

    /// Use a service; its clients become injectable into actions.
    pub fn service<S: Service>(mut self, service: S) -> Self {
        self.services.retain(|s| s.name() != service.name());
        self.services.push(Arc::new(service));
        self
    }

    /// Offer an action (a function annotated with `#[arkitekt::action]`).
    pub fn action<A: Action>(mut self, action: A) -> Self {
        self.registry.register(action);
        self
    }

    /// Declare a state (a `#[derive(State)]` struct) with its initial value.
    /// Every change actions make to it is published, so do not add actions
    /// that only read it back.
    pub fn state<T: StateType>(mut self, initial: T) -> Self {
        self.registry.state(initial);
        self
    }

    /// Declare a state whose initial value a startup hook provides.
    pub fn declare_state<T: StateType>(mut self) -> Self {
        self.registry.declare_state::<T>();
        self
    }

    /// Run once before any action: connect hardware, set states, provide contexts.
    pub fn startup<F, Fut>(mut self, hook: F) -> Self
    where
        F: Fn(Startup) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.registry.startup(hook);
        self
    }

    /// Run for the app's lifetime (cancelled on shutdown).
    pub fn background<F, Fut>(mut self, hook: F) -> Self
    where
        F: Fn(Background) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.registry.background(hook);
        self
    }

    /// Run once when the app stops.
    pub fn shutdown<F, Fut>(mut self, hook: F) -> Self
    where
        F: Fn(Background) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.registry.shutdown(hook);
        self
    }

    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Whether this app serves actions (and so needs the rekuest agent).
    pub fn provides(&self) -> bool {
        !self.registry.is_empty()
    }

    /// The manifest, derived from the declaration: the requirements are
    /// exactly what the services (and, if actions are offered, the agent) need.
    pub fn manifest(&self, device_id: Option<String>) -> Manifest {
        self.manifest_for(device_id, true)
    }

    /// The manifest; `remote_agent` adds the rekuest requirement when the app
    /// offers actions (a served app needs no rekuest server).
    pub fn manifest_for(&self, device_id: Option<String>, remote_agent: bool) -> Manifest {
        let mut requirements = vec![];
        if remote_agent && self.provides() {
            requirements.push(rekuest_requirement());
        }
        for service in &self.services {
            for requirement in service.requirements() {
                if !requirements
                    .iter()
                    .any(|r: &fakts::Requirement| r.key == requirement.key)
                {
                    requirements.push(requirement);
                }
            }
        }
        let mut manifest = Manifest::new(&self.identifier, &self.version);
        manifest.scopes = self.scopes.clone();
        manifest.logo = self.logo.clone();
        manifest.description = self.description.clone();
        manifest.public_sources = self.public_sources.clone();
        manifest.requirements = requirements;
        manifest.device_id = device_id;
        manifest
    }

    /// Authorize (once, then from cache) and build every service client.
    pub async fn connect(self) -> anyhow::Result<Runtime> {
        Runtime::connect(self, ConnectOptions::default()).await
    }

    pub async fn connect_with(self, options: ConnectOptions) -> anyhow::Result<Runtime> {
        Runtime::connect(self, options).await
    }

    /// Connect and serve the app's actions until the agent stops.
    pub async fn run(self) -> anyhow::Result<()> {
        self.connect().await?.serve().await
    }
}

/// Serve an app's actions: authenticate (once, then from cache), register the
/// actions and block until stopped. Nothing connects before this is called.
pub async fn run(app: App) -> anyhow::Result<()> {
    app.run().await
}

/// Connect an app without serving it, e.g. to call its services from a script.
pub async fn connect(app: App) -> anyhow::Result<Runtime> {
    app.connect().await
}

/// An app for scripts that only call services: `easy("script", "0.1.0").service(..).connect()`.
pub fn easy(identifier: impl Into<String>, version: impl Into<String>) -> App {
    App::new(identifier, version)
}
