//! The `Fakts` handle: loads (or grants) the active configuration, hands out
//! access tokens and resolves service aliases.

use core::time::Duration;
#[cfg(feature = "std")]
use std::path::PathBuf;

use async_lock::Mutex;
use async_trait::async_trait;
use base64::Engine;
#[cfg(feature = "std")]
use reqwest::Client;

#[cfg(feature = "std")]
use crate::cache::FileStore;
use crate::cache::{self, FaktsStore};
use crate::collections::HashMap;
use crate::error::{FaktsError, Result};
use crate::grants::{self, DeviceCodeOptions, MandateKeyOptions};
use crate::host::{Host, Request};
use crate::mesh_join::{MeshJoin, MeshJoiner};
use crate::models::{ActiveFakts, Alias, Instance, Manifest, WellKnown};
use crate::prelude::*;

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
    /// A standing mandate key, e.g. from `FAKTS_MANDATE_KEY`: headless, and
    /// renewed by logging in again (there is no refresh token).
    MandateKey(Arc<MandateKeyOptions>),
}

/// How long an alias may take to answer its challenge.
const CHALLENGE_TIMEOUT: Duration = Duration::from_secs(3);
/// The same through the mesh proxy.
const MESH_CHALLENGE_TIMEOUT: Duration = Duration::from_secs(20);

/// Configures and loads a [`Fakts`] handle.
pub struct FaktsBuilder {
    url: String,
    manifest: Manifest,
    grant: Grant,
    store: Option<Arc<dyn FaktsStore>>,
    use_cache: bool,
    allow_insecure_transport: bool,
    host: Option<Arc<dyn Host>>,
    #[cfg(feature = "std")]
    http: Option<Client>,
    mesh_proxy: Option<String>,
    mesh_joiner: Option<Arc<dyn MeshJoiner>>,
    #[cfg(feature = "mesh")]
    mesh: Option<crate::mesh::MeshOptions>,
}

impl FaktsBuilder {
    /// With std the grant comes from `FAKTS_MANDATE_KEY` or `FAKTS_TOKEN` if
    /// set, else the device code; without std it is the device code until
    /// [`grant`](Self::grant) says otherwise.
    pub fn new(url: impl Into<String>, manifest: Manifest) -> Self {
        #[cfg(feature = "std")]
        let mandate_key = std::env::var("FAKTS_MANDATE_KEY")
            .ok()
            .filter(|k| !k.is_empty());
        #[cfg(feature = "std")]
        let token = std::env::var("FAKTS_TOKEN").ok();
        #[cfg(not(feature = "std"))]
        let (mandate_key, token): (Option<String>, Option<String>) = (None, None);
        let grant = match (mandate_key, token) {
            (Some(key), _) => Grant::MandateKey(Arc::new(MandateKeyOptions::new(key))),
            (None, Some(token)) if token.contains(':') => {
                let (client_id, refresh_token) = token.split_once(':').expect("checked");
                Grant::RefreshToken {
                    client_id: client_id.to_owned(),
                    refresh_token: refresh_token.to_owned(),
                }
            }
            (None, _) => Grant::DeviceCode(Arc::new(DeviceCodeOptions::default())),
        };
        Self {
            url: url.into(),
            manifest,
            grant,
            store: None,
            use_cache: true,
            allow_insecure_transport: false,
            host: None,
            #[cfg(feature = "std")]
            http: None,
            mesh_proxy: None,
            mesh_joiner: None,
            #[cfg(feature = "mesh")]
            mesh: None,
        }
    }

    pub fn grant(mut self, grant: Grant) -> Self {
        self.grant = grant;
        self
    }

    /// Use a specific cache file instead of the per-user state directory.
    #[cfg(feature = "std")]
    pub fn cache_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.store = Some(Arc::new(FileStore::new(path)));
        self
    }

    /// Keep the fakts somewhere other than a file, e.g. a firmware's NVS.
    pub fn store(mut self, store: Arc<dyn FaktsStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Ignore (and do not write) the store.
    pub fn no_cache(mut self, no_cache: bool) -> Self {
        self.use_cache = !no_cache;
        self
    }

    pub fn allow_insecure_transport(mut self, allow: bool) -> Self {
        self.allow_insecure_transport = allow;
        self
    }

    #[cfg(feature = "std")]
    pub fn http_client(mut self, http: Client) -> Self {
        self.http = Some(http);
        self
    }

    /// Make every request through `host` (required without std; with std the
    /// default is reqwest, see [`http_client`](Self::http_client)).
    pub fn host(mut self, host: Arc<dyn Host>) -> Self {
        self.host = Some(host);
        self
    }

    /// Reach mesh aliases through this HTTP proxy (e.g. an already running
    /// `arkitekt mesh proxy` at `http://localhost:1055`). Aliases that need
    /// the mesh are skipped without one.
    pub fn mesh_proxy(mut self, proxy: impl Into<String>) -> Self {
        self.mesh_proxy = Some(proxy.into());
        self
    }

    /// Join the deployment's mesh with a node the app runs itself: ask for a
    /// mesh key when authorizing, hand it to `joiner`, and reach mesh aliases
    /// through the proxy it returns. Ignored when
    /// [`mesh_proxy`](Self::mesh_proxy) is set; wins over [`mesh`](Self::mesh).
    pub fn mesh_joiner(mut self, joiner: Arc<dyn MeshJoiner>) -> Self {
        self.mesh_joiner = Some(joiner);
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

    /// The grant to run: with the mesh on, the first login also asks for a mesh key.
    fn effective_grant(&self) -> Grant {
        #[cfg(feature = "mesh")]
        let joins = self.mesh_joiner.is_some() || self.mesh.is_some();
        #[cfg(not(feature = "mesh"))]
        let joins = self.mesh_joiner.is_some();
        if joins && self.mesh_proxy.is_none() {
            match &self.grant {
                Grant::DeviceCode(options) => {
                    return Grant::DeviceCode(Arc::new(DeviceCodeOptions {
                        request_auth_key: true,
                        ..(**options).clone()
                    }));
                }
                Grant::MandateKey(options) => {
                    return Grant::MandateKey(Arc::new(MandateKeyOptions {
                        request_auth_key: true,
                        ..(**options).clone()
                    }));
                }
                Grant::RefreshToken { .. } => {}
            }
        }
        self.grant.clone()
    }

    /// Discover the server, then load the cached credential or run the grant.
    pub async fn load(self) -> Result<Fakts> {
        let grant = self.effective_grant();
        #[cfg(feature = "std")]
        let (host, http) = {
            crate::install_crypto_provider();
            let http = match self.http {
                Some(http) => http,
                None => crate::http::Reqwest::default_client()?,
            };
            let host: Arc<dyn Host> = match self.host {
                Some(host) => host,
                None => Arc::new(crate::http::Reqwest::new(http.clone())),
            };
            (host, http)
        };
        #[cfg(not(feature = "std"))]
        let host = self.host.ok_or_else(|| {
            FaktsError::Protocol("without std fakts needs a Host (FaktsBuilder::host)".into())
        })?;

        let (base_url, well_known) = grants::discover_with(&*host, &self.url).await?;
        grants::check_transport(&well_known.token_endpoint, self.allow_insecure_transport)?;

        let cache_key = cache::cache_key(&self.manifest, &base_url);
        let store = if self.use_cache {
            self.store
                .clone()
                .or_else(|| default_store(&self.manifest, &base_url))
        } else {
            None
        };

        let cached = match &store {
            Some(store) => store.load(&cache_key).await,
            None => None,
        };

        let active = match cached {
            Some(active) => {
                tracing::debug!("using cached fakts");
                active
            }
            None => {
                let active = run_grant(&*host, &well_known, &self.manifest, &grant).await?;
                if let Some(store) = &store {
                    store.save(&cache_key, &active).await?;
                }
                active
            }
        };

        let joined = match (&self.mesh_joiner, &self.mesh_proxy) {
            (Some(joiner), None) => {
                joiner
                    .join(mesh_join(&active, &well_known, &self.manifest))
                    .await?
            }
            _ => None,
        };
        #[cfg(feature = "mesh")]
        let mesh_node = match (&self.mesh, &self.mesh_proxy, &joined) {
            (Some(options), None, None) if self.mesh_joiner.is_none() => {
                start_mesh(options, &active, &well_known, &self.manifest, &cache_key).await?
            }
            _ => None,
        };
        #[cfg(feature = "mesh")]
        let mesh_proxy = self
            .mesh_proxy
            .or(joined)
            .or_else(|| mesh_node.as_ref().map(|n| n.proxy_url().to_owned()));
        #[cfg(not(feature = "mesh"))]
        let mesh_proxy = self.mesh_proxy.or(joined);
        // Mesh aliases are challenged through the proxy (with std; without
        // it there is no proxy client, and mesh aliases are skipped).
        #[cfg(feature = "std")]
        let mesh_host = mesh_proxy
            .as_deref()
            .map(crate::http::Reqwest::with_proxy)
            .transpose()?
            .map(|host| Arc::new(host) as Arc<dyn Host>);
        #[cfg(not(feature = "std"))]
        let mesh_host: Option<Arc<dyn Host>> = None;

        Ok(Fakts {
            inner: Arc::new(Inner {
                host,
                mesh_host,
                #[cfg(feature = "std")]
                http,
                base_url,
                well_known,
                manifest: self.manifest,
                mandate_key: match grant {
                    Grant::MandateKey(options) => Some(options),
                    _ => None,
                },
                store,
                cache_key,
                active: Mutex::new(active),
                refresh_lock: Mutex::new(()),
                aliases: Mutex::new(HashMap::default()),
                mesh_proxy,
                #[cfg(feature = "mesh")]
                mesh_node: mesh_node.map(Mutex::new),
            }),
        })
    }
}

/// What a [`MeshJoiner`] gets: the key from this login (or the cached one),
/// its coordination server, and the node's hostname.
fn mesh_join(active: &ActiveFakts, well_known: &WellKnown, manifest: &Manifest) -> MeshJoin {
    MeshJoin {
        coord_url: active
            .mesh
            .as_ref()
            .and_then(|claim| claim.ionscale_coord_url.clone())
            .or_else(|| well_known.mesh_coord_url.clone()),
        auth_key: active
            .mesh
            .as_ref()
            .map(|claim| claim.ionscale_auth_key.clone()),
        hostname: crate::mesh_join::default_hostname(manifest),
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

    let hostname = options
        .hostname
        .clone()
        .unwrap_or_else(|| crate::mesh_join::default_hostname(manifest));
    MeshNode::start(options, statedir, &hostname, join)
        .await
        .map(Some)
}

/// The cache file in the user's state directory; nothing without one.
#[cfg(feature = "user-dirs")]
fn default_store(manifest: &Manifest, url: &str) -> Option<Arc<dyn FaktsStore>> {
    Some(Arc::new(FileStore::default_for(manifest, url)))
}

#[cfg(not(feature = "user-dirs"))]
fn default_store(_manifest: &Manifest, _url: &str) -> Option<Arc<dyn FaktsStore>> {
    tracing::debug!("no fakts store: the app logs in again on every start");
    None
}

fn report_endpoint(well_known: &WellKnown) -> Option<String> {
    well_known
        .base_url
        .as_ref()
        .map(|base| format!("{}/report/", base.trim_end_matches('/')))
}

async fn run_grant(
    host: &dyn Host,
    well_known: &WellKnown,
    manifest: &Manifest,
    grant: &Grant,
) -> Result<ActiveFakts> {
    let now = host.now_unix();
    match grant {
        Grant::DeviceCode(options) => {
            let outcome =
                grants::device_code_grant_with(host, well_known, manifest, options).await?;
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
            let response = grants::refresh_grant_with(
                host,
                &well_known.token_endpoint,
                client_id,
                refresh_token,
            )
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
        Grant::MandateKey(options) => {
            let response = grants::mandate_key_grant_with(
                host,
                &well_known.token_endpoint,
                manifest,
                options,
                options.request_auth_key,
            )
            .await?;
            let client_id = response.client_id.clone().ok_or_else(|| {
                FaktsError::Protocol("the mandate-key login named no client_id".into())
            })?;
            ActiveFakts::from_token_response(
                response,
                &client_id,
                &well_known.token_endpoint,
                report_endpoint(well_known),
                None,
                now,
            )
        }
    }
}

struct Inner {
    host: Arc<dyn Host>,
    /// Through the mesh proxy, for mesh aliases.
    mesh_host: Option<Arc<dyn Host>>,
    #[cfg(feature = "std")]
    http: Client,
    base_url: String,
    well_known: WellKnown,
    manifest: Manifest,
    /// Set when the app logs in with a mandate key: renewing logs in again.
    mandate_key: Option<Arc<MandateKeyOptions>>,
    store: Option<Arc<dyn FaktsStore>>,
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

impl core::fmt::Debug for Fakts {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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

    #[cfg(feature = "std")]
    pub fn http(&self) -> &Client {
        &self.inner.http
    }

    /// What every request goes through.
    pub fn host(&self) -> &Arc<dyn Host> {
        &self.inner.host
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
        let mut skipped_mesh = false;
        for alias in &instance.aliases {
            // The first request to a mesh peer sets up its tunnel (a WireGuard
            // handshake, through a DERP relay at first): seconds, on a board.
            let (host, proxy, timeout) = if alias.is_mesh() {
                match (&self.inner.mesh_host, self.mesh_proxy()) {
                    (Some(host), Some(proxy)) => (host, Some(proxy), MESH_CHALLENGE_TIMEOUT),
                    _ => {
                        skipped_mesh = true;
                        tracing::debug!(
                            "skipping alias {} of {}: it needs the mesh, which is off",
                            alias.id,
                            key
                        );
                        continue;
                    }
                }
            } else {
                (&self.inner.host, None, CHALLENGE_TIMEOUT)
            };
            match self.challenge(&**host, timeout, instance, alias).await {
                Ok(()) => {
                    let mut alias = alias.clone();
                    alias.proxy = proxy.map(str::to_owned);
                    return Ok(alias);
                }
                Err(e) => {
                    tracing::debug!("alias {} of {} failed its challenge: {}", alias.id, key, e)
                }
            }
        }
        if skipped_mesh {
            tracing::warn!(
                "{} has aliases only reachable over the mesh; enable the mesh to use them",
                key
            );
        }
        Err(FaktsError::NoReachableAlias(key.to_owned()))
    }

    /// A challenge answers itself: redirects are not followed.
    async fn challenge(
        &self,
        host: &dyn Host,
        timeout: Duration,
        instance: &Instance,
        alias: &Alias,
    ) -> Result<()> {
        let request = Request::get(alias.challenge_path())
            .no_redirects()
            .timeout(timeout);
        let Some(key) = &instance.challenge_key else {
            let reply = host.send(request).await?;
            return if reply.is_success() {
                Ok(())
            } else {
                Err(FaktsError::Protocol(format!(
                    "challenge answered {}",
                    reply.status
                )))
            };
        };

        let mut raw = [0u8; 24];
        host.fill_random(&mut raw);
        let nonce = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let reply = host.send(request.query("nonce", nonce.clone())).await?;
        if !reply.is_success() {
            return Err(FaktsError::Protocol(format!(
                "challenge answered {}",
                reply.status
            )));
        }
        #[derive(serde::Deserialize)]
        struct Signed {
            signature: String,
        }
        let signed: Signed = reply.json()?;
        verify_challenge(key, &nonce, &signed.signature)
    }

    async fn persist(&self, active: &ActiveFakts) -> Result<()> {
        if let Some(store) = &self.inner.store {
            store.save(&self.inner.cache_key, active).await?;
        }
        Ok(())
    }

    /// Adopt a credential another process rotated into the shared store.
    async fn adopt_from_cache(&self, current_refresh: &str) -> Option<ActiveFakts> {
        let store = self.inner.store.as_ref()?;
        let fakts = store.load(&self.inner.cache_key).await?;
        (fakts.auth.refresh_token.as_deref() != Some(current_refresh)).then_some(fakts)
    }

    /// Log in with the mandate key again, for a fresh access token and
    /// envelope. Never asks for a mesh key: the node keeps the one it joined with.
    async fn renew_with_mandate_key(&self, options: &MandateKeyOptions) -> Result<String> {
        let auth = self.inner.active.lock().await.auth.clone();
        let response = grants::mandate_key_grant_with(
            &*self.inner.host,
            &auth.token_endpoint,
            &self.inner.manifest,
            options,
            false,
        )
        .await?;
        let mut active = ActiveFakts::from_token_response(
            response,
            &auth.client_id,
            &auth.token_endpoint,
            auth.report_endpoint.clone(),
            None,
            self.inner.host.now_unix(),
        )?;
        active.mesh = self.inner.active.lock().await.mesh.clone();
        self.persist(&active).await?;
        let token = active.auth.access_token.clone();
        *self.inner.active.lock().await = active;
        self.inner.aliases.lock().await.clear();
        Ok(token)
    }

    async fn do_refresh(&self) -> Result<String> {
        if let Some(options) = &self.inner.mandate_key {
            return self.renew_with_mandate_key(options).await;
        }
        for _ in 0..=MAX_ADOPTIONS {
            let auth = self.inner.active.lock().await.auth.clone();
            let Some(refresh_token) = auth.refresh_token.clone() else {
                return Err(FaktsError::InvalidGrant(
                    "the credential has no refresh token and the app holds no mandate key".into(),
                ));
            };
            match grants::refresh_grant_with(
                &*self.inner.host,
                &auth.token_endpoint,
                &auth.client_id,
                &refresh_token,
            )
            .await
            {
                Ok(response) => {
                    let mut active = ActiveFakts::from_token_response(
                        response,
                        &auth.client_id,
                        &auth.token_endpoint,
                        auth.report_endpoint.clone(),
                        Some(&refresh_token),
                        self.inner.host.now_unix(),
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
                    match self.adopt_from_cache(&refresh_token).await {
                        Some(newer) => {
                            tracing::debug!("adopting a credential rotated by another process");
                            let expired = newer.auth.is_expired(self.inner.host.now_unix());
                            let token = newer.auth.access_token.clone();
                            *self.inner.active.lock().await = newer;
                            if !expired {
                                return Ok(token);
                            }
                        }
                        None => {
                            if let Some(store) = &self.inner.store {
                                store.clear().await;
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
                active.auth.is_expired(self.inner.host.now_unix()),
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
