use serde_json::json;
use zroutery_core::failure::FailureClass;
use zroutery_core::outcome::FinalStatus;

use super::support::*;

// ------------------------------------------------------------------ gate 3 (streaming)

/// A stream that ends normally is one served outcome, charged once, with the
/// handshake's health report not repeated at the end.
#[tokio::test]
async fn a_completed_stream_is_one_served_outcome() {
    let h = Harness::new().await;

    let response = h.ask_streaming("alpha-good-model").await;
    let wire = response.text().await.unwrap();
    assert!(
        wire.contains("\"text\":\"hi\""),
        "the client saw the answer: {wire}"
    );

    let record = h.record();
    assert!(record.ok);
    assert_eq!(record.status, 200);
    assert!(record.stream);
    assert!(record.cost.is_some(), "a stream's spend is still recorded");

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert!(outcome.streaming);
    assert_eq!(outcome.dialect, "anthropic");
    let served = outcome.served_identity().expect("an answer was delivered");
    assert_eq!(served.model(), "alpha-good-model");
    assert_eq!(outcome.last_attempted_identity(), Some(served.clone()));
    assert_eq!(outcome.planned_identity(), Some(served));
    assert_eq!(outcome.usage.map(|usage| usage.output_tokens), Some(2));
    assert_eq!(
        outcome.actual_cost,
        record.cost.as_ref().map(|cost| cost.amount)
    );
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    let health = h.state.router().health_snapshot();
    let alpha = health
        .iter()
        .find(|model| model.model_id == "alpha-good-model")
        .unwrap();
    assert_eq!(
        alpha.total_success, 1,
        "the handshake is reported once, not again at the end"
    );
    assert_eq!(alpha.total_failure, 0);

    h.shutdown().await;
}

/// A stream that fails after the handshake is a failed request, not a served
/// one, and the failure is classified once through the same path.
#[tokio::test]
async fn a_failed_stream_handshake_is_a_failed_outcome() {
    let h = Harness::new().await;

    let response = h.ask_streaming("beta-broken-model").await;
    assert_eq!(response.status(), 500);
    let _ = response.text().await.unwrap();

    let record = h.record();
    assert!(!record.ok);
    assert_eq!(record.status, 500);

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(outcome.attempts.len(), 1);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::Unknown)
    );
    assert!(outcome.served_identity().is_none());
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    let breakdown = h
        .state
        .router()
        .stats_store
        .get("beta-broken-model", "beta");
    assert_eq!(breakdown.failures.count(FailureClass::Unknown), 1);

    h.shutdown().await;
}

/// The drop path may not resolve to a success in the source either.
#[test]
fn dropping_a_stream_is_not_a_success_in_the_source_either() {
    // `SseState` and its `Drop` live in the pipeline's sse child, not the
    // barrel, so this asserts against the file that actually carries the impl.
    let source = include_str!("../../src/server/pipeline/sse.rs");
    let (_, after) = source
        .split_once("impl Drop for SseState")
        .expect("the stream state still has a drop path");
    let (drop_impl, _) = after.split_once("\n}").expect("the drop impl is closed");
    assert!(
        drop_impl.contains("ClientDisconnected"),
        "a dropped stream finalizes as a client disconnect"
    );
    assert!(
        !drop_impl.contains("TerminalKind::Served"),
        "a dropped stream is never finalized as served"
    );
    assert!(
        !drop_impl.contains("finalize(None)"),
        "a dropped stream is never finalized with no terminal state"
    );
}

// ------------------------------------------------------------------ PL-01 truncated streams

/// A 200 SSE body that ends without any terminal event is a truncated answer,
/// not a completed one: the client keeps what streamed, the request is
/// recorded as an interruption, and the provider is not credited with a
/// success it did not deliver.
#[tokio::test]
async fn a_truncated_stream_is_never_a_completed_success() {
    let h = Harness::new().await;

    let response = h.ask_streaming("alpha-truncate-model").await;
    assert_eq!(response.status(), 200, "the handshake succeeded");
    let wire = response.text().await.unwrap();
    assert!(
        wire.contains("partial answer"),
        "the client keeps the bytes that did arrive: {wire}"
    );
    assert!(
        wire.contains("event: error"),
        "the stream ends with a failure terminal, not a normal stop: {wire}"
    );
    assert!(
        !wire.contains("event: message_stop"),
        "an unterminated stream must not look finished: {wire}"
    );

    let record = h.record();
    assert!(!record.ok, "a truncated answer is not a success");
    assert_eq!(record.status, 502);
    assert!(
        record
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("terminal event"),
        "activity names the truncation: {:?}",
        record.error
    );

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Interrupted);
    assert_eq!(outcome.failure_class(), Some(FailureClass::Interrupted));
    assert!(!outcome.is_terminal_success());
    assert!(
        outcome.served_identity().is_none(),
        "nothing delivered a finished answer"
    );
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    // The handshake really happened, so it is reported once; the cut is an
    // interruption, not evidence that the provider is unhealthy.
    let health = h.state.router().health_snapshot();
    let alpha = health
        .iter()
        .find(|model| model.model_id == "alpha-truncate-model")
        .expect("the handshake was reported");
    assert_eq!(alpha.total_success, 1);
    assert_eq!(alpha.total_failure, 0);
    let breakdown = h
        .state
        .router()
        .stats_store
        .get("alpha-truncate-model", "alpha");
    assert_eq!(breakdown.failures.count(FailureClass::Interrupted), 1);

    h.shutdown().await;
}

/// Usage the relay reported before it cut the stream is real spend: the
/// outcome stays a failure, but the ledger keeps what actually happened.
#[tokio::test]
async fn a_truncated_stream_keeps_the_usage_it_reported() {
    let h = Harness::new().await;

    let response = h.ask_streaming("alpha-truncate-usage-model").await;
    let wire = response.text().await.unwrap();
    assert!(wire.contains("partial answer"), "{wire}");

    let record = h.record();
    assert!(!record.ok);
    assert!(record.cost.is_some(), "the partial spend is still recorded");

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Interrupted);
    assert_eq!(
        outcome
            .usage
            .map(|usage| (usage.input_tokens, usage.output_tokens)),
        Some((11, 7)),
        "the usage reported before the cut survives"
    );
    assert_eq!(
        outcome.actual_cost,
        record.cost.as_ref().map(|cost| cost.amount)
    );
    assert!(
        h.spent_today() > 0.0,
        "the spend reached the ledger even though the answer did not finish"
    );

    h.shutdown().await;
}

/// An empty 200 body names no answer at all, so it is a failure rather than an
/// empty success.
#[tokio::test]
async fn an_empty_upstream_stream_body_is_a_failure() {
    let h = Harness::new().await;

    let response = h.ask_streaming("alpha-empty-stream-model").await;
    assert_eq!(response.status(), 200);
    let wire = response.text().await.unwrap();
    assert!(
        wire.contains("event: error"),
        "an empty body still reports a terminal failure: {wire}"
    );

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Interrupted);
    assert!(outcome.served_identity().is_none());
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    h.shutdown().await;
}

/// The Responses dialect gets the same treatment: a truncated upstream stream
/// becomes `response.failed`, never `response.completed`.
#[tokio::test]
async fn a_truncated_responses_stream_fails_rather_than_completing() {
    let h = Harness::new().await;

    let response = h
        .post("/v1/responses")
        .json(&json!({"model": "alpha-truncate-model", "input": "hi", "stream": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let wire = response.text().await.unwrap();
    assert!(
        wire.contains("response.failed"),
        "the terminal frame is a failure: {wire}"
    );
    assert!(
        !wire.contains("response.completed"),
        "a truncated Responses stream must not be announced as completed: {wire}"
    );

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Interrupted);
    assert!(outcome.served_identity().is_none());
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    h.shutdown().await;
}
