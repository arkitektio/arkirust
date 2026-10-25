//! How a service is reached: an [`Alias`] per requirement, resolved by
//! whoever logged the app in (fakts), and the [`Wiring`] a service's client
//! is built from.

#[allow(unused_imports)]
use crate::prelude::*;

use serde::{Deserialize, Serialize};

/// One way of reaching a service instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alias {
    pub id: String,
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub ssl: bool,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub challenge: String,
    #[serde(default)]
    pub public: bool,
    /// How the server reaches the instance: `"absolute"`, `"relative"` or
    /// `"mesh"` (only reachable over the deployment's tailnet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The HTTP proxy this alias is reached through (the mesh sidecar), set
    /// when the alias is resolved. Never sent or cached.
    #[serde(skip)]
    pub proxy: Option<String>,
}

impl Alias {
    /// Whether this alias is only reachable over the mesh: the server says
    /// so, or (for servers that do not send `kind`) the host is a tailnet
    /// address in 100.64.0.0/10.
    pub fn is_mesh(&self) -> bool {
        match self.kind.as_deref() {
            Some(kind) => kind == "mesh",
            None => self
                .host
                .parse::<core::net::Ipv4Addr>()
                .is_ok_and(|ip| ip.octets()[0] == 100 && (ip.octets()[1] & 0b1100_0000) == 64),
        }
    }

    /// The proxy to reach this alias through, if any.
    pub fn proxy(&self) -> Option<&str> {
        self.proxy.as_deref()
    }

    fn build(&self, scheme: &str, append: Option<&str>) -> String {
        let mut url = format!("{scheme}://{}", self.host);
        if let Some(port) = self.port {
            url.push_str(&format!(":{port}"));
        }
        if let Some(path) = self.path.as_deref().filter(|p| !p.is_empty()) {
            url.push('/');
            url.push_str(path.trim_start_matches('/').trim_end_matches('/'));
        }
        if let Some(append) = append.filter(|a| !a.is_empty()) {
            url.push('/');
            url.push_str(append.trim_start_matches('/'));
        }
        url
    }

    /// `http(s)://host[:port][/path][/append]`
    pub fn to_http_path(&self, append: impl AsRef<str>) -> String {
        self.build(
            if self.ssl { "https" } else { "http" },
            Some(append.as_ref()),
        )
    }

    /// `ws(s)://host[:port][/path][/append]`
    pub fn to_ws_path(&self, append: impl AsRef<str>) -> String {
        self.build(if self.ssl { "wss" } else { "ws" }, Some(append.as_ref()))
    }

    /// The URL that must answer 200 before this alias is used.
    pub fn challenge_path(&self) -> String {
        self.to_http_path(&self.challenge)
    }
}

/// What a [`Service`](crate::Service) is built from: the alias each of its
/// requirements resolved to, and whatever else the login provides by type
/// (fakts provides its `Fakts`, the token loader).
#[cfg(feature = "declare")]
#[async_trait::async_trait]
pub trait Wiring: Send + Sync {
    /// The alias the requirement `key` resolved to; `None` when the
    /// deployment has no instance for it (an optional requirement).
    async fn alias(&self, key: &str) -> anyhow::Result<Option<Alias>>;

    /// A value of the type `type_id` names, if this wiring provides one.
    fn provided(&self, type_id: core::any::TypeId) -> Option<Box<dyn core::any::Any + Send>>;
}

#[cfg(feature = "declare")]
impl dyn Wiring + '_ {
    /// The alias for a required `key`.
    pub async fn require_alias(&self, key: &str) -> anyhow::Result<Alias> {
        self.alias(key)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the deployment has no instance for `{key}`"))
    }

    /// The provided value of type `T`.
    pub fn provide<T: Clone + Send + 'static>(&self) -> anyhow::Result<T> {
        self.provided(core::any::TypeId::of::<T>())
            .and_then(|value| value.downcast::<T>().ok())
            .map(|value| *value)
            .ok_or_else(|| {
                anyhow::anyhow!("nothing provides a {} here", core::any::type_name::<T>())
            })
    }
}
