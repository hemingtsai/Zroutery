//! Normalising what a request carries, and bounding what a trace is allowed to show.

use crate::ir::Capability;

/// Return a stable, duplicate-free capability vector.
///
/// Request derivation already emits this order, but policy and router entry
/// points are public APIs and can be called with hand-built vectors. Keeping
/// the normalization here makes every eligibility decision deterministic
/// without introducing a second capability classifier.
pub fn canonical_capabilities(capabilities: &[Capability]) -> Vec<Capability> {
    Capability::ALL
        .into_iter()
        .filter(|capability| capabilities.contains(capability))
        .collect()
}

/// A short, stable label for a capability used in decision evidence.
pub fn capability_label(capability: Capability) -> &'static str {
    match capability {
        Capability::Vision => "vision",
        Capability::Audio => "audio",
        Capability::Video => "video",
        Capability::Files => "files",
        Capability::Tools => "tools",
        Capability::Thinking => "thinking",
        Capability::StructuredOutput => "structured_output",
    }
}

/// Bound and normalize a trace fragment supplied by an adapter.
///
/// Eligibility evidence must never become a channel for provider response
/// bodies, request content, or other unbounded diagnostic text. This helper is
/// intentionally small and deterministic; policy reasons themselves use fixed
/// labels, while this protects compatibility adapters that pass a short reason
/// supplied by a caller.
pub(crate) fn bounded_trace_text(value: &str) -> String {
    const MAX_TRACE_CHARS: usize = 128;
    let mut out = String::new();
    for raw_token in value.split(',') {
        let token = raw_token.trim();
        if token.is_empty() {
            continue;
        }
        let safe_token = if is_known_trace_token(token) {
            token.to_string()
        } else {
            "redacted".to_string()
        };
        if !out.is_empty() {
            out.push_str(", ");
        }
        if out.len() + safe_token.len() > MAX_TRACE_CHARS {
            break;
        }
        out.push_str(&safe_token);
    }
    if out.is_empty() {
        "redacted".to_string()
    } else {
        out
    }
}

fn is_known_trace_token(token: &str) -> bool {
    if matches!(
        token,
        "eligible"
            | "below_min_tier"
            | "above_max_tier"
            | "unknown_tier"
            | "provider_forbidden"
            | "provider_not_allowed"
            | "model_forbidden"
            | "model_not_allowed"
            | "circuit_open"
    ) {
        return true;
    }
    ["missing_capability:", "unknown_capability:"]
        .iter()
        .any(|prefix| {
            token.strip_prefix(prefix).is_some_and(|capability| {
                Capability::ALL
                    .into_iter()
                    .any(|known| capability_label(known) == capability)
            })
        })
}
