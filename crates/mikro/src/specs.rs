use std::sync::Arc;

use arkitekt::rath::Rath;
use arkitekt::{Alias, Fakts};

use crate::datalayer::DataLayer;
use crate::Mikro;

/// Mikro: the user's images, files and metadata.
///
/// Use it with `App::service(mikro::service)`; actions then `#[inject]` a
/// [`Mikro`] client.
#[arkitekt::service(name = "mikro")]
pub fn service(
    #[require("live.arkitekt.mikro", "Where the user's images and their metadata live")] mikro: Alias,
    #[require("live.arkitekt.s3", "Where the user's files are stored")] s3: Alias,
    fakts: Fakts,
) -> anyhow::Result<Mikro> {
    let rath = Rath::from_alias(&mikro, "graphql", Arc::new(fakts))?;
    Ok(Mikro::new(rath, DataLayer::from_alias(&s3)))
}
