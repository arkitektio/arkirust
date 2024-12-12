use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug)]
pub struct InitialAgentMessage {
    #[serde(rename = "type")]
    pub type_: String,
    pub instance_id: String,
    pub token: String,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct AssignationEventMessage {
    #[serde(rename = "type")]
    pub type_: String,
    pub assignation: i64,
    pub kind: String,
    pub message: Option<String>,
    pub returns: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct HeartbeatResponseMessage {
    #[serde(rename = "type")]
    pub type_: String,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Provision {
    pub id: String,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Inquiry {
    pub id: String,
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(tag = "type")]
pub enum AgentMessage {
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
