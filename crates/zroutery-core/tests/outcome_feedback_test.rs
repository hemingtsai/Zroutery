#![cfg(feature = "ml")]

//! Focused Core Outcome/Feedback/schema-bridge contract tests.

use zroutery_core::failure::FailureClass;
use zroutery_core::feedback::{
    feedback_from_outcome, DataOrigin, Feedback, FeedbackSignal, FeedbackSource, OutcomeSummary,
};
use zroutery_core::ir::Usage;
use zroutery_core::ml::dataset::{
    canonical_samples_from_outcome, try_samples_from_outcome_with_feedback, DatasetStore,
    SampleBuilder, SampleScope,
};
use zroutery_core::outcome::{Attempt, FinalStatus, Outcome};

fn attempt(
    id: &str,
    model: &str,
    provider: &str,
    success: bool,
    latency_ms: f64,
    failure_class: Option<FailureClass>,
) -> Attempt {
    Attempt {
        attempt_id: id.to_string(),
        candidate_model: model.to_string(),
        candidate_provider: provider.to_string(),
        started_at: 1_700_000_000,
        completed_at: 1_700_000_001,
        latency_ms,
        ttft_ms: success.then_some(latency_ms * 0.25),
        success,
        failure_class,
        failure_message: (!success).then(|| "captured failure".to_string()),
        http_status: success.then_some(200).or(Some(503)),
        rectified: false,
    }
}

fn fallback_success() -> Outcome {
    Outcome::builder("req-bridge")
        .planned("model-a", "provider-a")
        .final_candidate("model-b", "provider-b")
        .dialect("openai")
        .streaming(true)
        .attempt(attempt(
            "att-a",
            "model-a",
            "provider-a",
            false,
            100.0,
            Some(FailureClass::ProviderUnavailable),
        ))
        .attempt(attempt("att-b", "model-b", "provider-b", true, 250.0, None))
        .total_latency_ms(350.0)
        .ttft_ms(60.0)
        .usage(Usage {
            input_tokens: 20,
            output_tokens: 10,
            ..Usage::default()
        })
        .cost(Some(0.02), Some(0.01))
        .build()
}

#[test]
fn identities_keep_planned_last_attempted_and_served_distinct() {
    let outcome = fallback_success();

    assert_eq!(
        outcome.planned_identity(),
        Some(zroutery_core::CandidateIdentity::new(
            "model-a",
            "provider-a"
        ))
    );
    assert_eq!(
        outcome.last_attempted_identity(),
        Some(zroutery_core::CandidateIdentity::new(
            "model-b",
            "provider-b"
        ))
    );
    assert_eq!(outcome.served_identity(), outcome.final_served_identity());
    assert_eq!(
        outcome.served_identity(),
        Some(zroutery_core::CandidateIdentity::new(
            "model-b",
            "provider-b"
        ))
    );
    assert_eq!(outcome.identity().planned.unwrap().model, "model-a");
    assert_eq!(outcome.identity().last_attempted.unwrap().model, "model-b");
    assert_eq!(outcome.identity().served.unwrap().model, "model-b");
    assert!(outcome.validate().is_ok());
}

#[test]
fn planned_only_outcome_has_no_last_attempt_or_served_identity() {
    let outcome = Outcome::builder("req-planned-only")
        .planned("model-only", "provider-only")
        .dialect("openai")
        .build();

    assert!(outcome.planned_identity().is_some());
    assert!(outcome.last_attempted_identity().is_none());
    assert!(outcome.served_identity().is_none());
    assert!(!outcome.success);
    assert!(outcome.validate().is_ok());
}

#[test]
fn failure_cancellation_and_interruption_never_become_success() {
    let failed = Outcome::builder("req-failed")
        .single_candidate("model-a", "provider-a")
        .dialect("openai")
        .attempt(attempt(
            "att-failed",
            "model-a",
            "provider-a",
            false,
            40.0,
            Some(FailureClass::Capability),
        ))
        .usage(Usage {
            input_tokens: 5,
            output_tokens: 2,
            ..Usage::default()
        })
        .cost(Some(0.1), Some(0.1))
        .build();
    assert!(!failed.success);
    assert_eq!(failed.final_status, FinalStatus::Failed);
    assert!(failed.served_identity().is_none());
    assert!(failed.validate().is_ok());
    let failed_summary = OutcomeSummary::try_from_outcome(&failed).expect("failure summary");
    assert!(!failed_summary.success);
    assert!(failed_summary.final_model.is_empty());
    assert_eq!(failed_summary.failure_class.as_deref(), Some("capability"));

    let cancelled = Outcome::builder("req-cancelled")
        .single_candidate("model-a", "provider-a")
        .attempt(attempt(
            "att-cancelled",
            "model-a",
            "provider-a",
            false,
            50.0,
            Some(FailureClass::ClientCancelled),
        ))
        .usage(Usage {
            input_tokens: 7,
            output_tokens: 1,
            ..Usage::default()
        })
        .cancelled()
        .build();
    assert!(!cancelled.success);
    assert_eq!(cancelled.final_status, FinalStatus::Cancelled);
    assert!(cancelled.served_identity().is_none());
    assert_eq!(
        cancelled
            .classified_terminal_failure()
            .map(|failure| failure.class),
        Some(FailureClass::ClientCancelled)
    );

    let interrupted = Outcome::builder("req-interrupted")
        .single_candidate("model-a", "provider-a")
        .attempt(attempt(
            "att-interrupted",
            "model-a",
            "provider-a",
            false,
            50.0,
            Some(FailureClass::Interrupted),
        ))
        .interrupted()
        .build();
    assert!(!interrupted.success);
    assert_eq!(interrupted.final_status, FinalStatus::Interrupted);
    assert!(interrupted.served_identity().is_none());
    assert!(interrupted.validate().is_ok());

    let cancelled_after_usage = Outcome::builder("req-cancel-after-usage")
        .single_candidate("model-a", "provider-a")
        .attempt(attempt(
            "att-cancel-after-usage",
            "model-a",
            "provider-a",
            true,
            50.0,
            None,
        ))
        .usage(Usage {
            input_tokens: 9,
            output_tokens: 3,
            ..Usage::default()
        })
        .cancelled()
        .build();
    assert!(!cancelled_after_usage.success);
    assert_eq!(cancelled_after_usage.final_status, FinalStatus::Cancelled);
    assert!(cancelled_after_usage.served_identity().is_none());
    assert!(cancelled_after_usage.validate().is_ok());
}

#[test]
fn local_and_capability_failures_remain_terminal_failure_evidence() {
    for class in [
        FailureClass::Capability,
        FailureClass::MissingApiKey,
        FailureClass::OverBudget,
        FailureClass::NoCandidate,
    ] {
        let outcome = Outcome::builder(format!("req-{class:?}"))
            .failure(class, "captured terminal error", None)
            .dialect("openai")
            .build();
        assert!(!outcome.success, "{class:?} must not be success");
        assert_eq!(outcome.final_status, FinalStatus::Failed);
        assert_eq!(
            outcome.terminal_failure_facts().map(|facts| facts.class),
            Some(class)
        );
        assert_eq!(
            outcome.terminal_error.as_ref().map(|facts| facts.class),
            Some(class)
        );
        assert!(outcome.served_identity().is_none());
        assert!(outcome.validate().is_ok(), "{class:?} should validate");
    }
}

#[test]
fn malformed_outcomes_fail_closed_at_validation_and_conversion() {
    let mut malformed = fallback_success();
    malformed.success = false;
    assert!(malformed.validate().is_err());

    let mut missing_class = fallback_success();
    missing_class.attempts[0].failure_class = None;
    assert!(missing_class.validate().is_err());

    let mut false_served = Outcome::builder("req-false-served")
        .single_candidate("model-a", "provider-a")
        .attempt(attempt(
            "att-false-served",
            "model-a",
            "provider-a",
            false,
            10.0,
            Some(FailureClass::Timeout),
        ))
        .build();
    false_served.served_model = Some("model-a".to_string());
    false_served.served_provider = Some("provider-a".to_string());
    assert!(false_served.validate().is_err());
    assert!(SampleBuilder::try_build(&malformed, Default::default(), DataOrigin::Native,).is_err());
    assert!(canonical_samples_from_outcome(&malformed, &[], DataOrigin::Native).is_err());
}

#[test]
fn feedback_absence_is_none_and_never_fabricates_a_rating() {
    let outcome = fallback_success();
    assert!(Feedback::try_from_outcome(
        &outcome,
        Vec::new(),
        outcome.timestamp,
        FeedbackSource::Client,
        DataOrigin::Native,
    )
    .expect("valid outcome")
    .is_none());
    assert!(feedback_from_outcome(
        &outcome,
        None,
        outcome.timestamp,
        FeedbackSource::Client,
        DataOrigin::Native,
    )
    .is_none());

    let feedback = Feedback::try_from_outcome(
        &outcome,
        vec![FeedbackSignal::ExplicitRating { score: 4.0 }],
        outcome.timestamp,
        FeedbackSource::Client,
        DataOrigin::Native,
    )
    .expect("valid feedback")
    .expect("explicit signal");
    assert_eq!(feedback.rating(), Some(4.0));
    assert!(feedback.matches_outcome(&outcome));
    assert!(feedback.validate().is_ok());
}

#[test]
fn outcome_summary_and_dataset_conversion_are_pure_and_deterministic() {
    let outcome = fallback_success();
    let summary = OutcomeSummary::try_from_outcome(&outcome).expect("summary");
    assert!(summary.success);
    assert_eq!(summary.initial_model, "model-a");
    assert_eq!(summary.final_model, "model-b");
    assert_eq!(summary.failure_class, None);

    let first = canonical_samples_from_outcome(
        &outcome,
        &[Default::default(), Default::default()],
        DataOrigin::Native,
    )
    .expect("canonical samples");
    let second = canonical_samples_from_outcome(
        &outcome,
        &[Default::default(), Default::default()],
        DataOrigin::Native,
    )
    .expect("canonical samples again");
    assert_eq!(first.len(), 3);
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&second).unwrap()
    );
    assert!(matches!(
        first[0].scope,
        SampleScope::Attempt { index: 0, .. }
    ));
    assert!(matches!(first[2].scope, SampleScope::Request));
    assert_eq!(first[0].provider_id, "provider-a");
    assert_eq!(first[2].provider_id, "provider-b");
    assert!(first[2].feedback.is_none());
    assert!(!first[2].has_feedback());

    let store = DatasetStore::new(10, 3600);
    assert!(store.is_empty());
    let _ = canonical_samples_from_outcome(
        &outcome,
        &[Default::default(), Default::default()],
        DataOrigin::Native,
    )
    .expect("conversion remains pure");
    assert!(store.is_empty());

    let sample_a = SampleBuilder::try_build(&outcome, Default::default(), DataOrigin::Native)
        .expect("legacy sample");
    let sample_b = SampleBuilder::try_build(&outcome, Default::default(), DataOrigin::Native)
        .expect("legacy sample again");
    assert_eq!(sample_a.sample_id, sample_b.sample_id);
}

#[test]
fn optional_feedback_reaches_samples_without_becoming_a_target() {
    let outcome = fallback_success();
    let feedback = Feedback::try_from_outcome(
        &outcome,
        vec![FeedbackSignal::ConversationContinued],
        outcome.timestamp,
        FeedbackSource::Client,
        DataOrigin::Native,
    )
    .expect("feedback")
    .expect("signal");
    let samples = try_samples_from_outcome_with_feedback(
        &outcome,
        &[Default::default(), Default::default()],
        DataOrigin::Native,
        Some(&feedback),
    )
    .expect("samples");
    assert!(samples.iter().all(|sample| sample.feedback.is_some()));
    assert!(samples
        .iter()
        .all(|sample| !sample.targets.success || sample.final_status == FinalStatus::Success));
    assert_eq!(samples[2].feedback.as_ref().unwrap().signals.len(), 1);

    let json = serde_json::to_string(&samples[2]).expect("serialize canonical sample");
    let restored: zroutery_core::ml::dataset::OutcomeTrainingSample =
        serde_json::from_str(&json).expect("deserialize canonical sample");
    assert_eq!(restored.outcome_id, outcome.outcome_id);
    assert_eq!(restored.request_id, outcome.request_id);
    assert_eq!(restored.streaming, outcome.streaming);
    assert_eq!(restored.dialect, outcome.dialect);
    assert_eq!(restored.identity.served, outcome.identity().served);
    assert_eq!(restored.terminal_error, outcome.terminal_error);
}

#[test]
fn optional_feedback_from_another_outcome_is_refused() {
    let outcome = fallback_success();
    let sample = SampleBuilder::try_build(&outcome, Default::default(), DataOrigin::Native)
        .expect("legacy sample");
    let feedback = Feedback::try_from_outcome(
        &outcome,
        vec![FeedbackSignal::ExplicitRating { score: 5.0 }],
        outcome.timestamp,
        FeedbackSource::Client,
        DataOrigin::Native,
    )
    .expect("feedback")
    .expect("signal");

    // A well formed record for a different Outcome: its own validation passes,
    // so only the correlation check can refuse it.
    let mut foreign = feedback.clone();
    foreign.outcome_id = "out-someone-else".to_string();
    foreign
        .validate()
        .expect("a well formed feedback record for another outcome");
    let error = sample
        .clone()
        .with_optional_feedback(Some(&foreign))
        .expect_err("a foreign feedback must be refused");
    assert!(error.contains("does not match"), "got: {error}");

    // The matching record is still accepted, and it stays a signal rather than
    // a target.
    let accepted = sample
        .with_optional_feedback(Some(&feedback))
        .expect("matching feedback");
    assert_eq!(accepted.outcome_id, outcome.outcome_id);
    assert_eq!(accepted.feedback, feedback.signals);
}

#[test]
fn serde_round_trip_preserves_identity_and_reads_legacy_identity_shapes() {
    let outcome = fallback_success();
    let json = serde_json::to_string(&outcome).expect("serialize outcome");
    let restored: Outcome = serde_json::from_str(&json).expect("deserialize outcome");
    assert_eq!(restored, outcome);
    assert_eq!(restored.served_identity(), outcome.served_identity());
    assert_eq!(restored.terminal_error, outcome.terminal_error);

    let mut legacy_json = serde_json::to_value(&outcome).expect("legacy value");
    let object = legacy_json.as_object_mut().expect("object");
    for field in [
        "planned_model",
        "planned_provider",
        "last_attempted_model",
        "last_attempted_provider",
        "served_model",
        "served_provider",
        "terminal_error",
    ] {
        object.remove(field);
    }
    let legacy: Outcome = serde_json::from_value(legacy_json).expect("legacy deserialize");
    assert_eq!(legacy.planned_identity(), outcome.planned_identity());
    assert_eq!(
        legacy.last_attempted_identity(),
        outcome.last_attempted_identity()
    );
    assert_eq!(legacy.served_identity(), outcome.served_identity());
    assert!(legacy.validate().is_ok());
}

#[test]
fn rectifier_attempt_evidence_is_retained_in_the_canonical_sample() {
    let mut rectified = attempt("att-rectified", "model-a", "provider-a", true, 100.0, None);
    rectified.rectified = true;
    let outcome = Outcome::builder("req-rectified")
        .single_candidate("model-a", "provider-a")
        .attempt(rectified)
        .build();
    let samples = canonical_samples_from_outcome(&outcome, &[], DataOrigin::Native)
        .expect("rectifier sample");
    assert!(samples[0].rectified);
    assert_eq!(samples[0].attempt_id.as_deref(), Some("att-rectified"));
}
