mod fakts;
mod rekuest;
mod unlok;

use std::vec;

use fakts::fakts_protocol::Manifest;
use fakts::fakts_protocol::Requirement;
use fakts::funcs::register_client;
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

use graphql_client::GraphQLQuery;
use graphql_client::Response;

use serde::{Deserialize, Serialize};
use unlok::client::UnlokClient;
use unlok::fakt::UnlokFakt;
use unlok::token::get_auth_token;

#[derive(Debug, Deserialize, Serialize)]
struct ExampleFuncArgs {
    port: i64,
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

    create_agent(
        &rekuest,
        "default",
        "My beautiful rust agent",
        vec!["default"],
    )
    .await?;

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
