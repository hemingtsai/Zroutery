//! Google Gemini native API dialect (`generateContent`).
//!
//! This module provides a practical subset of the Gemini translation: system
//! instructions, multi-part contents, function declarations/calls, image parts,
//! and a snapshot-style SSE parser/encoder.

mod request_in;
mod request_out;
mod response;
mod stream_in;
mod stream_out;

pub use request_in::decode_request;
pub use request_out::encode_request;
pub use response::{decode_response, encode_response};
pub use stream_in::GeminiStreamParser;
pub use stream_out::GeminiStreamEncoder;
