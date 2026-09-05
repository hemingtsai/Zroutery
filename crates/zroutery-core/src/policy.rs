//! Routing policy engine.
//!
//! A [`RoutingPolicy`] defines *why* a request should land on a particular
//! model.  Policies sit between request analysis and candidate selection:
//!
//! ```text
//! Request → TaskProfile → PolicyMatcher → PolicyRequirements → Candidate
//! ```
//!
//! Each policy has:
//! - **Matchers**: conditions that determine whether the policy applies to a
//!   given request.
//! - **Requirements**: hard constraints that candidates must satisfy.
//! - **Preference**: soft scoring weights for ranking eligible candidates.
//! - **Fallback**: what to do when no candidate is eligible.

use serde::{Deserialize, Serialize};

use crate::config::{ModelCapabilities, ModelTier};
use crate::ir::{Capability, CapabilityState, ChatRequest};

// ----------------------------------------------------------- Profile

/// Task complexity level, derived from request characteristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Complexity {
    Simple,
    Standard,
    Complex,
    Frontier,
}

impl Default for Complexity {
    fn default() -> Self {
        Complexity::Standard
    }
}

/// The kind of task the request represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    Chat,
    Code,
    Vision,
    Analysis,
    Creative,
    ToolUse,
}

impl Default for TaskType {
    fn default() -> Self {
        TaskType::Chat
    }
}

/// Derived profile of a request, fed into policy matchers.
#[derive(Debug, Clone)]
pub struct TaskProfile {
    pub tier: Option<ModelTier>,
    pub required_capabilities: Vec<Capability>,
    pub context_tokens: u64,
    pub estimated_output_tokens: u64,
    pub streaming: bool,
    pub has_tools: bool,
    pub has_vision: bool,
    pub complexity: Complexity,
    pub task_type: TaskType,
}

impl TaskProfile {
    /// Derive a [`TaskProfile`] from a [`ChatRequest`].
    pub fn from_request(req: &ChatRequest) -> Self {
        let required_capabilities = req.compute_required_capabilities();
        let context_tokens = req.estimate_tokens() as u64;
        let estimated_output_tokens = req.max_tokens.unwrap_or(4096) as u64;
        let has_tools = !req.tools.is_empty();
        let has_vision = required_capabilities.contains(&Capability::Vision);
        let thinking_enabled = req.thinking.as_ref().is_some_and(|t| t.enabled);
        let total_tokens = context_tokens + estimated_output_tokens;

        let complexity = if context_tokens > 200_000 && thinking_enabled && has_tools {
            Complexity::Frontier
        } else if thinking_enabled || total_tokens > 100_000 {
            Complexity::Complex
        } else if total_tokens > 10_000 || has_tools {
            Complexity::Standard
        } else {
            Complexity::Simple
        };

        let task_type = if has_tools {
            TaskType::ToolUse
        } else if has_vision {
            TaskType::Vision
        } else {
            TaskType::Chat
        };

        TaskProfile {
            tier: None,
            required_capabilities,
            context_tokens,
            estimated_output_tokens,
            streaming: req.stream,
            has_tools,
            has_vision,
            complexity,
            task_type,
        }
    }

    /// Suggest a model tier based on the task profile's complexity.
    pub fn suggested_tier(&self) -> ModelTier {
        match self.complexity {
            Complexity::Frontier => ModelTier::Frontier,
            Complexity::Complex => ModelTier::Reasoning,
            Complexity::Standard => ModelTier::Standard,
            Complexity::Simple => ModelTier::Fast,
        }
    }
}

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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PolicyRequirements {
    /// Candidates must support all of these capabilities.
    #[serde(default)]
    pub required_capabilities: Vec<Capability>,
    /// When `true`, candidates with [`CapabilityState::Unknown`] for a
    /// required capability are rejected. When `false`, unknown capabilities
    /// are treated as a soft fallback (the candidate is still eligible).
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
    /// Ignore requirements and try all candidates.
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
            PolicyMatcher::Client { value } => {
                ctx.client_id.map_or(false, |id| id == value)
            }
            PolicyMatcher::Application { value } => {
                ctx.application.map_or(false, |app| app == value)
            }
            PolicyMatcher::ModelPrefix { value } => ctx.model.starts_with(value),
            PolicyMatcher::Streaming { value } => ctx.streaming == *value,
            PolicyMatcher::HasTools { value } => ctx.has_tools == *value,
            PolicyMatcher::HasVision { value } => ctx.has_vision == *value,
            PolicyMatcher::MinComplexity { value } => {
                ctx.task.map_or(false, |t| t.complexity >= *value)
            }
            PolicyMatcher::TaskType { value } => {
                ctx.task.map_or(false, |t| t.task_type == *value)
            }
        }
    }
}

// -------------------------------------------------------- Eligibility

/// Result of checking a candidate against policy requirements.
#[derive(Debug, Clone)]
pub struct EligibilityCheck {
    pub eligible: bool,
    pub reasons: Vec<RejectionReason>,
}

/// Why a candidate was rejected.
#[derive(Debug, Clone)]
pub enum RejectionReason {
    MissingCapability(Capability),
    BelowMinTier,
    AboveMaxTier,
    ProviderForbidden,
    ProviderNotAllowed,
    ModelForbidden,
    ModelNotAllowed,
    CircuitOpen,
}

impl PolicyRequirements {
    /// Check whether a candidate satisfies all hard constraints.
    pub fn check(
        &self,
        model_id: &str,
        provider_id: &str,
        tier: Option<ModelTier>,
        capabilities: &ModelCapabilities,
        circuit_open: bool,
    ) -> EligibilityCheck {
        let mut reasons = Vec::new();

        // Capability check using tri-state capability_state()
        for cap in &self.required_capabilities {
            match capabilities.capability_state(*cap) {
                CapabilityState::Supported => {} // eligible
                CapabilityState::Unsupported => {
                    reasons.push(RejectionReason::MissingCapability(*cap));
                }
                CapabilityState::Unknown => {
                    if self.strict_capabilities {
                        reasons.push(RejectionReason::MissingCapability(*cap));
                    }
                    // When not strict, unknown = soft fallback (eligible)
                }
            }
        }

        // Tier bounds
        if let (Some(min), Some(tier)) = (self.min_tier, tier) {
            if tier < min {
                reasons.push(RejectionReason::BelowMinTier);
            }
        }
        if let (Some(max), Some(tier)) = (self.max_tier, tier) {
            if tier > max {
                reasons.push(RejectionReason::AboveMaxTier);
            }
        }

        // Provider constraints
        if !self.allowed_providers.is_empty()
            && !self.allowed_providers.iter().any(|p| p == provider_id)
        {
            reasons.push(RejectionReason::ProviderNotAllowed);
        }
        if self
            .forbidden_providers
            .iter()
            .any(|p| p == provider_id)
        {
            reasons.push(RejectionReason::ProviderForbidden);
        }

        // Model constraints
        if !self.allowed_models.is_empty()
            && !self.allowed_models.iter().any(|m| m == model_id)
        {
            reasons.push(RejectionReason::ModelNotAllowed);
        }
        if self
            .forbidden_models
            .iter()
            .any(|m| m == model_id)
        {
            reasons.push(RejectionReason::ModelForbidden);
        }

        // Circuit breaker
        if circuit_open {
            reasons.push(RejectionReason::CircuitOpen);
        }

        EligibilityCheck {
            eligible: reasons.is_empty(),
            reasons,
        }
    }
}

// ---------------------------------------------------------- Scoring

/// Individual dimension scores for a candidate, each in [0.0, 1.0].
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreBreakdown {
    /// 1.0 = perfectly healthy, 0.0 = circuit open.
    pub health: f64,
    /// Inverse-normalized latency (1000ms baseline).
    pub latency: f64,
    /// Inverse-normalized cost ($10/MTok baseline).
    pub cost: f64,
    /// Lower priority number = higher score.
    pub priority: f64,
    /// Closer to preferred tier = higher score.
    pub tier: f64,
}

/// A candidate annotated with its computed policy score.
#[derive(Debug, Clone)]
pub struct ScoredCandidate {
    pub exposed_id: String,
    pub total_score: f64,
    pub breakdown: ScoreBreakdown,
}

/// Context for scoring a single candidate against a policy's preferences.
///
/// The caller supplies runtime signals (health, latency) that the policy
/// engine itself does not own.
pub struct ScoringContext<'a> {
    /// Health score 0.0-1.0, from the router's circuit breaker state.
    pub health: f64,
    /// EWMA latency in milliseconds, from the router.
    pub avg_latency_ms: f64,
    /// Input price per million tokens. `None` when pricing is not configured.
    pub input_per_mtok: Option<f64>,
    /// Output price per million tokens. `None` when pricing is not configured.
    pub output_per_mtok: Option<f64>,
    /// Lower value = higher priority (from ModelEntry).
    pub priority: i32,
    /// The candidate's tier, if assigned.
    pub tier: Option<ModelTier>,
    /// The task profile for request-aware cost estimation.
    pub task: Option<&'a TaskProfile>,
}

const LATENCY_BASELINE_MS: f64 = 1000.0;
const COST_BASELINE_MTOK: f64 = 10.0;
/// Maximum tier distance (Fast=0 .. Frontier=3, so max gap is 3).
const MAX_TIER_DISTANCE: f64 = 3.0;
/// Cap for priority normalization. Priority values are user-configured i32;
/// anything above this is treated as equally low-priority.
const PRIORITY_CAP: f64 = 100.0;

/// Compute a weighted policy score for a single candidate.
///
/// Each dimension produces a value in [0.0, 1.0]. The final score is the
/// weighted sum, also in [0.0, 1.0] (weights are normalized internally so
/// they do not need to sum to 1.0).
pub fn score_candidate<'a>(pref: &PolicyPreference, ctx: &ScoringContext<'a>) -> ScoredCandidate {
    let health = ctx.health.clamp(0.0, 1.0);

    // Latency: lower is better. Inverse-normalized to 1000ms baseline.
    let latency = if ctx.avg_latency_ms <= 0.0 {
        1.0 // unmeasured = benefit of the doubt
    } else {
        (LATENCY_BASELINE_MS / ctx.avg_latency_ms).min(1.0)
    };

    // Cost: request-aware estimation when pricing and task profile are available.
    // estimated_cost = context_tokens * input_per_mtok / 1M + output_tokens * output_per_mtok / 1M
    // Normalize against $1.0 reference: lower cost → higher score.
    let cost = match (ctx.input_per_mtok, ctx.output_per_mtok, ctx.task) {
        (Some(inp), Some(out), Some(task)) => {
            let estimated_cost = task.context_tokens as f64 * inp / 1_000_000.0
                + task.estimated_output_tokens as f64 * out / 1_000_000.0;
            (1.0 / (1.0 + estimated_cost)).min(1.0)
        }
        (Some(inp), Some(out), None) => {
            // No task profile: fall back to average cost with baseline normalization.
            let avg = (inp + out) / 2.0;
            if avg > 0.0 { (COST_BASELINE_MTOK / avg).min(1.0) } else { 0.5 }
        }
        _ => 0.5, // unknown cost = neutral
    };

    // Priority: lower number = higher score. 0 → 1.0, cap → 0.0.
    let priority = if ctx.priority <= 0 {
        1.0
    } else {
        (1.0 - (ctx.priority as f64) / PRIORITY_CAP).clamp(0.0, 1.0)
    };

    // Tier: closer to preferred = higher score. Distance 0 → 1.0, max → 0.0.
    let tier = match (pref.preferred_tier, ctx.tier) {
        (Some(preferred), Some(candidate)) => {
            let dist = tier_distance(preferred, candidate);
            1.0 - (dist / MAX_TIER_DISTANCE)
        }
        _ => 0.5, // no preference or no tier = neutral
    };

    let breakdown = ScoreBreakdown {
        health,
        latency,
        cost,
        priority,
        tier,
    };

    // Weighted sum with normalized weights.
    let w_sum = pref.health_weight
        + pref.latency_weight
        + pref.cost_weight
        + pref.priority_weight
        + pref.tier_weight;

    let total_score = if w_sum > 0.0 {
        (health * pref.health_weight
            + latency * pref.latency_weight
            + cost * pref.cost_weight
            + priority * pref.priority_weight
            + tier * pref.tier_weight)
            / w_sum
    } else {
        0.0
    };

    ScoredCandidate {
        exposed_id: String::new(), // filled by caller
        total_score,
        breakdown,
    }
}

/// Absolute distance between two tiers (0-3).
fn tier_distance(a: ModelTier, b: ModelTier) -> f64 {
    (tier_ord(a) as f64 - tier_ord(b) as f64).abs()
}

fn tier_ord(t: ModelTier) -> u8 {
    match t {
        ModelTier::Fast => 0,
        ModelTier::Standard => 1,
        ModelTier::Reasoning => 2,
        ModelTier::Frontier => 3,
    }
}

// ----------------------------------------------------------- Default

/// The default policy: standard tier, escalate on failure.
pub fn default_policy() -> RoutingPolicy {
    RoutingPolicy {
        id: "default".into(),
        name: "Default".into(),
        enabled: true,
        matchers: Vec::new(),
        requirements: PolicyRequirements::default(),
        preference: PolicyPreference {
            preferred_tier: Some(ModelTier::Standard),
            ..PolicyPreference::default()
        },
        fallback: PolicyFallback::Escalate {
            enabled: true,
            max_steps: 2,
        },
    }
}

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
                .map_or(false, |ua| ua.to_lowercase().contains(&value.to_lowercase())),
            ClientMatcher::ApiKeyPrefix { value } => ctx
                .api_key_prefix
                .map_or(false, |key| key.starts_with(value)),
            ClientMatcher::Header { name, value } => ctx.headers.iter().any(|(n, v)| {
                n.eq_ignore_ascii_case(name) && v == value
            }),
            ClientMatcher::ModelPrefix { value } => ctx.model.starts_with(value),
            ClientMatcher::ClientId { value } => {
                ctx.client_id.map_or(false, |id| id == value)
            }
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

// ------------------------------------------------------------ Tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelCapabilities;

    fn ctx(model: &str) -> MatchContext<'_> {
        MatchContext {
            client_id: None,
            application: None,
            model,
            streaming: false,
            has_tools: false,
            has_vision: false,
            task: None,
        }
    }

    #[test]
    fn empty_matchers_always_match() {
        let p = default_policy();
        assert!(p.matches(&ctx("any")));
    }

    #[test]
    fn disabled_policy_never_matches() {
        let mut p = default_policy();
        p.enabled = false;
        assert!(!p.matches(&ctx("any")));
    }

    #[test]
    fn client_matcher() {
        let mut p = default_policy();
        p.matchers = vec![PolicyMatcher::Client {
            value: "codex".into(),
        }];
        let mut c = ctx("m");
        c.client_id = Some("codex");
        assert!(p.matches(&c));
        c.client_id = Some("other");
        assert!(!p.matches(&c));
    }

    #[test]
    fn capability_requirement_filters() {
        // With strict_capabilities = true, unknown capabilities are rejected.
        let reqs = PolicyRequirements {
            required_capabilities: vec![Capability::Vision],
            strict_capabilities: true,
            ..Default::default()
        };
        let caps = ModelCapabilities {
            vision: false,
            ..Default::default()
        };
        let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
        assert!(!check.eligible);
        assert!(matches!(
            check.reasons[0],
            RejectionReason::MissingCapability(Capability::Vision)
        ));
    }

    #[test]
    fn capability_unknown_is_soft_fallback_when_not_strict() {
        // With strict_capabilities = false (default), unknown capabilities
        // are a soft fallback — the candidate remains eligible.
        let reqs = PolicyRequirements {
            required_capabilities: vec![Capability::Vision],
            strict_capabilities: false,
            ..Default::default()
        };
        let caps = ModelCapabilities {
            vision: false,
            ..Default::default()
        };
        let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
        assert!(check.eligible, "unknown capability should be soft fallback");
    }

    #[test]
    fn tier_bounds_filter() {
        let reqs = PolicyRequirements {
            min_tier: Some(ModelTier::Standard),
            max_tier: Some(ModelTier::Reasoning),
            ..Default::default()
        };
        let caps = ModelCapabilities::default();

        // Below min
        let check = reqs.check("m", "p", Some(ModelTier::Fast), &caps, false);
        assert!(!check.eligible);

        // At min
        let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
        assert!(check.eligible);

        // At max
        let check = reqs.check("m", "p", Some(ModelTier::Reasoning), &caps, false);
        assert!(check.eligible);

        // Above max
        let check = reqs.check("m", "p", Some(ModelTier::Frontier), &caps, false);
        assert!(!check.eligible);
    }

    #[test]
    fn forbidden_provider_filter() {
        let reqs = PolicyRequirements {
            forbidden_providers: vec!["bad-provider".into()],
            ..Default::default()
        };
        let caps = ModelCapabilities::default();
        let check = reqs.check("m", "bad-provider", Some(ModelTier::Standard), &caps, false);
        assert!(!check.eligible);
        let check = reqs.check("m", "good-provider", Some(ModelTier::Standard), &caps, false);
        assert!(check.eligible);
    }

    #[test]
    fn circuit_open_rejects() {
        let reqs = PolicyRequirements::default();
        let caps = ModelCapabilities::default();
        let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, true);
        assert!(!check.eligible);
        assert!(matches!(check.reasons[0], RejectionReason::CircuitOpen));
    }

    #[test]
    fn all_constraints_pass() {
        let reqs = PolicyRequirements {
            required_capabilities: vec![Capability::Tools],
            min_tier: Some(ModelTier::Fast),
            ..Default::default()
        };
        let caps = ModelCapabilities {
            tools: true,
            ..Default::default()
        };
        let check = reqs.check("m", "p", Some(ModelTier::Standard), &caps, false);
        assert!(check.eligible);
        assert!(check.reasons.is_empty());
    }

    #[test]
    fn policy_matching_with_multiple_matchers() {
        let mut p = default_policy();
        p.matchers = vec![
            PolicyMatcher::Client { value: "codex".into() },
            PolicyMatcher::HasTools { value: true },
        ];
        let mut c = ctx("m");
        c.client_id = Some("codex");
        c.has_tools = true;
        assert!(p.matches(&c));

        // Missing one matcher
        c.has_tools = false;
        assert!(!p.matches(&c));
    }

    #[test]
    fn model_prefix_matcher() {
        let mut p = default_policy();
        p.matchers = vec![PolicyMatcher::ModelPrefix { value: "gpt".into() }];
        assert!(p.matches(&ctx("gpt-4")));
        assert!(p.matches(&ctx("gpt-5.3-sol")));
        assert!(!p.matches(&ctx("claude-3")));
    }

    // ------------------------------------------------ TaskProfile

    #[test]
    fn task_profile_simple_request() {
        let req = ChatRequest::new("gpt-4", crate::ir::Dialect::OpenAI);
        let profile = TaskProfile::from_request(&req);

        assert_eq!(profile.complexity, Complexity::Simple);
        assert_eq!(profile.task_type, TaskType::Chat);
        assert!(!profile.has_tools);
        assert!(!profile.has_vision);
        assert!(!profile.streaming);
        assert_eq!(profile.suggested_tier(), ModelTier::Fast);
    }

    #[test]
    fn task_profile_with_tools() {
        let mut req = ChatRequest::new("gpt-4", crate::ir::Dialect::OpenAI);
        req.tools = vec![crate::ir::ToolDef {
            name: "search".into(),
            description: Some("Search the web".into()),
            input_schema: serde_json::json!({}),
            cache_control: None,
        }];
        let profile = TaskProfile::from_request(&req);

        assert_eq!(profile.complexity, Complexity::Standard);
        assert_eq!(profile.task_type, TaskType::ToolUse);
        assert!(profile.has_tools);
        assert_eq!(profile.suggested_tier(), ModelTier::Standard);
    }

    #[test]
    fn task_profile_with_vision() {
        let mut req = ChatRequest::new("gpt-4", crate::ir::Dialect::OpenAI);
        req.messages = vec![crate::ir::Message {
            role: crate::ir::Role::User,
            content: vec![crate::ir::ContentBlock::Image {
                source: crate::ir::MediaSource::Base64 {
                    media_type: "image/png".into(),
                    data: "abc".into(),
                },
            }],
        }];
        let profile = TaskProfile::from_request(&req);

        assert!(profile.has_vision);
        assert_eq!(profile.task_type, TaskType::Vision);
        assert!(profile.required_capabilities.contains(&Capability::Vision));
    }

    #[test]
    fn task_profile_with_thinking_is_complex() {
        let mut req = ChatRequest::new("gpt-4", crate::ir::Dialect::OpenAI);
        req.thinking = Some(crate::ir::ThinkingConfig {
            enabled: true,
            budget_tokens: Some(10000),
        });
        let profile = TaskProfile::from_request(&req);

        assert_eq!(profile.complexity, Complexity::Complex);
        assert_eq!(profile.suggested_tier(), ModelTier::Reasoning);
    }

    #[test]
    fn task_profile_streaming() {
        let mut req = ChatRequest::new("gpt-4", crate::ir::Dialect::OpenAI);
        req.stream = true;
        let profile = TaskProfile::from_request(&req);

        assert!(profile.streaming);
    }

    #[test]
    fn task_profile_max_tokens_estimate() {
        let mut req = ChatRequest::new("gpt-4", crate::ir::Dialect::OpenAI);
        req.max_tokens = Some(8192);
        let profile = TaskProfile::from_request(&req);

        assert_eq!(profile.estimated_output_tokens, 8192);
    }

    #[test]
    fn task_profile_default_max_tokens() {
        let req = ChatRequest::new("gpt-4", crate::ir::Dialect::OpenAI);
        let profile = TaskProfile::from_request(&req);

        assert_eq!(profile.estimated_output_tokens, 4096);
    }

    #[test]
    fn suggested_tier_all_complexities() {
        let mut profile = TaskProfile {
            tier: None,
            required_capabilities: vec![],
            context_tokens: 0,
            estimated_output_tokens: 0,
            streaming: false,
            has_tools: false,
            has_vision: false,
            complexity: Complexity::Simple,
            task_type: TaskType::Chat,
        };

        assert_eq!(profile.suggested_tier(), ModelTier::Fast);
        profile.complexity = Complexity::Standard;
        assert_eq!(profile.suggested_tier(), ModelTier::Standard);
        profile.complexity = Complexity::Complex;
        assert_eq!(profile.suggested_tier(), ModelTier::Reasoning);
        profile.complexity = Complexity::Frontier;
        assert_eq!(profile.suggested_tier(), ModelTier::Frontier);
    }

    // ------------------------------------------------ MinComplexity / TaskType matchers

    fn profile_with_complexity(c: Complexity) -> TaskProfile {
        TaskProfile {
            tier: None,
            required_capabilities: vec![],
            context_tokens: 0,
            estimated_output_tokens: 0,
            streaming: false,
            has_tools: false,
            has_vision: false,
            complexity: c,
            task_type: TaskType::Chat,
        }
    }

    #[test]
    fn min_complexity_matcher() {
        let mut p = default_policy();
        p.matchers = vec![PolicyMatcher::MinComplexity {
            value: Complexity::Standard,
        }];

        let profile_simple = profile_with_complexity(Complexity::Simple);
        let profile_standard = profile_with_complexity(Complexity::Standard);
        let profile_complex = profile_with_complexity(Complexity::Complex);

        let mut c = ctx("m");
        c.task = Some(&profile_simple);
        assert!(!p.matches(&c), "Simple should not match MinComplexity::Standard");

        c.task = Some(&profile_standard);
        assert!(p.matches(&c), "Standard should match MinComplexity::Standard");

        c.task = Some(&profile_complex);
        assert!(p.matches(&c), "Complex should match MinComplexity::Standard");
    }

    #[test]
    fn min_complexity_matcher_no_task() {
        let mut p = default_policy();
        p.matchers = vec![PolicyMatcher::MinComplexity {
            value: Complexity::Simple,
        }];
        let c = ctx("m");
        assert!(!p.matches(&c), "No task profile should not match");
    }

    #[test]
    fn task_type_matcher() {
        let mut p = default_policy();
        p.matchers = vec![PolicyMatcher::TaskType {
            value: TaskType::ToolUse,
        }];

        let mut profile_tool = profile_with_complexity(Complexity::Standard);
        profile_tool.task_type = TaskType::ToolUse;
        let mut c = ctx("m");
        c.task = Some(&profile_tool);
        assert!(p.matches(&c));

        let mut profile_chat = profile_with_complexity(Complexity::Standard);
        profile_chat.task_type = TaskType::Chat;
        c.task = Some(&profile_chat);
        assert!(!p.matches(&c));
    }

    #[test]
    fn task_type_matcher_no_task() {
        let mut p = default_policy();
        p.matchers = vec![PolicyMatcher::TaskType {
            value: TaskType::Chat,
        }];
        let c = ctx("m");
        assert!(!p.matches(&c), "No task profile should not match");
    }

    #[test]
    fn complexity_ordering() {
        assert!(Complexity::Simple < Complexity::Standard);
        assert!(Complexity::Standard < Complexity::Complex);
        assert!(Complexity::Complex < Complexity::Frontier);
    }

    #[test]
    fn task_type_default_is_chat() {
        assert_eq!(TaskType::default(), TaskType::Chat);
    }

    #[test]
    fn complexity_default_is_standard() {
        assert_eq!(Complexity::default(), Complexity::Standard);
    }

    // ------------------------------------------------ Scoring

    fn scoring_ctx<'a>(health: f64, latency: f64, cost: Option<f64>, priority: i32, tier: Option<ModelTier>) -> ScoringContext<'a> {
        ScoringContext {
            health,
            avg_latency_ms: latency,
            input_per_mtok: cost,
            output_per_mtok: cost,
            priority,
            tier,
            task: None,
        }
    }

    #[test]
    fn perfect_candidate_scores_high() {
        let pref = PolicyPreference::default();
        let ctx = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
        let s = score_candidate(&pref, &ctx);
        assert!(s.total_score >= 0.85, "expected >=0.85, got {}", s.total_score);
        assert_eq!(s.breakdown.health, 1.0);
        assert!((s.breakdown.latency - 1.0).abs() < 0.001);
        assert!((s.breakdown.cost - 1.0).abs() < 0.001);
        assert_eq!(s.breakdown.priority, 1.0);
    }

    #[test]
    fn unhealthy_candidate_penalized() {
        let pref = PolicyPreference {
            health_weight: 1.0,
            latency_weight: 0.0,
            cost_weight: 0.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
            ..Default::default()
        };
        let healthy = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
        let unhealthy = scoring_ctx(0.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
        let s_h = score_candidate(&pref, &healthy);
        let s_u = score_candidate(&pref, &unhealthy);
        assert!(s_h.total_score > s_u.total_score);
    }

    #[test]
    fn lower_latency_scores_higher() {
        let pref = PolicyPreference {
            health_weight: 0.0,
            latency_weight: 1.0,
            cost_weight: 0.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
            ..Default::default()
        };
        let fast = scoring_ctx(1.0, 200.0, Some(1.0), 0, Some(ModelTier::Standard));
        let slow = scoring_ctx(1.0, 2000.0, Some(1.0), 0, Some(ModelTier::Standard));
        let s_f = score_candidate(&pref, &fast);
        let s_s = score_candidate(&pref, &slow);
        assert!(s_f.total_score > s_s.total_score, "fast {} > slow {}", s_f.total_score, s_s.total_score);
        // 200ms → 1000/200 = 1.0 (clamped), 2000ms → 1000/2000 = 0.5
        assert!((s_f.breakdown.latency - 1.0).abs() < 0.001);
        assert!((s_s.breakdown.latency - 0.5).abs() < 0.001);
    }

    #[test]
    fn cheaper_cost_scores_higher() {
        let pref = PolicyPreference {
            health_weight: 0.0,
            latency_weight: 0.0,
            cost_weight: 1.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
            ..Default::default()
        };
        let cheap = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
        let expensive = scoring_ctx(1.0, 100.0, Some(50.0), 0, Some(ModelTier::Standard));
        let s_c = score_candidate(&pref, &cheap);
        let s_e = score_candidate(&pref, &expensive);
        assert!(s_c.total_score > s_e.total_score);
    }

    #[test]
    fn lower_priority_number_scores_higher() {
        let pref = PolicyPreference {
            health_weight: 0.0,
            latency_weight: 0.0,
            cost_weight: 0.0,
            priority_weight: 1.0,
            tier_weight: 0.0,
            ..Default::default()
        };
        let high_prio = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
        let low_prio = scoring_ctx(1.0, 100.0, Some(1.0), 50, Some(ModelTier::Standard));
        let s_h = score_candidate(&pref, &high_prio);
        let s_l = score_candidate(&pref, &low_prio);
        assert!(s_h.total_score > s_l.total_score);
    }

    #[test]
    fn closer_tier_scores_higher() {
        let pref = PolicyPreference {
            preferred_tier: Some(ModelTier::Standard),
            health_weight: 0.0,
            latency_weight: 0.0,
            cost_weight: 0.0,
            priority_weight: 0.0,
            tier_weight: 1.0,
            ..Default::default()
        };
        let exact = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
        let close = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Reasoning));
        let far = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Frontier));
        let s_exact = score_candidate(&pref, &exact);
        let s_close = score_candidate(&pref, &close);
        let s_far = score_candidate(&pref, &far);
        assert!(s_exact.total_score > s_close.total_score);
        assert!(s_close.total_score > s_far.total_score);
    }

    #[test]
    fn unmeasured_latency_benefit_of_doubt() {
        let pref = PolicyPreference {
            health_weight: 0.0,
            latency_weight: 1.0,
            cost_weight: 0.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
            ..Default::default()
        };
        let unmeasured = scoring_ctx(1.0, 0.0, Some(1.0), 0, Some(ModelTier::Standard));
        let s = score_candidate(&pref, &unmeasured);
        assert!((s.breakdown.latency - 1.0).abs() < 0.001, "unmeasured should get full latency score, got {}", s.breakdown.latency);
    }

    #[test]
    fn unknown_cost_is_neutral() {
        let pref = PolicyPreference {
            health_weight: 0.0,
            latency_weight: 0.0,
            cost_weight: 1.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
            ..Default::default()
        };
        let unknown = scoring_ctx(1.0, 100.0, None, 0, Some(ModelTier::Standard));
        let s = score_candidate(&pref, &unknown);
        assert!((s.breakdown.cost - 0.5).abs() < 0.001);
    }

    #[test]
    fn weights_are_normalized() {
        // Weights that don't sum to 1 should still produce a [0,1] score.
        let pref = PolicyPreference {
            preferred_tier: None,
            health_weight: 5.0,
            latency_weight: 5.0,
            cost_weight: 5.0,
            priority_weight: 5.0,
            tier_weight: 5.0,
        };
        let ctx = scoring_ctx(0.5, 500.0, Some(5.0), 25, Some(ModelTier::Standard));
        let s = score_candidate(&pref, &ctx);
        assert!(s.total_score >= 0.0 && s.total_score <= 1.0, "score out of range: {}", s.total_score);
    }

    #[test]
    fn zero_weights_produce_zero_score() {
        let pref = PolicyPreference {
            preferred_tier: None,
            health_weight: 0.0,
            latency_weight: 0.0,
            cost_weight: 0.0,
            priority_weight: 0.0,
            tier_weight: 0.0,
        };
        let ctx = scoring_ctx(1.0, 100.0, Some(1.0), 0, Some(ModelTier::Standard));
        let s = score_candidate(&pref, &ctx);
        assert_eq!(s.total_score, 0.0);
    }

    #[test]
    fn tier_distance_symmetry() {
        assert_eq!(tier_distance(ModelTier::Fast, ModelTier::Frontier), 3.0);
        assert_eq!(tier_distance(ModelTier::Frontier, ModelTier::Fast), 3.0);
        assert_eq!(tier_distance(ModelTier::Standard, ModelTier::Standard), 0.0);
        assert_eq!(tier_distance(ModelTier::Fast, ModelTier::Reasoning), 2.0);
    }

    // ------------------------------------------------ Client Profiles

    fn codex_profile() -> ClientProfile {
        ClientProfile {
            id: "codex".into(),
            name: "Codex CLI".into(),
            matchers: vec![ClientMatcher::ClientId {
                value: "codex".into(),
            }],
            policy_id: "code-policy".into(),
        }
    }

    fn bot_profile() -> ClientProfile {
        ClientProfile {
            id: "bot".into(),
            name: "Bot via API key".into(),
            matchers: vec![ClientMatcher::ApiKeyPrefix {
                value: "sk-bot-".into(),
            }],
            policy_id: "bot-policy".into(),
        }
    }

    fn ua_profile() -> ClientProfile {
        ClientProfile {
            id: "curl".into(),
            name: "curl client".into(),
            matchers: vec![ClientMatcher::UserAgent {
                value: "curl".into(),
            }],
            policy_id: "default".into(),
        }
    }

    fn header_profile() -> ClientProfile {
        ClientProfile {
            id: "custom".into(),
            name: "Custom header client".into(),
            matchers: vec![ClientMatcher::Header {
                name: "X-App".into(),
                value: "myapp".into(),
            }],
            policy_id: "custom-policy".into(),
        }
    }

    fn model_prefix_profile() -> ClientProfile {
        ClientProfile {
            id: "gpt-user".into(),
            name: "GPT model user".into(),
            matchers: vec![ClientMatcher::ModelPrefix {
                value: "gpt".into(),
            }],
            policy_id: "gpt-policy".into(),
        }
    }

    fn client_ctx<'a>(
        client_id: Option<&'a str>,
        user_agent: Option<&'a str>,
        api_key_prefix: Option<&'a str>,
        headers: &'a [(&'a str, &'a str)],
        model: &'a str,
    ) -> ClientContext<'a> {
        ClientContext {
            client_id,
            user_agent,
            api_key_prefix,
            application: None,
            headers,
            model,
        }
    }

    #[test]
    fn client_matcher_client_id() {
        let profile = codex_profile();
        let ctx_match = client_ctx(Some("codex"), None, None, &[], "gpt-4");
        let ctx_miss = client_ctx(Some("other"), None, None, &[], "gpt-4");
        let ctx_none = client_ctx(None, None, None, &[], "gpt-4");
        assert!(profile.matches(&ctx_match));
        assert!(!profile.matches(&ctx_miss));
        assert!(!profile.matches(&ctx_none));
    }

    #[test]
    fn client_matcher_api_key_prefix() {
        let profile = bot_profile();
        let ctx_match = client_ctx(None, None, Some("sk-bot-abc123"), &[], "gpt-4");
        let ctx_miss = client_ctx(None, None, Some("sk-proj-abc"), &[], "gpt-4");
        let ctx_none = client_ctx(None, None, None, &[], "gpt-4");
        assert!(profile.matches(&ctx_match));
        assert!(!profile.matches(&ctx_miss));
        assert!(!profile.matches(&ctx_none));
    }

    #[test]
    fn client_matcher_user_agent_case_insensitive() {
        let profile = ua_profile();
        let ctx_lower = client_ctx(None, Some("curl/7.68.0"), None, &[], "gpt-4");
        let ctx_upper = client_ctx(None, Some("CURL/7.68.0"), None, &[], "gpt-4");
        let ctx_miss = client_ctx(None, Some("python-requests/2.28"), None, &[], "gpt-4");
        assert!(profile.matches(&ctx_lower));
        assert!(profile.matches(&ctx_upper));
        assert!(!profile.matches(&ctx_miss));
    }

    #[test]
    fn client_matcher_header() {
        let profile = header_profile();
        let headers_match = [("X-App", "myapp")];
        let headers_miss = [("X-App", "other")];
        let headers_case = [("x-app", "myapp")];
        let ctx_match = client_ctx(None, None, None, &headers_match, "gpt-4");
        let ctx_miss = client_ctx(None, None, None, &headers_miss, "gpt-4");
        let ctx_case = client_ctx(None, None, None, &headers_case, "gpt-4");
        assert!(profile.matches(&ctx_match));
        assert!(!profile.matches(&ctx_miss));
        // Header name is case-insensitive
        assert!(profile.matches(&ctx_case));
    }

    #[test]
    fn client_matcher_model_prefix() {
        let profile = model_prefix_profile();
        let ctx_match = client_ctx(None, None, None, &[], "gpt-4o");
        let ctx_miss = client_ctx(None, None, None, &[], "claude-3");
        assert!(profile.matches(&ctx_match));
        assert!(!profile.matches(&ctx_miss));
    }

    #[test]
    fn empty_matchers_never_match() {
        let profile = ClientProfile {
            id: "empty".into(),
            name: "Empty".into(),
            matchers: vec![],
            policy_id: "default".into(),
        };
        let ctx = client_ctx(Some("anything"), None, None, &[], "gpt-4");
        assert!(!profile.matches(&ctx));
    }

    #[test]
    fn multiple_matchers_all_must_pass() {
        let profile = ClientProfile {
            id: "multi".into(),
            name: "Multi".into(),
            matchers: vec![
                ClientMatcher::ClientId { value: "codex".into() },
                ClientMatcher::ModelPrefix { value: "gpt".into() },
            ],
            policy_id: "multi-policy".into(),
        };
        let ctx_both = client_ctx(Some("codex"), None, None, &[], "gpt-4");
        let ctx_partial = client_ctx(Some("codex"), None, None, &[], "claude-3");
        assert!(profile.matches(&ctx_both));
        assert!(!profile.matches(&ctx_partial));
    }

    #[test]
    fn resolve_client_returns_first_match() {
        let profiles = vec![bot_profile(), codex_profile(), ua_profile()];
        let ctx = client_ctx(Some("codex"), Some("curl/7.0"), Some("sk-bot-x"), &[], "gpt-4");
        // bot_profile matches on ApiKeyPrefix, which comes first
        let resolved = resolve_client(&profiles, &ctx);
        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap().id, "bot");
    }

    #[test]
    fn resolve_client_returns_none_when_no_match() {
        let profiles = vec![codex_profile(), bot_profile()];
        let ctx = client_ctx(Some("unknown"), None, None, &[], "gpt-4");
        assert!(resolve_client(&profiles, &ctx).is_none());
    }

    #[test]
    fn resolve_client_empty_profiles() {
        let profiles: Vec<ClientProfile> = vec![];
        let ctx = client_ctx(Some("codex"), None, None, &[], "gpt-4");
        assert!(resolve_client(&profiles, &ctx).is_none());
    }

    #[test]
    fn resolve_client_second_profile_matches() {
        let profiles = vec![codex_profile(), bot_profile()];
        let ctx = client_ctx(None, None, Some("sk-bot-key123"), &[], "gpt-4");
        let resolved = resolve_client(&profiles, &ctx);
        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap().id, "bot");
    }

    #[test]
    fn policy_config_serializes_clients() {
        let config = PolicyConfig {
            policies: vec![],
            default_policy: None,
            clients: vec![codex_profile()],
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"clients\""));
        assert!(json.contains("\"codex\""));
        let deserialized: PolicyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.clients.len(), 1);
        assert_eq!(deserialized.clients[0].id, "codex");
    }

    #[test]
    fn policy_config_clients_default_empty() {
        let json = r#"{"policies": []}"#;
        let config: PolicyConfig = serde_json::from_str(json).unwrap();
        assert!(config.clients.is_empty());
    }
}
