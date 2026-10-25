//! Agent state: typed values that every change of is published as JSON patches.
//!
//! A state is a plain struct (`#[derive(State)]`). Actions take a
//! [`StateMut<T>`] to change it or a [`StateRef<T>`] to read it; every change
//! made through [`StateMut::update`] is diffed into RFC 6902 operations and
//! handed to the runtime's [`StatePublisher`], which numbers and publishes
//! them (rekuest: as `STATE_PATCH`).
//!
//! Anyone observing the agent (the rekuest server, or subscribers of a served
//! app) therefore always knows every state. Reading state is not what actions
//! are for: do not write "getter" actions that only return a state.

use crate::collections::HashMap;
#[allow(unused_imports)]
use crate::prelude::*;
use crate::sync::Mutex;
use core::any::Any;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::actions::Port;
use crate::task::TaskGate;

/// A type that can be an agent state. Implemented by `#[derive(State)]`.
pub trait StateType: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {
    /// The state's interface, e.g. `CameraState`.
    const NAME: &'static str;
    /// Locks an action must hold to change this state.
    const REQUIRED_LOCKS: &'static [&'static str];
    /// One (return) port per field.
    fn ports() -> Vec<Port>;
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum StateError {
    #[error("Cannot modify state '{state}' without required locks: {locks:?}")]
    MissingLocks { state: String, locks: Vec<String> },
    #[error("state '{0}' is not declared on this app")]
    Undeclared(String),
    #[error("state '{0}' has no value yet")]
    Uninitialized(String),
    #[error("state '{0}' is read-only here")]
    ReadOnly(String),
    #[error("state '{state}' could not be serialized: {message}")]
    Serialize { state: String, message: String },
    #[error("this task is not running on an agent, so it has no states")]
    NoAgent,
    #[error("the task has ended; its state changes are no longer accepted")]
    TaskClosed,
}

/// Who changes a state: which task (if any) and which locks it holds.
/// `locks: None` means unrestricted (initialization). With a `gate`, changes
/// are refused once the task has ended.
#[derive(Debug, Clone, Default)]
pub struct Mutation {
    pub task_id: Option<String>,
    pub locks: Option<Vec<String>>,
    pub gate: Option<TaskGate>,
}

impl Mutation {
    /// A change made outside a task (background workers, hooks): no locks.
    pub fn unlocked() -> Self {
        Self {
            task_id: None,
            locks: Some(vec![]),
            gate: None,
        }
    }
}

/// A state's declaration, as registered.
#[derive(Clone)]
pub struct StateDeclaration {
    pub name: String,
    pub ports: Vec<Port>,
    pub required_locks: Vec<String>,
    make_cell: Arc<dyn Fn() -> Arc<dyn AnyCell> + Send + Sync>,
    initial: Option<Arc<dyn Fn() -> Box<dyn Any + Send> + Send + Sync>>,
}

impl core::fmt::Debug for StateDeclaration {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StateDeclaration")
            .field("name", &self.name)
            .field("required_locks", &self.required_locks)
            .finish_non_exhaustive()
    }
}

impl StateDeclaration {
    pub fn of<T: StateType>(initial: Option<T>) -> Self {
        Self {
            name: T::NAME.to_owned(),
            ports: T::ports().into_iter().map(Port::into_return).collect(),
            required_locks: T::REQUIRED_LOCKS.iter().map(|s| s.to_string()).collect(),
            make_cell: Arc::new(|| Arc::new(StateCell::<T>::new()) as Arc<dyn AnyCell>),
            initial: initial.map(|value| {
                Arc::new(move || Box::new(value.clone()) as Box<dyn Any + Send>)
                    as Arc<dyn Fn() -> Box<dyn Any + Send> + Send + Sync>
            }),
        }
    }

    /// The `StateImplementationInput` sent in REGISTER.
    pub fn to_declaration_json(&self) -> Value {
        serde_json::json!({
            "interface": self.name,
            "definition": { "ports": self.ports, "name": self.name },
        })
    }
}

/// A change to one state, for the [`StatePublisher`] to number and publish.
#[derive(Debug, Clone, PartialEq)]
pub struct StatePatch {
    pub state: String,
    pub op: String,
    pub path: String,
    pub value: Value,
    pub task_id: Option<String>,
    /// Epoch seconds.
    pub ts: f64,
}

/// Where changes go once a session publishes them (rekuest's state hub).
///
/// Called while the state's value is locked, so the changes of one state
/// arrive in the order they were made; it must not block.
pub trait StatePublisher: Send + Sync + 'static {
    fn publish(&self, patch: StatePatch);
}

/// Type-erased access to a state cell.
trait AnyCell: Send + Sync + 'static {
    fn as_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
    fn set_boxed(&self, value: Box<dyn Any + Send>) -> Result<(), StateError>;
    fn value_json(&self) -> Option<Value>;
    fn bind(&self, publisher: Arc<dyn StatePublisher>);
}

struct StateCell<T> {
    value: Mutex<Option<T>>,
    /// The JSON of `value` as of the last published change.
    json: Mutex<Value>,
    publisher: Mutex<Option<Arc<dyn StatePublisher>>>,
}

impl<T: StateType> StateCell<T> {
    fn new() -> Self {
        Self {
            value: Mutex::new(None),
            json: Mutex::new(Value::Null),
            publisher: Mutex::new(None),
        }
    }

    fn to_json(value: &T) -> Result<Value, StateError> {
        serde_json::to_value(value).map_err(|e| StateError::Serialize {
            state: T::NAME.to_owned(),
            message: e.to_string(),
        })
    }

    /// Replace the value without publishing (initialization).
    fn set(&self, value: T) -> Result<(), StateError> {
        let json = Self::to_json(&value)?;
        *self.value.lock() = Some(value);
        *self.json.lock() = json;
        Ok(())
    }

    fn get(&self) -> Result<T, StateError> {
        self.value
            .lock()
            .clone()
            .ok_or_else(|| StateError::Uninitialized(T::NAME.to_owned()))
    }

    fn read<R>(&self, f: impl FnOnce(&T) -> R) -> Result<R, StateError> {
        let guard = self.value.lock();
        let value = guard
            .as_ref()
            .ok_or_else(|| StateError::Uninitialized(T::NAME.to_owned()))?;
        Ok(f(value))
    }

    fn update<R>(&self, mutation: &Mutation, f: impl FnOnce(&mut T) -> R) -> Result<R, StateError> {
        // Checked before anything changes, and held until the change is
        // published: a task's end is never followed by one of its changes.
        let _pass = match &mutation.gate {
            Some(gate) => Some(gate.enter().ok_or(StateError::TaskClosed)?),
            None => None,
        };
        if let Some(held) = &mutation.locks {
            let missing: Vec<String> = T::REQUIRED_LOCKS
                .iter()
                .filter(|l| !held.iter().any(|h| h == *l))
                .map(|l| l.to_string())
                .collect();
            if !missing.is_empty() {
                return Err(StateError::MissingLocks {
                    state: T::NAME.to_owned(),
                    locks: T::REQUIRED_LOCKS.iter().map(|s| s.to_string()).collect(),
                });
            }
        }

        // The value lock is held until the operations are handed on, so
        // changes to one state are published in the order they were made.
        let mut guard = self.value.lock();
        let value = guard
            .as_mut()
            .ok_or_else(|| StateError::Uninitialized(T::NAME.to_owned()))?;
        let result = f(value);
        let new_json = Self::to_json(value)?;

        let mut json = self.json.lock();
        let ops = crate::patch::diff_ops(&json, &new_json);
        *json = new_json;
        drop(json);

        if !ops.is_empty() {
            if let Some(publisher) = self.publisher.lock().as_ref() {
                let ts = now_ts();
                for (op, path, value) in ops {
                    publisher.publish(StatePatch {
                        state: T::NAME.to_owned(),
                        op,
                        path,
                        value,
                        task_id: mutation.task_id.clone(),
                        ts,
                    });
                }
            }
        }
        drop(guard);
        Ok(result)
    }
}

impl<T: StateType> AnyCell for StateCell<T> {
    fn as_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn set_boxed(&self, value: Box<dyn Any + Send>) -> Result<(), StateError> {
        let value = value
            .downcast::<T>()
            .map_err(|_| StateError::Undeclared(T::NAME.to_owned()))?;
        self.set(*value)
    }

    fn value_json(&self) -> Option<Value> {
        self.value
            .lock()
            .is_some()
            .then(|| self.json.lock().clone())
    }

    fn bind(&self, publisher: Arc<dyn StatePublisher>) {
        *self.publisher.lock() = Some(publisher);
    }
}

fn now_ts() -> f64 {
    let now = crate::rt::now();
    now.timestamp() as f64 + f64::from(now.timestamp_subsec_micros()) / 1e6
}

/// Every declared state's value, typed.
pub struct StateCells {
    declarations: Vec<StateDeclaration>,
    cells: HashMap<String, Arc<dyn AnyCell>>,
}

impl core::fmt::Debug for StateCells {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StateCells")
            .field(
                "states",
                &self
                    .declarations
                    .iter()
                    .map(|d| &d.name)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl StateCells {
    pub fn new(declarations: Vec<StateDeclaration>) -> Self {
        let cells = declarations
            .iter()
            .map(|d| (d.name.clone(), (d.make_cell)()))
            .collect();
        Self {
            declarations,
            cells,
        }
    }

    pub fn declarations(&self) -> &[StateDeclaration] {
        &self.declarations
    }

    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }

    fn cell<T: StateType>(&self) -> Result<Arc<StateCell<T>>, StateError> {
        let cell = self
            .cells
            .get(T::NAME)
            .ok_or_else(|| StateError::Undeclared(T::NAME.to_owned()))?;
        cell.clone()
            .as_any()
            .downcast::<StateCell<T>>()
            .map_err(|_| StateError::Undeclared(T::NAME.to_owned()))
    }

    /// Set a state's value without publishing (startup).
    pub fn init<T: StateType>(&self, value: T) -> Result<(), StateError> {
        self.cell::<T>()?.set(value)
    }

    pub fn state_mut<T: StateType>(&self, mutation: Mutation) -> Result<StateMut<T>, StateError> {
        Ok(StateMut {
            cell: self.cell::<T>()?,
            mutation,
        })
    }

    pub fn state_ref<T: StateType>(&self) -> Result<StateRef<T>, StateError> {
        Ok(StateRef {
            cell: self.cell::<T>()?,
        })
    }

    /// Apply the declared initial values.
    pub fn apply_initial_values(&self) -> Result<(), StateError> {
        for declaration in &self.declarations {
            if let Some(initial) = &declaration.initial {
                self.cells[&declaration.name].set_boxed(initial())?;
            }
        }
        Ok(())
    }

    /// The states that have no value yet.
    pub fn uninitialized(&self) -> Vec<&str> {
        self.declarations
            .iter()
            .filter(|d| self.cells[&d.name].value_json().is_none())
            .map(|d| d.name.as_str())
            .collect()
    }

    /// Every state's value as JSON, in declaration order (`null` without one).
    pub fn values(&self) -> Map<String, Value> {
        self.declarations
            .iter()
            .map(|d| {
                (
                    d.name.clone(),
                    self.cells[&d.name].value_json().unwrap_or(Value::Null),
                )
            })
            .collect()
    }

    /// From now on, hand every change to `publisher`.
    pub fn bind(&self, publisher: Arc<dyn StatePublisher>) {
        for cell in self.cells.values() {
            cell.bind(publisher.clone());
        }
    }
}

/// A handle that changes a state. Take it as an action parameter:
/// `camera: StateMut<CameraState>`.
pub struct StateMut<T: StateType> {
    cell: Arc<StateCell<T>>,
    mutation: Mutation,
}

impl<T: StateType> Clone for StateMut<T> {
    fn clone(&self) -> Self {
        Self {
            cell: self.cell.clone(),
            mutation: self.mutation.clone(),
        }
    }
}

impl<T: StateType> core::fmt::Debug for StateMut<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StateMut").field("state", &T::NAME).finish()
    }
}

impl<T: StateType> StateMut<T> {
    /// A copy of the current value.
    pub fn get(&self) -> T {
        self.cell
            .get()
            .expect("states are initialized before actions run")
    }

    /// Read without copying.
    pub fn read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.cell
            .read(f)
            .expect("states are initialized before actions run")
    }

    /// Change the state. The difference is published as `STATE_PATCH`es,
    /// attributed to the task this handle belongs to.
    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> R) -> Result<R, StateError> {
        self.cell.update(&self.mutation, f)
    }

    /// Replace the whole value.
    pub fn set(&self, value: T) -> Result<(), StateError> {
        self.update(|current| *current = value)
    }
}

/// A read-only handle: `camera: StateRef<CameraState>`.
pub struct StateRef<T: StateType> {
    cell: Arc<StateCell<T>>,
}

impl<T: StateType> Clone for StateRef<T> {
    fn clone(&self) -> Self {
        Self {
            cell: self.cell.clone(),
        }
    }
}

impl<T: StateType> core::fmt::Debug for StateRef<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StateRef").field("state", &T::NAME).finish()
    }
}

impl<T: StateType> StateRef<T> {
    pub fn get(&self) -> T {
        self.cell
            .get()
            .expect("states are initialized before actions run")
    }

    pub fn read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.cell
            .read(f)
            .expect("states are initialized before actions run")
    }
}
