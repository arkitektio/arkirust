//! Typed mikro operations, generated from `graphql/operations.graphql`
//! against the schema shipped with the Python `mikro` package.

#![allow(clippy::all, non_camel_case_types, dead_code)]

use graphql_client::GraphQLQuery;

// Custom scalars used by the selected operations.
pub type ID = String;
/// A zarr store id (the `store` of an upload grant).
pub type ArrayLike = String;
pub type JSON = serde_json::Value;
pub type DateTime = String;
pub type UUID = String;
pub type Any = serde_json::Value;
pub type ThreeDVector = Vec<f64>;
pub type Length = serde_json::Value;
pub type Unit = serde_json::Value;
pub type Frequency = serde_json::Value;
pub type GenericQuantity = serde_json::Value;
pub type Power = serde_json::Value;
pub type Temperature = serde_json::Value;

macro_rules! operation {
    ($name:ident) => {
        #[derive(GraphQLQuery)]
        #[graphql(
            schema_path = "graphql/schema.graphql",
            query_path = "graphql/operations.graphql",
            response_derives = "Debug, Clone, PartialEq, Serialize",
            variables_derives = "Debug, Clone"
        )]
        pub struct $name;
    };
}

operation!(RequestZarrUpload);
operation!(FinishZarrUpload);
operation!(RequestZarrAccess);
operation!(CreateArrayDataset);
operation!(GetArrayDataset);
operation!(SearchArrayDatasets);
