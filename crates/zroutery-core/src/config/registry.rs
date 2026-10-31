//! The configured providers, accounts, maintenance windows and model entries.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::model::{ModelCapabilities, ModelTier};
use super::provider::default_weight;
use super::provider::ProviderKind;
use super::provider::{
    default_connect_timeout, default_timeout, default_true, ApiSurface, DecisionConfig,
    ProviderClientProfile,
};
use crate::billing::{BalanceConfig, BaseDepth, Pricing};
use crate::ir::Dialect;
pub use crate::protocol::ProviderQuirks;

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
    /// Which OpenAI surface this provider answers, when it speaks the OpenAI
    /// protocol.
    ///
    /// `Chat` is the default: `/v1/chat/completions`, the shape every
    /// "OpenAI compatible" relay has served for years. `Responses` switches
    /// the same provider to OpenAI's `/v1/responses` — newer gateways and the
    /// Codex-flavoured relays expose that surface, sometimes *only* that
    /// surface, and the two are not dialects of one another: the request body,
    /// the response body and the event stream all differ. The switch lives on
    /// the provider rather than the request because it is a property of what
    /// the upstream serves, not of what the client asked.
    #[serde(default)]
    pub api_surface: ApiSurface,
    /// Legacy Claude Code impersonation switch. New configurations use
    /// `client_profile`; this field is accepted for old documents and folded
    /// into that profile during normalization.
    #[serde(default, skip_serializing)]
    #[deprecated(note = "use client_profile")]
    pub impersonate_claude_code: bool,
    /// Client identity presented to this upstream.
    #[serde(default)]
    pub client_profile: ProviderClientProfile,
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
    /// How to ask this provider for a decision, when it answers decisions.
    ///
    /// Only meaningful for a provider of kind
    /// [`ProviderKind::Decision`](crate::config::ProviderKind::Decision); every
    /// other kind ignores it.
    #[serde(default)]
    pub decision: DecisionConfig,
    /// Accounts this provider hosts, when it hosts any.
    ///
    /// `#[serde(default)]` is what every optional field on this struct already
    /// uses, so a configuration written before accounts existed deserialises with
    /// an empty list rather than failing. That is the whole reason the account
    /// registry is configuration rather than a new durable file: nothing has to
    /// migrate, and there is no second store whose absence becomes a question.
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,
}

/// One account hosted by a provider.
///
/// This is a **declaration**, and the distinction is the whole point of the type.
/// It says *this provider has this account, and this is where its credential
/// lives*. It says nothing about the account's current state: quota, usage, rate
/// limits and last-success timestamps are derived by probing and expire, so they
/// are held in `account::AccountStore` and are never written here.
///
/// A provider key does not imply an account. A relay reached with one credential
/// can host several, and an account can exist on a provider whose own key is only
/// for health checks — which is why this is a list rather than a flag.
///
/// `AccountRuntime` derives `Serialize`, so writing observations into an
/// `AppConfig` is technically possible and wrong: it would turn a number that was
/// true at one moment into configuration that reads as authoritative afterwards.
/// See `docs/development/account-surface-plan.md`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountConfig {
    /// Stable identifier for this account within its provider.
    ///
    /// Required rather than defaulted. The store is keyed on
    /// `(provider_id, account_id)`, so two accounts that both defaulted to the
    /// same value would collide silently and one of them would be unreachable
    /// while the configuration still claimed both.
    pub account_id: String,
    #[serde(default)]
    pub binding: Option<super::binding::AccountBinding>,
    #[serde(default)]
    pub probe_kind: Option<super::binding::AccountProbeKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effective_rates: Vec<crate::billing::EffectiveRate>,
    /// Where this account's credential lives. Empty means "use the provider's own
    /// key", which is the common case for a relay with a single account.
    #[serde(default)]
    pub key_ref: String,
    /// Whether to probe and route to this account.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Explicit inference credentials for this account. The existing key_ref
    /// remains the panel credential and is never inferred to be a chat key.
    #[serde(default)]
    pub inference_keys: Vec<InferenceKeyConfig>,
    /// How this account's resource is kept topped up.
    ///
    /// A declaration like the rest of this struct: it says what maintenance the
    /// user asked for, never whether it worked. The last check-in, the last
    /// observed reward and the current balance are all runtime observations and
    /// are held in the account store, not written here.
    #[serde(default)]
    pub maintenance: MaintenanceConfig,
}

/// One inference key attached to an account; secret material stays in keychain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceKeyConfig {
    pub id: String,
    pub key_ref: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i32,
    /// Free-form operator note shown beside the key. Never used for routing.
    #[serde(default)]
    pub note: String,
}

impl Default for InferenceKeyConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            key_ref: String::new(),
            enabled: true,
            priority: 0,
            note: String::new(),
        }
    }
}

/// What the user asked to be done about an account's resources.
///
/// # Deliberately holds nothing secret
///
/// There is no username, no password and no cookie field here, and that is a
/// design decision rather than an oversight. Check-in is performed in a real
/// browser against a real page, which means the browser is where credentials
/// belong: it holds them in its own profile, under the same keychain-backed
/// setup as every other credential, and the account signs in once by hand. A
/// password in this struct would be a password in a user-editable file, which
/// `docs/development/account-surface-plan.md` already rules out for the
/// credential itself.
///
/// # No headless switch
///
/// There is deliberately no option to run check-in without a visible browser.
/// The operation needs to solve a challenge a script cannot solve, so a headless
/// mode would not be a faster path to the same result; it would be a mode that
/// cannot work, offered as though it could.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceConfig {
    /// Whether to check this account in.
    ///
    /// Off by default. Maintenance spends a browser and an interactive step, and
    /// silently opting every configured account into it is not a default anyone
    /// would choose deliberately.
    #[serde(default)]
    pub checkin_enabled: bool,
    /// Declared seconds between check-ins.
    ///
    /// Declared rather than learned. With no interval and no history there is no
    /// evidence about when to act, and the scheduler reports that rather than
    /// guessing — see `account::checkin::MaintenanceDecision`.
    #[serde(default)]
    pub checkin_interval_secs: Option<u64>,
    /// Panel-relative path of the page carrying the check-in control.
    ///
    /// Required for browser check-in and deliberately not defaulted. Relays host
    /// their console at different paths, and a wrong default would navigate a
    /// real browser somewhere useless while reporting progress.
    #[serde(default)]
    pub checkin_path: Option<String>,
    /// CSS selector for the check-in control on that page.
    ///
    /// Required alongside [`Self::checkin_path`], and also not defaulted. The
    /// browser activates the panel's own button rather than posting the request
    /// itself, because that is what carries the panel's session and its challenge
    /// token; a default selector would mean clicking an arbitrary element on a
    /// page whose markup nobody here has seen.
    #[serde(default)]
    pub checkin_trigger_selector: Option<String>,
    /// Panel-relative path of the sign-in page, when signing in needs one.
    #[serde(default)]
    pub login_path: Option<String>,
    /// The panel's own root, when it does not share the API endpoint's host.
    ///
    /// Relays increasingly front their relay API with one domain and their
    /// dashboard with another — AgentRouter answers `/v1` on
    /// `ps.air-outer.com` while signing in on `agentrouter.org`. Without this
    /// the dashboard root is derived from the API endpoint, and a relay like
    /// that sends every browser capture and panel probe to a host that hosts
    /// no panel at all: the browser opens a page that is not the dashboard,
    /// and the probe answers with an access token that nothing verifies.
    /// Empty means "same host as the endpoint", which is the common case.
    #[serde(default)]
    pub panel_url: Option<String>,
    /// The panel user id the `New-Api-User` header must name, when the panel's
    /// middleware demands one — the AgentRouter generation does, on
    /// `/api/user/self` itself, so a valid token without it answers 401.
    ///
    /// The browser token capture learns it from the logged-in dashboard and
    /// persists it here; a hand-configured account may type it. Empty means
    /// the panel never wanted the header, and every call behaves as before.
    #[serde(default)]
    pub new_api_user: Option<String>,
    /// Explicit browser executable to drive. Empty means autodetect.
    #[serde(default)]
    pub browser_executable: String,
}

impl MaintenanceConfig {
    /// Reject a maintenance declaration that cannot be acted on.
    ///
    /// Called from configuration validation so an unusable declaration is a
    /// reported issue rather than a check-in that silently never runs.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.checkin_interval_secs == Some(0) {
            return Err("checkin_interval_secs must be greater than zero when set".into());
        }
        if let Some(url) = self
            .panel_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(
                    "panel_url must be an http(s) panel root, e.g. https://panel.example".into(),
                );
            }
        }
        for (field, path) in [
            ("checkin_path", self.checkin_path.as_deref()),
            ("login_path", self.login_path.as_deref()),
        ] {
            let Some(path) = path.map(str::trim).filter(|p| !p.is_empty()) else {
                continue;
            };
            // A path is joined onto a base URL, so an absolute or
            // scheme-carrying value would send the browser somewhere the panel
            // does not host.
            if path.starts_with('/') && path.contains("://") {
                return Err(format!("{field} must be a panel-relative path, not a URL"));
            }
            if !path.starts_with('/') {
                return Err(format!("{field} must start with '/'"));
            }
        }
        // A path with no control means navigating somewhere and stopping there,
        // which is a check-in that silently does nothing. That is exactly the
        // failure this declaration exists to prevent, so it is refused at load
        // rather than discovered later as "the control was not found".
        if self.checkin_enabled && self.has_checkin_page() && self.checkin_selector().is_none() {
            return Err(
                "checkin_trigger_selector must be set when checkin is enabled with a \
                 checkin_path"
                    .into(),
            );
        }
        Ok(())
    }

    /// Whether a panel-relative check-in page is declared.
    fn has_checkin_page(&self) -> bool {
        self.checkin_path
            .as_deref()
            .map(str::trim)
            .is_some_and(|p| !p.is_empty())
    }

    /// The configured check-in control selector, when it is usable.
    pub fn checkin_selector(&self) -> Option<&str> {
        self.checkin_trigger_selector
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Whether browser check-in is fully specified.
    ///
    /// Requires the selector as well as the page, because the browser's job is to
    /// activate the panel's own control. Distinct from `checkin_enabled`: a user
    /// can legitimately want check-in reported as unsupported rather than
    /// attempted against a page that is not there, and the panel needs to be able
    /// to say which of those is the case.
    pub fn is_browser_checkin_configured(&self) -> bool {
        self.checkin_enabled && self.has_checkin_page() && self.checkin_selector().is_some()
    }

    /// The declared dashboard root, when this account's panel lives on its own
    /// host. Empty means the common single-domain case: the panel is the relay
    /// endpoint with its relay suffixes stripped, and the callers derive that.
    pub fn panel_override(&self) -> Option<&str> {
        self.panel_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
    }
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
            // Anthropic providers default to the Claude Code profile for
            // compatibility with relays imported from CC Switch.
            #[allow(deprecated)]
            impersonate_claude_code: false,
            client_profile: match kind {
                ProviderKind::Anthropic => ProviderClientProfile::ClaudeCode,
                ProviderKind::OpenCodeZenFree | ProviderKind::OpenCodeConsole => {
                    ProviderClientProfile::OpenCodeCli
                }
                // A decision endpoint is asked a question, not a chat, and the
                // profile decides how the request is presented. Nothing about a
                // decision request is a client impersonation, so it presents
                // its own identity.
                ProviderKind::Decision => ProviderClientProfile::Native,
                ProviderKind::OpenAICompatible => ProviderClientProfile::Auto,
            },
            bearer_auth: false,
            enabled: true,
            timeout_secs: default_timeout(),
            connect_timeout_secs: default_connect_timeout(),
            anthropic_version: None,
            api_surface: ApiSurface::default(),
            quirks: ProviderQuirks::default(),
            balance: BalanceConfig::default(),
            decision: DecisionConfig::default(),
            accounts: Vec::new(),
        }
    }

    /// Resolve the profile for one inbound request. The legacy boolean wins
    /// while old configurations are still in memory; normalized documents use
    /// only `client_profile`.
    pub fn client_profile_for(&self, identity: Option<&str>) -> ProviderClientProfile {
        self.configured_client_profile().resolve(identity)
    }

    /// The configured profile with the legacy boolean folded in, before any
    /// detection from inbound headers.
    ///
    /// This is what [`resolve_client_identity`] takes: it is the operator's
    /// choice, and it is the only place the deprecated switch is still honoured.
    pub fn configured_client_profile(&self) -> ProviderClientProfile {
        #[allow(deprecated)]
        if self.impersonate_claude_code {
            ProviderClientProfile::ClaudeCode
        } else {
            self.client_profile
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

    /// The wire dialect requests to this provider are encoded in.
    ///
    /// The kind decides, except inside the OpenAI family, where the provider's
    /// own surface choice answers: the same `/v1` base can serve Chat
    /// Completions or Responses, and everything downstream of that choice —
    /// the request encoder, the response decoder, the stream parser — differs.
    /// A decision endpoint keeps the placeholder OpenAI value; nothing asks it
    /// to chat (see [`ProviderKind::dialect`]).
    pub fn dialect(&self) -> Dialect {
        match self.kind {
            ProviderKind::OpenAICompatible
            | ProviderKind::OpenCodeZenFree
            | ProviderKind::OpenCodeConsole => match self.api_surface {
                ApiSurface::Chat => Dialect::OpenAI,
                ApiSurface::Responses => Dialect::OpenAIResponses,
            },
            other => other.dialect(),
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
            ProviderKind::OpenAICompatible
            | ProviderKind::OpenCodeZenFree
            | ProviderKind::OpenCodeConsole => match self.api_surface {
                ApiSurface::Chat => format!("{with_v1}/chat/completions"),
                ApiSurface::Responses => format!("{with_v1}/responses"),
            },
            // A decision endpoint has no chat url. Its url carries the
            // decision path, and the upstream asks it directly.
            ProviderKind::Decision => self.decision.url(&self.base_url),
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
            ProviderKind::OpenAICompatible
            | ProviderKind::OpenCodeZenFree
            | ProviderKind::OpenCodeConsole => {
                // Standard OpenAI gateways serve models at /v1/models.
                // If the base_url already ends with /v1, just append /models;
                // otherwise append /v1/models so gateways like ps.air-outer.com work.
                if base.ends_with("/v1") {
                    format!("{base}/models")
                } else {
                    format!("{base}/v1/models")
                }
            }
            // A decision endpoint publishes no catalogue: it answers questions,
            // it does not serve models. The model is declared by hand on the
            // models page, so there is nothing to list and nothing to fetch.
            ProviderKind::Decision => self.decision.url(&self.base_url),
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
    pub(super) legacy_id: Option<String>,
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
    /// The model's context window in tokens, when the operator knows it.
    ///
    /// Metadata the provider catalogue often publishes and the configuration
    /// used to drop. It is advertised in `GET /v1/models` and shown in the
    /// dashboard; it is not a request cap (that is `max_output_tokens`) and the
    /// proxy does not truncate a transcript to fit it — the upstream is the
    /// authority on what it accepts.
    #[serde(default)]
    pub context_window: Option<u32>,
    /// The reasoning-effort levels this model accepts, as a client picker
    /// should offer them.
    ///
    /// Filled from models.dev at import time and correctable by hand. The
    /// takeover publishes these as the client's model variants; an empty list
    /// means the model declares no effort levels, so no picker is written
    /// rather than one that offers levels the upstream may reject.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effort_levels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_capability: Option<super::reasoning::ReasoningCapability>,
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
            supports_tools: false,
            supports_vision: false,
            supports_thinking: false,
            display_name: None,
            aliases: Vec::new(),
            max_output_tokens: None,
            context_window: None,
            effort_levels: Vec::new(),
            reasoning_capability: None,
            pricing: None,
        }
    }

    /// The id clients use, `<provider>-<upstream model>`.
    pub fn exposed_id(&self) -> String {
        qualified_id(&self.provider_id, &self.upstream_model)
    }

    /// The effort levels to advertise for this model, in picker order.
    ///
    /// The stored list is normalized on read so a hand edit or an older
    /// catalogue cannot smuggle an unencodable level into a client's config.
    pub fn normalized_effort_levels(&self) -> Vec<String> {
        crate::config::model::normalize_effort_levels(&self.effort_levels)
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
