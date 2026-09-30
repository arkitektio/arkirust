//! Ports describe the inputs and outputs of an action.
//!
//! The same [`Port`] type serves argument and return ports: argument-only
//! fields (`default`, assign widgets) are simply never set on return ports and
//! are skipped when serializing.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PortKind {
    Int,
    String,
    Structure,
    List,
    Bool,
    Dict,
    Float,
    Date,
    Union,
    Enum,
    Model,
    MemoryStructure,
    Interface,
    Quantity,
}

/// A choice of an ENUM-like port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    pub value: Value,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Port {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub kind: PortKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    #[serde(default)]
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Port>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<Choice>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget: Option<Value>,
}

impl Port {
    pub fn new(key: impl Into<String>, kind: PortKind) -> Self {
        Self {
            key: key.into(),
            label: None,
            kind,
            description: None,
            identifier: None,
            nullable: false,
            children: vec![],
            choices: None,
            default: None,
            widget: None,
        }
    }

    pub fn identifier(mut self, identifier: impl Into<String>) -> Self {
        self.identifier = Some(identifier.into());
        self
    }

    pub fn nullable(mut self, nullable: bool) -> Self {
        self.nullable = nullable;
        self
    }

    pub fn child(mut self, child: Port) -> Self {
        self.children.push(child);
        self
    }

    pub fn widget(mut self, widget: Option<Value>) -> Self {
        self.widget = widget;
        self
    }

    /// Set a description. Like the Python library, a documented port is
    /// labelled with its key unless it already has a label.
    pub fn describe(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        if self.label.is_none() {
            self.label = Some(self.key.clone());
        }
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// A default makes the port optional for the caller.
    pub fn default_value(mut self, default: Value) -> Self {
        self.default = Some(default);
        self.nullable = true;
        self
    }

    /// Strip argument-only fields so the port is valid as a return port.
    pub fn into_return(mut self) -> Self {
        self.default = None;
        self.widget = None;
        self.children = self.children.into_iter().map(Port::into_return).collect();
        self
    }
}

/// Helpers for building assign widgets.
pub mod widgets {
    use serde_json::{json, Value};

    /// A search widget: the UI runs `query` against the service `ward` to
    /// offer values. The query must return `options { value label }`.
    pub fn search(query: &str, ward: &str) -> Value {
        json!({ "kind": "SEARCH", "query": query, "ward": ward })
    }

    pub fn slider(min: f64, max: f64, step: Option<f64>) -> Value {
        json!({ "kind": "SLIDER", "min": min, "max": max, "step": step })
    }

    pub fn string(placeholder: &str, as_paragraph: bool) -> Value {
        json!({ "kind": "STRING", "placeholder": placeholder, "as_paragraph": as_paragraph })
    }
}
