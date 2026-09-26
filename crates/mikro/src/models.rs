//! Owned models of what mikro returns, and their structure identities.

use arkitekt::rekuest::{widgets, Context};
use arkitekt::Structure;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::create_array_dataset::{AxisInput, AxisType};
use crate::{Mikro, MikroError};

/// The search query the UI runs to pick an `ArrayDataset` (same as Python's).
pub const SEARCH_ARRAY_DATASETS: &str = "query SearchArrayDatasets($search: String, $values: [ID!], $limit: Int, $offset: Int = 0) {\n  options: arrayDatasets(\n    filters: {search: $search, ids: $values}\n    pagination: {limit: $limit, offset: $offset}\n  ) {\n    value: id\n    label: name\n    __typename\n  }\n}";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZarrStore {
    pub id: String,
    pub key: String,
    pub bucket: String,
    pub path: String,
}

/// One scale level of an array dataset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataArray {
    pub id: String,
    pub level: i64,
    pub shape: Vec<i64>,
    pub chunk_shape: Vec<i64>,
    pub store: ZarrStore,
}

/// An n-dimensional image (or any array) in mikro. Travels as `@mikro/arraydataset`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrayDataset {
    pub id: String,
    pub name: String,
    pub axis_names: Vec<String>,
    pub shape: Vec<i64>,
    pub multiscale: bool,
    pub data_arrays: Vec<DataArray>,
}

impl ArrayDataset {
    /// Convert any generated `ArrayDataset` fragment (each operation has its own type).
    pub(crate) fn from_fragment<F: Serialize>(fragment: &F) -> Result<Self, MikroError> {
        Ok(serde_json::from_value(serde_json::to_value(fragment)?)?)
    }
}

impl Structure for ArrayDataset {
    const IDENTIFIER: &'static str = "@mikro/arraydataset";

    fn structure_id(&self) -> String {
        self.id.clone()
    }

    async fn expand(id: String, ctx: &Context) -> anyhow::Result<Self> {
        let mikro = ctx.require::<Mikro>()?;
        Ok(mikro.get_array_dataset(&id).await?)
    }

    fn widget() -> Option<Value> {
        Some(widgets::search(SEARCH_ARRAY_DATASETS, "mikro"))
    }
}

/// Axes for conventional dimension names: `c` is a channel, `t` time,
/// `z`/`y`/`x` space, anything else an index.
pub fn axes_for(names: &[&str]) -> Vec<AxisInput> {
    names
        .iter()
        .map(|&name| AxisInput {
            name: name.to_owned(),
            type_: match name {
                "c" => AxisType::CHANNEL,
                "t" => AxisType::TIME,
                "z" | "y" | "x" => AxisType::SPACE,
                _ => AxisType::INDEX,
            },
            long_name: None,
            description: None,
        })
        .collect()
}
