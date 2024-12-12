use graphql_client::GraphQLQuery;

type InstanceId = String;
type SearchQuery = String;
type ValidatorFunction = String;
type AnyDefault = String;
type NodeHash = String;
type Identifier = String;

// The paths are relative to the directory where your `Cargo.toml` is located.
// Both json and the GraphQL schema language are supported as sources for the schema
#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/rekuest/schema.graphql",
    query_path = "graphql/rekuest/create_agent.graphql",
    response_derives = "Debug,Clone"
)]
pub struct EnsureAgent;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/rekuest/schema.graphql",
    query_path = "graphql/rekuest/create_template.graphql",
    response_derives = "Debug,Clone",
    variables_derives = "Clone",
    derives = "Clone"
)]
pub struct CreateTemplate;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/rekuest/schema.graphql",
    query_path = "graphql/rekuest/get_provision.graphql",
    response_derives = "Debug,Clone",
    variables_derives = "Clone",
    derives = "Clone"
)]
pub struct GetProvision;
