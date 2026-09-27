//! What an agent declares when it registers: definitions and implementations.
//!
//! These are serialized in snake_case, which is what the agent socket expects
//! (the camelCase spelling belongs to the GraphQL API only).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::ports::Port;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActionKind {
    /// Returns exactly once.
    Function,
    /// Yields any number of times.
    Generator,
}

/// The public contract of an action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Definition {
    pub key: String,
    pub version: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub collections: Vec<String>,
    #[serde(default)]
    pub stateful: bool,
    #[serde(default)]
    pub port_groups: Vec<Value>,
    #[serde(default)]
    pub args: Vec<Port>,
    #[serde(default)]
    pub returns: Vec<Port>,
    pub kind: ActionKind,
    #[serde(default)]
    pub is_test_for: Vec<Value>,
    #[serde(default)]
    pub is_dev: bool,
}

impl Definition {
    pub fn new(key: impl Into<String>, name: impl Into<String>, kind: ActionKind) -> Self {
        Self {
            key: key.into(),
            version: "1".into(),
            name: name.into(),
            description: None,
            collections: vec![],
            stateful: false,
            port_groups: vec![],
            args: vec![],
            returns: vec![],
            kind,
            is_test_for: vec![],
            is_dev: false,
        }
    }
}

/// A definition bound to the interface this agent serves it under.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Implementation {
    pub definition: Definition,
    pub interface: String,
    #[serde(default)]
    pub dependencies: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(default = "yes")]
    pub needs_token: bool,
    /// Locks held while an assignment runs.
    #[serde(default)]
    pub locks: Vec<String>,
    /// States an assignment may change.
    #[serde(default)]
    pub manipulates: Vec<String>,
}

fn yes() -> bool {
    true
}

impl Implementation {
    pub fn new(interface: impl Into<String>, definition: Definition) -> Self {
        Self {
            definition,
            interface: interface.into(),
            dependencies: vec![],
            params: None,
            needs_token: true,
            locks: vec![],
            manipulates: vec![],
        }
    }
}

/// Everything an agent offers, as sent in `REGISTER`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentDeclaration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub implementations: Vec<Implementation>,
    #[serde(default)]
    pub states: Vec<Value>,
    #[serde(default)]
    pub locks: Vec<Value>,
    #[serde(default)]
    pub bloks: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

impl AgentDeclaration {
    pub fn new(
        name: Option<String>,
        description: Option<String>,
        implementations: Vec<Implementation>,
    ) -> Self {
        let mut declaration = Self {
            name,
            description,
            implementations,
            states: vec![],
            locks: vec![],
            bloks: vec![],
            hash: None,
        };
        declaration.hash = Some(declaration.compute_hash());
        declaration
    }

    /// sha256 over the canonical JSON of the declaration (without the hash).
    /// The backend only stores it and skips reconciliation when it matches.
    pub fn compute_hash(&self) -> String {
        let mut without = self.clone();
        without.hash = None;
        let value = serde_json::to_value(&without).expect("declaration serializes");
        let canonical = canonical_json(&value);
        hex::encode(Sha256::digest(canonical.as_bytes()))
    }
}

/// JSON with object keys sorted, recursively.
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical_json(&map[k])))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => other.to_string(),
    }
}
