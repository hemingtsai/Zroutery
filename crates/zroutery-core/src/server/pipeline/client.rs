//! Working out which client sent this request, so the policy and the provider can both act on the answer.

use axum::http::{header, HeaderMap};

use crate::config::{resolve_client_identity, ClientIdentity, ProviderConfig};
use crate::ir::ChatRequest;
use crate::policy::ClientContext;

/// Build a [`ClientContext`] from the request headers and model.
///
/// Extracts the User-Agent, any `x-zroutery-client` header, and uses the
/// pre-collected header pairs for policy matching.
pub(super) fn build_client_context<'a>(
    headers: &'a HeaderMap,
    model: &'a str,
    header_pairs: &'a [(&'a str, &'a str)],
) -> ClientContext<'a> {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());

    let client_id = headers
        .get("x-zroutery-client")
        .and_then(|v| v.to_str().ok());

    ClientContext {
        client_id,
        user_agent,
        api_key_prefix: None,
        application: headers
            .get("x-zroutery-application")
            .and_then(|v| v.to_str().ok()),
        headers: header_pairs,
        model,
    }
}

pub(super) fn client_identity(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-zroutery-client")
        .or_else(|| headers.get(header::USER_AGENT))
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// The resolved client identity for one candidate provider.
///
/// Resolved here, at the top of each attempt, rather than inside
/// [`Upstream::send_with_identity`], so that the profile detection and the
/// validated passthrough set happen exactly once per attempt and the handshake
/// retry loop inside the transport replays the identical client.
pub(super) fn client_for(req: &ChatRequest, provider: &ProviderConfig) -> ClientIdentity {
    #[allow(unused_mut)]
    let mut identity = resolve_client_identity(
        provider.client_profile_for(req.client_identity.as_deref()),
        &req.client_headers,
    );
    // The OpenCode gateway needs a canonical session. Resolve it once here, from
    // the client's own header when it already carries one, else from the
    // conversation — one session per conversation keeps the upstream prompt cache
    // warm across a multi-turn history.
    #[cfg(feature = "opencode")]
    if identity.profile == crate::config::ProviderClientProfile::OpenCodeCli {
        let inbound = req
            .client_headers
            .get("x-opencode-session")
            .and_then(|value| value.to_str().ok());
        identity.session = Some(crate::opencode::session_from_signal(
            inbound,
            conversation_seed(req).as_deref(),
        ));
    }
    identity
}

/// A stable per-conversation seed: the first user turn's text.
///
/// A multi-turn conversation keeps one session as its history grows, while
/// conversations with different openings separate — the same rule the reference
/// OpenCode clients use. `None` when the request carries no user text.
#[cfg(feature = "opencode")]
fn conversation_seed(req: &ChatRequest) -> Option<String> {
    let text: String = req
        .messages
        .iter()
        .find(|message| message.role == crate::ir::Role::User)
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(crate::ir::ContentBlock::as_text)
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    (!text.trim().is_empty()).then_some(text)
}
