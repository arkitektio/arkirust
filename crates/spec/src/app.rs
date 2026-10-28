//! The app declaration.

use core::future::Future;

#[allow(unused_imports)]
use crate::prelude::*;

use crate::action::{Action, Registry};
use crate::hooks::{Background, Startup};
use crate::manifest::{Manifest, PublicSource, Requirement};
use crate::service::Service;
use crate::state::StateType;

/// What an app is: its identity, the services it uses and the actions it offers.
///
/// An `App` does no I/O and holds no connection: a runtime runs it
/// (`arkitekt::run(app)`), as often as wanted.
///
/// ```ignore
/// let app = App::new("hello", "0.1.0")
///     .description("Says hello")
///     .service(mikro::service)
///     .action(greet);
/// arkitekt::run(app).await
/// ```
#[derive(Clone)]
pub struct App {
    identifier: String,
    version: String,
    description: Option<String>,
    logo: Option<String>,
    scopes: Vec<String>,
    public_sources: Vec<PublicSource>,
    services: Vec<Arc<dyn Service>>,
    registry: Registry,
}

impl core::fmt::Debug for App {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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
            scopes: vec![],
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

    /// Scopes the app asks for itself, beside `openid` and what its
    /// services' clients need.
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

    /// The services the app uses, in the order they were added.
    pub fn services(&self) -> &[Arc<dyn Service>] {
        &self.services
    }

    /// Whether this app offers anything (and so needs something to serve it).
    pub fn provides(&self) -> bool {
        !self.registry.is_empty()
    }

    /// The manifest, derived from the declaration: the requirements are
    /// exactly what whoever serves the app (`provider`: a rekuest agent
    /// needs rekuest, an app served over HTTP nothing) and the services need;
    /// the scopes are `openid`, the app's own and its services'.
    pub fn manifest(&self, device_id: Option<String>, provider: &[Requirement]) -> Manifest {
        let mut requirements = provider.to_vec();
        for service in &self.services {
            for requirement in service.requirements() {
                if !requirements
                    .iter()
                    .any(|r: &Requirement| r.key == requirement.key)
                {
                    requirements.push(requirement);
                }
            }
        }
        let mut manifest = Manifest::new(&self.identifier, &self.version);
        let mut scopes = Vec::from([String::from("openid")]);
        scopes.extend(self.scopes.iter().cloned());
        scopes.extend(self.services.iter().flat_map(|s| s.scopes()));
        scopes.sort();
        scopes.dedup();
        manifest.scopes = scopes;
        manifest.logo = self.logo.clone();
        manifest.description = self.description.clone();
        manifest.public_sources = self.public_sources.clone();
        manifest.requirements = requirements;
        manifest.device_id = device_id;
        manifest
    }
}
