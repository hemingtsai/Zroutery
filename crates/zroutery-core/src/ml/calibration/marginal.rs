use serde::Serialize;

use super::cohort::DecisionCohort;
use super::config::{MarginalFitConfig, ReliabilityConfig};
use super::errors::CalibrationError;
use super::kway::log_odds;
use super::measure::{build_curve, CandidateCalibration, ReliabilityCurve};
use super::report::AcceptanceTolerances;
use super::LOG_LOSS_CLAMP;
use crate::ml::evaluation::PredictionMetrics;
use crate::outcome::CandidateIdentity;

// ---------------------------------------------------------------------------
// The independent per-candidate route, measured only to price it
// ---------------------------------------------------------------------------

/// A shared 1-D Platt map on the log-odds: `p = sigmoid(a + b * logit(raw))`.
///
/// One map for every candidate on purpose. A per-candidate map would be fitted
/// on a handful of rows each and would flatter the naive route with its own
/// overfitting, which would make the measured normalization damage smaller than
/// it really is. This is the most defensible form of "calibrate each candidate
/// independently", so the number it produces is an upper bound on how good that
/// route can look.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MarginalCalibrator {
    intercept: f64,
    slope: f64,
    iterations: usize,
    initial_log_loss: f64,
    final_log_loss: f64,
    observations: usize,
}

impl MarginalCalibrator {
    /// Fit the shared map on the fit partition's candidate observations.
    pub fn fit(
        observations: &[(f64, bool)],
        config: &MarginalFitConfig,
        probability_floor: f64,
    ) -> Result<Self, CalibrationError> {
        if observations.is_empty() {
            return Err(CalibrationError::NoObservations {
                context: "the fit partition's candidate marginals",
            });
        }
        if config.iterations == 0 {
            return Err(CalibrationError::ZeroFitIterations {
                iterations: config.iterations,
            });
        }
        if !config.learning_rate.is_finite() || config.learning_rate <= 0.0 {
            return Err(CalibrationError::InvalidLearningRate {
                value: config.learning_rate,
            });
        }

        let logits: Vec<f64> = observations
            .iter()
            .map(|(raw, _)| log_odds(*raw, probability_floor))
            .collect::<Result<Vec<f64>, _>>()?;

        let mut intercept = 0.0_f64;
        let mut slope = 1.0_f64;
        for _ in 0..config.iterations {
            let mut gradient_intercept = 0.0_f64;
            let mut gradient_slope = 0.0_f64;
            for (index, (_, actual)) in observations.iter().enumerate() {
                let probability = sigmoid(intercept + slope * logits[index]);
                let residual = probability - f64::from(*actual);
                gradient_intercept += residual;
                gradient_slope += residual * logits[index];
            }
            let scale = 1.0 / observations.len() as f64;
            intercept -= config.learning_rate * gradient_intercept * scale;
            slope -= config.learning_rate * gradient_slope * scale;
        }

        // The identity map is the starting point, so its loss is the number the
        // fit has to beat. It is measured the same way as the final one, on the
        // same rows, rather than accumulated inside the loop.
        let initial_loss = marginal_log_loss(&logits, observations, 0.0, 1.0);
        let final_loss = marginal_log_loss(&logits, observations, intercept, slope);

        if !final_loss.is_finite() {
            return Err(CalibrationError::FitDiverged {
                reason: "the fitted marginal log loss is not finite".to_string(),
            });
        }

        Ok(Self {
            intercept,
            slope,
            iterations: config.iterations,
            initial_log_loss: initial_loss,
            final_log_loss: final_loss,
            observations: observations.len(),
        })
    }

    /// Apply the map to one raw probability.
    pub fn apply(&self, raw: f64, probability_floor: f64) -> Result<f64, CalibrationError> {
        let logit = log_odds(raw, probability_floor)?;
        Ok(sigmoid(self.intercept + self.slope * logit)
            .clamp(probability_floor, 1.0 - probability_floor))
    }

    /// The fitted intercept.
    pub fn intercept(&self) -> f64 {
        self.intercept
    }

    /// The fitted slope.
    pub fn slope(&self) -> f64 {
        self.slope
    }

    /// Observations the map was fitted on.
    pub fn observations(&self) -> usize {
        self.observations
    }

    /// Mean per-candidate log loss after fitting.
    pub fn final_log_loss(&self) -> f64 {
        self.final_log_loss
    }

    /// Mean per-candidate log loss before the first step.
    pub fn initial_log_loss(&self) -> f64 {
        self.initial_log_loss
    }
}

/// Mean per-candidate log loss of one `(intercept, slope)` pair.
fn marginal_log_loss(
    logits: &[f64],
    observations: &[(f64, bool)],
    intercept: f64,
    slope: f64,
) -> f64 {
    if observations.is_empty() {
        return 0.0;
    }
    let total: f64 = observations
        .iter()
        .enumerate()
        .map(|(index, (_, actual))| {
            let logit = logits.get(index).copied().unwrap_or(0.0);
            binary_log_loss(sigmoid(intercept + slope * logit), *actual)
        })
        .sum();
    total / observations.len() as f64
}

fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exponent = value.exp();
        exponent / (1.0 + exponent)
    }
}

fn binary_log_loss(probability: f64, actual: bool) -> f64 {
    let p = probability.clamp(LOG_LOSS_CLAMP, 1.0 - LOG_LOSS_CLAMP);
    let y = f64::from(actual);
    -(y * p.ln() + (1.0 - y) * (1.0 - p).ln())
}

/// One candidate's marginal numbers, before and after normalization.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MarginalObservation {
    /// The candidate.
    pub candidate: CandidateIdentity,
    /// Whether this candidate served its decision.
    pub served: bool,
    /// The success head's raw output.
    pub raw: f64,
    /// After the shared 1-D map, before normalization.
    pub calibrated: f64,
    /// After dividing by its decision's total calibrated mass.
    pub normalized: f64,
}

/// Walk a partition and collect both marginal routes at once, so the two
/// measurements are taken over exactly the same observations.
pub fn collect_marginal_observations(
    cohorts: &[DecisionCohort],
    calibrator: &MarginalCalibrator,
    probability_floor: f64,
) -> Result<Vec<MarginalObservation>, CalibrationError> {
    let mut collected = Vec::new();
    for cohort in cohorts {
        let calibrated: Vec<f64> = cohort
            .candidates()
            .iter()
            .filter_map(|input| input.raw_success_probability())
            .map(|raw| calibrator.apply(raw, probability_floor))
            .collect::<Result<Vec<f64>, _>>()?;
        let total: f64 = calibrated.iter().sum();
        if !total.is_finite() || total <= 0.0 {
            return Err(CalibrationError::Measurement {
                context: "the marginal route",
                reason: format!(
                    "decision {} calibrated mass summed to {total}, so it cannot be normalized",
                    cohort.fingerprint()
                ),
            });
        }
        let mut cursor = 0usize;
        for input in cohort.candidates() {
            let Some(raw) = input.raw_success_probability() else {
                continue;
            };
            let value = calibrated.get(cursor).copied().unwrap_or(0.0);
            collected.push(MarginalObservation {
                candidate: input.candidate().clone(),
                served: cohort.served() == Some(input.candidate()),
                raw,
                calibrated: value,
                normalized: value / total,
            });
            cursor += 1;
        }
    }
    Ok(collected)
}

/// Which column of a [`MarginalObservation`] to measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MarginalView {
    /// After the shared 1-D map, before normalization.
    Calibrated,
    /// After dividing by the decision's total.
    Normalized,
}

/// The measurement of one marginal route.
///
/// Deliberately a different type from [`CalibrationMeasure`], which
/// `measure_emitted` does not accept. A marginal number and a final vector
/// cannot be passed to the same function, so the headline can never be
/// substituted for a diagnostic.
#[derive(Debug, Clone, Serialize)]
pub struct MarginalCalibration {
    /// Which column was measured.
    pub view: MarginalView,
    /// Candidate observations measured.
    pub observations: usize,
    /// The same binned curve, with the same definition.
    pub curve: ReliabilityCurve,
    /// The same bin-free per-candidate table, with the same definition.
    pub per_candidate: Vec<CandidateCalibration>,
    /// The same pooled probability metrics, from the same constructor.
    pub pooled: PredictionMetrics,
}

impl MarginalCalibration {
    /// The count-weighted mean absolute gap.
    pub fn expected_calibration_error(&self) -> f64 {
        self.curve.expected_calibration_error()
    }

    /// The largest absolute bin gap, or `None` on an empty curve.
    pub fn maximum_calibration_error(&self) -> Option<f64> {
        self.curve.maximum_calibration_error()
    }

    /// The largest absolute per-candidate gap. Bin-free.
    pub fn maximum_candidate_calibration_error(&self) -> Option<f64> {
        self.per_candidate
            .iter()
            .filter(|row| row.observations > 0)
            .map(|row| row.gap.abs())
            .fold(None, |worst: Option<f64>, gap| {
                Some(worst.map_or(gap, |current: f64| current.max(gap)))
            })
    }

    /// The same acceptance rule, applied to a marginal route.
    ///
    /// Exists so the two routes can be judged under one rule rather than under
    /// two, which is the only way the comparison between them means anything.
    pub fn passes(&self, tolerances: &AcceptanceTolerances) -> bool {
        match (
            self.maximum_calibration_error(),
            self.maximum_candidate_calibration_error(),
        ) {
            (Some(worst_bin), Some(worst_candidate)) => {
                self.expected_calibration_error() <= tolerances.max_expected_calibration_error
                    && worst_bin <= tolerances.max_calibration_error
                    && worst_candidate <= tolerances.max_candidate_calibration_error
            }
            _ => false,
        }
    }
}

/// Measure one marginal route.
///
/// Same bins, same gap definition, same pooled constructor as
/// [`measure_emitted`], so the two numbers are comparable and the difference
/// between them is attributable to normalization alone.
pub fn measure_marginal(
    observations: &[MarginalObservation],
    view: MarginalView,
    config: &ReliabilityConfig,
) -> Result<MarginalCalibration, CalibrationError> {
    if config.bin_count == 0 {
        return Err(CalibrationError::ZeroReliabilityBins {
            bins: config.bin_count,
        });
    }
    if observations.is_empty() {
        return Err(CalibrationError::NoObservations {
            context: "the marginal route",
        });
    }
    let predictions: Vec<f64> = observations
        .iter()
        .map(|observation| match view {
            MarginalView::Calibrated => observation.calibrated,
            MarginalView::Normalized => observation.normalized,
        })
        .collect();
    let actuals: Vec<bool> = observations
        .iter()
        .map(|observation| observation.served)
        .collect();
    let pooled =
        PredictionMetrics::try_compute_classification(&predictions, &actuals).map_err(|error| {
            CalibrationError::Measurement {
                context: "the marginal route",
                reason: error.to_string(),
            }
        })?;
    Ok(MarginalCalibration {
        view,
        observations: observations.len(),
        curve: build_curve(&predictions, &actuals, config.bin_count),
        per_candidate: marginal_candidate_rows(observations, view),
        pooled,
    })
}

/// The bin-free per-candidate table for one marginal view.
fn marginal_candidate_rows(
    observations: &[MarginalObservation],
    view: MarginalView,
) -> Vec<CandidateCalibration> {
    let mut rows: Vec<CandidateCalibration> = Vec::new();
    for observation in observations {
        let predicted = match view {
            MarginalView::Calibrated => observation.calibrated,
            MarginalView::Normalized => observation.normalized,
        };
        let slot = match rows
            .iter()
            .position(|row| row.candidate == observation.candidate)
        {
            Some(slot) => slot,
            None => {
                rows.push(CandidateCalibration {
                    candidate: observation.candidate.clone(),
                    observations: 0,
                    served: 0,
                    mean_predicted: 0.0,
                    observed_frequency: 0.0,
                    gap: 0.0,
                });
                rows.len() - 1
            }
        };
        let row = &mut rows[slot];
        row.observations += 1;
        row.mean_predicted += predicted;
        if observation.served {
            row.served += 1;
        }
    }
    for row in &mut rows {
        if row.observations > 0 {
            row.mean_predicted /= row.observations as f64;
            row.observed_frequency = row.served as f64 / row.observations as f64;
            row.gap = row.mean_predicted - row.observed_frequency;
        }
    }
    rows.sort_by(|left, right| {
        left.candidate
            .provider()
            .cmp(right.candidate.provider())
            .then_with(|| left.candidate.model().cmp(right.candidate.model()))
    });
    rows
}

/// What normalizing an independently-calibrated vector costs.
///
/// `delta` is `ece(after) - ece(before)`, so a positive delta is calibration
/// damage done by the normalization step itself. This is the number that
/// answers "does normalizing make it worse", measured rather than argued.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct NormalizationDamage {
    /// Expected calibration error of the calibrated vector before normalizing.
    pub expected_calibration_error_before: f64,
    /// Expected calibration error of the same vector after normalizing.
    pub expected_calibration_error_after: f64,
    /// `after - before`. Positive means the normalization degraded calibration.
    pub delta: f64,
    /// Largest single-bin gap before normalizing.
    pub maximum_calibration_error_before: Option<f64>,
    /// Largest single-bin gap after normalizing.
    pub maximum_calibration_error_after: Option<f64>,
    /// Largest single-candidate gap before normalizing. Bin-free.
    pub maximum_candidate_error_before: Option<f64>,
    /// Largest single-candidate gap after normalizing. Bin-free.
    pub maximum_candidate_error_after: Option<f64>,
}

impl NormalizationDamage {
    /// Price the normalization step.
    pub fn measure(before: &MarginalCalibration, after: &MarginalCalibration) -> Self {
        let before_ece = before.expected_calibration_error();
        let after_ece = after.expected_calibration_error();
        Self {
            expected_calibration_error_before: before_ece,
            expected_calibration_error_after: after_ece,
            delta: after_ece - before_ece,
            maximum_calibration_error_before: before.maximum_calibration_error(),
            maximum_calibration_error_after: after.maximum_calibration_error(),
            maximum_candidate_error_before: before.maximum_candidate_calibration_error(),
            maximum_candidate_error_after: after.maximum_candidate_calibration_error(),
        }
    }

    /// Whether normalization made the vector *worse* calibrated.
    pub fn degraded_calibration(&self) -> bool {
        self.delta > 0.0
    }

    /// The bin-free view of the same question: the change in the worst single
    /// candidate's gap. Positive means normalization made some named candidate
    /// worse, which the binned `delta` can average away.
    pub fn candidate_delta(&self) -> Option<f64> {
        match (
            self.maximum_candidate_error_before,
            self.maximum_candidate_error_after,
        ) {
            (Some(before), Some(after)) => Some(after - before),
            _ => None,
        }
    }
}
