use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct MikroFakt {
    pub endpoint_url: String,
}
