//! The datalayer: S3-compatible object storage behind mikro.
//!
//! Mikro never hands out long-lived storage credentials. Every upload or
//! download starts with a grant (temporary STS credentials scoped to one
//! key prefix). The store built from a grant is rooted *at* that prefix, so
//! nothing ever touches the bucket root, which a prefix-scoped grant would
//! refuse.

use std::sync::Arc;

use arkitekt::Alias;
use object_store::aws::AmazonS3Builder;
use object_store::prefix::PrefixStore;
use object_store::ClientOptions;
use zarrs_object_store::AsyncObjectStore;

/// A zarr-capable store rooted at a grant's key.
pub type GrantStore = AsyncObjectStore<PrefixStore<object_store::aws::AmazonS3>>;

/// Temporary credentials for one key prefix of the datalayer.
#[derive(Debug, Clone)]
pub struct Grant {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: String,
    pub bucket: String,
    pub key: String,
}

/// Where the datalayer is reachable (the `live.arkitekt.s3` alias).
#[derive(Debug, Clone)]
pub struct DataLayer {
    endpoint_url: String,
    /// The HTTP proxy the endpoint is reached through (the mesh sidecar).
    proxy: Option<String>,
}

impl DataLayer {
    pub fn new(endpoint_url: impl Into<String>) -> Self {
        Self {
            endpoint_url: endpoint_url.into().trim_end_matches('/').to_owned(),
            proxy: None,
        }
    }

    /// The datalayer behind an `s3` alias, keeping the alias's proxy.
    pub fn from_alias(alias: &Alias) -> Self {
        Self {
            proxy: alias.proxy().map(str::to_owned),
            ..Self::new(alias.to_http_path(""))
        }
    }

    pub fn endpoint_url(&self) -> &str {
        &self.endpoint_url
    }

    /// A store rooted at the grant's key, path-style addressed.
    pub fn store(&self, grant: &Grant) -> Result<Arc<GrantStore>, object_store::Error> {
        arkitekt::fakts::install_crypto_provider();
        let s3 = AmazonS3Builder::new()
            .with_endpoint(&self.endpoint_url)
            .with_allow_http(self.endpoint_url.starts_with("http://"))
            .with_virtual_hosted_style_request(false)
            .with_region("us-east-1")
            .with_bucket_name(&grant.bucket)
            .with_access_key_id(&grant.access_key)
            .with_secret_access_key(&grant.secret_key)
            .with_token(&grant.session_token)
            .with_client_options(match &self.proxy {
                Some(proxy) => ClientOptions::new().with_proxy_url(proxy),
                None => ClientOptions::new(),
            })
            .build()?;
        let prefixed = PrefixStore::new(s3, grant.key.as_str());
        Ok(Arc::new(AsyncObjectStore::new(prefixed)))
    }
}
