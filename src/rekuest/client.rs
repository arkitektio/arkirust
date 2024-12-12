use anyhow::Error;
use graphql_client::QueryBody;
use reqwest::Client;

use super::fakt::RekuestFakt;

pub type RekuestClientFunc = reqwest::RequestBuilder;

pub struct RekuestClient {
    client: Client,
    endpoint_url: String,
}

impl RekuestClient {
    pub fn new(fakt: RekuestFakt, token: &str) -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .user_agent("graphql-rust/0.10.0")
            .default_headers(
                std::iter::once((
                    reqwest::header::AUTHORIZATION,
                    reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token)).unwrap(),
                ))
                .collect(),
            )
            .build()?;

        Ok(Self {
            client,
            endpoint_url: fakt.endpoint_url.clone(),
        })
    }

    pub fn request<T: serde::Serialize>(&self, body: &QueryBody<T>) -> RekuestClientFunc {
        self.client.post(&self.endpoint_url).json(body)
    }
}
