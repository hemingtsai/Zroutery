//! Stage 5 Outcome & Feedback integration tests.
//!
//! These tests verify the full chain: construction, classification, serialization,
//! feedback signals, training sample conversion, and failure attribution.

use serde_json::json;

use zroutery_core::failure::FailureClass;
use zroutery_core::feedback::{DataOrigin, Feedback, FeedbackSignal, FeedbackSource, OutcomeSummary, TrainingSample};
use zroutery_core::ir::Usage;
use zroutery_core::outcome::{Attempt, FinalStatus, Outcome};
use zroutery_core::policy::{
    CandidateDecision, DecisionReason, PolicyRevision, RouteDecision, TaskProfileSummary,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
        ttft_ms: if success { Some(latency_ms * 0.3) } else { None },
        success,
        failure_class,
        failure_message: if success {
            None
        } else {
            Some(format!("{} failure", failure_class.map(|c| format!("{:?}", c)).unwrap_or_default()))
        },
        http_status: if success { Some(200) } else { Some(500) },
        rectified: false,
    }
}

fn make_route_decision(decision_id: &str, selected_model: &str) -> RouteDecision {
    RouteDecision {
        decision_id: decision_id.to_string(),
        timestamp: 1_700_000_000,
        task: TaskProfileSummary {
            complexity: "medium".to_string(),
            task_type: "chat".to_string(),
            context_tokens: 1000,
            estimated_output_tokens: 200,
            streaming: false,
            has_tools: false,
            has_vision: false,
            required_capabilities: vec![],
        },
        policy_id: "default".to_string(),
        client_id: None,
        candidates: vec![CandidateDecision {
            model_id: selected_model.to_string(),
            provider_id: "openai".to_string(),
            tier: Some("primary".to_string()),
            eligible: true,
            rejection: None,
            score: None,
            final_score: Some(0.95),
        }],
        selected: Some(selected_model.to_string()),
        fallback_chain: vec![],
        reason: DecisionReason::PolicySelected,
        policy_revision: PolicyRevision {
            policy_id: "default".to_string(),
            policy_enabled: true,
            requirements_hash: 12345,
            preference_hash: 67890,
        },
    }
}

fn outcome_to_summary(outcome: &Outcome) -> OutcomeSummary {
    OutcomeSummary {
        success: outcome.success,
        final_status: format!("{:?}", outcome.final_status).to_lowercase(),
        initial_model: outcome.initial_model.clone(),
        final_model: outcome.final_model.clone(),
        fallback_count: outcome.fallback_count,
        total_latency_ms: outcome.total_latency_ms,
        ttft_ms: outcome.ttft_ms,
        input_tokens: outcome.usage.as_ref().map(|u| u.input_tokens).unwrap_or(0),
        output_tokens: outcome.usage.as_ref().map(|u| u.output_tokens).unwrap_or(0),
        failure_class: outcome.attempts.last()
            .and_then(|a| a.failure_class)
            .map(|c| format!("{:?}", c).to_lowercase()),
    }
}

// ===========================================================================
// 1. Success outcome
// ===========================================================================

#[test]
fn success_outcome() {
    let outcome = Outcome::builder("req_success")
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
        .cost(Some(0.01), Some(0.009))
        .build();

    assert!(outcome.success);
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert_eq!(outcome.attempts.len(), 1);
    assert_eq!(outcome.fallback_count, 0);
    assert_eq!(outcome.initial_model, "gpt-4");
    assert_eq!(outcome.final_model, "gpt-4");
    assert_eq!(outcome.initial_provider, "openai");
    assert_eq!(outcome.final_provider, "openai");
    assert_eq!(outcome.total_latency_ms, 350.0);
    assert_eq!(outcome.ttft_ms, Some(105.0));
    assert!(outcome.usage.is_some());
    assert_eq!(outcome.usage.as_ref().unwrap().input_tokens, 100);
    assert_eq!(outcome.usage.as_ref().unwrap().output_tokens, 50);
    assert_eq!(outcome.estimated_cost, Some(0.01));
    assert_eq!(outcome.actual_cost, Some(0.009));
    assert!(outcome.outcome_id.starts_with("out_"));
}

// ===========================================================================
// 2. Transport failure outcome
// ===========================================================================

#[test]
fn transport_failure_outcome() {
    let outcome = Outcome::builder("req_transport")
        .single_candidate("gpt-4", "openai")
        .dialect("openai")
        .streaming(false)
        .attempt(make_attempt(
            "gpt-4",
            "openai",
            false,
            120.0,
            Some(FailureClass::Transport),
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
        Some(FailureClass::Transport)
    );

    // Transport failure is provider-attributable.
    let impact = FailureClass::Transport.impact();
    assert!(impact.provider_fault);
    assert!(impact.retryable);
    assert!(impact.fallbackable);
    assert!(impact.affects_observation);
    assert!(impact.affects_circuit);
}

// ===========================================================================
// 3. Timeout outcome
// ===========================================================================

#[test]
fn timeout_outcome() {
    let outcome = Outcome::builder("req_timeout")
        .single_candidate("claude-3", "anthropic")
        .dialect("anthropic")
        .streaming(true)
        .attempt(make_attempt(
            "claude-3",
            "anthropic",
            false,
            30_000.0,
            Some(FailureClass::Timeout),
        ))
        .total_latency_ms(30_000.0)
        .build();

    assert!(!outcome.success);
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::Timeout)
    );

    // Timeout is provider-attributable and retryable.
    let impact = FailureClass::Timeout.impact();
    assert!(impact.provider_fault);
    assert!(impact.retryable);
    assert!(impact.fallbackable);
}

// ===========================================================================
// 4. Rate limit outcome
// ===========================================================================

#[test]
fn rate_limit_outcome() {
    let outcome = Outcome::builder("req_ratelimit")
        .single_candidate("gpt-4", "openai")
        .dialect("openai")
        .streaming(false)
        .attempt(make_attempt(
            "gpt-4",
            "openai",
            false,
            50.0,
            Some(FailureClass::RateLimit),
        ))
        .total_latency_ms(50.0)
        .build();

    assert!(!outcome.success);
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::RateLimit)
    );

    // Rate limit is NOT provider fault (caller's fault) and should NOT open circuit.
    let impact = FailureClass::RateLimit.impact();
    assert!(!impact.provider_fault);
    assert!(!impact.affects_circuit);
    assert!(impact.retryable);
    assert!(impact.fallbackable);
}

// ===========================================================================
// 5. Single fallback outcome
// ===========================================================================

#[test]
fn single_fallback_outcome() {
    let outcome = Outcome::builder("req_fallback1")
        .initial("gpt-4", "openai")
        .final_candidate("claude-3", "anthropic")
        .dialect("openai")
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
    assert_eq!(outcome.initial_model, "gpt-4");
    assert_eq!(outcome.final_model, "claude-3");
    assert_eq!(outcome.fallback_count, 1);
    assert_eq!(outcome.attempts.len(), 2);
    assert!(!outcome.attempts[0].success);
    assert!(outcome.attempts[1].success);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::ProviderUnavailable)
    );
    assert!(outcome.attempts[1].failure_class.is_none());
}

// ===========================================================================
// 6. Multi-step fallback
// ===========================================================================

#[test]
fn multi_step_fallback() {
    let outcome = Outcome::builder("req_multifallback")
        .initial("gpt-4", "openai")
        .final_candidate("gemini-pro", "google")
        .dialect("openai")
        .streaming(false)
        // Attempt 1: OpenAI fails with transport error.
        .attempt(make_attempt(
            "gpt-4",
            "openai",
            false,
            100.0,
            Some(FailureClass::Transport),
        ))
        // Attempt 2: Anthropic times out.
        .attempt(make_attempt(
            "claude-3",
            "anthropic",
            false,
            30_000.0,
            Some(FailureClass::Timeout),
        ))
        // Attempt 3: Google succeeds.
        .attempt(make_attempt("gemini-pro", "google", true, 500.0, None))
        .total_latency_ms(30_600.0)
        .usage(Usage {
            input_tokens: 150,
            output_tokens: 80,
            ..Usage::default()
        })
        .build();

    assert!(outcome.success);
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert_eq!(outcome.attempts.len(), 3);
    assert_eq!(outcome.fallback_count, 2);
    assert_eq!(outcome.initial_model, "gpt-4");
    assert_eq!(outcome.final_model, "gemini-pro");
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::Transport)
    );
    assert_eq!(
        outcome.attempts[1].failure_class,
        Some(FailureClass::Timeout)
    );
    assert!(outcome.attempts[2].success);
}

// ===========================================================================
// 7. Decision <-> Outcome correlation
// ===========================================================================

#[test]
fn decision_outcome_correlation() {
    let decision = make_route_decision("dec_abc123", "gpt-4");
    assert_eq!(decision.selected, Some("gpt-4".to_string()));

    let outcome = Outcome::builder("req_corr")
        .decision_id(&decision.decision_id)
        .single_candidate("gpt-4", "openai")
        .dialect("openai")
        .attempt(make_attempt("gpt-4", "openai", true, 300.0, None))
        .total_latency_ms(300.0)
        .usage(Usage {
            input_tokens: 50,
            output_tokens: 20,
            ..Usage::default()
        })
        .build();

    // The outcome references the decision.
    assert_eq!(outcome.decision_id, Some("dec_abc123".to_string()));

    // The decision's selected model matches the outcome's final model.
    assert_eq!(decision.selected.as_deref(), Some(outcome.final_model.as_str()));

    // Both can be serialized and the correlation survives.
    let decision_json = serde_json::to_string(&decision).unwrap();
    let outcome_json = serde_json::to_string(&outcome).unwrap();
    let restored_decision: RouteDecision = serde_json::from_str(&decision_json).unwrap();
    let restored_outcome: Outcome = serde_json::from_str(&outcome_json).unwrap();
    assert_eq!(restored_decision.decision_id, restored_outcome.decision_id.unwrap());
}

// ===========================================================================
// 8. Outcome serialization round-trip
// ===========================================================================

#[test]
fn outcome_serde_round_trip() {
    let outcome = Outcome::builder("req_rt")
        .decision_id("dec_999")
        .response_id("resp_888")
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
            cache_read_tokens: 50,
            ..Usage::default()
        })
        .cost(Some(0.02), Some(0.018))
        .build();

    let json = serde_json::to_string(&outcome).expect("serialize");
    let restored: Outcome = serde_json::from_str(&json).expect("deserialize");

    // Identity fields.
    assert_eq!(restored.outcome_id, outcome.outcome_id);
    assert_eq!(restored.decision_id, Some("dec_999".to_string()));
    assert_eq!(restored.response_id, Some("resp_888".to_string()));
    assert_eq!(restored.request_id, "req_rt");

    // Result.
    assert!(restored.success);
    assert_eq!(restored.final_status, FinalStatus::Success);

    // Candidates.
    assert_eq!(restored.initial_model, "gpt-4");
    assert_eq!(restored.initial_provider, "openai");
    assert_eq!(restored.final_model, "claude-3");
    assert_eq!(restored.final_provider, "anthropic");

    // Attempts.
    assert_eq!(restored.attempts.len(), 2);
    assert!(!restored.attempts[0].success);
    assert!(restored.attempts[1].success);
    assert_eq!(restored.fallback_count, 1);

    // Timing.
    assert_eq!(restored.total_latency_ms, 600.0);
    assert_eq!(restored.ttft_ms, Some(120.0));

    // Usage.
    let usage = restored.usage.as_ref().unwrap();
    assert_eq!(usage.input_tokens, 200);
    assert_eq!(usage.output_tokens, 100);
    assert_eq!(usage.cache_read_tokens, 50);

    // Cost.
    assert_eq!(restored.estimated_cost, Some(0.02));
    assert_eq!(restored.actual_cost, Some(0.018));

    // Context.
    assert!(restored.streaming);
    assert_eq!(restored.dialect, "anthropic");
}

#[test]
fn outcome_final_status_serde_variants() {
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

// ===========================================================================
// 9. Feedback classification
// ===========================================================================

#[test]
fn feedback_signals() {
    // Build an outcome.
    let outcome = Outcome::builder("req_feedback")
        .single_candidate("gpt-4", "openai")
        .dialect("openai")
        .attempt(make_attempt("gpt-4", "openai", true, 300.0, None))
        .total_latency_ms(300.0)
        .usage(Usage {
            input_tokens: 100,
            output_tokens: 50,
            ..Usage::default()
        })
        .build();

    // Construct feedback signals.
    let feedback = Feedback {
        outcome_id: outcome.outcome_id.clone(),
        timestamp: 1_700_000_005,
        signals: vec![
            FeedbackSignal::ExplicitRating { score: 4.5 },
            FeedbackSignal::ConversationContinued,
        ],
        source: FeedbackSource::Client,
        data_origin: DataOrigin::Native,
    };

    assert_eq!(feedback.outcome_id, outcome.outcome_id);
    assert_eq!(feedback.signals.len(), 2);
    assert_eq!(feedback.source, FeedbackSource::Client);
    assert_eq!(feedback.data_origin, DataOrigin::Native);

    // Verify each signal variant.
    match &feedback.signals[0] {
        FeedbackSignal::ExplicitRating { score } => assert_eq!(*score, 4.5),
        _ => panic!("expected ExplicitRating"),
    }
    assert!(matches!(
        feedback.signals[1],
        FeedbackSignal::ConversationContinued
    ));

    // Round-trip the feedback.
    let json = serde_json::to_string(&feedback).unwrap();
    let restored: Feedback = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.outcome_id, outcome.outcome_id);
    assert_eq!(restored.signals.len(), 2);
}

#[test]
fn feedback_signal_variants_all_round_trip() {
    let signals = vec![
        FeedbackSignal::ExplicitRating { score: 3.0 },
        FeedbackSignal::RetryRequested,
        FeedbackSignal::ConversationContinued,
        FeedbackSignal::ConversationAbandoned,
        FeedbackSignal::ToolExecution { success: true },
        FeedbackSignal::ToolExecution { success: false },
        FeedbackSignal::Incomplete,
    ];

    for signal in &signals {
        let json = serde_json::to_string(signal).unwrap();
        let restored: FeedbackSignal = serde_json::from_str(&json).unwrap();
        // Verify the tag is present.
        assert!(json.contains("\"type\""));
        // Re-serialize and compare.
        let json2 = serde_json::to_string(&restored).unwrap();
        assert_eq!(json, json2);
    }
}

// ===========================================================================
// 10. Training sample conversion
// ===========================================================================

#[test]
fn training_sample_from_outcome() {
    let outcome = Outcome::builder("req_train")
        .decision_id("dec_train")
        .single_candidate("gpt-4", "openai")
        .dialect("openai")
        .streaming(false)
        .attempt(make_attempt("gpt-4", "openai", true, 350.0, None))
        .total_latency_ms(350.0)
        .usage(Usage {
            input_tokens: 100,
            output_tokens: 50,
            ..Usage::default()
        })
        .build();

    // Convert outcome to summary (the bridge to training data).
    let summary = outcome_to_summary(&outcome);
    assert!(summary.success);
    assert_eq!(summary.initial_model, "gpt-4");
    assert_eq!(summary.final_model, "gpt-4");
    assert_eq!(summary.fallback_count, 0);
    assert_eq!(summary.input_tokens, 100);
    assert_eq!(summary.output_tokens, 50);
    assert!(summary.failure_class.is_none());

    // Build a training sample.
    let sample = TrainingSample {
        sample_id: format!("samp_{}", uuid::Uuid::new_v4().simple()),
        outcome_id: outcome.outcome_id.clone(),
        decision_id: outcome.decision_id.clone(),
        timestamp: outcome.timestamp,
        data_origin: DataOrigin::Native,
        outcome_summary: summary,
        feedback: vec![
            FeedbackSignal::ExplicitRating { score: 4.0 },
            FeedbackSignal::ConversationContinued,
        ],
        features: Some(json!({
            "latency_bucket": "fast",
            "has_tools": false,
        })),
    };

    assert_eq!(sample.outcome_id, outcome.outcome_id);
    assert_eq!(sample.decision_id, Some("dec_train".to_string()));
    assert!(sample.outcome_summary.success);
    assert_eq!(sample.feedback.len(), 2);
    assert!(sample.features.is_some());

    // Round-trip.
    let json = serde_json::to_string(&sample).unwrap();
    let restored: TrainingSample = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.sample_id, sample.sample_id);
    assert_eq!(restored.outcome_id, outcome.outcome_id);
    assert!(restored.outcome_summary.success);
    assert_eq!(restored.outcome_summary.input_tokens, 100);
    assert_eq!(restored.data_origin, DataOrigin::Native);
}

#[test]
fn training_sample_from_failed_outcome_with_fallback() {
    let outcome = Outcome::builder("req_train_fail")
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
        .attempt(make_attempt(
            "claude-3",
            "anthropic",
            false,
            150.0,
            Some(FailureClass::Timeout),
        ))
        .total_latency_ms(350.0)
        .build();

    let summary = outcome_to_summary(&outcome);
    assert!(!summary.success);
    assert_eq!(summary.initial_model, "gpt-4");
    assert_eq!(summary.final_model, "claude-3");
    assert_eq!(summary.fallback_count, 1);
    assert_eq!(summary.output_tokens, 0);
    // Last attempt failure class.
    assert_eq!(summary.failure_class, Some("timeout".to_string()));

    let sample = TrainingSample {
        sample_id: "samp_fail".to_string(),
        outcome_id: outcome.outcome_id.clone(),
        decision_id: None,
        timestamp: outcome.timestamp,
        data_origin: DataOrigin::Native,
        outcome_summary: summary,
        feedback: vec![FeedbackSignal::RetryRequested],
        features: None,
    };

    assert!(!sample.outcome_summary.success);
    assert_eq!(sample.outcome_summary.fallback_count, 1);
    assert_eq!(sample.feedback.len(), 1);

    // Round-trip.
    let json = serde_json::to_string(&sample).unwrap();
    let restored: TrainingSample = serde_json::from_str(&json).unwrap();
    assert!(!restored.outcome_summary.success);
    assert_eq!(restored.outcome_summary.failure_class, Some("timeout".to_string()));
}

// ===========================================================================
// 11. Client cancellation is not provider failure
// ===========================================================================

#[test]
fn client_cancelled_not_provider_fault() {
    // When the client cancels, the final status should be Cancelled, not Failed.
    let outcome = Outcome::builder("req_cancel")
        .single_candidate("gpt-4", "openai")
        .dialect("openai")
        .streaming(true)
        .attempt(make_attempt(
            "gpt-4",
            "openai",
            false,
            500.0,
            Some(FailureClass::ClientCancelled),
        ))
        .cancelled()
        .total_latency_ms(500.0)
        .build();

    assert!(!outcome.success);
    assert_eq!(outcome.final_status, FinalStatus::Cancelled);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::ClientCancelled)
    );

    // ClientCancelled has zero provider impact.
    let impact = FailureClass::ClientCancelled.impact();
    assert!(!impact.provider_fault);
    assert!(!impact.affects_observation);
    assert!(!impact.affects_circuit);
    assert!(!impact.retryable);
    assert!(!impact.fallbackable);

    // classify_final also returns Cancelled when the last attempt is ClientCancelled
    // even without the explicit cancelled() flag.
    let attempts = vec![make_attempt(
        "gpt-4",
        "openai",
        false,
        500.0,
        Some(FailureClass::ClientCancelled),
    )];
    assert_eq!(
        Outcome::classify_final(&attempts, false),
        FinalStatus::Cancelled
    );
}

// ===========================================================================
// 12. Invalid request is not provider failure
// ===========================================================================

#[test]
fn invalid_request_not_provider_fault() {
    let outcome = Outcome::builder("req_invalid")
        .single_candidate("gpt-4", "openai")
        .dialect("openai")
        .streaming(false)
        .attempt(make_attempt(
            "gpt-4",
            "openai",
            false,
            30.0,
            Some(FailureClass::InvalidRequest),
        ))
        .total_latency_ms(30.0)
        .build();

    assert!(!outcome.success);
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::InvalidRequest)
    );

    // InvalidRequest has zero provider impact.
    let impact = FailureClass::InvalidRequest.impact();
    assert!(!impact.provider_fault);
    assert!(!impact.affects_observation);
    assert!(!impact.affects_circuit);
    assert!(!impact.retryable);
    assert!(!impact.fallbackable);

    // Should NOT trigger fallback.
    assert!(!impact.fallbackable, "same request will fail on every provider");
}
