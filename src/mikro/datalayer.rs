use std::sync::Arc;

use super::api;
use super::fakt::DatalayerFakt;
use super::fakt::MikroFakt;
use anyhow::Error;
use graphql_client::GraphQLQuery;
use graphql_client::QueryBody;
use graphql_client::Response;
use object_store::aws::AmazonS3Builder;
use reqwest::Client;
use zarrs_storage::AsyncReadableWritableListableStorage;

pub type MikroClientFunc = reqwest::RequestBuilder;

pub struct DatalayerClient {
    client: Client,
    endpoint_url: String,
    datalayer_fakt: DatalayerFakt,
}

pub struct DatalayerStore {
    pub zarr_store: AsyncReadableWritableListableStorage,
    pub key: String,
    pub store_id: String,
}

impl DatalayerClient {
    pub fn new(fakt: MikroFakt, datalayer_fakt: DatalayerFakt, token: &str) -> Result<Self, Error> {
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
            datalayer_fakt: datalayer_fakt,
        })
    }

    pub fn request<T: serde::Serialize>(&self, body: &QueryBody<T>) -> MikroClientFunc {
        self.client.post(&self.endpoint_url).json(body)
    }

    pub async fn get_object_store(&self) -> Result<DatalayerStore, Error> {
        let key = uuid::Uuid::new_v4().to_string();

        let credentials_request: graphql_client::QueryBody<api::request_upload::Variables> =
            api::RequestUpload::build_query(api::request_upload::Variables {
                input: api::request_upload::RequestUploadInput {
                    key: key,
                    datalayer: "default".to_string(),
                },
            });

        let response: reqwest::Response = self.request(&credentials_request).send().await?;

        println!("Response: {:?}", response);
        let body: Response<api::request_upload::ResponseData> =
            response.json().await.map_err(|e| {
                println!("Deserialization error: {}", e);
                e
            })?;

        println!("Response body: {:#?}", body);

        let credentials = body.data.unwrap().request_upload;

        let object_store = AmazonS3Builder::new()
            .with_allow_http(true)
            .with_bucket_name(credentials.bucket)
            .with_endpoint(format!("http://127.0.0.1").as_str())
            .with_access_key_id(credentials.access_key)
            .with_secret_access_key(credentials.secret_key)
            .with_token(credentials.session_token)
            .build()?;

        println!("Creating a new S3 object store");

        let store: AsyncReadableWritableListableStorage =
            Arc::new(zarrs_object_store::AsyncObjectStore::new(object_store));
        Ok(DatalayerStore {
            zarr_store: store,
            key: credentials.key,
            store_id: credentials.store,
        })
    }
}

impl Clone for DatalayerClient {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            endpoint_url: self.endpoint_url.clone(),
            datalayer_fakt: self.datalayer_fakt.clone(),
        }
    }
}
