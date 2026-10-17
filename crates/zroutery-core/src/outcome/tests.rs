//! The outcome model fixtures.

use crate::failure::FailureClass;
use crate::ir::Usage;
use crate::observation::ObservationStore;
use crate::stats_ext::StatsStore;

use super::*;

fn make_attempt(
    model: &str,
    provider: &str,
    success: bool,
    latency_ms: f64,
    failure_class: Option<FailureClass>,
) -> Attempt {
    Attempt {
        attempt_id: format!("att_{}", uuid::Uuid::new_v4().simple()),
        candidate_model: model.to_string(),
        candidate_provider: provider.to_string(),
        started_at: 1_700_000_000,
        completed_at: 1_700_000_001,
        latency_ms,
        ttft_ms: if success {
            Some(latency_ms * 0.3)
        } else {
            None
        },
        success,
        failure_class,
        failure_message: if success {
            None
        } else {
            Some("test failure".to_string())
        },
        http_status: if success { Some(200) } else { Some(500) },
        rectified: false,
        cost: None,
    }
}

// -- Success outcome construction --

#[test]
fn success_outcome_single_attempt() {
    let outcome = Outcome::builder("req_1")
        .single_candidate("gpt-4", "openai")
        .dialect("openai")
        .streaming(true)
        .attempt(make_attempt("gpt-4", "openai", true, 350.0, None))
        .total_latency_ms(350.0)
        .ttft_ms(105.0)
        .usage(Usage {
            input_tokens: 100,
            output_tokens: 50,
            ..Usage::default()
        })
        .build();

    assert!(outcome.success);
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert_eq!(outcome.attempts.len(), 1);
    assert_eq!(outcome.fallback_count, 0);
    assert_eq!(outcome.initial_model, "gpt-4");
    assert_eq!(outcome.final_model, "gpt-4");
    assert_eq!(outcome.total_latency_ms, 350.0);
    assert_eq!(outcome.ttft_ms, Some(105.0));
    assert!(outcome.usage.is_some());
    assert_eq!(outcome.usage.as_ref().unwrap().output_tokens, 50);
    assert!(outcome.outcome_id.starts_with("out_"));
}

// -- Failure outcome with single attempt --

#[test]
fn failure_outcome_single_attempt() {
    let outcome = Outcome::builder("req_2")
        .single_candidate("gpt-4", "openai")
        .dialect("openai")
        .streaming(false)
        .attempt(make_attempt(
            "gpt-4",
            "openai",
            false,
            120.0,
            Some(FailureClass::RateLimit),
        ))
        .total_latency_ms(120.0)
        .build();

    assert!(!outcome.success);
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(outcome.attempts.len(), 1);
    assert_eq!(outcome.fallback_count, 0);
    assert!(outcome.usage.is_none());
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::RateLimit)
    );
}

// -- Outcome with multiple attempts (fallback) --

#[test]
fn fallback_outcome_two_attempts() {
    let outcome = Outcome::builder("req_3")
        .initial("gpt-4", "openai")
        .final_candidate("claude-3", "anthropic")
        .dialect("anthropic")
        .streaming(true)
        .attempt(make_attempt(
            "gpt-4",
            "openai",
            false,
            200.0,
            Some(FailureClass::ProviderUnavailable),
        ))
        .attempt(make_attempt("claude-3", "anthropic", true, 400.0, None))
        .total_latency_ms(600.0)
        .ttft_ms(120.0)
        .usage(Usage {
            input_tokens: 200,
            output_tokens: 100,
            ..Usage::default()
        })
        .build();

    assert!(outcome.success);
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert_eq!(outcome.attempts.len(), 2);
    assert_eq!(outcome.fallback_count, 1);
    assert_eq!(outcome.initial_model, "gpt-4");
    assert_eq!(outcome.final_model, "claude-3");
    assert_eq!(outcome.total_latency_ms, 600.0);
}

#[test]
fn fallback_all_fail() {
    let outcome = Outcome::builder("req_4")
        .initial("gpt-4", "openai")
        .final_candidate("claude-3", "anthropic")
        .dialect("openai")
        .streaming(false)
        .attempt(make_attempt(
            "gpt-4",
            "openai",
            false,
            100.0,
            Some(FailureClass::Transport),
        ))
        .attempt(make_attempt(
            "claude-3",
            "anthropic",
            false,
            150.0,
            Some(FailureClass::Timeout),
        ))
        .total_latency_ms(250.0)
        .build();

    assert!(!outcome.success);
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(outcome.fallback_count, 1);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::Transport)
    );
    assert_eq!(
        outcome.attempts[1].failure_class,
        Some(FailureClass::Timeout)
    );
}

// -- FinalStatus classification --

#[test]
fn classify_final_success() {
    let attempts = vec![
        make_attempt("m", "p", false, 100.0, Some(FailureClass::Timeout)),
        make_attempt("m2", "p2", true, 200.0, None),
    ];
    assert_eq!(
        Outcome::classify_final(&attempts, false),
        FinalStatus::Success
    );
}

#[test]
fn classify_final_failed() {
    let attempts = vec![make_attempt(
        "m",
        "p",
        false,
        100.0,
        Some(FailureClass::Transport),
    )];
    assert_eq!(
        Outcome::classify_final(&attempts, false),
        FinalStatus::Failed
    );
}

#[test]
fn classify_final_cancelled_flag() {
    let attempts = vec![make_attempt(
        "m",
        "p",
        false,
        100.0,
        Some(FailureClass::Timeout),
    )];
    assert_eq!(
        Outcome::classify_final(&attempts, true),
        FinalStatus::Cancelled
    );
}

#[test]
fn classify_final_client_cancelled_in_attempt() {
    let attempts = vec![make_attempt(
        "m",
        "p",
        false,
        50.0,
        Some(FailureClass::ClientCancelled),
    )];
    assert_eq!(
        Outcome::classify_final(&attempts, false),
        FinalStatus::Cancelled
    );
}

#[test]
fn classify_final_empty_attempts() {
    assert_eq!(Outcome::classify_final(&[], false), FinalStatus::Failed);
}

// -- Outcome serde round-trip --

#[test]
fn serde_round_trip() {
    let outcome = Outcome::builder("req_rt")
        .decision_id("dec_123")
        .response_id("resp_456")
        .single_candidate("gpt-4", "openai")
        .dialect("openai")
        .streaming(true)
        .attempt(make_attempt("gpt-4", "openai", true, 300.0, None))
        .total_latency_ms(300.0)
        .ttft_ms(90.0)
        .usage(Usage {
            input_tokens: 50,
            output_tokens: 30,
            ..Usage::default()
        })
        .cost(Some(0.01), Some(0.009))
        .build();

    let json = serde_json::to_string(&outcome).expect("serialize");
    let restored: Outcome = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(restored.outcome_id, outcome.outcome_id);
    assert_eq!(restored.decision_id, Some("dec_123".to_string()));
    assert_eq!(restored.response_id, Some("resp_456".to_string()));
    assert_eq!(restored.request_id, "req_rt");
    assert!(restored.success);
    assert_eq!(restored.final_status, FinalStatus::Success);
    assert_eq!(restored.attempts.len(), 1);
    assert_eq!(restored.dialect, "openai");
    assert!(restored.streaming);
    assert_eq!(restored.estimated_cost, Some(0.01));
    assert_eq!(restored.actual_cost, Some(0.009));
}

#[test]
fn final_status_serde_variants() {
    let variants = [
        FinalStatus::Success,
        FinalStatus::Failed,
        FinalStatus::Cancelled,
        FinalStatus::Interrupted,
    ];
    for v in &variants {
        let json = serde_json::to_string(v).unwrap();
        let restored: FinalStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(*v, restored);
    }
    // Check wire format.
    assert_eq!(
        serde_json::to_string(&FinalStatus::Success).unwrap(),
        "\"success\""
    );
    assert_eq!(
        serde_json::to_string(&FinalStatus::Failed).unwrap(),
        "\"failed\""
    );
    assert_eq!(
        serde_json::to_string(&FinalStatus::Cancelled).unwrap(),
        "\"cancelled\""
    );
    assert_eq!(
        serde_json::to_string(&FinalStatus::Interrupted).unwrap(),
        "\"interrupted\""
    );
}

// -- record_to_observation writes to both stores --

#[test]
fn record_to_observation_writes_success() {
    let obs = ObservationStore::new();
    let stats = StatsStore::new();

    let outcome = Outcome::builder("req_obs1")
        .single_candidate("gpt-4", "openai")
        .dialect("openai")
        .attempt(make_attempt("gpt-4", "openai", true, 300.0, None))
        .total_latency_ms(300.0)
        .build();

    outcome.record_to_observation(&obs, &stats);

    let obs_entry = obs.get("gpt-4", "openai");
    assert_eq!(obs_entry.health.total_requests, 1);
    assert_eq!(obs_entry.health.total_failures, 0);
    assert!(obs_entry.latency.total_ms.is_known());

    let stats_entry = stats.get("gpt-4", "openai");
    assert_eq!(stats_entry.total_requests, 1);
    assert_eq!(stats_entry.total_successes, 1);
    assert_eq!(stats_entry.total_failures, 0);
}

#[test]
fn record_to_observation_writes_failure() {
    let obs = ObservationStore::new();
    let stats = StatsStore::new();

    let outcome = Outcome::builder("req_obs2")
        .single_candidate("gpt-4", "openai")
        .dialect("openai")
        .attempt(make_attempt(
            "gpt-4",
            "openai",
            false,
            100.0,
            Some(FailureClass::RateLimit),
        ))
        .total_latency_ms(100.0)
        .build();

    outcome.record_to_observation(&obs, &stats);

    let obs_entry = obs.get("gpt-4", "openai");
    assert_eq!(obs_entry.health.total_requests, 1);
    assert_eq!(obs_entry.health.total_failures, 1);

    let stats_entry = stats.get("gpt-4", "openai");
    assert_eq!(stats_entry.total_requests, 1);
    assert_eq!(stats_entry.total_successes, 0);
    assert_eq!(stats_entry.total_failures, 1);
    assert_eq!(stats_entry.failures.count(FailureClass::RateLimit), 1);
}

#[test]
fn record_to_observation_writes_fallback_chain() {
    let obs = ObservationStore::new();
    let stats = StatsStore::new();

    let outcome = Outcome::builder("req_obs3")
        .initial("gpt-4", "openai")
        .final_candidate("claude-3", "anthropic")
        .dialect("openai")
        .attempt(make_attempt(
            "gpt-4",
            "openai",
            false,
            200.0,
            Some(FailureClass::ProviderUnavailable),
        ))
        .attempt(make_attempt("claude-3", "anthropic", true, 400.0, None))
        .total_latency_ms(600.0)
        .build();

    outcome.record_to_observation(&obs, &stats);

    // OpenAI: 1 failure
    let obs_openai = obs.get("gpt-4", "openai");
    assert_eq!(obs_openai.health.total_failures, 1);
    let stats_openai = stats.get("gpt-4", "openai");
    assert_eq!(stats_openai.total_failures, 1);

    // Anthropic: 1 success
    let obs_anthropic = obs.get("claude-3", "anthropic");
    assert_eq!(obs_anthropic.health.total_requests, 1);
    assert_eq!(obs_anthropic.health.total_failures, 0);
    let stats_anthropic = stats.get("claude-3", "anthropic");
    assert_eq!(stats_anthropic.total_successes, 1);
    assert!(stats_anthropic.total_latency.sample_count() > 0);
}

// -- Edge cases --

#[test]
fn builder_sets_timestamp() {
    let before = chrono::Utc::now().timestamp();
    let outcome = Outcome::builder("req_ts")
        .single_candidate("m", "p")
        .dialect("openai")
        .build();
    let after = chrono::Utc::now().timestamp();
    assert!(outcome.timestamp >= before && outcome.timestamp <= after);
}

#[test]
fn explicit_timestamp_override() {
    let outcome = Outcome::builder("req_ts2")
        .single_candidate("m", "p")
        .dialect("openai")
        .timestamp(1_700_000_000)
        .build();
    assert_eq!(outcome.timestamp, 1_700_000_000);
}

#[test]
fn cancelled_builder_overrides_status() {
    let outcome = Outcome::builder("req_c")
        .single_candidate("m", "p")
        .dialect("openai")
        .cancelled()
        .build();
    assert!(!outcome.success);
    assert_eq!(outcome.final_status, FinalStatus::Cancelled);
}

#[test]
fn explicit_fallback_count_is_preserved() {
    let outcome = Outcome::builder("req_fc")
        .initial("m1", "p1")
        .final_candidate("m2", "p2")
        .dialect("openai")
        .attempt(make_attempt(
            "m1",
            "p1",
            false,
            100.0,
            Some(FailureClass::Timeout),
        ))
        .attempt(make_attempt("m2", "p2", true, 200.0, None))
        .fallback_count(5) // deliberately wrong, but explicit
        .build();
    assert_eq!(
        outcome.fallback_count, 5,
        "explicit fallback_count must be preserved"
    );
}

#[test]
fn attempt_fields_round_trip() {
    let a = Attempt {
        attempt_id: "att_test".to_string(),
        candidate_model: "m".to_string(),
        candidate_provider: "p".to_string(),
        started_at: 100,
        completed_at: 200,
        latency_ms: 100.0,
        ttft_ms: Some(30.0),
        success: true,
        failure_class: None,
        failure_message: None,
        http_status: Some(200),
        rectified: true,
        cost: None,
    };
    let json = serde_json::to_string(&a).unwrap();
    let restored: Attempt = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.attempt_id, "att_test");
    assert!(restored.rectified);
    assert_eq!(restored.http_status, Some(200));
}

#[test]
fn outcome_ids_are_unique() {
    let o1 = Outcome::builder("r1")
        .single_candidate("m", "p")
        .dialect("openai")
        .build();
    let o2 = Outcome::builder("r2")
        .single_candidate("m", "p")
        .dialect("openai")
        .build();
    assert_ne!(o1.outcome_id, o2.outcome_id);
}
