//! Startup, background and shutdown hooks.
//!
//! * A **startup** hook runs once before any action: connect hardware, give
//!   states their initial values ([`Startup::set_state`]) and provide contexts
//!   ([`Startup::set_context`]) that actions then take with `#[inject]`.
//! * A **background** hook runs for the agent's lifetime and is cancelled on
//!   shutdown. It may change states, but only ones that require no locks.
//! * A **shutdown** hook runs once when the agent stops.

#[allow(unused_imports)]
use crate::prelude::*;
use crate::sync::Mutex;
use core::future::Future;

use futures::future::BoxFuture;

use crate::context::{Context, ContextBuilder, MissingClient};
use crate::state::{Mutation, StateCells, StateError, StateMut, StateRef, StateType};

/// A declared hook.
pub type HookFn<H> = Arc<dyn Fn(H) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

pub(crate) fn hook_fn<H, F, Fut>(f: F) -> HookFn<H>
where
    H: Send + 'static,
    F: Fn(H) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    Arc::new(move |handle| Box::pin(f(handle)))
}

/// The hooks an app declares.
#[derive(Clone, Default)]
pub struct Hooks {
    pub startup: Vec<HookFn<Startup>>,
    pub background: Vec<HookFn<Background>>,
    pub shutdown: Vec<HookFn<Background>>,
}

impl core::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Hooks")
            .field("startup", &self.startup.len())
            .field("background", &self.background.len())
            .field("shutdown", &self.shutdown.len())
            .finish()
    }
}

type ContextEdit = Box<dyn FnOnce(&mut ContextBuilder) + Send>;

/// What a startup hook gets.
#[derive(Clone)]
pub struct Startup {
    ctx: Context,
    states: Arc<StateCells>,
    contexts: Arc<Mutex<Vec<ContextEdit>>>,
}

impl Startup {
    /// For the runtime that runs the hooks.
    pub fn new(ctx: Context, states: Arc<StateCells>) -> Self {
        Self {
            ctx,
            states,
            contexts: Arc::default(),
        }
    }

    /// A service client.
    pub fn client<T: Clone + Send + Sync + 'static>(&self) -> Option<T> {
        self.ctx.get::<T>()
    }

    pub fn require<T: Clone + Send + Sync + 'static>(&self) -> Result<T, MissingClient> {
        self.ctx.require::<T>()
    }

    /// Give a declared state its value for this session.
    pub fn set_state<T: StateType>(&self, value: T) -> Result<(), StateError> {
        self.states.init(value)
    }

    /// Provide a context (any shared value, e.g. a device handle) that actions
    /// take with `#[inject]`.
    pub fn set_context<T: Clone + Send + Sync + 'static>(&self, value: T) {
        self.contexts
            .lock()
            .push(Box::new(move |builder: &mut ContextBuilder| {
                builder.insert(value);
            }));
    }

    /// The contexts the hooks provided, if any, on top of `ctx`. For the runtime.
    pub fn provided_context(&self, ctx: &Context) -> Option<Context> {
        let edits = core::mem::take(&mut *self.contexts.lock());
        if edits.is_empty() {
            return None;
        }
        let mut builder = ctx.to_builder();
        for edit in edits {
            edit(&mut builder);
        }
        Some(builder.build())
    }
}

/// What background and shutdown hooks get.
#[derive(Clone)]
pub struct Background {
    ctx: Context,
    states: Arc<StateCells>,
}

/// Shutdown hooks get the same handle as background hooks.
pub type Shutdown = Background;

impl Background {
    /// For the runtime that runs the hooks.
    pub fn new(ctx: Context, states: Arc<StateCells>) -> Self {
        Self { ctx, states }
    }

    /// A service client or context.
    pub fn client<T: Clone + Send + Sync + 'static>(&self) -> Option<T> {
        self.ctx.get::<T>()
    }

    pub fn require<T: Clone + Send + Sync + 'static>(&self) -> Result<T, MissingClient> {
        self.ctx.require::<T>()
    }

    /// A state to change outside of any task. Changes are not attributed to a
    /// task, and states that require locks refuse them.
    pub fn state<T: StateType>(&self) -> Result<StateMut<T>, StateError> {
        self.states.state_mut::<T>(Mutation::unlocked())
    }

    pub fn state_ref<T: StateType>(&self) -> Result<StateRef<T>, StateError> {
        self.states.state_ref::<T>()
    }
}
