//! Node 7E-2D: offline calibration of a K-way candidate distribution.
//!
//! # What this module is
//!
//! It produces a [`DecisionDistribution`] — the validated K-way type accepted
//! from 7E-2A — as an **offline diagnostic and planning artifact**, measures
//! whether that distribution is calibrated, and refuses to report a calibration
//! claim it cannot support. It is a pure library entry point. It installs
//! nothing, wires nothing, schedules nothing, and exposes no endpoint. The
//! distribution it emits is *unconsumed*: no live predictor, no router, and no
//! policy in the running product can name it, because nothing outside this
//! module names the live surfaces at all.
//!
//! # The event, and why the model is joint
//!
//! The quantity being predicted is **"candidate _i_ serves this request"** — a
//! mutually exclusive K-way outcome, not "candidate _i_ succeeds". Those are
//! different events, and conflating them is the whole difficulty.
//!
//! The per-dimension models produce `Pr(candidate i would succeed)`. Nothing
//! forces K such numbers to sum to one, and nothing forces their ranking to
//! agree with the empirical one. Dividing them by their sum asserts they are
//! already consistent marginals of the serve event; that assertion is usually
//! false, and the assertion is made silently.
//!
//! So the calibrator here is a **joint** model on the simplex:
//!
//! ```text
//! u_i = (ln p_i + b_i) / T
//! q_i = softmax_i(u)
//! ```
//!
//! One global temperature `T` and a per-candidate intercept `b_i` constrained
//! to sum to zero for identifiability, L2-pulled toward zero and clamped so a
//! candidate the fit partition never sees serving stays near zero. The
//! parameterization *is* a simplex point: sum-to-one is inherent, and there is
//! no normalization step left over to damage anything.
//!
//! The competing route is built anyway, and measured. `MarginalCalibrator` fits
//! one shared 1-D Platt map on the log-odds — the most defensible form of
//! "calibrate each candidate independently", since a per-candidate map would
//! only be flattered by per-candidate sparsity — and
//! [`NormalizationDamage::measure`] reports the literal difference
//! `ece(after) - ece(before)` between normalizing that vector and not. The cost
//! of the naive route is a number in the report rather than an assertion in a
//! doc comment.
//!
//! # The final vector is the only thing that is claimed
//!
//! [`measure_emitted`] accepts [`EmittedDecision`], and `EmittedDecision`'s
//! `distribution` field is **private**: the only way to obtain one is
//! [`DecisionDistribution::try_new`]. There is no constructor that takes a bare
//! `f64` vector, and `EmittedDecision` deliberately derives neither
//! `Deserialize` nor `Default`. So the headline measurement *cannot* be
//! computed from pre-normalization numbers — the API does not exist.
//!
//! The two marginal measurements live in a different type,
//! [`MarginalCalibration`], which `measure_emitted` does not accept, and they
//! are reported as strictly subordinate diagnostics.
//!
//! [`CalibrationVerdict::from_measurements`] recomputes the verdict from the
//! stored final numbers and the stored ceilings;
//! [`CalibrationReport::recomputed_verdict`] recomputes it again from the
//! report, and [`CalibrationReport::is_calibrated`] asks *that*. There is no
//! stored boolean anywhere in the claim path.
//!
//! # Determinism, and the ordering trap
//!
//! Two ordering facts decide whether this module is reproducible at all, and
//! both were nearly got wrong.
//!
//! **The identifiers are not content.** `deterministic_sample_id` is
//! `format!("samp-{}-{suffix}", outcome.outcome_id)`
//! (`ml/dataset.rs`), and `outcome_id` is `req_{uuid}`
//! (`stats.rs`), while `decision_id` is `dec_{uuid}` (`router.rs`). Both are
//! fresh per request, and `Decision.timestamp` is second-resolution, so a
//! timestamp comparison ties constantly and the UUID becomes the tiebreaker.
//! Ordering by either one makes the report a function of randomness. This
//! module therefore orders cohorts by [`CohortOrderKey`], which is built only
//! from what the cohort *contains*. `sample_id` is used to **reject** a
//! repeated row and never as a sort key.
//!
//! **Why a content key is enough.** `CohortOrderKey` carries every input this
//! module's computation consumes: the ordered candidate identities, each
//! candidate's rank, each candidate's raw success probability as exact `f64`
//! bits, and the observed served identity. Two cohorts that tie on the key are
//! therefore *indistinguishable to the computation* — same axis, same
//! probabilities, same label — so exchanging them cannot change the fit gradient
//! sum, the multiclass log-loss sum, any reliability bin, any drift bin, the
//! base rate, or the arity. Ties are not a tiebreaker waiting for a UUID; they
//! are an equivalence class over which the report is invariant. The
//! request-level facts in the key (`timestamp`, `dialect`, `streaming`, the
//! terminal status and failure class) only make ties rarer.
//!
//! Two runs of the same snapshot and configuration produce byte-identical
//! calibrator and report. The `UUID-invariance` test proves the stronger
//! property: two snapshots that are the *same logical data* carrying
//! *deliberately different* UUIDs produce the same partition, the same
//! calibrator, the same emitted distributions, the same measurements, and the
//! same verdict. An in-process byte comparison cannot catch a UUID leak — the
//! UUIDs are fixed within a process — so that test is the one that bites.
//!
//! # Cohort scope
//!
//! One cohort is one **decision**: the set of candidates that decision
//! considered. A request whose attempt samples disagree about carrying a
//! `decision_id` is **refused** ([`CalibrationError::MixedDecisionScope`]),
//! not coalesced. Coalescing would merge two real planning events into one
//! invented candidate set, and would count a candidate twice if the same
//! identity appears in both. Refusing keeps the K axis equal to one decision's
//! candidate set and treats "the dataset lost a fact" as the fail-closed
//! condition it is. A `decision_id` spanning two requests is refused for the
//! same reason ([`CalibrationError::DecisionScopeSpansRequests`]).
//!
//! # Unranked candidates and exact zero mass
//!
//! A candidate can enter the axis with no prediction at all
//! ([`CandidateInput::Unranked`]) — ineligible, or without evidence. It
//! receives **exactly `0.0`**: the softmax runs over the ranked candidates only
//! and the zero is placed afterwards, so the ranked candidates keep the vector
//! they would have had and the type's total mass is untouched. `0.0` is a
//! legal probability for the 7E-2A type, so this is representable without
//! distorting the rest.
//!
//! # A known limitation, stated rather than hidden
//!
//! The K axis is the set of candidates **actually attempted**, reconstructed
//! retrospectively from recorded attempts. It is not a counterfactual set of
//! candidates that *could* have served, and a candidate that was never tried
//! carries no observation at all. Every number here is therefore a statement
//! about the recorded attempt cohort, and the report says so in
//! [`DISTRIBUTION_ROLE`]. Widening the axis to counterfactual candidates would
//! require a counterfactual store this repository does not have.

use std::fmt;

use serde::Serialize;

use super::dataset::{validate_outcome_sample, OutcomeTrainingSample, SampleScope};
use super::decision_contract::{DecisionContractError, DecisionDistribution};
use super::evaluation::PredictionMetrics;
use super::features::{FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use super::model::RoutingModel;
use crate::outcome::{CandidateIdentity, FinalStatus};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Reliability and drift bins, when the configuration does not say.
pub const DEFAULT_RELIABILITY_BINS: usize = 10;
/// Default drift bin count.
pub const DEFAULT_DRIFT_BINS: usize = 10;

/// Floor substituted for a zero bin proportion inside the population stability
/// index. A zero would make the term infinite, and an infinite drift number is
/// a refusal expressed as a measurement.
pub const PSI_PROPORTION_FLOOR: f64 = 1e-6;

/// Default clamp applied to a finite, in-range success probability before it is
/// logged. `ln(0)` is not a number and `ln(1)` is not finite.
pub const DEFAULT_PROBABILITY_FLOOR: f64 = 1e-6;

/// Clamp used only inside a log, mirroring the accepted evaluator's own guard.
pub const LOG_LOSS_CLAMP: f64 = 1e-15;

/// FNV-1a 64-bit offset basis, for the content fingerprint.
const FINGERPRINT_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FINGERPRINT_PRIME: u64 = 0x0000_0100_0000_01b3;

/// What the emitted distribution **is**, restated in every report.
///
/// There is no consumer. The distribution is a diagnostic and planning
/// artifact; it is not a live probability, and nothing routes on it.
pub const DISTRIBUTION_ROLE: &str = "unconsumed offline diagnostic and planning artifact; \
no installed consumer, no routing effect, and not a live probability";

// ---------------------------------------------------------------------------
// UnrankedReason / CandidateInput
// ---------------------------------------------------------------------------

/// Why a candidate is in the axis but carries no prediction.
///
/// Typed rather than free text, because "this candidate has no mass" and "this
/// candidate has a small mass" are different facts and a reader must be able to
/// tell them apart without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnrankedReason {
    /// The candidate was considered and refused before the request ran.
    NotEligible,
    /// The candidate was in the set but no prediction could be formed.
    InsufficientEvidence,
    /// The model behind the candidate has never been trained.
    ModelCold,
}

impl UnrankedReason {
    /// A stable label, used in reports and error text.
    pub const fn label(self) -> &'static str {
        match self {
            Self::NotEligible => "not_eligible",
            Self::InsufficientEvidence => "insufficient_evidence",
            Self::ModelCold => "model_cold",
        }
    }
}

impl fmt::Display for UnrankedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One candidate in one decision's K axis.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "role")]
pub enum CandidateInput {
    /// The model produced a success probability for this candidate.
    Ranked {
        /// Which candidate this is.
        candidate: CandidateIdentity,
        /// The success head's raw output, before any calibration.
        raw_success_probability: f64,
    },
    /// The candidate is in the axis but carries no prediction, and therefore
    /// receives exactly `0.0` mass without moving anyone else's.
    Unranked {
        /// Which candidate this is.
        candidate: CandidateIdentity,
        /// Why there is no prediction.
        reason: UnrankedReason,
    },
}

impl CandidateInput {
    /// The candidate identity, ranked or not.
    pub fn candidate(&self) -> &CandidateIdentity {
        match self {
            Self::Ranked { candidate, .. } | Self::Unranked { candidate, .. } => candidate,
        }
    }

    /// The raw success probability, or `None` when unranked.
    pub fn raw_success_probability(&self) -> Option<f64> {
        match self {
            Self::Ranked {
                raw_success_probability,
                ..
            } => Some(*raw_success_probability),
            Self::Unranked { .. } => None,
        }
    }

    /// Whether this candidate carries a prediction.
    pub const fn is_ranked(&self) -> bool {
        matches!(self, Self::Ranked { .. })
    }

    /// The unranked reason, or `None` when ranked.
    pub fn unranked_reason(&self) -> Option<UnrankedReason> {
        match self {
            Self::Ranked { .. } => None,
            Self::Unranked { reason, .. } => Some(*reason),
        }
    }
}

// ---------------------------------------------------------------------------
// Cohort scope types
// ---------------------------------------------------------------------------

/// Which partition a number or a refusal belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PartitionKind {
    /// The partition the calibrator is fitted on.
    Fit,
    /// The partition the calibrator is measured on. Disjoint from the fit
    /// partition by construction.
    Holdout,
}

impl PartitionKind {
    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fit => "fit",
            Self::Holdout => "holdout",
        }
    }
}

impl fmt::Display for PartitionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Why a partition cannot support a calibration claim.
///
/// These are the K-way analogues of the degeneracy 7E-2B refused on. The
/// single-winner case is the K-way version of the single-class case: if one
/// candidate serves every cohort, then every other candidate's observed
/// frequency is exactly zero, so a model that is *uniformly and confidently
/// wrong* about them looks calibrated, and a cold answer of `1/K` scores the
/// entropy of the base rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DegeneracyReason {
    /// Fewer cohorts than the configuration's floor.
    TooFewCohorts,
    /// No candidate anywhere in the partition carries a prediction.
    NoRankedCandidateAnywhere,
    /// No cohort in the partition was served, so every observed frequency is
    /// zero and any non-zero mass is confidently wrong.
    NoAttributedOutcome,
    /// Only one candidate ever served, so the axis cannot be told apart.
    SingleServedCandidate,
    /// Fewer attributed cohorts than the configured floor.
    TooFewAttributedOutcomes,
}

impl DegeneracyReason {
    /// A stable label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::TooFewCohorts => "too_few_cohorts",
            Self::NoRankedCandidateAnywhere => "no_ranked_candidate_anywhere",
            Self::NoAttributedOutcome => "no_attributed_outcome",
            Self::SingleServedCandidate => "single_served_candidate",
            Self::TooFewAttributedOutcomes => "too_few_attributed_outcomes",
        }
    }
}

impl fmt::Display for DegeneracyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

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
    #[error("calibration sample {index} carries a feature vector of width {found}, expected {expected}")]
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
    Measurement { context: &'static str, reason: String },

    /// The 7E-2A type refused a vector this module emitted. Unreachable by
    /// construction; surfaced rather than unwrapped so a future change to the
    /// parameterization cannot panic the product.
    #[error("the decision distribution type refused the emitted vector for cohort {cohort}: {reason}")]
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
    #[error("maximum temperature {value} must be finite and greater than the minimum temperature {min}")]
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
        if !self
            .reliability
            .max_candidate_calibration_error
            .is_finite()
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

// ---------------------------------------------------------------------------
// Cohort ordering
// ---------------------------------------------------------------------------

/// Request-level facts about one decision, used only for ordering and for the
/// report. None of them is an identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CohortContext {
    /// Unix seconds, as recorded. Second resolution, so this alone ties.
    pub timestamp: i64,
    /// The dialect the request arrived in.
    pub dialect: String,
    /// Whether the request was streaming.
    pub streaming: bool,
    /// The terminal status, as a stable rank.
    pub final_status_rank: u8,
    /// The terminal failure class, as a stable rank. `None` on success.
    pub failure_class_rank: Option<u8>,
}

/// The rank of a terminal status, declared here so the ordering never depends
/// on a derive or on a discriminant layout.
const fn final_status_rank(status: FinalStatus) -> u8 {
    match status {
        FinalStatus::Success => 0,
        FinalStatus::Failed => 1,
        FinalStatus::Cancelled => 2,
        FinalStatus::Interrupted => 3,
    }
}

/// One candidate's contribution to the cohort ordering key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AxisKey {
    /// `false` for ranked so ranked candidates sort first and the axis order is
    /// readable in a diagnostic dump.
    ranked: bool,
    provider: String,
    model: String,
    /// The raw success probability as exact bits, so two cohorts tie only when
    /// they would contribute the same gradient.
    probability_bits: u64,
}

/// The canonical content order of a cohort.
///
/// Carries every input the computation consumes, and nothing that came from a
/// UUID. See the module documentation for why a tie here is an equivalence
/// class rather than an arbitrary choice.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CohortOrderKey {
    context: CohortContext,
    served: Option<(String, String)>,
    axis: Vec<AxisKey>,
}

impl CohortOrderKey {
    fn of(candidates: &[CandidateInput], served: Option<&CandidateIdentity>, context: CohortContext) -> Self {
        Self {
            context,
            served: served.map(|identity| {
                (
                    identity.provider().to_string(),
                    identity.model().to_string(),
                )
            }),
            axis: candidates
                .iter()
                .map(|input| AxisKey {
                    ranked: input.is_ranked(),
                    provider: input.candidate().provider().to_string(),
                    model: input.candidate().model().to_string(),
                    probability_bits: input
                        .raw_success_probability()
                        .map_or(0, |value| value.to_bits()),
                })
                .collect(),
        }
    }

    /// A stable content fingerprint, as lowercase hex.
    ///
    /// FNV-1a over a canonical byte encoding. It is a label, not a security
    /// boundary, and it is computed from the ordering key rather than from any
    /// identifier, so the same logical data fingerprints the same way no
    /// matter what UUIDs the rows carry.
    fn fingerprint(&self) -> String {
        let mut bytes: Vec<u8> = Vec::with_capacity(96);
        bytes.extend_from_slice(&self.context.timestamp.to_le_bytes());
        encode_string(&mut bytes, &self.context.dialect);
        bytes.push(u8::from(self.context.streaming));
        bytes.push(self.context.final_status_rank);
        bytes.push(self.context.failure_class_rank.unwrap_or(u8::MAX));
        match &self.served {
            Some((provider, model)) => {
                bytes.push(1);
                encode_string(&mut bytes, provider);
                encode_string(&mut bytes, model);
            }
            None => bytes.push(0),
        }
        bytes.extend_from_slice(&(self.axis.len() as u64).to_le_bytes());
        for entry in &self.axis {
            bytes.push(u8::from(entry.ranked));
            encode_string(&mut bytes, &entry.provider);
            encode_string(&mut bytes, &entry.model);
            bytes.extend_from_slice(&entry.probability_bits.to_le_bytes());
        }
        format!("{:016x}", fnv1a(&bytes))
    }
}

fn encode_string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u64).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = FINGERPRINT_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FINGERPRINT_PRIME);
    }
    hash
}

// ---------------------------------------------------------------------------
// DecisionCohort
// ---------------------------------------------------------------------------

/// One decision's candidate set, its axis order, and what actually happened.
///
/// Built either by [`project_cohorts`] from a dataset snapshot, or directly by
/// a caller that already has a candidate set. Either way the axis is validated
/// on the way in: a cohort that cannot form a distribution is refused here, not
/// at emit time.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionCohort {
    fingerprint: String,
    order_key: CohortOrderKey,
    subject: CandidateIdentity,
    candidates: Vec<CandidateInput>,
    served: Option<CandidateIdentity>,
}

impl DecisionCohort {
    /// Validate and build one cohort.
    ///
    /// `subject_hint` is the identity the decision was planned around. The
    /// 7E-2A type requires the subject to be one of the outcomes, so a hint
    /// that is not in the axis is replaced by the first ranked candidate rather
    /// than emitted as a distribution about a decision that never considered
    /// its own first choice.
    pub fn try_new(
        context: CohortContext,
        subject_hint: Option<&CandidateIdentity>,
        candidates: Vec<CandidateInput>,
        served: Option<CandidateIdentity>,
    ) -> Result<Self, CalibrationError> {
        let fingerprint = CohortOrderKey::of(&candidates, served.as_ref(), context.clone()).fingerprint();

        if candidates.is_empty() {
            return Err(CalibrationError::CohortWithoutCandidates { cohort: fingerprint });
        }
        let mut seen: Vec<&CandidateIdentity> = Vec::with_capacity(candidates.len());
        for input in &candidates {
            if seen.contains(&input.candidate()) {
                return Err(CalibrationError::DuplicateCohortCandidate {
                    cohort: fingerprint.clone(),
                    candidate: format!(
                        "{}/{}",
                        input.candidate().provider(),
                        input.candidate().model()
                    ),
                });
            }
            seen.push(input.candidate());
        }

        if let Some(served) = served.as_ref() {
            if !candidates.iter().any(|input| input.candidate() == served) {
                return Err(CalibrationError::ServedCandidateNotInCohort {
                    cohort: fingerprint.clone(),
                    served: format!("{}/{}", served.provider(), served.model()),
                });
            }
        }

        let first_ranked = candidates
            .iter()
            .find(|input| input.is_ranked())
            .ok_or_else(|| CalibrationError::CohortWithoutRankedCandidate {
                cohort: fingerprint.clone(),
            })?;

        let subject = subject_hint
            .filter(|hint| candidates.iter().any(|input| input.candidate() == *hint))
            .cloned()
            .unwrap_or_else(|| first_ranked.candidate().clone());

        Ok(Self {
            fingerprint,
            order_key: CohortOrderKey::of(&candidates, served.as_ref(), context),
            subject,
            candidates,
            served,
        })
    }

    /// The content fingerprint. Two cohorts with the same fingerprint are
    /// indistinguishable to this module's computation.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The request-level facts.
    pub fn context(&self) -> &CohortContext {
        &self.order_key.context
    }

    /// The axis, in order.
    pub fn candidates(&self) -> &[CandidateInput] {
        &self.candidates
    }

    /// The decision this distribution is about.
    pub fn subject(&self) -> &CandidateIdentity {
        &self.subject
    }

    /// The candidate that actually served, if any.
    pub fn served(&self) -> Option<&CandidateIdentity> {
        self.served.as_ref()
    }

    /// The arity K.
    pub fn arity(&self) -> usize {
        self.candidates.len()
    }

    /// How many candidates carry a prediction.
    pub fn ranked_count(&self) -> usize {
        self.candidates.iter().filter(|input| input.is_ranked()).count()
    }

    /// Whether this decision was served.
    pub fn is_attributed(&self) -> bool {
        self.served.is_some()
    }
}

/// Whether a set of cohorts is in the canonical content order.
///
/// Exposed so a consumer that assembles its own cohort list can check the
/// invariant the determinism claim rests on, without the ordering key itself
/// becoming public surface. [`project_cohorts`] guarantees it; this is how a
/// caller confirms it.
pub fn canonical_order_holds(cohorts: &[DecisionCohort]) -> bool {
    unordered_at(cohorts).is_none()
}

// ---------------------------------------------------------------------------
// project_cohorts
// ---------------------------------------------------------------------------

/// Reconstruct the decision cohorts of a dataset snapshot.
///
/// Only attempt-scope rows are candidate evidence: a request-scope row is the
/// request as a whole, not a candidate, and treating it as one would put a
/// twelfth "candidate" in the axis that no decision ever compared.
///
/// The returned cohorts are in the canonical content order, so the partition a
/// caller takes from this vector is a function of the snapshot's contents and
/// not of the order the rows arrived in.
pub fn project_cohorts(
    snapshot: &[OutcomeTrainingSample],
    model: &dyn RoutingModel,
    probability_floor: f64,
) -> Result<Vec<DecisionCohort>, CalibrationError> {
    if snapshot.is_empty() {
        return Err(CalibrationError::EmptySnapshot);
    }
    if !probability_floor.is_finite() || probability_floor <= 0.0 || probability_floor >= 0.5 {
        return Err(CalibrationError::InvalidProbabilityFloor {
            floor: probability_floor,
        });
    }

    let attempt_rows = attempt_row_indexes(snapshot)?;
    let groups = group_attempt_rows(snapshot, &attempt_rows)?;

    let mut cohorts = Vec::with_capacity(groups.len());
    for group in &groups {
        cohorts.push(build_cohort(snapshot, group, model)?);
    }

    // The construction above walks identifier-keyed groups, so the order it
    // produces is a function of randomness. Sorting on the content key is what
    // removes that dependence, and it is the only place order can enter.
    cohorts.sort_by(|left, right| left.order_key.cmp(&right.order_key));
    if let Some(index) = unordered_at(&cohorts) {
        return Err(CalibrationError::UnorderedCohorts { index });
    }
    Ok(cohorts)
}

/// Indexes of the attempt-scope rows, after rejecting a repeated sample id.
fn attempt_row_indexes(snapshot: &[OutcomeTrainingSample]) -> Result<Vec<usize>, CalibrationError> {
    let mut rows = Vec::with_capacity(snapshot.len());
    let mut seen: Vec<(&str, usize)> = Vec::with_capacity(snapshot.len());
    for (index, sample) in snapshot.iter().enumerate() {
        if matches!(sample.scope, SampleScope::Request) {
            continue;
        }
        if let Some((_, first)) = seen.iter().find(|(id, _)| *id == sample.sample_id.as_str()) {
            return Err(CalibrationError::DuplicateSampleId {
                sample_id: sample.sample_id.clone(),
                first: *first,
                second: index,
            });
        }
        seen.push((sample.sample_id.as_str(), index));
        rows.push(index);
    }
    if rows.is_empty() {
        return Err(CalibrationError::EmptySnapshot);
    }
    Ok(rows)
}

/// A group of attempt rows that belong to one decision.
struct CohortGroup {
    outcome_id: String,
    decision_id: Option<String>,
    context: CohortContext,
    subject_hint: Option<CandidateIdentity>,
    served: Option<CandidateIdentity>,
    /// Row indexes, in the order the decision attempted them.
    rows: Vec<usize>,
}

impl CohortGroup {
    /// A content-derived label for a cohort that has not been built yet.
    ///
    /// Used only in the per-row refusals below, which fire before the cohort
    /// exists and therefore before it has a fingerprint. Derived from the
    /// group's own contents, never from its identifier.
    fn pending_label(&self) -> String {
        format!(
            "{}/{}/{}",
            self.context.dialect,
            self.context.timestamp,
            self.rows.len()
        )
    }
}

/// Group attempt rows into decisions, refusing an incoherent decision scope.
fn group_attempt_rows(
    snapshot: &[OutcomeTrainingSample],
    rows: &[usize],
) -> Result<Vec<CohortGroup>, CalibrationError> {
    // First by request, so a request that disagrees with itself about carrying
    // a decision id is caught before it is split.
    let mut by_request: Vec<(&str, Vec<usize>)> = Vec::new();
    for &row in rows {
        let outcome_id = snapshot[row].outcome_id.as_str();
        match by_request.iter_mut().find(|(id, _)| *id == outcome_id) {
            Some((_, bucket)) => bucket.push(row),
            None => by_request.push((outcome_id, vec![row])),
        }
    }

    // Then by decision within the request, in attempt order.
    let mut scoped: Vec<CohortGroup> = Vec::new();
    for (outcome_id, request_rows) in &by_request {
        let with_decision = request_rows
            .iter()
            .filter(|row| snapshot[**row].decision_id.is_some())
            .count();
        if with_decision > 0 && with_decision != request_rows.len() {
            return Err(CalibrationError::MixedDecisionScope {
                request: (*outcome_id).to_string(),
                with: with_decision,
                without: request_rows.len() - with_decision,
            });
        }

        let mut ordered = request_rows.clone();
        ordered.sort_by_key(|row| attempt_index(&snapshot[*row].scope));
        let mut active: Option<(Option<String>, Vec<usize>)> = None;
        for row in ordered {
            let decision_id = snapshot[row].decision_id.clone();
            match &mut active {
                Some((current, bucket)) if *current == decision_id => bucket.push(row),
                _ => {
                    if let Some((decision_id, bucket)) = active.replace((decision_id, vec![row])) {
                        scoped.push(new_group(snapshot, outcome_id, decision_id, &bucket));
                    }
                }
            }
        }
        if let Some((decision_id, bucket)) = active {
            scoped.push(new_group(snapshot, outcome_id, decision_id, &bucket));
        }
    }

    // Finally, a decision id that names two requests is not one decision.
    let mut owners: Vec<(&str, &str)> = Vec::new();
    for group in &scoped {
        let Some(decision_id) = group.decision_id.as_deref() else {
            continue;
        };
        match owners.iter_mut().find(|(id, _)| *id == decision_id) {
            Some((_, owner)) => {
                if *owner != group.outcome_id {
                    return Err(CalibrationError::DecisionScopeSpansRequests {
                        decision: decision_id.to_string(),
                        requests: 2,
                    });
                }
            }
            None => owners.push((decision_id, group.outcome_id.as_str())),
        }
    }

    Ok(scoped)
}

fn new_group(
    snapshot: &[OutcomeTrainingSample],
    outcome_id: &str,
    decision_id: Option<String>,
    rows: &[usize],
) -> CohortGroup {
    let head = &snapshot[rows[0]];
    CohortGroup {
        outcome_id: outcome_id.to_string(),
        decision_id,
        context: CohortContext {
            timestamp: head.timestamp,
            dialect: head.dialect.clone(),
            streaming: head.streaming,
            final_status_rank: final_status_rank(head.final_status),
            failure_class_rank: head
                .terminal_error
                .as_ref()
                .map(|facts| failure_class_rank(facts.class)),
        },
        subject_hint: head.identity.planned.clone(),
        served: head.identity.served.clone(),
        rows: rows.to_vec(),
    }
}

fn failure_class_rank(class: crate::failure::FailureClass) -> u8 {
    crate::failure::FailureClass::ALL
        .iter()
        .position(|candidate| *candidate == class)
        .map_or(u8::MAX, |index| index as u8)
}

fn attempt_index(scope: &SampleScope) -> usize {
    match scope {
        SampleScope::Attempt { index, .. } => *index,
        SampleScope::Request => usize::MAX,
    }
}

fn build_cohort(
    snapshot: &[OutcomeTrainingSample],
    group: &CohortGroup,
    model: &dyn RoutingModel,
) -> Result<DecisionCohort, CalibrationError> {
    let mut candidates = Vec::with_capacity(group.rows.len());
    for &row in &group.rows {
        let sample = &snapshot[row];
        validate_calibration_sample(row, sample)?;
        let raw = model.predict(&sample.features).value;
        let candidate = format!("{}/{}", sample.provider_id, sample.model_id);
        if !raw.is_finite() {
            return Err(CalibrationError::NonFinitePrediction {
                cohort: group.pending_label(),
                candidate,
                value: raw,
            });
        }
        if !(0.0..=1.0).contains(&raw) {
            return Err(CalibrationError::PredictionOutOfRange {
                cohort: group.pending_label(),
                candidate,
                value: raw,
            });
        }
        candidates.push(CandidateInput::Ranked {
            candidate: CandidateIdentity::new(sample.model_id.clone(), sample.provider_id.clone()),
            raw_success_probability: raw,
        });
    }
    DecisionCohort::try_new(
        group.context.clone(),
        group.subject_hint.as_ref(),
        candidates,
        group.served.clone(),
    )
}

fn validate_calibration_sample(
    index: usize,
    sample: &OutcomeTrainingSample,
) -> Result<(), CalibrationError> {
    if sample.schema_version != FEATURE_SCHEMA_VERSION {
        return Err(CalibrationError::SchemaMismatch {
            index,
            component: "sample",
            found: sample.schema_version,
            expected: FEATURE_SCHEMA_VERSION,
        });
    }
    if sample.features.schema_version != FEATURE_SCHEMA_VERSION {
        return Err(CalibrationError::SchemaMismatch {
            index,
            component: "feature",
            found: sample.features.schema_version,
            expected: FEATURE_SCHEMA_VERSION,
        });
    }
    if sample.features.values.len() != FEATURE_DIMENSION {
        return Err(CalibrationError::FeatureDimension {
            index,
            found: sample.features.values.len(),
            expected: FEATURE_DIMENSION,
        });
    }
    if let Some((position, value)) = sample
        .features
        .values
        .iter()
        .enumerate()
        .find(|(_, value)| !value.is_finite())
    {
        return Err(CalibrationError::NonFiniteFeature {
            index,
            position,
            value: *value,
        });
    }
    validate_outcome_sample(sample).map_err(|reason| CalibrationError::InvalidSample { index, reason })
}

fn unordered_at(cohorts: &[DecisionCohort]) -> Option<usize> {
    cohorts
        .windows(2)
        .position(|pair| pair[1].order_key < pair[0].order_key)
}

// ---------------------------------------------------------------------------
// KWayCalibrator
// ---------------------------------------------------------------------------

/// One candidate's fitted intercept.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CandidateIntercept {
    /// The candidate the intercept belongs to.
    pub candidate: CandidateIdentity,
    /// The fitted shift, in log-probability space.
    pub intercept: f64,
    /// Cohorts in the fit partition where this candidate appeared.
    pub fit_cohorts: usize,
    /// Cohorts in the fit partition where this candidate served.
    pub fit_serving_cohorts: usize,
}

/// The fitted joint K-way calibrator.
///
/// `q_i = softmax_i((ln p_i + b_i) / T)`. The sum-to-zero constraint on `b` is
/// what makes `T` and `b` separately identifiable: adding a constant to every
/// `b` and rescaling `T` leaves `q` unchanged, so without the constraint one of
/// them would be unidentifiable and the other would absorb its drift.
///
/// Nothing here is random. `iterations` is the stopping rule, there is no
/// seed, and no wall clock is read, so the same partition and configuration
/// give the same parameters.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KWayCalibrator {
    temperature: f64,
    intercepts: Vec<CandidateIntercept>,
    fit_cohorts: usize,
    fit_attributed_cohorts: usize,
    fit_observations: usize,
    iterations: usize,
    intercept_l2: f64,
    initial_log_loss: f64,
    final_log_loss: f64,
}

impl KWayCalibrator {
    /// The identity joint: temperature one, no intercepts.
    ///
    /// This is the uncalibrated *joint* baseline — the same parameterization
    /// with nothing fitted — so the diagnostic comparison is measured through
    /// the same validated-type path as the claim rather than through a side
    /// channel.
    pub fn uncalibrated() -> Self {
        Self {
            temperature: 1.0,
            intercepts: Vec::new(),
            fit_cohorts: 0,
            fit_attributed_cohorts: 0,
            fit_observations: 0,
            iterations: 0,
            intercept_l2: 0.0,
            initial_log_loss: 0.0,
            final_log_loss: 0.0,
        }
    }

    /// Fit the joint calibrator on one partition.
    ///
    /// Only attributed cohorts contribute: the target is "who serves", and a
    /// request nobody served has no target. Those rows are still counted in the
    /// measurement, where they correctly push every candidate's observed
    /// frequency down.
    pub fn fit(
        cohorts: &[DecisionCohort],
        config: &CalibrationConfig,
        probability_floor: f64,
    ) -> Result<Self, CalibrationError> {
        let (indexed, attributed) = fit_index(cohorts)?;
        if attributed.is_empty() {
            return Err(CalibrationError::FitDiverged {
                reason: "the fit partition holds no attributed decision, so there is no target to fit".to_string(),
            });
        }

        let mut temperature = 1.0_f64;
        let mut intercepts = vec![0.0_f64; indexed.len()];

        for _ in 0..config.fit.iterations {
            let mut gradient_intercept = vec![0.0_f64; indexed.len()];
            let mut gradient_temperature = 0.0_f64;

            for cohort in &attributed {
                let scores = cohort_scores(cohort, &intercepts, &indexed, temperature, probability_floor);
                let Some(mass) = softmax(&scores.utility) else {
                    return Err(CalibrationError::FitDiverged {
                        reason: "the joint utility produced no finite mass".to_string(),
                    });
                };
                let Some(winner) = scores.winner else {
                    return Err(CalibrationError::FitDiverged {
                        reason: "an attributed decision has no ranked winner".to_string(),
                    });
                };
                if winner >= mass.len() {
                    return Err(CalibrationError::FitDiverged {
                        reason: "the joint mass has no slot for the observed winner".to_string(),
                    });
                }

                for (slot, &share) in mass.iter().enumerate() {
                    let residual = share - f64::from(slot == winner);
                    if let Some(global) = scores.slot_to_global[slot] {
                        gradient_intercept[global] += residual / temperature;
                    }
                    gradient_temperature -= residual * scores.utility[slot] / temperature;
                }
            }

            for (slot, value) in intercepts.iter_mut().enumerate() {
                *value -= config.fit.learning_rate
                    * (gradient_intercept[slot] + config.fit.intercept_l2 * *value);
                *value = value.clamp(-config.fit.max_abs_intercept, config.fit.max_abs_intercept);
            }
            project_to_zero_mean(&mut intercepts);
            temperature = (temperature - config.fit.learning_rate * gradient_temperature).clamp(
                config.fit.min_temperature,
                config.fit.max_temperature,
            );
        }

        // Both losses are measured the same way, on the same partition, with
        // the same code: once at the identity parameterization and once at the
        // fitted one. Neither is read from the gradient loop, so neither can be
        // an artefact of when the accumulation stopped.
        let identity = vec![0.0_f64; indexed.len()];
        let initial_loss = mean_log_loss(
            &attributed,
            &identity,
            &indexed,
            1.0,
            probability_floor,
        );
        let final_loss = mean_log_loss(
            &attributed,
            &intercepts,
            &indexed,
            temperature,
            probability_floor,
        );

        if !final_loss.is_finite() {
            return Err(CalibrationError::FitDiverged {
                reason: "the fitted joint log loss is not finite".to_string(),
            });
        }

        let observations = cohorts
            .iter()
            .map(|cohort| cohort.ranked_count())
            .sum::<usize>();
        let mut records = Vec::with_capacity(indexed.len());
        for (slot, candidate) in indexed.iter().enumerate() {
            let mut seen = 0usize;
            let mut won = 0usize;
            for cohort in cohorts {
                if cohort
                    .candidates()
                    .iter()
                    .any(|input| input.is_ranked() && input.candidate() == candidate)
                {
                    seen += 1;
                }
                if cohort.served() == Some(candidate) {
                    won += 1;
                }
            }
            records.push(CandidateIntercept {
                candidate: candidate.clone(),
                intercept: intercepts[slot],
                fit_cohorts: seen,
                fit_serving_cohorts: won,
            });
        }

        Ok(Self {
            temperature,
            intercepts: records,
            fit_cohorts: cohorts.len(),
            fit_attributed_cohorts: attributed.len(),
            fit_observations: observations,
            iterations: config.fit.iterations,
            intercept_l2: config.fit.intercept_l2,
            initial_log_loss: initial_loss,
            final_log_loss: final_loss,
        })
    }

    /// The fitted temperature.
    pub fn temperature(&self) -> f64 {
        self.temperature
    }

    /// The per-candidate intercepts, in the canonical candidate order.
    pub fn intercepts(&self) -> &[CandidateIntercept] {
        &self.intercepts
    }

    /// One candidate's intercept, or `0.0` for a candidate the fit never saw.
    ///
    /// Returning zero rather than refusing is deliberate: a candidate that
    /// appears only in the holdout has no fitted shift, and inventing one would
    /// be a claim about data the fit never saw.
    pub fn intercept_of(&self, candidate: &CandidateIdentity) -> f64 {
        self.intercepts
            .iter()
            .find(|record| &record.candidate == candidate)
            .map_or(0.0, |record| record.intercept)
    }

    /// Cohorts this calibrator was fitted on.
    pub fn fit_cohorts(&self) -> usize {
        self.fit_cohorts
    }

    /// Attributed cohorts this calibrator was fitted on.
    pub fn fit_attributed_cohorts(&self) -> usize {
        self.fit_attributed_cohorts
    }

    /// Ranked candidate observations this calibrator was fitted on.
    pub fn fit_observations(&self) -> usize {
        self.fit_observations
    }

    /// Mean multiclass log loss over the fit partition after fitting.
    pub fn final_log_loss(&self) -> f64 {
        self.final_log_loss
    }

    /// Mean multiclass log loss over the fit partition before the first step.
    pub fn initial_log_loss(&self) -> f64 {
        self.initial_log_loss
    }

    /// Whether this calibrator was fitted at all.
    pub fn is_fitted(&self) -> bool {
        self.iterations > 0
    }

    /// Emit the K-way distribution for one decision.
    ///
    /// The softmax runs over the ranked candidates only; an
    /// [`CandidateInput::Unranked`] candidate is placed at exactly `0.0`
    /// afterwards, so it takes no mass and the ranked candidates keep exactly
    /// the vector the fit gave them.
    pub fn distribution(
        &self,
        cohort: &DecisionCohort,
        probability_floor: f64,
    ) -> Result<EmittedDecision, CalibrationError> {
        let mut ranked_utility = Vec::with_capacity(cohort.ranked_count());
        for input in cohort.candidates() {
            if let Some(raw) = input.raw_success_probability() {
                let shifted = log_probability(raw, probability_floor)?
                    + self.intercept_of(input.candidate());
                ranked_utility.push(shifted / self.temperature);
            }
        }

        let Some(mass) = softmax(&ranked_utility) else {
            return Err(CalibrationError::DistributionRejected {
                cohort: cohort.fingerprint().to_string(),
                reason: "the joint utility produced no finite mass".to_string(),
            });
        };
        let total: f64 = mass.iter().sum();
        if !total.is_finite() || total <= 0.0 {
            return Err(CalibrationError::DistributionRejected {
                cohort: cohort.fingerprint().to_string(),
                reason: format!("the joint mass summed to {total}"),
            });
        }

        // The softmax covered the ranked candidates only. The unranked ones are
        // placed at exactly 0.0 here, so they take no mass and the ranked
        // candidates keep the vector the fit gave them.
        let mut probabilities = Vec::with_capacity(cohort.arity());
        let mut unranked: Vec<CandidateIdentity> = Vec::new();
        let mut cursor = 0usize;
        for input in cohort.candidates() {
            if input.is_ranked() {
                probabilities.push(mass.get(cursor).copied().unwrap_or(0.0) / total);
                cursor += 1;
            } else {
                probabilities.push(0.0);
                unranked.push(input.candidate().clone());
            }
        }

        let outcomes: Vec<CandidateIdentity> = cohort
            .candidates()
            .iter()
            .map(|input| input.candidate().clone())
            .collect();
        let distribution =
            DecisionDistribution::try_new(cohort.subject().clone(), outcomes, probabilities)
                .map_err(|error: DecisionContractError| CalibrationError::DistributionRejected {
                    cohort: cohort.fingerprint().to_string(),
                    reason: error.to_string(),
                })?;

        Ok(EmittedDecision {
            cohort_fingerprint: cohort.fingerprint().to_string(),
            distribution,
            served: cohort.served().cloned(),
            unranked,
        })
    }
}

/// The candidate vocabulary of a partition, in a canonical order, plus the
/// attributed cohorts the fit may use.
fn fit_index(
    cohorts: &[DecisionCohort],
) -> Result<(Vec<CandidateIdentity>, Vec<&DecisionCohort>), CalibrationError> {
    let mut vocabulary: Vec<CandidateIdentity> = Vec::new();
    for cohort in cohorts {
        for input in cohort.candidates() {
            if input.is_ranked() {
                let identity = input.candidate().clone();
                if !vocabulary.contains(&identity) {
                    vocabulary.push(identity);
                }
            }
        }
    }
    vocabulary.sort_by(|left, right| {
        left.provider()
            .cmp(right.provider())
            .then_with(|| left.model().cmp(right.model()))
    });
    let attributed: Vec<&DecisionCohort> = cohorts.iter().filter(|cohort| cohort.is_attributed()).collect();
    Ok((vocabulary, attributed))
}

/// The per-cohort quantities one gradient step needs.
struct CohortScores {
    /// The joint utility of each ranked candidate, in axis order.
    utility: Vec<f64>,
    /// Axis-order index of the observed winner among the ranked candidates.
    winner: Option<usize>,
    /// Maps a ranked slot back to its slot in the global candidate vocabulary.
    slot_to_global: Vec<Option<usize>>,
}

fn cohort_scores(
    cohort: &DecisionCohort,
    intercepts: &[f64],
    vocabulary: &[CandidateIdentity],
    temperature: f64,
    probability_floor: f64,
) -> CohortScores {
    let mut utility = Vec::with_capacity(cohort.ranked_count());
    let mut slot_to_global = Vec::with_capacity(cohort.ranked_count());
    let mut winner = None;
    for input in cohort.candidates() {
        let Some(raw) = input.raw_success_probability() else {
            continue;
        };
        let global = vocabulary
            .iter()
            .position(|identity| identity == input.candidate())
            .unwrap_or(0);
        let shift = intercepts.get(global).copied().unwrap_or(0.0);
        utility.push((log_probability(raw, probability_floor).unwrap_or(0.0) + shift) / temperature);
        slot_to_global.push(Some(global));
        if cohort.served() == Some(input.candidate()) {
            winner = Some(utility.len() - 1);
        }
    }
    CohortScores {
        utility,
        winner,
        slot_to_global,
    }
}

/// A numerically stable softmax. `None` when the input cannot produce a
/// distribution, which is a refusal rather than a uniform fallback.
fn softmax(utility: &[f64]) -> Option<Vec<f64>> {
    if utility.is_empty() {
        return None;
    }
    let mut largest = f64::NEG_INFINITY;
    for value in utility {
        if !value.is_finite() {
            return None;
        }
        if *value > largest {
            largest = *value;
        }
    }
    let mut mass = Vec::with_capacity(utility.len());
    let mut total = 0.0;
    for value in utility {
        let exponent = (value - largest).exp();
        if !exponent.is_finite() {
            return None;
        }
        total += exponent;
        mass.push(exponent);
    }
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    for value in &mut mass {
        *value /= total;
    }
    Some(mass)
}

/// Mean multiclass log loss of one parameterization over the attributed
/// decisions of a partition.
///
/// `None` for a decision whose utility cannot produce a mass, so a broken
/// parameterization is reported as a refusal by the caller rather than as a
/// flattering number.
fn mean_log_loss(
    attributed: &[&DecisionCohort],
    intercepts: &[f64],
    vocabulary: &[CandidateIdentity],
    temperature: f64,
    probability_floor: f64,
) -> f64 {
    if attributed.is_empty() {
        return 0.0;
    }
    let mut total = 0.0;
    for cohort in attributed {
        let scores = cohort_scores(cohort, intercepts, vocabulary, temperature, probability_floor);
        if let (Some(mass), Some(winner)) = (softmax(&scores.utility), scores.winner) {
            let probability = mass.get(winner).copied().unwrap_or(0.0).max(LOG_LOSS_CLAMP);
            total -= probability.ln();
        }
    }
    total / attributed.len() as f64
}

fn project_to_zero_mean(values: &mut [f64]) {
    if values.is_empty() {
        return;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    for value in values.iter_mut() {
        *value -= mean;
    }
}

/// `ln(p / (1 - p))` for a finite probability inside `[0, 1]`.
///
/// The log-odds, not `ln p`. A Platt map is a map *on the log-odds*: feeding it
/// `ln p` instead silently changes what the fitted slope means, and the
/// resulting map is not the monotone recalibration the method describes.
fn log_odds(raw: f64, floor: f64) -> Result<f64, CalibrationError> {
    let log_p = log_probability(raw, floor)?;
    let log_q = log_probability(1.0 - raw, floor)?;
    Ok(log_p - log_q)
}

fn log_probability(raw: f64, floor: f64) -> Result<f64, CalibrationError> {
    if !raw.is_finite() {
        return Err(CalibrationError::NonFinitePrediction {
            cohort: String::new(),
            candidate: String::new(),
            value: raw,
        });
    }
    if !(0.0..=1.0).contains(&raw) {
        return Err(CalibrationError::PredictionOutOfRange {
            cohort: String::new(),
            candidate: String::new(),
            value: raw,
        });
    }
    Ok(raw.clamp(floor, 1.0 - floor).ln())
}

// ---------------------------------------------------------------------------
// EmittedDecision / DistributionRecord
// ---------------------------------------------------------------------------

/// One emitted K-way distribution and the outcome it is scored against.
///
/// The `distribution` field is private and the type derives neither
/// `Deserialize` nor `Default`, so the only way to obtain an `EmittedDecision`
/// is to have gone through [`DecisionDistribution::try_new`]. That is the
/// structural reason the headline measurement cannot be computed from
/// pre-normalization numbers: there is no constructor that takes one.
#[derive(Debug, Clone, PartialEq)]
pub struct EmittedDecision {
    cohort_fingerprint: String,
    distribution: DecisionDistribution,
    served: Option<CandidateIdentity>,
    unranked: Vec<CandidateIdentity>,
}

impl EmittedDecision {
    /// The content fingerprint of the decision this came from.
    pub fn cohort_fingerprint(&self) -> &str {
        &self.cohort_fingerprint
    }

    /// The validated distribution.
    pub fn distribution(&self) -> &DecisionDistribution {
        &self.distribution
    }

    /// The candidates that received exactly zero mass, and why they are in the
    /// axis at all.
    pub fn unranked(&self) -> &[CandidateIdentity] {
        &self.unranked
    }

    /// The candidate that actually served.
    pub fn served(&self) -> Option<&CandidateIdentity> {
        self.served.as_ref()
    }

    /// A validating wire form.
    ///
    /// The 7E-2A type deliberately derives neither `Serialize` nor
    /// `Deserialize`, on the stated terms that the node which produces one owns
    /// giving it a wire form. This is that wire form, and it is *validating*:
    /// [`DistributionRecord::try_into_distribution`] runs the same
    /// [`DecisionDistribution::try_new`] on the way back in, so a stored record
    /// cannot re-enter the measurement path having skipped the arity and
    /// normalization checks.
    pub fn to_record(&self) -> DistributionRecord {
        DistributionRecord {
            cohort_fingerprint: self.cohort_fingerprint.clone(),
            subject: self.distribution.subject().clone(),
            outcomes: self.distribution.outcomes().to_vec(),
            probabilities: self.distribution.probabilities().to_vec(),
            total_mass: self.distribution.total_mass(),
            served: self.served.clone(),
            unranked: self.unranked.clone(),
        }
    }
}

/// The serializable form of an emitted distribution.
///
/// `Serialize` only. Deserialization goes through
/// [`DistributionRecord::try_into_distribution`], never through a derive.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DistributionRecord {
    /// The content fingerprint of the decision.
    pub cohort_fingerprint: String,
    /// The decision the distribution is about.
    pub subject: CandidateIdentity,
    /// The ordered K axis.
    pub outcomes: Vec<CandidateIdentity>,
    /// The mass on each axis entry.
    pub probabilities: Vec<f64>,
    /// The recorded total, so a reader can see the check that was run.
    pub total_mass: f64,
    /// The candidate that actually served.
    pub served: Option<CandidateIdentity>,
    /// The candidates that received exactly zero mass.
    pub unranked: Vec<CandidateIdentity>,
}

impl DistributionRecord {
    /// Rebuild the validated distribution, re-running every 7E-2A check.
    pub fn try_into_distribution(&self) -> Result<DecisionDistribution, CalibrationError> {
        DecisionDistribution::try_new(
            self.subject.clone(),
            self.outcomes.clone(),
            self.probabilities.clone(),
        )
        .map_err(|error| CalibrationError::DistributionRejected {
            cohort: self.cohort_fingerprint.clone(),
            reason: error.to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Reliability
// ---------------------------------------------------------------------------

/// One bin of a reliability curve.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ReliabilityBin {
    /// Inclusive lower edge of the predicted-probability bin.
    pub lower: f64,
    /// Exclusive upper edge, except on the last bin.
    pub upper: f64,
    /// How many candidate observations landed here.
    pub count: usize,
    /// Mean predicted mass over the bin.
    pub mean_predicted: f64,
    /// Fraction of the bin's observations that actually served.
    pub observed_frequency: f64,
    /// `mean_predicted - observed_frequency`. Positive is over-confident.
    pub gap: f64,
}

/// A binned reliability curve with per-bin counts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReliabilityCurve {
    /// Bins across `[0, 1]`.
    pub bin_count: usize,
    /// Total observations across every bin.
    pub observations: usize,
    /// The bins, in ascending predicted-probability order.
    pub bins: Vec<ReliabilityBin>,
}

impl ReliabilityCurve {
    /// The count-weighted mean absolute gap.
    pub fn expected_calibration_error(&self) -> f64 {
        if self.observations == 0 {
            return 0.0;
        }
        self.bins
            .iter()
            .filter(|bin| bin.count > 0)
            .map(|bin| (bin.count as f64 / self.observations as f64) * bin.gap.abs())
            .sum()
    }

    /// The largest absolute gap over the populated bins, or `None` when the
    /// curve holds no observation at all.
    pub fn maximum_calibration_error(&self) -> Option<f64> {
        self.bins
            .iter()
            .filter(|bin| bin.count > 0)
            .map(|bin| bin.gap.abs())
            .fold(None, |worst: Option<f64>, gap| {
                Some(worst.map_or(gap, |current: f64| current.max(gap)))
            })
    }

    /// How many bins hold at least one observation.
    pub fn populated_bins(&self) -> usize {
        self.bins.iter().filter(|bin| bin.count > 0).count()
    }
}

/// One candidate's reliability, measured without binning.
///
/// The binned curve is the required artefact and it is what a reader expects to
/// look at, but it has a blind spot that matters here: with K candidates and
/// fewer bins than distinct masses, two candidates whose emitted masses land in
/// the same bin are *averaged into one row*, and their errors can cancel. A
/// vector that is 20 points wrong on one candidate and 20 points wrong in the
/// other direction on another can score a bin gap of zero. On a three-candidate
/// axis with ten bins that is not a hypothetical — it is what this node's own
/// fixture does to the independent route.
///
/// This table is bin-free by construction: one row per candidate, over every
/// observation of that candidate. It cannot hide a collision, and it is
/// reported beside the curve rather than instead of it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CandidateCalibration {
    /// Which candidate.
    pub candidate: CandidateIdentity,
    /// How many decisions it appeared in.
    pub observations: usize,
    /// How many of those it served.
    pub served: usize,
    /// Mean mass this candidate was given.
    pub mean_predicted: f64,
    /// Fraction of its decisions it actually served.
    pub observed_frequency: f64,
    /// `mean_predicted - observed_frequency`. Positive is over-confident.
    pub gap: f64,
}

/// The full measurement of one set of emitted distributions.
///
/// This is the headline. `pooled` is the accepted evaluator's own
/// [`PredictionMetrics`] over the flattened K axis, obtained through the
/// fallible constructor, so the probability numbers here and the numbers
/// 7D already trusts are the same arithmetic.
///
/// `PartialEq` is deliberately absent: `PredictionMetrics` does not carry one,
/// and the accepted warmup node set the same precedent by comparing the
/// serialized form instead.
#[derive(Debug, Clone, Serialize)]
pub struct CalibrationMeasure {
    /// Decisions measured.
    pub decisions: usize,
    /// Decisions where somebody served; the denominator of the multiclass loss.
    pub attributed_decisions: usize,
    /// Candidate observations across the K axis.
    pub observations: usize,
    /// The binned reliability curve.
    pub curve: ReliabilityCurve,
    /// The bin-free per-candidate table.
    pub per_candidate: Vec<CandidateCalibration>,
    /// The accepted per-candidate probability metrics.
    pub pooled: PredictionMetrics,
    /// Mean `-ln q` on the observed winner. `None` when nothing was served.
    pub multiclass_log_loss: Option<f64>,
}

impl CalibrationMeasure {
    /// The count-weighted mean absolute gap.
    pub fn expected_calibration_error(&self) -> f64 {
        self.curve.expected_calibration_error()
    }

    /// The largest absolute bin gap, or `None` on an empty curve.
    pub fn maximum_calibration_error(&self) -> Option<f64> {
        self.curve.maximum_calibration_error()
    }

    /// The largest absolute per-candidate gap. Bin-free, so it cannot be
    /// averaged away by two candidates sharing a bin.
    pub fn maximum_candidate_calibration_error(&self) -> Option<f64> {
        self.per_candidate
            .iter()
            .filter(|row| row.observations > 0)
            .map(|row| row.gap.abs())
            .fold(None, |worst: Option<f64>, gap| {
                Some(worst.map_or(gap, |current: f64| current.max(gap)))
            })
    }

    /// The pooled Brier score, or `None` when it was not computable.
    pub fn brier_score(&self) -> Option<f64> {
        self.pooled.brier_score
    }

    /// The whole acceptance rule, as a pure function of this measurement and
    /// the ceilings.
    ///
    /// The same three-ceiling rule the verdict uses, exposed so a reader can
    /// apply it to a *marginal* measurement and see for themselves that the
    /// per-candidate ceiling is what refuses a vector the bins would have
    /// accepted. It is a method on both measurement types precisely so the two
    /// can be compared under one rule.
    pub fn passes(&self, tolerances: &AcceptanceTolerances) -> bool {
        matches!(
            (self.maximum_calibration_error(), self.maximum_candidate_calibration_error()),
            (Some(worst_bin), Some(worst_candidate))
                if self.expected_calibration_error() <= tolerances.max_expected_calibration_error
                    && worst_bin <= tolerances.max_calibration_error
                    && worst_candidate <= tolerances.max_candidate_calibration_error
        )
    }

    /// The pooled per-candidate log loss.
    pub fn log_loss(&self) -> Option<f64> {
        self.pooled.log_loss
    }

    /// Mean predicted mass over the K axis.
    pub fn mean_predicted(&self) -> f64 {
        self.pooled.mean_prediction
    }

    /// Fraction of candidate observations that served.
    pub fn observed_positive_rate(&self) -> f64 {
        self.pooled.mean_actual
    }
}

/// Measure the calibration of emitted distributions.
///
/// The only input is [`EmittedDecision`], and the only way to build one is
/// through [`DecisionDistribution::try_new`]. This function therefore cannot
/// be handed pre-normalization per-candidate numbers: the API does not exist,
/// and the marginal measurements it would need live in a different type it does
/// not accept.
pub fn measure_emitted(
    decisions: &[EmittedDecision],
    config: &ReliabilityConfig,
) -> Result<CalibrationMeasure, CalibrationError> {
    if config.bin_count == 0 {
        return Err(CalibrationError::ZeroReliabilityBins {
            bins: config.bin_count,
        });
    }
    if decisions.is_empty() {
        return Err(CalibrationError::NoObservations {
            context: "the emitted distribution",
        });
    }

    let mut predictions: Vec<f64> = Vec::new();
    let mut actuals: Vec<bool> = Vec::new();
    let mut attributed = 0usize;
    let mut multiclass_loss = 0.0_f64;

    for decision in decisions {
        let distribution = decision.distribution();
        for (index, outcome) in distribution.outcomes().iter().enumerate() {
            let Some(&mass) = distribution.probabilities().get(index) else {
                return Err(CalibrationError::Measurement {
                    context: "the emitted distribution",
                    reason: "the validated distribution has no probability for one of its own outcomes".to_string(),
                });
            };
            predictions.push(mass);
            actuals.push(decision.served() == Some(outcome));
        }
        if let Some(served) = decision.served() {
            attributed += 1;
            let mass = distribution
                .probability_of(served)
                .unwrap_or(0.0)
                .max(LOG_LOSS_CLAMP);
            multiclass_loss -= mass.ln();
        }
    }

    let pooled = PredictionMetrics::try_compute_classification(&predictions, &actuals)
        .map_err(|error| CalibrationError::Measurement {
            context: "the emitted distribution",
            reason: error.to_string(),
        })?;

    Ok(CalibrationMeasure {
        decisions: decisions.len(),
        attributed_decisions: attributed,
        observations: predictions.len(),
        curve: build_curve(&predictions, &actuals, config.bin_count),
        per_candidate: build_candidate_rows(decisions),
        pooled,
        multiclass_log_loss: (attributed > 0).then_some(multiclass_loss / attributed as f64),
    })
}

/// The bin-free per-candidate table.
///
/// Walks the axis of every decision, so a candidate that appears in some
/// decisions and not others is reported over the decisions it actually appeared
/// in, and the counts say so.
fn build_candidate_rows(decisions: &[EmittedDecision]) -> Vec<CandidateCalibration> {
    let mut order: Vec<CandidateIdentity> = Vec::new();
    let mut rows: Vec<CandidateCalibration> = Vec::new();

    for decision in decisions {
        let distribution = decision.distribution();
        for (index, outcome) in distribution.outcomes().iter().enumerate() {
            let mass = distribution.probabilities().get(index).copied().unwrap_or(0.0);
            let served = decision.served() == Some(outcome);
            let slot = match order.iter().position(|known| known == outcome) {
                Some(slot) => slot,
                None => {
                    order.push(outcome.clone());
                    rows.push(CandidateCalibration {
                        candidate: outcome.clone(),
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
            row.mean_predicted += mass;
            if served {
                row.served += 1;
            }
        }
    }

    for row in &mut rows {
        if row.observations > 0 {
            row.mean_predicted /= row.observations as f64;
            row.observed_frequency = row.served as f64 / row.observations as f64;
            row.gap = row.mean_predicted - row.observed_frequency;
        }
    }
    // A canonical candidate order, so two runs serialize identically.
    rows.sort_by(|left, right| {
        left.candidate
            .provider()
            .cmp(right.candidate.provider())
            .then_with(|| left.candidate.model().cmp(right.candidate.model()))
    });
    rows
}

fn build_curve(predictions: &[f64], actuals: &[bool], bin_count: usize) -> ReliabilityCurve {
    let bins = bin_count.max(1);
    let mut counts = vec![0usize; bins];
    let mut predicted_sum = vec![0.0_f64; bins];
    let mut positive = vec![0usize; bins];

    for (prediction, actual) in predictions.iter().zip(actuals) {
        let slot = bin_index(*prediction, bins);
        counts[slot] += 1;
        predicted_sum[slot] += *prediction;
        if *actual {
            positive[slot] += 1;
        }
    }

    let width = 1.0 / bins as f64;
    let entries = counts
        .iter()
        .enumerate()
        .map(|(slot, &count)| {
            let lower = slot as f64 * width;
            let upper = if slot + 1 == bins { 1.0 } else { lower + width };
            if count == 0 {
                ReliabilityBin {
                    lower,
                    upper,
                    count: 0,
                    mean_predicted: 0.0,
                    observed_frequency: 0.0,
                    gap: 0.0,
                }
            } else {
                let mean_predicted = predicted_sum[slot] / count as f64;
                let observed_frequency = positive[slot] as f64 / count as f64;
                ReliabilityBin {
                    lower,
                    upper,
                    count,
                    mean_predicted,
                    observed_frequency,
                    gap: mean_predicted - observed_frequency,
                }
            }
        })
        .collect();

    ReliabilityCurve {
        bin_count: bins,
        observations: predictions.len(),
        bins: entries,
    }
}

fn bin_index(value: f64, bins: usize) -> usize {
    // A `NaN` prediction lands in the first bin rather than reaching the cast
    // below, and the accepted fallible metric constructor is what keeps a `NaN`
    // out of the vectors this is called with in the first place.
    if value.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return 0;
    }
    if value >= 1.0 {
        return bins - 1;
    }
    let slot = (value * bins as f64).floor() as usize;
    if slot >= bins {
        bins - 1
    } else {
        slot
    }
}

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
    let actuals: Vec<bool> = observations.iter().map(|observation| observation.served).collect();
    let pooled = PredictionMetrics::try_compute_classification(&predictions, &actuals)
        .map_err(|error| CalibrationError::Measurement {
            context: "the marginal route",
            reason: error.to_string(),
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
        match (self.maximum_candidate_error_before, self.maximum_candidate_error_after) {
            (Some(before), Some(after)) => Some(after - before),
            _ => None,
        }
    }
}

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
        let f = fit_bins.get(slot).copied().unwrap_or(0.0).max(PSI_PROPORTION_FLOOR);
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

fn attributed_rate(cohorts: &[DecisionCohort]) -> f64 {
    if cohorts.is_empty() {
        return 0.0;
    }
    let attributed = cohorts.iter().filter(|cohort| cohort.is_attributed()).count();
    attributed as f64 / cohorts.len() as f64
}

fn mean_arity(cohorts: &[DecisionCohort]) -> f64 {
    if cohorts.is_empty() {
        return 0.0;
    }
    cohorts.iter().map(|cohort| cohort.arity()).sum::<usize>() as f64 / cohorts.len() as f64
}

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
                .map_or(f64::NAN, |worst| worst),
            self.final_vector
                .maximum_candidate_calibration_error()
                .map_or(f64::NAN, |worst| worst),
            self.recomputed_verdict(),
            self.uncalibrated_joint.expected_calibration_error(),
            self.normalization_damage.delta,
            self.drift.summary(),
        )
    }
}

/// Everything one offline calibration run produced.
pub struct CalibrationOutcome {
    calibrator: KWayCalibrator,
    marginal_calibrator: MarginalCalibrator,
    holdout: Vec<EmittedDecision>,
    report: CalibrationReport,
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
        self.holdout.iter().map(EmittedDecision::to_record).collect()
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

// ---------------------------------------------------------------------------
// run_calibration
// ---------------------------------------------------------------------------

/// Fit a joint K-way calibrator, measure the vector it emits, and report
/// honestly.
///
/// The run is pure. The snapshot, the model, and the configuration are the only
/// inputs; a fitted calibrator, one emitted distribution per holdout decision,
/// and the report are the only outputs. Nothing is installed, and nothing in
/// the running product calls this.
///
/// The order of the gates is the order of the claims. The configuration is
/// checked first, because a vacuous gate must be refused before it can report.
/// The partition is taken on the canonical content order, so fit and holdout
/// are disjoint and the split does not depend on row arrival. Both partitions
/// are checked for degeneracy before anything is fitted, so a degenerate
/// holdout cannot produce a fitted calibrator. The drift gate runs on the two
/// partitions and **refuses the whole run** when it is out of tolerance,
/// carrying the measurement in the error. Only then is the final vector
/// measured and judged.
pub fn run_calibration(
    snapshot: &[OutcomeTrainingSample],
    model: &dyn RoutingModel,
    config: &CalibrationConfig,
) -> Result<CalibrationOutcome, CalibrationError> {
    config.checked()?;

    let cohorts = project_cohorts(snapshot, model, config.probability_floor)?;
    let needed = config
        .holdout
        .holdout_cohorts
        .saturating_add(config.holdout.min_fit_cohorts);
    if cohorts.len() < needed {
        return Err(CalibrationError::SnapshotTooSmall {
            cohorts: cohorts.len(),
            holdout: config.holdout.holdout_cohorts,
            min_fit: config.holdout.min_fit_cohorts,
        });
    }

    let split = cohorts.len() - config.holdout.holdout_cohorts;
    let (fit, holdout) = cohorts.split_at(split);
    check_partition(PartitionKind::Fit, fit, config)?;
    check_partition(PartitionKind::Holdout, holdout, config)?;

    // Drift before fitting: a calibrator fitted across a regime change and
    // reported on both sides of it is not a measurement of anything, so there
    // is no point spending a fit on it.
    let drift = measure_drift(fit, holdout, &config.drift)?;
    if !drift.is_acceptable() {
        return Err(CalibrationError::DistributionShift {
            measurement: Box::new(drift),
        });
    }

    let calibrator = KWayCalibrator::fit(fit, config, config.probability_floor)?;
    let marginal_calibrator =
        MarginalCalibrator::fit(&fit_marginal_pairs(fit, config.probability_floor)?, &config.marginal, config.probability_floor)?;

    let mut emitted = Vec::with_capacity(holdout.len());
    for cohort in holdout {
        emitted.push(calibrator.distribution(cohort, config.probability_floor)?);
    }
    let final_vector = measure_emitted(&emitted, &config.reliability)?;

    let uncalibrated = KWayCalibrator::uncalibrated();
    let mut baseline = Vec::with_capacity(holdout.len());
    for cohort in holdout {
        baseline.push(uncalibrated.distribution(cohort, config.probability_floor)?);
    }
    let uncalibrated_joint = measure_emitted(&baseline, &config.reliability)?;

    let marginal_observations =
        collect_marginal_observations(holdout, &marginal_calibrator, config.probability_floor)?;
    let marginal_calibrated = measure_marginal(
        &marginal_observations,
        MarginalView::Calibrated,
        &config.reliability,
    )?;
    let marginal_normalized = measure_marginal(
        &marginal_observations,
        MarginalView::Normalized,
        &config.reliability,
    )?;
    let normalization_damage =
        NormalizationDamage::measure(&marginal_calibrated, &marginal_normalized);

    let tolerances = AcceptanceTolerances::from(&config.reliability);
    let report = CalibrationReport {
        cohorts_total: cohorts.len(),
        fit_cohorts: fit.len(),
        holdout_cohorts: holdout.len(),
        fit_attributed_cohorts: fit.iter().filter(|cohort| cohort.is_attributed()).count(),
        holdout_attributed_cohorts: holdout.iter().filter(|cohort| cohort.is_attributed()).count(),
        ranked_observations: final_vector.observations,
        unranked_candidates_emitted: emitted
            .iter()
            .map(|decision| decision.unranked().len())
            .sum(),
        fit_attributed_rate: attributed_rate(fit),
        holdout_attributed_rate: attributed_rate(holdout),
        verdict: CalibrationVerdict::from_measurements(&final_vector, &tolerances),
        final_vector,
        uncalibrated_joint,
        marginal_calibrated,
        marginal_normalized,
        normalization_damage,
        drift,
        tolerances,
        distribution_role: DISTRIBUTION_ROLE.to_string(),
    };

    Ok(CalibrationOutcome {
        calibrator,
        marginal_calibrator,
        holdout: emitted,
        report,
    })
}

/// The fit partition's per-candidate `(raw probability, served)` pairs.
fn fit_marginal_pairs(
    fit: &[DecisionCohort],
    probability_floor: f64,
) -> Result<Vec<(f64, bool)>, CalibrationError> {
    let mut pairs = Vec::new();
    for cohort in fit {
        for input in cohort.candidates() {
            if let Some(raw) = input.raw_success_probability() {
                log_probability(raw, probability_floor)?;
                pairs.push((raw, cohort.served() == Some(input.candidate())));
            }
        }
    }
    if pairs.is_empty() {
        return Err(CalibrationError::NoObservations {
            context: "the fit partition's candidate marginals",
        });
    }
    Ok(pairs)
}

/// Refuse a partition that cannot support a calibration claim.
fn check_partition(
    kind: PartitionKind,
    cohorts: &[DecisionCohort],
    config: &CalibrationConfig,
) -> Result<(), CalibrationError> {
    let floor = match kind {
        PartitionKind::Fit => config.holdout.min_fit_cohorts,
        PartitionKind::Holdout => config.holdout.holdout_cohorts,
    };
    if cohorts.len() < floor {
        return Err(CalibrationError::DegeneratePartition {
            partition: kind,
            reason: DegeneracyReason::TooFewCohorts,
            detail: format!("{} decisions, floor {floor}", cohorts.len()),
        });
    }

    let ranked: usize = cohorts.iter().map(|cohort| cohort.ranked_count()).sum();
    if ranked == 0 {
        return Err(CalibrationError::DegeneratePartition {
            partition: kind,
            reason: DegeneracyReason::NoRankedCandidateAnywhere,
            detail: "no candidate in the partition carries a prediction".to_string(),
        });
    }

    let attributed: Vec<&CandidateIdentity> = cohorts
        .iter()
        .filter_map(|cohort| cohort.served())
        .collect();
    if attributed.is_empty() {
        return Err(CalibrationError::DegeneratePartition {
            partition: kind,
            reason: DegeneracyReason::NoAttributedOutcome,
            detail: "no decision was served, so every observed frequency would be zero".to_string(),
        });
    }
    if attributed.len() < config.holdout.min_attributed_outcomes {
        return Err(CalibrationError::DegeneratePartition {
            partition: kind,
            reason: DegeneracyReason::TooFewAttributedOutcomes,
            detail: format!(
                "{} served decisions, floor {}",
                attributed.len(),
                config.holdout.min_attributed_outcomes
            ),
        });
    }

    let mut winners: Vec<&CandidateIdentity> = Vec::new();
    for identity in &attributed {
        if !winners.contains(identity) {
            winners.push(identity);
        }
    }
    if winners.len() < 2 {
        return Err(CalibrationError::DegeneratePartition {
            partition: kind,
            reason: DegeneracyReason::SingleServedCandidate,
            detail: format!(
                "only {}/{} ever served, so the axis cannot be told apart and a uniform 1/K answer would score the base rate",
                winners[0].provider(),
                winners[0].model()
            ),
        });
    }
    Ok(())
}
