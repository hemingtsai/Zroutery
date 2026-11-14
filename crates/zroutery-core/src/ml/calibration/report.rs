use std::fmt;

use serde::Serialize;

use super::config::ReliabilityConfig;
use super::drift::DriftMeasurement;
use super::kway::KWayCalibrator;
use super::marginal::{MarginalCalibration, MarginalCalibrator, NormalizationDamage};
use super::measure::{CalibrationMeasure, DistributionRecord, EmittedDecision};
use super::DISTRIBUTION_ROLE;

// ---------------------------------------------------------------------------
// Verdict and report
// ---------------------------------------------------------------------------

/// The ceilings the verdict is recomputed against, stored on the report.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AcceptanceTolerances {
    /// Largest tolerated expected calibration error of the final vector.
    pub max_expected_calibration_error: f64,
    /// Largest tolerated single-bin gap of the final vector.
    pub max_calibration_error: f64,
    /// Largest tolerated single-*candidate* gap of the final vector.
    ///
    /// Separate from the bin ceiling on purpose. The binned curve can average
    /// two candidates' errors into one row, so a ceiling on bins alone can be
    /// met by a vector that is badly wrong about one named candidate. This
    /// ceiling is bin-free and is checked on the same measurement.
    pub max_candidate_calibration_error: f64,
}

impl From<&ReliabilityConfig> for AcceptanceTolerances {
    fn from(config: &ReliabilityConfig) -> Self {
        Self {
            max_expected_calibration_error: config.max_expected_calibration_error,
            max_calibration_error: config.max_calibration_error,
            max_candidate_calibration_error: config.max_candidate_calibration_error,
        }
    }
}

/// Whether the **final emitted vector** is calibrated.
///
/// Two values, and no third. A measurement that cannot be judged is reported
/// as [`CalibrationVerdict::Miscalibrated`], because "we cannot show it is
/// calibrated" and "it is calibrated" are different claims and only one of them
/// is supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationVerdict {
    /// Both ceilings hold on the final vector.
    Calibrated,
    /// At least one ceiling is broken, or the measurement cannot be judged.
    Miscalibrated,
}

impl CalibrationVerdict {
    /// The whole of the acceptance rule, as a pure function of the final
    /// measurement and the ceilings.
    ///
    /// It reads `final_vector` and nothing else. The marginal measurements are
    /// not inputs, deliberately: a route that happens to look good before
    /// normalization cannot make the emitted vector calibrated.
    pub fn from_measurements(
        final_vector: &CalibrationMeasure,
        tolerances: &AcceptanceTolerances,
    ) -> Self {
        if final_vector.passes(tolerances) {
            Self::Calibrated
        } else {
            Self::Miscalibrated
        }
    }

    /// Whether this verdict is a calibration claim.
    pub const fn is_calibrated(self) -> bool {
        matches!(self, Self::Calibrated)
    }
}

impl fmt::Display for CalibrationVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Calibrated => f.write_str("calibrated"),
            Self::Miscalibrated => f.write_str("miscalibrated"),
        }
    }
}

/// The honest report of one offline calibration run.
///
/// Every field is a function of the snapshot, the model, and the configuration.
/// There is no timestamp, no duration, and no run counter, so two runs of the
/// same input produce two identical reports.
///
/// `PartialEq` is deliberately absent, for the same reason as on
/// [`CalibrationMeasure`].
#[derive(Debug, Clone, Serialize)]
pub struct CalibrationReport {
    /// Decisions in the snapshot.
    pub cohorts_total: usize,
    /// Decisions the calibrator was fitted on.
    pub fit_cohorts: usize,
    /// Decisions the calibrator was measured on.
    pub holdout_cohorts: usize,
    /// Fit decisions that were served.
    pub fit_attributed_cohorts: usize,
    /// Holdout decisions that were served.
    pub holdout_attributed_cohorts: usize,
    /// Ranked candidate observations across both partitions.
    pub ranked_observations: usize,
    /// Candidates that entered an emitted axis with exactly zero mass.
    pub unranked_candidates_emitted: usize,
    /// Fraction of the fit partition that was served.
    pub fit_attributed_rate: f64,
    /// Fraction of the holdout that was served.
    pub holdout_attributed_rate: f64,

    /// **The headline.** Calibration of the final emitted, normalized vector,
    /// measured on data disjoint from the fit.
    pub final_vector: CalibrationMeasure,
    /// The same parameterization with nothing fitted, measured the same way.
    /// A diagnostic of what the fit bought, not a substitute for the headline.
    pub uncalibrated_joint: CalibrationMeasure,
    /// The independent per-candidate route, before normalizing. Subordinate.
    pub marginal_calibrated: MarginalCalibration,
    /// The independent per-candidate route, after normalizing. Subordinate.
    pub marginal_normalized: MarginalCalibration,
    /// What the normalization step cost the independent route.
    pub normalization_damage: NormalizationDamage,
    /// The shift between the two partitions.
    pub drift: DriftMeasurement,
    /// The ceilings the verdict is recomputed against.
    pub tolerances: AcceptanceTolerances,
    /// The recorded verdict.
    pub verdict: CalibrationVerdict,
    /// What the emitted distribution is, restated in every report.
    pub distribution_role: String,
}

impl CalibrationReport {
    /// The verdict, recomputed from the stored final measurement and the stored
    /// ceilings.
    ///
    /// [`CalibrationReport::verdict`] is the recorded value; this is the
    /// derived one. A test asserts the two agree, so the recorded value cannot
    /// drift away from the arithmetic that produced it.
    pub fn recomputed_verdict(&self) -> CalibrationVerdict {
        CalibrationVerdict::from_measurements(&self.final_vector, &self.tolerances)
    }

    /// Whether the final emitted vector is calibrated.
    ///
    /// Asks [`CalibrationReport::recomputed_verdict`], not the stored field.
    pub fn is_calibrated(&self) -> bool {
        self.recomputed_verdict().is_calibrated()
    }

    /// `(before, after)` expected calibration error of the independent route,
    /// or `None` when the fit produced no uncalibrated baseline.
    pub fn improvement_over_uncalibrated(&self) -> Option<(f64, f64)> {
        Some((
            self.uncalibrated_joint.expected_calibration_error(),
            self.final_vector.expected_calibration_error(),
        ))
    }

    /// The headline, in one line.
    pub fn headline(&self) -> String {
        format!(
            "final vector ece={:.4} mce={:.4} worst-candidate={:.4} verdict={}; \
             uncalibrated joint ece={:.4}; normalization damage delta={:+.4}; {}",
            self.final_vector.expected_calibration_error(),
            self.final_vector
                .maximum_calibration_error()
                .unwrap_or(f64::NAN),
            self.final_vector
                .maximum_candidate_calibration_error()
                .unwrap_or(f64::NAN),
            self.recomputed_verdict(),
            self.uncalibrated_joint.expected_calibration_error(),
            self.normalization_damage.delta,
            self.drift.summary(),
        )
    }
}

/// Everything one offline calibration run produced.
pub struct CalibrationOutcome {
    pub(super) calibrator: KWayCalibrator,
    pub(super) marginal_calibrator: MarginalCalibrator,
    pub(super) holdout: Vec<EmittedDecision>,
    pub(super) report: CalibrationReport,
}

impl fmt::Debug for CalibrationOutcome {
    /// Structural, without dumping every probability vector.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CalibrationOutcome")
            .field("temperature", &self.calibrator.temperature())
            .field("intercepts", &self.calibrator.intercepts().len())
            .field("holdout_distributions", &self.holdout.len())
            .field("verdict", &self.report.recomputed_verdict())
            .field("role", &DISTRIBUTION_ROLE)
            .finish()
    }
}

impl CalibrationOutcome {
    /// The fitted joint calibrator.
    pub fn calibrator(&self) -> &KWayCalibrator {
        &self.calibrator
    }

    /// The fitted independent per-candidate map. Priced, not claimed.
    pub fn marginal_calibrator(&self) -> &MarginalCalibrator {
        &self.marginal_calibrator
    }

    /// The emitted holdout distributions, as validated 7E-2A values.
    pub fn holdout_distributions(&self) -> &[EmittedDecision] {
        &self.holdout
    }

    /// The emitted holdout distributions in the validating wire form.
    pub fn holdout_records(&self) -> Vec<DistributionRecord> {
        self.holdout
            .iter()
            .map(EmittedDecision::to_record)
            .collect()
    }

    /// The honest report.
    pub fn report(&self) -> &CalibrationReport {
        &self.report
    }

    /// Whether the final emitted vector is calibrated, recomputed.
    pub fn is_calibrated(&self) -> bool {
        self.report.is_calibrated()
    }
}
