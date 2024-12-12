use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct UnlokFakt {
    pub authorization_url: String,
    pub base_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub endpoint_url: String,
    pub name: String,
    pub scopes: Vec<String>,
}
