//! The [`Host`] with std: reqwest over rustls natively, over `fetch` on wasm.
//!
//! On wasm, reqwest's futures hold JS values, so they are not `Send`; fakts'
//! futures must be (`TokenLoader`). An exchange is therefore made whole here
//! (send, read the body) and handed back as plain data, its future wrapped on
//! wasm, where there is one thread.

use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;

use crate::host::{Body, Host, HostError, Method, Reply as HostReply, Request};

/// reqwest as the [`Host`]: a client for requests that follow redirects, and
/// one built alike that does not (alias challenges), through `proxy` if set.
#[derive(Debug)]
pub struct Reqwest {
    client: reqwest::Client,
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), allow(dead_code))]
    proxy: Option<String>,
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), allow(dead_code))]
    no_redirects: OnceLock<reqwest::Client>,
}

impl Reqwest {
    /// Requests through `client` (redirect-free ones through a client like it).
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            proxy: None,
            no_redirects: OnceLock::new(),
        }
    }

    /// A fresh client with fakts' user agent.
    pub fn default_client() -> reqwest::Result<reqwest::Client> {
        crate::install_crypto_provider();
        reqwest::Client::builder()
            .user_agent(concat!("fakts-rs/", env!("CARGO_PKG_VERSION")))
            .build()
    }

    /// Everything through the HTTP proxy at `proxy` (the mesh's).
    pub fn with_proxy(proxy: &str) -> crate::Result<Self> {
        crate::install_crypto_provider();
        let builder =
            reqwest::Client::builder().user_agent(concat!("fakts-rs/", env!("CARGO_PKG_VERSION")));
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        let builder = builder.proxy(reqwest::Proxy::all(proxy)?);
        Ok(Self {
            client: builder.build()?,
            proxy: Some(proxy.to_owned()),
            no_redirects: OnceLock::new(),
        })
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    fn client_for(&self, follow_redirects: bool) -> reqwest::Result<reqwest::Client> {
        if follow_redirects {
            return Ok(self.client.clone());
        }
        // fetch follows redirects and has no proxy: there is one client on wasm.
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        return Ok(self.client.clone());
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            if let Some(client) = self.no_redirects.get() {
                return Ok(client.clone());
            }
            let builder = reqwest::Client::builder()
                .user_agent(concat!("fakts-rs/", env!("CARGO_PKG_VERSION")))
                .redirect(reqwest::redirect::Policy::none());
            let builder = match &self.proxy {
                Some(proxy) => builder.proxy(reqwest::Proxy::all(proxy)?),
                None => builder,
            };
            let client = builder.build()?;
            Ok(self.no_redirects.get_or_init(|| client).clone())
        }
    }

    fn build(&self, request: &Request) -> reqwest::Result<reqwest::RequestBuilder> {
        let client = self.client_for(request.follow_redirects)?;
        let mut builder = match request.method {
            Method::Get => client.get(&request.url),
            Method::Post => client.post(&request.url),
        };
        if !request.query.is_empty() {
            builder = builder.query(&request.query);
        }
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        builder = match &request.body {
            Body::Empty => builder,
            Body::Json(bytes) => builder
                .header("content-type", "application/json")
                .body(bytes.clone()),
            Body::Form(fields) => builder.form(fields),
        };
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        if let Some(timeout) = request.timeout {
            builder = builder.timeout(timeout);
        }
        Ok(builder)
    }
}

#[async_trait]
impl Host for Reqwest {
    async fn send(&self, request: Request) -> Result<HostReply, HostError> {
        let builder = self.build(&request).map_err(|e| HostError(e.to_string()))?;
        let reply = fetch(builder).await.map_err(|e| HostError(e.to_string()))?;
        Ok(HostReply {
            status: reply.status.as_u16(),
            body: reply.body,
        })
    }

    async fn sleep(&self, duration: Duration) {
        sleep(duration).await
    }

    fn now_unix(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }

    fn fill_random(&self, bytes: &mut [u8]) {
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), bytes)
    }
}

/// A response, read to its end.
pub struct Reply {
    pub status: reqwest::StatusCode,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_slice(&self.body)
    }
}

/// Send `request` and read the whole response.
pub async fn fetch(request: reqwest::RequestBuilder) -> reqwest::Result<Reply> {
    portable(async move {
        let response = request.send().await?;
        let status = response.status();
        let body = response.bytes().await?.to_vec();
        Ok(Reply { status, body })
    })
    .await
}

pub async fn sleep(duration: Duration) {
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    tokio::time::sleep(duration).await;
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    portable(gloo_timers::future::sleep(duration)).await;
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn portable<F: Future + Send>(future: F) -> F {
    future
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn portable<F: Future>(future: F) -> send_wrapper::SendWrapper<F> {
    send_wrapper::SendWrapper::new(future)
}

/// reqwest clients for an [`Alias`](crate::Alias).
pub trait AliasExt {
    /// A client for this alias: proxied through the mesh sidecar when needed.
    fn http_client_builder(&self) -> reqwest::Result<reqwest::ClientBuilder>;

    /// See [`AliasExt::http_client_builder`].
    fn http_client(&self) -> reqwest::Result<reqwest::Client>;
}

impl AliasExt for crate::Alias {
    fn http_client_builder(&self) -> reqwest::Result<reqwest::ClientBuilder> {
        let builder = reqwest::Client::builder();
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        return Ok(match &self.proxy {
            Some(proxy) => builder.proxy(reqwest::Proxy::all(proxy)?),
            None => builder,
        });
        // A browser has no proxy, and no mesh to reach.
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        Ok(builder)
    }

    fn http_client(&self) -> reqwest::Result<reqwest::Client> {
        crate::install_crypto_provider();
        self.http_client_builder()?.build()
    }
}
