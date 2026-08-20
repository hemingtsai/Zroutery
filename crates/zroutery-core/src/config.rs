//! Persistent configuration: providers, model registry entries, routing policy
//! and local server settings.
//!
//! Secrets are never stored here. A provider only holds a `key_ref` that is
//! resolved through a [`SecretStore`] (OS keychain in the desktop app).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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

/// A model exposed by Zroutery, bound to one upstream provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelEntry {
    /// Id exposed to clients, e.g. `deepseek-v4-pro`. Must be unique.
    pub id: String,
    pub provider_id: String,
    /// Model id sent upstream. Often equal to `id`.
    pub upstream_model: String,
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
    /// Extra alias ids that resolve to this exact model.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Hard cap on `max_tokens` sent upstream.
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
}

impl ModelEntry {
    pub fn new(
        id: impl Into<String>,
        provider_id: impl Into<String>,
        class: Option<ModelClass>,
    ) -> Self {
        let id = id.into();
        ModelEntry {
            upstream_model: id.clone(),
            id,
            provider_id: provider_id.into(),
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
        }
    }

    pub fn with_upstream(mut self, upstream: impl Into<String>) -> Self {
        self.upstream_model = upstream.into();
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

    /// True when the server is reachable from outside this machine.
    pub fn is_exposed(&self) -> bool {
        !matches!(self.host.as_str(), "127.0.0.1" | "localhost" | "::1")
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
        self.models.iter().find(|m| m.id == id)
    }

    /// Models that the user still has to classify. The GUI nags about these.
    pub fn unclassified_models(&self) -> Vec<&ModelEntry> {
        self.models.iter().filter(|m| m.class.is_none()).collect()
    }

    pub fn validate(&self) -> Vec<ConfigIssue> {
        let mut issues = Vec::new();
        let mut seen_ids: BTreeMap<&str, usize> = BTreeMap::new();

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
            *seen_ids.entry(m.id.as_str()).or_insert(0) += 1;
            if self.provider(&m.provider_id).is_none() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "model.orphan".into(),
                    message: format!("Model `{}` points at a missing provider", m.id),
                    subject: Some(m.id.clone()),
                });
            }
            if ModelClass::from_virtual_id(&m.id).is_some() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "model.reserved_id".into(),
                    message: format!("`{}` is a reserved virtual model id", m.id),
                    subject: Some(m.id.clone()),
                });
            }
            if m.class.is_none() {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Warning,
                    code: "model.unclassified".into(),
                    message: format!(
                        "Model `{}` has no class yet, so it is excluded from *-class routing",
                        m.id
                    ),
                    subject: Some(m.id.clone()),
                });
            }
        }

        for (id, count) in seen_ids {
            if count > 1 {
                issues.push(ConfigIssue {
                    severity: IssueSeverity::Error,
                    code: "model.duplicate_id".into(),
                    message: format!("Model id `{id}` is declared {count} times"),
                    subject: Some(id.to_string()),
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
        cfg.models.push(ModelEntry::new(
            "deepseek-v4-pro",
            "deepseek",
            Some(ModelClass::Sonnet),
        ));
        cfg
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
        cfg.models
            .push(ModelEntry::new("deepseek-v4-flash", "deepseek", None));
        let issues = cfg.validate();
        assert!(issues
            .iter()
            .any(|i| i.code == "model.unclassified"
                && i.subject.as_deref() == Some("deepseek-v4-flash")));
        // sonnet is covered, opus and haiku are not
        assert_eq!(issues.iter().filter(|i| i.code == "class.empty").count(), 2);
        assert!(issues.iter().all(|i| i.severity == IssueSeverity::Warning));
    }

    #[test]
    fn validate_rejects_reserved_and_duplicate_ids() {
        let mut cfg = sample();
        cfg.models.push(ModelEntry::new(
            "sonnet-class",
            "deepseek",
            Some(ModelClass::Sonnet),
        ));
        cfg.models.push(ModelEntry::new(
            "deepseek-v4-pro",
            "deepseek",
            Some(ModelClass::Opus),
        ));
        cfg.models
            .push(ModelEntry::new("x", "nope", Some(ModelClass::Haiku)));
        let issues = cfg.validate();
        assert!(issues.iter().any(|i| i.code == "model.reserved_id"));
        assert!(issues.iter().any(|i| i.code == "model.duplicate_id"));
        assert!(issues.iter().any(|i| i.code == "model.orphan"));
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
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn partial_json_uses_defaults() {
        let cfg: AppConfig = serde_json::from_str(
            r#"{"providers":[{"id":"p","name":"P","kind":"openai_compatible","base_url":"http://x"}],
                "models":[{"id":"m","provider_id":"p","upstream_model":"m"}]}"#,
        )
        .unwrap();
        assert_eq!(cfg.server.port, 8787);
        assert!(cfg.server.require_auth);
        assert!(cfg.models[0].enabled);
        assert_eq!(cfg.models[0].weight, 1);
        assert_eq!(cfg.models[0].class, None);
    }
}
