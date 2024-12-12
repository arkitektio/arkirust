#[derive(Deserialize, Serialize, Debug)]
struct AgentFakt {
    endpoint_url: String,
}

#[derive(Deserialize, Serialize, Debug)]
struct RekuestFakt {
    endpoint_url: String,
    agent: AgentFakt,
}
