use std::cmp::Ordering;

use super::reward::{holdout_pairs_only_differ_on_success, sigmoid, softplus, Pair, Row};
use super::safety::insufficient_reason;
use super::selection::{assemble_arms, reject_degenerate_reward, seeded_hash};
use super::*;
use crate::ml::dataset::{Targets, TrainingSample as DatasetTrainingSample};
use crate::ml::evaluation::RoutingMetrics;
use crate::ml::features::FEATURE_SCHEMA_VERSION;
use crate::ml::reward::RewardPolicy;

fn policy(success: f64, latency: f64, cost: f64, fallback: f64) -> RewardPolicy {
    RewardPolicy {
        success_weight: success,
        latency_weight: latency,
        cost_weight: cost,
        fallback_penalty: fallback,
        switch_cost: -0.2,
        uncertainty_weight: 0.1,
    }
}

fn targets(
    success: bool,
    latency_ms: Option<f64>,
    cost: Option<f64>,
    fallback_count: u32,
) -> Targets {
    Targets {
        success,
        latency_ms,
        ttft_ms: success.then_some(10.0),
        cost,
        failure_class: (!success).then(|| "Test".to_string()),
        fallback_count,
    }
}

// -- The basis is the accepted score, exactly --

#[test]
fn basis_reproduces_accepted_score() {
    let cases = [
        (true, Some(120.0), Some(0.02), 0u32),
        (true, Some(999.0), Some(1.0), 2),
        (true, None, None, 0),
        (false, None, Some(0.5), 1),
        (false, None, None, 3),
        (true, Some(0.0), Some(0.0), 0),
    ];
    let policies = [
        RewardPolicy::default(),
        policy(2.0, 0.7, 0.4, -1.5),
        policy(0.0, 0.0, 0.0, 0.0),
    ];
    for (success, latency, cost, fallback) in cases {
        let label = targets(success, latency, cost, fallback);
        let proxy = OutcomeProxy::from_targets(&label);
        for switch_count in [0u32, 1, 3] {
            for confidence in [0.0f64, 0.5, 1.0] {
                let basis = RewardBasis::from_targets(&label, switch_count, confidence);
                for candidate in &policies {
                    assert_eq!(
                        basis.score(candidate),
                        accepted_outcome_proxy_score(&proxy, switch_count, confidence, candidate),
                        "basis must equal the accepted score for \
                             success={success} latency={latency:?} cost={cost:?} \
                             fallback={fallback} switches={switch_count} \
                             confidence={confidence} policy={candidate:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn weights_vector_round_trips_through_a_policy() {
    let original = policy(1.5, 0.25, 0.75, -0.5);
    // `RewardPolicy` is not `PartialEq`, so the round trip is checked field by
    // field rather than by adding a derive to an accepted type.
    let restored = policy_from_weights(weights_vector(&original));
    assert!((restored.success_weight - original.success_weight).abs() < f64::EPSILON);
    assert!((restored.latency_weight - original.latency_weight).abs() < f64::EPSILON);
    assert!((restored.cost_weight - original.cost_weight).abs() < f64::EPSILON);
    assert!((restored.fallback_penalty - original.fallback_penalty).abs() < f64::EPSILON);
    assert!((restored.switch_cost - original.switch_cost).abs() < f64::EPSILON);
    assert!((restored.uncertainty_weight - original.uncertainty_weight).abs() < f64::EPSILON);
}

// -- The declared order --

#[test]
fn outcome_order_puts_success_first() {
    let success = OutcomeProxy {
        success: true,
        latency_ms: Some(900.0),
        cost: Some(1.0),
        fallback_count: 3,
    };
    let failure = OutcomeProxy {
        success: false,
        latency_ms: None,
        cost: Some(0.0),
        fallback_count: 0,
    };
    assert_eq!(compare_outcome_proxy(&success, &failure), Ordering::Less);
    assert_eq!(compare_outcome_proxy(&failure, &success), Ordering::Greater);
}

#[test]
fn outcome_order_ranks_unmeasured_last_and_ties_cleanly() {
    let measured = OutcomeProxy {
        success: true,
        latency_ms: Some(900.0),
        cost: Some(0.5),
        fallback_count: 1,
    };
    let unmeasured = OutcomeProxy {
        success: true,
        latency_ms: None,
        cost: None,
        fallback_count: 1,
    };
    assert_eq!(
        compare_outcome_proxy(&measured, &unmeasured),
        Ordering::Less,
        "an unmeasured latency must rank after every measured value"
    );
    assert_eq!(
        compare_outcome_proxy(&measured, &measured),
        Ordering::Equal,
        "identical outcomes must tie, not be invented apart"
    );
}

// -- Numeric stability --

#[test]
fn softplus_and_sigmoid_survive_extreme_inputs() {
    for z in [-1e12, -700.0, -1.0, 0.0, 1.0, 700.0, 1e12] {
        let value = softplus(z);
        assert!(
            value.is_finite(),
            "softplus({z}) must be finite, got {value}"
        );
        let probability = sigmoid(z);
        assert!(
            probability.is_finite() && (0.0..=1.0).contains(&probability),
            "sigmoid({z}) must be finite and in [0,1], got {probability}"
        );
    }
    // `softplus(0) = ln 2`, not 0: the function is the logistic loss, and the
    // only property under test is that it stays finite at both extremes.
    assert!((softplus(0.0) - std::f64::consts::LN_2).abs() < 1e-12);
    assert!((sigmoid(0.0) - 0.5).abs() < 1e-12);
}

// -- Percentile matches the accepted evaluator --

#[test]
fn percentile_matches_accepted_evaluator() {
    let latencies: Vec<f64> = (0..40).map(|index| 10.0 + f64::from(index) * 7.5).collect();
    let mut sorted = latencies.clone();
    sorted.sort_by(f64::total_cmp);
    let computed = percentile(&sorted, DEFAULT_TAIL_PERCENTILE);

    let samples: Vec<DatasetTrainingSample> = latencies
        .iter()
        .enumerate()
        .map(|(index, latency)| DatasetTrainingSample {
            sample_id: format!("s{index}"),
            schema_version: FEATURE_SCHEMA_VERSION,
            timestamp: index as i64,
            features: super::super::features::RoutingFeatures::default(),
            targets: targets(true, Some(*latency), Some(0.01), 0),
            provider_id: "p".to_string(),
            model_id: "m".to_string(),
            origin: crate::feedback::DataOrigin::Native,
            outcome_id: format!("o{index}"),
            feedback: Vec::new(),
        })
        .collect();
    let accepted = RoutingMetrics::from_samples(&samples);
    assert!(
        (computed - accepted.p95_latency_ms).abs() < 1e-9,
        "restated percentile ({computed}) must match the accepted evaluator ({})",
        accepted.p95_latency_ms
    );
}

#[test]
fn percentile_of_empty_and_singleton() {
    assert!(percentile(&[], 0.95).abs() < f64::EPSILON);
    assert!((percentile(&[42.0], 0.95) - 42.0).abs() < f64::EPSILON);
}

// -- Degeneracy and configuration --

#[test]
fn degenerate_reward_is_refused() {
    let rows: Vec<Row> = (0..4)
        .map(|index| {
            let label = targets(true, Some(100.0), Some(0.01), 0);
            Row {
                sample_id: format!("s{index}"),
                cell: "c".to_string(),
                proxy: OutcomeProxy::from_targets(&label),
                basis: RewardBasis::from_targets(&label, 0, 1.0),
                legacy: DatasetTrainingSample {
                    sample_id: format!("s{index}"),
                    schema_version: FEATURE_SCHEMA_VERSION,
                    timestamp: 0,
                    features: super::super::features::RoutingFeatures::default(),
                    targets: label,
                    provider_id: "p".to_string(),
                    model_id: "m".to_string(),
                    origin: crate::feedback::DataOrigin::Native,
                    outcome_id: format!("o{index}"),
                    feedback: Vec::new(),
                },
            }
        })
        .collect();
    assert!(matches!(
        reject_degenerate_reward(&rows),
        Err(BanditError::DegenerateReward { rows: 4 })
    ));
}

#[test]
fn meaningless_configurations_are_refused() {
    let base = BanditConfig::default();
    assert!(base.checked().is_ok());

    let mut config = base.clone();
    config.reward.holdout_cells = 0;
    assert!(matches!(
        config.checked(),
        Err(BanditError::EmptyHoldoutCells)
    ));

    let mut config = base.clone();
    config.reward.iterations = 0;
    assert!(matches!(
        config.checked(),
        Err(BanditError::EmptyFitIterations)
    ));

    let mut config = base.clone();
    config.reward.learning_rate = f64::INFINITY;
    assert!(matches!(
        config.checked(),
        Err(BanditError::MeaninglessLearningRate(_))
    ));

    let mut config = base.clone();
    config.reward.ridge_lambda = -1.0;
    assert!(matches!(
        config.checked(),
        Err(BanditError::MeaninglessRidgeLambda(_))
    ));

    let mut config = base.clone();
    config.safety.min_reward_improvement = 0.0;
    assert!(matches!(
        config.checked(),
        Err(BanditError::MeaninglessSafetyFloor(_))
    ));

    let mut config = base.clone();
    config.safety.min_reward_improvement = f64::NAN;
    assert!(matches!(
        config.checked(),
        Err(BanditError::MeaninglessSafetyFloor(_))
    ));

    let mut config = base.clone();
    config.safety.min_evaluation_samples = 1;
    assert!(matches!(
        config.checked(),
        Err(BanditError::MeaninglessEvaluationFloor(1))
    ));

    // An infinite tolerance would make one of the three required gates
    // incapable of failing, which is the exact failure mode this node exists
    // to prevent.
    let mut config = base.clone();
    config.safety.max_failure_rate_regression = f64::INFINITY;
    assert!(matches!(
        config.checked(),
        Err(BanditError::MeaninglessTolerance {
            name: "max_failure_rate_regression",
            ..
        })
    ));

    let mut config = base.clone();
    config.safety.max_cost_regression = f64::NAN;
    assert!(matches!(
        config.checked(),
        Err(BanditError::MeaninglessTolerance { .. })
    ));

    let mut config = base.clone();
    config.safety.max_tail_latency_regression_ms = -1.0;
    assert!(matches!(
        config.checked(),
        Err(BanditError::NegativeTolerance { .. })
    ));

    let mut config = base.clone();
    config.selection.exploration_c = f64::NEG_INFINITY;
    assert!(matches!(
        config.checked(),
        Err(BanditError::MeaninglessExploration(_))
    ));

    let mut config = base.clone();
    config.selection.min_arm_observations = 0;
    assert!(matches!(
        config.checked(),
        Err(BanditError::EmptyArmObservationFloor)
    ));
}

// -- The seeded assignment is reproducible and seed-dependent --

#[test]
fn seeded_assignment_is_deterministic_and_seed_sensitive() {
    let ids = ["a", "b", "c", "d", "e", "f", "g", "h"];
    let first: Vec<usize> = ids
        .iter()
        .map(|id| (seeded_hash(7, id) % 3) as usize)
        .collect();
    let second: Vec<usize> = ids
        .iter()
        .map(|id| (seeded_hash(7, id) % 3) as usize)
        .collect();
    assert_eq!(first, second, "the same seed must assign identically");
    let other: Vec<usize> = ids
        .iter()
        .map(|id| (seeded_hash(8, id) % 3) as usize)
        .collect();
    assert_ne!(
        first, other,
        "a different seed must explore a different schedule"
    );
}

#[test]
fn a_success_only_holdout_cannot_carry_an_improvement_claim() {
    // Pairs that all differ on the success column: `success_weight` alone
    // explains every one of them, so a better score is a fact about the base
    // rate and not about the learned weights.
    let pairs = vec![
        Pair {
            difference: [2.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            label: 1.0,
        },
        Pair {
            difference: [2.0, 0.1, 0.0, 0.0, 0.0, 0.0],
            label: 1.0,
        },
    ];
    assert!(holdout_pairs_only_differ_on_success(&pairs));
    assert_eq!(
        RewardFitVerdict::Better.downgraded_to_not_better(),
        RewardFitVerdict::NotBetter
    );
    assert_eq!(
        RewardFitVerdict::Worse.downgraded_to_not_better(),
        RewardFitVerdict::Worse,
        "a fit that is confidently wrong is still worse"
    );

    // One pair that shares a success label is enough to make the holdout able
    // to discriminate, and the claim is no longer suppressed.
    let mixed = vec![
        pairs[0],
        Pair {
            difference: [0.0, 0.2, 0.0, 0.0, 0.0, 0.0],
            label: 1.0,
        },
    ];
    assert!(!holdout_pairs_only_differ_on_success(&mixed));
    assert!(!holdout_pairs_only_differ_on_success(&[]));
}

#[test]
fn the_fit_can_report_a_better_reward() {
    // The guard above suppresses a claim on a degenerate holdout only. A fit
    // that genuinely reproduces the declared order better must be able to say
    // so, or the verdict would be decoration.
    assert!(RewardFitVerdict::Better.is_improvement());
    assert!(!RewardFitVerdict::NotBetter.is_improvement());
    assert!(!RewardFitVerdict::Worse.is_improvement());
}

// -- The safety verdict is recomputed, never a stored flag --

#[test]
fn violations_are_recomputed_from_deltas_and_tolerances() {
    let evaluation = SafetyEvaluation {
        reference: "r".to_string(),
        candidate: "c".to_string(),
        evaluation_samples: 100,
        reference_metrics: metrics_stub("r"),
        candidate_metrics: metrics_stub("c"),
        mean_reward_delta: 0.5,
        failure_rate_delta: 0.02,
        mean_cost_delta: -0.001,
        tail_latency_delta_ms: -5.0,
        fallback_rate_delta: 0.0,
        tolerances: SafetyTolerances {
            min_reward_improvement: 1e-9,
            max_failure_rate_regression: 0.0,
            max_cost_regression: 0.0,
            max_tail_latency_regression_ms: 0.0,
            max_fallback_rate_regression: 0.0,
        },
        insufficient_reason: None,
    };

    // Mean reward improved and cost and tail improved, but the failure rate
    // regressed: the whole point of the gate.
    let violations = evaluation.violations();
    assert_eq!(violations.len(), 1, "only the failure rate regressed");
    assert!(matches!(
        violations[0],
        SafetyViolation::FailureRateRegressed { .. }
    ));
    assert_eq!(evaluation.verdict(), SafetyVerdict::Reject);
    assert!(!evaluation.verdict().is_acceptance());
}

#[test]
fn equal_reward_is_not_an_improvement() {
    let mut evaluation = SafetyEvaluation {
        reference: "r".to_string(),
        candidate: "c".to_string(),
        evaluation_samples: 100,
        reference_metrics: metrics_stub("r"),
        candidate_metrics: metrics_stub("c"),
        mean_reward_delta: 0.0,
        failure_rate_delta: 0.0,
        mean_cost_delta: 0.0,
        tail_latency_delta_ms: 0.0,
        fallback_rate_delta: 0.0,
        tolerances: SafetyTolerances {
            min_reward_improvement: 1e-9,
            max_failure_rate_regression: 0.0,
            max_cost_regression: 0.0,
            max_tail_latency_regression_ms: 0.0,
            max_fallback_rate_regression: 0.0,
        },
        insufficient_reason: None,
    };
    assert_eq!(evaluation.verdict(), SafetyVerdict::Reject);
    assert!(matches!(
        evaluation.violations()[0],
        SafetyViolation::NoRewardImprovement { .. }
    ));

    evaluation.tolerances.min_reward_improvement = 0.1;
    assert!(
        !evaluation.violations().is_empty(),
        "raising the floor is stricter, not looser: a zero improvement still fails it"
    );
    assert_eq!(evaluation.verdict(), SafetyVerdict::Reject);

    evaluation.mean_reward_delta = 0.5;
    assert!(
        evaluation.violations().is_empty(),
        "an improvement past the floor clears it"
    );
    assert_eq!(evaluation.verdict(), SafetyVerdict::Accept);
}

#[test]
fn insufficient_evidence_outranks_a_clean_comparison() {
    let evaluation = SafetyEvaluation {
        reference: "r".to_string(),
        candidate: "c".to_string(),
        evaluation_samples: 100,
        reference_metrics: metrics_stub("r"),
        candidate_metrics: metrics_stub("c"),
        mean_reward_delta: 10.0,
        failure_rate_delta: -0.5,
        mean_cost_delta: -0.5,
        tail_latency_delta_ms: -500.0,
        fallback_rate_delta: -0.5,
        tolerances: SafetyTolerances {
            min_reward_improvement: 1e-9,
            max_failure_rate_regression: 0.0,
            max_cost_regression: 0.0,
            max_tail_latency_regression_ms: 0.0,
            max_fallback_rate_regression: 0.0,
        },
        insufficient_reason: Some("the evaluation partition holds no failure".to_string()),
    };
    assert_eq!(evaluation.verdict(), SafetyVerdict::InsufficientEvidence);
    assert!(!evaluation.verdict().is_acceptance());
    assert!(
        evaluation.violations().is_empty(),
        "no violation was measured, and the verdict is still not an acceptance"
    );
}

#[test]
fn insufficient_reason_names_every_gap() {
    let config = SafetyConfig::default();
    let mut evidence = SafetyEvidence {
        total: 100,
        successes: 50,
        failures: 50,
        cost_observations: 50,
        latency_observations: 50,
        fallback_observations: 0,
        feedback_signals_present: 0,
    };
    assert_eq!(insufficient_reason(&evidence, &config), None);

    evidence.failures = 0;
    assert!(insufficient_reason(&evidence, &config)
        .unwrap()
        .contains("no failure"));

    evidence.failures = 50;
    evidence.successes = 0;
    assert!(insufficient_reason(&evidence, &config)
        .unwrap()
        .contains("no success"));

    evidence.successes = 50;
    evidence.cost_observations = 0;
    assert!(insufficient_reason(&evidence, &config)
        .unwrap()
        .contains("no cost measurement"));

    evidence.cost_observations = 50;
    evidence.latency_observations = 1;
    assert!(insufficient_reason(&evidence, &config)
        .unwrap()
        .contains("tail latency"));

    evidence.latency_observations = 50;
    evidence.total = 1;
    assert!(insufficient_reason(&evidence, &config)
        .unwrap()
        .contains("safety floor"));
}

#[test]
fn arm_names_are_validated() {
    let fitted = RewardPolicy::default();
    let safety = SafetyConfig::default();
    assert!(matches!(
        assemble_arms(&[], fitted.clone(), &safety),
        Err(BanditError::NoArms)
    ));
    assert!(matches!(
        assemble_arms(
            &[RewardArm::new("  ", fitted.clone())],
            fitted.clone(),
            &safety
        ),
        Err(BanditError::EmptyArmName { index: 0 })
    ));
    assert!(matches!(
        assemble_arms(
            &[RewardArm::new(FITTED_ARM_NAME, fitted.clone())],
            fitted.clone(),
            &safety
        ),
        Err(BanditError::ReservedArmName { .. })
    ));
    assert!(matches!(
        assemble_arms(
            &[
                RewardArm::new("dup", fitted.clone()),
                RewardArm::new("dup", fitted.clone())
            ],
            fitted.clone(),
            &safety
        ),
        Err(BanditError::DuplicateArmName { .. })
    ));
    assert!(matches!(
        assemble_arms(
            &[RewardArm::new("only", fitted.clone())],
            fitted.clone(),
            &safety
        ),
        Err(BanditError::ReferenceArmUnknown { .. })
    ));

    let arms = assemble_arms(&[RewardArm::accepted_prior()], fitted, &safety)
        .expect("a prior arm plus the fitted arm is a valid set");
    assert_eq!(arms.len(), 2);
    assert_eq!(arms[0].name, ACCEPTED_PRIOR_ARM_NAME);
    assert_eq!(arms[1].name, FITTED_ARM_NAME);
}

fn metrics_stub(arm: &str) -> ArmSafetyMetrics {
    ArmSafetyMetrics {
        arm: arm.to_string(),
        mean_outcome_proxy_reward: 0.0,
        failure_rate: 0.0,
        mean_cost: 0.0,
        tail_latency_ms: 0.0,
        tail_percentile: DEFAULT_TAIL_PERCENTILE,
        fallback_rate: 0.0,
        accepted_metrics: RoutingMetrics::default(),
    }
}
