//! A client for Arkitekt's lovekit service: LiveKit broadcasts and streams.
//!
//! Lovekit hands out LiveKit tokens (`ensure_stream`, `join_broadcast`); the
//! media server is the `io.livekit.livekit` alias. With the `livekit`
//! feature, [`Lovekit::connect_room`] joins a room and [`VideoPublisher`]
//! publishes frames. With `mesh`, that also works for a media server only on
//! the deployment's mesh: signaling goes through a local forward and all media
//! through the mesh node's TURN relay, as UDP over the mesh. No root or TUN
//! device is needed.
//!
//! ```no_run
//! # async fn demo(lovekit: lovekit::Lovekit) -> anyhow::Result<()> {
//! use lovekit::StreamKind;
//!
//! let broadcast = lovekit.ensure_solo_broadcast(Some("microscope"), None).await?;
//! let token = lovekit
//!     .ensure_stream(Some(&broadcast.id), StreamKind::Video, Some("camera"))
//!     .await?;
//! # #[cfg(feature = "livekit")] {
//! let (room, _events) = lovekit.connect_room(&token).await?;
//! let camera = lovekit::VideoPublisher::publish(&room, "camera", 640, 480).await?;
//! camera.push_rgba(&vec![0u8; 640 * 480 * 4], 0);
//! # }
//! # Ok(()) }
//! ```

pub mod api;
#[cfg(feature = "livekit")]
mod livekit_room;
mod models;
mod specs;

use arkitekt::rath::{Rath, RathError};
use arkitekt::{Alias, Fakts};

#[cfg(feature = "livekit")]
pub use crate::livekit_room::{relay_options, RoomTarget, VideoPublisher};
pub use crate::models::*;
pub use crate::specs::service;
#[cfg(feature = "livekit")]
pub use livekit;

#[derive(Debug, thiserror::Error)]
pub enum LovekitError {
    #[error(transparent)]
    GraphQL(#[from] RathError),
    #[error("unexpected response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("{0} not found")]
    NotFound(String),
    #[error("this client was built without the livekit alias")]
    NoLivekit,
    #[error("the media server is only reachable over the mesh: {0}")]
    Mesh(String),
    #[cfg(feature = "livekit")]
    #[error(transparent)]
    Room(#[from] livekit::RoomError),
}

pub type Result<T, E = LovekitError> = std::result::Result<T, E>;

/// Lovekit: broadcasts, streams and the tokens to join them.
#[derive(Clone)]
pub struct Lovekit {
    rath: Rath,
    livekit: Option<Alias>,
    /// For a media server only on the mesh (feature `mesh`).
    #[cfg_attr(not(feature = "mesh"), allow(dead_code))]
    fakts: Option<Fakts>,
}

impl std::fmt::Debug for Lovekit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lovekit")
            .field("livekit", &self.livekit)
            .finish_non_exhaustive()
    }
}

impl Lovekit {
    pub fn new(rath: Rath, livekit: Option<Alias>, fakts: Option<Fakts>) -> Self {
        Self {
            rath,
            livekit,
            fakts,
        }
    }

    /// The GraphQL client, for operations this crate does not wrap.
    pub fn rath(&self) -> &Rath {
        &self.rath
    }

    /// The media server rooms are hosted on.
    pub fn livekit(&self) -> Option<&Alias> {
        self.livekit.as_ref()
    }

    /// This app's own broadcast (created once per `instance_id`).
    pub async fn ensure_solo_broadcast(
        &self,
        title: Option<&str>,
        instance_id: Option<&str>,
    ) -> Result<SoloBroadcast> {
        use api::ensure_solo_broadcast::{EnsureSoloBroadcastInput, Variables};
        let data = self
            .rath
            .execute::<api::EnsureSoloBroadcast>(Variables {
                input: EnsureSoloBroadcastInput {
                    title: title.map(str::to_owned),
                    instance_id: instance_id.map(str::to_owned),
                },
            })
            .await?;
        models::from_fragment(&data.ensure_solo_broadcast)
    }

    /// Create a stream (in `broadcast`, if given) and return the LiveKit
    /// token to publish it with.
    pub async fn ensure_stream(
        &self,
        broadcast: Option<&str>,
        kind: StreamKind,
        title: Option<&str>,
    ) -> Result<String> {
        use api::ensure_stream::{EnsureStreamInput, StreamKind as Kind, Variables};
        let data = self
            .rath
            .execute::<api::EnsureStream>(Variables {
                input: EnsureStreamInput {
                    broadcast: broadcast.map(str::to_owned),
                    kind: match kind {
                        StreamKind::Video => Kind::VIDEO,
                        StreamKind::Audio => Kind::AUDIO,
                    },
                    title: title.map(str::to_owned),
                },
            })
            .await?;
        Ok(data.ensure_stream)
    }

    /// The LiveKit token to join (watch) a broadcast.
    pub async fn join_broadcast(&self, broadcast: &str) -> Result<String> {
        use api::join_broadcast::{JoinBroadcastInput, Variables};
        let data = self
            .rath
            .execute::<api::JoinBroadcast>(Variables {
                input: JoinBroadcastInput {
                    broadcast: broadcast.to_owned(),
                },
            })
            .await?;
        Ok(data.join_broadcast)
    }

    pub async fn get_stream(&self, id: &str) -> Result<Stream> {
        let data = self
            .rath
            .execute::<api::GetStream>(api::get_stream::Variables { id: id.to_owned() })
            .await?;
        models::from_fragment(&data.stream)
    }

    /// Streams, optionally matching `search`, at most `limit` of them.
    pub async fn list_streams(
        &self,
        search: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<Stream>> {
        use api::list_streams::{OffsetPaginationInput, StreamFilter, Variables};
        let data = self
            .rath
            .execute::<api::ListStreams>(Variables {
                filter: search.map(|s| StreamFilter {
                    search: Some(s.to_owned()),
                    ..empty_stream_filter()
                }),
                pagination: Some(OffsetPaginationInput { offset: 0, limit }),
            })
            .await?;
        data.streams.iter().map(models::from_fragment).collect()
    }

    pub async fn get_solo_broadcast(&self, id: &str) -> Result<SoloBroadcast> {
        let data = self
            .rath
            .execute::<api::GetSoloBroadcast>(api::get_solo_broadcast::Variables {
                id: id.to_owned(),
            })
            .await?;
        models::from_fragment(&data.solo_broadcast)
    }

    pub async fn list_solo_broadcasts(&self, limit: Option<i64>) -> Result<Vec<SoloBroadcast>> {
        use api::list_solo_broadcasts::{OffsetPaginationInput, Variables};
        let data = self
            .rath
            .execute::<api::ListSoloBroadcasts>(Variables {
                filter: None,
                pagination: Some(OffsetPaginationInput { offset: 0, limit }),
            })
            .await?;
        data.solo_broadcasts
            .iter()
            .map(models::from_fragment)
            .collect()
    }

    pub async fn get_collaborative_broadcast(&self, id: &str) -> Result<CollaborativeBroadcast> {
        let data = self
            .rath
            .execute::<api::GetCollaborativeBroadcast>(
                api::get_collaborative_broadcast::Variables { id: id.to_owned() },
            )
            .await?;
        models::from_fragment(&data.collaborative_broadcast)
    }

    pub async fn list_collaborative_broadcasts(
        &self,
        limit: Option<i64>,
    ) -> Result<Vec<CollaborativeBroadcast>> {
        use api::list_collaborative_broadcasts::{OffsetPaginationInput, Variables};
        let data = self
            .rath
            .execute::<api::ListCollaborativeBroadcasts>(Variables {
                filter: None,
                pagination: Some(OffsetPaginationInput { offset: 0, limit }),
            })
            .await?;
        data.collaborative_broadcasts
            .iter()
            .map(models::from_fragment)
            .collect()
    }
}

fn empty_stream_filter() -> api::list_streams::StreamFilter {
    api::list_streams::StreamFilter {
        ids: None,
        search: None,
        and: Box::new(None),
        or: Box::new(None),
        not: Box::new(None),
        distinct: None,
    }
}
