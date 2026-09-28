//! The lovekit service: what an app declares to use it.

use std::sync::Arc;

use arkitekt::rath::Rath;
use arkitekt::{Alias, Fakts};

use crate::Lovekit;

/// Lovekit: live audio and video. `App::service(lovekit::service)`.
///
/// `livekit` is not dialled here: rooms are joined with a token lovekit hands
/// out, by the LiveKit SDK (`Lovekit::connect_room`, feature `livekit`). The
/// client keeps it, and `fakts` for a media server that is only on the mesh.
#[arkitekt::service(name = "lovekit")]
pub fn service(
    #[require("live.arkitekt.lovekit", "Where rooms and their tokens are managed")] lovekit: Alias,
    #[require("io.livekit.livekit", "The media server rooms are hosted on")] livekit: Alias,
    fakts: Fakts,
) -> anyhow::Result<Lovekit> {
    let rath = Rath::from_alias(&lovekit, "graphql", Arc::new(fakts.clone()))?;
    Ok(Lovekit::new(rath, Some(livekit), Some(fakts)))
}
