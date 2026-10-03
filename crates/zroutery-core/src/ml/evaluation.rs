//! Evaluation framework for ML routing models.
//!
//! Provides metrics for prediction quality (classification and regression),
//! routing utility metrics, and a comparison framework for A/B evaluation
//! of routing strategies.

use serde::{Deserialize, Serialize};

use crate::ml::dataset::TrainingSample;
use crate::ml::model::RoutingModel;

// ---------------------------------------------------------------------------
// EvaluationError
// ---------------------------------------------------------------------------

/// Every way the fallible metric constructors refuse.
///
/// [`PredictionMetrics::compute_classification`] and
/// [`PredictionMetrics::compute_regression`] predate this type and keep their
/// `assert!`-based signatures, so a caller that has already validated its input
/// is not forced through a `Result`. The fallible constructors below are the
/// same arithmetic behind a typed refusal, for callers that have *not* — the
/// offline calibration node among them, which measures a K-way vector and must
/// not be able to turn a malformed probability into a `NaN` metric.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum EvaluationError {
    /// The metric needs at least one observation. An empty input has no mean.
    #[error("cannot compute metrics on empty data")]
    EmptyObservations,

    /// The prediction and outcome slices are not parallel.
    #[error("predictions and actuals must have the same length: {predictions} predictions, {actuals} actuals")]
    LengthMismatch { predictions: usize, actuals: usize },

    /// A prediction is `NaN` or infinite, which would poison every mean below.
    #[error("classification prediction at index {index} is not finite: {value}")]
    NonFinitePrediction { index: usize, value: f64 },

    /// A classification prediction is outside `[0, 1]`, so it is not a
    /// probability and `mean_prediction` would not be a probability either.
    #[error("classification prediction at index {index} is outside [0, 1]: {value}")]
    PredictionOutOfRange { index: usize, value: f64 },

    /// A regression prediction is `NaN` or infinite.
    #[error("regression prediction at index {index} is not finite: {value}")]
    NonFiniteRegressionPrediction { index: usize, value: f64 },

    /// A field of a [`RoutingMetrics`] record is `NaN` or infinite, so no
    /// comparison of it — a delta, a degradation test, an improvement test —
    /// has a defined answer. A `NaN` success rate is the motivating case: every
    /// `<` test against it reads false, so a comparison that did not check
    /// would fall through to whatever the other terms said.
    #[error("routing metric {side}.{metric} is not finite: {value}")]
    NonFiniteRoutingMetric {
        side: &'static str,
        metric: &'static str,
        value: f64,
    },

    /// A field of a [`RoutingMetrics`] record is outside the interval it can
    /// occupy: a rate is a probability and a latency or a cost is a magnitude.
    #[error("routing metric {side}.{metric} is outside its possible range ({bound}): {value}")]
    RoutingMetricOutOfRange {
        side: &'static str,
        metric: &'static str,
        value: f64,
        bound: &'static str,
    },
}

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
        assert!(
            !predictions.is_empty(),
            "cannot compute metrics on empty data"
        );

        let n = predictions.len();
        let log_loss = Some(compute_log_loss(predictions, actuals));
        let brier_score = Some(compute_brier_score(predictions, actuals));

        let mean_prediction = predictions.iter().sum::<f64>() / n as f64;
        let mean_actual = actuals
            .iter()
            .map(|&b| if b { 1.0 } else { 0.0 })
            .sum::<f64>()
            / n as f64;

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

    /// Compute classification metrics, refusing a malformed input.
    ///
    /// The fallible twin of [`PredictionMetrics::compute_classification`], and
    /// the same arithmetic behind a typed refusal instead of a panic. Every
    /// consumer that cannot prove its probabilities are finite and inside
    /// `[0, 1]` belongs here: a `NaN` that reaches the arithmetic returns a
    /// metrics struct whose every field is `NaN` and whose `Option`s are
    /// populated, which reads exactly like a measurement.
    pub fn try_compute_classification(
        predictions: &[f64],
        actuals: &[bool],
    ) -> Result<Self, EvaluationError> {
        if predictions.len() != actuals.len() {
            return Err(EvaluationError::LengthMismatch {
                predictions: predictions.len(),
                actuals: actuals.len(),
            });
        }
        if predictions.is_empty() {
            return Err(EvaluationError::EmptyObservations);
        }
        for (index, prediction) in predictions.iter().enumerate() {
            if !prediction.is_finite() {
                return Err(EvaluationError::NonFinitePrediction {
                    index,
                    value: *prediction,
                });
            }
            if *prediction < 0.0 || *prediction > 1.0 {
                return Err(EvaluationError::PredictionOutOfRange {
                    index,
                    value: *prediction,
                });
            }
        }

        let n = predictions.len();
        let log_loss = Some(compute_log_loss(predictions, actuals));
        let brier_score = Some(compute_brier_score(predictions, actuals));
        let mean_prediction = predictions.iter().sum::<f64>() / n as f64;
        let mean_actual = actuals
            .iter()
            .map(|&b| if b { 1.0 } else { 0.0 })
            .sum::<f64>()
            / n as f64;

        Ok(PredictionMetrics {
            sample_count: n,
            log_loss,
            brier_score,
            mae: None,
            rmse: None,
            mean_prediction,
            mean_actual,
        })
    }

    /// Compute regression metrics, refusing a malformed input.
    ///
    /// The fallible twin of [`PredictionMetrics::compute_regression`]. Unlike
    /// the classification twin this one does not police a range: a latency or a
    /// cost is a magnitude, and there is no interval a magnitude must lie in.
    pub fn try_compute_regression(
        predictions: &[f64],
        actuals: &[f64],
    ) -> Result<Self, EvaluationError> {
        if predictions.len() != actuals.len() {
            return Err(EvaluationError::LengthMismatch {
                predictions: predictions.len(),
                actuals: actuals.len(),
            });
        }
        if predictions.is_empty() {
            return Err(EvaluationError::EmptyObservations);
        }
        for (index, prediction) in predictions.iter().enumerate() {
            if !prediction.is_finite() {
                return Err(EvaluationError::NonFiniteRegressionPrediction {
                    index,
                    value: *prediction,
                });
            }
        }

        let n = predictions.len();
        let mut sum_abs_err = 0.0;
        let mut sum_sq_err = 0.0;
        for (prediction, actual) in predictions.iter().zip(actuals) {
            let error = prediction - actual;
            sum_abs_err += error.abs();
            sum_sq_err += error * error;
        }

        let mean_prediction = predictions.iter().sum::<f64>() / n as f64;
        let mean_actual = actuals.iter().sum::<f64>() / n as f64;

        Ok(PredictionMetrics {
            sample_count: n,
            log_loss: None,
            brier_score: None,
            mae: Some(sum_abs_err / n as f64),
            rmse: Some((sum_sq_err / n as f64).sqrt()),
            mean_prediction,
            mean_actual,
        })
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
        assert!(
            !predictions.is_empty(),
            "cannot compute metrics on empty data"
        );

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
        let fallbacks = samples
            .iter()
            .filter(|s| s.targets.fallback_count > 0)
            .count();
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

    /// Refuse a metrics record whose numbers are not measurements.
    ///
    /// `side` names which compared side this record is (`"baseline"` or
    /// `"candidate"`), so a refusal says which record was malformed without the
    /// caller having to build a second message.
    ///
    /// The checks are exactly the ones a comparison silently depends on. A rate
    /// must be a finite probability in `[0, 1]`; a latency percentile and a
    /// mean cost must be finite and non-negative. `total_requests` is a `usize`
    /// and has no invalid value. Fields are checked in declaration order, so a
    /// malformed record's first refusal is deterministic.
    ///
    /// This is deliberately narrower than "every number is reasonable": a
    /// comparison has no opinion about how fast or how cheap is good, only
    /// about whether a number is a measurement at all.
    pub fn validate(&self, side: &'static str) -> Result<(), EvaluationError> {
        for (metric, value) in [
            ("success_rate", self.success_rate),
            ("fallback_rate", self.fallback_rate),
            ("escalation_rate", self.escalation_rate),
        ] {
            if !value.is_finite() {
                return Err(EvaluationError::NonFiniteRoutingMetric {
                    side,
                    metric,
                    value,
                });
            }
            if !(0.0..=1.0).contains(&value) {
                return Err(EvaluationError::RoutingMetricOutOfRange {
                    side,
                    metric,
                    value,
                    bound: "[0, 1]",
                });
            }
        }
        for (metric, value) in [
            ("p50_latency_ms", self.p50_latency_ms),
            ("p95_latency_ms", self.p95_latency_ms),
            ("mean_cost", self.mean_cost),
        ] {
            if !value.is_finite() {
                return Err(EvaluationError::NonFiniteRoutingMetric {
                    side,
                    metric,
                    value,
                });
            }
            if value < 0.0 {
                return Err(EvaluationError::RoutingMetricOutOfRange {
                    side,
                    metric,
                    value,
                    bound: ">= 0",
                });
            }
        }
        Ok(())
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
///
/// Every delta is `0.0` when the comparison could not be made at all, so a
/// caller that reads a delta without reading the recommendation sees "no
/// measured movement" rather than a `NaN` that compares false against
/// everything and serializes as `null`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// - Both records are validated first. A field that is not finite, or that
    ///   lies outside the interval it can occupy, is not a measurement: the
    ///   comparison returns `InsufficientData`, names the field, and reports no
    ///   deltas rather than a `NaN` that every threshold below would read as
    ///   false.
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

        // Validation comes first. Every delta below is arithmetic on these
        // numbers and every threshold below is a comparison against them; a
        // `NaN` makes the arithmetic `NaN` and every `<`/`>` test false, so a
        // comparison that skipped this could accept a candidate on an
        // unrelated improvement while the success rate was not a measurement.
        let mut reasons = Vec::new();
        for (side, metrics) in [("baseline", baseline), ("candidate", candidate)] {
            if let Err(refusal) = metrics.validate(side) {
                reasons.push(format!("cannot compare: {refusal}"));
                return ComparisonReport {
                    baseline: baseline.clone(),
                    candidate: candidate.clone(),
                    deltas: RoutingDeltas::default(),
                    recommendation: Recommendation::InsufficientData,
                    reasons,
                };
            }
        }

        let deltas = RoutingDeltas {
            success_rate_delta: candidate.success_rate - baseline.success_rate,
            p95_latency_delta_pct: if baseline.p95_latency_ms > 0.0 {
                (candidate.p95_latency_ms - baseline.p95_latency_ms) / baseline.p95_latency_ms
                    * 100.0
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
            reasons.push(format!("cost reduced by {:.1}%", -deltas.cost_delta_pct));
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

    pub fn samples(&self) -> &[TrainingSample] {
        &self.samples
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Evaluate a model against the holdout. This is the ONLY way to use holdout data.
    pub fn evaluate(&self, model: &dyn RoutingModel) -> PredictionMetrics {
        let predictions: Vec<f64> = self
            .samples
            .iter()
            .map(|s| model.predict(&s.features).value)
            .collect();
        let actuals: Vec<bool> = self.samples.iter().map(|s| s.targets.success).collect();
        PredictionMetrics::compute_classification(&predictions, &actuals)
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
// Exact equality — what "the same" means when a claim must be exact
// ---------------------------------------------------------------------------
//
// The metrics above answer "how far off". A release gate has to answer a
// different question — "is this the same artifact's result, or a near miss" —
// and for that question `==` is the wrong operator, in both directions:
//
//   * `f64` compares through [`f64::to_bits`], never through `==`. That is
//     strictly stronger, not merely different: `0.0 == -0.0` is true under
//     `==` and false in bits, and a signed zero reaching a decision is a fact
//     about the value that `==` would silently discard.
//   * A non-finite value never compares equal, in either direction. Under
//     `to_bits`, two `NaN`s sharing a payload compare *equal*, so a broken
//     model could be handed a match; under `==` they compare unequal and the
//     break is hidden behind an innocuous-looking divergence.
//     [`find_nonfinite_f64`] runs first and converts that into a refusal, which
//     is what makes bit equality sound as a verdict rather than a comparison.
//   * There is no tolerance, no rounding, and no relative epsilon in this
//     section. A caller who wants "close enough" is asking a measurement
//     question, and the answer to that belongs in a metric with a stated
//     tolerance — never in a claim that two runs produced the same thing.

/// The total-order image of an IEEE-754 `f64` bit pattern.
///
/// Maps the bit pattern to a signed integer that increases monotonically with
/// the value, so the difference between two images is the number of
/// representable doubles between them. `-0.0` and `+0.0` map to the same
/// integer: they are the same number, and [`f64::to_bits`] is the comparison
/// that tells them apart when that distinction is the one that matters.
fn total_order(bits: u64) -> i128 {
    let negative = bits >> 63 == 1;
    let magnitude = (bits & !(1u64 << 63)) as i128;
    if negative {
        -magnitude
    } else {
        magnitude
    }
}

/// How many representable `f64` values separate `left` from `right`.
///
/// Zero exactly when the two are the same number. Saturates at [`u64::MAX`]
/// rather than wrapping, because the honest answer for "these are nowhere near
/// each other" is a large number, not a small one. Two `NaN`s are reported as
/// maximally distant unless they share a bit pattern, which is why a non-finite
/// value must be refused before this is consulted.
pub fn ulp_distance(left: f64, right: f64) -> u64 {
    let distance = (total_order(left.to_bits()) - total_order(right.to_bits())).unsigned_abs();
    u64::try_from(distance).unwrap_or(u64::MAX)
}

/// A `f64` is bit-identical to another `f64`.
///
/// The strict test, and the one the replay gate uses. It distinguishes `+0.0`
/// from `-0.0`, which `==` does not.
pub fn f64_identical(left: f64, right: f64) -> bool {
    left.to_bits() == right.to_bits()
}

/// A `f32` is bit-identical to another `f32`.
///
/// The feature vectors the replay gate is driven by are `f32`, and they are
/// compared the same way as everything else: in bits.
pub fn f32_identical(left: f32, right: f32) -> bool {
    left.to_bits() == right.to_bits()
}

/// A value that is not finite, named precisely enough to find in the source.
#[derive(Debug, Clone, PartialEq)]
pub struct NonFiniteComponent {
    /// Dotted path of the component, in the caller's comparison order.
    pub component: String,
    /// Index within a repeated component, when the component is repeated.
    pub index: Option<usize>,
    /// The value that refused.
    pub value: f64,
}

impl std::fmt::Display for NonFiniteComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.index {
            Some(index) => write!(
                f,
                "non-finite measurement at {}[{}]: {}",
                self.component, index, self.value
            ),
            None => write!(
                f,
                "non-finite measurement at {}: {}",
                self.component, self.value
            ),
        }
    }
}

impl std::error::Error for NonFiniteComponent {}

/// The first non-finite value in a fixed, caller-declared component order.
///
/// The scan is deliberately ordered rather than parallel: a refusal that names
/// *the first* non-finite component is reproducible, and a refusal that names
/// whichever one a parallel scan happened to reach first is not. Passing the
/// named values in a caller's documented order is how this stays deterministic
/// without this function knowing anything about the shapes involved.
pub fn find_nonfinite_f64(components: &[(&str, f64)]) -> Result<(), NonFiniteComponent> {
    for (component, value) in components {
        if !value.is_finite() {
            return Err(NonFiniteComponent {
                component: (*component).to_string(),
                index: None,
                value: *value,
            });
        }
    }
    Ok(())
}

/// The first place two structurally identical values differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// Dotted path of the component that differs.
    pub component: String,
    /// Index within a repeated component, when the component is repeated.
    pub index: Option<usize>,
    /// The recorded value, in the lossless `{:?}` spelling.
    pub expected: String,
    /// The replayed value, in the lossless `{:?}` spelling.
    pub actual: String,
}

impl std::fmt::Display for Divergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let index = self
            .index
            .map(|index| format!("[{index}]"))
            .unwrap_or_default();
        write!(
            f,
            "replay diverges at {}{}: recorded {} but replayed {}",
            self.component, index, self.expected, self.actual
        )
    }
}

impl std::error::Error for Divergence {}

/// A one-way cursor that records the **first** exact difference and then stops
/// caring.
///
/// Short-circuiting is the point. A gate that reports every difference is
/// reporting a diff; a gate that reports the first one, in a documented order,
/// is reporting where the replay stopped being the recorded decision. The
/// methods are called in that documented order by the caller, so the refusal is
/// deterministic and does not depend on field layout.
#[derive(Debug, Clone, Default)]
pub struct Exactness {
    divergence: Option<Divergence>,
}

impl Exactness {
    /// A cursor that has found no divergence yet.
    pub fn new() -> Self {
        Self { divergence: None }
    }

    /// Whether every component compared so far was bit-identical.
    pub fn is_exact(&self) -> bool {
        self.divergence.is_none()
    }

    /// The first divergence, if any.
    pub fn divergence(&self) -> Option<&Divergence> {
        self.divergence.as_ref()
    }

    /// Consume the cursor, yielding the first divergence as a refusal.
    pub fn into_divergence(self) -> Result<(), Divergence> {
        match self.divergence {
            Some(divergence) => Err(divergence),
            None => Ok(()),
        }
    }

    /// Record a divergence, keeping the first one recorded.
    fn record(&mut self, component: &str, index: Option<usize>, expected: String, actual: String) {
        if self.divergence.is_none() {
            self.divergence = Some(Divergence {
                component: component.to_string(),
                index,
                expected,
                actual,
            });
        }
    }

    /// Compare a `f64` in bits.
    pub fn expect_f64(
        &mut self,
        component: &str,
        index: Option<usize>,
        expected: f64,
        actual: f64,
    ) {
        if !f64_identical(expected, actual) {
            self.record(
                component,
                index,
                format!("{expected:?}"),
                format!("{actual:?}"),
            );
        }
    }

    /// Compare a `f32` in bits.
    pub fn expect_f32(
        &mut self,
        component: &str,
        index: Option<usize>,
        expected: f32,
        actual: f32,
    ) {
        if !f32_identical(expected, actual) {
            self.record(
                component,
                index,
                format!("{expected:?}"),
                format!("{actual:?}"),
            );
        }
    }

    /// Compare a string exactly.
    pub fn expect_str(
        &mut self,
        component: &str,
        index: Option<usize>,
        expected: &str,
        actual: &str,
    ) {
        if expected != actual {
            self.record(
                component,
                index,
                format!("{expected:?}"),
                format!("{actual:?}"),
            );
        }
    }

    /// Compare an optional string, discriminants included.
    pub fn expect_opt_str(
        &mut self,
        component: &str,
        index: Option<usize>,
        expected: Option<&str>,
        actual: Option<&str>,
    ) {
        if expected != actual {
            self.record(
                component,
                index,
                format!("{expected:?}"),
                format!("{actual:?}"),
            );
        }
    }

    /// Compare a `bool`.
    pub fn expect_bool(
        &mut self,
        component: &str,
        index: Option<usize>,
        expected: bool,
        actual: bool,
    ) {
        if expected != actual {
            self.record(
                component,
                index,
                format!("{expected:?}"),
                format!("{actual:?}"),
            );
        }
    }

    /// Compare a `u64` exactly.
    pub fn expect_u64(
        &mut self,
        component: &str,
        index: Option<usize>,
        expected: u64,
        actual: u64,
    ) {
        if expected != actual {
            self.record(component, index, format!("{expected}"), format!("{actual}"));
        }
    }

    /// Compare two counts exactly.
    pub fn expect_len(&mut self, component: &str, expected: usize, actual: usize) {
        if expected != actual {
            self.record(component, None, format!("{expected}"), format!("{actual}"));
        }
    }

    /// Compare any two `Eq` values, reporting them with `Debug`.
    pub fn expect_debug_eq<T>(&mut self, component: &str, expected: &T, actual: &T)
    where
        T: PartialEq + std::fmt::Debug,
    {
        if expected != actual {
            self.record(
                component,
                None,
                format!("{expected:?}"),
                format!("{actual:?}"),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::DataOrigin;
    use crate::ml::dataset::{Targets, TrainingSample};
    use crate::ml::features::{RoutingFeatures, FEATURE_DIMENSION};
    use crate::ml::model::SuccessModel;

    // -- helpers --

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
        assert!(
            ll < 0.01,
            "perfect predictions should have near-zero log loss, got {ll}"
        );

        // Perfect predictions -> brier score should be near zero
        let bs = metrics.brier_score.unwrap();
        assert!(
            bs < 0.01,
            "perfect predictions should have near-zero brier score, got {bs}"
        );

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
        assert!(
            (mae - 7.0 / 3.0).abs() < 1e-10,
            "MAE should be 7/3, got {mae}"
        );

        // RMSE = sqrt((4+4+9)/3) = sqrt(17/3)
        let rmse = metrics.rmse.unwrap();
        assert!(
            (rmse - (17.0_f64 / 3.0).sqrt()).abs() < 1e-10,
            "RMSE should be sqrt(17/3), got {rmse}"
        );

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
            (ll - std::f64::consts::LN_2).abs() < 1e-10,
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
        assert!(
            bs.abs() < 1e-10,
            "perfect predictions should have Brier=0, got {bs}"
        );

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

    // -- exact equality primitives --

    #[test]
    fn ulp_distance_is_zero_only_for_identical_values() {
        assert_eq!(ulp_distance(0.5, 0.5), 0);
        assert_eq!(
            ulp_distance(-0.0, 0.0),
            0,
            "the same number is zero ULP apart"
        );
        // Above 1.0 the spacing is 2^-52, so one `EPSILON` is one step.
        assert_eq!(ulp_distance(1.0, 1.0 + f64::EPSILON), 1);
        assert_eq!(ulp_distance(1.0, f64::from_bits(1.0f64.to_bits() + 1)), 1);
        // Below 1.0 the spacing is 2^-53, so the *same* absolute `EPSILON` is
        // two steps. The distance counts representable doubles, not absolute
        // error, which is why it is the right unit to report a float
        // round-trip in.
        assert_eq!(ulp_distance(1.0, 1.0 - f64::EPSILON), 2);
        assert_eq!(ulp_distance(1.0, f64::from_bits(1.0f64.to_bits() - 1)), 1);
        assert_eq!(
            ulp_distance(1.0, 2.0),
            ulp_distance(2.0, 1.0),
            "the distance is symmetric"
        );
        assert!(
            ulp_distance(0.0, 1.0e300) > 1,
            "distant values stay distant rather than saturating to a small number"
        );
        assert!(
            ulp_distance(f64::NAN, f64::NAN) <= 1,
            "two NaNs sharing a bit pattern are close, which is exactly why a \
             non-finite value must be refused before this is consulted"
        );
    }

    #[test]
    fn f64_identical_separates_signed_zero_which_equality_does_not() {
        // Bound first: clippy rightly refuses to be told that a constant is
        // true, and the point of the test is the comparison, not the literal.
        let zero: f64 = 0.0;
        let negative_zero: f64 = -zero;
        assert_eq!(
            zero, negative_zero,
            "numeric equality cannot tell these apart"
        );
        assert!(!f64_identical(zero, negative_zero), "bit identity must");
        assert!(f64_identical(0.1, 0.1));
        assert!(!f64_identical(0.1, 0.1 + f64::EPSILON));
    }

    #[test]
    fn f32_identical_separates_signed_zero_and_one_ulp() {
        assert!(!f32_identical(0.0f32, -0.0f32));
        assert!(f32_identical(0.25f32, 0.25f32));
        assert!(!f32_identical(0.25f32, 0.25f32 + f32::EPSILON));
    }

    #[test]
    fn find_nonfinite_f64_refuses_the_first_in_declared_order() {
        let ordered = [("a", 0.5f64), ("b", 1.0)];
        assert!(find_nonfinite_f64(&ordered).is_ok());

        let dirty = [("a", 0.5f64), ("b", f64::NAN), ("c", f64::INFINITY)];
        let refusal = find_nonfinite_f64(&dirty).expect_err("b is not finite");
        assert_eq!(refusal.component, "b", "the first offender is named");
        assert!(refusal.value.is_nan());
        assert!(refusal.to_string().contains("non-finite measurement at b"));
    }

    #[test]
    fn exactness_keeps_the_first_divergence_and_short_circuits() {
        let mut exactness = Exactness::new();
        exactness.expect_f64("candidates[0].utility.total", Some(0), 1.5, 1.5);
        exactness.expect_str("verdict.selected", None, "alpha", "alpha");
        assert!(exactness.is_exact());

        // A one-ULP difference is a divergence, never a "close enough".
        exactness.expect_f64(
            "candidates[0].utility.total",
            Some(0),
            1.5,
            1.5 + f64::EPSILON,
        );
        exactness.expect_str("verdict.selected", None, "alpha", "bravo");
        let divergence = exactness.divergence().expect("divergence recorded").clone();
        assert_eq!(divergence.component, "candidates[0].utility.total");
        assert_eq!(divergence.index, Some(0));
        assert!(
            !exactness.is_exact(),
            "a later component must not clear an earlier one"
        );
        assert!(divergence.to_string().contains("recorded 1.5 but replayed"));
    }

    #[test]
    fn exactness_into_divergence_is_the_refusal() {
        let mut exactness = Exactness::new();
        exactness.expect_opt_str("candidate.rejection_reason", Some(2), Some("policy"), None);
        let divergence = exactness
            .into_divergence()
            .expect_err("optional discriminants differ");
        assert_eq!(divergence.component, "candidate.rejection_reason");
        assert_eq!(divergence.index, Some(2));
        assert_eq!(divergence.expected, "Some(\"policy\")");
        assert_eq!(divergence.actual, "None");

        assert!(Exactness::new().into_divergence().is_ok());
    }

    #[test]
    fn exactness_compares_counts_and_debug_values() {
        let mut exactness = Exactness::new();
        exactness.expect_len("candidates", 3, 3);
        exactness.expect_u64("verdict.model_commit.hash", None, 42, 42);
        exactness.expect_bool("candidate[1].eligible", Some(1), true, true);
        exactness.expect_debug_eq("verdict.action", &"Keep", &"Keep");
        assert!(exactness.is_exact());

        exactness.expect_len("candidates", 3, 2);
        let divergence = exactness.into_divergence().expect_err("length differs");
        assert_eq!(divergence.component, "candidates");
        assert_eq!(divergence.expected, "3");
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

    // -- FrozenHoldout evaluate tests --

    #[test]
    fn frozen_holdout_evaluate_returns_metrics() {
        let samples = vec![
            make_sample(true, Some(100.0), Some(0.01), 0),
            make_sample(false, None, None, 0),
            make_sample(true, Some(200.0), Some(0.02), 0),
        ];
        let holdout = FrozenHoldout::new(samples, "eval test".to_string());

        let model = SuccessModel::new(FEATURE_DIMENSION);
        let metrics = holdout.evaluate(&model);

        assert_eq!(metrics.sample_count, 3);
        assert!(metrics.log_loss.is_some());
        assert!(metrics.brier_score.is_some());
    }

    #[test]
    fn frozen_holdout_samples_returns_read_only_slice() {
        let samples = vec![
            make_sample(true, Some(100.0), Some(0.01), 0),
            make_sample(false, None, None, 0),
        ];
        let holdout = FrozenHoldout::new(samples, "slice test".to_string());

        let slice = holdout.samples();
        assert_eq!(slice.len(), 2);
        assert!(slice[0].targets.success);
        assert!(!slice[1].targets.success);
    }
}
