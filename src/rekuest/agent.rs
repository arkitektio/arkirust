use crate::fakts;
use crate::App;

use super::agent_protocol::*;
use super::api::ensure_agent;
use super::api::get_provision;
use super::api::EnsureAgent;
use super::api::GetProvision;
use super::client::RekuestClient;
use super::fakt::RekuestFakt;
use super::registry::FunctionRegistry;
use futures::{SinkExt, StreamExt};
use graphql_client::GraphQLQuery;
use graphql_client::Response;
use tokio::pin;

pub async fn create_agent(
    client: &RekuestClient,
    instance_id: &str,
    name: &str,
    extensions: Vec<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = EnsureAgent::build_query(ensure_agent::Variables {
        input: ensure_agent::AgentInput {
            instance_id: instance_id.to_string(),
            name: Some(name.to_string()),
            extensions: Some(extensions.iter().map(|s| s.to_string()).collect()),
        },
    });

    client.request(&request).send().await?;

    Ok(())
}

pub async fn provide_forever(
    config: RekuestFakt,
    token: String,
    registry: FunctionRegistry,
    app: App,
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

                            let res = app.clone().rekuest.request(&get_provision).send().await;

                            let response_body: Response<get_provision::ResponseData> =
                                res.unwrap().json().await.unwrap();

                            let template = response_body.data.unwrap().provision.template.id;
                            match registry.get_function(template.as_str()) {
                                Some(func) => {
                                    let returns =
                                        func((app.clone(), serde_json::to_string(&args).unwrap()));
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
