//! The HTTP legs of fakts v2: discovery, device-code demand + polling, refresh.

use std::time::{Duration, Instant};

use reqwest::{Client, StatusCode, Url};
use serde::Serialize;

use crate::error::{FaktsError, Result};
use crate::models::{DeviceCodeResponse, Manifest, OAuthError, TokenResponse, WellKnown};

pub const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// The kind of OAuth client the server should mint for this app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    #[default]
    Development,
    Website,
    Desktop,
}

/// The role the minted client plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClientRole {
    #[default]
    Interface,
    Agent,
}

fn allow_insecure() -> bool {
    std::env::var("FAKTS_ALLOW_INSECURE_TRANSPORT")
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// Plain http is fine against loopback; anywhere else it needs an opt-in.
pub fn check_transport(url: &str, allow_insecure_transport: bool) -> Result<()> {
    let parsed =
        Url::parse(url).map_err(|e| FaktsError::Discovery(format!("invalid url {url}: {e}")))?;
    if parsed.scheme() != "http" || allow_insecure_transport || allow_insecure() {
        return Ok(());
    }
    let host = parsed.host_str().unwrap_or_default();
    let loopback = host == "localhost"
        || host
            .trim_matches(|c| c == '[' || c == ']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if loopback {
        Ok(())
    } else {
        Err(FaktsError::InsecureTransport(host.to_owned()))
    }
}

fn candidates(url: &str) -> Vec<String> {
    let url = url.trim_end_matches('/');
    if url.contains("://") {
        vec![url.to_owned()]
    } else {
        vec![format!("https://{url}"), format!("http://{url}")]
    }
}

/// `GET {url}/.well-known/fakts`. A url without a scheme is tried over https
/// first, then http. Returns the base url that answered and its document.
pub async fn discover(http: &Client, url: &str) -> Result<(String, WellKnown)> {
    let mut last_error = String::from("no candidate urls");
    for base in candidates(url) {
        let endpoint = format!("{base}/.well-known/fakts");
        match http
            .get(&endpoint)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                let doc: WellKnown = resp.json().await.map_err(|e| {
                    FaktsError::Discovery(format!("{endpoint}: invalid document: {e}"))
                })?;
                match doc.protocol_version.as_deref() {
                    Some("2") => return Ok((base, doc)),
                    other => {
                        return Err(FaktsError::Discovery(format!(
                            "{endpoint} speaks fakts protocol {}, this client needs 2",
                            other.unwrap_or("1")
                        )))
                    }
                }
            }
            Ok(resp) => last_error = format!("{endpoint}: HTTP {}", resp.status()),
            Err(e) => last_error = format!("{endpoint}: {e}"),
        }
    }
    Err(FaktsError::Discovery(last_error))
}

#[derive(Serialize)]
struct DemandBody<'a> {
    manifest: &'a Manifest,
    expiration_time_seconds: u64,
    redirect_uris: Vec<String>,
    requested_client_kind: ClientKind,
    requested_client_role: ClientRole,
}

/// Called with the approval URL and user code while the device code is pending.
pub type DeviceCodeHook = std::sync::Arc<dyn Fn(&str, &str) + Send + Sync>;

pub fn default_device_code_hook() -> DeviceCodeHook {
    std::sync::Arc::new(|uri: &str, user_code: &str| {
        eprintln!();
        eprintln!("  Please authorize this app by visiting:");
        eprintln!("    {uri}");
        if !user_code.is_empty() {
            eprintln!("  and confirm the code: {user_code}");
        }
        eprintln!();
    })
}

pub struct DeviceCodeOptions {
    pub client_kind: ClientKind,
    pub client_role: ClientRole,
    pub expiration: Duration,
    pub hook: DeviceCodeHook,
}

impl Default for DeviceCodeOptions {
    fn default() -> Self {
        Self {
            client_kind: ClientKind::default(),
            client_role: ClientRole::default(),
            expiration: Duration::from_secs(300),
            hook: default_device_code_hook(),
        }
    }
}

/// Result of a successful interactive grant: the response plus the client
/// id and token endpoint to carry into every later refresh.
pub struct GrantOutcome {
    pub response: TokenResponse,
    pub client_id: String,
    pub token_endpoint: String,
}

/// RFC 8628 device authorization, extended with the fakts manifest.
pub async fn device_code_grant(
    http: &Client,
    well_known: &WellKnown,
    manifest: &Manifest,
    options: &DeviceCodeOptions,
) -> Result<GrantOutcome> {
    let endpoint = well_known
        .device_authorization_endpoint
        .as_deref()
        .ok_or_else(|| {
            FaktsError::Discovery(
                "server does not advertise a device_authorization_endpoint".into(),
            )
        })?;

    let body = DemandBody {
        manifest,
        expiration_time_seconds: options.expiration.as_secs(),
        redirect_uris: vec![],
        requested_client_kind: options.client_kind,
        requested_client_role: options.client_role,
    };

    let demand = loop {
        let resp = http.post(endpoint).json(&body).send().await?;
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        let demand: DeviceCodeResponse = resp.json().await?;
        if demand.status.as_deref() == Some("error") || demand.error.is_some() {
            return Err(FaktsError::Protocol(format!(
                "device authorization refused: {}",
                demand.error.unwrap_or_else(|| "unknown error".into())
            )));
        }
        break demand;
    };

    let device_code = demand
        .device_code
        .ok_or_else(|| FaktsError::Protocol("demand response has no device_code".into()))?;
    let client_id = demand
        .client_id
        .ok_or_else(|| FaktsError::Protocol("demand response has no client_id".into()))?;
    let token_endpoint = demand
        .token_endpoint
        .unwrap_or_else(|| well_known.token_endpoint.clone());
    let verification = demand
        .verification_uri_complete
        .or(demand.verification_uri)
        .ok_or_else(|| FaktsError::Protocol("demand response has no verification uri".into()))?;

    (options.hook)(
        &verification,
        demand.user_code.as_deref().unwrap_or_default(),
    );

    let mut interval = Duration::from_secs(demand.interval.unwrap_or(5).max(1));
    let deadline = Instant::now() + Duration::from_secs(demand.expires_in.unwrap_or(300));

    loop {
        if Instant::now() >= deadline {
            return Err(FaktsError::Expired);
        }
        tokio::time::sleep(interval.min(deadline.saturating_duration_since(Instant::now()))).await;

        let resp = http
            .post(&token_endpoint)
            .form(&[
                ("grant_type", DEVICE_CODE_GRANT),
                ("device_code", device_code.as_str()),
                ("client_id", client_id.as_str()),
            ])
            .send()
            .await?;

        match parse_token_response(resp).await? {
            Ok(response) => {
                return Ok(GrantOutcome {
                    response,
                    client_id,
                    token_endpoint,
                })
            }
            Err(err) => match err.error.as_str() {
                "authorization_pending" => {}
                "slow_down" => interval += Duration::from_secs(5),
                "access_denied" => return Err(FaktsError::AccessDenied),
                "expired_token" => return Err(FaktsError::Expired),
                _ => {
                    return Err(FaktsError::OAuth {
                        error: err.error,
                        description: err.error_description,
                    })
                }
            },
        }
    }
}

/// `grant_type=refresh_token`. Refresh tokens rotate: the caller must persist
/// the new one before using the new access token.
pub async fn refresh_grant(
    http: &Client,
    token_endpoint: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenResponse> {
    let resp = http
        .post(token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .await?;
    match parse_token_response(resp).await? {
        Ok(response) => Ok(response),
        Err(err) if matches!(err.error.as_str(), "invalid_grant" | "invalid_client") => {
            Err(FaktsError::InvalidGrant(err.error))
        }
        Err(err) => Err(FaktsError::OAuth {
            error: err.error,
            description: err.error_description,
        }),
    }
}

async fn parse_token_response(
    resp: reqwest::Response,
) -> Result<std::result::Result<TokenResponse, OAuthError>> {
    let status = resp.status();
    let bytes = resp.bytes().await?;
    if status.is_success() {
        return Ok(Ok(serde_json::from_slice(&bytes)?));
    }
    match serde_json::from_slice::<OAuthError>(&bytes) {
        Ok(err) => Ok(Err(err)),
        Err(_) => Err(FaktsError::Protocol(format!(
            "token endpoint answered HTTP {status}: {}",
            String::from_utf8_lossy(&bytes)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_rules() {
        assert!(check_transport("http://127.0.0.1/lok", false).is_ok());
        assert!(check_transport("http://localhost:8000", false).is_ok());
        assert!(check_transport("https://example.com", false).is_ok());
        assert!(check_transport("http://example.com", true).is_ok());
    }
}
