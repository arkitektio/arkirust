//! Actions: typed functions exposed to Arkitekt, and the registry holding them.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{Map, Value};

use crate::context::Context;
use crate::definition::{Definition, Implementation};
use crate::task::Task;

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

    fn implementation(&self) -> Implementation {
        Implementation::new(self.interface(), self.definition())
    }
}

/// The actions an app offers, in registration order.
#[derive(Clone, Default)]
pub struct Registry {
    actions: Vec<Arc<dyn Action>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.actions.iter().map(|a| a.interface()))
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

    pub fn get(&self, interface: &str) -> Option<Arc<dyn Action>> {
        self.actions
            .iter()
            .find(|a| a.interface() == interface)
            .cloned()
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
}
