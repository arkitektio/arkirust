use super::fakt::UnlokFakt;
use oauth2::basic::BasicClient;
use oauth2::http::{request, response};
use oauth2::reqwest::async_http_client; // Use the provided async HTTP client function
use oauth2::{AuthUrl, ClientId, ClientSecret, Scope, TokenResponse, TokenUrl};

pub async fn get_auth_token(config: UnlokFakt) -> Result<String, Box<dyn std::error::Error>> {
    let client = BasicClient::new(
        ClientId::new(config.client_id),
        Some(ClientSecret::new(config.client_secret)),
        AuthUrl::new(config.base_url.clone() + "/authorize/")?,
        Some(TokenUrl::new(config.base_url.clone() + "/token/")?),
    );

    let token_result = client
        .exchange_client_credentials()
        .add_scopes(config.scopes.into_iter().map(|s| Scope::new(s)))
        // Use the async_http_client function provided by the oauth2 crate
        .request_async(async_http_client)
        .await?;

    Ok(token_result.access_token().secret().to_string())
}
