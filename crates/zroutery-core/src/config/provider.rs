//! What an upstream is: its wire protocol, the client profiles it needs, and how an inbound request is attributed to one.

use axum::http::HeaderMap;
use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};

use crate::ir::Dialect;

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
    /// OpenCode's anonymous free tier (Zen). OpenAI wire format at `/zen/v1`;
    /// the locked [`ProviderClientProfile::OpenCodeCli`] shaping identity and no
    /// login. Serves main traffic only.
    #[serde(rename = "opencode_zen_free")]
    OpenCodeZenFree,
    /// OpenCode's authenticated Console tier. OpenAI wire format; a device-code
    /// OAuth token is the credential and the shaping identity is
    /// [`ProviderClientProfile::OpenCodeCli`]. Serves main traffic only.
    #[serde(rename = "opencode_console")]
    OpenCodeConsole,
    /// A decision endpoint: a model that answers typed questions instead of
    /// generating text.
    ///
    /// The System One family — Jev and the models built in its shape — takes a
    /// state and a set of typed questions and answers with calibrated
    /// probabilities over the choices. It cannot generate prose at all, so it
    /// serves the Auto Mode pool and nothing else: a decision endpoint is not
    /// eligible for a tier, and the Auto Mode is the only thing that knows how
    /// to ask it a question.
    ///
    /// The wire protocol is not OpenAI's, so nothing here may treat it as one.
    /// The upstream speaks to it in the decision protocol, and the value
    /// [`Self::dialect`] returns is only ever reached by code that asks a
    /// decision endpoint to chat — which nothing does.
    Decision,
}

/// Which OpenAI surface a provider answers.
///
/// The two are separate protocols that happen to share a vendor: the request
/// body, the response body and the stream events of `/v1/responses` differ
/// from `/v1/chat/completions` in shape and in semantics (stateful items,
/// reasoning items, tool calls as function calls). Both encoders already
/// exist — the gateway itself serves the Responses dialect inbound — so this
/// is a routing choice, not new code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiSurface {
    /// `/v1/chat/completions` — the default, and what every OpenAI-compatible
    /// clone has served historically.
    #[default]
    Chat,
    /// `/v1/responses` — OpenAI's newer surface, and the one Codex-shaped
    /// gateways expect.
    Responses,
}

/// How to ask a decision endpoint for an answer.
///
/// Only meaningful for a provider of kind [`ProviderKind::Decision`]; every
/// other kind ignores it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionConfig {
    /// The path on the provider's base url that answers decisions.
    ///
    /// Configurable rather than fixed, because the decision-model family does
    /// not agree on one path: the reference service answers at
    /// `/v1/systemone`, and the gateways that front it each publish their own.
    /// The path is joined to the base url as it stands, so a base url that
    /// already ends in `/v1` is not given a second one.
    #[serde(default = "DecisionConfig::default_path")]
    pub path: String,
    /// The confidence the answer must carry, in either direction, before it is
    /// acted on.
    ///
    /// A decision model answers with a probability rather than a verdict, and
    /// a probability of 0.4 is not the same claim as a probability of 0.99. The
    /// Auto Mode is a security gate, so an answer that is not confident has not
    /// cleared the action and the action is blocked. The default is the
    /// detector's own floor, for the same reason: this is the gate, and the
    /// gate is closed unless the evidence is strong.
    ///
    /// Lower it to 0.5 to let a calibrated model's own majority decide, which
    /// is what a decision model is for.
    #[serde(default = "DecisionConfig::default_threshold")]
    pub threshold: f64,
}

impl Default for DecisionConfig {
    fn default() -> Self {
        Self {
            path: Self::default_path(),
            threshold: Self::default_threshold(),
        }
    }
}

impl DecisionConfig {
    fn default_path() -> String {
        "/v1/systemone".to_string()
    }

    fn default_threshold() -> f64 {
        0.85
    }

    /// The full url of the decision endpoint.
    ///
    /// The base url is used as it stands: a base that already ends in `/v1` is
    /// not given a second one, and a path that already begins with `/v1` is
    /// not given a third.
    pub fn url(&self, base_url: &str) -> String {
        let base = base_url.trim_end_matches('/');
        let path = self.path.trim();
        if path.is_empty() {
            return base.to_string();
        }
        if path.starts_with("/v1") {
            format!("{base}{path}")
        } else {
            format!("{base}/v1{path}")
        }
    }
}

impl ProviderKind {
    pub fn dialect(&self) -> Dialect {
        match self {
            ProviderKind::Anthropic => Dialect::Anthropic,
            ProviderKind::OpenAICompatible
            | ProviderKind::OpenCodeZenFree
            | ProviderKind::OpenCodeConsole => Dialect::OpenAI,
            // A decision endpoint has no chat dialect. The upstream speaks to
            // it in the decision protocol and never reaches this value, because
            // nothing asks a decision endpoint to chat: it is not eligible for
            // a tier, and the Auto Mode pool is the only thing that may reach
            // it. The value is the OpenAI one so that a caller which reached it
            // by mistake would fail at the wire rather than silently encode a
            // request the endpoint cannot answer.
            ProviderKind::Decision => Dialect::OpenAI,
        }
    }

    /// Whether this provider targets OpenCode's gateway, which needs the locked
    /// `opencode` shaping identity and is served main traffic only.
    pub fn is_opencode(&self) -> bool {
        matches!(
            self,
            ProviderKind::OpenCodeZenFree | ProviderKind::OpenCodeConsole
        )
    }

    /// Whether this provider answers typed questions instead of generating
    /// text.
    ///
    /// A decision endpoint cannot serve main traffic — it has no answer for a
    /// request that wants prose — so it is eligible for the Auto Mode pool and
    /// for nothing else.
    pub fn is_decision(&self) -> bool {
        matches!(self, ProviderKind::Decision)
    }

    /// Default base url shown in the GUI when creating a provider.
    pub fn default_base_url(&self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "https://api.anthropic.com",
            ProviderKind::OpenAICompatible => "https://api.openai.com/v1",
            ProviderKind::OpenCodeZenFree => "https://opencode.ai/inference/openai/v1",
            ProviderKind::OpenCodeConsole => "https://opencode.ai/inference/openai/v1",
            ProviderKind::Decision => "https://api.typesafe.ai",
        }
    }
}

pub(super) fn default_true() -> bool {
    true
}
pub(super) fn default_weight() -> u32 {
    1
}
pub(super) fn default_timeout() -> u64 {
    600
}
pub(super) fn default_connect_timeout() -> u64 {
    15
}

/// The upstream client identity used for a provider.
///
/// `Auto` derives the identity from the inbound client's User-Agent (or the
/// explicit `x-zroutery-client` hint). It falls back to the native Zroutery
/// identity when the client is unknown. Fixed profiles apply equally to normal
/// traffic, probes and metadata requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderClientProfile {
    #[default]
    Auto,
    Native,
    ClaudeCode,
    Codex,
    /// The OpenCode CLI identity. Unlike the others this is a *gateway* target,
    /// not a client to imitate: it is fixed, never auto-detected, and carries the
    /// fingerprint OpenCode's free tier demands (an `opencode/<ver>` User-Agent,
    /// the `x-opencode-*` headers, a canonical session).
    #[serde(rename = "opencode_cli")]
    OpenCodeCli,
}

impl ProviderClientProfile {
    /// Select a concrete profile for one inbound client identity.
    ///
    /// The identity is classified by its *leading* client token, not by whether
    /// a client name appears anywhere in it. Every real client starts its
    /// User-Agent (and its `x-zroutery-client` hint) with its own token —
    /// `claude-cli/…`, `codex_cli_rs/…` — so a leading match is what the wire
    /// actually carries. A substring rule would mis-classify a relay that
    /// advertises itself as one client while naming another in a compatibility
    /// string: `codex_cli_rs/0.1.0 (claude-code-compat)` is a Codex client, and
    /// must not be relayed as Claude Code.
    pub fn resolve(self, identity: Option<&str>) -> Self {
        if !matches!(self, Self::Auto) {
            return self;
        }
        let identity = identity.unwrap_or_default().trim().to_ascii_lowercase();
        if identity.starts_with("claude") {
            Self::ClaudeCode
        } else if identity.starts_with("codex") {
            Self::Codex
        } else {
            Self::Native
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Native => "native",
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
            Self::OpenCodeCli => "opencode_cli",
        }
    }

    /// Inbound headers forwarded verbatim when this profile is active.
    /// An empty slice means "fabricate only".
    ///
    /// These are the clients' own fingerprint headers, not ours. Everything on
    /// them is something the real CLI puts on the wire itself, so forwarding it
    /// can only make the request *more* faithful — which is the opposite of the
    /// argument that keeps [`Self::Native`] and the fabricated fingerprints
    /// minimal. Nothing here may influence authentication; see
    /// [`PASSTHROUGH_DENIED`].
    pub fn passthrough_headers(self) -> &'static [&'static str] {
        match self {
            // Fabrication-only: the native identity carries nothing of its own.
            Self::Auto | Self::Native => &[],
            // The OpenCode identity is fabricated by the profile arm; its session
            // header is owned there too, never forwarded verbatim (a non-canonical
            // client value would be refused upstream with 403).
            Self::OpenCodeCli => &[],
            Self::Codex => &[
                "originator",
                "session_id",
                "x-session-id",
                "x-codex-beta-features",
                "x-codex-turn-metadata",
            ],
            Self::ClaudeCode => &[
                "x-app",
                "x-stainless-arch",
                "x-stainless-lang",
                "x-stainless-os",
                "x-stainless-package-version",
                "x-stainless-retry-count",
                "x-stainless-runtime",
                "x-stainless-runtime-version",
                "x-stainless-timeout",
                "x-stainless-helper-method",
                "anthropic-beta",
                "anthropic-dangerous-direct-browser-access",
                "anthropic-version",
            ],
        }
    }
}

/// Headers a passthrough may never carry, whatever any allowlist says.
///
/// A passthrough forwards what the *client* sent. Anything on this list decides
/// who the request authenticates as, or how it is framed on the wire, and letting
/// an inbound request choose that would let it redirect Zroutery's own
/// credential or rewrite the framing it is about to send. The allowlists above
/// already exclude every one of these — this list is the belt to their braces,
/// so that widening an allowlist later cannot quietly open an auth hole.
pub const PASSTHROUGH_DENIED: &[&str] = &[
    "authorization",
    "x-api-key",
    "content-length",
    "host",
    "cookie",
    "proxy-authorization",
];

/// Whether `name` may never be forwarded from an inbound request.
pub fn passthrough_denied(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    PASSTHROUGH_DENIED.iter().any(|denied| *denied == name)
}

/// Detection result plus the already-validated forwarding set.
///
/// Built once per inbound request by [`resolve_client_identity`] and then handed
/// to every upstream attempt, so a retry, a second candidate or a handshake
/// replay all present the identical client.
///
/// Forwarding is an *allowlist*: this carries only the client headers the
/// resolved profile names, each one actually sent by the client, and never a
/// general copy of the inbound request. Authentication and framing headers are
/// excluded both by the allowlist and by [`PASSTHROUGH_DENIED`].
#[derive(Debug, Clone)]
pub struct ClientIdentity {
    /// The profile that won: detected when `Auto`, otherwise the configured one.
    pub profile: ProviderClientProfile,
    /// Real inbound header values, in allowlist order, already denied-checked.
    /// Empty when the profile forwards nothing (or the client sent none of the
    /// allowlisted names).
    pub passthrough: Vec<(HeaderName, HeaderValue)>,
    /// The canonical `ses_…` OpenCode session for this request. `Some` only when
    /// the resolved profile is [`ProviderClientProfile::OpenCodeCli`]; `None`
    /// otherwise, and for auxiliary calls that carry no conversation.
    pub session: Option<String>,
    /// The OpenCode Console billing org, sent back as `x-opencode-org-id`. `Some`
    /// only for an `OPENCODE-CONSOLE` provider with a resolved token.
    pub org_id: Option<String>,
}

/// Resolve the client profile for one request and collect its forwarding set.
///
/// This is the single entry point shared by election probes, real traffic,
/// retries and auxiliary (model-list / balance / vision) requests. The profile is
/// detected from the inbound headers when `configured` is
/// [`ProviderClientProfile::Auto`]; a fixed profile skips detection but still
/// builds the passthrough set, because an operator who pins Claude Code wants the
/// real CLI's headers just as much as an auto-detected one does.
///
/// Only the resolved profile's allowlisted headers are collected. This is
/// deliberately not a general passthrough: the profile is the contract for which
/// of the client's own headers may travel, and everything outside it is dropped.
pub fn resolve_client_identity(
    configured: ProviderClientProfile,
    headers: &HeaderMap,
) -> ClientIdentity {
    let profile = if matches!(configured, ProviderClientProfile::Auto) {
        detect_client_profile(headers)
    } else {
        configured
    };

    let mut passthrough = Vec::new();
    for header in profile.passthrough_headers() {
        // Defence in depth: an allowlist entry that reaches this point with a
        // denied name is dropped, never forwarded.
        if passthrough_denied(header) {
            tracing::warn!(
                profile = profile.as_str(),
                header,
                "dropping a passthrough header that is never forwarded upstream"
            );
            continue;
        }
        let Ok(name) = HeaderName::from_bytes(header.as_bytes()) else {
            continue;
        };
        // Only a value the client really sent is forwarded. With no real value
        // the fabricator keeps the field; inventing one here would be a second
        // place where the fingerprint is made up.
        if let Some(value) = headers.get(&name) {
            passthrough.push((name.clone(), value.clone()));
        }
    }

    ClientIdentity {
        profile,
        passthrough,
        session: None,
        org_id: None,
    }
}

/// Read one inbound header as text, ignoring a value that is not valid text.
fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

/// Identify the inbound client from its own headers.
///
/// The order is the whole contract, and each step is something a real client
/// actually sends — no fingerprint guessing, no substring matching on a value
/// that could be anything:
/// 1. `x-zroutery-client`, the operator-facing override.
/// 2. `originator: codex_cli_rs`, which only the Codex CLI sends.
/// 3. `x-app: cli` together with an `anthropic-beta` carrying `claude-code-`,
///    the Claude Code CLI pairing.
/// 4. `User-Agent` starting with `claude-cli/`.
/// 5. `User-Agent` starting with `codex_cli_rs/`, `codex/`, or an observed
///    Codex Desktop form (`codex_desktop_` / `Codex Desktop/`).
/// 6. Nothing matched: the client is unknown and gets the native identity.
fn detect_client_profile(headers: &HeaderMap) -> ProviderClientProfile {
    if let Some(hint) = header_text(headers, "x-zroutery-client") {
        match hint.trim().to_ascii_lowercase().as_str() {
            "claude_code" => return ProviderClientProfile::ClaudeCode,
            "codex" | "codex_desktop" => return ProviderClientProfile::Codex,
            // Not an override we know: keep detecting rather than guessing.
            _ => {}
        }
    }

    if let Some(originator) = header_text(headers, "originator") {
        if originator.trim().eq_ignore_ascii_case("codex_cli_rs") {
            return ProviderClientProfile::Codex;
        }
    }

    let app_is_cli =
        header_text(headers, "x-app").is_some_and(|app| app.trim().eq_ignore_ascii_case("cli"));
    let beta_is_claude_code =
        header_text(headers, "anthropic-beta").is_some_and(|beta| beta.contains("claude-code-"));
    if app_is_cli && beta_is_claude_code {
        return ProviderClientProfile::ClaudeCode;
    }

    if let Some(agent) = header_text(headers, "user-agent") {
        let agent = agent.trim();
        if agent.to_ascii_lowercase().starts_with("claude-cli/") {
            return ProviderClientProfile::ClaudeCode;
        }
        let agent = agent.to_ascii_lowercase();
        if agent.starts_with("codex_cli_rs/")
            || agent.starts_with("codex/")
            || agent.starts_with("codex_desktop_")
            || agent.starts_with("codex desktop/")
        {
            return ProviderClientProfile::Codex;
        }
    }

    ProviderClientProfile::Native
}
