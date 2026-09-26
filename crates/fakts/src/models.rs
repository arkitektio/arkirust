//! Wire and cache models of the fakts v2 protocol.

use std::collections::{BTreeMap, HashMap};

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

/// `GET {url}/.well-known/fakts`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WellKnown {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub protocol_version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub issuer: Option<String>,
    pub token_endpoint: String,
    #[serde(default)]
    pub device_authorization_endpoint: Option<String>,
}

/// One way of reaching a service instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alias {
    pub id: String,
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub ssl: bool,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub challenge: String,
    #[serde(default)]
    pub public: bool,
}

impl Alias {
    fn build(&self, scheme: &str, append: Option<&str>) -> String {
        let mut url = format!("{scheme}://{}", self.host);
        if let Some(port) = self.port {
            url.push_str(&format!(":{port}"));
        }
        if let Some(path) = self.path.as_deref().filter(|p| !p.is_empty()) {
            url.push('/');
            url.push_str(path.trim_start_matches('/').trim_end_matches('/'));
        }
        if let Some(append) = append.filter(|a| !a.is_empty()) {
            url.push('/');
            url.push_str(append.trim_start_matches('/'));
        }
        url
    }

    /// `http(s)://host[:port][/path][/append]`
    pub fn to_http_path(&self, append: impl AsRef<str>) -> String {
        self.build(
            if self.ssl { "https" } else { "http" },
            Some(append.as_ref()),
        )
    }

    /// `ws(s)://host[:port][/path][/append]`
    pub fn to_ws_path(&self, append: impl AsRef<str>) -> String {
        self.build(if self.ssl { "wss" } else { "ws" }, Some(append.as_ref()))
    }

    /// The URL that must answer 200 before this alias is used.
    pub fn challenge_path(&self) -> String {
        self.to_http_path(&self.challenge)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeKey {
    pub kind: String,
    pub key: String,
}

/// A service instance granted for one requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instance {
    pub service: String,
    pub identifier: String,
    #[serde(default)]
    pub aliases: Vec<Alias>,
    #[serde(default)]
    pub challenge_key: Option<ChallengeKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfFakt {
    #[serde(default)]
    pub deployment_name: String,
    pub alias: Alias,
}

/// Response of the device authorization endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCodeResponse {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub device_code: Option<String>,
    #[serde(default)]
    pub user_code: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub token_endpoint: Option<String>,
    #[serde(default)]
    pub verification_uri: Option<String>,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub interval: Option<u64>,
}

/// Successful token endpoint response: tokens *and* configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    #[serde(default = "default_token_type")]
    pub token_type: String,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(rename = "self")]
    pub self_: Option<SelfFakt>,
    #[serde(default)]
    pub instances: HashMap<String, Instance>,
    #[serde(default)]
    pub statuses: HashMap<String, String>,
}

fn default_token_type() -> String {
    "Bearer".into()
}

/// OAuth2 error body (`{"error": "...", "error_description": "..."}`).
#[derive(Debug, Clone, Deserialize)]
pub struct OAuthError {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
}

/// The credential half of the active configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthFakt {
    pub client_id: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub report_endpoint: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub refresh_token: String,
    pub access_token: String,
    /// Unix timestamp (seconds) after which the access token is considered stale.
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default = "default_token_type")]
    pub token_type: String,
}

impl AuthFakt {
    pub fn is_expired(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|at| now >= at)
    }
}

/// Everything a grant produced: credentials plus the granted instances.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveFakts {
    #[serde(rename = "self")]
    pub self_: Option<SelfFakt>,
    pub auth: AuthFakt,
    #[serde(default)]
    pub instances: HashMap<String, Instance>,
    #[serde(default)]
    pub statuses: HashMap<String, String>,
}

impl ActiveFakts {
    /// Build from a token response. `fallback_refresh` keeps the old refresh
    /// token when a server does not rotate.
    pub(crate) fn from_token_response(
        response: TokenResponse,
        client_id: &str,
        token_endpoint: &str,
        report_endpoint: Option<String>,
        fallback_refresh: Option<&str>,
        now: i64,
    ) -> Result<Self, crate::FaktsError> {
        let refresh_token = response
            .refresh_token
            .or_else(|| fallback_refresh.map(str::to_owned))
            .ok_or_else(|| {
                crate::FaktsError::Protocol("token response carried no refresh_token".into())
            })?;
        let expires_at = response
            .expires_in
            .map(|exp| now + exp - std::cmp::min(30, exp / 2));
        Ok(Self {
            self_: response.self_,
            auth: AuthFakt {
                client_id: response.client_id.unwrap_or_else(|| client_id.to_owned()),
                token_endpoint: token_endpoint.to_owned(),
                report_endpoint,
                scopes: response
                    .scope
                    .map(|s| s.split_whitespace().map(str::to_owned).collect())
                    .unwrap_or_default(),
                refresh_token,
                access_token: response.access_token,
                expires_at,
                token_type: response.token_type,
            },
            instances: response.instances,
            statuses: response.statuses,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_paths() {
        let alias = Alias {
            id: "lan".into(),
            host: "10.0.0.4".into(),
            port: Some(8080),
            ssl: false,
            path: Some("/mikro".into()),
            challenge: "ht".into(),
            public: false,
        };
        assert_eq!(
            alias.to_http_path("graphql"),
            "http://10.0.0.4:8080/mikro/graphql"
        );
        assert_eq!(
            alias.to_ws_path("/graphql"),
            "ws://10.0.0.4:8080/mikro/graphql"
        );
        assert_eq!(alias.to_http_path(""), "http://10.0.0.4:8080/mikro");
        assert_eq!(alias.challenge_path(), "http://10.0.0.4:8080/mikro/ht");
    }

    #[test]
    fn manifest_hash_is_order_independent() {
        let mut a = Manifest::new("app", "1");
        a.requirements = vec![Requirement::new("a", "x"), Requirement::new("b", "y")];
        let mut b = a.clone();
        b.requirements.reverse();
        assert_eq!(a.hash(), b.hash());
    }
}
