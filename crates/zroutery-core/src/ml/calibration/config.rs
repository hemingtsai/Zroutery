use serde::Serialize;

use super::errors::CalibrationError;
use super::{DEFAULT_DRIFT_BINS, DEFAULT_PROBABILITY_FLOOR, DEFAULT_RELIABILITY_BINS};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// How the joint calibrator is fitted.
///
/// Every field is a fixed count or a fixed number, never a seed and never a
/// wall clock. That is what makes the fit reproducible without a random source:
/// the same partition and the same configuration give the same parameters,
/// because the iteration count is the stopping rule.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct FitConfig {
    /// Full-batch gradient steps. Must be at least 1.
    pub iterations: usize,
    /// Step size. Must be finite and positive.
    pub learning_rate: f64,
    /// L2 pull on the intercepts toward zero. Must be finite and
    /// non-negative. At zero, a candidate the fit partition never sees serving
    /// is driven arbitrarily far down by its own absence.
    pub intercept_l2: f64,
    /// Lower clamp on the temperature. Must be finite and positive.
    pub min_temperature: f64,
    /// Upper clamp on the temperature. Must be finite and above the floor.
    pub max_temperature: f64,
    /// Symmetric clamp on each intercept. Must be finite and positive.
    pub max_abs_intercept: f64,
}

impl Default for FitConfig {
    fn default() -> Self {
        Self {
            iterations: 800,
            learning_rate: 0.5,
            intercept_l2: 0.02,
            min_temperature: 0.05,
            max_temperature: 20.0,
            max_abs_intercept: 8.0,
        }
    }
}

/// How the *independent* per-candidate route is fitted.
///
/// Present so the cost of that route is a measured number. It is never the
/// claim.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MarginalFitConfig {
    /// Full-batch gradient steps on the shared 1-D map.
    pub iterations: usize,
    /// Step size. Must be finite and positive.
    pub learning_rate: f64,
}

impl Default for MarginalFitConfig {
    fn default() -> Self {
        Self {
            iterations: 800,
            learning_rate: 0.5,
        }
    }
}

/// The acceptance ceilings for the final vector, and the binning of the curve.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ReliabilityConfig {
    /// Bins across `[0, 1]`. Must be at least 1.
    pub bin_count: usize,
    /// Largest tolerated expected calibration error.
    pub max_expected_calibration_error: f64,
    /// Largest tolerated single-bin calibration error.
    pub max_calibration_error: f64,
    /// Largest tolerated single-candidate calibration error, measured without
    /// binning. Must be finite and non-negative.
    ///
    /// Separate from the bin ceiling because a binned curve can average two
    /// candidates into one row: with three candidates and ten bins, two masses
    /// that land together and err in opposite directions produce a bin gap of
    /// zero. A ceiling on bins alone would then be met by a vector that is
    /// badly wrong about a named candidate.
    pub max_candidate_calibration_error: f64,
}

impl Default for ReliabilityConfig {
    fn default() -> Self {
        Self {
            bin_count: DEFAULT_RELIABILITY_BINS,
            max_expected_calibration_error: 0.10,
            max_calibration_error: 0.25,
            max_candidate_calibration_error: 0.15,
        }
    }
}

/// The drift gate between the fit partition and the holdout.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DriftConfig {
    /// Bins across `[0, 1]` for the raw success probability. Must be at least 1.
    pub bin_count: usize,
    /// Largest tolerated population stability index. The conventional reading
    /// is below 0.1 as "stable", 0.1 to 0.25 as "shifted but explicable", and
    /// above 0.25 as "shifted".
    pub max_population_stability_index: f64,
    /// Largest tolerated absolute change in the fraction of decisions that were
    /// served by anybody.
    pub max_base_rate_delta: f64,
}

impl Default for DriftConfig {
    fn default() -> Self {
        Self {
            bin_count: DEFAULT_DRIFT_BINS,
            max_population_stability_index: 0.25,
            max_base_rate_delta: 0.20,
        }
    }
}

/// How the snapshot is split, and what each partition must contain.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct HoldoutConfig {
    /// Trailing decisions reserved as the holdout, after the canonical content
    /// order is applied. Must be at least 1.
    pub holdout_cohorts: usize,
    /// Fewest decisions the fit partition may hold. Must be at least 1.
    pub min_fit_cohorts: usize,
    /// Fewest served decisions each partition must hold. Must be at least 1.
    pub min_attributed_outcomes: usize,
}

impl Default for HoldoutConfig {
    fn default() -> Self {
        Self {
            holdout_cohorts: 12,
            min_fit_cohorts: 8,
            min_attributed_outcomes: 4,
        }
    }
}

/// Everything one offline calibration run is configured with.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CalibrationConfig {
    /// The partition.
    pub holdout: HoldoutConfig,
    /// The joint fit.
    pub fit: FitConfig,
    /// The independent per-candidate fit, measured only to price it.
    pub marginal: MarginalFitConfig,
    /// The calibration gate.
    pub reliability: ReliabilityConfig,
    /// The drift gate.
    pub drift: DriftConfig,
    /// Clamp applied to a finite in-range probability before it is logged.
    pub probability_floor: f64,
}

impl Default for CalibrationConfig {
    fn default() -> Self {
        Self {
            holdout: HoldoutConfig::default(),
            fit: FitConfig::default(),
            marginal: MarginalFitConfig::default(),
            reliability: ReliabilityConfig::default(),
            drift: DriftConfig::default(),
            probability_floor: DEFAULT_PROBABILITY_FLOOR,
        }
    }
}

impl CalibrationConfig {
    /// Every configuration that would make a gate meaningless.
    ///
    /// Checked before anything is read, so a vacuous gate is refused rather
    /// than reported. A ceiling that is negative could never be met and one
    /// that is infinite could always be met; a holdout that consumes the whole
    /// snapshot would leave the calibrator unfitted while still reporting a
    /// verdict; zero gradient steps would report an unfitted calibrator as
    /// fitted; and a zero temperature would collapse the vector onto one
    /// candidate and call the result calibrated.
    pub fn checked(&self) -> Result<(), CalibrationError> {
        if self.reliability.bin_count == 0 {
            return Err(CalibrationError::ZeroReliabilityBins {
                bins: self.reliability.bin_count,
            });
        }
        if self.drift.bin_count == 0 {
            return Err(CalibrationError::ZeroDriftBins {
                bins: self.drift.bin_count,
            });
        }
        if !self.reliability.max_expected_calibration_error.is_finite()
            || self.reliability.max_expected_calibration_error < 0.0
        {
            return Err(CalibrationError::InvalidEceCeiling {
                value: self.reliability.max_expected_calibration_error,
            });
        }
        if !self.reliability.max_calibration_error.is_finite()
            || self.reliability.max_calibration_error < 0.0
        {
            return Err(CalibrationError::InvalidMceCeiling {
                value: self.reliability.max_calibration_error,
            });
        }
        if !self.reliability.max_candidate_calibration_error.is_finite()
            || self.reliability.max_candidate_calibration_error < 0.0
        {
            return Err(CalibrationError::InvalidCandidateCeiling {
                value: self.reliability.max_candidate_calibration_error,
            });
        }
        if !self.drift.max_population_stability_index.is_finite()
            || self.drift.max_population_stability_index < 0.0
        {
            return Err(CalibrationError::InvalidPsiCeiling {
                value: self.drift.max_population_stability_index,
            });
        }
        if !self.drift.max_base_rate_delta.is_finite()
            || self.drift.max_base_rate_delta < 0.0
            || self.drift.max_base_rate_delta > 1.0
        {
            return Err(CalibrationError::InvalidBaseRateCeiling {
                value: self.drift.max_base_rate_delta,
            });
        }
        if self.holdout.holdout_cohorts == 0 {
            return Err(CalibrationError::ZeroHoldoutCohortCount {
                holdout: self.holdout.holdout_cohorts,
            });
        }
        if self.holdout.min_fit_cohorts == 0 {
            return Err(CalibrationError::ZeroFitCohortFloor {
                min_fit: self.holdout.min_fit_cohorts,
            });
        }
        // A split no snapshot could ever satisfy. Checked with `checked_add`
        // rather than `+`, because an overflowing sum would panic here instead
        // of refusing, and this function's whole contract is that it refuses.
        if self
            .holdout
            .holdout_cohorts
            .checked_add(self.holdout.min_fit_cohorts)
            .is_none()
        {
            return Err(CalibrationError::MeaninglessHoldoutSplit {
                holdout: self.holdout.holdout_cohorts,
                min_fit: self.holdout.min_fit_cohorts,
            });
        }
        if self.holdout.min_attributed_outcomes == 0 {
            return Err(CalibrationError::ZeroAttributedFloor {
                minimum: self.holdout.min_attributed_outcomes,
            });
        }
        if self.fit.iterations == 0 || self.marginal.iterations == 0 {
            return Err(CalibrationError::ZeroFitIterations {
                iterations: self.fit.iterations.min(self.marginal.iterations),
            });
        }
        if !self.fit.learning_rate.is_finite() || self.fit.learning_rate <= 0.0 {
            return Err(CalibrationError::InvalidLearningRate {
                value: self.fit.learning_rate,
            });
        }
        if !self.marginal.learning_rate.is_finite() || self.marginal.learning_rate <= 0.0 {
            return Err(CalibrationError::InvalidLearningRate {
                value: self.marginal.learning_rate,
            });
        }
        if !self.fit.intercept_l2.is_finite() || self.fit.intercept_l2 < 0.0 {
            return Err(CalibrationError::InvalidInterceptL2 {
                value: self.fit.intercept_l2,
            });
        }
        if !self.fit.max_abs_intercept.is_finite() || self.fit.max_abs_intercept <= 0.0 {
            return Err(CalibrationError::InvalidInterceptBound {
                value: self.fit.max_abs_intercept,
            });
        }
        if !self.fit.min_temperature.is_finite() || self.fit.min_temperature <= 0.0 {
            return Err(CalibrationError::InvalidMinTemperature {
                value: self.fit.min_temperature,
            });
        }
        if !self.fit.max_temperature.is_finite()
            || self.fit.max_temperature <= self.fit.min_temperature
        {
            return Err(CalibrationError::InvalidMaxTemperature {
                value: self.fit.max_temperature,
                min: self.fit.min_temperature,
            });
        }
        if !self.probability_floor.is_finite()
            || self.probability_floor <= 0.0
            || self.probability_floor >= 0.5
        {
            return Err(CalibrationError::InvalidProbabilityFloor {
                floor: self.probability_floor,
            });
        }
        Ok(())
    }
}
