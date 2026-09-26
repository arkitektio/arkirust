use std::sync::Arc;

use arkitekt::rath::Rath;
use arkitekt::rekuest::ContextBuilder;
use arkitekt::{async_trait, Fakts, Requirement, Service};

use crate::datalayer::DataLayer;
use crate::Mikro;

/// Makes a [`Mikro`] client available to an app.
///
/// Needs two instances: `mikro` (`live.arkitekt.mikro`, the GraphQL API) and
/// `s3` (`live.arkitekt.s3`, the datalayer the arrays are stored in).
#[derive(Debug, Clone, Copy, Default)]
pub struct MikroService;

/// The mikro service, for `App::service(mikro::service())`.
pub fn service() -> MikroService {
    MikroService
}

#[async_trait]
impl Service for MikroService {
    fn name(&self) -> &'static str {
        "mikro"
    }

    fn requirements(&self) -> Vec<Requirement> {
        vec![
            Requirement::new("mikro", "live.arkitekt.mikro")
                .description("Where the user's images and their metadata live"),
            Requirement::new("s3", "live.arkitekt.s3")
                .description("Where the user's files are stored"),
        ]
    }

    async fn build(&self, fakts: &Fakts, clients: &mut ContextBuilder) -> anyhow::Result<()> {
        let mikro = fakts.get_alias("mikro").await?;
        let s3 = fakts.get_alias("s3").await?;
        let rath = Rath::new(mikro.to_http_path("graphql"), Arc::new(fakts.clone()));
        clients.insert(Mikro::new(rath, DataLayer::new(s3.to_http_path(""))));
        Ok(())
    }
}
