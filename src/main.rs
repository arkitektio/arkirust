use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use anyhow;
use arkirust::json_types;
use futures::{SinkExt, StreamExt};
use graphql_client::GraphQLQuery;
use graphql_client::Response;
use oauth2::basic::BasicClient;
use oauth2::http::{request, response};
use oauth2::reqwest::async_http_client; // Use the provided async HTTP client function
use oauth2::{AuthUrl, ClientId, ClientSecret, Scope, TokenResponse, TokenUrl};
use serde::{Deserialize, Serialize};
use tokio::pin;
#[derive(Serialize)]
struct Requirement {
    key: String,
    service: String,
    optional: bool,
}

#[derive(Serialize)]
struct Manifest {
    identifier: String,
    version: String,
    scopes: Vec<String>,
    requirements: Vec<Requirement>,
}

#[derive(Serialize)]
struct DeviceCodeStartRequest {
    manifest: Manifest,
    requested_client_kind: String,
}

#[derive(Serialize)]
struct DeviceCodeChallengeRequest {
    code: String,
}

#[derive(Deserialize, Serialize)]
struct DeviceCodeAnswer {
    code: String,
    status: String,
}

#[derive(Deserialize, Serialize)]
struct DeviceCodeChallengeAnswer {
    status: String,
    token: Option<String>,
}

#[derive(Deserialize, Serialize, Debug)]
struct UnlokFakt {
    authorization_url: String,
    base_url: String,
    client_id: String,
    client_secret: String,
    endpoint_url: String,
    name: String,
    scopes: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug)]
struct AgentFakt {
    endpoint_url: String,
}

#[derive(Deserialize, Serialize, Debug)]
struct RekuestFakt {
    endpoint_url: String,
    agent: AgentFakt,
}

#[derive(Deserialize, Serialize, Debug)]
struct ExpectedFakts {
    unlok: UnlokFakt,
    rekuest: RekuestFakt,
}

#[derive(Deserialize, Serialize, Debug)]
struct FaktsAnswer {
    config: ExpectedFakts,
}

#[derive(Deserialize, Serialize, Debug)]
struct RetrieveRequest {
    token: String,
}

#[derive(Deserialize, Serialize, Debug)]
struct TokenConfig {
    token: String,
}

#[derive(Deserialize, Serialize, Debug)]
struct InitialAgentMessage {
    #[serde(rename = "type")]
    type_: String,
    instance_id: String,
    token: String,
}

#[derive(Deserialize, Serialize, Debug)]
struct AssignationEventMessage {
    #[serde(rename = "type")]
    type_: String,
    assignation: i64,
    kind: String,
    message: Option<String>,
    returns: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Deserialize, Serialize, Debug)]
struct HeartbeatResponseMessage {
    #[serde(rename = "type")]
    type_: String,
}

#[derive(Deserialize, Serialize, Debug)]
struct Provision {
    id: String,
}

#[derive(Deserialize, Serialize, Debug)]
struct Inquiry {
    id: String,
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(tag = "type")]
enum AgentMessage {
    #[serde(rename = "HEARTBEAT")]
    Heartbeat,
    #[serde(rename = "INIT")]
    Initial {
        instance_id: String,
        agent: String,
        registry: String,
        provisions: Vec<Provision>,
        inquiries: Vec<Inquiry>,
    },
    #[serde(rename = "ASSIGN")]
    Assign {
        assignation: i64,
        args: HashMap<String, serde_json::Value>,
        provision: i64,
    },
    #[serde(rename = "PROVIDE")]
    Provide { provision: i64 },
    #[serde(rename = "UNPROVIDE")]
    Unprovide {},
    #[serde(rename = "ERROR")]
    Error { code: i64 },
}

async fn get_saved_token() -> Result<Option<String>, Box<dyn std::error::Error>> {
    // read save token form token.json
    let token_path = std::path::Path::new("token.json");
    // parse token from json according to a struct with token field
    if !token_path.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(token_path)?;
    let token_data: TokenConfig = serde_json::from_str(&contents)?;
    Ok(Some(token_data.token))
}

async fn claim_fakts(token: String) -> Result<ExpectedFakts, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let retrieve_response = client
        .post("http://127.0.0.1/lok/f/claim/")
        .json(&RetrieveRequest {
            token: token.clone(),
        })
        .send()
        .await?;

    let body = retrieve_response.text().await?;

    let fakts_answer: FaktsAnswer = match serde_json::from_str(&body) {
        Ok(answer) => answer,
        Err(e) => {
            println!("Failed to deserialize response: {}", body);
            return Err(Box::new(e));
        }
    };

    println!("Response from register_client: {:?}", fakts_answer);
    return Ok(fakts_answer.config);
}

async fn register_client() -> Result<ExpectedFakts, Box<dyn std::error::Error>> {
    // Try to retrive from saved token (if any)

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

    let token = get_saved_token().await?;
    if let Some(token) = token {
        match claim_fakts(token).await {
            Ok(fakts) => return Ok(fakts),
            Err(_) => (), // Continue with rest of function if error occurs
        };
    }

    let request = DeviceCodeStartRequest {
        manifest: manifest,
        requested_client_kind: "development".to_string(),
    };

    let client = reqwest::Client::new();
    let res = client
        .post("http://127.0.0.1/lok/f/start/")
        .json(&request)
        .send()
        .await?;

    let body = res.text().await?;

    // Parse the response body into a DeviceCodeAnswer struct
    let device_code_answer: DeviceCodeAnswer = serde_json::from_str(&body)?;

    println!("Response from register_client: http://127.0.0.1/lok/f/configure/?grant=device_code&device_code={}", device_code_answer.code);

    // Check if the challenge has been accepted for a while
    // and if the status is still pending

    let mut counter = 0;

    let challenge = DeviceCodeChallengeRequest {
        code: device_code_answer.code,
    };

    let mut token: Option<String> = None;

    loop {
        let res = client
            .post("http://127.0.0.1/lok/f/challenge/")
            .json(&challenge)
            .send()
            .await?;

        let body = res.text().await?;

        let device_code_answer: DeviceCodeChallengeAnswer = match serde_json::from_str(&body) {
            Ok(answer) => answer,
            Err(e) => {
                println!("Failed to deserialize response: {}", body);
                return Err(Box::new(e));
            }
        };

        if device_code_answer.status == "granted" {
            token = Some(device_code_answer.token.unwrap());
            break;
        }

        // Sleep for a while
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        counter += 1;

        if counter > 10 {
            break;
        }
    }

    // Check if token is None
    let token = match token {
        Some(token) => token,
        None => panic!("Token is None"),
    };

    // Save token to token.json
    let token_path = std::path::Path::new("token.json");

    let token_data = TokenConfig {
        token: token.clone(),
    };
    let token_json = serde_json::to_string(&token_data)?;

    std::fs::write(token_path, token_json)?;

    claim_fakts(token).await
}

async fn get_auth_token(config: UnlokFakt) -> Result<String, Box<dyn std::error::Error>> {
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

async fn loop_forever(
    config: RekuestFakt,
    token: String,
    registry: FunctionRegistry,
    client: reqwest::Client,
) -> Result<String, Box<dyn std::error::Error>> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(config.agent.endpoint_url).await?;
    let (write, read) = ws_stream.split();

    // Create a channel for sending messages
    let (msg_tx, mut msg_rx) = tokio::sync::mpsc::channel::<String>(100);
    let msg_tx_clone = msg_tx.clone();

    // Spawn a task to handle the message queue
    let queue_task = tokio::spawn(async move {
        let mut write = write;

        let init = InitialAgentMessage {
            type_: "INITIAL".to_string(),
            instance_id: "default".to_string(),
            token: token.clone(),
        };

        let msg =
            tokio_tungstenite::tungstenite::Message::Text(serde_json::to_string(&init).unwrap());

        if let Err(e) = write.send(msg).await {
            println!("Error sending message: {}", e);
            return;
        }

        while let Some(msg) = msg_rx.recv().await {
            println!("Sending message: {}", msg);
            if let Err(e) = write
                .send(tokio_tungstenite::tungstenite::Message::Text(msg))
                .await
            {
                println!("Error sending queued message: {}", e);
                break;
            }
        }
    });

    let receive_task = tokio::spawn(async move {
        let mut read = read;
        while let Some(msg) = read.next().await {
            match msg {
                Ok(msg) => {
                    let msg: AgentMessage = match serde_json::from_str(msg.to_text().unwrap()) {
                        Ok(msg) => msg,
                        Err(e) => {
                            println!("Failed to deserialize message: {} {}", e, msg);
                            break;
                        }
                    };

                    match msg {
                        AgentMessage::Heartbeat => {
                            let heartbeat_response = HeartbeatResponseMessage {
                                type_: "HEARTBEAT".to_string(),
                            };
                            msg_tx_clone
                                .send(serde_json::to_string(&heartbeat_response).unwrap())
                                .await
                                .unwrap();

                            println!("Received heartbeat");
                        }

                        AgentMessage::Initial { instance_id, .. } => {
                            println!("Received initial message: {} ", instance_id);
                        }

                        AgentMessage::Assign {
                            provision,
                            args,
                            assignation,
                            ..
                        } => {
                            println!("Received assignment: {}", provision);

                            let get_provision =
                                GetProvision::build_query(get_provision::Variables {
                                    id: provision.to_string(),
                                });

                            let mut res = client
                                .post(config.endpoint_url.clone())
                                .json(&get_provision)
                                .send()
                                .await;

                            let response_body: Response<get_provision::ResponseData> =
                                res.unwrap().json().await.unwrap();

                            let template = response_body.data.unwrap().provision.template.id;
                            match registry.get_function(template.as_str()) {
                                Some(func) => {
                                    let returns = func(serde_json::to_string(&args).unwrap());
                                    pin!(returns);

                                    let x = returns.await;

                                    let event = AssignationEventMessage {
                                        type_: "ASSIGNATION_EVENT".to_string(),
                                        assignation: assignation,
                                        kind: "YIELD".to_string(),
                                        message: None,
                                        returns: Some(serde_json::from_str(&x).unwrap()),
                                    };
                                    msg_tx
                                        .send(serde_json::to_string(&event).unwrap())
                                        .await
                                        .unwrap();

                                    let event = AssignationEventMessage {
                                        type_: "ASSIGNATION_EVENT".to_string(),
                                        assignation: assignation,
                                        kind: "DONE".to_string(),
                                        message: None,
                                        returns: None,
                                    };

                                    msg_tx
                                        .send(serde_json::to_string(&event).unwrap())
                                        .await
                                        .unwrap();
                                }
                                None => {
                                    println!("Function not found: {}", template);
                                }
                            };
                        }

                        AgentMessage::Provide { provision } => {
                            println!("Received provision: {}", provision);
                        }

                        AgentMessage::Unprovide {} => {
                            println!("Received unprovide");
                        }

                        AgentMessage::Error { code } => {
                            println!("Received error: {}", code);
                        }
                    }
                }
                Err(e) => {
                    println!("Error receiving message: {}", e);
                    break;
                }
            }
        }
    });

    // Wait for both tasks
    let _ = tokio::try_join!(queue_task, receive_task)?;
    Ok("Connection closed".to_string())
}

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

type InstanceId = String;
type SearchQuery = String;
type ValidatorFunction = String;
type AnyDefault = String;
type NodeHash = String;
type Identifier = String;

// The paths are relative to the directory where your `Cargo.toml` is located.
// Both json and the GraphQL schema language are supported as sources for the schema
#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/rekuest/schema.graphql",
    query_path = "graphql/rekuest/create_agent.graphql",
    response_derives = "Debug,Clone"
)]
pub struct EnsureAgent;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/rekuest/schema.graphql",
    query_path = "graphql/rekuest/create_template.graphql",
    response_derives = "Debug,Clone",
    variables_derives = "Clone",
    derives = "Clone"
)]
pub struct CreateTemplate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/rekuest/schema.graphql",
    query_path = "graphql/rekuest/get_provision.graphql",
    response_derives = "Debug,Clone",
    variables_derives = "Clone",
    derives = "Clone"
)]
pub struct GetProvision;

struct FunctionRegistry {
    functions: HashMap<
        String,
        Box<dyn Fn(String) -> Pin<Box<dyn Future<Output = String> + Send>> + Send + Sync>,
    >,
    templates: HashMap<String, create_template::TemplateInput>,
}

impl FunctionRegistry {
    fn new() -> Self {
        Self {
            functions: HashMap::new(),
            templates: HashMap::new(),
        }
    }

    fn register<F, Fut>(
        &mut self,
        name: &str,
        function: F,
        template: create_template::TemplateInput,
    ) where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = String> + Send + 'static,
    {
        // Wrap the given function into one returning a boxed, pinned future
        let wrapped = move |input: String| -> Pin<Box<dyn Future<Output = String> + Send>> {
            Box::pin(function(input))
        };

        self.functions.insert(name.to_string(), Box::new(wrapped));
        self.templates.insert(name.to_string(), template);
    }

    fn get_function(
        &self,
        name: &str,
    ) -> Option<&Box<dyn Fn(String) -> Pin<Box<dyn Future<Output = String> + Send>> + Send + Sync>>
    {
        self.functions.get(name)
    }

    fn get_template(&self, name: &str) -> Option<&create_template::TemplateInput> {
        self.templates.get(name)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // fakts

    let fakts = register_client().await?;
    println!("Response from register_client: {:?}", fakts);

    // token
    let token = get_auth_token(fakts.unlok).await?;
    println!("Access token: {:?}", token);

    let request = EnsureAgent::build_query(ensure_agent::Variables {
        input: ensure_agent::AgentInput {
            instance_id: "default".to_string(),
            name: Some("my-agent".to_string()),
            extensions: Some(Vec::new()),
        },
    });

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

    let mut res = client
        .post(fakts.rekuest.endpoint_url.clone())
        .json(&request)
        .send()
        .await?;

    let template_input = create_template::TemplateInput {
        definition: create_template::DefinitionInput {
            description: Some("My template".to_string()),
            name: "my-template".to_string(),
            args: vec![create_template::PortInput {
                key: "port".to_string(),
                default: None,
                scope: create_template::PortScope::GLOBAL,
                kind: create_template::PortKind::INT,
                children: Some(vec![]),
                description: Some("Port number".to_string()),
                groups: Some(vec!["default".to_string()]),
                effects: Some(vec![]),
                label: Some("Port".to_string()),
                assign_widget: Box::new(None),
                identifier: None,
                nullable: false,
                return_widget: None,
                validators: Some(vec![]),
            }],
            kind: create_template::NodeKind::FUNCTION,
            port_groups: vec![],
            stateful: false,
            is_dev: false,
            is_test_for: vec![],
            interfaces: vec![],
            returns: vec![create_template::PortInput {
                key: "port".to_string(),
                default: None,
                scope: create_template::PortScope::GLOBAL,
                kind: create_template::PortKind::INT,
                children: Some(vec![]),
                description: Some("Port number".to_string()),
                groups: Some(vec!["default".to_string()]),
                effects: Some(vec![]),
                label: Some("Port".to_string()),
                assign_widget: Box::new(None),
                identifier: None,
                nullable: false,
                return_widget: None,
                validators: Some(vec![]),
            }],
            collections: vec![],
        },
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

    let response_body: Response<ensure_agent::ResponseData> = res.json().await?;
    println!("{:#?}", response_body);

    let mut res = client
        .post(fakts.rekuest.endpoint_url.clone())
        .json(&create_first_template)
        .send()
        .await?;

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

    let _ = loop_forever(fakts.rekuest, token, registry, client).await?;

    Ok(())
}
