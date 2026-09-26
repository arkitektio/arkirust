//! Services: typed clients built from the app's fakts.

use async_trait::async_trait;
use fakts::{Fakts, Requirement};
use rekuest::ContextBuilder;

/// A service an app talks to, e.g. mikro.
///
/// A service declares which fakts instances it needs and, once the app is
/// authorized, builds its client(s) into the runtime's [`ContextBuilder`].
/// Actions then receive them as `#[inject]` parameters, and scripts look
/// them up with [`Runtime::require`](crate::Runtime::require).
///
/// ```ignore
/// pub struct MikroService;
///
/// #[async_trait]
/// impl Service for MikroService {
///     fn name(&self) -> &'static str { "mikro" }
///     fn requirements(&self) -> Vec<Requirement> {
///         vec![Requirement::new("mikro", "live.arkitekt.mikro")]
///     }
///     async fn build(&self, fakts: &Fakts, clients: &mut ContextBuilder) -> anyhow::Result<()> {
///         let alias = fakts.get_alias("mikro").await?;
///         clients.insert(Mikro::new(alias.to_http_path("graphql"), ...));
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait Service: Send + Sync + 'static {
    /// A short, unique name (used in logs and errors).
    fn name(&self) -> &'static str;

    /// The fakts instances this service needs, keyed by requirement key.
    fn requirements(&self) -> Vec<Requirement>;

    /// Build the client(s) and insert them into `clients`.
    async fn build(&self, fakts: &Fakts, clients: &mut ContextBuilder) -> anyhow::Result<()>;
}

/// The requirement the rekuest agent needs to serve actions.
pub(crate) fn rekuest_requirement() -> Requirement {
    Requirement::new("rekuest", "live.arkitekt.rekuest").description("Where actions are assigned")
}
