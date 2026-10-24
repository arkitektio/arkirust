//! One HTTP exchange and one wait, as every target makes them.
//!
//! On wasm, reqwest runs over `fetch` and its futures hold JS values, so they
//! are not `Send`; fakts' futures must be (`TokenLoader`). An exchange is
//! therefore made whole here (send, read the body) and handed back as plain
//! data, its future wrapped on wasm, where there is one thread. The GraphQL
//! client (rath) makes its requests through here too.

use std::future::Future;
use std::time::Duration;

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
