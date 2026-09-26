//! Mapping Rust types to ports, and values to and from the wire.
//!
//! [`PortType`] is what `#[action]` uses for every argument and return value:
//! the port shape comes from the type, and `expand`/`shrink` convert between
//! the JSON the agent receives and the Rust value the function works with.
//!
//! [`Structure`] is the Rust take on the Python structure registry: a type
//! that lives on a service (e.g. an `ArrayDataset` in mikro) travels as a
//! `{"__identifier", "object"}` reference and is expanded back to the full
//! object through the service's client.

use std::collections::HashMap;
use std::future::Future;

use serde_json::{json, Value};

use crate::context::Context;
use crate::ports::{Port, PortKind};

/// Why a value could not be converted.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct PortError {
    pub message: String,
}

impl PortError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Prefix the error with the port it happened on.
    pub fn at(self, key: &str) -> Self {
        Self::new(format!("port '{key}': {}", self.message))
    }
}

/// A Rust type that can travel through a port.
pub trait PortType: Sized + Send + 'static {
    /// The port describing this type under `key`.
    fn port(key: &str) -> Port;

    /// Convert an incoming JSON value into `Self`.
    fn expand(value: Value, ctx: &Context) -> impl Future<Output = Result<Self, PortError>> + Send;

    /// Convert `self` into the JSON value sent back to the server.
    fn shrink(self, ctx: &Context) -> impl Future<Output = Result<Value, PortError>> + Send;
}

fn decode<T: serde::de::DeserializeOwned>(value: Value, expected: &str) -> Result<T, PortError> {
    serde_json::from_value(value.clone())
        .map_err(|_| PortError::new(format!("expected {expected}, got {value}")))
}

macro_rules! primitive {
    ($kind:ident, $expected:literal, $($ty:ty),+) => {$(
        impl PortType for $ty {
            fn port(key: &str) -> Port {
                Port::new(key, PortKind::$kind)
            }

            async fn expand(value: Value, _ctx: &Context) -> Result<Self, PortError> {
                decode(value, $expected)
            }

            async fn shrink(self, _ctx: &Context) -> Result<Value, PortError> {
                Ok(json!(self))
            }
        }
    )+};
}

primitive!(String, "a string", String);
primitive!(Bool, "a boolean", bool);
primitive!(
    Int,
    "an integer",
    i8,
    i16,
    i32,
    i64,
    u8,
    u16,
    u32,
    u64,
    usize,
    isize
);
primitive!(Float, "a number", f32, f64);

impl PortType for chrono::DateTime<chrono::Utc> {
    fn port(key: &str) -> Port {
        Port::new(key, PortKind::Date)
    }

    async fn expand(value: Value, _ctx: &Context) -> Result<Self, PortError> {
        decode(value, "an ISO 8601 date")
    }

    async fn shrink(self, _ctx: &Context) -> Result<Value, PortError> {
        Ok(Value::String(self.to_rfc3339()))
    }
}

impl<T: PortType> PortType for Option<T> {
    fn port(key: &str) -> Port {
        T::port(key).nullable(true)
    }

    async fn expand(value: Value, ctx: &Context) -> Result<Self, PortError> {
        match value {
            Value::Null => Ok(None),
            value => Ok(Some(T::expand(value, ctx).await?)),
        }
    }

    async fn shrink(self, ctx: &Context) -> Result<Value, PortError> {
        match self {
            None => Ok(Value::Null),
            Some(value) => value.shrink(ctx).await,
        }
    }
}

impl<T: PortType> PortType for Vec<T> {
    fn port(key: &str) -> Port {
        Port::new(key, PortKind::List).child(T::port("..."))
    }

    async fn expand(value: Value, ctx: &Context) -> Result<Self, PortError> {
        let Value::Array(items) = value else {
            return Err(PortError::new(format!("expected a list, got {value}")));
        };
        let mut out = Vec::with_capacity(items.len());
        for (i, item) in items.into_iter().enumerate() {
            out.push(
                T::expand(item, ctx)
                    .await
                    .map_err(|e| e.at(&i.to_string()))?,
            );
        }
        Ok(out)
    }

    async fn shrink(self, ctx: &Context) -> Result<Value, PortError> {
        let mut out = Vec::with_capacity(self.len());
        for (i, item) in self.into_iter().enumerate() {
            out.push(item.shrink(ctx).await.map_err(|e| e.at(&i.to_string()))?);
        }
        Ok(Value::Array(out))
    }
}

impl<T: PortType> PortType for HashMap<String, T> {
    fn port(key: &str) -> Port {
        Port::new(key, PortKind::Dict).child(T::port("..."))
    }

    async fn expand(value: Value, ctx: &Context) -> Result<Self, PortError> {
        let Value::Object(entries) = value else {
            return Err(PortError::new(format!("expected a dict, got {value}")));
        };
        let mut out = HashMap::with_capacity(entries.len());
        for (k, v) in entries {
            let v = T::expand(v, ctx).await.map_err(|e| e.at(&k))?;
            out.insert(k, v);
        }
        Ok(out)
    }

    async fn shrink(self, ctx: &Context) -> Result<Value, PortError> {
        let mut out = serde_json::Map::with_capacity(self.len());
        for (k, v) in self {
            let v = v.shrink(ctx).await.map_err(|e| e.at(&k))?;
            out.insert(k, v);
        }
        Ok(Value::Object(out))
    }
}

/// An object that lives on a service and travels by reference.
///
/// ```ignore
/// impl Structure for ArrayDataset {
///     const IDENTIFIER: &'static str = "@mikro/arraydataset";
///     fn structure_id(&self) -> String { self.id.clone() }
///     async fn expand(id: String, ctx: &Context) -> anyhow::Result<Self> {
///         ctx.require::<Mikro>()?.get_array_dataset(&id).await
///     }
/// }
/// ```
pub trait Structure: Sized + Send + Sync + 'static {
    /// `@package/key`, e.g. `@mikro/arraydataset`.
    const IDENTIFIER: &'static str;

    /// The id this object is referenced by.
    fn structure_id(&self) -> String;

    /// Fetch the object for an id.
    fn expand(id: String, ctx: &Context) -> impl Future<Output = anyhow::Result<Self>> + Send;

    /// The widget the UI uses to pick a value for an argument of this type.
    fn widget() -> Option<Value> {
        None
    }
}

impl<S: Structure> PortType for S {
    fn port(key: &str) -> Port {
        Port::new(key, PortKind::Structure)
            .identifier(S::IDENTIFIER)
            .widget(S::widget())
    }

    async fn expand(value: Value, ctx: &Context) -> Result<Self, PortError> {
        let id = unwrap_reference(value, S::IDENTIFIER)?;
        S::expand(id.clone(), ctx)
            .await
            .map_err(|e| PortError::new(format!("could not expand {} {id}: {e:#}", S::IDENTIFIER)))
    }

    async fn shrink(self, _ctx: &Context) -> Result<Value, PortError> {
        Ok(json!({ "__identifier": S::IDENTIFIER, "object": self.structure_id() }))
    }
}

/// Validate a `{"__identifier", "object"}` envelope and return the object id.
pub fn unwrap_reference(value: Value, identifier: &str) -> Result<String, PortError> {
    let Value::Object(mut map) = value else {
        return Err(PortError::new(format!(
            "expected a {identifier} reference object, got {value}"
        )));
    };
    match map.get("__identifier").and_then(Value::as_str) {
        Some(found) if found == identifier => {}
        Some(found) => {
            return Err(PortError::new(format!(
                "identifier mismatch: expected {identifier}, got {found}"
            )))
        }
        None => return Err(PortError::new("missing __identifier in reference")),
    }
    match map.remove("object") {
        Some(Value::String(id)) => Ok(id),
        Some(Value::Number(id)) => Ok(id.to_string()),
        other => Err(PortError::new(format!(
            "invalid reference object {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Thing(String);

    impl Structure for Thing {
        const IDENTIFIER: &'static str = "@test/thing";
        fn structure_id(&self) -> String {
            self.0.clone()
        }
        async fn expand(id: String, _ctx: &Context) -> anyhow::Result<Self> {
            Ok(Thing(id))
        }
    }

    #[tokio::test]
    async fn round_trips() {
        let ctx = Context::default();
        let v = <Vec<Option<i64>> as PortType>::expand(json!([1, null, 3]), &ctx)
            .await
            .unwrap();
        assert_eq!(v, vec![Some(1), None, Some(3)]);

        let thing = <Thing as PortType>::expand(
            json!({"__identifier": "@test/thing", "object": "7"}),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(thing.0, "7");
        assert_eq!(
            PortType::shrink(thing, &ctx).await.unwrap(),
            json!({"__identifier": "@test/thing", "object": "7"})
        );

        let wrong =
            <Thing as PortType>::expand(json!({"__identifier": "@x/y", "object": "7"}), &ctx).await;
        assert!(wrong.is_err());
        assert!(<i64 as PortType>::expand(json!("nope"), &ctx)
            .await
            .is_err());
    }

    #[test]
    fn ports() {
        let port = <Vec<Thing> as PortType>::port("things");
        assert_eq!(port.kind, PortKind::List);
        assert_eq!(port.children[0].key, "...");
        assert_eq!(port.children[0].identifier.as_deref(), Some("@test/thing"));
        assert!(<Option<String> as PortType>::port("x").nullable);
    }
}
