mod fakts;
mod mikro;
mod rekuest;
mod unlok;

use std::path::PathBuf;
use std::sync::Arc;
use std::vec;

use fakts::fakts_protocol::Manifest;
use fakts::fakts_protocol::Requirement;
use fakts::funcs::register_client;
use mikro::api::request_upload::RequestUploadInput;
use mikro::client::MikroClient;
use object_store::aws::AmazonS3Builder;
use rekuest::agent::create_agent;
use rekuest::agent::provide_forever;
use rekuest::api::create_template;
use rekuest::api::create_template::DefinitionInput;
use rekuest::api::create_template::NodeKind;
use rekuest::api::ensure_agent;
use rekuest::api::CreateTemplate;
use rekuest::api::EnsureAgent;
use rekuest::client::RekuestClient;
use rekuest::definition::Definition;
use rekuest::fakt::RekuestFakt;
use rekuest::ports::Port;
use rekuest::registry::FunctionRegistry;
use zarrs::array::ArrayBuilder;
use zarrs_object_store::object_store::ObjectStore;

use graphql_client::GraphQLQuery;
use graphql_client::Response;

use futures::StreamExt;
use mikro::fakt::MikroFakt;
use serde::{Deserialize, Serialize};
use unlok::client::UnlokClient;
use unlok::fakt::UnlokFakt;
use unlok::token::get_auth_token;
use zarrs::array::codec::GzipCodec;
use zarrs::filesystem::FilesystemStore;
use zarrs::group::GroupBuilder;
use zarrs::{
    array::{DataType, FillValue, ZARR_NAN_F32},
    array_subset::ArraySubset,
    node::Node,
};
use zarrs_object_store::AsyncObjectStore;
use zarrs_storage::AsyncReadableWritableListableStorage;
#[derive(Debug, Deserialize, Serialize)]
struct ExampleFuncArgs {
    name: i64,
}

#[derive(Debug, Deserialize, Serialize)]
struct ExampleFuncReturns {
    port: i64,
}

async fn example_func(args: String) -> String {
    let args = serde_json::from_str::<ExampleFuncArgs>(&args).unwrap();

    println!("Received args: {:?}", args);
    let returns = ExampleFuncReturns {
        port: args.port + 1,
    };
    serde_json::to_string(&returns).unwrap()
}

#[derive(Deserialize, Serialize, Debug)]
struct ExpectedFakts {
    unlok: UnlokFakt,
    rekuest: RekuestFakt,
    mikro: MikroFakt,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // fakts

    let manifest = Manifest {
        identifier: "my-app".to_string(),
        version: "0.1.0".to_string(),
        scopes: vec!["read".to_string()],
        requirements: vec![
            Requirement {
                key: "unlok".to_string(),
                service: "live.arkitekt.lok".to_string(),
                optional: false,
            },
            Requirement {
                key: "rekuest".to_string(),
                service: "live.arkitekt.rekuest".to_string(),
                optional: false,
            },
            Requirement {
                key: "mikro".to_string(),
                service: "live.arkitekt.mikro".to_string(),
                optional: false,
            },
        ],
    };

    let fakts: ExpectedFakts = register_client(manifest).await?;
    println!("Response from register_client: {:?}", fakts);

    // token
    let token = get_auth_token(fakts.unlok.clone()).await?;
    println!("Access token: {:?}", token);

    // rekuest
    let rekuest = RekuestClient::new(fakts.rekuest.clone(), &token).unwrap();
    let unlok = UnlokClient::new(fakts.unlok.clone(), &token).unwrap();
    let mikro = MikroClient::new(fakts.mikro.clone(), &token).unwrap();

    create_agent(
        &rekuest,
        "default",
        "My beautiful rust agent",
        vec!["default"],
    )
    .await?;

    let key = uuid::Uuid::new_v4().to_string();

    let credentials_request: graphql_client::QueryBody<mikro::api::request_upload::Variables> =
        mikro::api::RequestUpload::build_query(mikro::api::request_upload::Variables {
            input: mikro::api::request_upload::RequestUploadInput {
                key: key,
                datalayer: "default".to_string(),
            },
        });

    let response = mikro.request(&credentials_request).send().await?;

    println!("Response: {:?}", response);
    let body: Response<mikro::api::request_upload::ResponseData> =
        response.json().await.map_err(|e| {
            println!("Deserialization error: {}", e);
            e
        })?;

    println!("Response body: {:#?}", body);

    let credentials = body.data.unwrap().request_upload;

    let cloned = credentials.clone();

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
    let array = ArrayBuilder::new(
        vec![3, 4], // array shape
        DataType::Float32,
        vec![2, 2].try_into()?, // regular chunk shape (non-zero elements)
        FillValue::from(ZARR_NAN_F32),
    )
    .bytes_to_bytes_codecs(vec![Arc::new(GzipCodec::new(5)?)])
    .dimension_names(["y", "x"].into())
    .attributes(
        serde_json::json!({"Zarr V3": "is great"})
            .as_object()
            .unwrap()
            .clone(),
    )
    .build(
        store.clone(),
        format!("/{}/data", credentials.key.as_str()).as_str(),
    )?
    .async_store_metadata()
    .await?; // /path/to/hierarchy.zarr/array

    let from_array_like_request: graphql_client::QueryBody<mikro::api::from_array_like::Variables> =
        mikro::api::FromArrayLike::build_query(mikro::api::from_array_like::Variables {
            input: mikro::api::from_array_like::FromArrayLikeInput {
                array: credentials.store,
                name: "my-array".to_string(),
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
    let body: Response<mikro::api::from_array_like::ResponseData> =
        response.json().await.map_err(|e: reqwest::Error| {
            println!("Deserialization error: {}", e);
            e
        })?;

    println!("Response body: {:#?}", body);

    let function_def = Definition::new("my-app", NodeKind::FUNCTION)
        .args(vec![Port::new_int("port").build()])
        .returns(vec![Port::new_int("port").build()])
        .build();

    let template_input = create_template::TemplateInput {
        definition: function_def,
        interface: "my-agent".to_string(),
        dependencies: Vec::new(),
        logo: None,
        params: None,
        dynamic: false,
    };

    let tmp_copy = template_input.clone();

    let create_template_input = create_template::CreateTemplateInput {
        template: template_input,
        extension: "default".to_string(),
        instance_id: "default".to_string(),
    };

    let create_first_template = CreateTemplate::build_query(create_template::Variables {
        input: create_template_input,
    });

    let mut res = rekuest.request(&create_first_template).send().await?;

    let mut registry = FunctionRegistry::new();

    let response_body: Response<create_template::ResponseData> = res.json().await?;

    registry.register(
        response_body
            .data
            .clone()
            .unwrap()
            .create_template
            .id
            .as_str(),
        example_func,
        tmp_copy,
    );
    println!("{:#?}", response_body);

    let _ = provide_forever(fakts.rekuest, token, registry, rekuest).await?;

    Ok(())
}
