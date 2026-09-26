use thiserror::Error;

#[derive(Debug, Error)]
pub enum FaktsError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("discovery failed: {0}")]
    Discovery(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("the user denied the authorization request")]
    AccessDenied,
    #[error("the device code expired before it was approved")]
    Expired,
    #[error(
        "the refresh token was rejected ({0}); the app has to be authorized again \
         (delete the fakts cache or run with no_cache)"
    )]
    InvalidGrant(String),
    #[error("oauth error: {error}{}", description.as_deref().map(|d| format!(": {d}")).unwrap_or_default())]
    OAuth {
        error: String,
        description: Option<String>,
    },
    #[error("no instance was granted for requirement '{key}' (status: {status})")]
    MissingInstance { key: String, status: String },
    #[error("no alias of '{0}' answered its challenge")]
    NoReachableAlias(String),
    #[error("refusing plain http to non-loopback host {0}; set FAKTS_ALLOW_INSECURE_TRANSPORT=1 to allow")]
    InsecureTransport(String),
}

pub type Result<T, E = FaktsError> = std::result::Result<T, E>;
