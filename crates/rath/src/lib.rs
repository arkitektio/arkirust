#![cfg_attr(not(feature = "std"), no_std)]

//! Rath: a small authenticated GraphQL client for Arkitekt services.
//!
//! Mirrors the Python `rath` link chain that every service uses:
//! `AuthTokenLink -> HttpLink`. It attaches the bearer token from a
//! [`TokenLoader`], refreshes it and retries when the server rejects it, and
//! forwards the current rekuest task (if any) as a `Rekuest-Task` header so
//! that work done inside an action is attributed to that task.
//!
//! Every request goes through a [`fakts::Host`] (reqwest with std, the app's
//! without it), so the client runs wherever fakts does. Typed operations use
//! `graphql_client` (with std); [`Rath::execute_raw`] works everywhere:
//!
//! ```ignore
//! let data = client.execute::<GetArrayDataset>(get_array_dataset::Variables { id }).await?;
//! ```

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use fakts::host::{Host, HostError, Request};
use fakts::TokenLoader;
#[cfg(feature = "std")]
use graphql_client::GraphQLQuery;
use serde::Deserialize;
use thiserror::Error;

/// Header carrying the token of the rekuest task a request is made for.
pub const TASK_HEADER: &str = "Rekuest-Task";

#[cfg(feature = "std")]
std::thread_local! {
    /// The token of the rekuest task whose future is being polled, if any.
    static CURRENT_TASK_TOKEN: core::cell::RefCell<Option<String>> =
        const { core::cell::RefCell::new(None) };
}

/// Without std, one executor thread: a global slot.
#[cfg(not(feature = "std"))]
static CURRENT_TASK_TOKEN: spin::Mutex<Option<String>> = spin::Mutex::new(None);

fn swap_task_token(token: Option<String>) -> Option<String> {
    #[cfg(feature = "std")]
    return CURRENT_TASK_TOKEN.with(|t| t.replace(token));
    #[cfg(not(feature = "std"))]
    core::mem::replace(&mut *CURRENT_TASK_TOKEN.lock(), token)
}

/// Run `fut` with `token` as the current task token, so every GraphQL request
/// made from inside it carries the `Rekuest-Task` header.
///
/// Set while `fut` is polled (on any runtime, as a task local is): work it
/// hands to another thread does not carry it.
pub async fn with_task_token<F: core::future::Future>(token: Option<String>, fut: F) -> F::Output {
    let mut fut = core::pin::pin!(fut);
    core::future::poll_fn(move |cx| {
        let previous = swap_task_token(token.clone());
        let poll = fut.as_mut().poll(cx);
        swap_task_token(previous);
        poll
    })
    .await
}

fn current_task_token() -> Option<String> {
    #[cfg(feature = "std")]
    return CURRENT_TASK_TOKEN.with(|t| t.borrow().clone());
    #[cfg(not(feature = "std"))]
    CURRENT_TASK_TOKEN.lock().clone()
}

#[derive(Debug, Clone, Deserialize)]
pub struct GraphQLError {
    pub message: String,
    #[serde(default)]
    pub path: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub extensions: Option<serde_json::Value>,
}

impl GraphQLError {
    fn code(&self) -> Option<&str> {
        self.extensions.as_ref()?.get("code")?.as_str()
    }
}

#[derive(Debug, Error)]
pub enum RathError {
    #[cfg(feature = "std")]
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    /// The host could not make the exchange.
    #[error("http error: {0}")]
    Transport(#[from] HostError),
    #[error("token error: {0}")]
    Token(#[from] fakts::FaktsError),
    #[error("graphql errors: {}", .0.iter().map(|e| e.message.as_str()).collect::<Vec<_>>().join("; "))]
    GraphQL(Vec<GraphQLError>),
    #[error("the server answered HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("response had no data")]
    NoData,
    #[error("still unauthenticated after {0} token refreshes")]
    Unauthenticated(usize),
    #[error("invalid response: {0}")]
    Decode(#[from] serde_json::Error),
}

pub type Result<T, E = RathError> = core::result::Result<T, E>;

#[derive(Deserialize)]
struct RawResponse<D> {
    data: Option<D>,
    #[serde(default)]
    errors: Option<Vec<GraphQLError>>,
}

/// An authenticated GraphQL client for one service endpoint. Cheap to clone.
#[derive(Clone)]
pub struct Rath {
    host: Arc<dyn Host>,
    endpoint: String,
    token_loader: Arc<dyn TokenLoader>,
    max_refresh_attempts: usize,
}

impl core::fmt::Debug for Rath {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Rath")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl Rath {
    /// A client for `endpoint` whose requests go through `host`.
    pub fn with_host(
        host: Arc<dyn Host>,
        endpoint: impl Into<String>,
        token_loader: Arc<dyn TokenLoader>,
    ) -> Self {
        Self {
            host,
            endpoint: endpoint.into(),
            token_loader,
            max_refresh_attempts: 3,
        }
    }

    /// A client for `endpoint` over reqwest.
    #[cfg(feature = "std")]
    pub fn new(endpoint: impl Into<String>, token_loader: Arc<dyn TokenLoader>) -> Self {
        fakts::install_crypto_provider();
        let http = reqwest::Client::builder()
            .user_agent(concat!("rath-rs/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client builds");
        Self::with_host(
            Arc::new(fakts::http::Reqwest::new(http)),
            endpoint,
            token_loader,
        )
    }

    /// A client for `append` under `alias`, proxied through the mesh when
    /// the alias needs it.
    #[cfg(feature = "std")]
    pub fn from_alias(
        alias: &fakts::Alias,
        append: impl AsRef<str>,
        token_loader: Arc<dyn TokenLoader>,
    ) -> Result<Self> {
        fakts::install_crypto_provider();
        let http = alias
            .http_client_builder()
            .and_then(|b| {
                b.user_agent(concat!("rath-rs/", env!("CARGO_PKG_VERSION")))
                    .build()
            })
            .map_err(fakts::FaktsError::from)?;
        Ok(Self::new(alias.to_http_path(append), token_loader).with_http_client(http))
    }

    /// Make requests through `http`.
    #[cfg(feature = "std")]
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.host = Arc::new(fakts::http::Reqwest::new(http));
        self
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn token_loader(&self) -> &Arc<dyn TokenLoader> {
        &self.token_loader
    }

    /// Execute a typed query or mutation.
    #[cfg(feature = "std")]
    pub async fn execute<Q: GraphQLQuery>(
        &self,
        variables: Q::Variables,
    ) -> Result<Q::ResponseData> {
        let body = Q::build_query(variables);
        let body = serde_json::to_value(&body)?;
        let value = self.execute_raw(&body).await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Execute an untyped `{query, variables, operationName}` body and return `data`.
    pub async fn execute_raw(&self, body: &serde_json::Value) -> Result<serde_json::Value> {
        let mut token = self.token_loader.get_token().await?;
        let body = serde_json::to_vec(body)?;
        for attempt in 0..=self.max_refresh_attempts {
            let mut request =
                Request::post(self.endpoint.clone(), fakts::host::Body::Json(body.clone()))
                    .header("authorization", format!("Bearer {token}"));
            if let Some(task) = current_task_token() {
                request = request.header(TASK_HEADER, task);
            }
            let reply = self.host.send(request).await?;
            let status = reply.status;

            if status == 401 {
                tracing::debug!(
                    "{} rejected the token (attempt {}), refreshing",
                    self.endpoint,
                    attempt
                );
                token = self.token_loader.refresh_token(&token).await?;
                continue;
            }

            let parsed: RawResponse<serde_json::Value> = match serde_json::from_slice(&reply.body) {
                Ok(parsed) => parsed,
                Err(_) if !(200..300).contains(&status) => {
                    return Err(RathError::Status {
                        status,
                        body: String::from_utf8_lossy(&reply.body).into_owned(),
                    })
                }
                Err(e) => return Err(e.into()),
            };

            if let Some(errors) = parsed.errors.filter(|e| !e.is_empty()) {
                if errors.iter().any(|e| e.code() == Some("UNAUTHENTICATED")) {
                    token = self.token_loader.refresh_token(&token).await?;
                    continue;
                }
                return Err(RathError::GraphQL(errors));
            }
            return parsed.data.ok_or(RathError::NoData);
        }
        Err(RathError::Unauthenticated(self.max_refresh_attempts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct Counting(AtomicUsize);

    #[async_trait::async_trait]
    impl TokenLoader for Counting {
        async fn get_token(&self) -> fakts::Result<String> {
            Ok(format!("t{}", self.0.load(Ordering::SeqCst)))
        }
        async fn refresh_token(&self, _stale: &str) -> fakts::Result<String> {
            Ok(format!("t{}", self.0.fetch_add(1, Ordering::SeqCst) + 1))
        }
    }

    #[tokio::test]
    async fn refreshes_on_401_and_sends_task_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("authorization", "Bearer t0"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(header("authorization", "Bearer t1"))
            .and(header(TASK_HEADER, "task-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": {"ok": true}})),
            )
            .mount(&server)
            .await;

        let rath = Rath::new(server.uri(), Arc::new(Counting(AtomicUsize::new(0))));
        let data = with_task_token(
            Some("task-token".into()),
            rath.execute_raw(&serde_json::json!({"query": "{ ok }"})),
        )
        .await
        .unwrap();
        assert_eq!(data, serde_json::json!({"ok": true}));
    }

    #[tokio::test]
    async fn surfaces_graphql_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"data": null, "errors": [{"message": "boom"}]}),
                ),
            )
            .mount(&server)
            .await;
        let rath = Rath::new(server.uri(), Arc::new(Counting(AtomicUsize::new(0))));
        let err = rath
            .execute_raw(&serde_json::json!({"query": "{ ok }"}))
            .await
            .unwrap_err();
        assert!(matches!(err, RathError::GraphQL(ref e) if e[0].message == "boom"));
    }
}
