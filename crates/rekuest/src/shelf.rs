//! Values an agent keeps in memory and hands out by reference.
//!
//! A [`Memory<T>`] return value is put on the agent's [`Shelf`] under an id
//! the agent mints itself, and travels as `{"__identifier", "object": id}`.
//! Shelving never waits: the id is usable at once, and the `SHELVE` message
//! that tells the server about the drawer is recorded in the journal, so it
//! reaches the server before anything that references it. The server's
//! `COLLECT` (or [`Shelf::remove`]) drops a value and records `UNSHELVE`.

use std::any::Any;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::context::Context;
use crate::emit::Emitter;
use crate::messages::FromAgent;
use crate::port_type::{unwrap_reference, PortError, PortType};
use crate::ports::{Port, PortKind};

type Stored = Arc<dyn Any + Send + Sync>;

struct Inner {
    values: Mutex<HashMap<String, Stored>>,
    emitter: Arc<dyn Emitter>,
}

/// The agent's in-memory values. Cheap to clone; every [`Context`] of an
/// executor holds it.
#[derive(Clone)]
pub struct Shelf {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Shelf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shelf")
            .field("values", &self.len())
            .finish()
    }
}

impl Shelf {
    pub fn new(emitter: Arc<dyn Emitter>) -> Self {
        Self {
            inner: Arc::new(Inner {
                values: Mutex::default(),
                emitter,
            }),
        }
    }

    /// Keep `value` and return the id it is referenced by. Records `SHELVE`.
    pub fn put<T: Send + Sync + 'static>(
        &self,
        identifier: &str,
        value: Arc<T>,
        label: Option<String>,
    ) -> String {
        let id = uuid::Uuid::new_v4().simple().to_string();
        self.inner
            .values
            .lock()
            .expect("shelf lock")
            .insert(id.clone(), value as Stored);
        self.inner.emitter.emit(
            FromAgent::Shelve {
                reference: id.clone(),
                identifier: identifier.to_owned(),
                resource_id: id.clone(),
                label,
                description: None,
                task: None,
            },
            None,
        );
        id
    }

    /// The value shelved under `id`, if it is a `T`.
    pub fn get<T: Send + Sync + 'static>(&self, id: &str) -> Option<Arc<T>> {
        let value = self
            .inner
            .values
            .lock()
            .expect("shelf lock")
            .get(id)
            .cloned()?;
        value.downcast::<T>().ok()
    }

    /// Drop a value. Records `UNSHELVE` if there was one.
    pub fn remove(&self, id: &str) -> bool {
        let removed = self
            .inner
            .values
            .lock()
            .expect("shelf lock")
            .remove(id)
            .is_some();
        if removed {
            self.inner.emitter.emit(
                FromAgent::Unshelve {
                    reference: id.to_owned(),
                    drawer: id.to_owned(),
                },
                None,
            );
        } else {
            tracing::debug!("asked to drop {id}, which is not on the shelf");
        }
        removed
    }

    pub fn len(&self) -> usize {
        self.inner.values.lock().expect("shelf lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A type that stays in the agent's memory and travels by reference.
pub trait MemoryStructure: Send + Sync + 'static {
    /// `@package/key`.
    const IDENTIFIER: &'static str;

    /// Shown in the UI for the drawer.
    fn label(&self) -> Option<String> {
        None
    }
}

/// A value kept on the agent's [`Shelf`]; as a port, a memory structure.
pub struct Memory<T>(pub Arc<T>);

impl<T> Memory<T> {
    pub fn new(value: T) -> Self {
        Self(Arc::new(value))
    }
}

impl<T> Clone for Memory<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Deref for Memory<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Memory<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Memory").field(&self.0).finish()
    }
}

fn shelf_of(ctx: &Context) -> Result<Shelf, PortError> {
    ctx.get::<Shelf>()
        .ok_or_else(|| PortError::new("memory structures need an agent (there is no shelf)"))
}

impl<T: MemoryStructure> PortType for Memory<T> {
    fn port(key: &str) -> Port {
        Port::new(key, PortKind::MemoryStructure).identifier(T::IDENTIFIER)
    }

    async fn expand(value: Value, ctx: &Context) -> Result<Self, PortError> {
        let id = unwrap_reference(value, T::IDENTIFIER)?;
        shelf_of(ctx)?
            .get::<T>(&id)
            .map(Memory)
            .ok_or_else(|| PortError::new(format!("nothing is shelved as {} {id}", T::IDENTIFIER)))
    }

    async fn shrink(self, ctx: &Context) -> Result<Value, PortError> {
        let label = self.0.label();
        let id = shelf_of(ctx)?.put(T::IDENTIFIER, self.0, label);
        Ok(json!({ "__identifier": T::IDENTIFIER, "object": id }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Journal;
    use std::sync::Mutex as StdMutex;

    #[derive(Debug, PartialEq)]
    struct Frame(Vec<u8>);
    impl MemoryStructure for Frame {
        const IDENTIFIER: &'static str = "@test/frame";
    }

    #[derive(Default)]
    struct Recorder(StdMutex<Vec<FromAgent>>);
    impl Emitter for Recorder {
        fn emit(&self, message: FromAgent, _: Option<&str>) {
            self.0.lock().unwrap().push(message);
        }
    }

    #[tokio::test]
    async fn shelves_locally_and_records_in_order() {
        let recorder = Arc::new(Recorder::default());
        let journal = Arc::new(Journal::new(recorder.clone(), None, Some("s".into())));
        let shelf = Shelf::new(journal.clone());
        let ctx = {
            let mut builder = Context::builder();
            builder.insert(shelf.clone());
            builder.build()
        };

        let reference = Memory::new(Frame(vec![1, 2])).shrink(&ctx).await.unwrap();
        let id = reference["object"].as_str().unwrap().to_owned();
        assert_eq!(id.len(), 32, "a uuid the agent minted");
        let back = Memory::<Frame>::expand(reference.clone(), &ctx)
            .await
            .unwrap();
        assert_eq!(*back, Frame(vec![1, 2]));

        assert!(shelf.remove(&id));
        assert!(Memory::<Frame>::expand(reference, &ctx).await.is_err());
        let messages = recorder.0.lock().unwrap();
        assert!(
            matches!(&messages[0], FromAgent::Shelve { resource_id, reference, .. } if *resource_id == id && *reference == id)
        );
        assert!(matches!(&messages[1], FromAgent::Unshelve { drawer, .. } if *drawer == id));
        assert_eq!(journal.watermark().unwrap().pos, 2);
    }
}
