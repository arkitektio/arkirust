//! A board as the platform: arkitekt-core's [`Runtime`](arkitekt_core::Runtime)
//! logs in through the board's [`NetHost`] and serves with this crate's
//! [`Agent`], over embassy-net's TCP.
//!
//! ```ignore
//! let board = Board { host: &HOST, tcp: &tcp, dns: &dns, verify: Verify::Ca(ca) };
//! let runtime = Runtime::connect(board, app, options).await?;
//! runtime.serve().await
//! ```

use alloc::sync::Arc;

use arkitekt_core::Platform;
use embedded_nal_async::{Dns, TcpConnect};
use fakts::FaktsBuilder;

use crate::agent::{Agent, AgentOptions};
use crate::conn::Verify;
use crate::http::NetHost;

/// An app connected on a board.
pub type Runtime<'n, T, D> = arkitekt_core::Runtime<Board<'n, T, D>>;

/// The board's network: HTTP through `host` (served by
/// [`serve_http`](crate::serve_http)), the agent's websocket over `tcp`.
pub struct Board<'n, T, D> {
    pub host: &'static NetHost,
    pub tcp: &'n T,
    pub dns: &'n D,
    /// How `wss://` servers are checked (the login's `https://` ones are
    /// checked as `serve_http` was told).
    pub verify: Verify,
}

impl<T: TcpConnect, D: Dns> Platform for Board<'_, T, D> {
    fn login(&self, builder: FaktsBuilder) -> FaktsBuilder {
        builder.host(Arc::new(self.host))
    }

    async fn serve(&self, runtime: &Runtime<'_, T, D>) -> anyhow::Result<()> {
        let agent = agent(runtime).await?;
        let result = agent.run(self.tcp, self.dns).await;
        agent.shutdown().await;
        Ok(result?)
    }
}

/// The agent's options for `runtime`: rekuest's alias, the app's name.
pub async fn agent_options<T: TcpConnect, D: Dns>(
    runtime: &Runtime<'_, T, D>,
) -> anyhow::Result<AgentOptions> {
    let alias = runtime.agent_alias().await?;
    let mut options = AgentOptions::new(alias.to_ws_path("agi"));
    options.name = Some(runtime.agent_name());
    options.description = runtime.agent_description();
    options.force = runtime.options().force;
    options.verify = runtime.platform().verify.clone();
    if let Some(policy) = &runtime.options().policy {
        options.policy = policy.clone();
    }
    Ok(options)
}

/// The rekuest agent for `runtime`'s actions.
pub async fn agent<T: TcpConnect, D: Dns>(runtime: &Runtime<'_, T, D>) -> anyhow::Result<Agent> {
    let options = agent_options(runtime).await?;
    agent_with(runtime, options)
}

/// The rekuest agent for `runtime`'s actions, connecting as `options` say.
pub fn agent_with<T: TcpConnect, D: Dns>(
    runtime: &Runtime<'_, T, D>,
    options: AgentOptions,
) -> anyhow::Result<Agent> {
    Ok(Agent::new(
        options,
        runtime.app().registry().clone(),
        runtime.context().clone(),
        Arc::new(runtime.agent_fakts()?.clone()),
    ))
}
