use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct MikroFakt {
    pub endpoint_url: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct DatalayerFakt {
    pub endpoint_url: String,
}
