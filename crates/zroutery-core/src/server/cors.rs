//! The CORS policy, and the origin validation that keeps a typo from widening an allow-list into an open one.
use std::time::Duration;

use axum::http::{header, HeaderName, HeaderValue, Method};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use crate::config::ServerConfig;

/// Build the CORS layer for the configured origins.
///
/// Browsers are the only reason this exists, so the allowed methods and headers
/// are pinned to what the two APIs actually use instead of `Any`. An empty origin
/// list with CORS enabled means "any origin", which `AppConfig::validate` flags as
/// a warning and the dashboard shows in red.
pub(super) fn cors_layer(server: &ServerConfig) -> Option<CorsLayer> {
    if !server.allow_cors {
        return None;
    }
    let mut layer = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            header::ACCEPT,
            HeaderName::from_static("x-api-key"),
            HeaderName::from_static("x-goog-api-key"),
            HeaderName::from_static("anthropic-version"),
            HeaderName::from_static("anthropic-beta"),
            HeaderName::from_static("x-zroutery-session-id"),
            HeaderName::from_static("session_id"),
            HeaderName::from_static("x-session-id"),
            HeaderName::from_static("x-opencode-session"),
            HeaderName::from_static("x-claude-code-session-id"),
        ])
        .expose_headers([
            HeaderName::from_static("x-zroutery-session-id"),
            HeaderName::from_static("x-session-id"),
            HeaderName::from_static("session_id"),
            HeaderName::from_static("x-claude-code-session-id"),
        ])
        .max_age(Duration::from_secs(600));

    // Origins split into valid / invalid. An empty *configured* list means
    // "allow any", but if the user configured origins and none of them are
    // usable, deny everything rather than silently widening to `Any` — a
    // typo must never turn into an open CORS policy.
    match origin_policy(&server.cors_origins) {
        OriginPolicy::Any => layer = layer.allow_origin(Any),
        OriginPolicy::List(valid) => {
            let origins: Vec<HeaderValue> = valid
                .iter()
                .filter_map(|o| HeaderValue::from_str(o).ok())
                .collect();
            layer = layer.allow_origin(AllowOrigin::list(origins));
        }
        OriginPolicy::DenyAll => layer = layer.allow_origin(AllowOrigin::list(Vec::new())),
    }
    Some(layer)
}

/// What to allow, derived from the configured `cors_origins`.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum OriginPolicy {
    /// No origins configured: allow any.
    Any,
    /// These (validated) origins are allowed.
    List(Vec<String>),
    /// Origins were configured but none are valid: allow none. A typo must
    /// never widen an explicit allow-list into an open policy.
    DenyAll,
}

pub(super) fn origin_policy(configured: &[String]) -> OriginPolicy {
    let valid: Vec<String> = configured
        .iter()
        .map(|o| o.trim())
        .filter(|o| is_valid_origin(o))
        .map(str::to_string)
        .collect();
    if valid.is_empty() {
        let any_configured = configured.iter().any(|o| !o.trim().is_empty());
        if any_configured {
            tracing::warn!("every configured cors_origin is invalid; no origin will be allowed");
            OriginPolicy::DenyAll
        } else {
            OriginPolicy::Any
        }
    } else {
        if valid.len() != configured.len() {
            tracing::warn!("some cors_origins entries are invalid and were ignored");
        }
        OriginPolicy::List(valid)
    }
}

/// Whether `origin` is a well-formed `scheme://host[:port]` value usable as a
/// CORS allow-origin entry. Rejects paths, queries, fragments, whitespace,
/// userinfo, non-ASCII text and invalid ports — those are config mistakes, and
/// treating them as "no origins" would silently open the proxy to `Any`.
pub(crate) fn is_valid_origin(origin: &str) -> bool {
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return false;
    }
    if rest.is_empty() || !rest.is_ascii() {
        return false;
    }
    // No path / query / fragment / whitespace / userinfo after the authority.
    if rest.contains(['/', '?', '#', ' ', '\t', '@']) {
        return false;
    }

    let (host, port) = if let Some(stripped) = rest.strip_prefix('[') {
        // IPv6 literal: [::1] or [::1]:8787.
        let Some(close) = stripped.find(']') else {
            return false;
        };
        let host = &stripped[..close];
        let after = &stripped[close + 1..];
        if host.is_empty() {
            return false;
        }
        if after.is_empty() {
            (host, None)
        } else if let Some(port) = after.strip_prefix(':') {
            (host, Some(port))
        } else {
            return false;
        }
    } else {
        match rest.rsplit_once(':') {
            None => (rest, None),
            Some((_, "")) => return false, // trailing colon, no port
            Some((host, _)) if host.contains(':') => return false, // bare IPv6 without []
            Some((host, port)) => (host, Some(port)),
        }
    };

    if host.is_empty() {
        return false;
    }
    if let Some(port) = port {
        if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        if port.parse::<u16>().is_err() {
            return false;
        }
    }
    true
}
