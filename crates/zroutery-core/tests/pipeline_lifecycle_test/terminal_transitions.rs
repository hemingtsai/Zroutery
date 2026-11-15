use std::time::Duration;

use serde_json::{json, Value};
use zroutery_core::config::ModelTier;
use zroutery_core::failure::FailureClass;
use zroutery_core::outcome::FinalStatus;

use super::support::*;

#[tokio::test]
async fn response_completed_ends_the_request_without_waiting_for_upstream_eof() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models = vec![model(
        "alpha",
        "terminal-tail-model",
        0,
        ModelTier::Standard,
    )];
    let h = Harness::start(cfg, mock).await;
    let since = chrono::Utc::now() - chrono::Duration::seconds(1);
    let mut response = h
        .responses_request("standard-class", None, true)
        .header("x-opencode-client", "cli")
        .header("x-opencode-session", "test-session")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut wire = String::new();
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(3), response.chunk())
            .await
            .expect("the client must not wait for TCP EOF")
            .unwrap()
            .expect("response.completed arrives");
        wire.push_str(&String::from_utf8_lossy(&chunk));
        if wire.contains("event: response.completed") {
            break;
        }
    }
    drop(response);
    wait_for_outcomes(&h, 1).await;
    assert!(h.record().ok, "completed output must not become 499");
    assert_eq!(h.record().status, 200);
    assert_eq!(h.outcome().final_status, FinalStatus::Success);
    assert_eq!(h.record().usage.input_tokens, 11);
    assert_eq!(h.record().usage.output_tokens, 7);
    assert!(h.state.stats().client_seen_since("opencode", since));
    h.shutdown().await;
}

/// A relay's HTTP-200 error stream is still a failed handshake until any actual
/// answer byte is released. It can fail over without publishing response.failed.
#[tokio::test]
async fn an_http200_quota_frame_before_output_fails_over_without_partial_replay() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.providers[0].api_surface = serde_json::from_value(json!("responses")).unwrap();
    cfg.models = vec![
        model("alpha", "preoutput-quota-model", 0, ModelTier::Standard),
        model("beta", "good-model", 10, ModelTier::Standard),
    ];
    let h = Harness::start(cfg, mock).await;
    let response = h
        .responses_request("standard-class", Some(false), true)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let wire = response.text().await.unwrap();
    assert!(wire.contains("event: response.completed"));
    assert!(!wire.contains("event: response.failed"));
    let record = h.record();
    assert_eq!(record.resolved_model.as_deref(), Some("beta-good-model"));
    let outcome = h.outcome();
    assert_eq!(outcome.attempts.len(), 2);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::RateLimit)
    );
    assert!(outcome.attempts[1].is_terminal_success());
    assert_eq!(h.mock.count(), 2);
    h.shutdown().await;
}

#[tokio::test]
async fn chat_refusal_before_output_fails_over_with_its_class_and_known_cost() {
    for (suffix, class) in [
        ("rate", FailureClass::RateLimit),
        ("wallet", FailureClass::BalanceExhausted),
    ] {
        let (addr, mock) = start_mock().await;
        let first = format!("chat-refusal-{suffix}");
        let h = Harness::start(failover_config(addr, &first, "good-model"), mock).await;
        let response = h.ask_streaming("fast-class").await;
        assert_eq!(response.status(), 200);
        let wire = response.text().await.unwrap();
        assert!(!wire.contains("event: error"), "{wire}");
        assert!(wire.contains("message_stop"), "{wire}");
        wait_for_outcomes(&h, 1).await;

        let outcome = h.outcome();
        assert_eq!(outcome.final_status, FinalStatus::Success);
        assert_eq!(outcome.attempts.len(), 2);
        assert_eq!(outcome.attempts[0].failure_class, Some(class));
        assert!(outcome.attempts[0].cost.is_some_and(|cost| cost > 0.0));
        assert!(outcome.attempts[1].is_terminal_success());
        assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());
        assert_eq!(h.mock.count_of(&first), 1);
        assert_eq!(h.mock.count_of("good-model"), 1);
        if class == FailureClass::RateLimit {
            assert!(h
                .state
                .router()
                .model_rate_limit_remaining("alpha", &format!("alpha-{first}"))
                .is_some_and(|remaining| remaining > Duration::from_secs(118)));
            assert!(!h.state.router().is_circuit_open(&format!("alpha-{first}")));
        } else {
            assert!(h.state.router().is_provider_gated("alpha"));
        }
        h.shutdown().await;
    }
}

#[tokio::test]
async fn chat_refusal_after_output_keeps_the_partial_answer_without_replay() {
    let (addr, mock) = start_mock().await;
    let first = "chat-partial-refusal-rate";
    let h = Harness::start(failover_config(addr, first, "good-model"), mock).await;
    let response = h.ask_streaming("fast-class").await;
    assert_eq!(response.status(), 200);
    let wire = response.text().await.unwrap();
    assert!(wire.contains("partial answer"), "{wire}");
    assert!(wire.contains("rate_limit_error"), "{wire}");
    assert!(
        !wire.contains("message_stop"),
        "a failure is not a completed turn: {wire}"
    );
    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(outcome.attempts.len(), 1);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::RateLimit)
    );
    assert!(outcome.attempts[0].cost.is_some_and(|cost| cost > 0.0));
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());
    assert_eq!(h.record().usage.input_tokens, 11);
    assert_eq!(h.record().usage.output_tokens, 7);
    assert_eq!(h.mock.count_of(first), 1);
    assert_eq!(h.mock.count_of("good-model"), 0);
    h.shutdown().await;
}

#[tokio::test]
async fn chat_rate_refusal_returns_provider_and_retry_after_then_cools_the_scope() {
    let (addr, mock) = start_mock().await;
    let first = "chat-refusal-rate";
    let id = format!("alpha-{first}");
    let mut cfg = config_for(addr);
    cfg.models = vec![model("alpha", first, 0, ModelTier::Fast)];
    let h = Harness::start(cfg, mock).await;
    let response = h.ask_streaming(&id).await;
    assert_eq!(response.status(), 429);
    assert_eq!(response.headers()["retry-after"], "120");
    let wire: Value = response.json().await.unwrap();
    assert_eq!(wire["error"]["type"], "rate_limit_error");
    assert_eq!(wire["error"]["message"], "upstream Alpha returned 429");
    assert!(!h.record().ok);
    assert_eq!(h.outcome().attempts.len(), 1);

    let cooled = h.ask_streaming(&id).await;
    assert_eq!(cooled.status(), 429);
    assert!(
        cooled.headers()["retry-after"]
            .to_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            >= 118
    );
    let _ = cooled.text().await.unwrap();
    assert_eq!(h.mock.count(), 1, "a cooled candidate is not sent again");
    assert!(!h.state.router().is_circuit_open(&id));
    h.shutdown().await;
}

// ------------------------------------------------------------------ gate 4

/// REG-007: a client that walks away mid-answer is not a success.
///
/// The abandoned answer used to be finalized with no error at all, so Activity,
/// the stats summary and the dashboard all reported a completed request. Here
/// the client reads part of the stream and disconnects, and the terminal state
/// has to say what actually happened.
#[tokio::test]
async fn a_client_disconnect_is_never_recorded_as_success() {
    let h = Harness::new().await;

    let mut response = h.ask_streaming("alpha-hold-model").await;
    assert_eq!(response.status(), 200);
    // Part of the answer arrives, so the drop lands mid-answer rather than
    // before the first byte.
    let first = response.chunk().await.unwrap().expect("some output");
    assert!(!first.is_empty(), "the mock produced answer bytes");
    drop(response);

    wait_for_outcomes(&h, 1).await;

    let record = h.record();
    assert!(!record.ok, "an abandoned answer is not a success");
    assert_eq!(record.status, 499, "the client closed the request");
    let error = record.error.as_deref().unwrap_or_default();
    assert!(
        error.contains("client disconnected"),
        "activity explains the terminal state: {error}"
    );

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Interrupted);
    assert!(!outcome.is_terminal_success());
    assert_eq!(outcome.failure_class(), Some(FailureClass::Interrupted));
    // The candidate that was streaming is the last attempt, and nothing served.
    let identity = outcome
        .last_attempted_identity()
        .expect("a stream that started has a last attempt");
    assert_eq!(identity.model(), "alpha-hold-model");
    assert_eq!(identity.provider(), "alpha");
    assert_eq!(outcome.planned_identity(), Some(identity.clone()));
    assert!(
        outcome.served_identity().is_none(),
        "no answer was delivered"
    );
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    h.shutdown().await;
}

/// A truncated stream that reported usage before it vanished is charged, and the
/// charge is attributed to the attempt that incurred it.
///
/// The second half is the part that was missing. `Error::InterruptedStream`
/// carries the usage the body reported, and the streaming path deliberately keeps
/// it — but "the spend is accounted for" used to mean only that the *ledger*
/// recorded a total. Per-attempt attribution is what the ML cost axis reads, and
/// `Targets::from_attempt` reads `attempt.cost`. A request that failed after
/// being billed would otherwise contribute a cost-free attempt sample, and a
/// failover chain's waste is exactly the spend a cost-aware router most needs to
/// see.
///
/// The fixture already existed for the ledger half (`truncate-usage-model`
/// reports 11 prompt and 7 completion tokens and then stops). What is new is the
/// assertion that the tokens land on the attempt rather than only in a total.
#[tokio::test]
async fn a_truncated_stream_that_reported_usage_is_charged_to_its_attempt() {
    let h = Harness::new().await;

    let response = h.ask_streaming("alpha-truncate-usage-model").await;
    assert_eq!(response.status(), 200);
    // Drain it so the stream really does end mid-answer rather than being dropped
    // by the client.
    let mut body = response;
    while let Ok(Some(chunk)) = body.chunk().await {
        assert!(!chunk.is_empty());
    }
    drop(body);

    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert!(
        !outcome.is_terminal_success(),
        "a truncated stream did not finish, so it is not a success"
    );
    // The spend is real: the upstream reported 11 prompt and 7 completion tokens
    // before it stopped.
    let total = outcome
        .actual_cost
        .expect("a stream that reported usage is charged for it");
    assert!(
        total > 0.0,
        "the ledger recorded a zero for a stream that reported usage: {total}"
    );

    // And it is attributed per attempt, which is what the cost axis reads.
    let attempt = outcome
        .attempts
        .last()
        .expect("a stream that started has an attempt");
    assert_eq!(attempt.candidate_model, "alpha-truncate-usage-model");
    let attributed = attempt
        .cost
        .expect("the truncated attempt carries the cost it incurred");
    assert!(
        attributed > 0.0,
        "the attempt is recorded at zero cost while the request total is {total}; \
         the attempt-level figure is what Targets::from_attempt reads"
    );
    assert!(
        (attributed - total).abs() < 1e-12,
        "one attempt incurred the whole spend, so the attempt figure ({attributed}) \
         and the request total ({total}) should agree"
    );

    // Scope stops at the outcome on purpose. Reaching a *model* additionally needs
    // the routing path's decision-time snapshot, because dataset ingestion for a
    // direct-model request has no candidate set to attach features to. That half is
    // covered end to end by `a_served_request_carries_its_price_into_the_durable_body`
    // in `ml_multi_provider_test.rs`, which drives real routed requests. Asserting
    // it here would be asserting that a fixture set up for streams also happens to
    // produce routed samples, which is a statement about this test rather than
    // about the mechanism.

    h.shutdown().await;
}

/// The same drop must not poison the provider: the upstream really did answer,
/// and a client leaving is not evidence that the model is unhealthy.
#[tokio::test]
async fn a_client_disconnect_does_not_count_against_provider_health() {
    let h = Harness::new().await;

    let mut response = h.ask_streaming("alpha-hold-model").await;
    let _ = response.chunk().await;
    drop(response);
    wait_for_outcomes(&h, 1).await;

    let health = h.state.router().health_snapshot();
    let alpha = health
        .iter()
        .find(|model| model.model_id == "alpha-hold-model")
        .expect("the handshake was reported");
    assert_eq!(alpha.total_success, 1, "one handshake, reported once");
    assert_eq!(alpha.total_failure, 0, "a client leaving is not a failure");
    assert!(!h.state.router().is_cooling("alpha-hold-model"));
    // The observation store only exists when the `ml` feature is compiled in, so
    // this assertion is feature-gated rather than gating the whole lifecycle
    // suite: the drop behaviour itself must still be covered by the default
    // build, which is the configuration the desktop app ships.
    #[cfg(feature = "ml")]
    {
        let observation = h
            .state
            .router()
            .observations()
            .get("alpha-hold-model", "alpha");
        assert_eq!(observation.health.total_failures, 0);
        assert_eq!(observation.health.total_requests, 1);
    }
    // The terminal state is still recorded as a classified fact, once.
    let breakdown = h
        .state
        .router()
        .stats_store
        .get("alpha-hold-model", "alpha");
    assert_eq!(breakdown.failures.count(FailureClass::Interrupted), 1);

    h.shutdown().await;
}

/// An explicit cancellation is its own terminal state, and it is not a success.
///
/// The Responses API cancel endpoint signals the in-flight stream, which used
/// to finalize the request with no error at all. Here the client cancels a
/// stream that is still being written, and the record has to say the request was
/// cancelled rather than completed.
#[tokio::test]
async fn an_explicit_cancellation_is_never_recorded_as_success() {
    let h = Harness::new().await;

    let mut response = h
        .post("/v1/responses")
        .json(&json!({"model": "alpha-cancel-model", "input": "hi", "stream": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let first = response.chunk().await.unwrap().expect("some output");
    let wire = String::from_utf8_lossy(&first).to_string();
    let response_id = wire
        .split("\"id\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("the first frame names the response")
        .to_string();
    assert!(
        response_id.starts_with("resp-"),
        "the client learns the in-flight id: {response_id}"
    );

    let cancelled = h
        .post(&format!("/v1/responses/{response_id}/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(cancelled.status(), 200);
    let _ = cancelled.text().await;

    wait_for_outcomes(&h, 1).await;

    let record = h.record();
    assert!(!record.ok, "a cancelled request is not a success");
    assert_eq!(record.status, 499);
    let error = record.error.as_deref().unwrap_or_default();
    assert!(error.contains("cancelled"), "activity says why: {error}");

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Cancelled);
    assert_eq!(outcome.failure_class(), Some(FailureClass::ClientCancelled));
    assert!(outcome.served_identity().is_none());
    assert_eq!(outcome.response_id.as_deref(), Some(response_id.as_str()));
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());
    assert_eq!(
        h.state.outcomes().len(),
        1,
        "the cancelled request is recorded once"
    );

    // A cancellation is the client's doing, so provider health is untouched.
    let health = h.state.router().health_snapshot();
    let alpha = health
        .iter()
        .find(|model| model.model_id == "alpha-cancel-model")
        .expect("the handshake was reported");
    assert_eq!(alpha.total_success, 1);
    assert_eq!(alpha.total_failure, 0);

    h.shutdown().await;
}
