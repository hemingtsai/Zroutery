//! Persistent configuration: providers, model registry entries, routing policy
//! and local server settings.
//!
//! Secrets are never stored here. A provider only holds a `key_ref` that is
//! resolved through a [`SecretStore`] (OS keychain in the desktop app).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::billing::{BalanceConfig, BaseDepth, Pricing};
use crate::budget::Budget;
use crate::circuit_breaker::CircuitBreakerConfig;
use crate::classifier::DetectionConfig;
use crate::election::ScoringConfig;
use crate::ir::Dialect;
pub use crate::protocol::ProviderQuirks;

/// Capability tier a model belongs to. Assigned manually by the user; Zroutery
/// never guesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    /// Cheapest and fastest — simple tasks.
    #[serde(alias = "haiku")]
    Fast,
    /// Balanced workhorse — general purpose.
    #[serde(alias = "sonnet")]
    Standard,
    /// Most capable reasoning — complex tasks.
    #[serde(alias = "opus")]
    Reasoning,
    /// Frontier — pushing the boundary of capability.
    Frontier,
}

impl ModelTier {
    pub const ALL: [ModelTier; 4] = [
        ModelTier::Fast,
        ModelTier::Standard,
        ModelTier::Reasoning,
        ModelTier::Frontier,
    ];

    /// The virtual model id exposed to clients, e.g. "standard-class".
    pub fn virtual_id(&self) -> &'static str {
        match self {
            ModelTier::Fast => "fast-class",
            ModelTier::Standard => "standard-class",
            ModelTier::Reasoning => "reasoning-class",
            ModelTier::Frontier => "frontier-class",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ModelTier::Fast => "fast",
            ModelTier::Standard => "standard",
            ModelTier::Reasoning => "reasoning",
            ModelTier::Frontier => "frontier",
        }
    }

    pub fn from_virtual_id(id: &str) -> Option<ModelTier> {
        match id {
            // Internal
            "fast-class" | "standard-class" | "reasoning-class" | "frontier-class" |
            // Anthropic
            "haiku-class" | "sonnet-class" | "opus-class" | "fable-class" |
            // OpenAI
            "luna-class" | "terra-class" | "sol-class" | "astra-class" => {
                // Parse by prefix to determine the tier.
                let lower = id.strip_suffix("-class").unwrap_or(id);
                if matches!(lower, "fast" | "haiku" | "luna") {
                    Some(ModelTier::Fast)
                } else if matches!(lower, "standard" | "sonnet" | "terra") {
                    Some(ModelTier::Standard)
                } else if matches!(lower, "reasoning" | "opus" | "sol") {
                    Some(ModelTier::Reasoning)
                } else if matches!(lower, "frontier" | "fable" | "astra") {
                    Some(ModelTier::Frontier)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Next tier up (for escalation). None if already at the top.
    pub fn higher(&self) -> Option<ModelTier> {
        match self {
            ModelTier::Fast => Some(ModelTier::Standard),
            ModelTier::Standard => Some(ModelTier::Reasoning),
            ModelTier::Reasoning => Some(ModelTier::Frontier),
            ModelTier::Frontier => None,
        }
    }

    /// Next tier down (for de-escalation). None if already at the bottom.
    pub fn lower(&self) -> Option<ModelTier> {
        match self {
            ModelTier::Fast => None,
            ModelTier::Standard => Some(ModelTier::Fast),
            ModelTier::Reasoning => Some(ModelTier::Standard),
            ModelTier::Frontier => Some(ModelTier::Reasoning),
        }
    }

    /// Display name for the given naming style.
    pub fn display_name(&self, style: NamingStyle) -> &'static str {
        match (self, style) {
            (ModelTier::Fast, NamingStyle::Internal) => "Fast",
            (ModelTier::Fast, NamingStyle::Anthropic) => "Haiku",
            (ModelTier::Fast, NamingStyle::OpenAI) => "Luna",
            (ModelTier::Standard, NamingStyle::Internal) => "Standard",
            (ModelTier::Standard, NamingStyle::Anthropic) => "Sonnet",
            (ModelTier::Standard, NamingStyle::OpenAI) => "Terra",
            (ModelTier::Reasoning, NamingStyle::Internal) => "Reasoning",
            (ModelTier::Reasoning, NamingStyle::Anthropic) => "Opus",
            (ModelTier::Reasoning, NamingStyle::OpenAI) => "Sol",
            (ModelTier::Frontier, NamingStyle::Internal) => "Frontier",
            (ModelTier::Frontier, NamingStyle::Anthropic) => "Fable",
            (ModelTier::Frontier, NamingStyle::OpenAI) => "Astra",
        }
    }

    /// Virtual model id for the given naming style, e.g. "haiku-class".
    pub fn virtual_id_styled(&self, style: NamingStyle) -> &'static str {
        match (self, style) {
            (ModelTier::Fast, NamingStyle::Internal) => "fast-class",
            (ModelTier::Fast, NamingStyle::Anthropic) => "haiku-class",
            (ModelTier::Fast, NamingStyle::OpenAI) => "luna-class",
            (ModelTier::Standard, NamingStyle::Internal) => "standard-class",
            (ModelTier::Standard, NamingStyle::Anthropic) => "sonnet-class",
            (ModelTier::Standard, NamingStyle::OpenAI) => "terra-class",
            (ModelTier::Reasoning, NamingStyle::Internal) => "reasoning-class",
            (ModelTier::Reasoning, NamingStyle::Anthropic) => "opus-class",
            (ModelTier::Reasoning, NamingStyle::OpenAI) => "sol-class",
            (ModelTier::Frontier, NamingStyle::Internal) => "frontier-class",
            (ModelTier::Frontier, NamingStyle::Anthropic) => "fable-class",
            (ModelTier::Frontier, NamingStyle::OpenAI) => "astra-class",
        }
    }
}

/// How tiers and virtual model ids are presented to clients.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NamingStyle {
    /// Internal names: fast, standard, reasoning, frontier.
    #[default]
    Internal,
    /// Anthropic-style: Haiku, Sonnet, Opus, Fable.
    Anthropic,
    /// OpenAI-style: Luna, Terra, Sol, Astra.
    OpenAI,
}

/// Declared capabilities of a model. Independent of tier.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub thinking: bool,
    #[serde(default)]
    pub structured_output: bool,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub video: bool,
    #[serde(default)]
    pub files: bool,
}

/// Upstream wire protocol of a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Anthropic Messages API (`/v1/messages`, `x-api-key` header).
    Anthropic,
    #[serde(rename = "openai_compatible", alias = "open_a_i_compatible")]
    /// OpenAI Chat Completions API and every compatible clone (DeepSeek, Groq,
    /// Ollama, vLLM, OpenRouter, ...).
    OpenAICompatible,
}

impl ProviderKind {
    pub fn dialect(&self) -> Dialect {
        match self {
            ProviderKind::Anthropic => Dialect::Anthropic,
            ProviderKind::OpenAICompatible => Dialect::OpenAI,
        }
    }

    /// Default base url shown in the GUI when creating a provider.
    pub fn default_base_url(&self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "https://api.anthropic.com",
            ProviderKind::OpenAICompatible => "https://api.openai.com/v1",
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_weight() -> u32 {
    1
}
fn default_timeout() -> u64 {
    600
}
fn default_connect_timeout() -> u64 {
    15
}

/// One upstream account/endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Stable internal id (uuid or slug).
    pub id: String,
    /// Human readable name shown in the GUI, e.g. "DeepSeek".
    pub name: String,
    pub kind: ProviderKind,
    /// Base url without the endpoint path, e.g. `https://api.deepseek.com/v1`.
    pub base_url: String,
    /// Lookup key for the secret store. Empty means "no auth" (local models).
    #[serde(default)]
    pub key_ref: String,
    /// Extra headers merged into every upstream request.
    #[serde(default)]
    pub extra_headers: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Whole request timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_secs: u64,
    /// Anthropic API version header. Ignored for OpenAI compatible providers.
    #[serde(default)]
    pub anthropic_version: Option<String>,
    /// Whether to impersonate Claude Code client (User-Agent, x-app, anthropic-beta headers
    /// and system prompt identity line).
    #[serde(default)]
    pub impersonate_claude_code: bool,
    /// Send the key as `Authorization: Bearer <key>` in addition to
    /// `x-api-key`, for Anthropic-protocol relays whose gateway reads the
    /// Bearer header — some reject a request carrying only `x-api-key` with
    /// "Authorization 格式错误".
    #[serde(default)]
    pub bearer_auth: bool,
    #[serde(default)]
    pub quirks: ProviderQuirks,
    /// How to ask this provider for the remaining credit, when it can be asked.
    #[serde(default)]
    pub balance: BalanceConfig,
}

impl ProviderConfig {
    pub fn new(id: impl Into<String>, name: impl Into<String>, kind: ProviderKind) -> Self {
        let id = id.into();
        ProviderConfig {
            key_ref: format!("provider:{id}"),
            id,
            name: name.into(),
            base_url: kind.default_base_url().to_string(),
            kind,
            extra_headers: BTreeMap::new(),
            // Anthropic providers default to impersonation enabled for gateway compatibility
            impersonate_claude_code: kind == ProviderKind::Anthropic,
            bearer_auth: false,
            enabled: true,
            timeout_secs: default_timeout(),
            connect_timeout_secs: default_connect_timeout(),
            anthropic_version: None,
            quirks: ProviderQuirks::default(),
            balance: BalanceConfig::default(),
        }
    }

    /// How deep this provider's base URL already reaches, for appending metadata
    /// paths such as a balance endpoint.
    ///
    /// This mirrors the `/v1` normalisation in [`ProviderConfig::chat_url`]: a
    /// base that already ends in `/v1` is versioned, anything else is treated as
    /// the API root so balance paths can add the version themselves.
    pub fn base_depth(&self) -> BaseDepth {
        if self.base_url.trim_end_matches('/').ends_with("/v1") {
            BaseDepth::Versioned
        } else {
            BaseDepth::ApiRoot
        }
    }

    /// Full URL for the chat endpoint of this provider.
    pub fn chat_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        // Base URLs that already end in /v1 (ps.air-outer.com style) must not
        // grow a second /v1, and bare OpenAI-compatible hosts (Ollama) need
        // one. Mirrors the tolerance of `models_url`.
        let with_v1 = if base.ends_with("/v1") {
            base.to_string()
        } else {
            format!("{base}/v1")
        };
        match self.kind {
            ProviderKind::Anthropic => format!("{with_v1}/messages"),
            ProviderKind::OpenAICompatible => format!("{with_v1}/chat/completions"),
        }
    }

    /// Full URL for model listing, used by the "fetch models" button.
    pub fn models_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        match self.kind {
            ProviderKind::Anthropic => {
                if base.ends_with("/v1") {
                    format!("{base}/models")
                } else {
                    format!("{base}/v1/models")
                }
            }
            ProviderKind::OpenAICompatible => {
                // Standard OpenAI gateways serve models at /v1/models.
                // If the base_url already ends with /v1, just append /models;
                // otherwise append /v1/models so gateways like ps.air-outer.com work.
                if base.ends_with("/v1") {
                    format!("{base}/models")
                } else {
                    format!("{base}/v1/models")
                }
            }
        }
    }
}

/// Build the id clients use for a model: `<provider>-<upstream model>`.
///
/// Two providers offering the same upstream model therefore stay
/// distinguishable, e.g. `deepseek-deepseek-chat` next to
/// `openrouter-deepseek-chat`. Characters that are awkward in an id, such as the
/// slash in `deepseek/deepseek-chat`, become hyphens; the untouched name is
/// still what goes upstream.
pub fn qualified_id(provider_id: &str, upstream_model: &str) -> String {
    let raw = format!("{}-{}", provider_id.trim(), upstream_model.trim());
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    out
}

/// A model exposed by Zroutery, bound to one upstream provider.
///
/// Identity is the `(provider_id, upstream_model)` pair. The id clients see is
/// derived from it by [`ModelEntry::exposed_id`] instead of being stored, so it
/// can neither drift nor collide with the same model coming from another
/// provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub provider_id: String,
    /// Model id sent upstream, exactly as the provider spells it.
    pub upstream_model: String,
    /// Free-form id from configurations written before ids were derived.
    /// [`AppConfig::normalize`] folds it into `aliases` so existing clients keep
    /// working, and it is never written back.
    #[serde(default, rename = "id", skip_serializing)]
    legacy_id: Option<String>,
    /// Tier used by `*-class` virtual models. `None` means the user has not
    /// assigned a tier yet: it stays callable by exact id but never participates
    /// in tier routing.
    #[serde(default, alias = "class")]
    pub tier: Option<ModelTier>,
    /// Declared capabilities, independent of tier.
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    /// Lower value wins within a tier.
    #[serde(default)]
    pub priority: i32,
    /// Weight for random tie breaking among equal priorities.
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Legacy field — use `capabilities.tools` at runtime. Kept for serde compat.
    #[serde(default, skip_serializing)]
    #[deprecated(note = "use capabilities.tools instead")]
    pub supports_tools: bool,
    /// Legacy field — use `capabilities.vision` at runtime. Kept for serde compat.
    #[serde(default, skip_serializing)]
    #[deprecated(note = "use capabilities.vision instead")]
    pub supports_vision: bool,
    /// Legacy field — use `capabilities.thinking` at runtime. Kept for serde compat.
    #[serde(default, skip_serializing)]
    #[deprecated(note = "use capabilities.thinking instead")]
    pub supports_thinking: bool,
    /// Optional display name for `/v1/models`.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Extra ids that resolve to this exact model, for shorter names.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Hard cap on `max_tokens` sent upstream.
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    /// What the provider charges for this model. Entered by hand, like the tier;
    /// without it a request is logged with no cost rather than a guessed one.
    #[serde(default)]
    pub pricing: Option<Pricing>,
}

impl ModelEntry {
    /// Named rather than `new` so every call site has to state which argument is
    /// the provider now that both are plain strings.
    #[allow(deprecated)]
    pub fn for_upstream(
        provider_id: impl Into<String>,
        upstream_model: impl Into<String>,
        tier: Option<ModelTier>,
    ) -> Self {
        ModelEntry {
            provider_id: provider_id.into(),
            upstream_model: upstream_model.into(),
            legacy_id: None,
            tier,
            capabilities: ModelCapabilities {
                tools: true,
                ..ModelCapabilities::default()
            },
            priority: 0,
            weight: default_weight(),
            enabled: true,
            supports_tools: true,
            supports_vision: false,
            supports_thinking: false,
            display_name: None,
            aliases: Vec::new(),
            max_output_tokens: None,
            pricing: None,
        }
    }

    /// The id clients use, `<provider>-<upstream model>`.
    pub fn exposed_id(&self) -> String {
        qualified_id(&self.provider_id, &self.upstream_model)
    }

    /// True when `id` is this model's exposed id or one of its aliases.
    pub fn answers_to(&self, id: &str) -> bool {
        self.exposed_id() == id || self.aliases.iter().any(|a| a == id)
    }

    /// Identity of the model: which provider, which upstream name.
    pub fn key(&self) -> (&str, &str) {
        (&self.provider_id, &self.upstream_model)
    }

    pub fn with_alias(mut self, alias: impl Into<String>) -> Self {
        self.aliases.push(alias.into());
        self
    }

    pub fn with_priority(mut self, p: i32) -> Self {
        self.priority = p;
        self
    }
}

/// How a tier picks among its candidate models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingStrategy {
    /// Strict priority order; weight breaks ties randomly.
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
}

impl Default for RectifierConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            thinking_signature: true,
            media_fallback: true,
            thinking_budget: true,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Loopback by default. Changing this exposes your API keys to the network.
    #[serde(default = "ServerConfig::default_host")]
    pub host: String,
    #[serde(default = "ServerConfig::default_port")]
    pub port: u16,
    /// Require `Authorization: Bearer <token>` or `x-api-key: <token>`.
    #[serde(default = "default_true")]
    pub require_auth: bool,
    /// Local access token. Generated on first run.
    #[serde(default)]
    pub auth_token: String,
    /// Start the proxy as soon as the app launches.
    #[serde(default = "default_true")]
    pub autostart: bool,
    /// Allow browser origins to call the proxy.
    #[serde(default)]
    pub allow_cors: bool,
    /// Origins allowed when `allow_cors` is on. Empty means every origin, which
    /// `validate` reports as a warning.
    #[serde(default)]
    pub cors_origins: Vec<String>,
    /// Largest accepted request body, in mebibytes. Inline images make prompts
    /// big, but not unbounded.
    #[serde(default = "ServerConfig::default_body_limit_mib")]
    pub max_body_mib: usize,
    /// Number of request records kept in memory for the GUI.
    #[serde(default = "ServerConfig::default_log_limit")]
    pub log_limit: usize,
    /// Bypass the system proxy for upstream requests. Some proxies strip
    /// non-standard headers (x-app, x-stainless-*) needed for gateway
    /// fingerprint checks.
    #[serde(default)]
    pub bypass_proxy: bool,
}

impl ServerConfig {
    fn default_host() -> String {
        "127.0.0.1".to_string()
    }
    fn default_port() -> u16 {
        8787
    }
    fn default_log_limit() -> usize {
        500
    }
    fn default_body_limit_mib() -> usize {
        32
    }

    /// True when the server is reachable from outside this machine.
    pub fn is_exposed(&self) -> bool {
        !matches!(self.host.as_str(), "127.0.0.1" | "localhost" | "::1")
    }

    /// Body limit in bytes, clamped to something a proxy can actually buffer.
    pub fn max_body_bytes(&self) -> usize {
        self.max_body_mib.clamp(1, 512) * 1024 * 1024
    }

    /// True when CORS is on without an origin list, i.e. any site may call in.
    pub fn cors_is_wide_open(&self) -> bool {
        self.allow_cors && self.cors_origins.iter().all(|o| o.trim().is_empty())
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            host: Self::default_host(),
            port: Self::default_port(),
            require_auth: true,
            auth_token: String::new(),
            autostart: true,
            allow_cors: false,
            cors_origins: Vec::new(),
            max_body_mib: Self::default_body_limit_mib(),
            log_limit: Self::default_log_limit(),
            bypass_proxy: false,
        }
    }
}

/// How the desktop app behaves as a resident process: what autostart, the
/// launch, and the close button do.
///
/// One set of names on every platform — the tray is a tray, the close button
/// is the close button, and "minimize" is deliberately not used: minimizing is
/// a window state, hiding to the tray is a lifecycle, and conflating them is
/// how users lose track of whether the app is still running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowBehavior {
    /// Register with the OS to launch Zroutery at login.
    #[serde(default)]
    pub launch_on_login: bool,
    /// Start without showing the main window; the tray is the only presence.
    #[serde(default)]
    pub silent_start: bool,
    /// Closing the window keeps the process (and the gateway) alive in the
    /// tray. Off: closing the window quits, stopping the gateway with it.
    #[serde(default = "default_true")]
    pub keep_in_tray: bool,
}

impl Default for WindowBehavior {
    fn default() -> Self {
        WindowBehavior {
            launch_on_login: false,
            silent_start: false,
            keep_in_tray: true,
        }
    }
}

/// Root persisted document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
    /// Routing for Auto Mode classifier side queries. Orthogonal to
    /// `routing`, which stays about the main conversation.
    #[serde(default)]
    pub classifier: ClassifierConfig,
    /// Desktop application lifecycle. The gateway itself never reads this —
    /// it belongs to the window layer.
    #[serde(default)]
    pub window: WindowBehavior,
    /// Vision fallback for non-vision models.
    #[serde(default)]
    pub vision: VisionConfig,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
    /// Spending limits. Empty means no limit, which is the default because a proxy
    /// that refuses requests out of the box would be a surprise.
    #[serde(default)]
    pub budgets: Vec<Budget>,
}

/// A problem found by [`AppConfig::validate`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigIssue {
    pub severity: IssueSeverity,
    pub code: String,
    pub message: String,
    /// Model id or provider id the issue refers to.
    pub subject: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueSeverity {
    Error,
    Warning,
}

impl AppConfig {
    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }

    pub fn model(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.answers_to(id))
    }

    /// Look a model up by identity rather than by exposed id.
    pub fn model_by_key(&self, provider_id: &str, upstream_model: &str) -> Option<&ModelEntry> {
        self.models
            .iter()
            .find(|m| m.provider_id == provider_id && m.upstream_model == upstream_model)
    }

    /// The exposed ids of every configured model, in order.
    ///
    /// The GUI renders these instead of deriving ids itself, so the naming rule
    /// has exactly one implementation.
    pub fn exposed_ids(&self) -> Vec<String> {
        self.models.iter().map(|m| m.exposed_id()).collect()
    }

    /// Fold configurations written before ids were derived into the current
    /// shape, and tidy the alias lists.
    ///
    /// Must be called after every deserialization (including from the config
    /// file) so that legacy fields like `break_after_failures` and `cooldown_secs`
    /// are migrated into their current equivalents.  [`AppConfig::validate`]
    /// runs after this and only sees the post-migration state.
    ///
    /// Returns a note for every id that moved, so the UI can explain itself.
    pub fn normalize(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        self.routing.apply_legacy();
        for model in &mut self.models {
            let exposed = qualified_id(&model.provider_id, &model.upstream_model);
            if let Some(legacy) = model.legacy_id.take() {
                let legacy = legacy.trim().to_string();
                if !legacy.is_empty() && legacy != exposed && !model.aliases.contains(&legacy) {
                    model.aliases.push(legacy.clone());
                    notes.push(format!(
                        "`{legacy}` is now exposed as `{exposed}`; the old id still works as an alias"
                    ));
                }
            }
            model
                .aliases
                .retain(|a| !a.trim().is_empty() && a != &exposed);
            model.aliases.sort();
            model.aliases.dedup();

            // Migrate old supports_* fields into capabilities when capabilities
            // is still at its default (all false) and the old fields are set.
            #[allow(deprecated)]
            if model.capabilities == ModelCapabilities::default()
                && (model.supports_tools || model.supports_vision || model.supports_thinking)
            {
                model.capabilities.tools = model.supports_tools;
                model.capabilities.vision = model.supports_vision;
                model.capabilities.thinking = model.supports_thinking;
            }
            // Clear deprecated fields so they don't persist in new configs.
            #[allow(deprecated)]
            {
                model.supports_tools = false;
                model.supports_vision = false;
                model.supports_thinking = false;
            }
        }
        for budget in &mut self.budgets {
            if budget.id.trim().is_empty() {
                budget.id = format!("budget_{}", uuid::Uuid::new_v4().simple());
            }
        }
        notes
    }

    /// Models that the user still has to classify. The GUI nags about these.
    pub fn unclassified_models(&self) -> Vec<&ModelEntry> {
        self.models.iter().filter(|m| m.tier.is_none()).collect()
    }

    pub fn validate(&self) -> Vec<ConfigIssue> {
        let mut issues = Vec::new();
        let mut seen_ids: BTreeMap<String, usize> = BTreeMap::new();
        let mut seen_providers: BTreeMap<String, usize> = BTreeMap::new();

        for p in &self.providers {
            *seen_providers.entry(p.id.clone()).or_insert(0) += 1;
        }
        for (id, count) in seen_providers {
            if count > 1 {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "provider.duplicate_id".into(),
                    message: format!(
                        "Provider id `{id}` is used {count} times; provider ids have to be unique"
                    ),
                    subject: Some(id),
                });
            }
        }

        if self.server.allow_cors {
            for o in &self.server.cors_origins {
                let trimmed = o.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if !crate::server::is_valid_origin(trimmed) {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Error,
                        code: "server.cors_origin_invalid".into(),
                        message: format!(
                            "`{o}` is not a valid CORS origin (expected scheme://host[:port]); \
                             invalid entries are ignored, and if none are valid no origin is \
                             allowed at all"
                        ),
                        subject: None,
                    });
                }
            }
        }

        for p in &self.providers {
            if p.base_url.trim().is_empty() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "provider.base_url_empty".into(),
                    message: format!("Provider `{}` has no base url", p.name),
                    subject: Some(p.id.clone()),
                });
            }
            if p.timeout_secs == 0 {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "provider.zero_timeout".into(),
                    message: format!(
                        "Provider `{}` has a zero timeout, so every request would time out \
                         before it is sent",
                        p.name
                    ),
                    subject: Some(p.id.clone()),
                });
            }
            let managed_headers = [
                "content-type",
                "authorization",
                "x-api-key",
                "accept",
            ];
            for key in p.extra_headers.keys() {
                if managed_headers.contains(&key.to_lowercase().as_str()) {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Warning,
                        code: "provider.extra_header_override".into(),
                        message: format!(
                            "Provider `{}` has `extra_headers` setting `{}`, which Zroutery \
                             manages itself; the override may cause unexpected behavior",
                            p.name, key
                        ),
                        subject: Some(p.id.clone()),
                    });
                }
            }
        }

        for m in &self.models {
            let exposed = m.exposed_id();
            *seen_ids.entry(exposed.clone()).or_insert(0) += 1;
            for alias in &m.aliases {
                *seen_ids.entry(alias.clone()).or_insert(0) += 1;
            }
            if self.provider(&m.provider_id).is_none() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "model.orphan".into(),
                    message: format!(
                        "Model `{}` points at missing provider `{}`",
                        m.upstream_model, m.provider_id
                    ),
                    subject: Some(exposed.clone()),
                });
            }
            if m.upstream_model.trim().is_empty() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "model.no_upstream".into(),
                    message: format!("A model on provider `{}` has no name", m.provider_id),
                    subject: Some(exposed.clone()),
                });
            }
            for reserved in std::iter::once(&exposed).chain(m.aliases.iter()) {
                if ModelTier::from_virtual_id(reserved).is_some() {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Error,
                        code: "model.reserved_id".into(),
                        message: format!("`{reserved}` is a reserved virtual model id"),
                        subject: Some(exposed.clone()),
                    });
                }
            }
            if m.tier.is_none() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Warning,
                    code: "model.unclassified".into(),
                    message: format!(
                        "Model `{exposed}` has no tier yet, so it is excluded from *-class routing"
                    ),
                    subject: Some(exposed.clone()),
                });
            }
            if let Some(pricing) = &m.pricing {
                for problem in pricing.problems() {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Error,
                        code: "model.bad_pricing".into(),
                        message: format!("The price for `{exposed}` {problem}"),
                        subject: Some(exposed.clone()),
                    });
                }
            }
        }

        for (id, count) in seen_ids {
            if count > 1 {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "model.duplicate_id".into(),
                    message: format!(
                        "`{id}` resolves to {count} models; the same provider and model must not \
                         be listed twice, and aliases have to be unique"
                    ),
                    subject: Some(id),
                });
            }
        }

        for tier in ModelTier::ALL {
            let has = self.models.iter().any(|m| {
                m.enabled
                    && m.tier == Some(tier)
                    && self
                        .provider(&m.provider_id)
                        .map(|p| p.enabled)
                        .unwrap_or(false)
            });
            if !has {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Warning,
                    code: "tier.empty".into(),
                    message: format!(
                        "No enabled model is assigned to `{}`, requests to it will fail",
                        tier.virtual_id()
                    ),
                    subject: Some(tier.virtual_id().to_string()),
                });
            }
        }

        // Classifier candidates reference models by exposed id or alias; a
        // dangling reference means a classifier request would fail at routing
        // time, which is better caught at save time.
        if self.classifier.enabled {
            if !(0.0..=1.0).contains(&self.classifier.detection.minimum_confidence) {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Warning,
                    code: "classifier.confidence_out_of_range".into(),
                    message: format!(
                        "Classifier `minimum_confidence` is {}, which is outside the expected \
                         range of 0.0 to 1.0; the detector may never fire (or always fire)",
                        self.classifier.detection.minimum_confidence,
                    ),
                    subject: None,
                });
            }
            for candidate in &self.classifier.candidates {
                if candidate.model.trim().is_empty() {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Error,
                        code: "classifier.candidate_empty".into(),
                        message: "A classifier candidate has no model id".into(),
                        subject: None,
                    });
                    continue;
                }
                let entry = self.model(&candidate.model);
                if let Some(entry) = entry {
                    let exposed = entry.exposed_id();
                    if !entry.enabled {
                        issues.push(ConfigIssue {
                            severity: IssueSeverity::Warning,
                            code: "classifier.candidate_disabled".into(),
                            message: format!(
                                "Classifier candidate `{}` is a disabled model",
                                candidate.model
                            ),
                            subject: Some(exposed),
                        });
                    } else if self
                        .provider(&entry.provider_id)
                        .map(|p| !p.enabled)
                        .unwrap_or(true)
                    {
                        issues.push(ConfigIssue {
                            severity: IssueSeverity::Warning,
                            code: "classifier.candidate_provider_off".into(),
                            message: format!(
                                "Classifier candidate `{}` sits on a disabled provider",
                                candidate.model
                            ),
                            subject: Some(exposed),
                        });
                    }
                } else {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Error,
                        code: "classifier.candidate_unknown".into(),
                        message: format!(
                            "Classifier candidate `{}` is not a configured model",
                            candidate.model
                        ),
                        subject: Some(candidate.model.clone()),
                    });
                }
            }
            if self
                .classifier
                .candidates
                .iter()
                .all(|c| !c.enabled)
            {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Warning,
                    code: "classifier.no_candidates".into(),
                    message: "Classifier routing is on but no candidate is enabled, so \
                              classifier requests will fail"
                        .into(),
                    subject: None,
                });
            }
        }

        if self.server.is_exposed() {
            issues.push(ConfigIssue {
                severity: IssueSeverity::Warning,
                code: "server.exposed".into(),
                message: format!(
                    "Server binds {} which is reachable from the network; keep authentication on",
                    self.server.host
                ),
                subject: None,
            });
        }
        if !self.server.require_auth {
            issues.push(ConfigIssue {
                severity: IssueSeverity::Warning,
                code: "server.no_auth".into(),
                message: "Authentication is disabled: any local process can spend your API credit"
                    .into(),
                subject: None,
            });
        } else if self.server.auth_token.trim().is_empty() {
            // Auth that rejects everyone looks like a broken proxy from the
            // outside; say so instead of letting the user debug their client.
            issues.push(ConfigIssue {
                severity: IssueSeverity::Error,
                code: "server.empty_token".into(),
                message: "Authentication is required but the token is empty, so every request \
                          would be rejected"
                    .into(),
                subject: None,
            });
        }
        if self.server.cors_is_wide_open() {
            issues.push(ConfigIssue {
                severity: IssueSeverity::Warning,
                code: "server.cors_any_origin".into(),
                message: "CORS is on with no origin list, so any website you visit can call the \
                          proxy from your browser; list the origins you need"
                    .into(),
                subject: None,
            });
        }

        for budget in &self.budgets {
            let subject = format!("{} / {}", budget.scope.label(), budget.period.label());
            for problem in budget.problems() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "budget.impossible".into(),
                    message: format!("The budget for {subject} {problem}"),
                    subject: Some(subject.clone()),
                });
            }
            // A limit in a currency nothing bills in can never be reached, which
            // looks like protection without being any.
            let billed: Vec<&str> = self
                .models
                .iter()
                .filter_map(|m| m.pricing.as_ref().map(|p| p.currency.as_str()))
                .collect();
            if !billed.is_empty() && !billed.contains(&budget.limit.currency.as_str()) {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Warning,
                    code: "budget.unused_currency".into(),
                    message: format!(
                        "The budget for {subject} counts {}, which none of your models bill in, \
                         so it can never be reached",
                        budget.limit.currency
                    ),
                    subject: Some(subject.clone()),
                });
            }
            if let crate::budget::BudgetScope::Provider { id } = &budget.scope {
                if self.provider(id).is_none() {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Warning,
                        code: "budget.orphan".into(),
                        message: format!("The budget for {subject} names a missing provider"),
                        subject: Some(subject.clone()),
                    });
                }
            }
            if let crate::budget::OnExceeded::Degrade { to } = &budget.on_exceeded {
                let reachable = self
                    .models
                    .iter()
                    .any(|m| m.enabled && m.tier == Some(*to));
                if !reachable {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Warning,
                        code: "budget.degrade_nowhere".into(),
                        message: format!(
                            "The budget for {subject} degrades to {}, which has no enabled model",
                            to.virtual_id()
                        ),
                        subject: Some(subject),
                    });
                }
            }
        }

        issues
    }
}

/// Resolves provider secrets. Implemented by the desktop app with the macOS
/// keychain, and by [`MemorySecretStore`] in tests and headless mode.
pub trait SecretStore: Send + Sync + 'static {
    fn get(&self, key_ref: &str) -> Option<String>;
}

/// In-memory store, also used for `ZROUTERY_KEY_<REF>` environment fallbacks.
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    keys: BTreeMap<String, String>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, key_ref: impl Into<String>, secret: impl Into<String>) -> Self {
        self.keys.insert(key_ref.into(), secret.into());
        self
    }

    pub fn insert(&mut self, key_ref: impl Into<String>, secret: impl Into<String>) {
        self.keys.insert(key_ref.into(), secret.into());
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, key_ref: &str) -> Option<String> {
        self.keys.get(key_ref).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> AppConfig {
        let mut cfg = AppConfig::default();
        // A real install has a generated token; an empty one is its own issue.
        cfg.server.auth_token = "test-token".into();
        cfg.providers.push(ProviderConfig::new(
            "deepseek",
            "DeepSeek",
            ProviderKind::OpenAICompatible,
        ));
        cfg.models.push(ModelEntry::for_upstream(
            "deepseek",
            "deepseek-chat",
            Some(ModelTier::Standard),
        ));
        cfg
    }

    #[test]
    fn exposed_ids_are_namespaced_by_provider() {
        let direct = ModelEntry::for_upstream("deepseek", "deepseek-chat", None);
        let proxied = ModelEntry::for_upstream("openrouter", "deepseek-chat", None);
        assert_eq!(direct.exposed_id(), "deepseek-deepseek-chat");
        assert_eq!(proxied.exposed_id(), "openrouter-deepseek-chat");
        assert_ne!(direct.exposed_id(), proxied.exposed_id());
    }

    #[test]
    fn exposed_ids_stay_url_safe() {
        // OpenRouter style names carry a slash, and some providers use a colon.
        let m = ModelEntry::for_upstream("openrouter", "deepseek/deepseek-chat:free", None);
        assert_eq!(m.exposed_id(), "openrouter-deepseek-deepseek-chat-free");
        // Dots survive: they are common in version numbers.
        let m = ModelEntry::for_upstream("openai", " gpt-5.3-sol ", None);
        assert_eq!(m.exposed_id(), "openai-gpt-5.3-sol");
    }

    #[test]
    fn models_answer_to_their_id_and_aliases() {
        let m = ModelEntry::for_upstream("deepseek", "deepseek-chat", None).with_alias("ds");
        assert!(m.answers_to("deepseek-deepseek-chat"));
        assert!(m.answers_to("ds"));
        assert!(!m.answers_to("deepseek-chat"));
    }

    #[test]
    fn the_same_model_from_two_providers_is_allowed() {
        let mut cfg = sample();
        cfg.providers.push(ProviderConfig::new(
            "openrouter",
            "OpenRouter",
            ProviderKind::OpenAICompatible,
        ));
        cfg.models.push(ModelEntry::for_upstream(
            "openrouter",
            "deepseek-chat",
            Some(ModelTier::Standard),
        ));
        assert!(cfg
            .validate()
            .iter()
            .all(|i| i.severity != IssueSeverity::Error));
        assert_eq!(
            cfg.exposed_ids(),
            vec!["deepseek-deepseek-chat", "openrouter-deepseek-chat"]
        );
        assert_eq!(
            cfg.model("openrouter-deepseek-chat").unwrap().provider_id,
            "openrouter"
        );
        assert_eq!(
            cfg.model_by_key("deepseek", "deepseek-chat")
                .unwrap()
                .exposed_id(),
            "deepseek-deepseek-chat"
        );
    }

    #[test]
    fn virtual_ids_round_trip() {
        for t in ModelTier::ALL {
            assert_eq!(ModelTier::from_virtual_id(t.virtual_id()), Some(t));
        }
        assert_eq!(ModelTier::from_virtual_id("gpt-5.3-sol"), None);
        // Anthropic aliases
        assert_eq!(ModelTier::from_virtual_id("haiku-class"), Some(ModelTier::Fast));
        assert_eq!(ModelTier::from_virtual_id("sonnet-class"), Some(ModelTier::Standard));
        assert_eq!(ModelTier::from_virtual_id("opus-class"), Some(ModelTier::Reasoning));
        assert_eq!(ModelTier::from_virtual_id("fable-class"), Some(ModelTier::Frontier));
        // OpenAI aliases
        assert_eq!(ModelTier::from_virtual_id("luna-class"), Some(ModelTier::Fast));
        assert_eq!(ModelTier::from_virtual_id("terra-class"), Some(ModelTier::Standard));
        assert_eq!(ModelTier::from_virtual_id("sol-class"), Some(ModelTier::Reasoning));
        assert_eq!(ModelTier::from_virtual_id("astra-class"), Some(ModelTier::Frontier));
    }

    #[test]
    fn chat_urls() {
        let mut p = ProviderConfig::new("d", "DeepSeek", ProviderKind::OpenAICompatible);
        p.base_url = "https://api.deepseek.com/v1/".into();
        assert_eq!(p.chat_url(), "https://api.deepseek.com/v1/chat/completions");

        let a = ProviderConfig::new("a", "Anthropic", ProviderKind::Anthropic);
        assert_eq!(a.chat_url(), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn base_depth_matches_chat_url_v1_normalisation() {
        let mut openai_bare = ProviderConfig::new("o", "O", ProviderKind::OpenAICompatible);
        openai_bare.base_url = "https://api.example.com".into();
        assert_eq!(openai_bare.base_depth(), BaseDepth::ApiRoot);
        assert_eq!(
            openai_bare.chat_url(),
            "https://api.example.com/v1/chat/completions"
        );

        let mut openai_v1 = ProviderConfig::new("o", "O", ProviderKind::OpenAICompatible);
        openai_v1.base_url = "https://api.example.com/v1".into();
        assert_eq!(openai_v1.base_depth(), BaseDepth::Versioned);

        let anthropic_root = ProviderConfig::new("a", "A", ProviderKind::Anthropic);
        assert_eq!(anthropic_root.base_depth(), BaseDepth::ApiRoot);

        let mut anthropic_v1 = ProviderConfig::new("a", "A", ProviderKind::Anthropic);
        anthropic_v1.base_url = "https://relay.example/v1".into();
        assert_eq!(anthropic_v1.base_depth(), BaseDepth::Versioned);
    }

    #[test]
    fn validate_flags_invalid_cors_origins() {
        let mut cfg = sample();
        cfg.server.allow_cors = true;
        cfg.server.cors_origins = vec!["https://good.example".into(), "not an origin".into()];
        let issues = cfg.validate();
        assert_eq!(
            issues
                .iter()
                .filter(|i| i.code == "server.cors_origin_invalid")
                .count(),
            1
        );

        // Whitespace-only entries are ignored, valid ones pass silently.
        cfg.server.cors_origins = vec!["  ".into(), "https://good.example".into()];
        let issues = cfg.validate();
        assert!(!issues
            .iter()
            .any(|i| i.code == "server.cors_origin_invalid"));

        // No check while CORS is off.
        cfg.server.allow_cors = false;
        cfg.server.cors_origins = vec!["garbage".into()];
        let issues = cfg.validate();
        assert!(!issues
            .iter()
            .any(|i| i.code == "server.cors_origin_invalid"));
    }

    #[test]
    fn validate_flags_unclassified_and_empty_tiers() {
        let mut cfg = sample();
        cfg.models.push(ModelEntry::for_upstream(
            "deepseek",
            "deepseek-reasoner",
            None,
        ));
        let issues = cfg.validate();
        assert!(issues.iter().any(|i| i.code == "model.unclassified"
            && i.subject.as_deref() == Some("deepseek-deepseek-reasoner")));
        // standard is covered, reasoning/fast/frontier are not
        assert_eq!(issues.iter().filter(|i| i.code == "tier.empty").count(), 3);
        assert!(issues.iter().all(|i| i.severity == IssueSeverity::Warning));
    }

    #[test]
    fn validate_flags_a_tier_whose_only_members_are_on_disabled_providers() {
        let mut cfg = sample();
        cfg.providers[0].enabled = false;
        let issues = cfg.validate();
        // standard is assigned but its provider is disabled, so it is effectively empty.
        assert!(issues
            .iter()
            .any(|i| i.code == "tier.empty" && i.subject.as_deref() == Some("standard-class")));
    }

    #[test]
    fn validate_flags_impossible_timeouts_and_auth() {
        let mut cfg = sample();
        cfg.providers[0].timeout_secs = 0;
        assert!(cfg
            .validate()
            .iter()
            .any(|i| i.code == "provider.zero_timeout"));

        let mut cfg = sample();
        // An install that somehow has no token: requiring auth rejects everyone.
        cfg.server.auth_token.clear();
        assert!(cfg
            .validate()
            .iter()
            .any(|i| i.code == "server.empty_token"));

        let mut cfg = sample();
        cfg.server.require_auth = false;
        assert!(!cfg
            .validate()
            .iter()
            .any(|i| i.code == "server.empty_token"));
    }

    #[test]
    fn validate_rejects_reserved_ids_duplicates_and_orphans() {
        let mut cfg = sample();
        // An alias must not shadow a virtual tier id.
        cfg.models[0].aliases.push("sonnet-class".into());
        // The very same provider and model listed twice.
        cfg.models.push(ModelEntry::for_upstream(
            "deepseek",
            "deepseek-chat",
            Some(ModelTier::Reasoning),
        ));
        cfg.models.push(ModelEntry::for_upstream(
            "nope",
            "whatever",
            Some(ModelTier::Fast),
        ));
        let issues = cfg.validate();
        assert!(issues.iter().any(|i| i.code == "model.reserved_id"));
        assert!(issues.iter().any(|i| i.code == "model.duplicate_id"
            && i.subject.as_deref() == Some("deepseek-deepseek-chat")));
        assert!(issues.iter().any(|i| i.code == "model.orphan"));
    }

    #[test]
    fn validate_rejects_duplicate_provider_ids() {
        let mut cfg = sample();
        cfg.providers.push(ProviderConfig::new(
            "deepseek",
            "DeepSeek Again",
            ProviderKind::OpenAICompatible,
        ));
        let issues = cfg.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.code == "provider.duplicate_id"
                    && i.subject.as_deref() == Some("deepseek"))
        );
    }

    #[test]
    fn validate_rejects_an_alias_shared_by_two_models() {
        let mut cfg = sample();
        cfg.providers.push(ProviderConfig::new(
            "openrouter",
            "OpenRouter",
            ProviderKind::OpenAICompatible,
        ));
        cfg.models[0].aliases.push("chat".into());
        cfg.models.push(
            ModelEntry::for_upstream("openrouter", "deepseek-chat", Some(ModelTier::Standard))
                .with_alias("chat"),
        );
        assert!(cfg
            .validate()
            .iter()
            .any(|i| i.code == "model.duplicate_id" && i.subject.as_deref() == Some("chat")));
    }

    #[test]
    fn exposure_detection() {
        let mut s = ServerConfig::default();
        assert!(!s.is_exposed());
        s.host = "0.0.0.0".into();
        assert!(s.is_exposed());
    }

    #[test]
    fn config_json_round_trip() {
        let mut cfg = sample();
        cfg.normalize();
        let json = serde_json::to_string(&cfg).unwrap();
        // Identity is persisted, the derived id is not.
        assert!(json.contains("\"upstream_model\":\"deepseek-chat\""));
        assert!(!json.contains("\"id\":\"deepseek-deepseek-chat\""));
        // Legacy supports_* fields should not appear in serialized output.
        assert!(!json.contains("\"supports_tools\""));
        let mut back: AppConfig = serde_json::from_str(&json).unwrap();
        back.normalize();
        assert_eq!(cfg, back);
    }

    #[test]
    fn legacy_breaker_fields_migrate_into_circuit_breaker() {
        let cfg: AppConfig = serde_json::from_str(
            r#"{
                "routing": {
                    "break_after_failures": 2,
                    "cooldown_secs": 7
                }
            }"#,
        )
        .unwrap();
        assert_eq!(cfg.routing.circuit_breaker.failure_threshold, 4);
        assert_eq!(cfg.routing.circuit_breaker.timeout_secs, 60);

        let mut cfg = cfg;
        cfg.normalize();
        assert_eq!(cfg.routing.circuit_breaker.failure_threshold, 2);
        assert_eq!(cfg.routing.circuit_breaker.timeout_secs, 7);
        assert!(cfg.routing.break_after_failures.is_none());
        assert!(cfg.routing.cooldown_secs.is_none());
    }

    #[test]
    fn normalize_keeps_pre_0_2_ids_working_as_aliases() {
        // A configuration written by 0.1.x, where the id was free-form.
        let mut cfg: AppConfig = serde_json::from_str(
            r#"{"providers":[{"id":"deepseek","name":"DeepSeek","kind":"openai_compatible","base_url":"http://x"}],
                "models":[{"id":"deepseek-v4-pro","provider_id":"deepseek","upstream_model":"deepseek-v4-pro","class":"sonnet"}]}"#,
        )
        .unwrap();
        let notes = cfg.normalize();

        assert_eq!(cfg.models[0].exposed_id(), "deepseek-deepseek-v4-pro");
        assert_eq!(cfg.models[0].aliases, vec!["deepseek-v4-pro"]);
        assert!(notes[0].contains("deepseek-v4-pro"));
        // Clients pinned to the old id keep resolving.
        assert!(cfg.model("deepseek-v4-pro").is_some());
        assert!(cfg.model("deepseek-deepseek-v4-pro").is_some());
        // Running it again changes nothing.
        assert!(cfg.normalize().is_empty());
        assert_eq!(cfg.models[0].aliases, vec!["deepseek-v4-pro"]);
    }

    #[test]
    fn normalize_drops_redundant_and_empty_aliases() {
        let mut cfg: AppConfig = serde_json::from_str(
            r#"{"models":[{"id":"p-m","provider_id":"p","upstream_model":"m",
                           "aliases":["dup","dup","  ","p-m"]}]}"#,
        )
        .unwrap();
        cfg.normalize();
        assert_eq!(cfg.models[0].aliases, vec!["dup"]);
    }

    #[test]
    fn classifier_config_round_trips_and_stays_off_by_default() {
        let mut cfg = sample();
        cfg.normalize();
        assert!(!cfg.classifier.enabled);
        assert!(cfg.classifier.candidates.is_empty());
        let json = serde_json::to_string(&cfg).unwrap();
        let mut back: AppConfig = serde_json::from_str(&json).unwrap();
        back.normalize();
        assert_eq!(cfg, back);

        // And a config that predates classifier routing loads without it.
        let legacy: AppConfig = serde_json::from_str(
            r#"{"server":{"auth_token":"zr-x"},"models":[]}"#,
        )
        .unwrap();
        assert!(!legacy.classifier.enabled);
    }

    #[test]
    fn classifier_candidates_are_validated_against_the_registry() {
        let mut cfg = sample();
        cfg.classifier.enabled = true;

        // A candidate naming a model that does not exist is an error.
        cfg.classifier.candidates.push(ClassifierCandidate {
            model: "nope-missing".into(),
            priority: 10,
            enabled: true,
        });
        assert!(cfg
            .validate()
            .iter()
            .any(|i| i.code == "classifier.candidate_unknown"));

        // A valid candidate on an enabled model is fine...
        cfg.classifier.candidates[0].model = "deepseek-deepseek-chat".into();
        assert!(!cfg
            .validate()
            .iter()
            .any(|i| i.code.starts_with("classifier.")));

        // ...until the model itself is disabled, which is only a warning.
        cfg.models[0].enabled = false;
        assert!(cfg
            .validate()
            .iter()
            .any(|i| i.code == "classifier.candidate_disabled"));
    }

    #[test]
    fn an_enabled_classifier_with_no_live_candidate_warns() {
        let mut cfg = sample();
        cfg.classifier.enabled = true;
        cfg.classifier.candidates.push(ClassifierCandidate {
            model: "deepseek-deepseek-chat".into(),
            priority: 10,
            enabled: false,
        });
        assert!(cfg
            .validate()
            .iter()
            .any(|i| i.code == "classifier.no_candidates"));

        // Nothing is checked while the feature is off.
        cfg.classifier.enabled = false;
        assert!(!cfg
            .validate()
            .iter()
            .any(|i| i.code.starts_with("classifier.")));
    }

    #[test]
    fn partial_json_uses_defaults() {
        let cfg: AppConfig = serde_json::from_str(
            r#"{"providers":[{"id":"p","name":"P","kind":"openai_compatible","base_url":"http://x"}],
                "models":[{"provider_id":"p","upstream_model":"m"}]}"#,
        )
        .unwrap();
        assert_eq!(cfg.server.port, 8787);
        assert!(cfg.server.require_auth);
        assert!(cfg.models[0].enabled);
        assert_eq!(cfg.models[0].weight, 1);
        assert_eq!(cfg.models[0].tier, None);
        assert_eq!(cfg.models[0].exposed_id(), "p-m");
    }

    #[test]
    fn chat_url_tolerates_base_urls_with_or_without_v1() {
        // OpenAI-compatible: bare hosts get /v1, /v1 bases stay single.
        let mut p = ProviderConfig::new("p", "P", ProviderKind::OpenAICompatible);
        p.base_url = "https://api.example.com".into();
        assert_eq!(p.chat_url(), "https://api.example.com/v1/chat/completions");
        let mut v1 = ProviderConfig::new("p", "P", ProviderKind::OpenAICompatible);
        v1.base_url = "https://ps.air-outer.com/v1/".into();
        assert_eq!(
            v1.chat_url(),
            "https://ps.air-outer.com/v1/chat/completions"
        );
        let mut v1 = ProviderConfig::new("p", "P", ProviderKind::OpenAICompatible);
        v1.base_url = "https://ps.air-outer.com/v1/".into();
        assert_eq!(
            v1.chat_url(),
            "https://ps.air-outer.com/v1/chat/completions"
        );

        // Anthropic: a /v1 base must not become /v1/v1/messages.
        let a = ProviderConfig::new("a", "A", ProviderKind::Anthropic);
        assert_eq!(a.chat_url(), "https://api.anthropic.com/v1/messages");
        let mut av1 = ProviderConfig::new("a", "A", ProviderKind::Anthropic);
        av1.base_url = "https://relay.example/v1".into();
        assert_eq!(av1.chat_url(), "https://relay.example/v1/messages");
    }

    #[test]
    fn models_url_avoids_double_v1() {
        // base_url without /v1
        let p1 = ProviderConfig::new("p1", "P1", ProviderKind::Anthropic);
        assert!(p1.models_url().ends_with("/v1/models"));

        // base_url already ends with /v1
        let mut p2 = ProviderConfig::new("p2", "P2", ProviderKind::Anthropic);
        p2.base_url = "https://example.com/v1".to_string();
        assert_eq!(p2.models_url(), "https://example.com/v1/models");

        // base_url ends with /v1/
        let mut p3 = ProviderConfig::new("p3", "P3", ProviderKind::Anthropic);
        p3.base_url = "https://example.com/v1/".to_string();
        assert_eq!(p3.models_url(), "https://example.com/v1/models");

        // OpenAI compatible without /v1 also gets /v1/models
        let p4 = ProviderConfig::new("p4", "P4", ProviderKind::OpenAICompatible);
        assert!(p4.models_url().ends_with("/v1/models"));

        // OpenAI compatible with /v1 in base_url
        let mut p5 = ProviderConfig::new("p5", "P5", ProviderKind::OpenAICompatible);
        p5.base_url = "https://api.openai.com/v1".to_string();
        assert_eq!(p5.models_url(), "https://api.openai.com/v1/models");
    }
}
