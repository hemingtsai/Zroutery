//! OpenAI Responses API (`/v1/responses`) dialect.
//!
//! This is the newer OpenAI format built around items and parts. The decoder
//! lifts Responses items into the canonical IR, the encoder renders IR content
//! back into a Responses response, and the stream parser/encoder handle the SSE
//! lifecycle.
//!
//! # Why this is five files
//!
//! The dialect has five responsibilities that share no code except a handful of
//! helpers, and the five section markers this file used to carry said so
//! explicitly. Keeping them in one file meant a change to the streaming encoder
//! — the largest and most churn-prone part, since it tracks a public API that
//! keeps changing — sat 2000 lines away from the request decoder it has nothing
//! to do with, and the reader had to hold five things in mind to answer any one
//! question.
//!
//! The split is `openai.rs`'s, deliberately: both dialects are organised the
//! same way, so a reader who knows one knows the other.
//!
//! * [`request_in`] — items and parts in, canonical IR out.
//! * [`request_out`] — canonical IR in, a request body out.
//! * `sanitize` — make that body portable across upstreams (history ids,
//!   reasoning items and stored-response references belong to one resource).
//! * [`response`] — non-streaming responses, and the wire usage shape.
//! * [`stream_in`] — the inbound SSE parser.
//! * [`stream_out`] — the outbound SSE encoder.
//!
//! Everything the rest of the crate used before the split is re-exported below,
//! so `protocol::responses::decode_request` still resolves. Three helpers are
//! shared between the halves — `encode_text_format`, `arguments_json` and the
//! usage pair — and are `pub(super)` for that reason alone.

mod request_in;
mod request_out;
mod response;
mod sanitize;
mod stream_in;
mod stream_out;
mod tools;

pub use request_in::{decode_history, decode_request, input_items, StoragePolicy};
pub use request_out::{encode_request, encode_request_with};
pub use response::{decode_response, encode_response};
pub use stream_in::ResponsesStreamParser;
pub use stream_out::ResponsesStreamEncoder;
pub use tools::{restore_response_tools, restore_stream_frames};
pub fn response_usage(body: &serde_json::Value) -> crate::ir::Usage {
    response::decode_usage(body.get("usage"))
}
