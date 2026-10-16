//! Per-client policy: which profile a client gets, and the config that carries it.

use super::policy::RoutingPolicy;
use serde::{Deserialize, Serialize};

// --------------------------------------------------- Client Profiles

/// Describes how to identify a client application from request metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMatcher {
    /// Match by the User-Agent header (substring match, case-insensitive).
    UserAgent { value: String },
    /// Match by the API key prefix (e.g. "sk-proj-abc").
    ApiKeyPrefix { value: String },
    /// Match by an arbitrary header (exact value match, case-insensitive name).
    Header { name: String, value: String },
    /// Match by the requested model id prefix (e.g. "gpt-").
    ModelPrefix { value: String },
    /// Match by an explicit client identifier passed in the request.
    ClientId { value: String },
}

/// A named client/application profile that maps to a routing policy.
///
/// Profiles are tried in order; the first matching profile selects the
/// policy to use for the request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientProfile {
    /// Unique identifier for this client profile.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Conditions that must all pass for this profile to match.
    #[serde(default)]
    pub matchers: Vec<ClientMatcher>,
    /// The policy id to apply when this profile matches.
    pub policy_id: String,
}

/// Request metadata used to resolve which client profile applies.
#[derive(Debug, Clone)]
pub struct ClientContext<'a> {
    /// Explicit client identifier, if provided.
    pub client_id: Option<&'a str>,
    /// User-Agent header value.
    pub user_agent: Option<&'a str>,
    /// Prefix of the API key used (first few characters, for privacy).
    pub api_key_prefix: Option<&'a str>,
    /// Application name from x-zroutery-application header.
    pub application: Option<&'a str>,
    /// All request headers as (name, value) pairs.
    pub headers: &'a [(&'a str, &'a str)],
    /// The requested model id.
    pub model: &'a str,
}

impl ClientProfile {
    /// Returns `true` if every matcher in this profile matches the context.
    /// An empty matcher list means the profile never matches (must have at
    /// least one condition to avoid accidental catch-all).
    pub fn matches(&self, ctx: &ClientContext) -> bool {
        if self.matchers.is_empty() {
            return false;
        }
        self.matchers.iter().all(|m| m.matches(ctx))
    }
}

impl ClientMatcher {
    fn matches(&self, ctx: &ClientContext) -> bool {
        match self {
            ClientMatcher::UserAgent { value } => ctx
                .user_agent
                .is_some_and(|ua| ua.to_lowercase().contains(&value.to_lowercase())),
            ClientMatcher::ApiKeyPrefix { value } => {
                ctx.api_key_prefix.is_some_and(|key| key.starts_with(value))
            }
            ClientMatcher::Header { name, value } => ctx
                .headers
                .iter()
                .any(|(n, v)| n.eq_ignore_ascii_case(name) && v == value),
            ClientMatcher::ModelPrefix { value } => ctx.model.starts_with(value),
            ClientMatcher::ClientId { value } => ctx.client_id.is_some_and(|id| id == value),
        }
    }
}

/// Resolve the first matching client profile from a list, or `None` if no
/// profile matches.
pub fn resolve_client<'a>(
    profiles: &'a [ClientProfile],
    ctx: &ClientContext,
) -> Option<&'a ClientProfile> {
    profiles.iter().find(|p| p.matches(ctx))
}

// ----------------------------------------------------------- Config

/// Policies configured in the routing section.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PolicyConfig {
    /// Named routing policies.
    #[serde(default)]
    pub policies: Vec<RoutingPolicy>,
    /// Default policy id to use when no other policy matches.
    #[serde(default)]
    pub default_policy: Option<String>,
    /// Client/application profiles that map request metadata to policies.
    #[serde(default)]
    pub clients: Vec<ClientProfile>,
}
