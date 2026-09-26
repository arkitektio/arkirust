//! The `Fakts` handle: loads (or grants) the active configuration, hands out
//! access tokens and resolves service aliases.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use rand::RngCore;
use reqwest::Client;
use tokio::sync::Mutex;

use crate::cache;
use crate::error::{FaktsError, Result};
use crate::grants::{self, DeviceCodeOptions};
use crate::models::{ActiveFakts, Alias, Instance, Manifest, WellKnown};

/// Anything that can hand out a bearer token and renew a stale one.
///
/// `refresh_token` receives the token that was rejected so that concurrent
/// failures trigger only one refresh: if the current token already differs
/// from `stale`, it is returned as is.
#[async_trait]
pub trait TokenLoader: Send + Sync {
    async fn get_token(&self) -> Result<String>;
    async fn refresh_token(&self, stale: &str) -> Result<String>;
}

/// How the first credential is obtained when nothing is cached.
#[derive(Clone)]
pub enum Grant {
    /// Interactive RFC 8628 device code flow (the default).
    DeviceCode(Arc<DeviceCodeOptions>),
    /// A known `client_id` + refresh token, e.g. from `FAKTS_TOKEN=client_id:refresh_token`.
    RefreshToken {
        client_id: String,
        refresh_token: String,
    },
}

/// Configures and loads a [`Fakts`] handle.
pub struct FaktsBuilder {
    url: String,
    manifest: Manifest,
    grant: Grant,
    cache_path: Option<PathBuf>,
    use_cache: bool,
    allow_insecure_transport: bool,
    http: Option<Client>,
}

impl FaktsBuilder {
    pub fn new(url: impl Into<String>, manifest: Manifest) -> Self {
        let grant = match std::env::var("FAKTS_TOKEN") {
            Ok(token) if token.contains(':') => {
                let (client_id, refresh_token) = token.split_once(':').expect("checked");
                Grant::RefreshToken {
                    client_id: client_id.to_owned(),
                    refresh_token: refresh_token.to_owned(),
                }
            }
            _ => Grant::DeviceCode(Arc::new(DeviceCodeOptions::default())),
        };
        Self {
            url: url.into(),
            manifest,
            grant,
            cache_path: None,
            use_cache: true,
            allow_insecure_transport: false,
            http: None,
        }
    }

    pub fn grant(mut self, grant: Grant) -> Self {
        self.grant = grant;
        self
    }

    /// Use a specific cache file instead of the per-user state directory.
    pub fn cache_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.cache_path = Some(path.into());
        self
    }

    /// Ignore (and do not write) the on-disk cache.
    pub fn no_cache(mut self, no_cache: bool) -> Self {
        self.use_cache = !no_cache;
        self
    }

    pub fn allow_insecure_transport(mut self, allow: bool) -> Self {
        self.allow_insecure_transport = allow;
        self
    }

    pub fn http_client(mut self, http: Client) -> Self {
        self.http = Some(http);
        self
    }

    /// Discover the server, then load the cached credential or run the grant.
    pub async fn load(self) -> Result<Fakts> {
        crate::install_crypto_provider();
        let http = match self.http {
            Some(http) => http,
            None => Client::builder()
                .user_agent(concat!("fakts-rs/", env!("CARGO_PKG_VERSION")))
                .build()?,
        };

        let (base_url, well_known) = grants::discover(&http, &self.url).await?;
        grants::check_transport(&well_known.token_endpoint, self.allow_insecure_transport)?;

        let cache_key = cache::cache_key(&self.manifest, &base_url);
        let cache_path = self.use_cache.then(|| {
            self.cache_path
                .clone()
                .unwrap_or_else(|| cache::default_cache_path(&self.manifest, &base_url))
        });

        let cached = match &cache_path {
            Some(path) => cache::read(path, &cache_key).await.map(|file| file.fakts),
            None => None,
        };

        let active = match cached {
            Some(active) => {
                tracing::debug!("using cached fakts");
                active
            }
            None => {
                let active = run_grant(&http, &well_known, &self.manifest, &self.grant).await?;
                if let Some(path) = &cache_path {
                    cache::write(path, &cache_key, &active).await?;
                }
                active
            }
        };

        Ok(Fakts {
            inner: Arc::new(Inner {
                http,
                base_url,
                well_known,
                manifest: self.manifest,
                cache_path,
                cache_key,
                active: Mutex::new(active),
                refresh_lock: Mutex::new(()),
                aliases: Mutex::new(HashMap::new()),
            }),
        })
    }
}

fn report_endpoint(well_known: &WellKnown) -> Option<String> {
    well_known
        .base_url
        .as_ref()
        .map(|base| format!("{}/report/", base.trim_end_matches('/')))
}

async fn run_grant(
    http: &Client,
    well_known: &WellKnown,
    manifest: &Manifest,
    grant: &Grant,
) -> Result<ActiveFakts> {
    let now = chrono::Utc::now().timestamp();
    match grant {
        Grant::DeviceCode(options) => {
            let outcome = grants::device_code_grant(http, well_known, manifest, options).await?;
            ActiveFakts::from_token_response(
                outcome.response,
                &outcome.client_id,
                &outcome.token_endpoint,
                report_endpoint(well_known),
                None,
                now,
            )
        }
        Grant::RefreshToken {
            client_id,
            refresh_token,
        } => {
            let response =
                grants::refresh_grant(http, &well_known.token_endpoint, client_id, refresh_token)
                    .await?;
            ActiveFakts::from_token_response(
                response,
                client_id,
                &well_known.token_endpoint,
                report_endpoint(well_known),
                Some(refresh_token),
                now,
            )
        }
    }
}

struct Inner {
    http: Client,
    base_url: String,
    well_known: WellKnown,
    manifest: Manifest,
    cache_path: Option<PathBuf>,
    cache_key: String,
    active: Mutex<ActiveFakts>,
    refresh_lock: Mutex<()>,
    aliases: Mutex<HashMap<String, Alias>>,
}

/// A loaded fakts configuration. Cheap to clone.
#[derive(Clone)]
pub struct Fakts {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Fakts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fakts")
            .field("base_url", &self.inner.base_url)
            .field("manifest", &self.inner.manifest.identifier)
            .finish_non_exhaustive()
    }
}

/// How many times a rejected refresh may adopt a newer credential from the cache.
const MAX_ADOPTIONS: usize = 4;

impl Fakts {
    pub fn builder(url: impl Into<String>, manifest: Manifest) -> FaktsBuilder {
        FaktsBuilder::new(url, manifest)
    }

    pub fn manifest(&self) -> &Manifest {
        &self.inner.manifest
    }

    pub fn base_url(&self) -> &str {
        &self.inner.base_url
    }

    pub fn well_known(&self) -> &WellKnown {
        &self.inner.well_known
    }

    pub fn http(&self) -> &Client {
        &self.inner.http
    }

    /// A snapshot of the current configuration.
    pub async fn active(&self) -> ActiveFakts {
        self.inner.active.lock().await.clone()
    }

    /// The instance granted for a requirement key.
    pub async fn instance(&self, key: &str) -> Result<Instance> {
        let active = self.inner.active.lock().await;
        active
            .instances
            .get(key)
            .cloned()
            .ok_or_else(|| FaktsError::MissingInstance {
                key: key.to_owned(),
                status: active
                    .statuses
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| "not granted".into()),
            })
    }

    /// Resolve a requirement key to the first alias that answers its challenge.
    pub async fn get_alias(&self, key: &str) -> Result<Alias> {
        if let Some(alias) = self.inner.aliases.lock().await.get(key) {
            return Ok(alias.clone());
        }
        let instance = self.instance(key).await?;
        let alias = self.resolve_alias(key, &instance).await?;
        self.inner
            .aliases
            .lock()
            .await
            .insert(key.to_owned(), alias.clone());
        Ok(alias)
    }

    /// The fakts server's own address (for services hosted next to it).
    pub async fn get_self_alias(&self) -> Result<Alias> {
        self.inner
            .active
            .lock()
            .await
            .self_
            .as_ref()
            .map(|s| s.alias.clone())
            .ok_or_else(|| FaktsError::Protocol("the server sent no self alias".into()))
    }

    async fn resolve_alias(&self, key: &str, instance: &Instance) -> Result<Alias> {
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()?;
        for alias in &instance.aliases {
            match self.challenge(&http, instance, alias).await {
                Ok(()) => return Ok(alias.clone()),
                Err(e) => tracing::debug!("alias {} of {key} failed its challenge: {e}", alias.id),
            }
        }
        Err(FaktsError::NoReachableAlias(key.to_owned()))
    }

    async fn challenge(&self, http: &Client, instance: &Instance, alias: &Alias) -> Result<()> {
        let url = alias.challenge_path();
        let Some(key) = &instance.challenge_key else {
            let resp = http.get(&url).send().await?;
            return if resp.status().is_success() {
                Ok(())
            } else {
                Err(FaktsError::Protocol(format!(
                    "challenge answered {}",
                    resp.status()
                )))
            };
        };

        let mut raw = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut raw);
        let nonce = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let resp = http.get(&url).query(&[("nonce", &nonce)]).send().await?;
        if !resp.status().is_success() {
            return Err(FaktsError::Protocol(format!(
                "challenge answered {}",
                resp.status()
            )));
        }
        #[derive(serde::Deserialize)]
        struct Signed {
            signature: String,
        }
        let signed: Signed = resp.json().await?;
        verify_challenge(key, &nonce, &signed.signature)
    }

    async fn persist(&self, active: &ActiveFakts) -> Result<()> {
        if let Some(path) = &self.inner.cache_path {
            cache::write(path, &self.inner.cache_key, active).await?;
        }
        Ok(())
    }

    /// Adopt a credential another process rotated into the shared cache.
    async fn adopt_from_cache(&self, current_refresh: &str) -> Option<ActiveFakts> {
        let path = self.inner.cache_path.as_ref()?;
        let file = cache::read(path, &self.inner.cache_key).await?;
        (file.fakts.auth.refresh_token != current_refresh).then_some(file.fakts)
    }

    async fn do_refresh(&self) -> Result<String> {
        for _ in 0..=MAX_ADOPTIONS {
            let auth = self.inner.active.lock().await.auth.clone();
            match grants::refresh_grant(
                &self.inner.http,
                &auth.token_endpoint,
                &auth.client_id,
                &auth.refresh_token,
            )
            .await
            {
                Ok(response) => {
                    let active = ActiveFakts::from_token_response(
                        response,
                        &auth.client_id,
                        &auth.token_endpoint,
                        auth.report_endpoint.clone(),
                        Some(&auth.refresh_token),
                        chrono::Utc::now().timestamp(),
                    )?;
                    // Persist the rotated refresh token *before* using the access token.
                    self.persist(&active).await?;
                    let token = active.auth.access_token.clone();
                    *self.inner.active.lock().await = active;
                    self.inner.aliases.lock().await.clear();
                    return Ok(token);
                }
                Err(FaktsError::InvalidGrant(reason)) => {
                    match self.adopt_from_cache(&auth.refresh_token).await {
                        Some(newer) => {
                            tracing::debug!("adopting a credential rotated by another process");
                            let expired = newer.auth.is_expired(chrono::Utc::now().timestamp());
                            let token = newer.auth.access_token.clone();
                            *self.inner.active.lock().await = newer;
                            if !expired {
                                return Ok(token);
                            }
                        }
                        None => {
                            if let Some(path) = &self.inner.cache_path {
                                cache::remove(path).await;
                            }
                            return Err(FaktsError::InvalidGrant(reason));
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(FaktsError::InvalidGrant(
            "too many concurrent rotations".into(),
        ))
    }
}

#[async_trait]
impl TokenLoader for Fakts {
    async fn get_token(&self) -> Result<String> {
        let (token, expired) = {
            let active = self.inner.active.lock().await;
            (
                active.auth.access_token.clone(),
                active.auth.is_expired(chrono::Utc::now().timestamp()),
            )
        };
        if expired {
            self.refresh_token(&token).await
        } else {
            Ok(token)
        }
    }

    async fn refresh_token(&self, stale: &str) -> Result<String> {
        let _guard = self.inner.refresh_lock.lock().await;
        let current = self.inner.active.lock().await.auth.access_token.clone();
        if current != stale {
            // Someone else refreshed while we waited for the lock.
            return Ok(current);
        }
        self.do_refresh().await
    }
}

fn verify_challenge(
    key: &crate::models::ChallengeKey,
    nonce: &str,
    signature_b64: &str,
) -> Result<()> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let b64 = base64::engine::general_purpose::STANDARD;
    if key.kind != "ed25519" {
        return Err(FaktsError::Protocol(format!(
            "unsupported challenge key kind {}",
            key.kind
        )));
    }
    let key_bytes: [u8; 32] = b64
        .decode(&key.key)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| FaktsError::Protocol("malformed challenge key".into()))?;
    let verifying = VerifyingKey::from_bytes(&key_bytes)
        .map_err(|e| FaktsError::Protocol(format!("invalid challenge key: {e}")))?;
    let sig_bytes: [u8; 64] = b64
        .decode(signature_b64)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| FaktsError::Protocol("malformed challenge signature".into()))?;
    verifying
        .verify(
            format!("fakts-challenge-v1:{nonce}").as_bytes(),
            &Signature::from_bytes(&sig_bytes),
        )
        .map_err(|_| FaktsError::Protocol("challenge signature does not verify".into()))
}
