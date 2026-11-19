//! The order a client tries an instance's aliases in: the server's ranking
//! ([`Alias::priority`]), narrowed by the client's own [`AliasPolicy`].

use crate::models::Alias;
use crate::prelude::*;

/// Reaches tried first, in this order (comma-separated).
pub const ALIAS_PREFER_REACH_ENV: &str = "FAKTS_ALIAS_PREFER_REACH";
/// Reaches never used (comma-separated).
pub const ALIAS_EXCLUDE_REACH_ENV: &str = "FAKTS_ALIAS_EXCLUDE_REACH";
/// Alias ids never used (comma-separated).
pub const ALIAS_EXCLUDE_ENV: &str = "FAKTS_ALIAS_EXCLUDE";

/// This client's own say over which aliases it uses, on top of the server's.
///
/// The server hands every instance's aliases ranked ([`Alias::priority`])
/// and the client follows that. A policy can only narrow it: drop aliases by
/// reach or id, and try some reaches before the server's ranking. It never
/// adds an alias. Reaches are `"loopback"`, `"internal"`, `"lan"`, `"mesh"`
/// or `"public"`; they are not checked, so a reach a newer server sends can
/// already be named.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AliasPolicy {
    /// Reaches tried first, in this order; the server's ranking decides within each.
    pub prefer_reach: Vec<String>,
    /// Reaches never used.
    pub exclude_reach: Vec<String>,
    /// Alias ids never used.
    pub exclude_aliases: Vec<String>,
}

impl AliasPolicy {
    /// The policy `FAKTS_ALIAS_PREFER_REACH`, `FAKTS_ALIAS_EXCLUDE_REACH` and
    /// `FAKTS_ALIAS_EXCLUDE` (comma-separated) describe; empty when none is set.
    #[cfg(feature = "std")]
    pub fn from_env() -> Self {
        fn env_list(name: &str) -> Vec<String> {
            std::env::var(name)
                .map(|value| parse_list(&value))
                .unwrap_or_default()
        }
        Self {
            prefer_reach: env_list(ALIAS_PREFER_REACH_ENV),
            exclude_reach: env_list(ALIAS_EXCLUDE_REACH_ENV),
            exclude_aliases: env_list(ALIAS_EXCLUDE_ENV),
        }
    }

    /// Whether this policy never uses `alias`.
    pub fn excludes(&self, alias: &Alias) -> bool {
        self.exclude_aliases.contains(&alias.id)
            || alias
                .reach
                .as_ref()
                .is_some_and(|reach| self.exclude_reach.contains(reach))
    }

    /// Where `alias`'s reach comes in [`prefer_reach`](Self::prefer_reach);
    /// after all of them if not listed.
    pub fn reach_rank(&self, alias: &Alias) -> usize {
        alias
            .reach
            .as_ref()
            .and_then(|reach| self.prefer_reach.iter().position(|p| p == reach))
            .unwrap_or(self.prefer_reach.len())
    }
}

/// A comma-separated list, items trimmed, empty ones dropped.
#[cfg_attr(not(feature = "std"), allow(dead_code))]
fn parse_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `aliases` in the order this client tries them.
///
/// The server's ranking ([`Alias::priority`], lower first) decides, after the
/// client's own [`AliasPolicy`]: aliases it excludes are dropped, and reaches
/// it prefers come first. Aliases without a priority come after those with
/// one; among aliases ranked alike the server's order stands, so a server
/// that sends no priority (and no policy) leaves the order as it is.
pub fn ranked_aliases(aliases: &[Alias], policy: &AliasPolicy) -> Vec<Alias> {
    let mut ranked: Vec<(usize, &Alias)> = aliases
        .iter()
        .filter(|alias| !policy.excludes(alias))
        .enumerate()
        .collect();
    ranked.sort_by_key(|&(index, alias)| {
        (
            policy.reach_rank(alias),
            alias.priority.is_none(),
            alias.priority.unwrap_or(0),
            index,
        )
    });
    ranked.into_iter().map(|(_, alias)| alias.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias(id: &str, reach: Option<&str>, priority: Option<i32>) -> Alias {
        Alias {
            id: id.into(),
            host: format!("{id}.example"),
            port: None,
            ssl: true,
            path: None,
            challenge: "ht".into(),
            public: false,
            kind: None,
            reach: reach.map(Into::into),
            priority,
            proxy: None,
        }
    }

    fn ids(aliases: &[Alias]) -> Vec<&str> {
        aliases.iter().map(|a| a.id.as_str()).collect()
    }

    #[test]
    fn server_priority_beats_the_original_order() {
        let none = AliasPolicy::default();
        let aliases = [
            alias("public", Some("public"), Some(20)),
            alias("legacy", None, None),
            alias("lan", Some("lan"), Some(10)),
            alias("loopback", Some("loopback"), Some(0)),
        ];
        assert_eq!(
            ids(&ranked_aliases(&aliases, &none)),
            ["loopback", "lan", "public", "legacy"]
        );
        // No priorities and no policy: the order stays as sent.
        let unranked = [alias("b", None, None), alias("a", None, None)];
        assert_eq!(ids(&ranked_aliases(&unranked, &none)), ["b", "a"]);
    }

    #[test]
    fn policy_excludes_then_prefers_reaches() {
        let policy = AliasPolicy {
            prefer_reach: parse_list(" mesh , ,lan"),
            exclude_reach: parse_list("public"),
            exclude_aliases: parse_list("lan-2"),
        };
        let aliases = [
            alias("loopback", Some("loopback"), Some(0)),
            alias("lan-1", Some("lan"), Some(10)),
            alias("lan-2", Some("lan"), Some(5)),
            alias("public", Some("public"), Some(1)),
            alias("mesh", Some("mesh"), Some(30)),
        ];
        assert_eq!(
            ids(&ranked_aliases(&aliases, &policy)),
            ["mesh", "lan-1", "loopback"]
        );
    }
}
