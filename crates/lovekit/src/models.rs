//! Owned models of what lovekit returns, and their structure identities.

use arkitekt::rekuest::{widgets, Context};
use arkitekt::Structure;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Lovekit, LovekitError};

/// The search queries the UI runs to pick one (the same as Python's).
pub const SEARCH_STREAMS: &str = "query SearchStreams($search: String, $values: [ID!]) {\n  options: streams(\n    filters: {search: $search, ids: $values}\n    pagination: {limit: 10}\n  ) {\n    value: id\n    label: title\n    __typename\n  }\n}";
pub const SEARCH_SOLO_BROADCASTS: &str = "query SearchSoloBroadcast($search: String, $values: [ID!]) {\n  options: soloBroadcasts(\n    filters: {search: $search, ids: $values}\n    pagination: {limit: 10}\n  ) {\n    value: id\n    label: title\n    __typename\n  }\n}";

/// Video or audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum StreamKind {
    #[default]
    Video,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamerUser {
    pub sub: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamerClient {
    pub client_id: String,
}

/// Who streams: a user through one app client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Streamer {
    pub user: StreamerUser,
    pub client: StreamerClient,
}

/// One published track. Travels as `@lovekit/stream`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stream {
    pub id: String,
    pub title: String,
    pub kind: StreamKind,
}

/// A streamer's own broadcast (one per app instance). Travels as
/// `@lovekit/solo_broadcast`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SoloBroadcast {
    pub id: String,
    pub title: String,
    pub streamer: Streamer,
}

/// A broadcast several streamers publish into.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollaborativeBroadcast {
    pub id: String,
    pub title: String,
    pub streamers: Vec<Streamer>,
}

/// Convert any generated fragment (each operation has its own type).
pub(crate) fn from_fragment<T: for<'de> Deserialize<'de>, F: Serialize>(
    fragment: &F,
) -> Result<T, LovekitError> {
    Ok(serde_json::from_value(serde_json::to_value(fragment)?)?)
}

impl Structure for Stream {
    const IDENTIFIER: &'static str = "@lovekit/stream";

    fn structure_id(&self) -> String {
        self.id.clone()
    }

    async fn expand(id: String, ctx: &Context) -> anyhow::Result<Self> {
        Ok(ctx.require::<Lovekit>()?.get_stream(&id).await?)
    }

    fn widget() -> Option<Value> {
        Some(widgets::search(SEARCH_STREAMS, "lovekit"))
    }
}

impl Structure for SoloBroadcast {
    const IDENTIFIER: &'static str = "@lovekit/solo_broadcast";

    fn structure_id(&self) -> String {
        self.id.clone()
    }

    async fn expand(id: String, ctx: &Context) -> anyhow::Result<Self> {
        Ok(ctx.require::<Lovekit>()?.get_solo_broadcast(&id).await?)
    }

    fn widget() -> Option<Value> {
        Some(widgets::search(SEARCH_SOLO_BROADCASTS, "lovekit"))
    }
}
