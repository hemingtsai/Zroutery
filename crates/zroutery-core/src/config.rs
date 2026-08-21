//! Persistent configuration: providers, model registry entries, routing policy
//! and local server settings.
//!
//! Secrets are never stored here. A provider only holds a `key_ref` that is
//! resolved through a [`SecretStore`] (OS keychain in the desktop app).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::billing::{BalanceConfig, Pricing};
use crate::ir::Dialect;

/// Capability tier a model belongs to. Assigned manually by the user; Zroutery
/// never guesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelClass {
    /// Most capable / most expensive.
    Opus,
    /// Balanced workhorse.
    Sonnet,
    /// Cheapest and fastest.
    Haiku,
}

impl ModelClass {
    pub const ALL: [ModelClass; 3] = [ModelClass::Opus, ModelClass::Sonnet, ModelClass::Haiku];

    /// The virtual model id exposed to clients, e.g. `sonnet-class`.
    pub fn virtual_id(&self) -> &'static str {
        match self {
            ModelClass::Opus => "opus-class",
            ModelClass::Sonnet => "sonnet-class",
            ModelClass::Haiku => "haiku-class",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ModelClass::Opus => "opus",
            ModelClass::Sonnet => "sonnet",
            ModelClass::Haiku => "haiku",
        }
    }

    pub fn from_virtual_id(id: &str) -> Option<ModelClass> {
        match id {
            "opus-class" => Some(ModelClass::Opus),
            "sonnet-class" => Some(ModelClass::Sonnet),
            "haiku-class" => Some(ModelClass::Haiku),
            _ => None,
        }
    }
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

/// Per provider deviations from the reference dialect.
///
/// These exist because "OpenAI compatible" is a spectrum: reasoning models
/// reject `max_tokens` and `temperature`, some gateways choke on
/// `stream_options`, and only a few accept `reasoning_effort`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderQuirks {
    /// Send `max_completion_tokens` instead of `max_tokens`.
    #[serde(default)]
    pub use_max_completion_tokens: bool,
    #[serde(default)]
    pub drop_temperature: bool,
    #[serde(default)]
    pub drop_top_p: bool,
    #[serde(default)]
    pub drop_stop: bool,
    /// Ask for a usage trailer on streaming responses.
    #[serde(default = "default_true")]
    pub stream_usage: bool,
    /// Use `role: "developer"` for the system prompt.
    #[serde(default)]
    pub system_as_developer: bool,
    /// Translate thinking budgets into `reasoning_effort`.
    #[serde(default)]
    pub send_reasoning_effort: bool,
}

impl Default for ProviderQuirks {
    fn default() -> Self {
        ProviderQuirks {
            use_max_completion_tokens: false,
            drop_temperature: false,
            drop_top_p: false,
            drop_stop: false,
            stream_usage: true,
            system_as_developer: false,
            send_reasoning_effort: false,
        }
    }
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
            enabled: true,
            timeout_secs: default_timeout(),
            connect_timeout_secs: default_connect_timeout(),
            anthropic_version: None,
            quirks: ProviderQuirks::default(),
            balance: BalanceConfig::default(),
        }
    }

    /// Full URL for the chat endpoint of this provider.
    pub fn chat_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        match self.kind {
            ProviderKind::Anthropic => format!("{base}/v1/messages"),
            ProviderKind::OpenAICompatible => format!("{base}/chat/completions"),
        }
    }

    /// Full URL for model listing, used by the "fetch models" button.
    pub fn models_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        match self.kind {
            ProviderKind::Anthropic => format!("{base}/v1/models"),
            ProviderKind::OpenAICompatible => format!("{base}/models"),
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
    /// classified it yet: it stays callable by exact id but never participates
    /// in class routing.
    #[serde(default)]
    pub class: Option<ModelClass>,
    /// Lower value wins inside a class.
    #[serde(default)]
    pub priority: i32,
    /// Weight for random tie breaking among equal priorities.
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default)]
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
    /// What the provider charges for this model. Entered by hand, like the class;
    /// without it a request is logged with no cost rather than a guessed one.
    #[serde(default)]
    pub pricing: Option<Pricing>,
}

impl ModelEntry {
    /// Named rather than `new` so every call site has to state which argument is
    /// the provider now that both are plain strings.
    pub fn for_upstream(
        provider_id: impl Into<String>,
        upstream_model: impl Into<String>,
        class: Option<ModelClass>,
    ) -> Self {
        ModelEntry {
            provider_id: provider_id.into(),
            upstream_model: upstream_model.into(),
            legacy_id: None,
            class,
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

/// How a class picks among its candidate models.
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingConfig {
    #[serde(default)]
    pub strategy: RoutingStrategy,
    /// Try the next candidate in the class when a request fails.
    #[serde(default = "default_true")]
    pub failover: bool,
    /// Max upstream attempts for a single client request.
    #[serde(default = "RoutingConfig::default_attempts")]
    pub max_attempts: u32,
    /// Consecutive failures before a model is put in cooldown.
    #[serde(default = "RoutingConfig::default_break_after")]
    pub break_after_failures: u32,
    /// Cooldown duration in seconds.
    #[serde(default = "RoutingConfig::default_cooldown")]
    pub cooldown_secs: u64,
    /// When a client asks for an unknown model id, fall back to this class
    /// instead of returning 404.
    #[serde(default)]
    pub unknown_model_fallback: Option<ModelClass>,
    /// Map arbitrary client model ids onto a class, e.g.
    /// `claude-sonnet-4-5-20250929 -> sonnet`. Used by Anthropic native clients.
    #[serde(default)]
    pub client_aliases: BTreeMap<String, ModelClass>,
    /// Interpret client model ids containing `opus`/`sonnet`/`haiku` as the
    /// matching class, so Anthropic-native tools work out of the box. This only
    /// affects how *incoming* ids are read; upstream models are always
    /// classified by hand.
    #[serde(default = "default_true")]
    pub match_claude_names: bool,
}

impl RoutingConfig {
    fn default_attempts() -> u32 {
        3
    }
    fn default_break_after() -> u32 {
        3
    }
    fn default_cooldown() -> u64 {
        60
    }
}

impl Default for RoutingConfig {
    fn default() -> Self {
        RoutingConfig {
            strategy: RoutingStrategy::default(),
            failover: true,
            max_attempts: Self::default_attempts(),
            break_after_failures: Self::default_break_after(),
            cooldown_secs: Self::default_cooldown(),
            unknown_model_fallback: None,
            client_aliases: BTreeMap::new(),
            match_claude_names: true,
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
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
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
    /// Returns a note for every id that moved, so the UI can explain itself.
    pub fn normalize(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
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
        }
        notes
    }

    /// Models that the user still has to classify. The GUI nags about these.
    pub fn unclassified_models(&self) -> Vec<&ModelEntry> {
        self.models.iter().filter(|m| m.class.is_none()).collect()
    }

    pub fn validate(&self) -> Vec<ConfigIssue> {
        let mut issues = Vec::new();
        let mut seen_ids: BTreeMap<String, usize> = BTreeMap::new();

        for p in &self.providers {
            if p.base_url.trim().is_empty() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "provider.base_url_empty".into(),
                    message: format!("Provider `{}` has no base url", p.name),
                    subject: Some(p.id.clone()),
                });
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
                if ModelClass::from_virtual_id(reserved).is_some() {
                    issues.push(ConfigIssue {
                        severity: IssueSeverity::Error,
                        code: "model.reserved_id".into(),
                        message: format!("`{reserved}` is a reserved virtual model id"),
                        subject: Some(exposed.clone()),
                    });
                }
            }
            if m.class.is_none() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Warning,
                    code: "model.unclassified".into(),
                    message: format!(
                        "Model `{exposed}` has no class yet, so it is excluded from *-class routing"
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

        for class in ModelClass::ALL {
            let has = self
                .models
                .iter()
                .any(|m| m.enabled && m.class == Some(class));
            if !has {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Warning,
                    code: "class.empty".into(),
                    message: format!(
                        "No enabled model is assigned to `{}`, requests to it will fail",
                        class.virtual_id()
                    ),
                    subject: Some(class.virtual_id().to_string()),
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
        cfg.providers.push(ProviderConfig::new(
            "deepseek",
            "DeepSeek",
            ProviderKind::OpenAICompatible,
        ));
        cfg.models.push(ModelEntry::for_upstream(
            "deepseek",
            "deepseek-chat",
            Some(ModelClass::Sonnet),
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
            Some(ModelClass::Sonnet),
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
        for c in ModelClass::ALL {
            assert_eq!(ModelClass::from_virtual_id(c.virtual_id()), Some(c));
        }
        assert_eq!(ModelClass::from_virtual_id("gpt-5.3-sol"), None);
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
    fn validate_flags_unclassified_and_empty_classes() {
        let mut cfg = sample();
        cfg.models.push(ModelEntry::for_upstream(
            "deepseek",
            "deepseek-reasoner",
            None,
        ));
        let issues = cfg.validate();
        assert!(issues.iter().any(|i| i.code == "model.unclassified"
            && i.subject.as_deref() == Some("deepseek-deepseek-reasoner")));
        // sonnet is covered, opus and haiku are not
        assert_eq!(issues.iter().filter(|i| i.code == "class.empty").count(), 2);
        assert!(issues.iter().all(|i| i.severity == IssueSeverity::Warning));
    }

    #[test]
    fn validate_rejects_reserved_ids_duplicates_and_orphans() {
        let mut cfg = sample();
        // An alias must not shadow a virtual class id.
        cfg.models[0].aliases.push("sonnet-class".into());
        // The very same provider and model listed twice.
        cfg.models.push(ModelEntry::for_upstream(
            "deepseek",
            "deepseek-chat",
            Some(ModelClass::Opus),
        ));
        cfg.models.push(ModelEntry::for_upstream(
            "nope",
            "whatever",
            Some(ModelClass::Haiku),
        ));
        let issues = cfg.validate();
        assert!(issues.iter().any(|i| i.code == "model.reserved_id"));
        assert!(issues.iter().any(|i| i.code == "model.duplicate_id"
            && i.subject.as_deref() == Some("deepseek-deepseek-chat")));
        assert!(issues.iter().any(|i| i.code == "model.orphan"));
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
            ModelEntry::for_upstream("openrouter", "deepseek-chat", Some(ModelClass::Sonnet))
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
        let cfg = sample();
        let json = serde_json::to_string(&cfg).unwrap();
        // Identity is persisted, the derived id is not.
        assert!(json.contains("\"upstream_model\":\"deepseek-chat\""));
        assert!(!json.contains("\"id\":\"deepseek-deepseek-chat\""));
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
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
        assert_eq!(cfg.models[0].class, None);
        assert_eq!(cfg.models[0].exposed_id(), "p-m");
    }
}
