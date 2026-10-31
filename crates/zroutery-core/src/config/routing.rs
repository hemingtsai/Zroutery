//! How a tier picks among its candidates, and what the rectifier, classifier and vision layers add.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::model::ModelTier;
use super::model::NamingStyle;
use super::provider::default_true;
use crate::circuit_breaker::CircuitBreakerConfig;
use crate::classifier::DetectionConfig;
use crate::election::ScoringConfig;
use crate::policy::PolicyConfig;

/// How a tier picks among its candidate models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingStrategy {
    /// Strict priority order (lower priority number first). The sort is
    /// stable, so candidates with equal priority preserve the upstream
    /// (registry/policy) order instead of being reshuffled.
    #[default]
    Priority,
    /// Weighted random across all healthy candidates.
    WeightedRandom,
    /// Round robin across all healthy candidates, ignoring priority.
    RoundRobin,
    /// Lowest observed latency first.
    LowestLatency,
    /// The order an election decided from measured latency and price. Falls back
    /// to `Priority` until one has been held.
    Balanced,
}

/// Which request-repair rectifiers are active.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RectifierConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub thinking_signature: bool,
    #[serde(default = "default_true")]
    pub media_fallback: bool,
    #[serde(default = "default_true")]
    pub thinking_budget: bool,
    /// Serve a reasoning request through a model whose thinking support is
    /// unknown, with the reasoning controls removed.
    ///
    /// A reasoning request is a hint about how an answer should be produced,
    /// not content that would be lost: a model that cannot reason still
    /// answers. The alternative — rejecting the whole pool because no member
    /// declares support — makes a tier unusable for any client that selects a
    /// reasoning variant, which is the common shape for agent CLIs. The
    /// attempt is marked degraded, and the dropped controls are visible in the
    /// route decision, so the fallback is never silent.
    #[serde(default = "default_true")]
    pub thinking_fallback: bool,
    /// Repair an explicit unknown-parameter rejection of Codex's private
    /// internal_chat_message_metadata_passthrough member on Responses items.
    #[serde(default = "default_true")]
    pub codex_metadata: bool,
    /// Magpie Auto V10: stage Responses SSE and conditionally repair history.
    #[serde(default = "default_true")]
    pub codex_auto: bool,
}

impl Default for RectifierConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            thinking_signature: true,
            media_fallback: true,
            thinking_budget: true,
            thinking_fallback: true,
            codex_metadata: true,
            codex_auto: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingConfig {
    #[serde(default)]
    pub strategy: RoutingStrategy,
    /// Try the next candidate in the tier when a request fails.
    #[serde(default = "default_true")]
    pub failover: bool,
    /// Max upstream attempts for a single client request.
    #[serde(default = "RoutingConfig::default_attempts")]
    pub max_attempts: u32,
    /// Circuit breaker settings shared by every model.
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,
    /// Request rectifier toggles.
    #[serde(default)]
    pub rectifier: RectifierConfig,
    /// Legacy field, migrated into `circuit_breaker.failure_threshold` by
    /// [`RoutingConfig::apply_legacy`]. Kept only for deserializing old configs.
    #[serde(default, skip_serializing)]
    pub break_after_failures: Option<u32>,
    /// Legacy field, migrated into `circuit_breaker.timeout_secs`.
    #[serde(default, skip_serializing)]
    pub cooldown_secs: Option<u64>,
    /// When a client asks for an unknown model id, fall back to this tier
    /// instead of returning 404.
    #[serde(default)]
    pub unknown_model_fallback: Option<ModelTier>,
    /// Map arbitrary client model ids onto a tier, e.g.
    /// `claude-sonnet-4-5-20250929 -> standard`. Used by Anthropic native clients.
    #[serde(default)]
    pub client_aliases: BTreeMap<String, ModelTier>,
    /// Interpret client model ids containing legacy class names (`opus`/`sonnet`/`haiku`) as the
    /// matching tier, so Anthropic-native tools work out of the box. This only
    /// affects how *incoming* ids are read; upstream models are always
    /// classified by hand.
    #[serde(default = "default_true")]
    pub match_claude_names: bool,
    /// How `Balanced` weighs latency against price.
    #[serde(default)]
    pub scoring: ScoringConfig,
    /// Hold an election when the proxy starts, so the pinned order reflects today
    /// rather than whenever it was last run. Costs one tiny request per model.
    #[serde(default = "default_true")]
    pub elect_on_start: bool,
    /// How tiers and virtual model ids are presented to clients.
    #[serde(default)]
    pub naming_style: NamingStyle,
    /// Filter candidates by capability match before routing.
    /// When enabled and the request carries `required_capabilities`,
    /// candidates whose capabilities don't satisfy the requirements are
    /// excluded. If no candidate passes, the unfiltered list is used as a
    /// soft fallback so the request is not rejected just because no model
    /// declares every capability.
    #[serde(default = "default_true")]
    pub capability_filter: bool,
    /// When `true`, requests whose `required_capabilities` no candidate
    /// satisfies are rejected with an error instead of falling back to the
    /// unfiltered candidate list.  Only meaningful when `capability_filter`
    /// is also `true`.
    #[serde(default)]
    pub strict_capability_filter: bool,
    /// Named routing policies with eligibility requirements.
    #[serde(default)]
    pub policies: PolicyConfig,
}

impl RoutingConfig {
    fn default_attempts() -> u32 {
        3
    }

    /// Fold legacy `break_after_failures` / `cooldown_secs` into the nested
    /// circuit breaker settings.
    pub fn apply_legacy(&mut self) {
        if let Some(failure_threshold) = self.break_after_failures.take() {
            self.circuit_breaker.failure_threshold = failure_threshold;
        }
        if let Some(timeout_secs) = self.cooldown_secs.take() {
            self.circuit_breaker.timeout_secs = timeout_secs;
        }
    }
}

impl Default for RoutingConfig {
    fn default() -> Self {
        RoutingConfig {
            strategy: RoutingStrategy::default(),
            failover: true,
            max_attempts: Self::default_attempts(),
            circuit_breaker: CircuitBreakerConfig::default(),
            rectifier: RectifierConfig::default(),
            break_after_failures: None,
            cooldown_secs: None,
            unknown_model_fallback: None,
            client_aliases: BTreeMap::new(),
            match_claude_names: true,
            scoring: ScoringConfig::default(),
            elect_on_start: true,
            naming_style: NamingStyle::default(),
            capability_filter: true,
            strict_capability_filter: false,
            policies: PolicyConfig::default(),
        }
    }
}

/// One member of the Auto Mode classifier pool.
///
/// `model` is an existing exposed model id (or alias): the classifier pool is a
/// *selection* of models, not a second set of provider credentials. Everything
/// the entry already has — provider, secret, protocol, pricing, health — is
/// reused as-is, which is why there is no base url or key here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierCandidate {
    /// Exposed id (or alias) of the model to use, e.g. `zai-glm-5.3`.
    pub model: String,
    /// Lower wins, exactly like a tier member's priority.
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Routing policy for Claude Code Auto Mode classifier side queries.
///
/// Main requests and classifier requests are two orthogonal dimensions: a
/// `*-class` id routes by *capability*, this routes by *purpose*. The pools
/// share model health (a provider that is down is down for both) but keep
/// their own strategy, failover and attempt budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierConfig {
    /// Master switch: off means every request is routed as a main request.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub strategy: RoutingStrategy,
    #[serde(default = "default_true")]
    pub failover: bool,
    /// Max upstream attempts for one classifier request.
    #[serde(default = "ClassifierConfig::default_attempts")]
    pub max_attempts: u32,
    /// The classifier pool, in no particular order; the strategy orders it.
    /// Always serialized so the GUI round-trips the list without special cases.
    #[serde(default)]
    pub candidates: Vec<ClassifierCandidate>,
    #[serde(default)]
    pub detection: DetectionConfig,
}

impl ClassifierConfig {
    fn default_attempts() -> u32 {
        2
    }
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        ClassifierConfig {
            enabled: false,
            strategy: RoutingStrategy::Priority,
            failover: true,
            max_attempts: Self::default_attempts(),
            candidates: Vec::new(),
            detection: DetectionConfig::default(),
        }
    }
}

/// Vision fallback: turning images into descriptions for models that cannot
/// see.
///
/// A text model in front of an image either fails the whole request or
/// silently loses the image's meaning. The fallback routes the image, once,
/// through a model that can see, and puts the description where the text was
/// going — the conversation continues with the image's content instead of a
/// placeholder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VisionConfig {
    /// Master switch. Off: images go to non-vision models as-is (and the old
    /// placeholder rectifier still catches the rejection).
    #[serde(default)]
    pub enabled: bool,
    /// Exposed id (or alias) of an existing model that can describe images.
    /// Reuses the provider, secret and pricing of that model, exactly like a
    /// classifier candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// What to put when even the vision model fails or none is configured:
    /// a placeholder beats a dead request, but it must be honest about what
    /// happened to the image.
    pub placeholder: String,
}

impl Default for VisionConfig {
    fn default() -> Self {
        VisionConfig {
            enabled: false,
            model: None,
            placeholder: "[Unsupported Image]".into(),
        }
    }
}
