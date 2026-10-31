//! Magpie Auto V10 policy regressions, using offline wire fixtures.
use serde_json::{json, Value};
use std::time::Duration;
use zroutery_core::error::Error;
use zroutery_core::rectifier::codex_auto::{self as auto, RetryBudget, RetryKind};

fn encrypted() -> Value {
    json!({"error":{"type":"invalid_request_error","code":"invalid_encrypted_content",
        "message":"The encrypted content could not be verified."}})
}
fn history() -> Value {
    json!({"model":"mock","store":true,"reasoning":{"effort":"xhigh"},
        "include":["reasoning.encrypted_content","message.output_text.logprobs"],"input":[
        {"type":"reasoning","id":"rs","encrypted_content":"sealed","summary":[]},
        {"type":"message","id":"msg","role":"assistant","phase":"commentary","content":"keep"},
        {"type":"function_call","id":"fc","call_id":"pair","name":"lookup","arguments":"{\"id\":\"nested\"}"},
        {"type":"function_call_output","id":"out","call_id":"pair","output":"done"},
        {"type":"custom_tool_call","id":"custom","call_id":"pair2","name":"exec","input":"echo hello"},
        {"type":"custom_tool_call_output","id":"custom_out","call_id":"pair2","output":"hello"},
        {"type":"web_search_call","id":"ws","status":"completed"}
    ]})
}

#[test]
fn cleanup_preserves_content_pairs_phases_current_effort_and_nested_ids() {
    let original = history();
    let mut body = original.clone();
    let report = auto::clean_history(&mut body).unwrap();
    assert_eq!(report.reasoning, 1);
    assert_eq!(report.ids, 5);
    assert_eq!(body["store"], false);
    assert_eq!(body["reasoning"], original["reasoning"]);
    assert_eq!(body["include"], json!(["message.output_text.logprobs"]));
    assert_eq!(body["input"][0]["phase"], "commentary");
    assert_eq!(
        body["input"][1]["arguments"],
        original["input"][2]["arguments"]
    );
    assert_eq!(body["input"][1]["call_id"], body["input"][2]["call_id"]);
    assert_eq!(
        body["input"].as_array().unwrap().last().unwrap()["id"],
        "ws"
    );
    assert_eq!(auto::clean_history(&mut body).unwrap(), Default::default());
    assert_eq!(
        original,
        history(),
        "a new client request starts with original history"
    );
}

#[test]
fn unsafe_cleanup_is_atomic_and_explains_why_context_was_not_discarded() {
    for extra in [
        json!({"previous_response_id":"old"}),
        json!({"conversation":"old"}),
        json!({"context_management":[{"type":"compaction"}]}),
        json!({"input":[{"type":"compaction","encrypted_content":"summary"}]}),
        json!({"input":[{"type":"item_reference","id":"old"}]}),
        json!({"input":[{"type":"message","encrypted_content":"opaque","role":"user"}]}),
    ] {
        let mut body = history();
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let original = body.clone();
        let error = auto::clean_history(&mut body).unwrap_err();
        assert!(error.to_string().contains("portable") || error.to_string().contains("compaction"));
        assert_eq!(body, original);
    }
}

#[test]
fn cleanup_matches_only_exact_http400_or_sse_errors_and_excludes_business_errors() {
    for status in [401, 403, 422, 429, 500, 503] {
        assert!(auto::classify_value(&encrypted(), status, false).is_none());
    }
    assert!(
        auto::classify_value(&encrypted(), 400, false)
            .unwrap()
            .cleanup
    );
    assert!(
        auto::classify_value(&encrypted(), 200, true)
            .unwrap()
            .cleanup
    );
    let resource = json!({"error":{"type":"invalid_request_error", "message":
        "The requested item was created under a different Azure OpenAI resource. Use the same resource that created the item to access it."}});
    assert_eq!(
        auto::classify_value(&resource, 400, false).unwrap().code,
        "different_azure"
    );
    for message in [
        "decryption failed",
        "invalid signature",
        "created under a different resource",
        "Account quota exhausted",
    ] {
        let value = json!({"error":{"type":"invalid_request_error","message":message}});
        assert!(auto::classify_value(&value, 400, false).is_none());
    }
    for kind in [
        "authentication_error",
        "invalid_api_key",
        "permission_denied",
    ] {
        let mut value = encrypted();
        value["error"]["type"] = json!(kind);
        assert!(auto::classify_value(&value, 400, false).is_none());
        assert!(auto::classify_value(&value, 200, true).is_none());
    }
}

#[test]
fn alb_retry_is_specific_and_missing_sse_error_details_are_transport() {
    let page="<html> <head><title>400 Bad Request</title></head><body><h1>400 Bad Request</h1><center>alb</center></body></html>";
    for body in [
        page.to_string(),
        json!({"error":{"code":"cc_switch_upstream_error","message":page}}).to_string(),
    ] {
        let error = Error::Upstream {
            provider: "mock".into(),
            status: 400,
            body,
            retry_after: None,
        };
        assert_eq!(
            auto::classify_error(&error).unwrap().kind,
            RetryKind::Transport
        );
    }
    for code in [
        "invalid_api_key",
        "insufficient_quota",
        "invalid_request_error",
    ] {
        let error = Error::Upstream {
            provider: "mock".into(),
            status: 400,
            body: json!({"error":{"code":code,"message":page}}).to_string(),
            retry_after: None,
        };
        assert!(auto::classify_error(&error).is_none());
    }
    let empty = json!({"type":"response.failed","response":{"error":null}});
    assert_eq!(
        auto::classify_value(&empty, 200, true).unwrap().code,
        "upstream_failed_without_details"
    );
}

#[test]
fn retry_allowances_are_per_kind_but_share_twenty_and_never_violate_server_floor() {
    let mut budget = RetryBudget::default();
    let remaining = Duration::from_secs(1800);
    for kind in [
        RetryKind::Resource,
        RetryKind::Transport,
        RetryKind::Availability,
    ] {
        for _ in 0..kind.limit() {
            let decision = auto::RetryDecision {
                kind,
                code: "fixture",
                cleanup: false,
            };
            if budget.total < 20 {
                assert!(budget
                    .schedule(decision, Duration::ZERO, remaining, 0)
                    .is_some());
            } else {
                assert!(budget
                    .schedule(decision, Duration::ZERO, remaining, 0)
                    .is_none());
            }
        }
    }
    assert_eq!(budget.total, 20);
    assert_eq!(
        RetryKind::RateLimit.delay(19, Duration::ZERO, 0),
        Duration::from_secs(60)
    );
    assert_eq!(
        RetryKind::Transport.delay(9, Duration::ZERO, 0),
        Duration::from_secs(15)
    );
    assert_eq!(
        RetryKind::Availability.delay(9, Duration::ZERO, 0),
        Duration::from_secs(60)
    );
    assert_eq!(
        RetryKind::RateLimit.delay(0, Duration::from_secs(90), 500),
        Duration::from_millis(90500)
    );
    let mut budget = RetryBudget::default();
    let decision = auto::classify_value(&encrypted(), 400, false).unwrap();
    assert!(budget.schedule(decision, remaining, remaining, 0).is_none());
    assert_eq!(budget.total, 0);
    assert!(!budget.cleaned);
}

#[test]
fn staged_sse_business_errors_keep_canonical_failure_class_and_non_success_status() {
    use zroutery_core::failure::FailureClass;
    for (code, class) in [
        ("invalid_api_key", FailureClass::Authentication),
        ("rate_limit_exceeded", FailureClass::RateLimit),
        ("insufficient_quota", FailureClass::BalanceExhausted),
    ] {
        let error = Error::Upstream {
            provider: "mock".into(),
            status: 200,
            body: json!({"type":"response.failed","response":{"error":{"code":code}}}).to_string(),
            retry_after: None,
        };
        assert_eq!(error.classified().class, class);
        assert!(!error.status().is_success());
    }
    let error = Error::Upstream {
        provider: "mock".into(),
        status: 200,
        body: json!({"type":"response.failed",
        "response":{"error":{"type":"authentication_error","code":"invalid_encrypted_content"}}})
        .to_string(),
        retry_after: None,
    };
    assert_eq!(error.classified().class, FailureClass::Authentication);
}
