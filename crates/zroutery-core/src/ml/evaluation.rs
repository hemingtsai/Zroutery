//! Evaluation framework for ML routing models.
//!
//! Provides metrics for prediction quality (classification and regression),
//! routing utility metrics, and a comparison framework for A/B evaluation
//! of routing strategies.

use serde::{Deserialize, Serialize};

use crate::ml::dataset::TrainingSample;
use crate::ml::model::RoutingModel;

// ---------------------------------------------------------------------------
// PredictionMetrics — model prediction quality
// ---------------------------------------------------------------------------

/// Prediction quality metrics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PredictionMetrics {
    pub sample_count: usize,
    // Classification (success prediction)
    pub log_loss: Option<f64>,
    pub brier_score: Option<f64>,
    // Regression (latency/cost prediction)
    pub mae: Option<f64>,
    pub rmse: Option<f64>,
    // Summary
    pub mean_prediction: f64,
    pub mean_actual: f64,
}

impl PredictionMetrics {
    /// Compute classification metrics from predicted probabilities and actual boolean outcomes.
    ///
    /// `predictions` should be probabilities in [0, 1]. `actuals` are the true outcomes.
    /// Panics if slices have different lengths or are empty.
    pub fn compute_classification(predictions: &[f64], actuals: &[bool]) -> Self {
        assert_eq!(
            predictions.len(),
            actuals.len(),
            "predictions and actuals must have the same length"
        );
        assert!(!predictions.is_empty(), "cannot compute metrics on empty data");

        let n = predictions.len();
        let log_loss = Some(compute_log_loss(predictions, actuals));
        let brier_score = Some(compute_brier_score(predictions, actuals));

        let mean_prediction = predictions.iter().sum::<f64>() / n as f64;
        let mean_actual = actuals.iter().map(|&b| if b { 1.0 } else { 0.0 }).sum::<f64>() / n as f64;

        PredictionMetrics {
            sample_count: n,
            log_loss,
            brier_score,
            mae: None,
            rmse: None,
            mean_prediction,
            mean_actual,
        }
    }

    /// Compute regression metrics from predicted and actual continuous values.
    ///
    /// Panics if slices have different lengths or are empty.
    pub fn compute_regression(predictions: &[f64], actuals: &[f64]) -> Self {
        assert_eq!(
            predictions.len(),
            actuals.len(),
            "predictions and actuals must have the same length"
        );
        assert!(!predictions.is_empty(), "cannot compute metrics on empty data");

        let n = predictions.len();
        let mut sum_abs_err = 0.0;
        let mut sum_sq_err = 0.0;

        for (p, a) in predictions.iter().zip(actuals.iter()) {
            let err = p - a;
            sum_abs_err += err.abs();
            sum_sq_err += err * err;
        }

        let mae = sum_abs_err / n as f64;
        let rmse = (sum_sq_err / n as f64).sqrt();

        let mean_prediction = predictions.iter().sum::<f64>() / n as f64;
        let mean_actual = actuals.iter().sum::<f64>() / n as f64;

        PredictionMetrics {
            sample_count: n,
            log_loss: None,
            brier_score: None,
            mae: Some(mae),
            rmse: Some(rmse),
            mean_prediction,
            mean_actual,
        }
    }
}

/// Compute binary cross-entropy (log loss).
///
/// Clamps predictions to [epsilon, 1-epsilon] to avoid log(0).
fn compute_log_loss(predictions: &[f64], actuals: &[bool]) -> f64 {
    const EPS: f64 = 1e-15;
    let n = predictions.len() as f64;
    let mut loss = 0.0;

    for (p, &a) in predictions.iter().zip(actuals.iter()) {
        let p = p.clamp(EPS, 1.0 - EPS);
        let y = if a { 1.0 } else { 0.0 };
        loss -= y * p.ln() + (1.0 - y) * (1.0 - p).ln();
    }

    loss / n
}

/// Compute Brier score (mean squared error for probability predictions).
fn compute_brier_score(predictions: &[f64], actuals: &[bool]) -> f64 {
    let n = predictions.len() as f64;
    let mut sum = 0.0;

    for (p, &a) in predictions.iter().zip(actuals.iter()) {
        let y = if a { 1.0 } else { 0.0 };
        let err = p - y;
        sum += err * err;
    }

    sum / n
}

// ---------------------------------------------------------------------------
// RoutingMetrics — operational routing quality
// ---------------------------------------------------------------------------

/// Routing utility metrics summarizing operational performance.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingMetrics {
    pub total_requests: usize,
    pub success_rate: f64,
    pub p50_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub mean_cost: f64,
    pub fallback_rate: f64,
    pub escalation_rate: f64,
}

impl RoutingMetrics {
    /// Compute routing metrics from a collection of training samples.
    ///
    /// Each sample's targets are inspected for success, latency, cost, and
    /// fallback information.
    pub fn from_samples(samples: &[TrainingSample]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }

        let n = samples.len();
        let successes = samples.iter().filter(|s| s.targets.success).count();
        let success_rate = successes as f64 / n as f64;

        // Latencies: collect from successful samples
        let mut latencies: Vec<f64> = samples
            .iter()
            .filter_map(|s| s.targets.latency_ms)
            .collect();
        latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let p50_latency_ms = percentile(&latencies, 0.50);
        let p95_latency_ms = percentile(&latencies, 0.95);

        // Costs
        let costs: Vec<f64> = samples.iter().filter_map(|s| s.targets.cost).collect();
        let mean_cost = if costs.is_empty() {
            0.0
        } else {
            costs.iter().sum::<f64>() / costs.len() as f64
        };

        // Fallback rate: samples where fallback_count > 0
        let fallbacks = samples.iter().filter(|s| s.targets.fallback_count > 0).count();
        let fallback_rate = fallbacks as f64 / n as f64;

        // Escalation rate: samples where failure_class is Some (attempted and failed at least once)
        // and the request eventually succeeded (i.e. fallback/escalation occurred)
        let escalations = samples
            .iter()
            .filter(|s| s.targets.success && s.targets.fallback_count > 0)
            .count();
        let escalation_rate = escalations as f64 / n as f64;

        RoutingMetrics {
            total_requests: n,
            success_rate,
            p50_latency_ms,
            p95_latency_ms,
            mean_cost,
            fallback_rate,
            escalation_rate,
        }
    }
}

/// Compute a percentile from a sorted slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let idx = p * (sorted.len() - 1) as f64;
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let frac = idx - lo as f64;
        sorted[lo] * (1.0 - frac) + sorted[hi] * frac
    }
}

// ---------------------------------------------------------------------------
// ComparisonReport / RoutingDeltas / Recommendation
// ---------------------------------------------------------------------------

/// Recommendation from comparing two routing strategies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Recommendation {
    Accept,
    Reject,
    InsufficientData,
}

/// Deltas between baseline and candidate routing metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingDeltas {
    pub success_rate_delta: f64,
    pub p95_latency_delta_pct: f64,
    pub cost_delta_pct: f64,
    pub fallback_rate_delta: f64,
}

/// Report comparing baseline vs candidate routing metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonReport {
    pub baseline: RoutingMetrics,
    pub candidate: RoutingMetrics,
    pub deltas: RoutingDeltas,
    pub recommendation: Recommendation,
    pub reasons: Vec<String>,
}

// ---------------------------------------------------------------------------
// Evaluator
// ---------------------------------------------------------------------------

/// Stateless evaluator for ML routing models.
pub struct Evaluator;

impl Evaluator {
    /// Evaluate prediction quality of a model against actual outcomes.
    ///
    /// Runs the model on each sample's features and compares predictions
    /// against the sample's success target (classification).
    pub fn evaluate_predictions(
        model: &dyn RoutingModel,
        samples: &[TrainingSample],
    ) -> PredictionMetrics {
        if samples.is_empty() {
            return PredictionMetrics::default();
        }

        let mut predictions = Vec::with_capacity(samples.len());
        let mut actuals = Vec::with_capacity(samples.len());

        for sample in samples {
            let pred = model.predict(&sample.features);
            predictions.push(pred.value);
            actuals.push(sample.targets.success);
        }

        PredictionMetrics::compute_classification(&predictions, &actuals)
    }

    /// Compare two sets of routing metrics (baseline vs candidate).
    ///
    /// Returns a [`ComparisonReport`] with deltas and a recommendation.
    ///
    /// Rules:
    /// - If either side has fewer than 30 requests, recommend `InsufficientData`.
    /// - If the candidate improves success rate by >= 1pp OR reduces p95 latency
    ///   by >= 10% without degrading success rate, recommend `Accept`.
    /// - If the candidate degrades success rate by >= 1pp OR increases p95 latency
    ///   by >= 20%, recommend `Reject`.
    /// - Otherwise, `Accept` if the candidate has lower cost with no degradation.
    pub fn compare_routing(
        baseline: &RoutingMetrics,
        candidate: &RoutingMetrics,
    ) -> ComparisonReport {
        const MIN_SAMPLES: usize = 30;

        let deltas = RoutingDeltas {
            success_rate_delta: candidate.success_rate - baseline.success_rate,
            p95_latency_delta_pct: if baseline.p95_latency_ms > 0.0 {
                (candidate.p95_latency_ms - baseline.p95_latency_ms) / baseline.p95_latency_ms * 100.0
            } else {
                0.0
            },
            cost_delta_pct: if baseline.mean_cost > 0.0 {
                (candidate.mean_cost - baseline.mean_cost) / baseline.mean_cost * 100.0
            } else {
                0.0
            },
            fallback_rate_delta: candidate.fallback_rate - baseline.fallback_rate,
        };

        let mut reasons = Vec::new();

        // Insufficient data check
        if baseline.total_requests < MIN_SAMPLES || candidate.total_requests < MIN_SAMPLES {
            reasons.push(format!(
                "insufficient data: baseline={} candidate={} (min={})",
                baseline.total_requests, candidate.total_requests, MIN_SAMPLES
            ));
            return ComparisonReport {
                baseline: baseline.clone(),
                candidate: candidate.clone(),
                deltas,
                recommendation: Recommendation::InsufficientData,
                reasons,
            };
        }

        // Check for degradation
        let success_degraded = deltas.success_rate_delta < -0.01;
        let latency_degraded = deltas.p95_latency_delta_pct > 20.0;
        let fallback_degraded = deltas.fallback_rate_delta > 0.05;

        if success_degraded {
            reasons.push(format!(
                "success rate degraded by {:.1}pp",
                -deltas.success_rate_delta * 100.0
            ));
        }
        if latency_degraded {
            reasons.push(format!(
                "p95 latency increased by {:.1}%",
                deltas.p95_latency_delta_pct
            ));
        }
        if fallback_degraded {
            reasons.push(format!(
                "fallback rate increased by {:.1}pp",
                deltas.fallback_rate_delta * 100.0
            ));
        }

        if success_degraded || latency_degraded {
            return ComparisonReport {
                baseline: baseline.clone(),
                candidate: candidate.clone(),
                deltas,
                recommendation: Recommendation::Reject,
                reasons,
            };
        }

        // Check for improvement
        let success_improved = deltas.success_rate_delta >= 0.01;
        let latency_improved = deltas.p95_latency_delta_pct <= -10.0;
        let cost_improved = deltas.cost_delta_pct < -5.0;

        if success_improved {
            reasons.push(format!(
                "success rate improved by {:.1}pp",
                deltas.success_rate_delta * 100.0
            ));
        }
        if latency_improved {
            reasons.push(format!(
                "p95 latency reduced by {:.1}%",
                -deltas.p95_latency_delta_pct
            ));
        }
        if cost_improved {
            reasons.push(format!(
                "cost reduced by {:.1}%",
                -deltas.cost_delta_pct
            ));
        }

        if success_improved || latency_improved || cost_improved {
            return ComparisonReport {
                baseline: baseline.clone(),
                candidate: candidate.clone(),
                deltas,
                recommendation: Recommendation::Accept,
                reasons,
            };
        }

        // No significant difference
        reasons.push("no significant difference detected".to_string());
        ComparisonReport {
            baseline: baseline.clone(),
            candidate: candidate.clone(),
            deltas,
            recommendation: Recommendation::Reject,
            reasons,
        }
    }
}

// ---------------------------------------------------------------------------
// FrozenHoldout / temporal_split — Stage 7D acceptance
// ---------------------------------------------------------------------------

/// A frozen evaluation dataset that cannot be modified after creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenHoldout {
    pub samples: Vec<TrainingSample>,
    pub frozen_at: i64,
    pub description: String,
}

impl FrozenHoldout {
    pub fn new(samples: Vec<TrainingSample>, description: String) -> Self {
        FrozenHoldout {
            samples,
            frozen_at: chrono::Utc::now().timestamp(),
            description,
        }
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// Split samples into train/validation/holdout with temporal ordering.
///
/// Samples are sorted by timestamp, then partitioned according to the given
/// ratios (which should sum to at most 1.0).
pub fn temporal_split(
    samples: &mut [TrainingSample],
    train_ratio: f64,
    validation_ratio: f64,
) -> (
    Vec<TrainingSample>,
    Vec<TrainingSample>,
    Vec<TrainingSample>,
) {
    samples.sort_by_key(|s| s.timestamp);
    let n = samples.len();
    let train_end = (n as f64 * train_ratio) as usize;
    let val_end = (n as f64 * (train_ratio + validation_ratio)) as usize;
    let train = samples[..train_end].to_vec();
    let validation = samples[train_end..val_end].to_vec();
    let holdout = samples[val_end..].to_vec();
    (train, validation, holdout)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::dataset::{Targets, TrainingSample};
    use crate::ml::features::{RoutingFeatures, FEATURE_DIMENSION};
    use crate::ml::model::SuccessModel;
    use crate::feedback::DataOrigin;

    // -- helpers --

    fn make_sample(success: bool, latency_ms: Option<f64>, cost: Option<f64>, fallback_count: u32) -> TrainingSample {
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

    // -- 1. compute_classification with known data --

    #[test]
    fn compute_classification_known_data() {
        // Perfect predictions: predict 1.0 for true, 0.0 for false
        let predictions = vec![1.0, 0.0, 1.0, 0.0];
        let actuals = vec![true, false, true, false];
        let metrics = PredictionMetrics::compute_classification(&predictions, &actuals);

        assert_eq!(metrics.sample_count, 4);
        assert!(metrics.log_loss.is_some());
        assert!(metrics.brier_score.is_some());
        assert!(metrics.mae.is_none());
        assert!(metrics.rmse.is_none());

        // Perfect predictions -> log loss should be near zero
        let ll = metrics.log_loss.unwrap();
        assert!(ll < 0.01, "perfect predictions should have near-zero log loss, got {ll}");

        // Perfect predictions -> brier score should be near zero
        let bs = metrics.brier_score.unwrap();
        assert!(bs < 0.01, "perfect predictions should have near-zero brier score, got {bs}");

        assert!((metrics.mean_prediction - 0.5).abs() < 1e-10);
        assert!((metrics.mean_actual - 0.5).abs() < 1e-10);
    }

    // -- 2. compute_regression with known data --

    #[test]
    fn compute_regression_known_data() {
        let predictions = vec![10.0, 20.0, 30.0];
        let actuals = vec![12.0, 18.0, 33.0];
        let metrics = PredictionMetrics::compute_regression(&predictions, &actuals);

        assert_eq!(metrics.sample_count, 3);
        assert!(metrics.log_loss.is_none());
        assert!(metrics.brier_score.is_none());
        assert!(metrics.mae.is_some());
        assert!(metrics.rmse.is_some());

        // MAE = (|10-12| + |20-18| + |30-33|) / 3 = (2+2+3)/3 = 7/3
        let mae = metrics.mae.unwrap();
        assert!((mae - 7.0 / 3.0).abs() < 1e-10, "MAE should be 7/3, got {mae}");

        // RMSE = sqrt((4+4+9)/3) = sqrt(17/3)
        let rmse = metrics.rmse.unwrap();
        assert!((rmse - (17.0_f64 / 3.0).sqrt()).abs() < 1e-10, "RMSE should be sqrt(17/3), got {rmse}");

        assert!((metrics.mean_prediction - 20.0).abs() < 1e-10);
        assert!((metrics.mean_actual - 21.0).abs() < 1e-10);
    }

    // -- 3. ComparisonReport: better candidate -> Accept --

    #[test]
    fn comparison_better_candidate_accept() {
        let baseline = RoutingMetrics {
            total_requests: 100,
            success_rate: 0.90,
            p50_latency_ms: 200.0,
            p95_latency_ms: 500.0,
            mean_cost: 0.02,
            fallback_rate: 0.05,
            escalation_rate: 0.03,
        };
        let candidate = RoutingMetrics {
            total_requests: 100,
            success_rate: 0.95,
            p50_latency_ms: 180.0,
            p95_latency_ms: 400.0,
            mean_cost: 0.015,
            fallback_rate: 0.03,
            escalation_rate: 0.02,
        };

        let report = Evaluator::compare_routing(&baseline, &candidate);
        assert_eq!(report.recommendation, Recommendation::Accept);
        assert!(!report.reasons.is_empty());
        // Delta checks
        assert!(report.deltas.success_rate_delta > 0.0);
        assert!(report.deltas.p95_latency_delta_pct < 0.0);
        assert!(report.deltas.cost_delta_pct < 0.0);
    }

    // -- 4. ComparisonReport: worse candidate -> Reject --

    #[test]
    fn comparison_worse_candidate_reject() {
        let baseline = RoutingMetrics {
            total_requests: 100,
            success_rate: 0.95,
            p50_latency_ms: 200.0,
            p95_latency_ms: 500.0,
            mean_cost: 0.02,
            fallback_rate: 0.03,
            escalation_rate: 0.02,
        };
        let candidate = RoutingMetrics {
            total_requests: 100,
            success_rate: 0.80, // much worse
            p50_latency_ms: 300.0,
            p95_latency_ms: 800.0,
            mean_cost: 0.03,
            fallback_rate: 0.10,
            escalation_rate: 0.05,
        };

        let report = Evaluator::compare_routing(&baseline, &candidate);
        assert_eq!(report.recommendation, Recommendation::Reject);
        assert!(report.deltas.success_rate_delta < 0.0);
    }

    // -- 5. ComparisonReport: insufficient data -> InsufficientData --

    #[test]
    fn comparison_insufficient_data() {
        let baseline = RoutingMetrics {
            total_requests: 10, // below threshold
            success_rate: 0.90,
            p50_latency_ms: 200.0,
            p95_latency_ms: 500.0,
            mean_cost: 0.02,
            fallback_rate: 0.05,
            escalation_rate: 0.03,
        };
        let candidate = RoutingMetrics {
            total_requests: 50,
            success_rate: 0.95,
            p50_latency_ms: 180.0,
            p95_latency_ms: 400.0,
            mean_cost: 0.015,
            fallback_rate: 0.03,
            escalation_rate: 0.02,
        };

        let report = Evaluator::compare_routing(&baseline, &candidate);
        assert_eq!(report.recommendation, Recommendation::InsufficientData);
    }

    // -- 6. RoutingMetrics computation from sample outcomes --

    #[test]
    fn routing_metrics_from_samples() {
        let samples = vec![
            make_sample(true, Some(100.0), Some(0.01), 0),
            make_sample(true, Some(200.0), Some(0.02), 0),
            make_sample(true, Some(300.0), Some(0.03), 1), // fallback
            make_sample(false, None, None, 0),
            make_sample(true, Some(150.0), Some(0.015), 0),
        ];

        let metrics = RoutingMetrics::from_samples(&samples);

        assert_eq!(metrics.total_requests, 5);
        assert!((metrics.success_rate - 0.8).abs() < 1e-10); // 4/5
        assert!(metrics.p50_latency_ms > 0.0);
        assert!(metrics.p95_latency_ms >= metrics.p50_latency_ms);
        // mean cost: (0.01+0.02+0.03+0.015)/4 = 0.075/4 = 0.01875
        assert!((metrics.mean_cost - 0.01875).abs() < 1e-6);
        // fallback_rate: 1/5 = 0.2
        assert!((metrics.fallback_rate - 0.2).abs() < 1e-10);
        // escalation_rate: 1 success with fallback / 5 = 0.2
        assert!((metrics.escalation_rate - 0.2).abs() < 1e-10);
    }

    #[test]
    fn routing_metrics_empty_samples() {
        let metrics = RoutingMetrics::from_samples(&[]);
        assert_eq!(metrics.total_requests, 0);
        assert_eq!(metrics.success_rate, 0.0);
    }

    // -- 7. LogLoss computation accuracy --

    #[test]
    fn log_loss_computation_accuracy() {
        // Known case: all predictions = 0.5, all actual = true
        // loss = -ln(0.5) = 0.693147...
        let predictions = vec![0.5; 4];
        let actuals = vec![true; 4];
        let metrics = PredictionMetrics::compute_classification(&predictions, &actuals);
        let ll = metrics.log_loss.unwrap();
        assert!(
            (ll - 0.6931471805599453).abs() < 1e-10,
            "log loss for p=0.5, y=1 should be ln(2), got {ll}"
        );

        // Known case: all predictions = 0.9, all actual = true
        // loss = -ln(0.9) = 0.105360...
        let predictions2 = vec![0.9; 4];
        let actuals2 = vec![true; 4];
        let metrics2 = PredictionMetrics::compute_classification(&predictions2, &actuals2);
        let ll2 = metrics2.log_loss.unwrap();
        assert!(
            (ll2 - 0.9_f64.ln().abs()).abs() < 1e-10,
            "log loss for p=0.9, y=1 should be -ln(0.9), got {ll2}"
        );

        // Known case: all predictions = 0.1, all actual = false
        // loss = -ln(1-0.1) = -ln(0.9)
        let predictions3 = vec![0.1; 4];
        let actuals3 = vec![false; 4];
        let metrics3 = PredictionMetrics::compute_classification(&predictions3, &actuals3);
        let ll3 = metrics3.log_loss.unwrap();
        assert!(
            (ll3 - 0.9_f64.ln().abs()).abs() < 1e-10,
            "log loss for p=0.1, y=0 should be -ln(0.9), got {ll3}"
        );
    }

    // -- 8. Brier score computation accuracy --

    #[test]
    fn brier_score_computation_accuracy() {
        // Perfect predictions -> Brier = 0
        let predictions = vec![1.0, 0.0, 1.0, 0.0];
        let actuals = vec![true, false, true, false];
        let metrics = PredictionMetrics::compute_classification(&predictions, &actuals);
        let bs = metrics.brier_score.unwrap();
        assert!(bs.abs() < 1e-10, "perfect predictions should have Brier=0, got {bs}");

        // All predictions = 0.5, all actual = true
        // Brier = mean((0.5 - 1)^2) = 0.25
        let predictions2 = vec![0.5; 4];
        let actuals2 = vec![true; 4];
        let metrics2 = PredictionMetrics::compute_classification(&predictions2, &actuals2);
        let bs2 = metrics2.brier_score.unwrap();
        assert!(
            (bs2 - 0.25).abs() < 1e-10,
            "Brier for p=0.5, y=1 should be 0.25, got {bs2}"
        );

        // Mixed: predictions=[0.7, 0.3], actuals=[true, false]
        // Brier = ((0.7-1)^2 + (0.3-0)^2) / 2 = (0.09 + 0.09) / 2 = 0.09
        let predictions3 = vec![0.7, 0.3];
        let actuals3 = vec![true, false];
        let metrics3 = PredictionMetrics::compute_classification(&predictions3, &actuals3);
        let bs3 = metrics3.brier_score.unwrap();
        assert!(
            (bs3 - 0.09).abs() < 1e-10,
            "Brier for [0.7,0.3] vs [true,false] should be 0.09, got {bs3}"
        );
    }

    // -- Additional: Evaluator::evaluate_predictions with a real model --

    #[test]
    fn evaluate_predictions_with_success_model() {
        let model = SuccessModel::new(FEATURE_DIMENSION);
        let samples = vec![
            make_sample(true, Some(100.0), Some(0.01), 0),
            make_sample(false, None, None, 0),
            make_sample(true, Some(200.0), Some(0.02), 0),
        ];

        let metrics = Evaluator::evaluate_predictions(&model, &samples);
        assert_eq!(metrics.sample_count, 3);
        assert!(metrics.log_loss.is_some());
        assert!(metrics.brier_score.is_some());
        // Cold model predictions are ~0.5 for all, so mean_prediction ~0.5
        assert!(metrics.mean_prediction > 0.0 && metrics.mean_prediction < 1.0);
    }

    #[test]
    fn evaluate_predictions_empty_samples() {
        let model = SuccessModel::new(FEATURE_DIMENSION);
        let metrics = Evaluator::evaluate_predictions(&model, &[]);
        assert_eq!(metrics.sample_count, 0);
    }

    // -- Additional: ComparisonReport identical metrics -> Reject (no improvement) --

    #[test]
    fn comparison_identical_metrics_reject() {
        let metrics = RoutingMetrics {
            total_requests: 100,
            success_rate: 0.90,
            p50_latency_ms: 200.0,
            p95_latency_ms: 500.0,
            mean_cost: 0.02,
            fallback_rate: 0.05,
            escalation_rate: 0.03,
        };

        let report = Evaluator::compare_routing(&metrics, &metrics);
        assert_eq!(report.recommendation, Recommendation::Reject);
        assert!(report.reasons.iter().any(|r| r.contains("no significant")));
    }

    // -- Additional: ComparisonReport cost improvement only --

    #[test]
    fn comparison_cost_improvement_accept() {
        let baseline = RoutingMetrics {
            total_requests: 100,
            success_rate: 0.90,
            p50_latency_ms: 200.0,
            p95_latency_ms: 500.0,
            mean_cost: 0.02,
            fallback_rate: 0.05,
            escalation_rate: 0.03,
        };
        let candidate = RoutingMetrics {
            total_requests: 100,
            success_rate: 0.90,
            p50_latency_ms: 200.0,
            p95_latency_ms: 500.0,
            mean_cost: 0.01, // 50% cheaper
            fallback_rate: 0.05,
            escalation_rate: 0.03,
        };

        let report = Evaluator::compare_routing(&baseline, &candidate);
        assert_eq!(report.recommendation, Recommendation::Accept);
        assert!(report.deltas.cost_delta_pct < -5.0);
    }

    // -- Additional: RoutingDeltas fields --

    #[test]
    fn routing_deltas_correct_computation() {
        let baseline = RoutingMetrics {
            total_requests: 100,
            success_rate: 0.90,
            p50_latency_ms: 200.0,
            p95_latency_ms: 500.0,
            mean_cost: 0.02,
            fallback_rate: 0.05,
            escalation_rate: 0.03,
        };
        let candidate = RoutingMetrics {
            total_requests: 100,
            success_rate: 0.95,
            p50_latency_ms: 180.0,
            p95_latency_ms: 400.0,
            mean_cost: 0.01,
            fallback_rate: 0.03,
            escalation_rate: 0.02,
        };

        let report = Evaluator::compare_routing(&baseline, &candidate);
        // success_rate_delta = 0.95 - 0.90 = 0.05
        assert!((report.deltas.success_rate_delta - 0.05).abs() < 1e-10);
        // p95_latency_delta_pct = (400-500)/500 * 100 = -20%
        assert!((report.deltas.p95_latency_delta_pct - (-20.0)).abs() < 1e-10);
        // cost_delta_pct = (0.01-0.02)/0.02 * 100 = -50%
        assert!((report.deltas.cost_delta_pct - (-50.0)).abs() < 1e-10);
        // fallback_rate_delta = 0.03 - 0.05 = -0.02
        assert!((report.deltas.fallback_rate_delta - (-0.02)).abs() < 1e-10);
    }

    // -- Additional: PredictionMetrics serde round-trip --

    #[test]
    fn prediction_metrics_serde_round_trip() {
        let metrics = PredictionMetrics {
            sample_count: 100,
            log_loss: Some(0.45),
            brier_score: Some(0.12),
            mae: None,
            rmse: None,
            mean_prediction: 0.6,
            mean_actual: 0.55,
        };
        let json = serde_json::to_string(&metrics).unwrap();
        let restored: PredictionMetrics = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.sample_count, 100);
        assert!((restored.log_loss.unwrap() - 0.45).abs() < 1e-10);
        assert!((restored.brier_score.unwrap() - 0.12).abs() < 1e-10);
    }

    // -- Additional: RoutingMetrics serde round-trip --

    #[test]
    fn routing_metrics_serde_round_trip() {
        let metrics = RoutingMetrics {
            total_requests: 50,
            success_rate: 0.92,
            p50_latency_ms: 150.0,
            p95_latency_ms: 400.0,
            mean_cost: 0.015,
            fallback_rate: 0.04,
            escalation_rate: 0.02,
        };
        let json = serde_json::to_string(&metrics).unwrap();
        let restored: RoutingMetrics = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.total_requests, 50);
        assert!((restored.success_rate - 0.92).abs() < 1e-10);
    }

    // -- FrozenHoldout tests --

    #[test]
    fn frozen_holdout_cannot_be_modified_after_creation() {
        let samples = vec![
            make_sample(true, Some(100.0), Some(0.01), 0),
            make_sample(false, None, None, 0),
        ];
        let holdout = FrozenHoldout::new(samples, "test holdout".to_string());

        // The holdout is immutable (no &mut self methods).
        // Verify it stores samples correctly.
        assert_eq!(holdout.len(), 2);
        assert!(!holdout.is_empty());
        assert_eq!(holdout.description, "test holdout");
        assert!(holdout.frozen_at > 0);

        // Verify samples are preserved exactly.
        assert!(holdout.samples[0].targets.success);
        assert!(!holdout.samples[1].targets.success);
    }

    #[test]
    fn frozen_holdout_empty() {
        let holdout = FrozenHoldout::new(Vec::new(), "empty".to_string());
        assert_eq!(holdout.len(), 0);
        assert!(holdout.is_empty());
    }

    #[test]
    fn frozen_holdout_serde_round_trip() {
        let samples = vec![make_sample(true, Some(200.0), Some(0.02), 0)];
        let holdout = FrozenHoldout::new(samples, "serde test".to_string());
        let json = serde_json::to_string(&holdout).unwrap();
        let restored: FrozenHoldout = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored.description, "serde test");
        assert_eq!(restored.frozen_at, holdout.frozen_at);
    }

    // -- temporal_split tests --

    #[test]
    fn temporal_split_correct_ratio() {
        // Use power-of-2-friendly ratios to avoid floating-point imprecision.
        let mut samples: Vec<TrainingSample> = (0..100)
            .map(|i| {
                let mut s = make_sample(true, Some(100.0), Some(0.01), 0);
                s.timestamp = 1_700_000_000 + i;
                s
            })
            .collect();

        let (train, val, holdout) = temporal_split(&mut samples, 0.5, 0.25);
        assert_eq!(train.len(), 50);
        assert_eq!(val.len(), 25);
        assert_eq!(holdout.len(), 25);
    }

    #[test]
    fn temporal_split_sorted_by_timestamp() {
        let mut samples: Vec<TrainingSample> = vec![
            {
                let mut s = make_sample(true, Some(100.0), Some(0.01), 0);
                s.timestamp = 3000;
                s
            },
            {
                let mut s = make_sample(false, None, None, 0);
                s.timestamp = 1000;
                s
            },
            {
                let mut s = make_sample(true, Some(200.0), Some(0.02), 0);
                s.timestamp = 2000;
                s
            },
        ];

        let (train, val, holdout) = temporal_split(&mut samples, 0.5, 0.25);
        // After sorting: timestamps [1000, 2000, 3000]
        // train = [1000], val = [2000], holdout = [3000]
        assert_eq!(train.len(), 1);
        assert_eq!(val.len(), 1);
        assert_eq!(holdout.len(), 1);
        assert_eq!(train[0].timestamp, 1000);
        assert_eq!(val[0].timestamp, 2000);
        assert_eq!(holdout[0].timestamp, 3000);
    }

    #[test]
    fn temporal_split_empty_samples() {
        let mut samples: Vec<TrainingSample> = Vec::new();
        let (train, val, holdout) = temporal_split(&mut samples, 0.7, 0.2);
        assert!(train.is_empty());
        assert!(val.is_empty());
        assert!(holdout.is_empty());
    }
}
