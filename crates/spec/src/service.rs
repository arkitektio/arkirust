//! Services: typed clients an app talks to, built from the app's login.

use crate::context::ContextBuilder;
use crate::manifest::Requirement;
#[allow(unused_imports)]
use crate::prelude::*;
use crate::wiring::Wiring;

/// A service an app talks to, e.g. mikro.
///
/// A service declares which instances it needs and, once the app is logged
/// in, builds its client(s) into the runtime's [`ContextBuilder`]. Actions
/// then receive them as `#[inject]` parameters, and scripts look them up
/// with `Runtime::require`.
///
/// Usually declared with [`#[service]`](crate::service), which derives the
/// requirements from the builder's parameters; any other parameter is
/// provided by type (fakts provides its `Fakts`):
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
#[async_trait::async_trait]
pub trait Service: Send + Sync + 'static {
    /// A short, unique name (used in logs and errors).
    fn name(&self) -> &'static str;

    /// The instances this service needs, keyed by requirement key.
    fn requirements(&self) -> Vec<Requirement>;

    /// What an app using this service has to be allowed to do there: the
    /// scopes its client's operations need, as the service names them
    /// (`mikro_write`). They go into the app's manifest beside its own.
    fn scopes(&self) -> Vec<String> {
        Vec::new()
    }

    /// Build the client(s) and insert them into `clients`.
    async fn build(&self, wiring: &dyn Wiring, clients: &mut ContextBuilder) -> anyhow::Result<()>;
}
