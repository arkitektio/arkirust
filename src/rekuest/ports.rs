use super::api::create_template::{
    AssignWidgetInput, ChildPortInput, EffectInput, PortInput, PortKind, PortScope,
    ReturnWidgetInput, ValidatorInput,
};

/// Builder for an INT port.
/// Required: `key`, `scope`.
/// Optional fields can be set after `new()`.
pub struct IntPortBuilder {
    key: String,
    scope: PortScope,
    default: Option<String>,
    description: Option<String>,
    groups: Option<Vec<String>>,
    effects: Option<Vec<EffectInput>>,
    label: Option<String>,
    assign_widget: Option<Box<Option<AssignWidgetInput>>>,
    identifier: Option<String>,
    nullable: bool,
    return_widget: Option<ReturnWidgetInput>,
    validators: Option<Vec<ValidatorInput>>,
}

impl IntPortBuilder {
    pub fn new(key: &str) -> Self {
        IntPortBuilder {
            key: key.to_string(),
            scope: PortScope::GLOBAL,
            default: None,
            description: None,
            groups: None,
            effects: None,
            label: None,
            assign_widget: None,
            identifier: None,
            nullable: false,
            return_widget: None,
            validators: None,
        }
    }

    pub fn default(mut self, default: &str) -> Self {
        self.default = Some(default.to_string());
        self
    }

    pub fn description(mut self, description: &str) -> Self {
        self.description = Some(description.to_string());
        self
    }

    pub fn groups(mut self, groups: Vec<&str>) -> Self {
        self.groups = Some(groups.into_iter().map(|g| g.to_string()).collect());
        self
    }

    pub fn effects(mut self, effects: Vec<EffectInput>) -> Self {
        self.effects = Some(effects);
        self
    }

    pub fn label(mut self, label: &str) -> Self {
        self.label = Some(label.to_string());
        self
    }

    pub fn assign_widget(mut self, widget: Option<AssignWidgetInput>) -> Self {
        self.assign_widget = Some(Box::new(widget));
        self
    }

    pub fn identifier(mut self, identifier: &str) -> Self {
        self.identifier = Some(identifier.to_string());
        self
    }

    pub fn nullable(mut self, nullable: bool) -> Self {
        self.nullable = nullable;
        self
    }

    pub fn return_widget(mut self, widget: ReturnWidgetInput) -> Self {
        self.return_widget = Some(widget);
        self
    }

    pub fn validators(mut self, validators: Vec<ValidatorInput>) -> Self {
        self.validators = Some(validators);
        self
    }

    pub fn build(self) -> PortInput {
        PortInput {
            key: self.key,
            default: self.default,
            scope: self.scope,
            kind: PortKind::INT,
            children: Some(vec![]),
            description: self.description,
            groups: self.groups,
            effects: self.effects,
            label: self.label,
            assign_widget: self.assign_widget.unwrap_or_else(|| Box::new(None)),
            identifier: self.identifier,
            nullable: self.nullable,
            return_widget: self.return_widget,
            validators: self.validators,
        }
    }
}

/// Builder for a STRING port.
/// `scope` is always `GLOBAL`, so we do not require it in the constructor.
/// Required: `key`.
pub struct StringPortBuilder {
    key: String,
    scope: PortScope,
    default: Option<String>,
    description: Option<String>,
    groups: Option<Vec<String>>,
    effects: Option<Vec<EffectInput>>,
    label: Option<String>,
    assign_widget: Option<Box<Option<AssignWidgetInput>>>,
    identifier: Option<String>,
    nullable: bool,
    return_widget: Option<ReturnWidgetInput>,
    validators: Option<Vec<ValidatorInput>>,
}

impl StringPortBuilder {
    pub fn new(key: &str) -> Self {
        StringPortBuilder {
            key: key.to_string(),
            scope: PortScope::GLOBAL,
            default: None,
            description: None,
            groups: None,
            effects: None,
            label: None,
            assign_widget: None,
            identifier: None,
            nullable: false,
            return_widget: None,
            validators: None,
        }
    }

    pub fn default(mut self, default: &str) -> Self {
        self.default = Some(default.to_string());
        self
    }

    pub fn description(mut self, description: &str) -> Self {
        self.description = Some(description.to_string());
        self
    }

    pub fn groups(mut self, groups: Vec<&str>) -> Self {
        self.groups = Some(groups.into_iter().map(|g| g.to_string()).collect());
        self
    }

    pub fn effects(mut self, effects: Vec<EffectInput>) -> Self {
        self.effects = Some(effects);
        self
    }

    pub fn label(mut self, label: &str) -> Self {
        self.label = Some(label.to_string());
        self
    }

    pub fn assign_widget(mut self, widget: Option<AssignWidgetInput>) -> Self {
        self.assign_widget = Some(Box::new(widget));
        self
    }

    pub fn identifier(mut self, identifier: &str) -> Self {
        self.identifier = Some(identifier.to_string());
        self
    }

    pub fn nullable(mut self, nullable: bool) -> Self {
        self.nullable = nullable;
        self
    }

    pub fn return_widget(mut self, widget: ReturnWidgetInput) -> Self {
        self.return_widget = Some(widget);
        self
    }

    pub fn validators(mut self, validators: Vec<ValidatorInput>) -> Self {
        self.validators = Some(validators);
        self
    }

    pub fn build(self) -> PortInput {
        PortInput {
            key: self.key,
            default: self.default,
            scope: PortScope::GLOBAL, // Always global
            kind: PortKind::STRING,
            children: Some(vec![]),
            description: self.description,
            groups: self.groups,
            effects: self.effects,
            label: self.label,
            assign_widget: self.assign_widget.unwrap_or_else(|| Box::new(None)),
            identifier: self.identifier,
            nullable: self.nullable,
            return_widget: self.return_widget,
            validators: self.validators,
        }
    }
}

/// Builder for a LIST port.
/// Required: `key`, `scope`.
pub struct ListPortBuilder {
    key: String,
    scope: PortScope,
    default: Option<String>,
    description: Option<String>,
    groups: Option<Vec<String>>,
    effects: Option<Vec<EffectInput>>,
    label: Option<String>,
    assign_widget: Option<Box<Option<AssignWidgetInput>>>,
    identifier: Option<String>,
    nullable: bool,
    return_widget: Option<ReturnWidgetInput>,
    validators: Option<Vec<ValidatorInput>>,
    children: Option<Vec<ChildPortInput>>,
}

impl ListPortBuilder {
    pub fn new(key: &str, child: ChildPortInput) -> Self {
        ListPortBuilder {
            key: key.to_string(),
            scope: PortScope::GLOBAL,
            default: None,
            description: None,
            groups: None,
            effects: None,
            label: None,
            assign_widget: None,
            identifier: None,
            nullable: false,
            return_widget: None,
            validators: None,
            children: Some(vec![child]),
        }
    }

    pub fn default(mut self, default: &str) -> Self {
        self.default = Some(default.to_string());
        self
    }

    pub fn description(mut self, description: &str) -> Self {
        self.description = Some(description.to_string());
        self
    }

    pub fn groups(mut self, groups: Vec<&str>) -> Self {
        self.groups = Some(groups.into_iter().map(|g| g.to_string()).collect());
        self
    }

    pub fn effects(mut self, effects: Vec<EffectInput>) -> Self {
        self.effects = Some(effects);
        self
    }

    pub fn label(mut self, label: &str) -> Self {
        self.label = Some(label.to_string());
        self
    }

    pub fn assign_widget(mut self, widget: Option<AssignWidgetInput>) -> Self {
        self.assign_widget = Some(Box::new(widget));
        self
    }

    pub fn identifier(mut self, identifier: &str) -> Self {
        self.identifier = Some(identifier.to_string());
        self
    }

    pub fn nullable(mut self, nullable: bool) -> Self {
        self.nullable = nullable;
        self
    }

    pub fn return_widget(mut self, widget: ReturnWidgetInput) -> Self {
        self.return_widget = Some(widget);
        self
    }

    pub fn validators(mut self, validators: Vec<ValidatorInput>) -> Self {
        self.validators = Some(validators);
        self
    }

    pub fn build(self) -> PortInput {
        PortInput {
            key: self.key,
            default: self.default,
            scope: self.scope,
            kind: PortKind::LIST,
            children: self.children,
            description: self.description,
            groups: self.groups,
            effects: self.effects,
            label: self.label,
            assign_widget: self.assign_widget.unwrap_or_else(|| Box::new(None)),
            identifier: self.identifier,
            nullable: self.nullable,
            return_widget: self.return_widget,
            validators: self.validators,
        }
    }
}

pub struct Port {}

impl Port {
    pub fn new_int(key: &str) -> IntPortBuilder {
        IntPortBuilder::new(key)
    }

    pub fn new_string(key: &str) -> StringPortBuilder {
        StringPortBuilder::new(key)
    }

    pub fn new_list(key: &str, child: ChildPortInput) -> ListPortBuilder {
        ListPortBuilder::new(key, child)
    }
}
