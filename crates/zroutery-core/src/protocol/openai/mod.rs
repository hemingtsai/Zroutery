//! OpenAI Chat Completions dialect, also used for every "OpenAI compatible"
//! provider such as DeepSeek, Groq, Ollama, vLLM and OpenRouter.
//!
//! The tricky direction is OpenAI -> canonical: chunks carry no block structure,
//! so [`OpenAiStreamParser`] synthesises Anthropic style block indices with a
//! small state machine.
//!
//! # Why this is five files
//!
//! Same shape as the Responses dialect, and split on the same five boundaries
//! so a reader who knows one knows the other. The dialect really is five
//! responsibilities: a decoder, an encoder, the non-streaming response shape,
//! and the two halves of the streaming lifecycle. Before the split the outbound
//! encoder sat 1500 lines from the decoder it has nothing to do with.
//!
//! * [`request_in`] — messages in, canonical IR out.
//! * [`request_out`] — canonical IR in, a body out.
//! * [`response`] — non-streaming completions, stop reasons, wire usage.
//! * [`stream_in`] — the inbound SSE parser.
//! * [`stream_out`] — the outbound SSE encoder.
//!
//! # The tests stayed whole
//!
//! [`tests`] is one module rather than five, deliberately. The assertions here
//! are dialect-level — `parses_text_stream_with_usage_trailer` is about the whole
//! path through the parser and the usage accounting together — so splitting them
//! by half would scatter closely related assertions and duplicate the two
//! stream helpers across files. It stays black-box against this barrel, which is
//! what a test of a wire format should be.

mod request_in;
mod request_out;
mod response;
mod stream_in;
mod stream_out;

#[cfg(test)]
mod tests;

pub use request_in::{decode_request, wants_stream_usage};
pub use request_out::{encode_request, encode_request_with, validate_tool_history};
pub use response::{decode_response, encode_response};
pub use stream_in::OpenAiStreamParser;
pub use stream_out::OpenAiStreamEncoder;

// The helpers more than one half needs are `pub(super)` in the half that defines
// them and imported across the sibling boundary by path — not re-exported here.
// A `pub(super) use` would have to be wider than the item it re-exports, which
// Rust refuses; a private `pub(crate)` instead would leak six internals into the
// crate's namespace for no reason. Naming them at the point of use keeps the
// list of shared helpers short enough to be worth reading.
