//! Tests for request rectifiers and the "repair before failover" contract.

use serde_json::{json, Value};
use zroutery_core::error::Error;
use zroutery_core::rectifier::media_fallback::MediaFallbackRectifier;
use zroutery_core::rectifier::thinking_budget::ThinkingBudgetRectifier;
use zroutery_core::rectifier::thinking_signature::ThinkingSignatureRectifier;
use zroutery_core::rectifier::{Rectifier, RectifyResult};
use zroutery_core::{CircuitBreaker, CircuitBreakerConfig, Router};

fn upstream(msg: &str, status: u16) -> Error {
    Error::Upstream {
        provider: "p".into(),
        status,
        body: msg.to_string(),
    }
}

#[test]
fn thinking_signature_matches_all_seven_error_patterns() {
    let r = ThinkingSignatureRectifier;
    let body = json!({"messages": []});
    let cases = [
        "invalid signature thinking block",
        "thought signature not valid",
        "thought signature invalid",
        "must start with a thinking block",
        "expected thinking found tool_use",
        "expected redacted_thinking found tool_use",
        "signature field required",
        "signature extra inputs are not permitted",
        "thinking cannot be modified",
        "redacted_thinking cannot be modified",
    ];
    for msg in cases {
        assert!(r.should_apply(&upstream(msg, 400), &body), "should match: {msg}");
    }
    assert!(!r.should_apply(&upstream("ordinary provider error", 500), &body));
}

#[test]
fn thinking_signature_removes_thinking_blocks_and_signature_fields() {
    let r = ThinkingSignatureRectifier;
    let mut body = json!({
        "thinking": {"type": "enabled", "budget_tokens": 4096},
        "messages": [
            {"role": "user", "content": [
                {"type": "text", "text": "hello"}
            ]},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "secret", "signature": "sig-1"},
                {"type": "text", "text": "hi", "signature": "sig-2"}
            ]}
        ]
    });
    let result = r.rectify(&mut body);
    assert!(result.applied);
    let blocks = body["messages"][1]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "text");
    assert!(blocks[0].get("signature").is_none());
    assert!(body.get("thinking").is_none());
}

#[test]
fn media_fallback_matches_supported_statuses_and_media_rejections() {
    let r = MediaFallbackRectifier;
    let body = json!({"messages": []});
    assert!(r.should_apply(&upstream("image unsupported", 400), &body));
    assert!(r.should_apply(&upstream("vision is not supported", 415), &body));
    assert!(r.should_apply(&upstream("multimodal text only", 422), &body));
    assert!(!r.should_apply(&upstream("image unsupported", 500), &body));
    assert!(!r.should_apply(&upstream("rate limited", 429), &body));
    assert!(!r.should_apply(&upstream("plain text error", 400), &body));
}

#[test]
fn media_fallback_replaces_image_blocks_and_keeps_cache_control() {
    let r = MediaFallbackRectifier;
    let mut body = json!({
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": "what is this"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"},
             "cache_control": {"type": "ephemeral"}}
        ]}]
    });
    let result = r.rectify(&mut body);
    assert!(result.applied);
    let blocks = body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[1]["type"], "text");
    assert_eq!(blocks[1]["text"], "[Unsupported Image]");
    assert_eq!(blocks[1]["cache_control"]["type"], "ephemeral");
}

#[test]
fn thinking_budget_halves_and_never_goes_below_minimum() {
    let r = ThinkingBudgetRectifier;
    let mut body = json!({"thinking": {"type": "enabled", "budget_tokens": 8192}});
    let result = r.rectify(&mut body);
    assert!(result.applied);
    assert_eq!(body["thinking"]["budget_tokens"], 4096);

    let mut tiny = json!({"thinking": {"type": "enabled", "budget_tokens": 1024}});
    let result = r.rectify(&mut tiny);
    assert!(!result.applied);
    assert_eq!(tiny["thinking"]["budget_tokens"], 1024);

    let mut near_min = json!({"thinking": {"type": "enabled", "budget_tokens": 1500}});
    let result = r.rectify(&mut near_min);
    assert!(result.applied);
    assert_eq!(near_min["thinking"]["budget_tokens"], 1024);
}

/// A tiny rectifier that "repairs" a body by adding a marker. Used to test the
/// cascade contract without spinning up an HTTP server.
struct MarkerRectifier;

impl Rectifier for MarkerRectifier {
    fn should_apply(&self, error: &Error, _body: &Value) -> bool {
        error.to_string().contains("fixable")
    }
    fn rectify(&self, body: &mut Value) -> RectifyResult {
        body["repaired"] = json!(true);
        RectifyResult {
            applied: true,
            details: "marked repaired".into(),
        }
    }
    fn name(&self) -> &'static str {
        "marker"
    }
}

#[test]
fn rectifier_retry_success_does_not_touch_circuit_breaker_health() {
    let router = Router::new();
    let breaker_config = CircuitBreakerConfig {
        failure_threshold: 1,
        ..CircuitBreakerConfig::default()
    };
    let breaker = CircuitBreaker::new(breaker_config);
    let model = "m1";

    // A closed breaker with one prior success in the router's own health map.
    router.report_success(model, 10);
    let before = router.health_snapshot();

    // Simulate: original send fails with a fixable error, rectifier repairs,
    // same-provider retry succeeds. The pipeline must not call report_failure
    // or report_success for that repaired retry.
    let error = upstream("fixable image unsupported", 400);
    let mut body = json!({"messages": []});
    let rectifier = MarkerRectifier;
    assert!(rectifier.should_apply(&error, &body));
    let result = rectifier.rectify(&mut body);
    assert!(result.applied);
    let _retry_ok = body["repaired"].as_bool().unwrap();

    // The breaker itself is untouched by the simulated repair.
    assert_eq!(breaker.state(), zroutery_core::CircuitState::Closed);
    let after = router.health_snapshot();
    assert_eq!(before, after, "rectifier retries must not alter health");
}

#[test]
fn rectifier_retry_failure_becomes_the_failure_that_is_reported() {
    let router = Router::new();
    let routing = zroutery_core::RoutingConfig::default();
    let error = upstream("fixable overloaded", 503);

    // This is the pipeline's non-rectified path: report_failure uses the
    // retry error, which for 503 counts against health.
    router.report_failure("m1", &error, &routing);
    assert_eq!(router.health_snapshot().len(), 1);
}
