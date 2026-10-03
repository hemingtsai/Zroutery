//! Offline supervised warmup for the `shadow` model lineage.
//!
//! # What this module is
//!
//! This is the first consumer of the collected training dataset. It takes a
//! dataset snapshot, trains the four per-dimension routing models on it through
//! the accepted training seam, verifies the artifact that comes back, and
//! reports honestly whether the result beats the cold baseline.
//!
//! It is a pure library entry point. It performs no server wiring, registers no
//! background work, schedules nothing, and installs nothing: the live predictor
//! production routes with is never named here, because a module that cannot name
//! it cannot reach it. Producing a verified commit is this module's whole job;
//! installing one belongs to the node that owns installation.
//!
//! # Sample shape
//!
//! The input is the canonical [`OutcomeTrainingSample`] — the exact value a
//! dataset snapshot retains, and the only shape production collects. One
//! request produces one row per attempt plus one row for the request as a
//! whole, and those rows describe a single observation, so warmup partitions
//! the snapshot by whole request/outcome group: every row of one request lands
//! on one side of the split, and the holdout has to clear an independent
//! request-count floor before it can decide a verdict.
//!
//! The accepted training seam, [`ModelEnsemblePredictor::try_train`], accepts
//! only the legacy shape, so warmup projects once, internally, through
//! [`OutcomeTrainingSample::into_legacy`] at the training boundary.
//!
//! Canonical-in is not tidiness. The evidence warmup needs in order to *police*
//! a label — the identity slots, the scope, the attempt id, the full ordered
//! attempt list, the terminal error facts, and the optional
//! [`Feedback`](crate::feedback::Feedback) — is exactly the evidence that
//! projection drops. A legacy-only input would force warmup to trust the very
//! labels it exists to verify. The projection is proven to carry the labels
//! verbatim by test, not assumed to.
//!
//! # The typed decision contract
//!
//! [`DecisionModel`] participates here as a *load* boundary, never as a
//! training one. The contract deliberately exposes no `update`, no `train`, and
//! no `&mut self`: it is a typed, artifact-pinned scorer, and a training row has
//! no representation in it at all — [`ModelInput`](super::decision_contract::ModelInput)
//! and [`DecisionCandidate`] describe what was known at decision time and carry
//! no label, no measurement, and no outcome. Warmup therefore loads the four
//! states it trained through [`DecisionModel::try_from_states`], pinned to the
//! commit it just verified, and scores one typed candidate to prove every
//! per-dimension prediction is finite. That the contract cannot accept a
//! training row is the reason training belongs on the legacy seam, and warmup
//! refuses rather than reporting an artifact the decision surface cannot load.
//!
//! No distribution is produced here: a K-way distribution over observed
//! candidates is a different node's artifact, and warmup does not pre-empt it.
//!
//! # Determinism
//!
//! Training is an ordered online update over an ordered partition of the
//! snapshot, and both the canonical commit identity and the checkpoint content
//! hash deliberately exclude wall-clock fields. The same snapshot and the same
//! configuration therefore produce byte-identical model state and an identical
//! commit id. Two wall-clock metadata fields exist on the artifacts involved —
//! the checkpoint's `created_at` and the frozen holdout's `frozen_at` — and
//! neither is model identity, so neither is part of that claim.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::dataset::{
    validate_outcome_sample, OutcomeTrainingSample, SampleScope,
    TrainingSample as DatasetTrainingSample,
};
use super::decision_contract::{
    CandidateEligibility, DecisionCandidate, DecisionDimension, DecisionModel, DecisionModelStates,
};
use super::evaluation::{FrozenHoldout, PredictionMetrics};
use super::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use super::model::RoutingModel;
use super::model_identity::{CommitId, ModelCommit, ModelEnsemble, ReplayError};
use super::shadow::ModelEnsemblePredictor;
use crate::outcome::CandidateIdentity;

// ---------------------------------------------------------------------------
// WarmupConfig
// ---------------------------------------------------------------------------

/// Configuration of one warmup run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarmupConfig {
    /// Fraction of the snapshot reserved for the frozen holdout, in `(0, 1)`.
    pub holdout_ratio: f64,
    /// Smallest holdout that may decide a verdict, counted in rows. A smaller
    /// holdout cannot support an honest comparison, so a snapshot that cannot
    /// produce one is refused instead of being split.
    pub min_holdout: usize,
    /// Smallest holdout measured in *whole requests*. Independent of the row
    /// floor: one canonical request can contribute several rows, so a row floor
    /// alone lets a holdout clear the bar with a single request, which is not
    /// independent evidence. A snapshot that cannot hold out this many whole
    /// requests is refused.
    #[serde(default = "default_min_holdout_requests")]
    pub min_holdout_requests: usize,
    /// Description recorded on the frozen holdout artifact.
    pub holdout_description: String,
}

/// Compatibility default for a configuration that names no request floor.
fn default_min_holdout_requests() -> usize {
    1
}

impl WarmupConfig {
    /// A configuration with an explicit holdout ratio and row floor.
    ///
    /// The request floor defaults to one whole request; callers that need a
    /// larger independent-evidence bar set [`WarmupConfig::min_holdout_requests`]
    /// explicitly.
    pub fn new(holdout_ratio: f64, min_holdout: usize) -> Self {
        WarmupConfig {
            holdout_ratio,
            min_holdout,
            min_holdout_requests: default_min_holdout_requests(),
            holdout_description: String::new(),
        }
    }

    fn checked(&self) -> Result<(), WarmupError> {
        if !self.holdout_ratio.is_finite() || self.holdout_ratio <= 0.0 || self.holdout_ratio >= 1.0
        {
            return Err(WarmupError::InvalidHoldoutRatio {
                ratio: self.holdout_ratio,
            });
        }
        if self.min_holdout == 0 {
            return Err(WarmupError::EmptyHoldoutFloor);
        }
        if self.min_holdout_requests == 0 {
            return Err(WarmupError::EmptyHoldoutRequestFloor);
        }
        Ok(())
    }
}

impl Default for WarmupConfig {
    fn default() -> Self {
        WarmupConfig {
            holdout_ratio: 0.25,
            min_holdout: 8,
            min_holdout_requests: 8,
            holdout_description: "supervised warmup holdout".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// WarmupError
// ---------------------------------------------------------------------------

/// Every way a warmup run refuses.
///
/// Warmup fails closed: a refusal is a typed value with a reason, never a panic
/// and never a silently empty or untrained model handed back as a success.
#[derive(Debug, Clone, thiserror::Error)]
pub enum WarmupError {
    /// The snapshot held no samples at all.
    #[error("warmup dataset is empty")]
    EmptyDataset,

    /// The snapshot cannot be split into a trainable partition and a holdout
    /// that meets the configured row floor.
    #[error(
        "warmup dataset of {samples} samples cannot hold out {min_holdout} samples and still train"
    )]
    DatasetTooSmallToHoldOut { samples: usize, min_holdout: usize },

    /// The snapshot cannot be split into a trainable partition and a holdout
    /// that meets the configured request floor, counted in whole requests.
    #[error(
        "warmup dataset of {requests} requests cannot hold out {min_requests} whole requests and still train"
    )]
    TooFewRequestsToHoldOut {
        requests: usize,
        min_requests: usize,
    },

    /// The configured holdout ratio is not a fraction in `(0, 1)`.
    #[error("holdout ratio {ratio} is not a fraction in (0, 1)")]
    InvalidHoldoutRatio { ratio: f64 },

    /// The configured holdout floor is zero, which would decide a verdict from
    /// a holdout of no samples.
    #[error("min_holdout must be at least 1")]
    EmptyHoldoutFloor,

    /// The configured request floor is zero, which would decide a verdict from
    /// a holdout of no independent requests.
    #[error("min_holdout_requests must be at least 1")]
    EmptyHoldoutRequestFloor,

    /// A sample was encoded against a different schema than the one this build
    /// accepts.
    #[error(
        "warmup sample {index} carries {component} schema version {found}, expected {expected}"
    )]
    SchemaMismatch {
        index: usize,
        component: &'static str,
        found: u32,
        expected: u32,
    },

    /// A sample's feature vector is not the width the models are built for.
    #[error(
        "warmup sample {index} carries a feature vector of width {found}, expected {expected}"
    )]
    FeatureDimension {
        index: usize,
        found: usize,
        expected: usize,
    },

    /// A sample's feature vector holds a value no model may be trained on.
    #[error("warmup sample {index} has a non-finite feature at position {position}: {value}")]
    NonFiniteFeature {
        index: usize,
        position: usize,
        value: f32,
    },

    /// A non-success sample carried a timing target. Timing is what a completed
    /// success observed; a failure that also reports it is contradictory
    /// evidence, and training a regression head on it would teach a duration
    /// for a request that never completed. `cost` is deliberately exempt: it is
    /// a captured billing fact for a non-success terminal result too, and the
    /// dataset records it that way on purpose.
    #[error(
        "warmup sample {index} is not a success but carries {dimension} = {value}; \
         a non-success sample must keep timing targets absent"
    )]
    NonSuccessTiming {
        index: usize,
        dimension: &'static str,
        value: f64,
    },

    /// The canonical sample failed its own validator.
    #[error("warmup sample {index} is invalid: {reason}")]
    InvalidSample { index: usize, reason: String },

    /// The snapshot held the same sample id twice. Two rows sharing an id could
    /// put one sample in both partitions, so the holdout would no longer be
    /// disjoint from training.
    #[error("warmup snapshot holds sample id '{sample_id}' at indexes {first} and {second}; the holdout could not stay disjoint from training")]
    DuplicateSampleId {
        sample_id: String,
        first: usize,
        second: usize,
    },

    /// The produced chain is not a verifiable root-to-commit lineage.
    #[error("warmup lineage is not verifiable: {reason}")]
    BrokenLineage { reason: String },

    /// The accepted seam produced an artifact that does not load back.
    #[error("the warmed artifact does not load back: {reason}")]
    ArtifactNotLoadable { reason: String },

    /// The trained states do not load into the typed decision contract.
    #[error("the warmed artifact does not load into the typed decision contract: {reason}")]
    ContractNotLoadable { reason: String },

    /// The typed decision contract produced a non-finite prediction from the
    /// warmed artifact.
    #[error("the typed decision contract scored a non-finite {dimension}: {value}")]
    ContractScoreNonFinite { dimension: &'static str, value: f64 },

    /// The accepted training seam refused the payload.
    #[error("the accepted training seam refused the warmup payload: {0}")]
    Training(ReplayError),
}

impl From<ReplayError> for WarmupError {
    fn from(error: ReplayError) -> Self {
        WarmupError::Training(error)
    }
}

// ---------------------------------------------------------------------------
// WarmupVerdict / WarmupReport
// ---------------------------------------------------------------------------

/// Whether the warmed model beat the cold baseline on the frozen holdout.
///
/// `Better` is the only value that constitutes an improvement claim. Every other
/// value says the same thing in different words: this artifact is verified, and
/// it is not better than what was already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarmupVerdict {
    /// Strictly lower log loss than the cold baseline, with no Brier regression.
    Better,
    /// The holdout did not separate the two models.
    NotBetter,
    /// At least one classification metric regressed against the cold baseline.
    Worse,
}

impl WarmupVerdict {
    /// Whether this verdict is an improvement claim at all.
    pub fn is_improvement(self) -> bool {
        matches!(self, Self::Better)
    }

    /// The whole of the "better than baseline" rule, as a pure function of the
    /// two deltas.
    ///
    /// A negative `log_loss_delta` is required, and a positive
    /// `brier_delta` is disqualifying on its own: a model can be better
    /// calibrated on average while getting individual rows worse, and that is not
    /// an improvement. Equal on both counts is reported as *not better* rather
    /// than as a win, because a holdout that cannot separate two models is
    /// evidence of nothing.
    pub fn from_deltas(log_loss_delta: f64, brier_delta: f64) -> Self {
        if log_loss_delta < 0.0 && brier_delta <= 0.0 {
            WarmupVerdict::Better
        } else if log_loss_delta > 0.0 || brier_delta > 0.0 {
            WarmupVerdict::Worse
        } else {
            WarmupVerdict::NotBetter
        }
    }

    /// Withhold an improvement claim the holdout cannot support.
    ///
    /// A holdout holding one class only cannot demonstrate discrimination: the
    /// cold baseline answers every row with `0.5` and therefore scores the
    /// entropy of that one class, so *any* model that has learned the base rate
    /// — including one trained on nothing but failures — "improves" on it. That
    /// is a fact about the base rate, not about the weights, so it is not
    /// reported as an improvement. A `Worse` verdict survives: a model that is
    /// confidently wrong on a single-class holdout really is worse.
    pub fn downgraded_to_not_better(self) -> Self {
        match self {
            Self::Better => Self::NotBetter,
            other => other,
        }
    }
}

/// How much supervision each per-dimension head actually received, over the
/// training partition.
///
/// The success target is present on every row by construction. The other three
/// are `Option` targets, so a head can be trained on far fewer rows than the
/// success head, and a run that says nothing about that is hiding the most
/// common way a warmup is quietly half-trained.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelCoverage {
    /// Rows carrying a success label.
    pub success: usize,
    /// Rows carrying a latency target.
    pub latency_ms: usize,
    /// Rows carrying a time-to-first-token target.
    pub ttft_ms: usize,
    /// Rows carrying a cost target.
    pub cost: usize,
    /// Rows carrying a feedback signal. A row without one contributes no
    /// rating, and warmup never supplies one.
    pub feedback: usize,
}

/// What "better than baseline" means here, in one place.
///
/// The baseline is the accepted cold ensemble: four zero-weight models that
/// answer every candidate with the same number, which makes its log loss the
/// entropy of the holdout's own success rate. Beating it is a claim about the
/// trained weights, not about the data being separable.
pub const BASELINE_DESCRIPTION: &str = "accepted cold ensemble (untrained, constant prediction)";

/// The honest report of one warmup run.
///
/// Every field is a function of the snapshot and the configuration alone. There
/// is no timestamp, no duration, and no run counter in here, so two runs of the
/// same input produce two identical reports. Comparison is done through the
/// serialized form rather than `PartialEq`, because the metrics it holds are the
/// accepted evaluator's own and carry no equality of their own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarmupReport {
    /// Rows in the snapshot.
    pub samples_total: usize,
    /// Rows the models were trained on.
    pub train_samples: usize,
    /// Rows reserved for the frozen holdout.
    pub holdout_samples: usize,
    /// Distinct requests the models were trained on.
    #[serde(default)]
    pub train_requests: usize,
    /// Distinct requests reserved for the frozen holdout.
    #[serde(default)]
    pub holdout_requests: usize,
    /// Fraction of the holdout whose label is a success.
    pub holdout_positive_rate: f64,
    /// Whether the holdout holds both classes. A single-class holdout can only
    /// demonstrate that the model learned the base rate, so it can never carry
    /// an improvement claim.
    pub holdout_is_degenerate: bool,
    /// Whether the two partitions share no sample id and no request/outcome
    /// group. Always true for a successful run; recorded so a reader does not
    /// have to take it on trust.
    pub holdout_disjoint_from_train: bool,
    /// Training rows labelled a success.
    pub train_success: usize,
    /// Training rows labelled a non-success.
    pub train_non_success: usize,
    /// Per-dimension label coverage over the training partition.
    pub coverage: LabelCoverage,
    /// What the accepted cold baseline scores on the holdout.
    pub baseline: PredictionMetrics,
    /// What the warmed model scores on the same holdout.
    pub warmed: PredictionMetrics,
    /// `warmed.log_loss - baseline.log_loss`; negative is an improvement.
    pub log_loss_delta: f64,
    /// `warmed.brier_score - baseline.brier_score`; negative is an improvement.
    pub brier_delta: f64,
    /// The verdict, derived from the two deltas.
    pub verdict: WarmupVerdict,
    /// The comparison this report made, stated in words.
    pub baseline_description: String,
}

impl WarmupReport {
    /// Whether this report claims an improvement.
    ///
    /// There is no separate stored flag that could disagree with the verdict.
    pub fn is_improvement(&self) -> bool {
        self.verdict.is_improvement()
    }
}

// ---------------------------------------------------------------------------
// WarmupOutcome
// ---------------------------------------------------------------------------

/// Everything one warmup run produced.
pub struct WarmupOutcome {
    commit: ModelCommit,
    lineage: Vec<ModelCommit>,
    ensemble: ModelEnsemble,
    holdout: FrozenHoldout,
    report: WarmupReport,
}

impl std::fmt::Debug for WarmupOutcome {
    /// Structural, without dumping four weight vectors.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WarmupOutcome")
            .field("commit_id", &self.commit.commit_id.as_str())
            .field("parent", &self.commit.parent)
            .field("lineage_len", &self.lineage.len())
            .field("verdict", &self.report.verdict)
            .field("improvement_claimed", &self.report.is_improvement())
            .finish()
    }
}

impl WarmupOutcome {
    /// The verified commit this run produced.
    pub fn commit(&self) -> &ModelCommit {
        &self.commit
    }

    /// The identity of the produced commit.
    pub fn commit_id(&self) -> CommitId {
        self.commit.commit_id.clone()
    }

    /// The produced commit's parent, which is `Some` whenever any sample was
    /// trained: warmup never rewrites history into a root it did not start at.
    pub fn parent(&self) -> Option<CommitId> {
        self.commit.parent.clone()
    }

    /// The complete root-to-commit lineage, starting at the cold genesis commit.
    pub fn lineage(&self) -> &[ModelCommit] {
        &self.lineage
    }

    /// The trained ensemble, loaded back out of the produced commit.
    pub fn ensemble(&self) -> &ModelEnsemble {
        &self.ensemble
    }

    /// The frozen holdout the report was computed on.
    pub fn holdout(&self) -> &FrozenHoldout {
        &self.holdout
    }

    /// The honest report.
    pub fn report(&self) -> &WarmupReport {
        &self.report
    }

    /// The four per-dimension states, ready to be loaded into the typed
    /// decision contract.
    pub fn decision_model_states(&self) -> DecisionModelStates {
        DecisionModelStates {
            success: self.ensemble.success.save(),
            latency: self.ensemble.latency.save(),
            ttft: self.ensemble.ttft.save(),
            cost: self.ensemble.cost.save(),
        }
    }

    /// Load the warmed artifact into the typed decision contract, pinned to the
    /// commit that holds it.
    ///
    /// This is the whole hand-off: warmup can produce a contract that scores the
    /// artifact, and it installs nothing.
    pub fn decision_model(&self) -> Result<DecisionModel, WarmupError> {
        DecisionModel::try_from_states(
            FEATURE_DIMENSION,
            FEATURE_SCHEMA_VERSION,
            self.commit_id(),
            &self.decision_model_states(),
        )
        .map_err(|reason| WarmupError::ContractNotLoadable {
            reason: reason.to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Head scope and projection
// ---------------------------------------------------------------------------

/// Which canonical scope supervises one per-dimension head.
///
/// The canonical dataset emits an attempt-scope row and a request-scope row for
/// one request, and those two rows can carry identical decision-time features
/// while describing different durations: the attempt row carries the candidate's
/// own service time and the request row carries the whole request's total. A
/// head supervised by both would learn a mixture of two durations from one
/// feature vector, so each head names the one scope it consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadScope {
    /// The request-scope row only. Attempt-scope rows of the same request carry
    /// no target for this head.
    Request,
    /// Every row. Both scopes carry a true label about their own subject, and
    /// the head is trained on each; the two labels agree whenever the rows
    /// share a feature vector.
    EveryRow,
}

/// The scope each per-dimension head consumes, stated once.
///
/// The whole-request heads are `Request`-scoped: `latency` is the request's
/// total duration, `ttft` is the first token of the request, and `cost` is the
/// request's captured billing fact (attempt rows carry no cost at all). The
/// success head is `EveryRow`-scoped: an attempt row says whether that
/// candidate's attempt succeeded and the request row says whether the request
/// terminated in success, and warmup never rewrites the first into the second,
/// so a failed candidate cannot become a positive label.
pub fn head_scope(dimension: DecisionDimension) -> HeadScope {
    match dimension {
        DecisionDimension::Success => HeadScope::EveryRow,
        DecisionDimension::Latency | DecisionDimension::Ttft | DecisionDimension::Cost => {
            HeadScope::Request
        }
    }
}

/// Project one canonical sample to the legacy training row, with every head's
/// documented scope applied.
///
/// This is the only projection warmup trains through. It starts from the raw
/// [`OutcomeTrainingSample::into_legacy`] conversion, which copies the targets
/// verbatim, and then removes from an attempt-scope row exactly the targets that
/// [`head_scope`] assigns to the request scope. The result is that one candidate
/// feature vector is never supervised by both its own single service duration
/// and the whole request's total, while both rows stay in the snapshot: the
/// request row keeps the terminal success and the captured cost, and the attempt
/// row keeps the candidate's own success.
pub fn project_for_training(sample: &OutcomeTrainingSample) -> DatasetTrainingSample {
    let mut row = sample.clone().into_legacy();
    if sample.scope != SampleScope::Request {
        row.targets.latency_ms = None;
        row.targets.ttft_ms = None;
        row.targets.cost = None;
    }
    row
}

// ---------------------------------------------------------------------------
// run_warmup
// ---------------------------------------------------------------------------

/// Train the four routing models on a dataset snapshot and report honestly.
///
/// The run is pure: the snapshot and the configuration are the only inputs, and
/// a trained ensemble, a verified commit, its complete lineage, the frozen
/// holdout, and the report are the only outputs. Nothing is installed, and
/// nothing in the running product calls this.
pub fn run_warmup(
    snapshot: &[OutcomeTrainingSample],
    config: &WarmupConfig,
) -> Result<WarmupOutcome, WarmupError> {
    let partition = partition_snapshot(snapshot, config)?;
    let training: Vec<DatasetTrainingSample> = partition
        .training
        .iter()
        .map(project_for_training)
        .collect();
    let holdout_samples: Vec<DatasetTrainingSample> = partition
        .holdout
        .iter()
        .map(project_for_training)
        .collect();

    let holdout = FrozenHoldout::new(holdout_samples.clone(), config.holdout_description.clone());
    let (lineage, commit) = train_lineage(&training)?;
    let ensemble = ModelEnsemble::load_all(&commit.checkpoint).map_err(|error| {
        WarmupError::ArtifactNotLoadable {
            reason: error.to_string(),
        }
    })?;
    attest_contract(&commit, &ensemble)?;

    let cold = ModelEnsemble::new();
    let baseline = holdout.evaluate(&cold.success);
    let warmed = holdout.evaluate(&ensemble.success);
    let (log_loss_delta, brier_delta, metric_verdict) = compare(&baseline, &warmed);
    let holdout_is_degenerate = !holdout_has_both_classes(&holdout_samples);
    let verdict = if holdout_is_degenerate {
        metric_verdict.downgraded_to_not_better()
    } else {
        metric_verdict
    };

    let report = WarmupReport {
        samples_total: training.len() + holdout_samples.len(),
        train_samples: training.len(),
        holdout_samples: holdout_samples.len(),
        train_requests: partition.train_requests(),
        holdout_requests: partition.holdout_requests(),
        holdout_positive_rate: positive_rate(&holdout_samples),
        holdout_is_degenerate,
        holdout_disjoint_from_train: partition.is_disjoint(),
        train_success: training
            .iter()
            .filter(|sample| sample.targets.success)
            .count(),
        train_non_success: training
            .iter()
            .filter(|sample| !sample.targets.success)
            .count(),
        coverage: coverage(&training),
        baseline,
        warmed,
        log_loss_delta,
        brier_delta,
        verdict,
        baseline_description: BASELINE_DESCRIPTION.to_string(),
    };

    Ok(WarmupOutcome {
        commit,
        lineage,
        ensemble,
        holdout,
        report,
    })
}

// ---------------------------------------------------------------------------
// Partition
// ---------------------------------------------------------------------------

/// One whole request/outcome group: every canonical row that shares a request
/// identity, kept together so a split can only cut between groups.
struct SampleGroup {
    /// `request_id` when the sample carries one, else its `outcome_id`.
    key: String,
    /// The earliest timestamp in the group.
    timestamp: i64,
    rows: Vec<OutcomeTrainingSample>,
}

/// The two partitions of one warmup run, as whole request/outcome groups.
///
/// Both fields hold the canonical rows rather than the projected legacy shape,
/// because group identity lives on the canonical sample and the projection
/// drops it. The order is the deterministic group order the split used, so a
/// caller that trains the rows itself reproduces warmup's chain.
#[derive(Debug, Clone, PartialEq)]
pub struct WarmupPartition {
    /// The whole groups the models are trained on, in group order.
    pub training: Vec<OutcomeTrainingSample>,
    /// The trailing whole groups frozen as the holdout, in group order.
    pub holdout: Vec<OutcomeTrainingSample>,
}

impl WarmupPartition {
    /// Distinct requests in the training partition.
    pub fn train_requests(&self) -> usize {
        request_count(&self.training)
    }

    /// Distinct requests in the holdout.
    pub fn holdout_requests(&self) -> usize {
        request_count(&self.holdout)
    }

    /// Whether no sample id and no request/outcome group appears on both sides.
    pub fn is_disjoint(&self) -> bool {
        let training_ids: HashSet<&str> = self
            .training
            .iter()
            .map(|sample| sample.sample_id.as_str())
            .collect();
        if self
            .holdout
            .iter()
            .any(|sample| training_ids.contains(sample.sample_id.as_str()))
        {
            return false;
        }
        let training_groups: HashSet<&str> = self
            .training
            .iter()
            .map(group_key)
            .collect();
        !self
            .holdout
            .iter()
            .any(|sample| training_groups.contains(group_key(sample)))
    }
}

/// Split a snapshot into the whole groups warmup trains on and the whole groups
/// it freezes, and nothing else.
///
/// The split is by request/outcome group: the canonical dataset emits one row
/// per attempt plus one row for the request as a whole, and those rows describe
/// a single observation. Cutting the ordered row list at an arbitrary boundary
/// can put the attempt row of a request in training and the request row of the
/// same request in the holdout, which leaves the holdout nominal rather than
/// disjoint. This function cuts between groups, so every row of one request
/// stays on one side, and it verifies the result before returning it.
///
/// It is the split [`run_warmup`] performs, exposed so a caller can audit the
/// partition without training.
pub fn partition_snapshot(
    snapshot: &[OutcomeTrainingSample],
    config: &WarmupConfig,
) -> Result<WarmupPartition, WarmupError> {
    config.checked()?;
    if snapshot.is_empty() {
        return Err(WarmupError::EmptyDataset);
    }
    for (index, sample) in snapshot.iter().enumerate() {
        validate_sample(index, sample)?;
    }
    reject_duplicate_ids(snapshot)?;

    let groups = order_groups(group_snapshot(snapshot));
    let holdout_groups = holdout_group_count(&groups, snapshot.len(), config)?;
    let split = groups.len() - holdout_groups;

    let training: Vec<OutcomeTrainingSample> = groups[..split]
        .iter()
        .flat_map(|group| group.rows.iter().cloned())
        .collect();
    let holdout: Vec<OutcomeTrainingSample> = groups[split..]
        .iter()
        .flat_map(|group| group.rows.iter().cloned())
        .collect();

    let partition = WarmupPartition { training, holdout };
    if !partition.is_disjoint() {
        return Err(WarmupError::BrokenLineage {
            reason: "holdout shares a sample id or a request group with the training partition"
                .to_string(),
        });
    }
    Ok(partition)
}

/// The identity a canonical row is grouped and split by.
fn group_key(sample: &OutcomeTrainingSample) -> &str {
    if sample.request_id.is_empty() {
        sample.outcome_id.as_str()
    } else {
        sample.request_id.as_str()
    }
}

/// Collapse a snapshot into one group per request identity.
fn group_snapshot(snapshot: &[OutcomeTrainingSample]) -> Vec<SampleGroup> {
    let mut index: HashMap<String, usize> = HashMap::with_capacity(snapshot.len());
    let mut groups: Vec<SampleGroup> = Vec::new();
    for sample in snapshot {
        let key = group_key(sample).to_string();
        let position = match index.get(&key) {
            Some(position) => *position,
            None => {
                index.insert(key.clone(), groups.len());
                groups.push(SampleGroup {
                    key,
                    timestamp: sample.timestamp,
                    rows: Vec::new(),
                });
                groups.len() - 1
            }
        };
        groups[position].rows.push(sample.clone());
        groups[position].timestamp = groups[position].timestamp.min(sample.timestamp);
    }
    groups
}

/// Order groups by `(timestamp, request identity)` and rows within a group by
/// sample id, so the partition is a function of the snapshot's contents rather
/// than of the order a caller handed them over.
fn order_groups(mut groups: Vec<SampleGroup>) -> Vec<SampleGroup> {
    groups.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.key.cmp(&right.key))
    });
    for group in &mut groups {
        group.rows.sort_by(|left, right| left.sample_id.cmp(&right.sample_id));
    }
    groups
}

/// Rows held by the trailing `count` groups.
fn trailing_rows(groups: &[SampleGroup], count: usize) -> usize {
    groups[groups.len().saturating_sub(count)..]
        .iter()
        .map(|group| group.rows.len())
        .sum()
}

/// How many trailing whole groups the holdout takes, refusing a split it cannot
/// make.
///
/// Both floors bind: the row floor keeps the holdout large enough to score, and
/// the independent request floor keeps it from being one request's rows dressed
/// up as a holdout. At least one whole group stays behind to train on.
fn holdout_group_count(
    groups: &[SampleGroup],
    total_rows: usize,
    config: &WarmupConfig,
) -> Result<usize, WarmupError> {
    if total_rows.saturating_sub(1) < config.min_holdout {
        return Err(WarmupError::DatasetTooSmallToHoldOut {
            samples: total_rows,
            min_holdout: config.min_holdout,
        });
    }
    let group_count = groups.len();
    if group_count < 2 || group_count < config.min_holdout_requests + 1 {
        return Err(WarmupError::TooFewRequestsToHoldOut {
            requests: group_count,
            min_requests: config.min_holdout_requests,
        });
    }

    let largest_holdout = group_count - 1;
    let proportional = (group_count as f64 * config.holdout_ratio).round() as usize;
    let mut holdout_groups = proportional
        .max(config.min_holdout_requests)
        .min(largest_holdout);
    while holdout_groups < largest_holdout
        && trailing_rows(groups, holdout_groups) < config.min_holdout
    {
        holdout_groups += 1;
    }
    if trailing_rows(groups, holdout_groups) < config.min_holdout {
        return Err(WarmupError::DatasetTooSmallToHoldOut {
            samples: total_rows,
            min_holdout: config.min_holdout,
        });
    }
    Ok(holdout_groups)
}

/// Distinct request identities over a set of canonical rows.
fn request_count(samples: &[OutcomeTrainingSample]) -> usize {
    samples.iter().map(group_key).collect::<HashSet<&str>>().len()
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Every per-row gate, applied to the canonical sample before it is projected.
fn validate_sample(index: usize, sample: &OutcomeTrainingSample) -> Result<(), WarmupError> {
    if sample.schema_version != FEATURE_SCHEMA_VERSION {
        return Err(WarmupError::SchemaMismatch {
            index,
            component: "sample",
            found: sample.schema_version,
            expected: FEATURE_SCHEMA_VERSION,
        });
    }
    if sample.features.schema_version != FEATURE_SCHEMA_VERSION {
        return Err(WarmupError::SchemaMismatch {
            index,
            component: "feature",
            found: sample.features.schema_version,
            expected: FEATURE_SCHEMA_VERSION,
        });
    }
    if sample.features.values.len() != FEATURE_DIMENSION {
        return Err(WarmupError::FeatureDimension {
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
        return Err(WarmupError::NonFiniteFeature {
            index,
            position,
            value: *value,
        });
    }
    if !sample.targets.success {
        for (dimension, value) in [
            ("latency_ms", sample.targets.latency_ms),
            ("ttft_ms", sample.targets.ttft_ms),
        ] {
            if let Some(value) = value {
                if value.is_finite() {
                    return Err(WarmupError::NonSuccessTiming {
                        index,
                        dimension,
                        value,
                    });
                }
            }
        }
    }
    validate_outcome_sample(sample).map_err(|reason| WarmupError::InvalidSample { index, reason })
}

/// Refuse a snapshot that holds one sample id twice.
///
/// The partition below is a split of one ordered list, so it is disjoint by
/// construction. It is disjoint by *identity* only if no id appears twice, and
/// a repeated id is exactly the case where a reader could no longer tell which
/// row the holdout scored.
fn reject_duplicate_ids(snapshot: &[OutcomeTrainingSample]) -> Result<(), WarmupError> {
    let mut seen: HashMap<&str, usize> = HashMap::with_capacity(snapshot.len());
    for (index, sample) in snapshot.iter().enumerate() {
        if let Some(first) = seen.insert(sample.sample_id.as_str(), index) {
            return Err(WarmupError::DuplicateSampleId {
                sample_id: sample.sample_id.clone(),
                first,
                second: index,
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Holdout description
// ---------------------------------------------------------------------------

/// The training rows' success rate, over a holdout that is never empty.
fn positive_rate(holdout: &[DatasetTrainingSample]) -> f64 {
    let successes = holdout
        .iter()
        .filter(|sample| sample.targets.success)
        .count();
    successes as f64 / holdout.len() as f64
}

/// Whether the holdout holds at least one row of each class.
fn holdout_has_both_classes(holdout: &[DatasetTrainingSample]) -> bool {
    let mut successes = 0usize;
    let mut non_successes = 0usize;
    for sample in holdout {
        if sample.targets.success {
            successes += 1;
        } else {
            non_successes += 1;
        }
    }
    successes > 0 && non_successes > 0
}

/// Per-dimension label coverage over the training rows.
fn coverage(training: &[DatasetTrainingSample]) -> LabelCoverage {
    let mut coverage = LabelCoverage {
        success: training.len(),
        ..LabelCoverage::default()
    };
    for sample in training {
        if sample.targets.latency_ms.is_some() {
            coverage.latency_ms += 1;
        }
        if sample.targets.ttft_ms.is_some() {
            coverage.ttft_ms += 1;
        }
        if sample.targets.cost.is_some() {
            coverage.cost += 1;
        }
        if !sample.feedback.is_empty() {
            coverage.feedback += 1;
        }
    }
    coverage
}

// ---------------------------------------------------------------------------
// Training
// ---------------------------------------------------------------------------

/// Train the chain and return its complete root-to-commit lineage.
///
/// The accepted seam returns the trained ensemble and the final commit and keeps
/// the ordered history to itself, so the chain is assembled one verified commit
/// at a time: each step trains a single row through the accepted predictor's own
/// fallible boundary, and the commit that comes back is re-loaded into a
/// predictor together with the lineage accumulated so far. That re-load is the
/// accepted lineage gate, and it is what makes the returned vector a verified
/// chain rather than a claim about one.
///
/// Starting from the cold genesis commit is what makes the parent honest: the
/// first trained commit names that genesis commit as its parent, so warmup
/// reports a parent it really has instead of presenting its artifact as a root it
/// did not start at.
///
/// The cost is one checkpoint load per row, which is the price of a complete
/// verified chain from a seam that returns only the head. Warmup is offline, so
/// that is the right place to pay it.
fn train_lineage(
    training: &[DatasetTrainingSample],
) -> Result<(Vec<ModelCommit>, ModelCommit), WarmupError> {
    let mut predictor = ModelEnsemblePredictor::genesis();
    let mut lineage = vec![predictor.commit_record()];

    for sample in training {
        let previous = lineage[lineage.len() - 1].commit_id.clone();
        let (_, commit) = predictor.try_train(std::slice::from_ref(sample))?;
        if commit.parent.as_ref() != Some(&previous) {
            return Err(WarmupError::BrokenLineage {
                reason: format!(
                    "commit '{}' does not name '{}' as its parent",
                    commit.commit_id, previous
                ),
            });
        }
        if !commit.verify() {
            return Err(WarmupError::BrokenLineage {
                reason: format!("commit '{}' failed verification", commit.commit_id),
            });
        }
        lineage.push(commit.clone());
        predictor = ModelEnsemblePredictor::from_model_commit_with_lineage(&commit, &lineage)
            .map_err(|error| WarmupError::ArtifactNotLoadable {
                reason: error.to_string(),
            })?;
    }

    let commit = lineage[lineage.len() - 1].clone();
    let expected_len = commit.learning_event_count as usize + 1;
    if lineage.len() != expected_len {
        return Err(WarmupError::BrokenLineage {
            reason: format!(
                "lineage of {} records does not match the commit's {} learned samples",
                lineage.len(),
                commit.learning_event_count
            ),
        });
    }
    if lineage[0].parent.is_some() {
        return Err(WarmupError::BrokenLineage {
            reason: "the chain does not start at a root commit".to_string(),
        });
    }
    if commit.parent.is_none() {
        return Err(WarmupError::BrokenLineage {
            reason: "a trained commit reported no parent".to_string(),
        });
    }
    Ok((lineage, commit))
}

// ---------------------------------------------------------------------------
// Contract attestation
// ---------------------------------------------------------------------------

/// Prove the warmed states load into the typed decision contract, and that the
/// contract scores a typed candidate finitely in all four dimensions.
///
/// This is the whole of the contract's participation here. It cannot train, and
/// a training row is not one of its types; what it can do is refuse a set of
/// states the decision surface would not accept, and refusing here means warmup
/// never reports an artifact it cannot hand on.
fn attest_contract(commit: &ModelCommit, ensemble: &ModelEnsemble) -> Result<(), WarmupError> {
    let states = DecisionModelStates {
        success: ensemble.success.save(),
        latency: ensemble.latency.save(),
        ttft: ensemble.ttft.save(),
        cost: ensemble.cost.save(),
    };
    let contract = DecisionModel::try_from_states(
        FEATURE_DIMENSION,
        FEATURE_SCHEMA_VERSION,
        commit.commit_id.clone(),
        &states,
    )
    .map_err(|reason| WarmupError::ContractNotLoadable {
        reason: reason.to_string(),
    })?;

    let mut probe = RoutingFeatures::default();
    probe.values[0] = 1.0;
    let candidate = DecisionCandidate::new(
        CandidateIdentity::new("warmup-probe", "warmup-probe"),
        CandidateEligibility::Eligible,
        probe,
    );
    let score = contract.try_score_candidate(&candidate).map_err(|reason| {
        WarmupError::ContractNotLoadable {
            reason: reason.to_string(),
        }
    })?;

    for dimension in DecisionDimension::ALL {
        let prediction = score.predictions.get(dimension);
        for value in [prediction.value, prediction.confidence] {
            if !value.is_finite() {
                return Err(WarmupError::ContractScoreNonFinite {
                    dimension: dimension.model_name(),
                    value,
                });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

/// Compare the warmed model against the cold baseline on one frozen holdout.
///
/// The rule is deliberately two-sided and deliberately strict. A lower log loss
/// with a higher Brier score means the model became better calibrated on average
/// while getting individual rows worse, and that is not an improvement a warmup
/// may claim. Every metric compared here is computed by the accepted evaluator
/// from the same holdout for both models, so the comparison contributes no free
/// parameters of its own.
///
/// A metric the evaluator declined to produce is reported as *not better* rather
/// than as an improvement. The evaluator only declines for an empty holdout,
/// which is refused earlier, so this branch is a fallback and not a path.
fn compare(baseline: &PredictionMetrics, warmed: &PredictionMetrics) -> (f64, f64, WarmupVerdict) {
    let (Some(baseline_log_loss), Some(warmed_log_loss)) = (baseline.log_loss, warmed.log_loss)
    else {
        return (0.0, 0.0, WarmupVerdict::NotBetter);
    };
    let (Some(baseline_brier), Some(warmed_brier)) = (baseline.brier_score, warmed.brier_score)
    else {
        return (0.0, 0.0, WarmupVerdict::NotBetter);
    };

    let log_loss_delta = warmed_log_loss - baseline_log_loss;
    let brier_delta = warmed_brier - baseline_brier;
    let verdict = WarmupVerdict::from_deltas(log_loss_delta, brier_delta);
    (log_loss_delta, brier_delta, verdict)
}
