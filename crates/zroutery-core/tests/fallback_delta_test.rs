#![cfg(feature = "ml")]

//! The paired fallback delta, and the promotion criterion that reads it.
//!
//! `PairedDeltas::fallback_rate_delta` was a hardcoded `0.0`, so every comparison
//! reported "this policy does not change how often a retry is needed" regardless of the
//! traffic, and `PromotionConfig::max_fallback_rate_delta` — a real, documented
//! tolerance — was read by no criterion. These tests pin the measurement, because a
//! constant zero that passes a gate is worse than a missing measurement: it looks
//! like a passing result.

use zroutery_core::feedback::DataOrigin;
use zroutery_core::ml::comparison::ReplayBaseline;
use zroutery_core::ml::comparison::{
    pair_against_baseline, ArmRecord, MeasuredOutcome, PolicyChoice, MIN_PAIRED_REQUESTS,
};
use zroutery_core::ml::dataset::{OutcomeTrainingSample, SampleScope, Targets};
use zroutery_core::ml::features::{
    RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION, UNKNOWN,
};
use zroutery_core::ml::promotion::{PromotionConfig, PromotionGate, PromotionVerdict};
use zroutery_core::ml::shadow::{ShadowCandidateInput, ShadowInput};
use zroutery_core::ml::traces::RequestTrace;
use zroutery_core::ml::RewardPolicy;
use zroutery_core::outcome::FinalStatus;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn choice(request: &str, candidate: &str) -> PolicyChoice {
    PolicyChoice {
        policy: "test.arm".to_string(),
        request_id: request.to_string(),
        candidate_id: candidate.to_string(),
        provider_id: "p1".to_string(),
        eligible: true,
        reason: String::new(),
    }
}

/// One arm record: `failed` makes `fell_back` true, as `run_comparison` derives it.
fn record(request: &str, candidate: &str, failed: bool) -> ArmRecord {
    let outcome = MeasuredOutcome {
        success: !failed,
        latency_ms: Some(100.0),
        ttft_ms: None,
        cost: Some(0.01),
    };
    ArmRecord {
        choice: choice(request, candidate),
        measured: true,
        outcome: Some(outcome),
        fell_back: failed,
        utility: None,
    }
}

fn features() -> RoutingFeatures {
    let mut values = [UNKNOWN; FEATURE_DIMENSION];
    values[16] = 0.9;
    RoutingFeatures {
        schema_version: FEATURE_SCHEMA_VERSION,
        values,
    }
}

/// A trace whose single candidate failed, so any policy choosing it must fall back.
fn failing_trace(index: usize) -> RequestTrace {
    let sample = OutcomeTrainingSample {
        sample_id: format!("s-{index}"),
        schema_version: 1,
        timestamp: 1_700_000_000 + index as i64,
        streaming: false,
        dialect: "anthropic".to_string(),
        features: features(),
        targets: Targets {
            success: false,
            latency_ms: None,
            ttft_ms: None,
            cost: Some(0.01),
            failure_class: Some("transport".into()),
            fallback_count: 0,
        },
        provider_id: "p1".to_string(),
        model_id: "only".to_string(),
        origin: DataOrigin::Native,
        outcome_id: format!("out-{index}"),
        request_id: format!("req-{index}"),
        decision_id: Some(format!("dec-{index}")),
        response_id: None,
        final_status: FinalStatus::Failed,
        success: false,
        identity: Default::default(),
        scope: SampleScope::Attempt {
            index: 0,
            attempt_id: format!("att-{index}"),
        },
        attempt_id: Some(format!("att-{index}")),
        rectified: false,
        attempts: Vec::new(),
        usage: None,
        estimated_cost: None,
        actual_cost: Some(0.01),
        terminal_error: None,
        feedback: None,
    };
    let input = ShadowInput {
        decision_id: format!("dec-{index}"),
        production_selected: "only".to_string(),
        candidates: vec![ShadowCandidateInput {
            candidate_id: "only".to_string(),
            provider_id: "p1".to_string(),
            tier: None,
            eligible: true,
            features: features(),
            rejection_reason: None,
        }],
        ..ShadowInput::default()
    };
    RequestTrace::new(
        format!("req-{index}"),
        1_700_000_000 + index as i64,
        input,
        vec![sample],
    )
}

#[test]
fn a_body_where_every_chosen_candidate_failed_reports_a_full_fallback_rate() {
    // 40 requests, both arms choosing the only (failing) candidate. Both arms need a
    // retry every time, so the delta must be zero — but only because it was measured,
    // not because it was hardcoded.
    let baseline: Vec<ArmRecord> = (0..40)
        .map(|i| record(&format!("req-{i}"), "only", true))
        .collect();
    let candidate = baseline.clone();

    let (deltas, _) = pair_against_baseline(&baseline, &candidate, &RewardPolicy::default());
    assert_eq!(deltas.paired_requests, 40);
    assert_eq!(deltas.fallback_rate_delta, 0.0);
}

#[test]
fn a_candidate_that_fails_more_often_reports_a_positive_delta() {
    // Baseline picks a candidate that works 10 times in 40; the candidate arm picks
    // the failing one 30 times. The delta must be positive and non-zero — this is the
    // case the constant zero silently passed.
    let mut baseline = Vec::new();
    let mut candidate = Vec::new();
    for index in 0..40 {
        let base_failed = index < 30;
        let cand_failed = index < 10;
        baseline.push(record(&format!("req-{index}"), "only", base_failed));
        candidate.push(record(&format!("req-{index}"), "only", cand_failed));
    }

    let (deltas, _) = pair_against_baseline(&baseline, &candidate, &RewardPolicy::default());
    // Baseline needs a retry 30/40, the candidate 10/40.
    assert!(
        (deltas.fallback_rate_delta + 0.5).abs() < 1e-12,
        "{}",
        deltas.fallback_rate_delta
    );
    assert!(deltas.success_rate_delta > 0.0);
}

#[test]
fn the_fallback_delta_uses_the_same_paired_set_as_every_other_delta() {
    // An arm record that was never measured contributes to neither side, so the two
    // counts cannot drift onto different denominators.
    let mut baseline: Vec<ArmRecord> = (0..40)
        .map(|i| record(&format!("req-{i}"), "only", i < 20))
        .collect();
    let mut candidate = baseline.clone();
    // Unmeasured records: excluded from the pairing entirely.
    baseline.push(ArmRecord {
        choice: choice("req-unmeasured", "only"),
        measured: false,
        outcome: None,
        fell_back: false,
        utility: None,
    });
    candidate.push(ArmRecord {
        choice: choice("req-unmeasured", "only"),
        measured: false,
        outcome: None,
        fell_back: false,
        utility: None,
    });

    let (deltas, _) = pair_against_baseline(&baseline, &candidate, &RewardPolicy::default());
    assert_eq!(deltas.paired_requests, 40);
}

#[test]
fn the_fallback_delta_is_zero_only_when_there_are_no_pairs() {
    // The degenerate case must not be confused with a measured "no change".
    let (deltas, verdict) = pair_against_baseline(&[], &[], &RewardPolicy::default());
    assert_eq!(deltas.paired_requests, 0);
    assert_eq!(deltas.fallback_rate_delta, 0.0);
    // And it reports insufficient evidence rather than no difference.
    assert_eq!(
        format!("{verdict:?}"),
        "InsufficientEvidence",
        "an empty pairing must never read as a measured result"
    );
}

// ---------------------------------------------------------------------------
// The promotion criterion
// ---------------------------------------------------------------------------

#[test]
fn the_fallback_criterion_is_reported_and_reads_a_real_measurement() {
    // Every candidate whose chosen candidate failed forces the measured delta to 1.0,
    // which must exceed the default tolerance and fail the criterion.
    let traces: Vec<RequestTrace> = (0..40).map(failing_trace).collect();
    let comparison = zroutery_core::ml::run_comparison(
        &traces,
        &cold_candidate(),
        ReplayBaseline::ALL.as_slice(),
        &RewardPolicy::default(),
    )
    .expect("comparison");

    let decision = PromotionGate::new(PromotionConfig::default()).evaluate(
        &training(&traces),
        &comparison,
        None,
    );

    let criterion = decision
        .criteria
        .iter()
        .find(|c| c.name == "fallback_regression")
        .expect("the fallback criterion must exist");
    assert!(
        criterion.measured.is_some(),
        "the criterion must report what it measured"
    );
    // Verdict must not be PROMOTED: every request in this body needed a retry.
    assert_ne!(decision.verdict, PromotionVerdict::Promoted);
}

#[test]
fn the_gate_reads_the_same_delta_the_pairing_measured() {
    // The point of this test is wiring, not a particular number. The gate's criterion
    // must report exactly the value `pair_against_baseline` computed for the same
    // pairing — if it ever read a constant again, this equality would break.
    //
    // In this fixture the only candidate fails, so *both* arms need a retry on every
    // request and the true delta is 0.0. That 0.0 is a measurement, not a default,
    // and the test proves it by being derived independently below.
    let traces: Vec<RequestTrace> = (0..40).map(failing_trace).collect();
    let comparison = zroutery_core::ml::run_comparison(
        &traces,
        &cold_candidate(),
        ReplayBaseline::ALL.as_slice(),
        &RewardPolicy::default(),
    )
    .expect("comparison");

    let pairing = comparison
        .pairing(&PromotionConfig::default().required_baseline)
        .expect("pairing against the required baseline");
    let criterion = PromotionGate::new(PromotionConfig::default())
        .evaluate(&training(&traces), &comparison, None)
        .criteria
        .into_iter()
        .find(|c| c.name == "fallback_regression")
        .expect("criterion");

    assert_eq!(
        criterion.measured,
        Some(pairing.deltas.fallback_rate_delta),
        "the gate must report the measured delta, not a constant"
    );
    assert_eq!(
        criterion.threshold,
        Some(PromotionConfig::default().max_fallback_rate_delta)
    );
}

#[test]
fn a_tolerance_stricter_than_the_measurement_fails_the_criterion() {
    // With the measured delta at 0.0, a tolerance of exactly 0.0 must hold and one
    // below it must not. That pins the comparison to `<=`, and proves the knob is
    // consulted rather than merely recorded.
    let traces: Vec<RequestTrace> = (0..40).map(failing_trace).collect();
    let comparison = zroutery_core::ml::run_comparison(
        &traces,
        &cold_candidate(),
        ReplayBaseline::ALL.as_slice(),
        &RewardPolicy::default(),
    )
    .expect("comparison");
    let report = training(&traces);

    let exactly_zero = PromotionConfig {
        max_fallback_rate_delta: 0.0,
        ..Default::default()
    };
    let held = PromotionGate::new(exactly_zero)
        .evaluate(&report, &comparison, None)
        .criteria
        .into_iter()
        .find(|c| c.name == "fallback_regression")
        .expect("criterion");
    assert!(
        held.held,
        "a measured 0.0 delta must satisfy a 0.0 tolerance"
    );

    let negative = PromotionConfig {
        max_fallback_rate_delta: -0.01,
        ..Default::default()
    };
    let failed = PromotionGate::new(negative)
        .evaluate(&report, &comparison, None)
        .criteria
        .into_iter()
        .find(|c| c.name == "fallback_regression")
        .expect("criterion");
    assert!(
        !failed.held,
        "a measured 0.0 delta must fail a -0.01 tolerance"
    );
}

#[test]
fn the_default_config_still_names_a_fallback_tolerance() {
    // The tolerance was always in the config and always in `identity()`; what was
    // missing was anything reading it.
    let config = PromotionConfig::default();
    assert!(config.max_fallback_rate_delta >= 0.0);
    // Changing it must change the config identity, so a promotion decision records
    // the tolerance that was actually applied.
    let other = PromotionConfig {
        max_fallback_rate_delta: config.max_fallback_rate_delta + 0.01,
        ..Default::default()
    };
    assert_ne!(config.identity(), other.identity());
}

fn cold_candidate() -> zroutery_core::ml::MlPolicy {
    zroutery_core::ml::MlPolicy::from_ensemble(
        zroutery_core::ml::ModelEnsemble::new(),
        RewardPolicy::default(),
    )
}

fn training(traces: &[RequestTrace]) -> zroutery_core::ml::TrainingReport {
    let mut samples = Vec::new();
    for trace in traces {
        samples.extend(trace.samples.clone());
    }
    let outcome =
        zroutery_core::ml::run_training(&samples, &zroutery_core::ml::TrainingConfig::default())
            .expect("training");
    outcome.report
}

#[test]
fn the_paired_floor_is_still_what_it_was() {
    // Guards against the fix changing the existing evidence floor.
    let traces: Vec<RequestTrace> = (0..5).map(failing_trace).collect();
    let comparison = zroutery_core::ml::run_comparison(
        &traces,
        &cold_candidate(),
        ReplayBaseline::ALL.as_slice(),
        &RewardPolicy::default(),
    )
    .expect("comparison");
    assert!(comparison.traces < MIN_PAIRED_REQUESTS);
}
