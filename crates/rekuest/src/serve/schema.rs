//! JSON views of the declaration as the Python routes render them
//! (GraphQL spelling, every field present), and the OpenAPI helpers.

use serde_json::{json, Map, Value};

use crate::definition::{ActionKind, Definition, Implementation};
use crate::ports::{Port, PortKind};
use crate::state::StateDeclaration;

fn kind_name(kind: PortKind) -> Value {
    serde_json::to_value(kind).unwrap_or(Value::Null)
}

/// A port in the API spelling: argument ports carry `validators`/`default`/`requires`,
/// return ports `provides`.
pub(crate) fn api_port(port: &Port, is_arg: bool) -> Value {
    let mut p = Map::new();
    p.insert("key".into(), json!(port.key));
    p.insert("label".into(), json!(port.label));
    p.insert("kind".into(), kind_name(port.kind));
    p.insert("description".into(), json!(port.description));
    p.insert("identifier".into(), json!(port.identifier));
    p.insert("nullable".into(), json!(port.nullable));
    p.insert("effects".into(), json!([]));
    p.insert("choices".into(), json!(port.choices));
    p.insert("referenceUnit".into(), Value::Null);
    p.insert("proposedUnits".into(), Value::Null);
    p.insert("dimension".into(), Value::Null);
    p.insert(
        "children".into(),
        if port.children.is_empty() {
            Value::Null
        } else {
            Value::Array(port.children.iter().map(|c| api_port(c, is_arg)).collect())
        },
    );
    if is_arg {
        p.insert("validators".into(), json!([]));
        p.insert("default".into(), json!(port.default));
        p.insert("widget".into(), json!(port.widget));
        p.insert("requires".into(), Value::Null);
    } else {
        p.insert("widget".into(), Value::Null);
        p.insert("provides".into(), Value::Null);
    }
    Value::Object(p)
}

fn api_definition(d: &Definition) -> Value {
    json!({
        "description": d.description,
        "collections": d.collections,
        "key": d.key,
        "version": d.version,
        "name": d.name,
        "stateful": d.stateful,
        "pure": null,
        "idempotent": null,
        "allowProbe": null,
        "catalogs": null,
        "portGroups": d.port_groups,
        "args": d.args.iter().map(|p| api_port(p, true)).collect::<Vec<_>>(),
        "returns": d.returns.iter().map(|p| api_port(p, false)).collect::<Vec<_>>(),
        "kind": match d.kind { ActionKind::Function => "FUNCTION", ActionKind::Generator => "GENERATOR" },
        "isTestFor": d.is_test_for,
        "isDev": d.is_dev,
    })
}

pub(crate) fn api_implementation(i: &Implementation) -> Value {
    json!({
        "definition": api_definition(&i.definition),
        "dependencies": i.dependencies,
        "tracks": [],
        "interface": i.interface,
        "params": i.params,
        "instanceId": null,
        "locks": i.locks,
        "optimistics": [],
        "manipulates": i.manipulates,
        "needsToken": i.needs_token,
        "provenanceAudience": null,
        "effect": null,
    })
}

pub(crate) fn api_state(s: &StateDeclaration) -> Value {
    json!({
        "interface": s.name,
        "key": null,
        "app": null,
        "definition": {
            "ports": s.ports.iter().map(|p| api_port(p, false)).collect::<Vec<_>>(),
            "name": s.name,
        },
    })
}

pub(crate) fn api_lock(key: &str) -> Value {
    json!({ "key": key, "definition": { "key": key, "description": format!("Lock definition for {key}") } })
}

/// JSON schema of one port (`openapi_utils.port_to_json_schema`).
pub(crate) fn port_to_json_schema(port: &Port) -> Value {
    let mut schema = Map::new();
    if let Some(label) = port.label.as_ref().filter(|l| !l.is_empty()) {
        schema.insert("title".into(), json!(label));
    }
    if let Some(description) = port.description.as_ref().filter(|d| !d.is_empty()) {
        schema.insert("description".into(), json!(description));
    }
    match port.kind {
        PortKind::Int => {
            schema.insert("type".into(), json!("integer"));
        }
        PortKind::String => {
            schema.insert("type".into(), json!("string"));
        }
        PortKind::Bool => {
            schema.insert("type".into(), json!("boolean"));
        }
        PortKind::Float => {
            schema.insert("type".into(), json!("number"));
        }
        PortKind::List => {
            schema.insert("type".into(), json!("array"));
            schema.insert(
                "items".into(),
                port.children.first().map(port_to_json_schema).unwrap_or_else(|| json!({})),
            );
        }
        PortKind::Dict | PortKind::Structure => {
            schema.insert("type".into(), json!("object"));
            if port.children.is_empty() {
                schema.insert("additionalProperties".into(), json!(true));
            } else {
                let properties: Map<String, Value> = port
                    .children
                    .iter()
                    .map(|c| (c.key.clone(), port_to_json_schema(c)))
                    .collect();
                schema.insert("properties".into(), Value::Object(properties));
                let required: Vec<&str> = port
                    .children
                    .iter()
                    .filter(|c| !c.nullable && c.default.is_none())
                    .map(|c| c.key.as_str())
                    .collect();
                if !required.is_empty() {
                    schema.insert("required".into(), json!(required));
                }
            }
        }
        _ => {
            schema.insert("type".into(), json!(["string", "number", "boolean", "object", "array", "null"]));
        }
    }
    if let Some(identifier) = &port.identifier {
        schema.insert("x-identifier".into(), json!(identifier));
    }
    if let Some(choices) = port.choices.as_ref().filter(|c| !c.is_empty()) {
        schema.insert("enum".into(), Value::Array(choices.iter().map(|c| c.value.clone()).collect()));
    }
    if let Some(default) = &port.default {
        schema.insert("default".into(), default.clone());
    }
    if port.nullable {
        if let Some(Value::String(t)) = schema.get("type").cloned() {
            schema.insert("type".into(), json!([t, "null"]));
        }
    }
    Value::Object(schema)
}

/// `create_json_schema_from_ports`.
pub(crate) fn schema_from_ports(ports: &[Port], title: &str) -> Value {
    if ports.is_empty() {
        return json!({ "type": "object", "title": title, "properties": {} });
    }
    let properties: Map<String, Value> = ports.iter().map(|p| (p.key.clone(), port_to_json_schema(p))).collect();
    let required: Vec<&str> = ports
        .iter()
        .filter(|p| !p.nullable && p.default.is_none())
        .map(|p| p.key.as_str())
        .collect();
    let mut schema = json!({ "type": "object", "title": title, "properties": properties });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

/// The request body schema of a per-action route (stale fields included, as in Python).
pub(crate) fn request_schema(definition: &Definition) -> Value {
    let name = &definition.name;
    json!({
        "type": "object",
        "title": format!("{name}Request"),
        "properties": {
            "args": schema_from_ports(&definition.args, &format!("{name}Args")),
            "policy": { "type": "object", "description": "The policy for the task" },
            "reference": { "type": "string", "description": "A reference string" },
            "cached": { "type": "boolean", "default": false },
            "log": { "type": "boolean", "default": false },
            "capture": { "type": "boolean", "default": false },
            "ephemeral": { "type": "boolean", "default": false },
            "step": { "type": "boolean", "description": "Whether to step through the task" },
        },
        "required": ["args", "cached", "log", "capture", "ephemeral"],
    })
}
