use graphql_client::GraphQLQuery;

type FourByFourMatrix = String;
type Milliseconds = String;
type StructureString = String;
type DateTime = String;
type ArrayLike = String;

// The paths are relative to the directory where your `Cargo.toml` is located.
// Both json and the GraphQL schema language are supported as sources for the schema
#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/mikro/schema.graphql",
    query_path = "graphql/mikro/image.graphql",
    response_derives = "Debug,Clone"
)]
pub struct FromArrayLike;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/mikro/schema.graphql",
    query_path = "graphql/mikro/image.graphql",
    response_derives = "Debug,Clone",
    variables_derives = "Clone",
    derives = "Clone"
)]
pub struct RequestUpload;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/mikro/schema.graphql",
    query_path = "graphql/mikro/image.graphql",
    response_derives = "Debug,Clone",
    variables_derives = "Clone",
    derives = "Clone"
)]
pub struct RequestAccess;
