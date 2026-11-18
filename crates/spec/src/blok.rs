//! Bloks: the UI an app ships with its actions and states.
//!
//! A blok is BSX, one `<Blok kind=... context=...>` element. An app declares it as that string
//! and nothing else: the server parses it, reads the context that draws it off the `<Blok>`, and
//! works out which of the app's states and actions it reads. Nothing here parses or checks BSX;
//! a blok the server refuses fails the registration, with the finding.

#[allow(unused_imports)]
use crate::prelude::*;

use serde_json::{Map, Value};

use crate::actions::Port;

/// The blok language version a source is written in.
pub const VOCAB: u32 = 2;

/// A blok an app declares: its key, its BSX source, and what it says beside it.
///
/// ```ignore
/// let app = App::new("camera", "0.1.0")
///     .state(CameraState::default())
///     .blok("status", include_str!("status.bsx"))
///     .blok_with(
///         Blok::new("inspector", include_str!("inspector.bsx"))
///             .description("One frame, next to its histogram")
///             .arg(<String as PortType>::port("frame")),
///     );
/// ```
///
/// Write the source as a literal (or `include_str!`), not as a string built at run time: a blok
/// is content-addressed, and one that changes with every start is a new blok every start.
#[derive(Debug, Clone, PartialEq)]
pub struct Blok {
    pub key: String,
    pub source: String,
    pub description: Option<String>,
    pub default: bool,
    pub for_model: Option<String>,
    pub for_node: Option<String>,
    pub args: Vec<Port>,
}

impl Blok {
    pub fn new(key: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            source: source.into(),
            description: None,
            default: false,
            for_model: None,
            for_node: None,
            args: vec![],
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Make this the agent's default blok for its context: what a UI opens first.
    pub fn as_default(mut self) -> Self {
        self.default = true;
        self
    }

    /// The model (`sha256:...`) this blok decorates. Panel bloks only.
    pub fn for_model(mut self, reference: impl Into<String>) -> Self {
        self.for_model = Some(reference.into());
        self
    }

    /// The node of the model this blok attaches to; its root otherwise.
    pub fn for_node(mut self, node: impl Into<String>) -> Self {
        self.for_node = Some(node.into());
        self
    }

    /// An item this blok is opened for, read in the source as `args.<key>`. Panel bloks only.
    pub fn arg(mut self, port: Port) -> Self {
        self.args.retain(|p| p.key != port.key);
        self.args.push(port);
        self
    }

    /// The `BlokImplementationInput` sent in REGISTER: the source, and only what was set
    /// beside it. The rest is the server's to read off the source.
    pub fn to_declaration_json(&self) -> Value {
        let mut blok = Map::new();
        blok.insert("key".into(), Value::from(self.key.as_str()));
        blok.insert("source".into(), Value::from(self.source.as_str()));
        blok.insert("vocab".into(), Value::from(VOCAB));
        if let Some(description) = &self.description {
            blok.insert("description".into(), Value::from(description.as_str()));
        }
        if self.default {
            blok.insert("default".into(), Value::Bool(true));
        }
        if let Some(for_model) = &self.for_model {
            blok.insert("for_model".into(), Value::from(for_model.as_str()));
        }
        if let Some(for_node) = &self.for_node {
            blok.insert("for_node".into(), Value::from(for_node.as_str()));
        }
        if !self.args.is_empty() {
            blok.insert(
                "args".into(),
                serde_json::to_value(&self.args).unwrap_or_default(),
            );
        }
        Value::Object(blok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Registry;

    const STATUS: &str =
        r#"<Blok kind="agent" context="panel"><Text>{self.CameraState.frames}</Text></Blok>"#;

    #[test]
    fn a_declared_blok_is_registered_as_its_source() {
        let mut registry = Registry::new();
        registry.blok(Blok::new("status", "<Blok />"));
        registry.blok(
            Blok::new("status", STATUS)
                .description("How many frames")
                .as_default(),
        );
        let declaration = registry.declaration(None, None);
        assert_eq!(
            declaration.bloks,
            vec![serde_json::json!({
                "key": "status",
                "source": STATUS,
                "vocab": 2,
                "description": "How many frames",
                "default": true,
            })]
        );
        // What an app declares is what its hash says: another source, another hash.
        registry.blok(Blok::new("status", "<Blok />"));
        assert_ne!(registry.declaration(None, None).hash, declaration.hash);
    }
}
