use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug)]
pub struct FaktsAnswer<T> {
    pub config: T,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct RetrieveRequest {
    pub token: String,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct TokenConfig {
    pub token: String,
}

#[derive(Serialize)]
pub struct Requirement {
    pub key: String,
    pub service: String,
    pub optional: bool,
}

#[derive(Serialize)]
pub struct Manifest {
    pub identifier: String,
    pub version: String,
    pub scopes: Vec<String>,
    pub requirements: Vec<Requirement>,
}

#[derive(Serialize)]
pub struct DeviceCodeStartRequest<T> {
    pub manifest: T,
    pub requested_client_kind: String,
}

#[derive(Serialize)]
pub struct DeviceCodeChallengeRequest {
    pub code: String,
}

#[derive(Deserialize, Serialize)]
pub struct DeviceCodeAnswer {
    pub code: String,
    pub status: String,
}

#[derive(Deserialize, Serialize)]
pub struct DeviceCodeChallengeAnswer {
    pub status: String,
    pub token: Option<String>,
}
