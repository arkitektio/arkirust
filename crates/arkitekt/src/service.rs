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
/// Usually declared with [`#[arkitekt::service]`](crate::service), which
/// derives the requirements from the builder's parameters:
///
/// ```ignore
/// #[arkitekt::service(name = "mikro")]
/// pub fn service(
///     #[require("live.arkitekt.mikro", "Where the user's images live")] mikro: Alias,
///     fakts: Fakts,
/// ) -> Mikro {
///     Mikro::new(Rath::new(mikro.to_http_path("graphql"), Arc::new(fakts)))
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
