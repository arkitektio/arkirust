//! Arkitekt apps in a browser or node (wasm32): the [`Platform`] over
//! `fetch` (the login, rath) and the JS `WebSocket` (rekuest's web agent),
//! on the JS event loop.
//!
//! Nothing is kept unless the options give a store, and the deployment is
//! the options' (there is no environment to find it in).
//!
//! ```ignore
//! let options = ConnectOptions::default().url("https://go.arkitekt.live");
//! wasm_bindgen_futures::spawn_local(async move {
//!     arkitekt_web::run(app, options).await.ok();
//! });
//! ```

#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

use std::sync::Arc;

use arkitekt_core::{ConnectOptions, Platform};
use arkitekt_spec::App;
use rekuest::web::{Agent, AgentOptions};

pub use arkitekt_core::{self as core, spec};

/// An app connected in a browser or node.
pub type Runtime = arkitekt_core::Runtime<Browser>;

/// The browser (or node) as the platform.
#[derive(Debug, Clone, Copy, Default)]
pub struct Browser;

impl Platform for Browser {
    /// rekuest's web agent, until it stops.
    async fn serve(&self, runtime: &Runtime) -> anyhow::Result<()> {
        let agent = agent(runtime).await?;
        let result = agent.run().await.map_err(anyhow::Error::from);
        agent.shutdown().await;
        result
    }
}

/// The rekuest agent for `runtime`'s actions.
pub async fn agent(runtime: &Runtime) -> anyhow::Result<Agent> {
    let fakts = runtime.agent_fakts()?;
    let alias = runtime.agent_alias().await?;
    let mut options = AgentOptions::new(alias.to_ws_path("agi"));
    options.name = Some(runtime.agent_name());
    options.description = runtime.agent_description();
    options.force = runtime.options().force;
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

/// Serve an app's actions until the agent stops.
pub async fn run(app: App, options: ConnectOptions) -> anyhow::Result<()> {
    connect(app, options).await?.serve().await
}

/// Connect an app without serving it, e.g. to call its services.
pub async fn connect(app: App, options: ConnectOptions) -> anyhow::Result<Runtime> {
    Runtime::connect(Browser, app, options).await
}

/// Lets the macros find the spec through arkitekt-web. Not a public API.
#[doc(hidden)]
pub mod __private {
    pub use arkitekt_spec as spec;
}
