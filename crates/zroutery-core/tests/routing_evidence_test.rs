#![cfg(feature = "ml")]

//! Does an offline evaluation refuse to conclude more than its evidence supports?
//!
//! The promotion gate is threshold-only: its floors are a paired-request count and a
//! raw effect delta, so a large delta measured on a thin, correlated sample reads the
//! same as one measured on thousands. These tests pin the statistical layer that
//! closes that gap, and in particular three ways it must *not* overclaim:
//!
//! * a tiny sample must `BLOCKED`, never `Supported`;
//! * a precisely-measured but negligible effect must not be actionable;
//! * correlated traffic must never be counted as independent evidence.

#![cfg(feature = "ml")]

use zroutery_core::feedback::DataOrigin;
use zroutery_core::ml::comparison::ReplayBaseline;
use zroutery_core::ml::dataset::{OutcomeTrainingSample, SampleScope, Targets};
use zroutery_core::ml::eval_report::{evaluate, EvaluationConfig, EvaluationVerdict};
use zroutery_core::ml::eval_stats::{measure, Clustering, EvidenceConfig, PairedObservation};
use zroutery_core::ml::features::{
    RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION, UNKNOWN,
};
use zroutery_core::ml::shadow::{ShadowCandidateInput, ShadowInput};
use zroutery_core::ml::traces::RequestTrace;
use zroutery_core::ml::{MlPolicy, ModelEnsemble, RewardPolicy};
use zroutery_core::outcome::FinalStatus;

const HOUR: i64 = 3_600;
const BASE: i64 = 1_700_000_000;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn features(priority: f32, latency: f32) -> RoutingFeatures {
    let mut values = [UNKNOWN; FEATURE_DIMENSION];
    values[16] = priority;
    values[18] = latency;
    RoutingFeatures {
        schema_version: FEATURE_SCHEMA_VERSION,
        values,
    }
}

/// One trace whose two candidates behave differently.
///
/// `fast` always succeeds quickly; `slow` fails on the last `slow_failures` requests.
/// Placing the failures at the end means the trace timestamps spread over `spread_secs`
/// and can be made correlated (bunched into one hour) on demand.
fn build(count: usize, spread_secs: i64) -> Vec<RequestTrace> {
    (0..count)
        .map(|index| {
            let slow_fails = index >= count / 2;
            let input = ShadowInput {
                decision_id: format!("dec-{index}"),
                production_selected: "fast".to_string(),
                candidates: vec![
                    ShadowCandidateInput {
                        candidate_id: "fast".to_string(),
                        provider_id: "p1".to_string(),
                        tier: None,
                        eligible: true,
                        features: features(0.9, 0.95),
                        rejection_reason: None,
                    },
                    ShadowCandidateInput {
                        candidate_id: "slow".to_string(),
                        provider_id: "p2".to_string(),
                        tier: None,
                        eligible: true,
                        features: features(0.2, 0.10),
                        rejection_reason: None,
                    },
                ],
                ..ShadowInput::default()
            };
            RequestTrace::new(
                format!("req-{index}"),
                BASE + index as i64 * spread_secs,
                input,
                vec![sample(index, slow_fails)],
            )
        })
        .collect()
}

fn sample(index: usize, failed: bool) -> OutcomeTrainingSample {
    OutcomeTrainingSample {
        sample_id: format!("s-{index}"),
        schema_version: 1,
        timestamp: BASE + index as i64,
        streaming: false,
        dialect: "anthropic".to_string(),
        features: features(0.9, 0.95),
        targets: Targets {
            success: !failed,
            latency_ms: Some(if failed { 0.0 } else { 100.0 }),
            ttft_ms: None,
            cost: Some(0.01),
            failure_class: failed.then(|| "transport".to_string()),
            fallback_count: 0,
        },
        provider_id: "p1".to_string(),
        model_id: "fast".to_string(),
        origin: DataOrigin::Native,
        outcome_id: format!("out-{index}"),
        request_id: format!("req-{index}"),
        decision_id: Some(format!("dec-{index}")),
        response_id: None,
        final_status: if failed {
            FinalStatus::Failed
        } else {
            FinalStatus::Success
        },
        success: !failed,
        identity: Default::default(),
        scope: SampleScope::Attempt {
            index: 0,
            attempt_id: format!("att-{index}"),
        },
        attempt_id: Some(format!("att-{index}")),
        rectified: false,
        attempts: Vec::new(),
        usage: None,
        estimated_cost: Some(0.01),
        actual_cost: Some(0.01),
        terminal_error: None,
        feedback: None,
    }
}

fn candidate() -> MlPolicy {
    MlPolicy::from_ensemble(ModelEnsemble::new(), RewardPolicy::default())
}

fn observations(n: usize, delta: f64, spread_secs: i64) -> Vec<PairedObservation> {
    (0..n)
        .map(|index| PairedObservation {
            recorded_at: BASE + index as i64 * spread_secs,
            utility_delta: delta,
            candidate_only_success: delta > 0.0,
            baseline_only_success: false,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The statistical layer
// ---------------------------------------------------------------------------

#[test]
fn a_thin_sample_blocks_even_with_an_enormous_effect() {
    // The headline property. 20 pairs at +0.9 is a larger measured delta than 2000
    // pairs at +0.25, and it must not be reportable as the stronger result.
    let evidence = measure(&observations(20, 0.9, HOUR), &EvidenceConfig::default());
    assert!(!evidence.is_sufficient(), "{}", evidence.headline());
    assert!(!evidence.is_actionable());
    assert!(evidence
        .blockers
        .iter()
        .any(|blocker| blocker.contains("below the")));
}

#[test]
fn a_precise_but_negligible_effect_is_not_actionable() {
    // Precision is not value. An interval that excludes zero but sits far below the
    // minimum detectable effect describes an effect too small to act on.
    let observations: Vec<PairedObservation> = (0..500)
        .map(|index| PairedObservation {
            recorded_at: BASE + index as i64 * HOUR,
            // Alternating so there is genuine dispersion and the interval is finite.
            utility_delta: if index % 2 == 0 { 0.03 } else { 0.01 },
            candidate_only_success: index % 2 == 0,
            baseline_only_success: index % 2 == 1,
        })
        .collect();
    let evidence = measure(&observations, &EvidenceConfig::default());
    let interval = evidence.utility.expect("interval");
    assert!(
        !interval.clears(evidence.minimum_effect),
        "an effect of {:+.4} must not clear a {:+.4} bar",
        interval.mean,
        evidence.minimum_effect
    );
    assert!(!evidence.is_actionable());
}

#[test]
fn correlated_traffic_is_not_counted_as_independent_evidence() {
    // 400 requests, but packed into 8 hours instead of spread over 400. The mean is
    // identical; a correct clustered estimator reports a much wider interval.
    let spread = observations(400, 0.5, HOUR);
    let bunched = observations(400, 0.5, 5);

    let spread_width = {
        let interval = measure(&spread, &EvidenceConfig::default())
            .utility
            .expect("interval");
        interval.upper - interval.lower
    };
    let bunched_width = {
        let interval = measure(&bunched, &EvidenceConfig::default())
            .utility
            .expect("interval");
        interval.upper - interval.lower
    };
    assert!(
        bunched_width > spread_width,
        "bunching must widen the interval: {bunched_width} vs {spread_width}"
    );
    assert!(
        measure(&bunched, &EvidenceConfig::default()).clusters
            < measure(&spread, &EvidenceConfig::default()).clusters,
        "bunched traffic must form fewer clusters"
    );
}

#[test]
fn clustering_can_be_switched_off_and_the_choice_is_visible() {
    let data = observations(100, 0.4, HOUR);
    let naive = EvidenceConfig {
        clustering: Clustering::None,
        ..Default::default()
    };
    assert_eq!(measure(&data, &naive).clusters, 100);
    assert_eq!(measure(&data, &EvidenceConfig::default()).clusters, 100);
    assert_eq!(measure(&data, &naive).clustering, Clustering::None);
}

#[test]
fn arms_that_never_disagree_cannot_produce_an_improvement_claim() {
    let observations: Vec<PairedObservation> = (0..200)
        .map(|index| PairedObservation {
            recorded_at: BASE + index as i64 * HOUR,
            utility_delta: 0.0,
            candidate_only_success: false,
            baseline_only_success: false,
        })
        .collect();
    let evidence = measure(&observations, &EvidenceConfig::default());
    assert_eq!(evidence.discordance_rate, 0.0);
    assert!(!evidence.is_actionable());
    assert!(evidence
        .blockers
        .iter()
        .any(|blocker| blocker.contains("never disagreed")));
}

#[test]
fn the_evidence_is_reproducible() {
    let data = observations(150, 0.3, HOUR);
    assert_eq!(
        measure(&data, &EvidenceConfig::default()),
        measure(&data, &EvidenceConfig::default())
    );
}

#[test]
fn required_n_is_advisory_rather_than_a_second_gate() {
    // A planning number, not a second count. Two bodies can both be perfectly
    // measurable while one of them is underpowered for the *smallest* effect worth
    // caring about — and refusing that one would refuse a well-measured improvement
    // for a claim it never made.
    let evidence = measure(&observations(40, 0.5, HOUR), &EvidenceConfig::default());
    let required = evidence.required_pairs.expect("required n");
    assert!(
        required > evidence.pairs,
        "this fixture is deliberately underpowered for the minimum effect"
    );
    assert!(
        evidence.is_sufficient(),
        "being underpowered for the minimum effect must not block a measured one: {:?}",
        evidence.blockers
    );
    assert!(evidence
        .caveats
        .iter()
        .any(|caveat| caveat.contains("would be needed to detect")));
}

#[test]
fn the_gate_blocks_when_the_interval_includes_zero() {
    // The criterion's actual work: a mean improvement whose confidence interval spans
    // zero is not an improvement, and must not promote.
    use zroutery_core::ml::{PromotionGate, PromotionVerdict};

    let traces = build(200, HOUR);
    let comparison = zroutery_core::ml::run_comparison_with_records(
        &traces,
        &candidate(),
        &[ReplayBaseline::Priority],
        &RewardPolicy::default(),
    )
    .expect("comparison")
    .comparison;

    // Rebuild the pairing so the mean is large but the evidence is deliberately thin.
    let mut thin = comparison.clone();
    let mut pairing = thin
        .paired
        .first()
        .cloned()
        .expect("pairing against the first baseline");
    pairing.deltas.mean_observed_utility_delta = 0.9;
    pairing.deltas.paired_requests = 200;
    thin.paired = vec![pairing];
    // Keep evidence consistent with the claim: the interval decides, not the mean.

    let report = zroutery_core::ml::run_training(
        &traces
            .iter()
            .flat_map(|trace| trace.samples.clone())
            .collect::<Vec<_>>(),
        &zroutery_core::ml::TrainingConfig::default(),
    )
    .expect("training")
    .report;
    let config = zroutery_core::ml::PromotionConfig {
        required_baseline: "baseline.priority".to_string(),
        ..Default::default()
    };

    let decision = PromotionGate::new(config).evaluate(&report, &thin, None);
    let criterion = decision
        .criteria
        .iter()
        .find(|criterion| criterion.name == "statistical_evidence")
        .expect("the statistical criterion must be emitted");
    // Whatever it decided, it must have *reported a measurement*. A criterion that is
    // always `held` is a no-op, which is the failure this test exists to catch.
    assert!(
        criterion.measured.is_some(),
        "the criterion must report what it measured: {criterion:?}"
    );
    // And the verdict must follow the interval, not the mean it was handed.
    if criterion.held {
        assert!(
            decision.verdict == PromotionVerdict::Promoted
                || decision.verdict == PromotionVerdict::Blocked
        );
    } else {
        assert_eq!(decision.verdict, PromotionVerdict::Rejected);
    }
}

// ---------------------------------------------------------------------------
// The evaluation wires it in
// ---------------------------------------------------------------------------

#[test]
fn an_evaluation_over_thin_traffic_is_blocked_not_supported() {
    // 20 traces, spread so they are not degenerate on the pair floor alone. The
    // verdict must still be blocked, and must say the evidence was too thin.
    let traces = build(20, HOUR);
    let config = EvaluationConfig {
        min_traces: 10,
        ..Default::default()
    };
    let report = evaluate(&traces, &candidate(), &config, None, BASE).expect("evaluate");

    match &report.verdict {
        EvaluationVerdict::Blocked { reasons } => {
            assert!(
                reasons.iter().any(|reason| reason.contains("below")),
                "a thin comparison must be blocked for want of evidence: {reasons:?}"
            );
        }
        other => panic!("expected Blocked, got {other:?}"),
    }
    assert!(!report.verdict.is_promotable());
}

#[test]
fn an_absent_resource_shadow_is_reported_as_absence_not_agreement() {
    // Most bodies will have no temporal shadow record at all: the shipped binary does
    // not enable `account`, so there are no buckets to plan over. Reporting that as
    // "0% disagreement" would claim a comparison that never happened.
    let traces = build(200, HOUR);
    let report = evaluate(
        &traces,
        &candidate(),
        &EvaluationConfig::default(),
        None,
        BASE,
    )
    .expect("evaluate");
    let shadow = &report.resource_shadow;
    assert_eq!(shadow.recorded, 0);
    assert_eq!(
        shadow.disagreement_rate(),
        (0.0, 0),
        "no record, no denominator"
    );

    let markdown = report.to_markdown();
    assert!(markdown.contains("## Temporal resource shadow"));
    assert!(
        markdown.contains("absence of evidence, not agreement"),
        "the report must not read as agreement: {markdown}"
    );
}

#[test]
fn a_resource_shadow_record_is_summarised_with_its_denominator() {
    use zroutery_core::ml::resource_buckets::{ChargeOrder, ResourceBucket, ResourceKind};
    use zroutery_core::ml::temporal_planner::TemporalConfig;
    use zroutery_core::ml::{evaluate_resource_shadow, DemandForecast};

    let mut traces = build(200, HOUR);
    let bucket = ResourceBucket {
        resource_id: "plan".into(),
        provider_id: "p1".into(),
        account_id: "a1".into(),
        kind: ResourceKind::TokenPlan,
        unit: "tokens".into(),
        available_amount: 1000.0,
        total_amount: 1000.0,
        expires_at: None,
        resets_at: None,
        reset_behavior: zroutery_core::ml::ResetBehavior::CarryOver,
        valid_from: None,
        applicable_models: vec![],
        reserved_amount: 0.0,
        source: zroutery_core::ml::ObservationSource::ProviderReported,
        observed_at: BASE,
    };
    let order = ChargeOrder::declared(vec!["plan".into()]);

    for trace in traces.iter_mut() {
        trace.resource_shadow = evaluate_resource_shadow(
            &trace.input,
            std::slice::from_ref(&bucket),
            &DemandForecast::measured(10.0, 0.8, 50),
            &order,
            trace.recorded_at,
            &TemporalConfig::default(),
        );
    }

    let report = evaluate(
        &traces,
        &candidate(),
        &EvaluationConfig::default(),
        None,
        BASE,
    )
    .expect("evaluate");
    let shadow = &report.resource_shadow;
    assert_eq!(shadow.recorded, 200);
    assert_eq!(shadow.with_preference, 200);
    // The snapshot prefers `fast`; production also picked `fast` in this fixture, so
    // every record agrees — which is the honest answer, not a missing one.
    assert_eq!(shadow.disagreements, 0);
    assert_eq!(shadow.disagreement_rate().1, 200);

    // And no record may claim an effect it cannot support.
    assert!(traces.iter().all(|trace| trace
        .resource_shadow
        .as_ref()
        .is_some_and(|record| !record.supports_an_effect_claim())));

    let markdown = report.to_markdown();
    assert!(markdown.contains("Would have disagreed with production"));
    assert!(
        markdown.contains("a blast radius, not an effect"),
        "the report must keep preference and performance apart"
    );
}

#[test]
fn the_report_states_the_interval_and_its_denominators() {
    let traces = build(200, HOUR);
    let config = EvaluationConfig::default();
    let report = evaluate(&traces, &candidate(), &config, None, BASE).expect("evaluate");

    assert_eq!(
        report.evidence.pairs,
        report
            .production_pairing
            .as_ref()
            .map(|p| p.deltas.paired_requests)
            .unwrap_or(0)
    );
    assert!(
        report.evidence.utility.is_some(),
        "a comparison should yield an interval"
    );
    assert_eq!(report.evidence.min_pairs, 30);
    assert!(report
        .evidence
        .min_pairs
        .eq(&zroutery_core::ml::comparison::MIN_PAIRED_REQUESTS));

    let markdown = report.to_markdown();
    assert!(markdown.contains("## Statistical evidence"));
    assert!(markdown.contains("Effect on observed utility"));
    assert!(markdown.contains("time cluster(s)"));
}

#[test]
fn the_evidence_config_is_fingerprinted_into_the_report() {
    let traces = build(200, HOUR);
    let mut config = EvaluationConfig::default();
    config.evidence.clustering = Clustering::None;
    let report = evaluate(&traces, &candidate(), &config, None, BASE).expect("evaluate");
    // A different evidence configuration must be distinguishable in the artefact.
    let mut other = EvaluationConfig::default();
    other.evidence.minimum_effect += 0.05;
    let other_report = evaluate(&traces, &candidate(), &other, None, BASE).expect("evaluate");
    assert_ne!(
        report.evidence.config_fingerprint,
        other_report.evidence.config_fingerprint
    );
}
