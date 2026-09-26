//! The client lookup handed to actions and structure expanders.
//!
//! This is the Rust twin of Python's "clients are injected by annotation":
//! every service client is stored by its type, and actions or structures ask
//! for the type they need.

use std::any::{type_name, Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
#[error("no client of type {0} is available; did you add its service to the App?")]
pub struct MissingClient(pub &'static str);

/// Builder for a [`Context`].
#[derive(Default)]
pub struct ContextBuilder {
    clients: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl ContextBuilder {
    pub fn insert<T: Clone + Send + Sync + 'static>(&mut self, client: T) -> &mut Self {
        self.clients.insert(TypeId::of::<T>(), Arc::new(client));
        self
    }

    pub fn build(self) -> Context {
        Context {
            clients: Arc::new(self.clients),
        }
    }
}

/// Typed client lookup. Cheap to clone.
#[derive(Clone, Default)]
pub struct Context {
    clients: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("clients", &self.clients.len())
            .finish()
    }
}

impl Context {
    pub fn builder() -> ContextBuilder {
        ContextBuilder::default()
    }

    /// The client of type `T`, if one was registered.
    pub fn get<T: Clone + Send + Sync + 'static>(&self) -> Option<T> {
        self.clients
            .get(&TypeId::of::<T>())
            .and_then(|c| c.downcast_ref::<T>())
            .cloned()
    }

    /// The client of type `T`, or an error naming the missing type.
    pub fn require<T: Clone + Send + Sync + 'static>(&self) -> Result<T, MissingClient> {
        self.get::<T>().ok_or(MissingClient(type_name::<T>()))
    }
}
