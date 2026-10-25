//! Wire and cache models of the fakts v2 protocol.

use crate::collections::HashMap;
use crate::prelude::*;

use serde::{Deserialize, Serialize};

pub use arkitekt_spec::{Alias, Manifest, PublicSource, Requirement};

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
    /// The mesh coordination server (ionscale), when the deployment has a mesh.
    #[serde(default)]
    pub mesh_coord_url: Option<String>,
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
    /// The user the app acts for.
    #[serde(default, deserialize_with = "string_or_number")]
    pub sub: Option<String>,
    /// The organization the app was authorized in.
    #[serde(default, deserialize_with = "string_or_number")]
    pub organization: Option<String>,
    /// The hub the app is bound to (its mesh tag is `tag:hub-<hub>`).
    #[serde(default, deserialize_with = "string_or_number")]
    pub hub: Option<String>,
}

/// An id the server may send as a string or a number.
fn string_or_number<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<serde_json::Value>::deserialize(de)? {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(s),
        Some(other) => Some(other.to_string()),
    })
}

/// A key to join the deployment's mesh, handed out once with the first
/// device-code token when the app asked for one (`request_auth_key`).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshClaim {
    pub ionscale_auth_key: String,
    #[serde(default)]
    pub ionscale_coord_url: Option<String>,
}

impl core::fmt::Debug for MeshClaim {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MeshClaim")
            .field("ionscale_auth_key", &"***")
            .field("ionscale_coord_url", &self.ionscale_coord_url)
            .finish()
    }
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
    #[serde(default)]
    pub mesh: Option<MeshClaim>,
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
    /// None for a mandate-key login, which renews by logging in again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
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
    /// The mesh key from the first token; kept across refreshes, which do
    /// not carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<MeshClaim>,
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
            .or_else(|| fallback_refresh.map(str::to_owned));
        let expires_at = response
            .expires_in
            .map(|exp| now + exp - core::cmp::min(30, exp / 2));
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
            mesh: response.mesh,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_detection() {
        let alias = |host: &str, kind: Option<&str>| Alias {
            id: "a".into(),
            host: host.into(),
            port: None,
            ssl: false,
            path: None,
            challenge: String::new(),
            public: false,
            kind: kind.map(Into::into),
            proxy: None,
        };
        assert!(alias("meshhub.org.mesh.example", Some("mesh")).is_mesh());
        assert!(alias("100.64.0.1", None).is_mesh());
        assert!(alias("100.127.255.254", None).is_mesh());
        assert!(!alias("100.128.0.1", None).is_mesh());
        assert!(!alias("100.64.0.1", Some("absolute")).is_mesh());
        assert!(!alias("go.arkitekt.live", None).is_mesh());
    }

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
            kind: None,
            proxy: None,
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
