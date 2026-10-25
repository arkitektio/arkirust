//! Who an app is and which services it needs: sent when it logs in (fakts),
//! kept with its releases (kabinet).

use alloc::collections::BTreeMap;

#[allow(unused_imports)]
use crate::prelude::*;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A service the app needs, keyed by the name it will look the service up by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    /// The key the instance is returned under, e.g. `"mikro"`.
    pub key: String,
    /// The service identifier, e.g. `"live.arkitekt.mikro"`.
    pub service: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Requirement {
    pub fn new(key: impl Into<String>, service: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            service: service.into(),
            optional: false,
            description: None,
        }
    }

    pub fn optional(mut self, optional: bool) -> Self {
        self.optional = optional;
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicSource {
    pub kind: String,
    pub url: String,
}

/// What an app is and what it needs. Sent with the device-code demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub identifier: String,
    pub version: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    #[serde(default)]
    pub requirements: Vec<Requirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default)]
    pub public_sources: Vec<PublicSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Manifest {
    pub fn new(identifier: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            identifier: identifier.into(),
            version: version.into(),
            scopes: vec!["openid".into()],
            logo: None,
            requirements: vec![],
            device_id: None,
            public_sources: vec![],
            description: None,
        }
    }

    /// A stable hash over the manifest, used to invalidate cached fakts when
    /// the app's declaration changes.
    pub fn hash(&self) -> String {
        let mut normalized = self.clone();
        normalized.scopes.sort();
        normalized.requirements.sort_by(|a, b| a.key.cmp(&b.key));
        normalized
            .public_sources
            .sort_by(|a, b| (&a.kind, &a.url).cmp(&(&b.kind, &b.url)));
        // Round-trip through a BTreeMap-backed Value so keys are sorted.
        let value: BTreeMap<String, serde_json::Value> =
            serde_json::from_value(serde_json::to_value(&normalized).expect("manifest serializes"))
                .expect("manifest is an object");
        let canonical = serde_json::to_string(&value).expect("manifest serializes");
        hex::encode(Sha256::digest(canonical.as_bytes()))
    }
}
