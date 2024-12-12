use super::api::create_template::{DefinitionInput, NodeKind, PortGroupInput, PortInput};

pub struct Definition {
    description: Option<String>,
    name: String,
    args: Option<Vec<PortInput>>,
    kind: NodeKind,
    port_groups: Option<Vec<PortGroupInput>>,
    stateful: Option<bool>,
    is_dev: Option<bool>,
    is_test_for: Option<Vec<String>>,
    interfaces: Option<Vec<String>>,
    returns: Option<Vec<PortInput>>,
    collections: Option<Vec<String>>,
}

impl Definition {
    /// Create a new `DefinitionInputBuilder` with required parameters: `name` and `kind`.
    pub fn new(name: &str, kind: NodeKind) -> Self {
        Self {
            description: None,
            name: name.to_string(),
            args: None,
            kind: kind,
            port_groups: None,
            stateful: None,
            is_dev: None,
            is_test_for: None,
            interfaces: None,
            returns: None,
            collections: None,
        }
    }

    /// Set the description of the definition.
    pub fn description(mut self, description: &str) -> Self {
        self.description = Some(description.to_string());
        self
    }

    /// Set the arguments (ports) that this definition takes.
    pub fn args(mut self, args: Vec<PortInput>) -> Self {
        self.args = Some(args);
        self
    }

    /// Set the port_groups for this definition.
    pub fn port_groups(mut self, port_groups: Vec<PortGroupInput>) -> Self {
        self.port_groups = Some(port_groups.into_iter().collect());
        self
    }

    /// Specify whether this definition is stateful.
    pub fn stateful(mut self, stateful: bool) -> Self {
        self.stateful = Some(stateful);
        self
    }

    /// Specify whether this definition is a dev definition.
    pub fn is_dev(mut self, is_dev: bool) -> Self {
        self.is_dev = Some(is_dev);
        self
    }

    /// Set the `is_test_for` field.
    pub fn is_test_for(mut self, is_test_for: Vec<&str>) -> Self {
        self.is_test_for = Some(is_test_for.into_iter().map(|s| s.to_string()).collect());
        self
    }

    /// Set the interfaces for this definition.
    pub fn interfaces(mut self, interfaces: Vec<&str>) -> Self {
        self.interfaces = Some(interfaces.into_iter().map(|s| s.to_string()).collect());
        self
    }

    /// Set the return ports of this definition.
    pub fn returns(mut self, returns: Vec<PortInput>) -> Self {
        self.returns = Some(returns);
        self
    }

    /// Set the collections field of this definition.
    pub fn collections(mut self, collections: Vec<&str>) -> Self {
        self.collections = Some(collections.into_iter().map(|s| s.to_string()).collect());
        self
    }

    /// Build the `DefinitionInput` struct, ensuring all required fields are present.
    pub fn build(self) -> DefinitionInput {
        DefinitionInput {
            description: self.description,
            name: self.name,
            args: self.args.unwrap_or_else(|| vec![]),
            kind: self.kind,
            port_groups: self.port_groups.unwrap_or_else(|| vec![]),
            stateful: self.stateful.unwrap_or(false),
            is_dev: self.is_dev.unwrap_or(false),
            is_test_for: self.is_test_for.unwrap_or_else(|| vec![]),
            interfaces: self.interfaces.unwrap_or_else(|| vec![]),
            returns: self.returns.unwrap_or_else(|| vec![]),
            collections: self.collections.unwrap_or_else(|| vec![]),
        }
    }
}
