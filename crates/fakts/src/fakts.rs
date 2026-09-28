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
    mesh_proxy: Option<String>,
    #[cfg(feature = "mesh")]
    mesh: Option<crate::mesh::MeshOptions>,
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
            mesh_proxy: None,
            #[cfg(feature = "mesh")]
            mesh: None,
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

    /// Reach mesh aliases through this HTTP proxy (e.g. an already running
    /// `arkitekt mesh proxy` at `http://localhost:1055`). Aliases that need
    /// the mesh are skipped without one.
    pub fn mesh_proxy(mut self, proxy: impl Into<String>) -> Self {
        self.mesh_proxy = Some(proxy.into());
        self
    }

    /// Join the deployment's mesh: ask for a mesh key when authorizing, then
    /// run a mesh node (the `arkitekt-meshd` sidecar, or our own client with
    /// [`MeshBackend::Native`](crate::mesh::MeshBackend)) and reach mesh
    /// aliases through it.
    /// Ignored when [`mesh_proxy`](Self::mesh_proxy) is set.
    #[cfg(feature = "mesh")]
    pub fn mesh(mut self, options: crate::mesh::MeshOptions) -> Self {
        self.mesh = Some(options);
        self
    }

    /// The grant to run: with the mesh on, the device code also asks for a mesh key.
    fn effective_grant(&self) -> Grant {
        #[cfg(feature = "mesh")]
        if self.mesh.is_some() && self.mesh_proxy.is_none() {
            if let Grant::DeviceCode(options) = &self.grant {
                return Grant::DeviceCode(Arc::new(DeviceCodeOptions {
                    request_auth_key: true,
                    ..(**options).clone()
                }));
            }
        }
        self.grant.clone()
    }

    /// Discover the server, then load the cached credential or run the grant.
    pub async fn load(self) -> Result<Fakts> {
        crate::install_crypto_provider();
        let grant = self.effective_grant();
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
                let active = run_grant(&http, &well_known, &self.manifest, &grant).await?;
                if let Some(path) = &cache_path {
                    cache::write(path, &cache_key, &active).await?;
                }
                active
            }
        };

        #[cfg(feature = "mesh")]
        let mesh_node = match (&self.mesh, &self.mesh_proxy) {
            (Some(options), None) => {
                start_mesh(options, &active, &well_known, &self.manifest, &cache_key).await?
            }
            _ => None,
        };
        #[cfg(feature = "mesh")]
        let mesh_proxy = self
            .mesh_proxy
            .or_else(|| mesh_node.as_ref().map(|n| n.proxy_url().to_owned()));
        #[cfg(not(feature = "mesh"))]
        let mesh_proxy = self.mesh_proxy;

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
                mesh_proxy,
                #[cfg(feature = "mesh")]
                mesh_node: mesh_node.map(Mutex::new),
            }),
        })
    }
}

/// Start the mesh node for this app. The node lives in a state
/// directory keyed by the app's identity, so it is joined once (with the key
/// from the first token) and re-used afterwards.
#[cfg(feature = "mesh")]
async fn start_mesh(
    options: &crate::mesh::MeshOptions,
    active: &ActiveFakts,
    well_known: &WellKnown,
    manifest: &Manifest,
    cache_key: &str,
) -> Result<Option<crate::mesh::MeshNode>> {
    use crate::mesh::{hostname_label, Join, MeshNode};
    use sha2::{Digest, Sha256};

    let identity = active
        .self_
        .as_ref()
        .and_then(|s| {
            Some(format!(
                "{}-{}-{}",
                s.sub.as_ref()?,
                s.organization.as_ref()?,
                s.hub.as_ref()?
            ))
        })
        .unwrap_or_else(|| hex::encode(&Sha256::digest(cache_key.as_bytes())[..8]));
    let statedir = options.node_dir(&format!(
        "{}-{}",
        hostname_label(&manifest.identifier),
        hostname_label(&identity)
    ));

    let join = match &active.mesh {
        Some(claim) => {
            let coord_url = claim
                .ionscale_coord_url
                .clone()
                .or_else(|| well_known.mesh_coord_url.clone())
                .ok_or_else(|| {
                    FaktsError::Mesh("the server sent a mesh key but no coordination url".into())
                })?;
            Join {
                coord_url: Some(coord_url),
                auth_key: Some(claim.ionscale_auth_key.clone()),
            }
        }
        // A joined node remembers its coordination server.
        None if MeshNode::has_state(options, &statedir) => Join {
            coord_url: well_known.mesh_coord_url.clone(),
            auth_key: None,
        },
        None => {
            tracing::warn!(
                "the mesh is enabled, but this app holds no mesh key; mesh aliases will be skipped \
                 (authorize again with no_cache and allow mesh access to join)"
            );
            return Ok(None);
        }
    };

    let hostname = options.hostname.clone().unwrap_or_else(|| {
        let device = manifest.device_id.as_deref().unwrap_or_default();
        let device: String = device
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(8)
            .collect();
        hostname_label(&format!("{}-{device}", manifest.identifier))
    });
    MeshNode::start(options, statedir, &hostname, join)
        .await
        .map(Some)
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
    /// The HTTP proxy mesh aliases are reached through.
    mesh_proxy: Option<String>,
    /// Kept alive for as long as the handle; stops the node when dropped.
    #[cfg(feature = "mesh")]
    #[cfg_attr(not(feature = "mesh-relay"), allow(dead_code))]
    mesh_node: Option<Mutex<crate::mesh::MeshNode>>,
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

    /// The HTTP proxy mesh aliases are reached through, if the mesh is on.
    pub fn mesh_proxy(&self) -> Option<&str> {
        self.inner.mesh_proxy.as_deref()
    }

    /// The mesh node's TURN relay, as an ICE server for a WebRTC client.
    ///
    /// WebRTC media (e.g. LiveKit) cannot use the HTTP proxy. Given only
    /// this ICE server and a relay-only transport policy, the client sends
    /// everything through the relay on 127.0.0.1, which relays it over the
    /// mesh to peers such as a mesh-only SFU. Needs the native backend.
    #[cfg(feature = "mesh-relay")]
    pub async fn mesh_turn(&self) -> Result<mesh::driver::TurnInfo> {
        self.running_mesh_node()?.lock().await.turn().await
    }

    /// A local `127.0.0.1` port that forwards TCP to a mesh alias (its port,
    /// or 443/80 by its scheme), for clients that cannot use the HTTP proxy
    /// (e.g. LiveKit's signaling websocket). Needs the native backend.
    #[cfg(feature = "mesh-relay")]
    pub async fn mesh_forward(&self, alias: &Alias) -> Result<std::net::SocketAddr> {
        let port = alias.port.unwrap_or(if alias.ssl { 443 } else { 80 });
        self.running_mesh_node()?
            .lock()
            .await
            .forward(&alias.host, port)
            .await
    }

    #[cfg(feature = "mesh-relay")]
    fn running_mesh_node(&self) -> Result<&Mutex<crate::mesh::MeshNode>> {
        self.inner.mesh_node.as_ref().ok_or_else(|| {
            FaktsError::Mesh(
                "the mesh is not running: enable it (ARKITEKT_MESH=native) and \
                 authorize with mesh access"
                    .into(),
            )
        })
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
        let challenge_client = |proxy: Option<&str>| -> Result<Client> {
            let builder = Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(3));
            Ok(match proxy {
                Some(proxy) => builder.proxy(reqwest::Proxy::all(proxy)?),
                None => builder,
            }
            .build()?)
        };
        let direct = challenge_client(None)?;
        let proxied = self
            .mesh_proxy()
            .map(|p| challenge_client(Some(p)))
            .transpose()?;

        let mut skipped_mesh = false;
        for alias in &instance.aliases {
            let (http, proxy) = if alias.is_mesh() {
                match (&proxied, self.mesh_proxy()) {
                    (Some(http), Some(proxy)) => (http, Some(proxy)),
                    _ => {
                        skipped_mesh = true;
                        tracing::debug!(
                            "skipping alias {} of {key}: it needs the mesh, which is off",
                            alias.id
                        );
                        continue;
                    }
                }
            } else {
                (&direct, None)
            };
            match self.challenge(http, instance, alias).await {
                Ok(()) => {
                    let mut alias = alias.clone();
                    alias.proxy = proxy.map(str::to_owned);
                    return Ok(alias);
                }
                Err(e) => tracing::debug!("alias {} of {key} failed its challenge: {e}", alias.id),
            }
        }
        if skipped_mesh {
            tracing::warn!(
                "{key} has aliases only reachable over the mesh; enable the mesh to use them"
            );
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
                    let mut active = ActiveFakts::from_token_response(
                        response,
                        &auth.client_id,
                        &auth.token_endpoint,
                        auth.report_endpoint.clone(),
                        Some(&auth.refresh_token),
                        chrono::Utc::now().timestamp(),
                    )?;
                    if active.mesh.is_none() {
                        active.mesh = self.inner.active.lock().await.mesh.clone();
                    }
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
