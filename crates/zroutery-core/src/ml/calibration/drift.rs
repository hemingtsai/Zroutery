use std::fmt;

use serde::Serialize;

use super::cohort::DecisionCohort;
use super::config::DriftConfig;
use super::errors::CalibrationError;
use super::measure::bin_index;
use super::PSI_PROPORTION_FLOOR;

// ---------------------------------------------------------------------------
// Drift
// ---------------------------------------------------------------------------

/// The drift ceilings, stored on the measurement so the verdict is a pure
/// function of the report rather than of a configuration held elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DriftTolerances {
    /// Bins used for the raw success probability.
    pub bin_count: usize,
    /// Largest tolerated population stability index.
    pub max_population_stability_index: f64,
    /// Largest tolerated change in the attributed rate.
    pub max_base_rate_delta: f64,
}

impl From<&DriftConfig> for DriftTolerances {
    fn from(config: &DriftConfig) -> Self {
        Self {
            bin_count: config.bin_count,
            max_population_stability_index: config.max_population_stability_index,
            max_base_rate_delta: config.max_base_rate_delta,
        }
    }
}

/// Whether the two partitions are the same regime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftVerdict {
    /// Both measured shifts are inside their ceilings.
    WithinTolerance,
    /// At least one measured shift is outside its ceiling.
    Shifted,
}

impl DriftVerdict {
    /// Whether a calibration claim may rest on this measurement.
    pub const fn is_acceptable(self) -> bool {
        matches!(self, Self::WithinTolerance)
    }
}

impl fmt::Display for DriftVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WithinTolerance => f.write_str("within_tolerance"),
            Self::Shifted => f.write_str("shifted"),
        }
    }
}

/// How far the holdout has moved from the partition the calibrator was fitted
/// on.
///
/// A calibrator fitted in one regime and reported on another is not a
/// measurement of anything, so this is measured on every run and the gate
/// refuses when it is out of tolerance. The measurement travels inside the
/// refusal, so a reader sees *how far* it moved and not merely that it did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriftMeasurement {
    /// Bins across `[0, 1]`.
    pub bin_count: usize,
    /// Share of the fit partition's ranked candidates in each bin.
    pub fit_bin_proportions: Vec<f64>,
    /// Share of the holdout's ranked candidates in each bin.
    pub holdout_bin_proportions: Vec<f64>,
    /// The population stability index between the two.
    pub population_stability_index: f64,
    /// Decisions in the fit partition.
    pub fit_cohorts: usize,
    /// Decisions in the holdout.
    pub holdout_cohorts: usize,
    /// Fraction of the fit partition that was served by somebody.
    pub fit_attributed_rate: f64,
    /// Fraction of the holdout that was served by somebody.
    pub holdout_attributed_rate: f64,
    /// Absolute change in the attributed rate.
    pub base_rate_delta: f64,
    /// Mean axis width in the fit partition.
    pub fit_mean_arity: f64,
    /// Mean axis width in the holdout.
    pub holdout_mean_arity: f64,
    /// Absolute change in mean axis width.
    pub arity_delta: f64,
    /// The ceilings this measurement was judged against.
    pub tolerances: DriftTolerances,
}

impl DriftMeasurement {
    /// The verdict, recomputed from the stored numbers and stored ceilings.
    ///
    /// There is no stored boolean, so the stored numbers and the verdict cannot
    /// disagree.
    pub fn verdict(&self) -> DriftVerdict {
        if self.population_stability_index > self.tolerances.max_population_stability_index
            || self.base_rate_delta > self.tolerances.max_base_rate_delta
        {
            DriftVerdict::Shifted
        } else {
            DriftVerdict::WithinTolerance
        }
    }

    /// Whether a calibration claim may rest on this measurement.
    pub fn is_acceptable(&self) -> bool {
        self.verdict().is_acceptable()
    }

    /// The measurement, in words, for the refusal text.
    pub fn summary(&self) -> String {
        format!(
            "psi={:.4} (ceiling {:.4}), base-rate delta={:.4} (ceiling {:.4}), \
             fit attributed rate={:.4}, holdout attributed rate={:.4}, \
             fit arity={:.3}, holdout arity={:.3}",
            self.population_stability_index,
            self.tolerances.max_population_stability_index,
            self.base_rate_delta,
            self.tolerances.max_base_rate_delta,
            self.fit_attributed_rate,
            self.holdout_attributed_rate,
            self.fit_mean_arity,
            self.holdout_mean_arity,
        )
    }
}

impl fmt::Display for DriftMeasurement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.summary())
    }
}

/// Measure the shift between the fit partition and the holdout.
///
/// The population stability index is taken over the *raw* success probability,
/// not over the emitted mass, because the raw probability is the model's own
/// claim and a change in its distribution is a change in the regime rather
/// than an artefact of the calibration.
pub fn measure_drift(
    fit: &[DecisionCohort],
    holdout: &[DecisionCohort],
    config: &DriftConfig,
) -> Result<DriftMeasurement, CalibrationError> {
    if config.bin_count == 0 {
        return Err(CalibrationError::ZeroDriftBins {
            bins: config.bin_count,
        });
    }
    if fit.is_empty() || holdout.is_empty() {
        return Err(CalibrationError::NoObservations {
            context: "the drift comparison",
        });
    }

    let fit_bins = probability_histogram(fit, config.bin_count);
    let holdout_bins = probability_histogram(holdout, config.bin_count);
    let mut index = 0.0_f64;
    for slot in 0..config.bin_count {
        let f = fit_bins
            .get(slot)
            .copied()
            .unwrap_or(0.0)
            .max(PSI_PROPORTION_FLOOR);
        let h = holdout_bins
            .get(slot)
            .copied()
            .unwrap_or(0.0)
            .max(PSI_PROPORTION_FLOOR);
        index += (h - f) * (h / f).ln();
    }

    let fit_rate = attributed_rate(fit);
    let holdout_rate = attributed_rate(holdout);
    let fit_arity = mean_arity(fit);
    let holdout_arity = mean_arity(holdout);

    Ok(DriftMeasurement {
        bin_count: config.bin_count,
        fit_bin_proportions: normalize_proportions(&fit_bins, config.bin_count),
        holdout_bin_proportions: normalize_proportions(&holdout_bins, config.bin_count),
        population_stability_index: index,
        fit_cohorts: fit.len(),
        holdout_cohorts: holdout.len(),
        fit_attributed_rate: fit_rate,
        holdout_attributed_rate: holdout_rate,
        base_rate_delta: (holdout_rate - fit_rate).abs(),
        fit_mean_arity: fit_arity,
        holdout_mean_arity: holdout_arity,
        arity_delta: (holdout_arity - fit_arity).abs(),
        tolerances: DriftTolerances::from(config),
    })
}

fn probability_histogram(cohorts: &[DecisionCohort], bins: usize) -> Vec<f64> {
    let mut counts = vec![0.0_f64; bins];
    let mut total = 0.0_f64;
    for cohort in cohorts {
        for input in cohort.candidates() {
            if let Some(raw) = input.raw_success_probability() {
                if raw.is_finite() {
                    counts[bin_index(raw.clamp(0.0, 1.0), bins)] += 1.0;
                    total += 1.0;
                }
            }
        }
    }
    if total > 0.0 {
        for count in &mut counts {
            *count /= total;
        }
    }
    counts
}

fn normalize_proportions(counts: &[f64], bins: usize) -> Vec<f64> {
    let mut proportions: Vec<f64> = (0..bins)
        .map(|slot| counts.get(slot).copied().unwrap_or(0.0))
        .collect();
    let total: f64 = proportions.iter().sum();
    if total > 0.0 {
        for value in &mut proportions {
            *value /= total;
        }
    }
    proportions
}

pub(super) fn attributed_rate(cohorts: &[DecisionCohort]) -> f64 {
    if cohorts.is_empty() {
        return 0.0;
    }
    let attributed = cohorts
        .iter()
        .filter(|cohort| cohort.is_attributed())
        .count();
    attributed as f64 / cohorts.len() as f64
}

pub(super) fn mean_arity(cohorts: &[DecisionCohort]) -> f64 {
    if cohorts.is_empty() {
        return 0.0;
    }
    cohorts.iter().map(|cohort| cohort.arity()).sum::<usize>() as f64 / cohorts.len() as f64
}
