#![cfg(feature = "ml")]

//! Stage 7D — Evaluation framework verification tests.
//!
//! Tests cover:
//! 1. Attempt attribution (samples_from_outcome)
//! 2. Evaluator (model quality + comparison reports)
//! 3. Prediction metrics (log_loss)
//! 4. Routing metrics (from_samples)
//! 5. Recommendation logic (comparison thresholds)

use zroutery_core::failure::FailureClass;
use zroutery_core::feedback::DataOrigin;
use zroutery_core::ml::dataset::{samples_from_outcome, Targets, TrainingSample};
use zroutery_core::ml::evaluation::{Evaluator, PredictionMetrics, Recommendation, RoutingMetrics};
use zroutery_core::ml::features::{RoutingFeatures, FEATURE_DIMENSION};
use zroutery_core::ml::model::{RoutingModel, SuccessModel};
use zroutery_core::outcome::{Attempt, Outcome};

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
    }
}

fn make_sample(
    success: bool,
    latency_ms: Option<f64>,
    cost: Option<f64>,
    fallback_count: u32,
) -> TrainingSample {
    TrainingSample {
        sample_id: format!("test-{}", uuid::Uuid::new_v4().simple()),
        schema_version: 1,
        timestamp: 1_700_000_000,
        features: RoutingFeatures::default(),
        targets: Targets {
            success,
            latency_ms,
            ttft_ms: latency_ms.map(|l| l * 0.3),
            cost,
            failure_class: None,
            fallback_count,
        },
        provider_id: "test".to_string(),
        model_id: "test-model".to_string(),
        origin: DataOrigin::Native,
        outcome_id: format!("out-{}", uuid::Uuid::new_v4().simple()),
        feedback: Vec::new(),
    }
}

fn make_routing_metrics(
    total_requests: usize,
    success_rate: f64,
    p50_latency_ms: f64,
    p95_latency_ms: f64,
    mean_cost: f64,
    fallback_rate: f64,
) -> RoutingMetrics {
    RoutingMetrics {
        total_requests,
        success_rate,
        p50_latency_ms,
        p95_latency_ms,
        mean_cost,
        fallback_rate,
        escalation_rate: 0.0,
    }
}

// ---------------------------------------------------------------------------
// 1. Attempt attribution: Outcome with 2 attempts produces 3 samples
//    (2 attempt + 1 request)
// ---------------------------------------------------------------------------

#[test]
fn attempt_attribution_two_attempts_produces_three_samples() {
    let outcome = Outcome::builder("req_eval_1")
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
        .cost(Some(0.02), Some(0.018))
        .build();

    let features_a = {
        let mut f = RoutingFeatures::default();
        f.values[0] = 1.0;
        f
    };
    let features_b = {
        let mut f = RoutingFeatures::default();
        f.values[0] = 0.5;
        f
    };

    let samples = samples_from_outcome(&outcome, &[features_a, features_b], DataOrigin::Native);

    assert_eq!(samples.len(), 3, "2 attempts + 1 request-level sample = 3");
}

// ---------------------------------------------------------------------------
// 2. Attempt attribution: failed attempt gets success=false
// ---------------------------------------------------------------------------

#[test]
fn attempt_attribution_failed_attempt_has_success_false() {
    let outcome = Outcome::builder("req_eval_2")
        .initial("gpt-4", "openai")
        .final_candidate("claude-3", "anthropic")
        .dialect("openai")
        .attempt(make_attempt(
            "gpt-4",
            "openai",
            false,
            200.0,
            Some(FailureClass::RateLimit),
        ))
        .attempt(make_attempt("claude-3", "anthropic", true, 400.0, None))
        .total_latency_ms(600.0)
        .build();

    let features = vec![RoutingFeatures::default(); 2];
    let samples = samples_from_outcome(&outcome, &features, DataOrigin::Native);

    // First sample: failed attempt
    assert!(
        !samples[0].targets.success,
        "failed attempt sample must have success=false"
    );
    assert!(
        samples[0].targets.latency_ms.is_none(),
        "failed attempt should have no latency"
    );
    assert_eq!(
        samples[0].targets.failure_class.as_deref(),
        Some("RateLimit")
    );
    assert_eq!(samples[0].provider_id, "openai");
    assert_eq!(samples[0].model_id, "gpt-4");

    // Second sample: successful attempt
    assert!(
        samples[1].targets.success,
        "successful attempt sample must have success=true"
    );
    assert_eq!(samples[1].targets.latency_ms, Some(400.0));

    // Third sample: request-level (success, from final candidate)
    assert!(
        samples[2].targets.success,
        "request-level sample reflects overall outcome"
    );
}

// ---------------------------------------------------------------------------
// 3. Attempt attribution: each attempt uses its own features
// ---------------------------------------------------------------------------

#[test]
fn attempt_attribution_each_attempt_uses_own_features() {
    let outcome = Outcome::builder("req_eval_3")
        .initial("gpt-4", "openai")
        .final_candidate("claude-3", "anthropic")
        .dialect("openai")
        .attempt(make_attempt(
            "gpt-4",
            "openai",
            false,
            200.0,
            Some(FailureClass::Transport),
        ))
        .attempt(make_attempt("claude-3", "anthropic", true, 400.0, None))
        .total_latency_ms(600.0)
        .build();

    let mut features_a = RoutingFeatures::default();
    features_a.values[0] = 1.0; // streaming=1.0 for attempt A
    let mut features_b = RoutingFeatures::default();
    features_b.values[0] = 0.0; // streaming=0.0 for attempt B

    let samples = samples_from_outcome(
        &outcome,
        &[features_a.clone(), features_b.clone()],
        DataOrigin::Native,
    );

    // Attempt0 uses features_a
    assert_eq!(
        samples[0].features.values[0], 1.0,
        "attempt 0 should use its own feature snapshot"
    );

    // Attempt1 uses features_b
    assert_eq!(
        samples[1].features.values[0], 0.0,
        "attempt 1 should use its own feature snapshot"
    );

    // Request-level sample uses the last feature snapshot (features_b)
    assert_eq!(
        samples[2].features.values[0], 0.0,
        "request-level sample uses last snapshot"
    );
}

// ---------------------------------------------------------------------------
// 4. Evaluator: known good model scores better than random
// ---------------------------------------------------------------------------

#[test]
fn evaluator_known_good_model_scores_better_than_random() {
    // Features used for both training and sample construction
    let features = {
        let mut f = RoutingFeatures::default();
        for v in f.values.iter_mut() {
            *v = 0.5;
        }
        f
    };

    // Build samples where all succeed, carrying the same features
    let good_samples: Vec<TrainingSample> = (0..100)
        .map(|_| {
            let mut s = make_sample(true, Some(200.0), Some(0.01), 0);
            s.features = features.clone();
            s
        })
        .collect();

    // Train a SuccessModel on the good data (target=1.0, matching features)
    let mut trained_model = SuccessModel::new(FEATURE_DIMENSION);
    for _ in 0..200 {
        trained_model.update(&features, 1.0);
    }

    // Cold model (no training) — predicts ~0.5 for everything
    let cold_model = SuccessModel::new(FEATURE_DIMENSION);

    let trained_metrics = Evaluator::evaluate_predictions(&trained_model, &good_samples);
    let cold_metrics = Evaluator::evaluate_predictions(&cold_model, &good_samples);

    // Trained model should have lower log_loss (better calibration toward 1.0)
    let trained_ll = trained_metrics.log_loss.unwrap();
    let cold_ll = cold_metrics.log_loss.unwrap();

    assert!(
        trained_ll < cold_ll,
        "trained model log_loss ({trained_ll:.4}) should be lower than cold ({cold_ll:.4})"
    );

    // Trained model should predict higher mean (closer to 1.0 since all succeed)
    assert!(
        trained_metrics.mean_prediction > cold_metrics.mean_prediction,
        "trained mean ({:.4}) > cold mean ({:.4})",
        trained_metrics.mean_prediction,
        cold_metrics.mean_prediction
    );
}

// ---------------------------------------------------------------------------
// 5. Evaluator: comparison report correctly identifies improvement
// ---------------------------------------------------------------------------

#[test]
fn evaluator_comparison_report_identifies_improvement() {
    let baseline = make_routing_metrics(100, 0.90, 200.0, 500.0, 0.02, 0.05);
    // Candidate: +5pp success, -20% p95 latency
    let candidate = make_routing_metrics(100, 0.95, 180.0, 400.0, 0.015, 0.03);

    let report = Evaluator::compare_routing(&baseline, &candidate);

    assert_eq!(
        report.recommendation,
        Recommendation::Accept,
        "improved candidate should be accepted"
    );
    assert!(
        report.deltas.success_rate_delta > 0.0,
        "success rate delta should be positive"
    );
    assert!(
        report.deltas.p95_latency_delta_pct < 0.0,
        "p95 latency delta should be negative (improved)"
    );
    assert!(
        !report.reasons.is_empty(),
        "report should include reasons for the decision"
    );
}

// ---------------------------------------------------------------------------
// 6. Evaluator: comparison report correctly identifies regression
// ---------------------------------------------------------------------------

#[test]
fn evaluator_comparison_report_identifies_regression() {
    let baseline = make_routing_metrics(100, 0.95, 200.0, 500.0, 0.02, 0.03);
    // Candidate: -15pp success (significant regression)
    let candidate = make_routing_metrics(100, 0.80, 300.0, 800.0, 0.03, 0.10);

    let report = Evaluator::compare_routing(&baseline, &candidate);

    assert_eq!(
        report.recommendation,
        Recommendation::Reject,
        "regressed candidate should be rejected"
    );
    assert!(
        report.deltas.success_rate_delta < 0.0,
        "success rate delta should be negative (degraded)"
    );
    assert!(
        report
            .reasons
            .iter()
            .any(|r| r.contains("degraded") || r.contains("increased")),
        "report should include degradation reason, got: {:?}",
        report.reasons
    );
}

// ---------------------------------------------------------------------------
// 7. Prediction metrics: perfect predictions -> log_loss near 0
// ---------------------------------------------------------------------------

#[test]
fn prediction_metrics_perfect_predictions_log_loss_near_zero() {
    let predictions = vec![0.999, 0.999, 0.001, 0.001];
    let actuals = vec![true, true, false, false];

    let metrics = PredictionMetrics::compute_classification(&predictions, &actuals);

    let ll = metrics
        .log_loss
        .expect("log_loss should be Some for classification");
    assert!(
        ll < 0.01,
        "perfect predictions should have log_loss near 0, got {ll}"
    );

    let bs = metrics.brier_score.expect("brier_score should be Some");
    assert!(
        bs < 0.01,
        "perfect predictions should have brier_score near 0, got {bs}"
    );
}

// ---------------------------------------------------------------------------
// 8. Prediction metrics: random predictions -> log_loss near 0.69
// ---------------------------------------------------------------------------

#[test]
fn prediction_metrics_random_predictions_log_loss_near_ln2() {
    // All predictions = 0.5 with all-true actuals => log_loss = -ln(0.5) = ln(2) ~ 0.6931
    let predictions = vec![0.5; 20];
    let actuals = vec![true; 20];

    let metrics = PredictionMetrics::compute_classification(&predictions, &actuals);

    let ll = metrics.log_loss.expect("log_loss should be Some");
    let ln2 = std::f64::consts::LN_2;
    assert!(
        (ll - ln2).abs() < 0.01,
        "random predictions (0.5) vs all-true should give log_loss ~ ln(2)={ln2:.4}, got {ll:.4}"
    );

    // Mixed 0.5 predictions vs mixed actuals => log_loss also ~ ln(2)
    let mixed_preds = vec![0.5; 20];
    let mixed_actuals: Vec<bool> = (0..20).map(|i| i % 2 == 0).collect();
    let mixed_metrics = PredictionMetrics::compute_classification(&mixed_preds, &mixed_actuals);
    let mixed_ll = mixed_metrics.log_loss.unwrap();
    assert!(
        (mixed_ll - ln2).abs() < 0.01,
        "random predictions (0.5) vs mixed actuals should also give ~ln(2)={ln2:.4}, got {mixed_ll:.4}"
    );
}

// ---------------------------------------------------------------------------
// 9. Routing metrics: compute from sample outcomes
// ---------------------------------------------------------------------------

#[test]
fn routing_metrics_compute_from_sample_outcomes() {
    let samples = vec![
        make_sample(true, Some(100.0), Some(0.01), 0),
        make_sample(true, Some(200.0), Some(0.02), 0),
        make_sample(true, Some(300.0), Some(0.03), 1), // fallback
        make_sample(false, None, None, 0),
        make_sample(true, Some(150.0), Some(0.015), 0),
    ];

    let metrics = RoutingMetrics::from_samples(&samples);

    assert_eq!(metrics.total_requests, 5);
    // success_rate: 4/5 = 0.8
    assert!(
        (metrics.success_rate - 0.8).abs() < 1e-10,
        "success_rate should be 0.8, got {}",
        metrics.success_rate
    );
    // Latency percentiles should be valid
    assert!(metrics.p50_latency_ms > 0.0, "p50 should be positive");
    assert!(
        metrics.p95_latency_ms >= metrics.p50_latency_ms,
        "p95 >= p50"
    );
    // mean_cost: (0.01+0.02+0.03+0.015)/4 = 0.01875
    assert!(
        (metrics.mean_cost - 0.01875).abs() < 1e-6,
        "mean_cost should be ~0.01875, got {}",
        metrics.mean_cost
    );
    // fallback_rate: 1/5 = 0.2
    assert!(
        (metrics.fallback_rate - 0.2).abs() < 1e-10,
        "fallback_rate should be 0.2, got {}",
        metrics.fallback_rate
    );
    // escalation_rate: 1 success with fallback / 5 = 0.2
    assert!(
        (metrics.escalation_rate - 0.2).abs() < 1e-10,
        "escalation_rate should be 0.2, got {}",
        metrics.escalation_rate
    );
}

// ---------------------------------------------------------------------------
// 10. Recommendation logic: accept when improvement >= 3%, reject otherwise
// ---------------------------------------------------------------------------

#[test]
fn recommendation_accept_when_improvement_at_least_3pp() {
    let baseline = make_routing_metrics(50, 0.90, 200.0, 500.0, 0.02, 0.05);
    // +3pp success rate (0.90 -> 0.93) triggers success_improved (threshold: >= 0.01)
    let candidate = make_routing_metrics(50, 0.93, 200.0, 500.0, 0.02, 0.05);

    let report = Evaluator::compare_routing(&baseline, &candidate);
    assert_eq!(
        report.recommendation,
        Recommendation::Accept,
        "3pp improvement should be accepted"
    );
    assert!(
        report.deltas.success_rate_delta >= 0.03,
        "success_rate_delta should be >= 0.03"
    );
}

#[test]
fn recommendation_reject_when_no_significant_improvement() {
    // Two identical metric sets: no improvement at all
    let baseline = make_routing_metrics(50, 0.90, 200.0, 500.0, 0.02, 0.05);
    let candidate = make_routing_metrics(50, 0.90, 200.0, 500.0, 0.02, 0.05);

    let report = Evaluator::compare_routing(&baseline, &candidate);
    assert_eq!(
        report.recommendation,
        Recommendation::Reject,
        "no improvement should be rejected"
    );
    assert!(
        report.reasons.iter().any(|r| r.contains("no significant")),
        "should explain lack of significant difference"
    );
}

#[test]
fn recommendation_reject_when_below_threshold() {
    let baseline = make_routing_metrics(50, 0.90, 200.0, 500.0, 0.02, 0.05);
    // +0.5pp success rate, +0.5% cost reduction — below all improvement thresholds
    let candidate = make_routing_metrics(50, 0.905, 200.0, 500.0, 0.0199, 0.05);

    let report = Evaluator::compare_routing(&baseline, &candidate);
    // success_improved requires delta >= 0.01 (1pp);0.005 is below threshold
    // cost_improved requires delta < -5%;0.0199 vs 0.02 is -0.5%, well below
    assert_eq!(
        report.recommendation,
        Recommendation::Reject,
        "marginal improvement below all thresholds should be rejected"
    );
}

// ---------------------------------------------------------------------------
// Node 7E-2D: the fallible metric constructors
// ---------------------------------------------------------------------------
//
// The calibration node measures a K-way vector, so it cannot prove its
// probabilities are finite and inside [0, 1] before handing them over. These
// tests pin the fallible twins added for it: the same arithmetic, a typed
// refusal instead of a `NaN` metrics struct, and exact agreement with the
// `assert!`-based constructors on well-formed input.

use zroutery_core::ml::evaluation::EvaluationError;

#[test]
fn try_compute_classification_agrees_with_the_infallible_constructor() {
    let predictions = [1.0, 0.0, 0.5, 0.9, 0.1, 0.7, 0.3, 0.0, 1.0];
    let actuals = [true, false, true, true, false, true, false, false, true];

    let fallible = PredictionMetrics::try_compute_classification(&predictions, &actuals)
        .expect("well-formed input is accepted");
    let infallible = PredictionMetrics::compute_classification(&predictions, &actuals);

    assert_eq!(fallible.sample_count, infallible.sample_count);
    assert_eq!(fallible.mean_prediction, infallible.mean_prediction);
    assert_eq!(fallible.mean_actual, infallible.mean_actual);
    assert_eq!(fallible.log_loss, infallible.log_loss);
    assert_eq!(fallible.brier_score, infallible.brier_score);
    assert_eq!(fallible.mae, infallible.mae);
    assert_eq!(fallible.rmse, infallible.rmse);
}

#[test]
fn try_compute_regression_agrees_with_the_infallible_constructor() {
    let predictions = [10.0, 20.0, 30.0, -5.5];
    let actuals = [12.0, 18.0, 33.0, -5.5];

    let fallible =
        PredictionMetrics::try_compute_regression(&predictions, &actuals).expect("well-formed");
    let infallible = PredictionMetrics::compute_regression(&predictions, &actuals);

    assert_eq!(fallible.sample_count, infallible.sample_count);
    assert_eq!(fallible.mae, infallible.mae);
    assert_eq!(fallible.rmse, infallible.rmse);
    assert_eq!(fallible.mean_prediction, infallible.mean_prediction);
    assert_eq!(fallible.mean_actual, infallible.mean_actual);
    assert_eq!(fallible.log_loss, infallible.log_loss);
    assert_eq!(fallible.brier_score, infallible.brier_score);
}

#[test]
fn try_compute_classification_refuses_rather_than_returning_nan_metrics() {
    assert_eq!(
        PredictionMetrics::try_compute_classification(&[], &[]).unwrap_err(),
        EvaluationError::EmptyObservations
    );
    assert_eq!(
        PredictionMetrics::try_compute_classification(&[0.5], &[true, false]).unwrap_err(),
        EvaluationError::LengthMismatch {
            predictions: 1,
            actuals: 2,
        }
    );
    // Matched rather than compared: `NaN != NaN`, so a `PartialEq` assertion on
    // a refusal that carries a `NaN` would fail for the wrong reason.
    for non_finite in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        match PredictionMetrics::try_compute_classification(&[0.5, non_finite], &[true, false])
            .unwrap_err()
        {
            EvaluationError::NonFinitePrediction { index, value } => {
                assert_eq!(index, 1, "the refusal must name the offending index");
                assert!(value.is_nan() == non_finite.is_nan());
                assert_eq!(value.is_infinite(), non_finite.is_infinite());
            }
            other => panic!("expected a non-finite refusal, got {other:?}"),
        }
    }
    for out_of_range in [1.5_f64, -0.001] {
        assert_eq!(
            PredictionMetrics::try_compute_classification(&[out_of_range], &[true]).unwrap_err(),
            EvaluationError::PredictionOutOfRange {
                index: 0,
                value: out_of_range,
            }
        );
    }

    // A prediction of exactly 0.0 and exactly 1.0 are probabilities, not
    // out-of-range values, and must be accepted.
    assert!(PredictionMetrics::try_compute_classification(&[0.0, 1.0], &[false, true]).is_ok());
}

#[test]
fn try_compute_regression_refuses_a_non_finite_prediction_but_not_a_magnitude() {
    assert_eq!(
        PredictionMetrics::try_compute_regression(&[], &[]).unwrap_err(),
        EvaluationError::EmptyObservations
    );
    assert_eq!(
        PredictionMetrics::try_compute_regression(&[1.0], &[1.0, 2.0]).unwrap_err(),
        EvaluationError::LengthMismatch {
            predictions: 1,
            actuals: 2,
        }
    );
    match PredictionMetrics::try_compute_regression(&[1.0, f64::NEG_INFINITY], &[1.0, 2.0])
        .unwrap_err()
    {
        EvaluationError::NonFiniteRegressionPrediction { index, value } => {
            assert_eq!(index, 1);
            assert!(value.is_infinite() && value.is_sign_negative());
        }
        other => panic!("expected a non-finite refusal, got {other:?}"),
    }
    // Unlike a probability, a regression target has no interval to lie in: a
    // negative latency is a magnitude, and policing a range here would be
    // inventing a constraint the accepted type does not have.
    assert!(PredictionMetrics::try_compute_regression(&[-1.0, 1e9], &[0.0, 0.0]).is_ok());
}

#[test]
fn every_evaluation_error_carries_a_reason() {
    let cases = [
        EvaluationError::EmptyObservations,
        EvaluationError::LengthMismatch {
            predictions: 2,
            actuals: 3,
        },
        EvaluationError::NonFinitePrediction {
            index: 0,
            value: -1.0,
        },
        EvaluationError::PredictionOutOfRange {
            index: 4,
            value: 2.0,
        },
        EvaluationError::NonFiniteRegressionPrediction {
            index: 1,
            value: 1.0e300,
        },
        EvaluationError::NonFiniteRoutingMetric {
            side: "candidate",
            metric: "success_rate",
            value: f64::NAN,
        },
        EvaluationError::RoutingMetricOutOfRange {
            side: "baseline",
            metric: "p95_latency_ms",
            value: -1.0,
            bound: ">= 0",
        },
    ];
    for case in cases {
        let rendered = case.to_string();
        assert!(!rendered.is_empty(), "every refusal must render a reason");
        assert!(
            !rendered.contains("Error {"),
            "the rendered reason must be prose, not a debug dump: {rendered}"
        );
    }
}

// ---------------------------------------------------------------------------
// The routing comparison's validation
// ---------------------------------------------------------------------------
//
// `RoutingMetrics` carries public `f64`s, so a caller can hand the comparison a
// record that is not a measurement at all. Every threshold in the comparison is
// a `<`/`>` test, and a `NaN` reads false for all of them: the success-rate
// degradation test goes quiet and any latency or cost improvement carries an
// `Accept` — while the reported delta is also `NaN`. These tests pin the
// explicit insufficient-evidence answer that replaced that acceptance.

/// A candidate whose named metric is replaced with `value`.
fn candidate_with(metric: &str, value: f64) -> RoutingMetrics {
    let mut candidate = make_routing_metrics(100, 0.995, 200.0, 170.0, 0.02, 0.0);
    match metric {
        "success_rate" => candidate.success_rate = value,
        "fallback_rate" => candidate.fallback_rate = value,
        "escalation_rate" => candidate.escalation_rate = value,
        "p50_latency_ms" => candidate.p50_latency_ms = value,
        "p95_latency_ms" => candidate.p95_latency_ms = value,
        "mean_cost" => candidate.mean_cost = value,
        other => panic!("unknown metric {other}"),
    }
    candidate
}

#[test]
fn compare_routing_refuses_a_candidate_success_rate_that_is_not_a_measurement() {
    // The measured latency improvement is real: p95 200 -> 170 is -15%, inside
    // the latency-improvement branch. Under a comparison that did not validate,
    // it alone produced `Accept` with a `NaN` success-rate delta.
    let baseline = make_routing_metrics(100, 0.995, 200.0, 200.0, 0.02, 0.0);

    for non_finite in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let report =
            Evaluator::compare_routing(&baseline, &candidate_with("success_rate", non_finite));
        assert_eq!(
            report.recommendation,
            Recommendation::InsufficientData,
            "a {non_finite} success rate is not evidence for an acceptance"
        );
        assert!(
            report
                .reasons
                .iter()
                .any(|reason| reason.contains("candidate.success_rate")),
            "the refusal must name the offending field: {:?}",
            report.reasons
        );
        // No delta is reported for a comparison that could not be made, so no
        // `NaN` escapes into a caller's arithmetic or serialization.
        assert_eq!(report.deltas.p95_latency_delta_pct, 0.0);
        assert!(report.deltas.success_rate_delta.is_finite());
    }
}

#[test]
fn compare_routing_refuses_a_baseline_that_is_not_a_measurement() {
    let baseline = make_routing_metrics(100, f64::NAN, 200.0, 200.0, 0.02, 0.0);
    let candidate = make_routing_metrics(100, 0.995, 200.0, 170.0, 0.02, 0.0);

    let report = Evaluator::compare_routing(&baseline, &candidate);
    assert_eq!(report.recommendation, Recommendation::InsufficientData);
    assert!(
        report
            .reasons
            .iter()
            .any(|reason| reason.contains("baseline.success_rate")),
        "the refusal must name the malformed side: {:?}",
        report.reasons
    );
    assert_eq!(report.deltas.success_rate_delta, 0.0);
}

#[test]
fn compare_routing_refuses_rates_outside_the_unit_interval_and_negative_magnitudes() {
    let baseline = make_routing_metrics(100, 0.99, 200.0, 200.0, 0.02, 0.0);

    for (metric, value) in [
        ("success_rate", 1.5),
        ("success_rate", -0.001),
        ("fallback_rate", 1.5),
        ("escalation_rate", -1.0),
        ("p50_latency_ms", -1.0),
        ("p95_latency_ms", -1.0),
        ("mean_cost", -0.01),
    ] {
        let report = Evaluator::compare_routing(&baseline, &candidate_with(metric, value));
        assert_eq!(
            report.recommendation,
            Recommendation::InsufficientData,
            "{metric} = {value} is not a measurement"
        );
        assert!(
            report.reasons.iter().any(|reason| reason.contains(metric)),
            "the refusal must name {metric} = {value}: {:?}",
            report.reasons
        );
    }

    // The boundaries themselves are measurements: a rate of exactly 0 or 1 and
    // a latency or cost of exactly 0 are possible, so they must not be refused.
    for (metric, value) in [
        ("success_rate", 1.0),
        ("success_rate", 0.0),
        ("fallback_rate", 1.0),
        ("p95_latency_ms", 0.0),
        ("mean_cost", 0.0),
    ] {
        let report = Evaluator::compare_routing(&baseline, &candidate_with(metric, value));
        assert_ne!(
            report.recommendation,
            Recommendation::InsufficientData,
            "{metric} = {value} is inside its possible range"
        );
    }
}
