//! Protocol golden conformance tests.
//!
//! These tests verify that wire-format to IR conversions are correct by testing
//! against known-good fixtures.  Each test decodes a protocol-specific JSON
//! payload into the canonical IR, asserts on the IR fields, then re-encodes
//! and verifies key fields survive the round-trip.

mod anthropic;
mod cross_protocol;
mod edge_cases;
mod gemini;
mod openai;
mod responses;
mod streaming;
mod structured_output;
