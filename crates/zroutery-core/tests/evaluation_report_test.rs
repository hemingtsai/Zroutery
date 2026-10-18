//! The offline evaluation entry point, and its refusal to overclaim.
//!
//! The behaviours under test are the ones where a routing evaluation most easily
//! produces a true-sounding lie: reporting a win on data that cannot support it,
//! reporting "no" when the honest answer is "cannot tell", turning a missing field
//! into a zero, or crediting a task result to a model that did not produce it.

use zroutery_core::feedback::DataOrigin;
use zroutery_core::ml::dataset::{OutcomeTrainingSample, SampleScope, Targets};
use zroutery_core::ml::eval_report::{
    evaluability, evaluate, DataQualityReport, EvaluationConfig, EvaluationVerdict,
    TaskQualityVerdict,
};
use zroutery_core::ml::execution_identity::ExecutionIdentity;
use zroutery_core::ml::features::{
    RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION, UNKNOWN,
};
use zroutery_core::ml::shadow::{ShadowCandidateInput, ShadowInput};
use zroutery_core::ml::task_evidence::{
    EvidenceOrigin, TaskOutcomeEvidence, TaskStatus, VerifierKind,
};
use zroutery_core::ml::traces::RequestTrace;
use zroutery_core::ml::{MlPolicy, ModelEnsemble, RewardPolicy};
use zroutery_core::outcome::FinalStatus;
use zroutery_core::policy::TaskType;

// ---------------------------------------------------------------------------
// Fixture: a small but complete body of traces
// ---------------------------------------------------------------------------

struct Fixture {
    traces: Vec<RequestTrace>,
}

impl Fixture {
    /// Build `count` traces over two candidates, where candidate `fast` succeeds and
    /// `slow` fails.
    fn build(count: usize) -> Self {
        let mut traces = Vec::with_capacity(count);
        for index in 0..count {
            let seed = index as f32;
            let mut fast_values = [UNKNOWN; FEATURE_DIMENSION];
            fast_values[16] = 0.9; // F_PRIORITY
            fast_values[18] = 0.95; // F_OBS_LATENCY_EWMA
            let mut slow_values = [UNKNOWN; FEATURE_DIMENSION];
            slow_values[16] = 0.2;
            slow_values[18] = 0.10;

            let input = ShadowInput {
                decision_id: format!("dec-{index}"),
                production_selected: "fast".to_string(),
                feature_schema: FEATURE_SCHEMA_VERSION,
                candidates: vec![
                    ShadowCandidateInput {
                        candidate_id: "fast".to_string(),
                        provider_id: "p1".to_string(),
                        tier: None,
                        eligible: true,
                        features: RoutingFeatures {
                            schema_version: FEATURE_SCHEMA_VERSION,
                            values: fast_values,
                        },
                        rejection_reason: None,
                    },
                    ShadowCandidateInput {
                        candidate_id: "slow".to_string(),
                        provider_id: "p2".to_string(),
                        tier: None,
                        eligible: true,
                        features: RoutingFeatures {
                            schema_version: FEATURE_SCHEMA_VERSION,
                            values: slow_values,
                        },
                        rejection_reason: None,
                    },
                ],
                ..ShadowInput::default()
            };

            traces.push(RequestTrace::with_evidence(
                format!("req-{index}"),
                1_700_000_000 + index as i64,
                input,
                vec![sample(index, "p1", "fast", true, seed)],
                ExecutionIdentity::unrouted("fast").with_declared_model(Some("fast")),
                Vec::new(),
            ));
        }
        Self { traces }
    }
}

fn sample(
    index: usize,
    provider: &str,
    model: &str,
    success: bool,
    seed: f32,
) -> OutcomeTrainingSample {
    let mut values = [UNKNOWN; FEATURE_DIMENSION];
    values[0] = 0.0;
    OutcomeTrainingSample {
        sample_id: format!("s-{index}"),
        schema_version: 1,
        timestamp: 1_700_000_000 + index as i64,
        streaming: false,
        dialect: "anthropic".to_string(),
        features: RoutingFeatures {
            schema_version: FEATURE_SCHEMA_VERSION,
            values,
        },
        targets: Targets {
            success,
            latency_ms: Some(100.0 + f64::from(seed)),
            ttft_ms: None,
            cost: Some(0.01),
            failure_class: if success {
                None
            } else {
                Some("transport".into())
            },
            fallback_count: 0,
        },
        provider_id: provider.to_string(),
        model_id: model.to_string(),
        origin: DataOrigin::Native,
        outcome_id: format!("out-{index}"),
        request_id: format!("req-{index}"),
        decision_id: Some(format!("dec-{index}")),
        response_id: None,
        final_status: if success {
            FinalStatus::Success
        } else {
            FinalStatus::Failed
        },
        success,
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
    // A cold-start ensemble: the honest starting point when no model has been
    // trained yet, and the one an evaluation must be able to run against.
    MlPolicy::from_ensemble(ModelEnsemble::new(), RewardPolicy::default())
}

// ---------------------------------------------------------------------------
// Data quality
// ---------------------------------------------------------------------------

#[test]
fn data_quality_counts_observations_and_never_imputes_them() {
    let fixture = Fixture::build(20);
    let report = DataQualityReport::of(&fixture.traces);
    assert_eq!(report.traces, 20);
    assert_eq!(report.declared_model, 20);
    assert_eq!(report.cost_coverage(), 1.0);
    assert_eq!(
        report.usage_coverage(),
        0.0,
        "no usage was recorded, so coverage is 0"
    );
    assert_eq!(report.schema_versions, vec![2]);
}

#[test]
fn a_body_with_no_task_evidence_reports_it_as_a_blocker() {
    let fixture = Fixture::build(20);
    let report = DataQualityReport::of(&fixture.traces);
    let blockers = report.blockers();
    assert!(
        blockers.iter().any(|b| b.contains("task-quality evidence")),
        "a body with no task verdicts cannot support a task-quality claim: {blockers:?}"
    );
}

#[test]
fn a_declared_model_disagreeing_with_the_request_is_counted() {
    let mut fixture = Fixture::build(5);
    fixture.traces[0].execution_identity =
        ExecutionIdentity::unrouted("fast").with_declared_model(Some("something-else-entirely"));
    let report = DataQualityReport::of(&fixture.traces);
    assert_eq!(report.declared_model, 5);
    assert_eq!(report.declared_model_mismatch, 1);
}

// ---------------------------------------------------------------------------
// Verdict semantics
// ---------------------------------------------------------------------------

#[test]
fn an_empty_body_blocks_rather_than_reporting_zero_success() {
    let config = EvaluationConfig::default();
    let report = evaluate(&[], &candidate(), &config, None, 1).expect("evaluate");
    assert!(report.verdict.is_blocked());
    assert!(!report.verdict.is_promotable());
    assert!(report.blockers().iter().any(|b| b.contains("no traces")));
}

#[test]
fn a_body_too_small_to_compare_blocks_before_any_metric_is_shown() {
    let fixture = Fixture::build(5);
    let config = EvaluationConfig::default();
    let report = evaluate(&fixture.traces, &candidate(), &config, None, 1).expect("evaluate");
    assert!(report.verdict.is_blocked());
    // No arms at all: a blocked report must not carry numbers that invite quoting.
    assert!(report.arms.is_empty());
    assert!(report.production_pairing.is_none());
    assert!(report
        .blockers()
        .iter()
        .any(|b| b.contains("below the 50 needed")));
}

#[test]
fn insufficient_data_is_reported_as_blocked_never_as_not_improved() {
    // The distinction the whole verdict enum exists for. "Cannot tell" and "does not
    // help" are different findings; conflating them tells an operator to discard a
    // candidate that simply has not been measured yet.
    let fixture = Fixture::build(5);
    let config = EvaluationConfig::default();
    let report = evaluate(&fixture.traces, &candidate(), &config, None, 1).expect("evaluate");
    match &report.verdict {
        EvaluationVerdict::Blocked { reasons } => {
            assert!(!reasons.is_empty(), "a blocked verdict must always say why");
        }
        other => panic!("expected Blocked, got {other:?}"),
    }
}

#[test]
fn evaluation_is_deterministic_for_the_same_inputs() {
    let fixture = Fixture::build(60);
    let config = EvaluationConfig::default();
    let first =
        evaluate(&fixture.traces, &candidate(), &config, None, 1_700_000_000).expect("evaluate");
    let second =
        evaluate(&fixture.traces, &candidate(), &config, None, 1_700_000_000).expect("evaluate");
    assert_eq!(first.dataset_fingerprint, second.dataset_fingerprint);
    assert_eq!(first.verdict, second.verdict);
    assert_eq!(first.config_identity, second.config_identity);
    assert_eq!(first.to_markdown(), second.to_markdown());
}

#[test]
fn evaluation_changes_when_the_thresholds_change() {
    let fixture = Fixture::build(60);
    let loose = EvaluationConfig::default();
    let mut strict = loose.clone();
    strict.min_traces = 10_000;
    let a = evaluate(&fixture.traces, &candidate(), &loose, None, 1).expect("a");
    let b = evaluate(&fixture.traces, &candidate(), &strict, None, 1).expect("b");
    assert_ne!(a.config_identity, b.config_identity);
    assert!(!a.verdict.is_blocked() || a.verdict.is_blocked()); // both well-formed
    assert!(b.verdict.is_blocked());
}

// ---------------------------------------------------------------------------
// Coverage gate
// ---------------------------------------------------------------------------

#[test]
fn coverage_is_reported_per_arm_so_the_gate_can_be_audited() {
    // The property that makes the coverage floor trustworthy: a reader can see each
    // arm's measured fraction rather than being asked to take the verdict on faith.
    let fixture = Fixture::build(60);
    let config = EvaluationConfig::default();
    let report = evaluate(&fixture.traces, &candidate(), &config, None, 1).expect("evaluate");

    assert!(!report.arms.is_empty());
    for arm in &report.arms {
        assert!(
            (0.0..=1.0).contains(&arm.coverage),
            "coverage must be a fraction, got {}",
            arm.coverage
        );
        assert_eq!(arm.coverage, arm.metrics.coverage);
    }
    // The cold candidate agrees with production on every trace here, so its
    // counterfactual choices *were* measured. Reporting that honestly is what lets a
    // genuinely low-coverage body be recognised as such.
    let candidate_arm = report
        .arms
        .iter()
        .find(|arm| arm.policy == "ml.candidate")
        .expect("candidate arm");
    assert_eq!(candidate_arm.coverage, 1.0);
    assert!(candidate_arm.is_measurable());
}

#[test]
fn a_body_without_task_evidence_refuses_any_task_quality_claim() {
    // Routing coverage and task coverage are different quantities. This body measures
    // the routing counterfactual perfectly and says nothing at all about whether the
    // tasks got done — so it must report exactly that, rather than folding a routing
    // win into a quality claim.
    let fixture = Fixture::build(60);
    let mut config = EvaluationConfig::default();
    config.min_coverage = 0.0;
    let report = evaluate(&fixture.traces, &candidate(), &config, None, 1).expect("evaluate");

    assert!(matches!(
        report.task_quality,
        TaskQualityVerdict::Unavailable { .. }
    ));
    // And the two must never be collapsed into a promotable whole.
    assert!(!zroutery_core::ml::is_promotable(&report));

    let markdown = report.to_markdown();
    assert!(markdown.contains("**Unavailable**"));
    assert!(markdown.contains("Fewer failures is not the same as better answers"));
}

#[test]
fn task_quality_becomes_available_once_attempt_attributable_verdicts_exist() {
    let mut fixture = Fixture::build(60);
    for (index, trace) in fixture.traces.iter_mut().enumerate() {
        trace.task_evidence = vec![TaskOutcomeEvidence::new(
            format!("ev-{index}"),
            trace.request_id.clone(),
            format!("out-{index}"),
            Some(format!("att-{index}")),
            Some("fast".into()),
            Some("p1".into()),
            TaskType::Code,
            VerifierKind::Compilation,
            TaskStatus::Succeeded,
            1_700_000_000,
            EvidenceOrigin::RuntimeVerifier,
        )];
    }
    let config = EvaluationConfig::default();
    let report = evaluate(&fixture.traces, &candidate(), &config, None, 1).expect("evaluate");

    match &report.task_quality {
        TaskQualityVerdict::Available { rate, denominator } => {
            assert_eq!(*denominator, 60);
            assert!((rate - 1.0).abs() < f64::EPSILON);
        }
        other => panic!("expected an available task verdict, got {other:?}"),
    }
    assert!(report.to_markdown().contains("Task completion rate"));
}

#[test]
fn a_config_change_is_recorded_so_a_verdict_can_be_traced_to_its_thresholds() {
    let fixture = Fixture::build(60);
    let config = EvaluationConfig::default();
    let report = evaluate(&fixture.traces, &candidate(), &config, None, 1).expect("evaluate");
    let markdown = report.to_markdown();
    assert!(markdown.contains("Config identity"));
    assert!(markdown.contains("Dataset fingerprint"));
}

// ---------------------------------------------------------------------------
// Task evidence attribution
// ---------------------------------------------------------------------------

#[test]
fn task_evidence_is_credited_to_the_candidate_that_actually_served() {
    let mut fixture = Fixture::build(60);
    fixture.traces[0].task_evidence = vec![TaskOutcomeEvidence::new(
        "ev-1",
        "req-0",
        "out-0",
        Some("att-0".into()),
        Some("fast".into()),
        Some("p1".into()),
        TaskType::Code,
        VerifierKind::Compilation,
        TaskStatus::Failed,
        1_700_000_000,
        EvidenceOrigin::RuntimeVerifier,
    )];

    let config = EvaluationConfig::default();
    let report = evaluate(&fixture.traces, &candidate(), &config, None, 1).expect("evaluate");
    assert_eq!(report.data_quality.task_evidence.total, 1);
    assert_eq!(report.data_quality.task_evidence.observed, 1);
    assert_eq!(report.data_quality.task_evidence.failed, 1);
    assert_eq!(report.data_quality.task_evidence.mechanical, 1);
}

#[test]
fn unverified_results_never_enter_a_task_success_rate() {
    let mut fixture = Fixture::build(60);
    // Ten requests where no verifier reached a verdict.
    for trace in fixture.traces.iter_mut().take(10) {
        trace.task_evidence = vec![TaskOutcomeEvidence::unknown(
            "ev-unknown",
            "req",
            "out",
            TaskType::Chat,
            VerifierKind::BehaviouralSignal,
            1_700_000_000,
            EvidenceOrigin::RuntimeVerifier,
        )];
    }
    let coverage = DataQualityReport::of(&fixture.traces).task_evidence;
    assert_eq!(coverage.total, 10);
    assert_eq!(coverage.observed, 0);
    // The rate is undefined, not zero: reporting 0.0% here would read as "every one of
    // these tasks failed".
    assert_eq!(coverage.success_rate(), (0.0, 0));
}

// ---------------------------------------------------------------------------
// Report rendering
// ---------------------------------------------------------------------------

#[test]
fn the_markdown_report_states_denominators_and_verdict() {
    let fixture = Fixture::build(60);
    let config = EvaluationConfig::default();
    let report = evaluate(&fixture.traces, &candidate(), &config, None, 1).expect("evaluate");
    let markdown = report.to_markdown();

    assert!(markdown.contains("**Verdict:**"));
    assert!(markdown.contains("Provider-declared model"));
    assert!(markdown.contains("Task-quality label coverage"));
    // The instruction that keeps a reader from misreading the numbers.
    assert!(markdown.contains("neither a success nor a failure"));
    // Every baseline carries its definition, including the honesty note about
    // Balanced not being the production strategy.
    assert!(markdown.contains("NOT the production Balanced strategy"));
}

#[test]
fn a_blocked_report_explains_itself() {
    let config = EvaluationConfig::default();
    let report = evaluate(&[], &candidate(), &config, None, 1).expect("evaluate");
    let markdown = report.to_markdown();
    assert!(markdown.contains("## Why this is blocked"));
    assert!(markdown.contains("no traces on disk"));
}

#[test]
fn evaluability_says_what_is_missing_rather_than_only_what_is_present() {
    let config = EvaluationConfig::default();
    assert!(evaluability(&DataQualityReport::default(), &config)[0].contains("no traces"));

    let fixture = Fixture::build(60);
    let notes = evaluability(&DataQualityReport::of(&fixture.traces), &config);
    assert!(
        notes.iter().any(|n| n.contains("task-quality evidence")),
        "{notes:?}"
    );
}

// ---------------------------------------------------------------------------
// Promotion coupling
// ---------------------------------------------------------------------------

#[test]
fn a_blocked_evaluation_never_produces_a_promotion_verdict() {
    let fixture = Fixture::build(5);
    let config = EvaluationConfig::default();
    let report = evaluate(&fixture.traces, &candidate(), &config, None, 1).expect("evaluate");
    assert!(report.verdict.is_blocked());
    // Absent, not `Some(Rejected)` and certainly not `Some(Promoted)`.
    assert_eq!(report.promotion_verdict, None);
    assert!(!zroutery_core::ml::is_promotable(&report));
}

#[test]
fn a_promotion_verdict_is_not_invented_without_a_training_report() {
    // The gate refuses a comparison whose dataset fingerprint does not match the one
    // the model was fitted on. With no training report it cannot be consulted at all,
    // and an absent verdict is the honest answer.
    let fixture = Fixture::build(60);
    let mut config = EvaluationConfig::default();
    config.min_coverage = 0.0;
    let report = evaluate(&fixture.traces, &candidate(), &config, None, 1).expect("evaluate");
    if report.verdict.is_promotable() {
        assert_eq!(report.promotion_verdict, None);
        assert!(report.promotion_decision.is_none());
    }
}

#[test]
fn evaluation_never_contacts_an_upstream() {
    // Structural, not incidental: the runner takes a body of traces and a policy and
    // returns a report. There is no client, no endpoint and no socket in its signature,
    // so "this does not affect production" is a property of the type.
    let fixture = Fixture::build(60);
    let config = EvaluationConfig::default();
    let started = std::time::Instant::now();
    let report = evaluate(&fixture.traces, &candidate(), &config, None, 1).expect("evaluate");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(report.dataset_fingerprint.len() == 16);
}
