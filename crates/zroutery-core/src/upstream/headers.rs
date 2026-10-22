//! Auth, versioning and client-fingerprint headers.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};

use super::DEFAULT_ANTHROPIC_VERSION;
use crate::config::{
    resolve_client_identity, ClientIdentity, ProviderClientProfile, ProviderConfig, ProviderKind,
};
use crate::error::{Error, Result};
/// Claude Code client fingerprint (used for impersonation to pass a
/// gateway's "Claude Code only" check).
pub(super) const CLAUDE_CODE_USER_AGENT: &str = "claude-cli/2.1.217 (external, sdk-cli)";
pub(super) const CLAUDE_CODE_SYSTEM_IDENTITY: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";
const CODEX_USER_AGENT: &str = "codex_cli_rs/0.1.0";

/// The OpenCode CLI User-Agent the gateway's free tier requires: it must contain
/// `opencode/<ver>` with `<ver> >= 1.18.0`, and the rest is the CLI's own suffix.
#[cfg(feature = "opencode")]
const OPENCODE_USER_AGENT: &str =
    "opencode/1.18.31 ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.14";
#[cfg(feature = "opencode")]
const OPENCODE_CLIENT_ID: &str = "cli";
#[cfg(feature = "opencode")]
const OPENCODE_PROJECT_ID: &str = "global";

/// Build the auth and content headers for a provider.
pub fn build_headers(provider: &ProviderConfig, api_key: Option<&str>) -> Result<HeaderMap> {
    build_headers_for_profile(provider, api_key, provider.client_profile_for(None))
}

/// Build headers with an already resolved client profile.
///
/// This is fabrication only: the profile is taken as fixed and there is no
/// inbound request to forward anything from.
pub fn build_headers_for_profile(
    provider: &ProviderConfig,
    api_key: Option<&str>,
    profile: ProviderClientProfile,
) -> Result<HeaderMap> {
    let identity = resolve_client_identity(profile, &HeaderMap::new());
    build_headers_for_identity(provider, api_key, &identity)
}

/// Build headers for one resolved [`ClientIdentity`]: the fabricated fingerprint
/// first, then the profile's allowlisted client headers on top.
///
/// Precedence, lowest to highest: auth and content framing, `provider.extra_headers`,
/// the fabricated profile fingerprint, the profile's allowlisted inbound
/// passthrough. The last step is the one that matters: an allowlisted header the
/// client genuinely sent is never replaced by a value we made up, because a
/// fabricated `x-stainless-runtime` on top of a real one is exactly the
/// inconsistency a strict gateway looks for.
///
/// This is *not* a general passthrough. Only the names the resolved profile
/// allowlists are forwarded, and only when the client actually sent them; every
/// other inbound header is dropped, and the denied names can never travel no
/// matter what an allowlist says.
pub fn build_headers_for_identity(
    provider: &ProviderConfig,
    api_key: Option<&str>,
    client: &ClientIdentity,
) -> Result<HeaderMap> {
    let profile = client.profile;
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );

    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        match provider.kind {
            ProviderKind::Anthropic => {
                headers.insert(
                    HeaderName::from_static("x-api-key"),
                    HeaderValue::from_str(key)
                        .map_err(|_| Error::internal("API key contains invalid characters"))?,
                );
                // Some Anthropic-protocol relays read Authorization and reject
                // a request carrying only x-api-key. Sending both costs
                // nothing — the standard API accepts either header — and
                // satisfies gateways that check exactly one of them.
                if provider.bearer_auth {
                    headers.insert(
                        AUTHORIZATION,
                        HeaderValue::from_str(&format!("Bearer {key}"))
                            .map_err(|_| Error::internal("API key contains invalid characters"))?,
                    );
                }
            }
            ProviderKind::OpenAICompatible
            | ProviderKind::OpenCodeZenFree
            | ProviderKind::OpenCodeConsole => {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {key}"))
                        .map_err(|_| Error::internal("API key contains invalid characters"))?,
                );
            }
            // A decision endpoint takes the same bearer credential the rest of
            // the OpenAI-shaped family does: the key is the panel's own access
            // token, and it is presented verbatim after `Bearer `.
            ProviderKind::Decision => {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {key}"))
                        .map_err(|_| Error::internal("API key contains invalid characters"))?,
                );
            }
        }
    }

    if provider.kind == ProviderKind::Anthropic {
        let version = provider
            .anthropic_version
            .as_deref()
            .unwrap_or(DEFAULT_ANTHROPIC_VERSION);
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_str(version)
                .map_err(|_| Error::internal("invalid anthropic-version"))?,
        );
    }

    for (k, v) in &provider.extra_headers {
        let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|_| Error::internal(format!("invalid header name `{k}`")))?;
        let value = HeaderValue::from_str(v)
            .map_err(|_| Error::internal(format!("invalid value for header `{k}`")))?;
        headers.insert(name, value);
    }

    // Client profile headers: minimal clean fingerprints, matching what the
    // real claude-cli sends (cc-switch's field-tested set). SDK-layer headers
    // (x-stainless-*) and request ids are deliberately NOT *fabricated*: a real
    // CLI does not invent them, and header/version inconsistencies are exactly
    // what strict gateways look for. Gateways needing extras can use
    // provider.extra_headers.
    //
    // That reasoning is about making values up, and only about making values up.
    // A header the client really carried is not a guess — it is the truth about
    // this request, so the profile's allowlisted ones are forwarded verbatim at
    // the end of this function and win over anything fabricated here.
    if profile == ProviderClientProfile::ClaudeCode {
        tracing::debug!(
            provider = %provider.name,
            "impersonation enabled – injecting Claude Code fingerprint headers"
        );
        // User-Agent: claude-cli style; gateways accept any CC-style version.
        headers.insert(
            HeaderName::from_static("user-agent"),
            HeaderValue::from_static(CLAUDE_CODE_USER_AGENT),
        );
        // x-app: identifies as CLI client.
        headers.insert(
            HeaderName::from_static("x-app"),
            HeaderValue::from_static("cli"),
        );
        // Real Claude Code sends this; harmless where it is not checked.
        headers.insert(
            HeaderName::from_static("anthropic-dangerous-direct-browser-access"),
            HeaderValue::from_static("true"),
        );
        // anthropic-beta: ensure claude-code marker is present
        const CLAUDE_CODE_BETA: &str = "claude-code-20250219";
        let beta_value = match headers.get("anthropic-beta") {
            Some(existing) => {
                let existing_str = existing.to_str().unwrap_or("");
                if existing_str.contains(CLAUDE_CODE_BETA) {
                    existing_str.to_string()
                } else {
                    format!("{CLAUDE_CODE_BETA},{existing_str}")
                }
            }
            None => CLAUDE_CODE_BETA.to_string(),
        };
        headers.insert(
            HeaderName::from_static("anthropic-beta"),
            HeaderValue::from_str(&beta_value)
                .map_err(|_| Error::internal("invalid anthropic-beta value"))?,
        );
    } else if profile == ProviderClientProfile::Codex {
        headers.insert(
            HeaderName::from_static("user-agent"),
            HeaderValue::from_static(CODEX_USER_AGENT),
        );
        headers.insert(
            HeaderName::from_static("originator"),
            HeaderValue::from_static("codex_cli_rs"),
        );
    }

    // The OpenCode identity is locked: it is the fingerprint the gateway's free
    // tier demands, so it is emitted after the Claude Code / Codex fabricators
    // (a fixed OpenCode profile must not be perturbed by them) and before the
    // passthrough. The session is owned here, never forwarded verbatim.
    #[cfg(feature = "opencode")]
    if profile == ProviderClientProfile::OpenCodeCli {
        headers.insert(
            HeaderName::from_static("user-agent"),
            HeaderValue::from_static(OPENCODE_USER_AGENT),
        );
        headers.insert(
            HeaderName::from_static("x-opencode-client"),
            HeaderValue::from_static(OPENCODE_CLIENT_ID),
        );
        headers.insert(
            HeaderName::from_static("x-opencode-project"),
            HeaderValue::from_static(OPENCODE_PROJECT_ID),
        );
        if let Some(session) = client.session.as_deref() {
            headers.insert(
                HeaderName::from_static("x-opencode-session"),
                HeaderValue::from_str(session)
                    .map_err(|_| Error::internal("invalid OpenCode session id"))?,
            );
        }
        headers.insert(
            HeaderName::from_static("x-opencode-request"),
            HeaderValue::from_str(&crate::opencode::request_id())
                .map_err(|_| Error::internal("invalid OpenCode request id"))?,
        );
        // The Console-issued inference endpoint needs the billing org.
        if let Some(org_id) = client.org_id.as_deref() {
            headers.insert(
                HeaderName::from_static("x-opencode-org-id"),
                HeaderValue::from_str(org_id)
                    .map_err(|_| Error::internal("invalid OpenCode org id"))?,
            );
        }
    }

    // Passthrough last: an allowlisted client header wins over the fabricated
    // fingerprint above.
    //
    // Two rules hold here no matter what the allowlists say:
    //
    // 1. A passthrough can never change authentication. Denied names are skipped
    //    outright, and the auth headers are captured before the loop and restored
    //    after it, so the upstream call always authenticates with the provider's
    //    own key no matter what the inbound request claimed.
    // 2. An allowlisted inbound header beats a fabricated one, which is what
    //    applying the passthrough after the fabricator does.
    // Every entry in `client.passthrough` is a name the resolved profile already
    // allowed and the client already sent; unlisted inbound headers never reach
    // this loop, so this is a filtered set and not a general copy.
    let auth: Vec<(HeaderName, HeaderValue)> = headers
        .iter()
        .filter(|(name, _)| is_auth_header(name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    for (name, value) in &client.passthrough {
        if crate::config::passthrough_denied(name.as_str()) {
            tracing::warn!(
                provider = %provider.name,
                header = %name,
                "refusing to forward a denied header from the inbound request"
            );
            continue;
        }
        headers.insert(name.clone(), value.clone());
    }
    for (name, value) in &auth {
        headers.insert(name.clone(), value.clone());
    }

    Ok(headers)
}

/// Whether a header decides who this request authenticates as.
fn is_auth_header(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "authorization" | "x-api-key" | "proxy-authorization" | "cookie"
    )
}

/// The identity used when there is no inbound client at all: an election probe,
/// a model-list or balance call, a classifier side query. The provider's
/// configured profile still applies — a fixed profile is configured precisely so
/// that these auxiliary requests look the same as real traffic — but there are
/// no client headers to forward.
pub(super) fn anonymous_identity(provider: &ProviderConfig) -> ClientIdentity {
    resolve_client_identity(provider.client_profile_for(None), &HeaderMap::new())
}
