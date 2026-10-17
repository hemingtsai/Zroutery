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

mod request_in;
mod request_out;
mod response;
mod stream_in;
mod stream_out;

#[cfg(test)]
mod tests;

pub use request_in::decode_request;
pub use request_out::encode_request;
pub use response::{decode_response, encode_response, stop_reason_from_str, stop_reason_to_str};
// `decode_usage` is read only by billing.rs's own unit tests; nothing in the
// shipping build reaches it, so gate the re-export rather than leave it dead.
#[cfg(test)]
pub(crate) use response::decode_usage;

pub use stream_in::AnthropicStreamParser;
pub use stream_out::AnthropicStreamEncoder;
