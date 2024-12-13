use std::sync::Arc;

use super::api;
use super::client::MikroClient;
use anyhow::Error;
use graphql_client::GraphQLQuery;
use graphql_client::Response;
use ndarray::Array;
use ndarray::Array5;
use ndarray_rand::rand::SeedableRng;
use ndarray_rand::rand_distr::Uniform;
use ndarray_rand::RandomExt;
use object_store::aws::AmazonS3Builder;
use rand_isaac::isaac64::Isaac64Rng;
use zarrs::array::codec::GzipCodec;
use zarrs::array::ArrayBuilder;
use zarrs::array::DataType;
use zarrs::array::FillValue;
use zarrs::filesystem::FilesystemStore;
use zarrs::group::GroupBuilder;
use zarrs::{array_subset::ArraySubset, node::Node};
use zarrs_object_store::object_store::ObjectStore;
use zarrs_object_store::AsyncObjectStore;
use zarrs_storage::AsyncReadableWritableListableStorage;

pub async fn create_image(
    mikro: MikroClient,
    array: Array5<u32>,
    name: String,
) -> Result<Response<api::from_array_like::ResponseData>, Error> {
    let key = uuid::Uuid::new_v4().to_string();

    let credentials_request: graphql_client::QueryBody<api::request_upload::Variables> =
        api::RequestUpload::build_query(api::request_upload::Variables {
            input: api::request_upload::RequestUploadInput {
                key: key,
                datalayer: "default".to_string(),
            },
        });

    let response = mikro.request(&credentials_request).send().await?;

    println!("Response: {:?}", response);
    let body: Response<api::request_upload::ResponseData> = response.json().await.map_err(|e| {
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

    println!("Creating a new Zarr V3 array in the object store");
    // Write the root group metadata
    zarrs::group::GroupBuilder::new()
        .build(
            store.clone(),
            format!("/{}", credentials.key.as_str()).as_str(),
        )?
        .async_store_metadata()
        .await?;

    println!("Created a new Zarr V3 array in the object store");
    // Create a new V3 array using the array builder
    let zarrarray = ArrayBuilder::new(
        array
            .shape()
            .iter()
            .map(|&dim| dim as u64)
            .collect::<Vec<u64>>(), // array shape
        // array shape
        DataType::UInt32,
        vec![1, 1, 1, 1000, 1000].try_into()?, // regular chunk shape (non-zero elements)
        FillValue::from(0),
    )
    .bytes_to_bytes_codecs(vec![Arc::new(GzipCodec::new(5)?)])
    .dimension_names(["c", "t", "z", "y", "x"].into())
    .attributes(
        serde_json::json!({"Zarr V3": "is great"})
            .as_object()
            .unwrap()
            .clone(),
    )
    .build(
        store.clone(),
        format!("/{}/data", credentials.key.as_str()).as_str(),
    )
    .unwrap();

    zarrarray.async_store_metadata().await?;

    println!("Attributes: {:?}", zarrarray.attributes());

    zarrarray
        .async_store_array_subset_ndarray(
            &[1, 1, 1, 1, 1], // array index (start of subset)
            array,
        )
        .await?;

    println!("Uploaded new Zarr V3 array in the object store");

    let from_array_like_request: graphql_client::QueryBody<api::from_array_like::Variables> =
        api::FromArrayLike::build_query(api::from_array_like::Variables {
            input: api::from_array_like::FromArrayLikeInput {
                array: credentials.store,
                name: name,
                dataset: None,
                acquisition_views: None,
                channel_views: None,
                transformation_views: None,
                pixel_views: None,
                structure_views: None,
                rgb_views: None,
                timepoint_views: None,
                optics_views: None,
                roi_views: None,
                file_views: None,
                tags: None,
                scale_views: None,
                derived_views: None,
            },
        });

    let response = mikro.request(&from_array_like_request).send().await?;
    let body: Response<api::from_array_like::ResponseData> =
        response.json().await.map_err(|e: reqwest::Error| {
            println!("Deserialization error: {}", e);
            e
        })?;
    return Ok(body);
}
