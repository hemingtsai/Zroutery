//! The policy itself: the weights, the hard constraints, the fallbacks, and the context they are matched against.

use super::profile::{Complexity, TaskProfile, TaskType};
use serde::{Deserialize, Serialize};

use crate::config::ModelTier;
use crate::ir::Capability;

// ---------------------------------------------------------------- Policy

/// A complete routing policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingPolicy {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// If all matchers pass, this policy applies.
    #[serde(default)]
    pub matchers: Vec<PolicyMatcher>,
    /// Hard constraints — candidates that fail these are excluded.
    #[serde(default)]
    pub requirements: PolicyRequirements,
    /// Soft preferences — used to rank eligible candidates.
    #[serde(default)]
    pub preference: PolicyPreference,
    /// What to do when no candidate passes eligibility.
    #[serde(default)]
    pub fallback: PolicyFallback,
}

fn default_true() -> bool {
    true
}

// ------------------------------------------------------------- Matcher

/// A single condition that can match or not match a request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PolicyMatcher {
    /// Match by client identifier (User-Agent, token, explicit client_id).
    Client { value: String },
    /// Match by application name.
    Application { value: String },
    /// Match by model id prefix.
    ModelPrefix { value: String },
    /// Match if request is streaming.
    Streaming { value: bool },
    /// Match if request uses tools.
    HasTools { value: bool },
    /// Match if request has vision content.
    HasVision { value: bool },
    /// Match if task complexity is at least this level.
    MinComplexity { value: Complexity },
    /// Match by task type.
    TaskType { value: TaskType },
}

// ---------------------------------------------------------- Requirements

/// Hard constraints for candidate eligibility.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PolicyRequirements {
    /// Candidates must support all of these capabilities.
    #[serde(default)]
    pub required_capabilities: Vec<Capability>,
    /// Retained as a compatibility/reason-shaping switch. Unknown provider
    /// capability is never a pass; the strict setting only preserves the
    /// historical `MissingCapability` reason token for policy-only checks.
    #[serde(default)]
    pub strict_capabilities: bool,
    /// Minimum tier (candidate tier must be >= this).
    #[serde(default)]
    pub min_tier: Option<ModelTier>,
    /// Maximum tier (candidate tier must be <= this).
    #[serde(default)]
    pub max_tier: Option<ModelTier>,
    /// Only these providers are allowed (empty = all).
    #[serde(default)]
    pub allowed_providers: Vec<String>,
    /// These providers are forbidden.
    #[serde(default)]
    pub forbidden_providers: Vec<String>,
    /// Only these models are allowed (empty = all).
    #[serde(default)]
    pub allowed_models: Vec<String>,
    /// These models are forbidden.
    #[serde(default)]
    pub forbidden_models: Vec<String>,
}

// ---------------------------------------------------------- Preference

/// Soft scoring weights for ranking eligible candidates.
///
/// Each weight is in [0.0, 1.0]. The scoring engine normalizes them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyPreference {
    /// Prefer candidates closer to this tier.
    #[serde(default)]
    pub preferred_tier: Option<ModelTier>,
    /// Weight for health score (healthy > degraded > unknown).
    #[serde(default = "default_weight")]
    pub health_weight: f64,
    /// Weight for latency (lower is better).
    #[serde(default = "default_weight")]
    pub latency_weight: f64,
    /// Weight for cost (cheaper is better).
    #[serde(default = "default_weight")]
    pub cost_weight: f64,
    /// Weight for explicit priority field.
    #[serde(default = "default_weight")]
    pub priority_weight: f64,
    /// Weight for tier proximity to preferred_tier.
    #[serde(default = "default_weight")]
    pub tier_weight: f64,
}

impl Default for PolicyPreference {
    fn default() -> Self {
        PolicyPreference {
            preferred_tier: None,
            health_weight: 0.2,
            latency_weight: 0.2,
            cost_weight: 0.1,
            priority_weight: 0.2,
            tier_weight: 0.3,
        }
    }
}

fn default_weight() -> f64 {
    0.2
}

impl std::hash::Hash for PolicyPreference {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.preferred_tier.hash(state);
        self.health_weight.to_bits().hash(state);
        self.latency_weight.to_bits().hash(state);
        self.cost_weight.to_bits().hash(state);
        self.priority_weight.to_bits().hash(state);
        self.tier_weight.to_bits().hash(state);
    }
}

// ----------------------------------------------------------- Fallback

/// What to do when no candidate passes eligibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PolicyFallback {
    /// Return an error to the client.
    Reject,
    /// Try escalating to a higher tier.
    Escalate {
        #[serde(default = "default_true")]
        enabled: bool,
        /// Maximum number of tier escalation steps.
        #[serde(default = "default_max_escalations")]
        max_steps: u32,
    },
    /// Degrade to a lower tier (cheaper/faster).
    Degrade {
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default = "default_max_escalations")]
        max_steps: u32,
    },
    /// Bypass the non-capability policy constraints and retry the pool.
    ///
    /// Despite the legacy name this is deliberately *not* "ignore every
    /// requirement". ADR-0003 keeps the capability contract hard, so this
    /// action only relaxes the constraints that are not part of that contract:
    ///
    /// - dropped: tier bounds, allowed/forbidden providers, and
    ///   allowed/forbidden models;
    /// - kept as hard requirements: policy `required_capabilities` (evaluated
    ///   strictly) and the request-derived capability vector;
    /// - kept as a hard rejection: an open circuit, which is candidate health
    ///   state rather than a policy requirement.
    ///
    /// If no candidate in the pool satisfies the capability contract, the
    /// router still returns `Error::NoCandidate` instead of planning an
    /// incapable candidate.
    IgnoreRequirements,
}

impl Default for PolicyFallback {
    fn default() -> Self {
        PolicyFallback::Escalate {
            enabled: true,
            max_steps: 2,
        }
    }
}

fn default_max_escalations() -> u32 {
    2
}

// ------------------------------------------------------------ Matching

/// Context available to matchers during policy evaluation.
///
/// Only contains request/client properties. Candidate constraints
/// (model, provider, tier, capabilities) belong in [`PolicyRequirements`].
#[derive(Debug)]
pub struct MatchContext<'a> {
    pub client_id: Option<&'a str>,
    pub application: Option<&'a str>,
    pub model: &'a str,
    pub streaming: bool,
    pub has_tools: bool,
    pub has_vision: bool,
    pub task: Option<&'a TaskProfile>,
}

impl RoutingPolicy {
    /// Returns true if all matchers pass for the given context.
    /// An empty matcher list means the policy always applies.
    pub fn matches(&self, ctx: &MatchContext) -> bool {
        if !self.enabled {
            return false;
        }
        self.matchers.iter().all(|m| m.matches(ctx))
    }
}

impl PolicyMatcher {
    fn matches(&self, ctx: &MatchContext) -> bool {
        match self {
            PolicyMatcher::Client { value } => ctx.client_id.is_some_and(|id| id == value),
            PolicyMatcher::Application { value } => ctx.application.is_some_and(|app| app == value),
            PolicyMatcher::ModelPrefix { value } => ctx.model.starts_with(value),
            PolicyMatcher::Streaming { value } => ctx.streaming == *value,
            PolicyMatcher::HasTools { value } => ctx.has_tools == *value,
            PolicyMatcher::HasVision { value } => ctx.has_vision == *value,
            PolicyMatcher::MinComplexity { value } => {
                ctx.task.is_some_and(|t| t.complexity >= *value)
            }
            PolicyMatcher::TaskType { value } => ctx.task.is_some_and(|t| t.task_type == *value),
        }
    }
}
