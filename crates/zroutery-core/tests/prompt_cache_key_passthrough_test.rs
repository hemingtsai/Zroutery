//! Verify that `prompt_cache_key` survives the `/v1/responses` decode→encode
//! path, so a real client's cache partition is forwarded verbatim instead of
//! being silently dropped by Zroutery.
//!
//! This is the regression test for the passthrough path: earlier the field was
//! absent from both decode and encode, so a client that set it lost its cache
//! affinity at the proxy boundary.

use serde_json::json;
use zroutery_core::protocol;

#[test]
fn prompt_cache_key_survives_a_responses_round_trip() {
    let body = json!({
        "model": "gpt-6-astra",
        "input": [{"role": "user", "content": "hi"}],
        "prompt_cache_key": "conv-abc-123"
    });

    let req = protocol::responses::decode_request(body.clone()).expect("the request should decode");
    assert_eq!(
        req.passthrough.get("prompt_cache_key"),
        Some(&json!("conv-abc-123")),
        "decode should preserve prompt_cache_key in the passthrough map"
    );

    let out = protocol::responses::encode_request(&req, "gpt-6-astra")
        .expect("the request should encode");
    assert_eq!(
        out.get("prompt_cache_key"),
        Some(&json!("conv-abc-123")),
        "encode should forward prompt_cache_key into the upstream body"
    );
}

#[test]
fn a_missing_prompt_cache_key_is_ab_present_not_null() {
    let body = json!({
        "model": "gpt-6-astra",
        "input": [{"role": "user", "content": "hi"}]
    });

    let req = protocol::responses::decode_request(body).expect("decode");
    let out = protocol::responses::encode_request(&req, "gpt-6-astra").expect("encode");

    assert!(
        out.get("prompt_cache_key").is_none(),
        "when the client did not set prompt_cache_key, Zroutery must not invent one"
    );
}
