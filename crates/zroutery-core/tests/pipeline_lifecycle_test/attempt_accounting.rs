use serde_json::json;
use zroutery_core::config::ModelTier;
use zroutery_core::failure::FailureClass;
use zroutery_core::outcome::FinalStatus;

use super::support::*;

// --------------------------------------------------- per-attempt failure accounting

/// The response id the proxy published in the first frame of an SSE body.
fn response_id_from_frame(frame: &[u8]) -> String {
    let wire = String::from_utf8_lossy(frame);
    wire.split("\"id\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("the first frame names the response")
        .to_string()
}

/// After candidate A fails at the handshake, candidate B's mid-stream failure
/// is still recorded in B's health and classified-failure statistics. The dedup
/// flag belongs to the attempt, not the request.
#[tokio::test]
async fn a_later_candidates_stream_failure_is_recorded() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(
        failover_config(addr, "broken-model", "streamerr-model"),
        mock,
    )
    .await;

    let response = h.ask_streaming("fast-class").await;
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(outcome.attempts.len(), 2, "two candidates, two attempts");
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::Unknown)
    );
    assert_eq!(
        outcome.attempts[1].failure_class,
        Some(FailureClass::ProviderUnavailable)
    );
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    let later = h
        .state
        .router()
        .stats_store
        .get("beta-streamerr-model", "beta");
    assert_eq!(
        later.failures.count(FailureClass::ProviderUnavailable),
        1,
        "the later candidate's failure is not swallowed by the earlier one"
    );
    let health = h.state.router().health_snapshot();
    let first = health
        .iter()
        .find(|row| row.model_id == "alpha-broken-model")
        .expect("A failed at the handshake");
    assert_eq!(first.total_failure, 1);
    let second = health
        .iter()
        .find(|row| row.model_id == "beta-streamerr-model")
        .expect("B answered the handshake");
    assert_eq!(
        second.total_failure, 1,
        "B's stream failure is counted once"
    );

    h.shutdown().await;
}

/// A client cancellation after an earlier candidate failed does not punish the
/// model that was reading: the cancellation is recorded as its own classified
/// fact and leaves B's health alone.
#[tokio::test]
async fn a_cancellation_after_an_earlier_failure_does_not_punish_the_reading_model() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(failover_config(addr, "broken-model", "cancel-model"), mock).await;

    let mut response = h
        .post("/v1/responses")
        .json(&json!({"model": "fast-class", "input": "hi", "stream": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let first = response.chunk().await.unwrap().expect("some output");
    let response_id = response_id_from_frame(&first);

    let cancelled = h
        .post(&format!("/v1/responses/{response_id}/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(cancelled.status(), 200);
    let _ = cancelled.text().await;
    let _ = response.text().await;
    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Cancelled);
    assert_eq!(outcome.attempts.len(), 2);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::Unknown)
    );
    assert_eq!(
        outcome.attempts[1].failure_class,
        Some(FailureClass::ClientCancelled)
    );
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    let health = h.state.router().health_snapshot();
    let reading = health
        .iter()
        .find(|row| row.model_id == "beta-cancel-model")
        .expect("B answered the handshake");
    assert_eq!(
        reading.total_failure, 0,
        "a cancellation is the client's doing, not B's"
    );
    assert!(!h.state.router().is_cooling("beta-cancel-model"));

    h.shutdown().await;
}

/// A failure before a later success keeps both attempts and credits the model
/// that actually served.
#[tokio::test]
async fn a_failure_before_a_later_success_keeps_both_attempts() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(failover_config(addr, "broken-model", "good-model"), mock).await;

    let response = h.ask_streaming("fast-class").await;
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert_eq!(outcome.attempts.len(), 2);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::Unknown)
    );
    assert!(outcome.attempts[1].is_terminal_success());
    assert_eq!(
        outcome.served_identity().map(|id| id.model().to_string()),
        Some("beta-good-model".to_string())
    );
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    let health = h.state.router().health_snapshot();
    let served = health
        .iter()
        .find(|row| row.model_id == "beta-good-model")
        .expect("B served");
    assert_eq!(served.total_success, 1);
    assert_eq!(served.total_failure, 0);

    h.shutdown().await;
}

/// A single candidate whose stream fails mid-way still records that failure
/// exactly once — the reset must not double-count it.
#[tokio::test]
async fn a_single_stream_failure_is_recorded_once() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models = vec![model("beta", "streamerr-model", 0, ModelTier::Fast)];
    let h = Harness::start(cfg, mock).await;

    let response = h.ask_streaming("fast-class").await;
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert_eq!(outcome.attempts.len(), 1);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::ProviderUnavailable)
    );
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    let breakdown = h
        .state
        .router()
        .stats_store
        .get("beta-streamerr-model", "beta");
    assert_eq!(
        breakdown.failures.count(FailureClass::ProviderUnavailable),
        1
    );
    let health = h.state.router().health_snapshot();
    assert_eq!(health[0].total_failure, 1, "counted once");

    h.shutdown().await;
}
