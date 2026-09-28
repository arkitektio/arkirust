//! Typed lovekit operations, generated from `graphql/operations.graphql`
//! against the schema shipped with the Python `lovekit` package.

#![allow(clippy::all, non_camel_case_types, dead_code)]

use graphql_client::GraphQLQuery;

pub type ID = String;
pub type DateTime = String;

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

operation!(EnsureSoloBroadcast);
operation!(EnsureStream);
operation!(JoinBroadcast);
operation!(GetStream);
operation!(ListStreams);
operation!(GetSoloBroadcast);
operation!(ListSoloBroadcasts);
operation!(GetCollaborativeBroadcast);
operation!(ListCollaborativeBroadcasts);
