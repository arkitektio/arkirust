//! Actions: typed functions exposed to Arkitekt, and the registry holding them.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{Map, Value};

use crate::context::Context;
use crate::definition::{AgentDeclaration, Definition, Implementation};
use crate::hooks::{hook_fn, Background, Hooks, Startup};
use crate::state::{StateDeclaration, StateType};
use crate::task::Task;

/// Whether assignments of one action may run at the same time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Concurrency {
    /// One assignment at a time (the default, as in Python).
    #[default]
    Serial,
    /// Any number at once.
    Parallel,
}

/// How an action run failed, which decides the event reported to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    /// The inputs could not be expanded or the outputs shrunk (`FAILED`).
    Failed(String),
    /// The action body returned an error (`CRITICAL`), as in Python where an
    /// exception raised by the function is critical.
    Critical(String),
}

impl ActionError {
    /// Wrap an error returned by the action body.
    pub fn from_body<E: std::fmt::Display>(error: E) -> Self {
        ActionError::Critical(format!("{error:#}"))
    }
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionError::Failed(m) | ActionError::Critical(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ActionError {}

/// Something an agent can run. Implemented by `#[action]`.
pub trait Action: Send + Sync + 'static {
    /// The interface the action is served under (unique per agent).
    fn interface(&self) -> String;

    /// The action's public contract.
    fn definition(&self) -> Definition;

    /// Expand `args`, run, and report results through `task.yield_returns`.
    fn run(
        &self,
        args: Map<String, Value>,
        ctx: Context,
        task: Task,
    ) -> BoxFuture<'static, Result<(), ActionError>>;

    /// Locks held while an assignment runs (declared, plus those required by
    /// the states it takes).
    fn locks(&self) -> Vec<String> {
        vec![]
    }

    /// The states this action changes.
    fn manipulates(&self) -> Vec<String> {
        vec![]
    }

    fn concurrency(&self) -> Concurrency {
        Concurrency::Serial
    }

    fn implementation(&self) -> Implementation {
        let mut implementation = Implementation::new(self.interface(), self.definition());
        implementation.locks = self.locks();
        implementation.manipulates = self.manipulates();
        implementation
    }
}

/// Everything an app offers: actions, states and hooks.
#[derive(Clone, Default)]
pub struct Registry {
    actions: Vec<Arc<dyn Action>>,
    states: Vec<StateDeclaration>,
    hooks: Hooks,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("actions", &self.actions.iter().map(|a| a.interface()).collect::<Vec<_>>())
            .field("states", &self.states.iter().map(|s| &s.name).collect::<Vec<_>>())
            .field("hooks", &self.hooks)
            .finish()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an action; a later registration under the same interface replaces the earlier one.
    pub fn register<A: Action>(&mut self, action: A) -> &mut Self {
        let interface = action.interface();
        self.actions.retain(|a| a.interface() != interface);
        self.actions.push(Arc::new(action));
        self
    }

    /// Declare a state with its initial value (a startup hook may replace it).
    pub fn state<T: StateType>(&mut self, initial: T) -> &mut Self {
        self.states.retain(|s| s.name != T::NAME);
        self.states.push(StateDeclaration::of(Some(initial)));
        self
    }

    /// Declare a state whose value a startup hook provides.
    pub fn declare_state<T: StateType>(&mut self) -> &mut Self {
        self.states.retain(|s| s.name != T::NAME);
        self.states.push(StateDeclaration::of::<T>(None));
        self
    }

    /// Run `hook` once before any action (see [`Startup`]).
    pub fn startup<F, Fut>(&mut self, hook: F) -> &mut Self
    where
        F: Fn(Startup) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.hooks.startup.push(hook_fn(hook));
        self
    }

    /// Run `hook` for the agent's lifetime (cancelled on shutdown).
    pub fn background<F, Fut>(&mut self, hook: F) -> &mut Self
    where
        F: Fn(Background) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.hooks.background.push(hook_fn(hook));
        self
    }

    /// Run `hook` once when the agent stops.
    pub fn shutdown<F, Fut>(&mut self, hook: F) -> &mut Self
    where
        F: Fn(Background) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.hooks.shutdown.push(hook_fn(hook));
        self
    }

    pub fn get(&self, interface: &str) -> Option<Arc<dyn Action>> {
        self.actions.iter().find(|a| a.interface() == interface).cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn len(&self) -> usize {
        self.actions.len()
    }

    pub fn implementations(&self) -> Vec<Implementation> {
        self.actions.iter().map(|a| a.implementation()).collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Action>> {
        self.actions.iter()
    }

    pub fn states(&self) -> &[StateDeclaration] {
        &self.states
    }

    pub(crate) fn hooks(&self) -> &Hooks {
        &self.hooks
    }

    /// Every lock key used by an action, in first-use order.
    pub fn lock_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = vec![];
        for action in &self.actions {
            for key in action.locks() {
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
        keys
    }

    /// The declaration sent in `REGISTER`.
    pub fn declaration(&self, name: Option<String>, description: Option<String>) -> AgentDeclaration {
        let mut declaration = AgentDeclaration {
            name,
            description,
            implementations: self.implementations(),
            states: self.states.iter().map(StateDeclaration::to_declaration_json).collect(),
            locks: self
                .lock_keys()
                .into_iter()
                .map(|key| {
                    serde_json::json!({
                        "key": key,
                        "definition": { "key": key, "description": format!("Lock definition for {key}") },
                    })
                })
                .collect(),
            bloks: vec![],
            hash: None,
        };
        declaration.hash = Some(declaration.compute_hash());
        declaration
    }
}
