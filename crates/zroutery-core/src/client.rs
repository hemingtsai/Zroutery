//! Client attribution: which application sent a request, how sure we are, and
//! how much of the traffic we cannot place.
//!
//! # Why this is a module and not a field on `RequestRecord`
//!
//! Zroutery already computes a client profile on every request — see
//! [`crate::config::resolve_client_identity`], called from the pipeline — and
//! throws it away. It is a *routing* concern: which upstream identity should
//! this request present. This module is the *reporting* concern: whose traffic
//! is this, so the Activity tab can say so.
//!
//! The two are deliberately separate. Collapsing them would mean a request
//! routed as `native` because nothing was detected gets reported as "not
//! Codex", which is a statement about routing dressed up as a statement about
//! the caller.
//!
//! # The ladder
//!
//! One order, one implementation, shared by every ingress — Anthropic,
//! OpenAI Chat Completions, Responses and Gemini all go through
//! [`identify`]. A second implementation inside one protocol would be a second
//! set of rules that quietly disagrees with the first.
//!
//! 1. **A validated named gateway credential.** Established by authentication,
//!    so it cannot be spoofed by a header.
//! 2. **`x-zroutery-client`.** An explicit operator- or client-set identifier.
//! 3. **A verified client-specific header pair.** For example `x-app: cli`
//!    together with an `anthropic-beta` carrying `claude-code-`.
//! 4. **An exact `User-Agent` prefix or canonical bare name.**
//! 5. **A runtime or SDK family rule.** `python-requests/…`, `Go-http-client/…`.
//! 6. **`unknown`.**
//!
//! # What is deliberately not trusted
//!
//! Ordinary request headers are caller-controlled. A `User-Agent` is a
//! string anyone can set, and step 4 is a *label*, never an authorisation: the
//! gateway still authenticates every request against a token before any of this
//! runs. Nothing in this module can make an unauthenticated request succeed,
//! and [`AttributionSource::CredentialBinding`] is the only source that implies
//! the caller proved anything.
//!
//! A header that merely *claims* to be a known client must not become a
//! security boundary, so `Confidence` is part of the fingerprint and is
//! carried through to the UI rather than being flattened into a name.

use std::collections::BTreeMap;

use axum::http::{header, HeaderMap};
use serde::{Deserialize, Serialize};

/// Which verified credential a request authenticated with.
///
/// Set only after the gateway has actually checked the token, so a value here
/// means the caller proved something rather than merely asserted it. `None` for
/// the single-token setup every configuration starts with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialBinding {
    /// The credential's stable id. Never the token itself.
    pub credential_id: String,
    /// The canonical client id this credential is bound to.
    pub client_id: ClientId,
}

/// The kind of application behind a request.
///
/// A display-and-aggregation axis, not a detection input: nothing here changes
/// which rules match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientFamily {
    /// A terminal agent — Claude Code, Codex CLI, OpenCode, Aider.
    Cli,
    /// An editor extension — Cursor, Continue, Cline, Roo Code, Windsurf.
    Ide,
    /// A desktop or self-hosted application — Chatbox, LobeChat, Open WebUI.
    Desktop,
    /// A language SDK — `openai`, `anthropic`, `@google/genai`.
    Sdk,
    /// An agent or workflow framework — LangChain, LlamaIndex, Dify, n8n.
    AgentFramework,
    /// Generic HTTP — curl, requests, httpx, `fetch`, a Rust client.
    Http,
    /// Not placeable.
    Unknown,
}

impl ClientFamily {
    /// The stable name used in aggregation keys and UI copy.
    pub fn as_str(&self) -> &'static str {
        match self {
            ClientFamily::Cli => "cli",
            ClientFamily::Ide => "ide",
            ClientFamily::Desktop => "desktop",
            ClientFamily::Sdk => "sdk",
            ClientFamily::AgentFramework => "agent_framework",
            ClientFamily::Http => "http",
            ClientFamily::Unknown => "unknown",
        }
    }
}

/// How a client id was arrived at.
///
/// Ordered by how much the answer can be trusted, so a UI can sort or filter
/// without re-deriving the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionSource {
    /// A named gateway credential that authenticated successfully.
    CredentialBinding,
    /// The `x-zroutery-client` header.
    ExplicitHeader,
    /// A verified pairing of client-specific headers.
    ClientHeader,
    /// An exact `User-Agent` prefix or canonical-name rule.
    UserAgent,
    /// A language-runtime or SDK family rule.
    RuntimeFamily,
    /// Nothing matched.
    Fallback,
}

impl AttributionSource {
    /// The stable name used in aggregation and UI copy.
    pub fn as_str(&self) -> &'static str {
        match self {
            AttributionSource::CredentialBinding => "credential_binding",
            AttributionSource::ExplicitHeader => "explicit_header",
            AttributionSource::ClientHeader => "client_header",
            AttributionSource::UserAgent => "user_agent",
            AttributionSource::RuntimeFamily => "runtime_family",
            AttributionSource::Fallback => "fallback",
        }
    }
}

/// How sure we are that the client id is right.
///
/// Distinct from [`AttributionSource`] because a source can be weak in two
/// different ways: a User-Agent rule is weak because anyone can set it, and a
/// generic runtime match is weak because it does not identify a product at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Proven by authentication. Cannot be forged by a header.
    Strong,
    /// A rule that only that client emits, but is caller-controlled.
    Probable,
    /// A family or runtime guess that does not identify a product.
    Weak,
    /// Nothing was determined.
    Unknown,
}

impl Confidence {
    /// The stable name used in aggregation and UI copy.
    pub fn as_str(&self) -> &'static str {
        match self {
            Confidence::Strong => "strong",
            Confidence::Probable => "probable",
            Confidence::Weak => "weak",
            Confidence::Unknown => "unknown",
        }
    }

    /// Whether this confidence counts as "we know who this is".
    ///
    /// Deliberately excludes [`Confidence::Weak`]. A runtime guess that names
    /// `python-requests` has identified a library, not an application, and
    /// counting it as known would make the recognised share of traffic mean
    /// something it does not.
    pub fn is_recognised(&self) -> bool {
        matches!(self, Confidence::Strong | Confidence::Probable)
    }
}

/// The canonical id for an unregistered client.
///
/// A hard cap so a caller cannot inflate the aggregation table by varying one
/// character per request. Anything longer is bucketed to `custom-…`.
pub const MAX_CUSTOM_CLIENT_ID_LEN: usize = 64;

/// How many distinct client buckets the per-client aggregate keeps.
///
/// The cap is the point: `User-Agent` is caller-controlled, so without a bound
/// one program sending a randomised agent string would create a new stats
/// dimension on every request. The overflow bucket is a single shared entry,
/// not a dropped count, so the sum still equals the global total.
pub const MAX_TRACKED_CLIENTS: usize = 256;

/// A stable, canonical client id.
///
/// Never a free-form echo of a header. `ClientId::sanitised` is the only way to
/// build one from caller-controlled text, and it folds anything unusable into
/// [`ClientId::UNKNOWN`] rather than passing it through.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ClientId(String);

impl ClientId {
    /// The id used when nothing was identified, and the bucket every unusable
    /// value folds into.
    pub const UNKNOWN: &'static str = "unknown";

    /// The id for a client Zroutery has no adapter for but whose runtime is
    /// recognisable.
    pub const CUSTOM: &'static str = "custom";

    /// A canonical id from trusted, already-validated text.
    ///
    /// For ids that come from Zroutery's own tables — an adapter's
    /// `client_name`, a credential binding. Not for header text.
    pub fn trusted(text: &str) -> Self {
        ClientId(text.trim().to_ascii_lowercase())
    }

    /// A canonical id from caller-controlled text.
    ///
    /// Lowercases, replaces every character outside `[a-z0-9._-]` with `_`,
    /// **collapses runs of those separators**, truncates to
    /// [`MAX_CUSTOM_CLIENT_ID_LEN`], and folds an empty or fully stripped result
    /// to [`ClientId::UNKNOWN`].
    ///
    /// Collapsing matters as much as replacing: without it `Acme/1.0 (+support@…)`
    /// and `Acme/1.0_(support@…)` would be two buckets for one product, which
    /// hands a caller a free way to inflate the per-client table. Two requests
    /// differing only in case, punctuation or separator spacing therefore share
    /// a bucket — which is what stops a `User-Agent` being a free storage
    /// dimension.
    pub fn sanitised(raw: &str) -> Self {
        let lowered = raw.trim().to_ascii_lowercase();
        let mut out = String::with_capacity(lowered.len());
        for ch in lowered.chars() {
            if out.len() >= MAX_CUSTOM_CLIENT_ID_LEN {
                break;
            }
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                out.push(ch);
            } else if !out.ends_with('_') {
                out.push('_');
            }
        }

        // Collapse separator runs in one pass rather than tracking state while
        // building: `Acme/1.0_(+support@…)` has an underscore the input spelled
        // and another the fold produced, and only a pass over the finished string
        // sees both.
        let mut collapsed = String::with_capacity(out.len());
        for ch in out.chars() {
            if ch == '_' && collapsed.ends_with('_') {
                continue;
            }
            collapsed.push(ch);
        }

        let trimmed = collapsed.trim_matches('_').to_string();
        if trimmed.is_empty() {
            return ClientId(ClientId::UNKNOWN.to_string());
        }
        ClientId(trimmed)
    }

    /// The bucket that absorbs ids beyond [`MAX_TRACKED_CLIENTS`].
    pub fn overflow() -> Self {
        ClientId(format!("{}-other", ClientId::CUSTOM))
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is the unresolved bucket.
    pub fn is_unknown(&self) -> bool {
        self.0 == ClientId::UNKNOWN
    }
}

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::convert::TryFrom<String> for ClientId {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let cleaned = ClientId::sanitised(&value);
        Ok(cleaned)
    }
}

impl From<ClientId> for String {
    fn from(value: ClientId) -> Self {
        value.0
    }
}

impl Default for ClientId {
    fn default() -> Self {
        ClientId(ClientId::UNKNOWN.to_string())
    }
}

/// What we believe about who sent a request.
///
/// Carries the evidence alongside the answer. A UI showing "Claude Code" with
/// no `source` is making the same claim this module exists to make falsifiable,
/// and `stats.rs` needs `confidence` to decide which of its ratios to publish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientFingerprint {
    /// The canonical id, folded to [`ClientId::UNKNOWN`] when nothing matched.
    pub client_id: ClientId,
    /// The name to show. Falls back to `client_id`, then to `Unknown`.
    pub client_name: String,
    /// The application kind.
    pub family: ClientFamily,
    /// Which rule produced the answer.
    pub source: AttributionSource,
    /// How much the answer can be trusted.
    pub confidence: Confidence,
}

impl ClientFingerprint {
    /// The bucket for a request nothing could identify.
    pub fn unknown() -> Self {
        Self {
            client_id: ClientId::default(),
            client_name: "Unknown".to_string(),
            family: ClientFamily::Unknown,
            source: AttributionSource::Fallback,
            confidence: Confidence::Unknown,
        }
    }

    /// Build a fingerprint for a product Zroutery ships a rule for.
    fn known(
        id: &str,
        name: &str,
        family: ClientFamily,
        source: AttributionSource,
        confidence: Confidence,
    ) -> Self {
        Self {
            client_id: ClientId::trusted(id),
            client_name: name.to_string(),
            family,
            source,
            confidence,
        }
    }

    /// Fold an unrecognised id into an anonymous, low-confidence bucket.
    ///
    /// `custom` rather than the caller's own text when the value is too long to
    /// be a meaningful product name; the raw value would be an unbounded
    /// aggregation key.
    ///
    /// Public because a caller registering a client Zroutery has no rule for
    /// needs the same folding `identify` applies — otherwise a manually
    /// registered client and a detected one would sanitise differently and
    /// land in two buckets.
    pub fn custom(raw: &str, family: ClientFamily) -> Self {
        let id = ClientId::sanitised(raw);
        let name = if id.is_unknown() {
            "Unknown".to_string()
        } else {
            id.as_str().to_string()
        };
        Self {
            client_id: id,
            client_name: name,
            family,
            source: AttributionSource::RuntimeFamily,
            confidence: Confidence::Weak,
        }
    }
}

impl Default for ClientFingerprint {
    fn default() -> Self {
        Self::unknown()
    }
}

/// Read one header as text, ignoring a value that is not valid text.
///
/// A header with invalid UTF-8 is treated as absent rather than normalised,
/// because there is nothing to normalise it to.
fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

/// The verified `User-Agent` rules.
///
/// Each entry is a product name plus the bounded prefixes and canonical bare
/// names that product actually sends. A rule never matches a substring:
/// `curl/8.4.0` appears verbatim inside the agent string of tools that embed
/// libcurl, and a substring match would attribute them all to curl.
///
/// Every entry is annotated with what it rests on. A rule whose provenance is
/// `Observed` was seen on this machine or in a pinned fixture; `Documented`
/// means the vendor documents the header; `Inferred` means nobody has confirmed
/// it and it is here as a hypothesis. Only the first two are treated as
/// verified — an `Inferred` rule still matches, because a weak guess labelled
/// as a weak guess is more useful than `unknown`, but it can never produce
/// [`Confidence::Probable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleProvenance {
    /// Seen in a pinned fixture or on a live client.
    Observed,
    /// Stated by the vendor.
    Documented,
    /// A hypothesis. Never produces `Probable`.
    Inferred,
}

/// One `User-Agent` prefix/canonical-name rule.
#[derive(Debug, Clone, Copy)]
struct UaRule {
    /// Canonical id.
    id: &'static str,
    /// Display name.
    name: &'static str,
    /// Application family.
    family: ClientFamily,
    /// The prefixes this product sends, lowercased and boundary-qualified.
    prefixes: &'static [&'static str],
    /// Complete agent strings this product sends, lowercased.
    ///
    /// Kept separate from prefixes so a canonical bare name such as `magpie`
    /// does not also match an unrelated `magpie-something` product.
    exact: &'static [&'static str],
    /// What the rule rests on.
    provenance: RuleProvenance,
}

/// Every registered `User-Agent` rule.
///
/// Ordered most specific first, because a prefix list is scanned in order and
/// `python-requests` must not shadow a product that embeds it.
static UA_RULES: &[UaRule] = &[
    // -- Agents whose own agent string is documented or observed -------------
    UaRule {
        id: "gemini",
        name: "Gemini CLI",
        family: ClientFamily::Cli,
        // Gemini CLI v0.66.0 contentGenerator: entrypoint is part of the UA.
        prefixes: &[
            "geminicli/",
            "geminicli-tui/",
            "geminicli-cli-command/",
            "geminicli-acp/",
        ],
        exact: &[],
        provenance: RuleProvenance::Documented,
    },
    UaRule {
        id: "codex_desktop",
        name: "Codex Desktop",
        family: ClientFamily::Desktop,
        // Both forms are observed: Codex's normalized desktop UA on Windows
        // uses underscores, while the desktop shell also emits the conventional
        // `Codex Desktop/<version>` form. Both include the product boundary so
        // similarly-prefixed names do not match.
        prefixes: &["codex_desktop_", "codex desktop/"],
        exact: &[],
        provenance: RuleProvenance::Observed,
    },
    UaRule {
        id: "pi-desktop",
        name: "PI-Desktop",
        family: ClientFamily::Desktop,
        // Reported by PI-Desktop and pinned by Magpie's client catalogue. Keep
        // this ahead of Pi even though both currently have explicit boundaries.
        prefixes: &["pi-desktop/"],
        exact: &["pi-desktop"],
        provenance: RuleProvenance::Observed,
    },
    UaRule {
        id: "kimi-code-desktop",
        name: "Kimi Code Desktop",
        family: ClientFamily::Desktop,
        // Observed from the desktop app's own configured client name.
        prefixes: &["kimi-code-desktop/"],
        exact: &["kimi-code-desktop"],
        provenance: RuleProvenance::Observed,
    },
    UaRule {
        id: "claude_code",
        name: "Claude Code",
        family: ClientFamily::Cli,
        // Verified against the shipped binary, which builds
        // `claude-cli/<version> (external, <entrypoint>…)` for API traffic and a
        // separate `claude-code/<version>` for metadata and token requests.
        // Both are needed: matching only the first misses the second, and
        // matching only the second misses ordinary traffic.
        prefixes: &["claude-cli/", "claude-code/"],
        exact: &[],
        provenance: RuleProvenance::Observed,
    },
    UaRule {
        id: "codex",
        name: "Codex CLI",
        family: ClientFamily::Cli,
        // Verified: Codex builds
        // `codex_cli_rs/<version> (<OS> <ver>; <arch>) <terminal> [ (suffix) ]`.
        // Only the leading `codex_cli_rs/<version>` is stable across releases;
        // the OS and terminal tail changes per machine and per shell.
        prefixes: &["codex_cli_rs/", "codex-cli/", "codex/"],
        exact: &[],
        provenance: RuleProvenance::Observed,
    },
    UaRule {
        id: "opencode",
        name: "OpenCode",
        family: ClientFamily::Cli,
        // Verified: OpenCode builds `opencode/<channel>/<version>/<name>`.
        prefixes: &["opencode/"],
        exact: &[],
        provenance: RuleProvenance::Observed,
    },
    UaRule {
        id: "aider",
        name: "Aider",
        family: ClientFamily::Cli,
        prefixes: &["aider/"],
        exact: &[],
        // Aider's documented UA prefix.
        provenance: RuleProvenance::Documented,
    },
    UaRule {
        id: "pi",
        name: "Pi",
        family: ClientFamily::Cli,
        // Pi's engine identifies itself as `pi (<platform>; <arch>)`, while
        // Pi bridges use the observed `pi-` prefix. PI-Desktop is deliberately
        // above this broader product prefix. Raft embeds this same engine, so
        // those requests remain Pi's; there is no Raft UA rule in this table.
        prefixes: &["pi (", "pi-"],
        exact: &["pi"],
        provenance: RuleProvenance::Observed,
    },
    UaRule {
        id: "magpie",
        name: "magpie",
        family: ClientFamily::Desktop,
        // Magpie's catalogue only establishes the canonical bare name. Its
        // distinct internal `magpie-router/…` etc. identities must not collapse
        // into this client, and no speculative version prefix is added here.
        prefixes: &[],
        exact: &["magpie"],
        provenance: RuleProvenance::Observed,
    },
    // -- SDKs ----------------------------------------------------------------
    UaRule {
        id: "openai_sdk",
        name: "OpenAI SDK",
        family: ClientFamily::Sdk,
        prefixes: &["openai-python/", "openai-node/", "openai/python"],
        exact: &[],
        provenance: RuleProvenance::Documented,
    },
    UaRule {
        id: "anthropic_sdk",
        name: "Anthropic SDK",
        family: ClientFamily::Sdk,
        prefixes: &["anthropic-sdk-python/", "anthropic-sdk-typescript/"],
        exact: &[],
        provenance: RuleProvenance::Documented,
    },
    // -- Generic runtimes ----------------------------------------------------
    // These identify a library, never an application, so they are `Weak`
    // whatever their provenance. They are ordered after every product rule so
    // a product embedding them still resolves to the product.
    UaRule {
        id: "curl",
        name: "curl",
        family: ClientFamily::Http,
        prefixes: &["curl/"],
        exact: &["curl"],
        provenance: RuleProvenance::Documented,
    },
    UaRule {
        id: "python_requests",
        name: "Python requests/httpx",
        family: ClientFamily::Http,
        prefixes: &["python-requests/", "python-httpx/", "httpx/"],
        exact: &[],
        provenance: RuleProvenance::Documented,
    },
    UaRule {
        id: "node_fetch",
        name: "Node fetch/undici",
        family: ClientFamily::Http,
        prefixes: &["node-fetch/", "undici/", "node/"],
        exact: &[],
        provenance: RuleProvenance::Documented,
    },
    UaRule {
        id: "go_http",
        name: "Go http client",
        family: ClientFamily::Http,
        prefixes: &["go-http-client/"],
        exact: &[],
        provenance: RuleProvenance::Documented,
    },
    UaRule {
        id: "python_urllib",
        name: "Python urllib",
        family: ClientFamily::Http,
        prefixes: &["python-urllib/"],
        exact: &[],
        provenance: RuleProvenance::Documented,
    },
    UaRule {
        id: "okhttp",
        name: "OkHttp",
        family: ClientFamily::Http,
        prefixes: &["okhttp/", "ktor/", "axios/"],
        exact: &[],
        provenance: RuleProvenance::Documented,
    },
    UaRule {
        id: "rust_reqwest",
        name: "Rust reqwest",
        family: ClientFamily::Http,
        prefixes: &["reqwest/"],
        exact: &[],
        provenance: RuleProvenance::Documented,
    },
];

/// Registered clients that deliberately have no `User-Agent` rule.
///
/// Raft runs Pi's engine and its requests therefore carry Pi's identity. Raft
/// can still be named by a credential or `x-zroutery-client`, but inventing a
/// UA prefix for it would reclassify indistinguishable Pi traffic.
static NON_UA_CLIENTS: &[(&str, &str, ClientFamily)] = &[("raft", "Raft", ClientFamily::Desktop)];

/// The registered client metadata for an id, whether or not it has a UA rule.
///
/// Only values already in these tables count as a *known* client. An
/// unrecognised value is still a declaration the caller made about itself, so it
/// is kept — but at [`Confidence::Weak`], and only after the stronger rules have
/// had their turn. A header is caller-controlled, so a value that happens to
/// read `claude_code` proves nothing either; what the table adds is that Zroutery
/// at least knows the product.
fn registered_client(id: &ClientId) -> Option<(&'static str, &'static str, ClientFamily)> {
    UA_RULES
        .iter()
        .find(|rule| rule.id == id.as_str())
        .map(|rule| (rule.id, rule.name, rule.family))
        .or_else(|| {
            NON_UA_CLIENTS
                .iter()
                .copied()
                .find(|(known_id, _, _)| *known_id == id.as_str())
        })
}

/// The header Codex CLI sends, which only it sends.
fn originator_is_codex(headers: &HeaderMap) -> bool {
    header_text(headers, "originator")
        .is_some_and(|o| o.trim().eq_ignore_ascii_case("codex_cli_rs"))
}

/// The header pairing that identifies the Claude Code CLI.
///
/// Two headers rather than one, because `x-app: cli` on its own is generic and
/// `anthropic-beta` values are not unique to one client.
fn claude_code_header_pair(headers: &HeaderMap) -> bool {
    let app_is_cli =
        header_text(headers, "x-app").is_some_and(|a| a.trim().eq_ignore_ascii_case("cli"));
    let beta_is_claude_code =
        header_text(headers, "anthropic-beta").is_some_and(|b| b.contains("claude-code-"));
    app_is_cli && beta_is_claude_code
}

/// The header pairing that identifies the OpenCode CLI.
///
/// Two headers rather than one. This exists because OpenCode's `User-Agent` is
/// **not** a reliable identifier for a custom provider: OpenCode sets
/// `opencode/<channel>/<version>/<name>` only on its built-in provider paths,
/// and a custom `openai-compatible` provider — which is exactly the case a
/// takeover creates — inherits the AI SDK's own agent string instead. So a UA
/// rule alone would fail precisely when it is most needed.
///
/// The `x-opencode-*` headers are set on the shared request path that every
/// provider goes through, so they survive where the User-Agent does not.
fn opencode_header_pair(headers: &HeaderMap) -> bool {
    header_text(headers, "x-opencode-client").is_some()
        && header_text(headers, "x-opencode-session").is_some()
}

/// Identify the caller of one request.
///
/// One implementation, shared by every ingress. The order below is the whole
/// contract; a caller that wants a different order has to change it here rather
/// than adding a second ladder inside its own protocol.
///
/// `binding` is `Some` only when the gateway has already authenticated a named
/// credential, which is what makes step 1 the only source a caller cannot
/// forge.
pub fn identify(headers: &HeaderMap, binding: Option<&CredentialBinding>) -> ClientFingerprint {
    // 1. A validated named credential. Established by authentication, so it
    //    outranks anything a header can claim.
    if let Some(binding) = binding {
        if let Some((id, name, family)) = registered_client(&binding.client_id) {
            return ClientFingerprint::known(
                id,
                name,
                family,
                AttributionSource::CredentialBinding,
                Confidence::Strong,
            );
        }
        return ClientFingerprint {
            client_id: binding.client_id.clone(),
            client_name: binding.client_id.as_str().to_string(),
            family: family_for(&binding.client_id),
            source: AttributionSource::CredentialBinding,
            confidence: Confidence::Strong,
        };
    }

    // 2. An explicit operator- or client-set identifier. A value Zroutery knows
    //    settles it; an unknown one is held back to the end, so a declaration
    //    that happens to be a typo cannot outrank a rule that would have
    //    answered correctly.
    let declared = header_text(headers, "x-zroutery-client")
        .map(ClientId::sanitised)
        .filter(|id| !id.is_unknown());
    if let Some((id, name, family)) = declared.as_ref().and_then(registered_client) {
        return ClientFingerprint::known(
            id,
            name,
            family,
            AttributionSource::ExplicitHeader,
            Confidence::Probable,
        );
    }

    // 3. A verified pairing of client-specific headers.
    if originator_is_codex(headers) {
        return ClientFingerprint::known(
            "codex",
            "Codex CLI",
            ClientFamily::Cli,
            AttributionSource::ClientHeader,
            Confidence::Probable,
        );
    }
    if claude_code_header_pair(headers) {
        return ClientFingerprint::known(
            "claude_code",
            "Claude Code",
            ClientFamily::Cli,
            AttributionSource::ClientHeader,
            Confidence::Probable,
        );
    }
    if opencode_header_pair(headers) {
        return ClientFingerprint::known(
            "opencode",
            "OpenCode",
            ClientFamily::Cli,
            AttributionSource::ClientHeader,
            Confidence::Probable,
        );
    }

    // 4. An exact User-Agent prefix/canonical-name rule, most specific first.
    if let Some(agent) = header_text(headers, header::USER_AGENT.as_str()) {
        let agent = agent.trim();
        if !agent.is_empty() {
            let lowered = agent.to_ascii_lowercase();
            for rule in UA_RULES {
                if !rule.prefixes.iter().any(|p| lowered.starts_with(p))
                    && !rule.exact.contains(&lowered.as_str())
                {
                    continue;
                }
                // A rule that only names a library never claims to have
                // identified an application, however well evidenced it is.
                let confidence =
                    if rule.family == ClientFamily::Http || rule.family == ClientFamily::Sdk {
                        Confidence::Weak
                    } else {
                        match rule.provenance {
                            // An unconfirmed rule cannot be called probable.
                            RuleProvenance::Inferred => Confidence::Weak,
                            _ => Confidence::Probable,
                        }
                    };
                return ClientFingerprint::known(
                    rule.id,
                    rule.name,
                    rule.family,
                    AttributionSource::UserAgent,
                    confidence,
                );
            }

            // 5. Unregistered but real: keep the agent string as a weak,
            //    sanitised bucket rather than dropping the information.
            return ClientFingerprint::custom(agent, ClientFamily::Unknown);
        }
    }

    // 6. Nothing to go on, unless the caller declared a custom identity of its
    //    own. That declaration is kept, at `weak`: it is the caller's own claim
    //    about itself, sanitised and bounded, and it is the only thing left.
    if let Some(id) = declared {
        return ClientFingerprint {
            client_name: id.as_str().to_string(),
            family: family_for(&id),
            client_id: id,
            source: AttributionSource::ExplicitHeader,
            confidence: Confidence::Weak,
        };
    }

    ClientFingerprint::unknown()
}

/// The family Zroutery already knows for a canonical id.
fn family_for(id: &ClientId) -> ClientFamily {
    registered_client(id)
        .map(|(_, _, family)| family)
        .unwrap_or(ClientFamily::Unknown)
}

/// Every User-Agent client rule Zroutery ships, with its provenance.
///
/// The single source for a diagnostics view, so "which clients can we
/// actually recognise, and on what basis" has one answer rather than a second
/// list that can fall behind [`UA_RULES`]. Explicit-only identities such as
/// Raft are intentionally absent because they have no UA evidence to report.
pub fn registered_rules() -> Vec<(&'static str, &'static str, ClientFamily, RuleProvenance)> {
    UA_RULES
        .iter()
        .map(|r| (r.id, r.name, r.family, r.provenance))
        .collect()
}

/// A bounded map of client id → fingerprint.
///
/// Bounded on purpose. `User-Agent` is caller-controlled, so a caller that
/// varies one character per request would otherwise grow an unbounded
/// aggregation table and turn the stats page into a denial-of-service against
/// itself. Once [`MAX_TRACKED_CLIENTS`] distinct ids are present, new ones share
/// a single overflow bucket — a count, not a loss, so the per-client totals
/// still sum to the global totals.
#[derive(Debug, Default)]
pub struct ClientRegistry {
    entries: BTreeMap<ClientId, ClientFingerprint>,
    overflowed: bool,
}

impl ClientRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fingerprint, returning the id it was filed under.
    ///
    /// Returns the id rather than nothing, because the caller has to write
    /// *that* into the record: if this folds into the overflow bucket and the
    /// record keeps the original id, the aggregate and the request list
    /// disagree.
    pub fn register(&mut self, fingerprint: ClientFingerprint) -> ClientId {
        let id = fingerprint.client_id.clone();

        if self.entries.contains_key(&id) {
            return id;
        }
        if self.entries.len() >= MAX_TRACKED_CLIENTS {
            self.overflowed = true;
            return ClientId::overflow();
        }

        self.entries.insert(id.clone(), fingerprint);
        id
    }

    /// The fingerprint filed under `id`, if it was registered.
    pub fn get(&self, id: &ClientId) -> Option<&ClientFingerprint> {
        self.entries.get(id)
    }

    /// Whether any id has been folded into the overflow bucket.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// How many distinct ids are being tracked.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
