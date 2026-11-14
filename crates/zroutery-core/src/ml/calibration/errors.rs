use super::drift::DriftMeasurement;
use super::types::{DegeneracyReason, PartitionKind};
use crate::ml::decision_contract::DecisionContractError;

// ---------------------------------------------------------------------------
// CalibrationError
// ---------------------------------------------------------------------------

/// Every way a calibration run refuses.
///
/// The run fails closed. A refusal is a typed value carrying the numbers that
/// caused it: there is no panic, no silently defaulted vector, and no
/// calibrated-looking artifact handed back as a success. The only place the
/// arithmetic can go wrong without a refusal is inside
/// [`DecisionDistribution::try_new`], and that refusal is surfaced here as
/// [`CalibrationError::DistributionRejected`] rather than unwrapped.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CalibrationError {
    /// The snapshot held no candidate rows at all.
    #[error("calibration snapshot is empty")]
    EmptySnapshot,

    /// The snapshot held one sample id twice. A repeated row would put one
    /// candidate into a cohort twice, so the K axis would no longer be the set
    /// of candidates that were actually compared.
    #[error(
        "calibration snapshot holds sample id '{sample_id}' at indexes {first} and {second}; \
         a repeated row would put one candidate into a cohort twice"
    )]
    DuplicateSampleId {
        sample_id: String,
        first: usize,
        second: usize,
    },

    /// A row was encoded against a different schema than this build accepts.
    #[error("calibration sample {index} carries {component} schema version {found}, expected {expected}")]
    SchemaMismatch {
        index: usize,
        component: &'static str,
        found: u32,
        expected: u32,
    },

    /// A row's feature vector is not the width the models are built for.
    #[error(
        "calibration sample {index} carries a feature vector of width {found}, expected {expected}"
    )]
    FeatureDimension {
        index: usize,
        found: usize,
        expected: usize,
    },

    /// A row's feature vector holds a value no model may score.
    #[error("calibration sample {index} has a non-finite feature at position {position}: {value}")]
    NonFiniteFeature {
        index: usize,
        position: usize,
        value: f32,
    },

    /// The canonical sample failed its own validator.
    #[error("calibration sample {index} is invalid: {reason}")]
    InvalidSample { index: usize, reason: String },

    /// The model produced `NaN` or an infinity. Never clamped into range: a
    /// non-finite prediction is a broken model, not a small probability.
    #[error(
        "the success model produced a non-finite probability for candidate '{candidate}' \
         in cohort {cohort}: {value}"
    )]
    NonFinitePrediction {
        cohort: String,
        candidate: String,
        value: f64,
    },

    /// The model produced something outside `[0, 1]`, so it is not a
    /// probability and the reliability bins would be meaningless.
    #[error(
        "the success model produced a probability outside [0, 1] for candidate '{candidate}' \
         in cohort {cohort}: {value}"
    )]
    PredictionOutOfRange {
        cohort: String,
        candidate: String,
        value: f64,
    },

    /// One candidate appears twice in one axis.
    #[error("cohort {cohort} carries candidate '{candidate}' more than once; a K axis is a set of distinct candidates")]
    DuplicateCohortCandidate { cohort: String, candidate: String },

    /// A cohort has no candidates, so it cannot form a distribution.
    #[error("cohort {cohort} has no candidates, so it cannot form a distribution")]
    CohortWithoutCandidates { cohort: String },

    /// A cohort has candidates but none of them carries a prediction, so the
    /// total mass would be zero and nothing could be normalized.
    #[error("cohort {cohort} has no ranked candidate, so it carries no mass and cannot form a distribution")]
    CohortWithoutRankedCandidate { cohort: String },

    /// The cohort's observed winner is not one of its own candidates.
    #[error("cohort {cohort} reports candidate '{served}' as served, but that candidate is not in its own candidate set")]
    ServedCandidateNotInCohort { cohort: String, served: String },

    /// Some attempt rows of one request carry a `decision_id` and others do
    /// not. Coalescing them would invent a candidate set, so the run refuses.
    #[error(
        "request {request} carries {with} attempt rows with a decision id and {without} without one; \
         the K axis would not be one decision's candidate set"
    )]
    MixedDecisionScope {
        request: String,
        with: usize,
        without: usize,
    },

    /// One `decision_id` appears under two requests, so it does not name one
    /// decision.
    #[error("decision id {decision} appears under {requests} requests; a decision id names one decision")]
    DecisionScopeSpansRequests { decision: String, requests: usize },

    /// The snapshot cannot be split into a fit partition and a holdout of the
    /// configured size.
    #[error(
        "calibration snapshot of {cohorts} decisions cannot hold out {holdout} and still fit on {min_fit}"
    )]
    SnapshotTooSmall {
        cohorts: usize,
        holdout: usize,
        min_fit: usize,
    },

    /// A partition cannot support a calibration claim.
    #[error("calibration {partition} partition is degenerate: {reason} ({detail})")]
    DegeneratePartition {
        partition: PartitionKind,
        reason: DegeneracyReason,
        detail: String,
    },

    /// The cohorts were not in the canonical content order. This is an internal
    /// invariant rather than a caller mistake, and it is checked rather than
    /// assumed, because the determinism claim rests on it.
    #[error("calibration cohorts are not in canonical content order at index {index}")]
    UnorderedCohorts { index: usize },

    /// The fit did not produce a usable parameter set.
    #[error("the calibrator did not reach a usable parameter set: {reason}")]
    FitDiverged { reason: String },

    /// A measurement was asked for over an empty observation set, which has no
    /// bins and no mean.
    #[error("cannot measure {context}: there are no observations to measure")]
    NoObservations { context: &'static str },

    /// The metric layer refused the vectors handed to it.
    #[error("cannot measure {context}: {reason}")]
    Measurement {
        context: &'static str,
        reason: String,
    },

    /// The 7E-2A type refused a vector this module emitted. Unreachable by
    /// construction; surfaced rather than unwrapped so a future change to the
    /// parameterization cannot panic the product.
    #[error(
        "the decision distribution type refused the emitted vector for cohort {cohort}: {reason}"
    )]
    DistributionRejected { cohort: String, reason: String },

    /// The calibrator was fitted on one regime and the holdout is another.
    #[error("distribution shift between the fit and holdout partitions: {measurement}")]
    DistributionShift {
        /// The measurement, boxed so a refusal stays small enough to return
        /// cheaply from every fallible entry point in this module.
        measurement: Box<DriftMeasurement>,
    },

    // -- configuration that would make the gate meaningless --
    /// Zero reliability bins is a curve with no bins.
    #[error("reliability bin count {bins} must be at least 1")]
    ZeroReliabilityBins { bins: usize },

    /// Zero drift bins is an index with no bins.
    #[error("drift bin count {bins} must be at least 1")]
    ZeroDriftBins { bins: usize },

    /// A negative or non-finite ECE ceiling could never be met or always met.
    #[error("expected calibration error ceiling {value} must be finite and non-negative")]
    InvalidEceCeiling { value: f64 },

    /// A negative or non-finite MCE ceiling could never be met or always met.
    #[error("maximum calibration error ceiling {value} must be finite and non-negative")]
    InvalidMceCeiling { value: f64 },

    /// A negative or non-finite per-candidate ceiling could never be met or
    /// always met.
    #[error("per-candidate calibration error ceiling {value} must be finite and non-negative")]
    InvalidCandidateCeiling { value: f64 },

    /// A negative or non-finite PSI ceiling could never be met or always met.
    #[error("population stability index ceiling {value} must be finite and non-negative")]
    InvalidPsiCeiling { value: f64 },

    /// A base-rate ceiling is a difference of two rates in `[0, 1]`.
    #[error("base rate drift ceiling {value} must be finite and within [0, 1]")]
    InvalidBaseRateCeiling { value: f64 },

    /// The holdout size leaves nothing to fit on, so the run could only ever
    /// report an unfitted calibrator.
    #[error(
        "holdout of {holdout} cohorts and a fit floor of {min_fit} cannot both hold in any snapshot; \
         the split would be meaningless"
    )]
    MeaninglessHoldoutSplit { holdout: usize, min_fit: usize },

    /// A fit floor of zero would permit fitting on nothing.
    #[error("minimum fit cohort count {min_fit} must be at least 1")]
    ZeroFitCohortFloor { min_fit: usize },

    /// A holdout of zero decisions would decide a verdict from no evidence.
    #[error("holdout cohort count {holdout} must be at least 1")]
    ZeroHoldoutCohortCount { holdout: usize },

    /// An attributed-outcome floor of zero would let an unattributed holdout
    /// pass a degeneracy check it exists to fail.
    #[error("minimum attributed outcomes {minimum} must be at least 1")]
    ZeroAttributedFloor { minimum: usize },

    /// Zero iterations would report an unfitted calibrator as fitted.
    #[error("gradient step count {iterations} must be at least 1; zero steps would report an unfitted calibrator as fitted")]
    ZeroFitIterations { iterations: usize },

    /// A non-positive step size never moves the parameters.
    #[error("gradient step size {value} must be finite and positive")]
    InvalidLearningRate { value: f64 },

    /// Negative regularization is an anti-prior.
    #[error("intercept regularization {value} must be finite and non-negative")]
    InvalidInterceptL2 { value: f64 },

    /// A non-positive intercept bound lets one candidate run away with the
    /// vector.
    #[error("maximum absolute intercept {value} must be finite and positive")]
    InvalidInterceptBound { value: f64 },

    /// A zero or negative temperature collapses the vector onto one candidate
    /// and is not a calibration.
    #[error("minimum temperature {value} must be finite and positive; a zero temperature collapses the vector onto one candidate")]
    InvalidMinTemperature { value: f64 },

    /// A temperature ceiling that is not above the floor would pin the fit.
    #[error(
        "maximum temperature {value} must be finite and greater than the minimum temperature {min}"
    )]
    InvalidMaxTemperature { value: f64, min: f64 },

    /// The log clamp must sit strictly inside the unit interval on both sides.
    #[error("probability floor {floor} must be finite and strictly inside (0, 0.5)")]
    InvalidProbabilityFloor { floor: f64 },
}

impl From<DecisionContractError> for CalibrationError {
    fn from(error: DecisionContractError) -> Self {
        Self::DistributionRejected {
            cohort: String::new(),
            reason: error.to_string(),
        }
    }
}
