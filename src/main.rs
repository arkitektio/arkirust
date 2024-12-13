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
use mikro::upload::create_image;
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
use zarrs::array::ZARR_NAN_F64;
use zarrs_object_store::object_store::ObjectStore;

use graphql_client::GraphQLQuery;
use graphql_client::Response;

use futures::StreamExt;
use mikro::fakt::MikroFakt;
use ndarray::Array;
use ndarray_rand::rand::SeedableRng;
use ndarray_rand::rand_distr::Uniform;
use ndarray_rand::RandomExt;
use rand_isaac::isaac64::Isaac64Rng;
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
    name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ExampleFuncReturns {
    image: String,
}

async fn example_func(app: App, args: String) -> String {
    let args = serde_json::from_str::<ExampleFuncArgs>(&args).unwrap();

    let mut rng = Isaac64Rng::seed_from_u64(42);

    let shape = (1, 1, 1, 1000, 1000);
    let array = Array::random_using(shape, Uniform::new(0, 100), &mut rng);

    let image = create_image(app.mikro, array, args.name).await.unwrap();

    println!("Image: {:?}", image);
    let returns = ExampleFuncReturns {
        image: image.data.unwrap().from_array_like.id,
    };
    serde_json::to_string(&returns).unwrap()
}

#[derive(Deserialize, Serialize, Debug)]
struct ExpectedFakts {
    unlok: UnlokFakt,
    rekuest: RekuestFakt,
    mikro: MikroFakt,
}

struct App {
    rekuest: RekuestClient,
    unlok: UnlokClient,
    mikro: MikroClient,
}
impl Clone for App {
    fn clone(&self) -> Self {
        App {
            rekuest: self.rekuest.clone(),
            unlok: self.unlok.clone(),
            mikro: self.mikro.clone(),
        }
    }
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

    let app = App {
        rekuest: rekuest,
        unlok: unlok,
        mikro: mikro,
    };

    create_agent(
        &app.rekuest,
        "default",
        "My beautiful rust agent",
        vec!["default"],
    )
    .await?;

    let function_def = Definition::new("Create Rusty image", NodeKind::FUNCTION)
        .description("Creates a really rusty image (unfortunatly only zarr v3")
        .args(vec![Port::new_string("name").build()])
        .returns(vec![Port::new_structure("image", "@mikro/image").build()])
        .build();

    let template_input = create_template::TemplateInput {
        definition: function_def,
        interface: "rusty-image".to_string(),
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

    let mut res = app.rekuest.request(&create_first_template).send().await?;

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

    let _ = provide_forever(fakts.rekuest, token, registry, app).await?;

    Ok(())
}
