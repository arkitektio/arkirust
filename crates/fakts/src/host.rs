//! What fakts needs from where it runs: HTTP exchanges, waiting, the clock
//! and randomness. The login flow (discovery, grants, refresh, alias
//! challenges) is written against [`Host`] and nothing else, so it runs
//! wherever one is implemented:
//!
//! - with std, [`Reqwest`](crate::http::Reqwest): reqwest over rustls
//!   natively, over `fetch` on wasm (the default);
//! - without std, the app's: e.g. an HTTP client on embassy-net.
//!
//! An app on embassy implements this and rekuest's `rt::Runtime` (the same
//! clock and randomness) on one struct.

use core::time::Duration;

use async_trait::async_trait;

use crate::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Empty,
    /// `application/json`
    Json(Vec<u8>),
    /// `application/x-www-form-urlencoded`
    Form(Vec<(String, String)>),
}

/// One HTTP request, as plain data.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub url: String,
    /// Appended to the url's query string, encoded.
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Body,
    /// Give up after this long (where the host can).
    pub timeout: Option<Duration>,
    /// Follow redirects. Alias challenges do not: a challenge answers itself.
    pub follow_redirects: bool,
}

impl Request {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: Method::Get,
            url: url.into(),
            query: vec![],
            headers: vec![],
            body: Body::Empty,
            timeout: None,
            follow_redirects: true,
        }
    }

    pub fn post(url: impl Into<String>, body: Body) -> Self {
        Self {
            method: Method::Post,
            body,
            ..Self::get(url)
        }
    }

    pub fn json<T: serde::Serialize + ?Sized>(
        url: impl Into<String>,
        body: &T,
    ) -> crate::Result<Self> {
        Ok(Self::post(url, Body::Json(serde_json::to_vec(body)?)))
    }

    pub fn form(url: impl Into<String>, fields: &[(&str, &str)]) -> Self {
        Self::post(
            url,
            Body::Form(
                fields
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            ),
        )
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn query(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push((name.into(), value.into()));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn no_redirects(mut self) -> Self {
        self.follow_redirects = false;
        self
    }

    /// The url with the query appended, for hosts that send it as one.
    pub fn full_url(&self) -> String {
        if self.query.is_empty() {
            return self.url.clone();
        }
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &self.query {
            query.append_pair(k, v);
        }
        let separator = if self.url.contains('?') { '&' } else { '?' };
        format!("{}{separator}{}", self.url, query.finish())
    }

    /// The body as bytes, with its content type.
    pub fn encoded_body(&self) -> Option<(&'static str, Vec<u8>)> {
        match &self.body {
            Body::Empty => None,
            Body::Json(bytes) => Some(("application/json", bytes.clone())),
            Body::Form(fields) => {
                let mut form = url::form_urlencoded::Serializer::new(String::new());
                for (k, v) in fields {
                    form.append_pair(k, v);
                }
                Some((
                    "application/x-www-form-urlencoded",
                    form.finish().into_bytes(),
                ))
            }
        }
    }
}

/// A response, read to its end.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json<T: serde::de::DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_slice(&self.body)
    }
}

/// The exchange did not happen (no answer, a broken connection, a timeout).
#[derive(Debug, Clone, PartialEq)]
pub struct HostError(pub String);

impl core::fmt::Display for HostError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::error::Error for HostError {}

/// See the module docs.
#[async_trait]
pub trait Host: Send + Sync {
    /// Make `request` and read the whole response.
    async fn send(&self, request: Request) -> Result<Reply, HostError>;

    async fn sleep(&self, duration: Duration);

    /// Seconds since the Unix epoch (token expiry).
    fn now_unix(&self) -> i64;

    /// Fill `bytes` with random ones (challenge nonces).
    fn fill_random(&self, bytes: &mut [u8]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_and_forms_are_encoded() {
        let request = Request::get("http://x/ht").query("nonce", "a b&c");
        assert_eq!(request.full_url(), "http://x/ht?nonce=a+b%26c");
        let form = Request::form("http://x/token", &[("grant_type", "a:b"), ("k", "v")]);
        assert_eq!(
            form.encoded_body(),
            Some((
                "application/x-www-form-urlencoded",
                b"grant_type=a%3Ab&k=v".to_vec()
            ))
        );
    }
}
