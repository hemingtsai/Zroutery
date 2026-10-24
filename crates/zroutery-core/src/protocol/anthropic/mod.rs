//! Anthropic Messages API dialect: `/v1/messages` requests, responses and the
//! stateful SSE event stream.

/// Anthropic requires `max_tokens`; used when the client omits it.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Anthropic rejects a thinking budget below this many tokens.
pub const MIN_THINKING_BUDGET: u32 = 1024;

/// Budget used when thinking is enabled without naming one.
pub(super) const DEFAULT_THINKING_BUDGET: u32 = 2048;

pub(super) const KNOWN_KEYS: &[&str] = &[
    "model",
    "messages",
    "system",
    "max_tokens",
    "temperature",
    "top_p",
    "top_k",
    "stop_sequences",
    "stream",
    "tools",
    "tool_choice",
    "thinking",
    "metadata",
];

/// The dated Anthropic API versions this gateway can shape a request for, and
/// what each one means for the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicApiVersion {
    /// 2023-06-01 — the three-year baseline. Sampling parameters ride the
    /// wire, documents are accepted in any role, and every relay has served
    /// this shape.
    Legacy,
    /// 2026-10-21 — the first dated revision since the baseline, and a
    /// tightening: `temperature`, `top_p` and `top_k` are *removed* — sending
    /// any of them answers 400 even at its default value — documents are
    /// confined to user messages, and client citations on user text are
    /// refused. The shape is opt-in per provider, which is why it is an
    /// explicit version rather than behaviour inferred from the request.
    V2026_10_21,
}

impl AnthropicApiVersion {
    /// Which wire behaviour a provider's configured version header selects.
    ///
    /// The baseline is both the default and the fallback for anything
    /// unrecognised: a version this build has not met yet degrades to "send
    /// the full shape" rather than silently stripping fields that version may
    /// still accept.
    pub fn of(version: Option<&str>) -> Self {
        match version {
            Some("2026-10-21") => Self::V2026_10_21,
            _ => Self::Legacy,
        }
    }

    /// The `anthropic-version` header value this selection sends.
    pub fn header_value(self) -> &'static str {
        match self {
            Self::Legacy => "2023-06-01",
            Self::V2026_10_21 => "2026-10-21",
        }
    }
}

mod request_in;
mod request_out;
mod response;
mod stream_in;
mod stream_out;

#[cfg(test)]
mod tests;

pub use request_in::decode_request;
pub use request_out::encode_request;
pub use request_out::encode_request_for;
pub use response::{decode_response, encode_response, stop_reason_from_str, stop_reason_to_str};
// `decode_usage` is read only by billing.rs's own unit tests; nothing in the
// shipping build reaches it, so gate the re-export rather than leave it dead.
#[cfg(test)]
pub(crate) use response::decode_usage;

pub use stream_in::AnthropicStreamParser;
pub use stream_out::AnthropicStreamEncoder;
