use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct AgentFakt {
    pub endpoint_url: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct RekuestFakt {
    pub endpoint_url: String,
    pub agent: AgentFakt,
}
