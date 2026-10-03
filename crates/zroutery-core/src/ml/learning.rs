//! The training pipeline: dataset in, identified candidate model out.
//!
//! # What was missing
//!
//! A `train_batch`-shaped loop already exists in this module tree and it is not a
//! training system. It walks samples and calls `update`. It has no split, so
//! every sample it fits is also a sample it is judged on; no holdout, so nothing
//! can be measured out of sample; no dataset identity, so two runs cannot be
//! told apart; no validation, so there is nothing to select a checkpoint on; and
//! no record of what it did, so a trained model cannot be explained after the
//! fact. Its unit tests confirm it does what it says. That is a real capability
//! and it is not this.
//!
//! # What this module adds
//!
//! [`run_training`] takes a body of canonical samples and produces a
//! [`TrainingOutcome`]: an identified ensemble, the verified commit that pins
//! it, and a [`TrainingReport`] naming every input that produced it.
//!
//! Four properties are enforced rather than described:
//!
//! 1. **Group-disjoint, temporal splits.** Samples are grouped by request and
//!    whole groups are assigned, so one request's attempts can never appear in
//!    both the training and the holdout partition. A per-sample split leaks: the
//!    model sees attempt 0 of a request in training and is then asked about
//!    attempt 1 of the same request.
//! 2. **A genuinely frozen holdout.** The holdout is fitted on zero samples and
//!    is not consulted until the final checkpoint, so the number it produces is
//!    an out-of-sample number and not a restatement of the training loss.
//! 3. **Reproducible identity.** The same samples and the same config produce
//!    the same commit id. That is what makes "the model did not change" a
//!    checkable claim instead of an assumption.
//! 4. **A named provenance.** The report carries the dataset fingerprint, the
//!    feature schema version, the reward policy, the config identity and the
//!    feature coverage of each head, so a downstream promotion decision can
//!    check what it is being asked to promote.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::dataset::{OutcomeTrainingSample, TrainingSample};
use super::evaluation::PredictionMetrics;
use super::features::{FEATURE_SCHEMA_VERSION, UNKNOWN};
use super::model::{Prediction, RoutingModel};
use super::model_identity::{CommitId, ModelCheckpoint, ModelEnsemble};
use super::reward::RewardPolicy;
use super::shadow::ModelEnsemblePredictor;
use super::traces::DatasetFingerprint;

// ---------------------------------------------------------------------------
// FNV-1a — the same checksum every other identity in this module tree uses
// ---------------------------------------------------------------------------

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn hash_u64(hash: &mut u64, value: u64) {
    hash_bytes(hash, &value.to_le_bytes());
}

fn hash_str(hash: &mut u64, value: &str) {
    hash_bytes(hash, value.as_bytes());
    hash_bytes(hash, &[0]);
}

fn hash_f64(hash: &mut u64, value: f64) {
    hash_u64(hash, value.to_bits());
}

// ---------------------------------------------------------------------------
// LearningError
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum LearningError {
    #[error("training needs at least {needed} samples for the requested split, got {got}")]
    InsufficientSamples { needed: usize, got: usize },
    #[error("sample '{sample_id}' has feature schema {found}, this build trains on {expected}")]
    FeatureSchemaMismatch {
        sample_id: String,
        found: u32,
        expected: u32,
    },
    #[error(
        "sample '{sample_id}' has {count} feature position(s) outside [-1, 1] \
         (first at index {first_index}, value {first_value}); the extractor clamps every \
         populated feature into that range, so an out-of-range vector came from somewhere \
         other than this router and would diverge the heads rather than inform them"
    )]
    FeatureOutOfRange {
        sample_id: String,
        count: usize,
        first_index: usize,
        first_value: f32,
    },
    #[error("training split is empty after grouping by request: {0}")]
    EmptySplit(String),
    #[error("the underlying model rejected the training run: {0}")]
    Model(String),
    #[error("split ratios must be in (0, 1) and sum to at most 1: train={train}, validation={validation}, holdout={holdout}")]
    InvalidRatios {
        train: f64,
        validation: f64,
        holdout: f64,
    },
}

// ---------------------------------------------------------------------------
// Feature coverage
// ---------------------------------------------------------------------------

/// How much of the feature vector a body of samples actually populates.
///
/// Recorded because an absent feature is not a neutral value: it enters the
/// model as a distinct constant, and a head that has only ever seen one value
/// will predict that value forever while still reporting a confidence. A
/// training report that does not say which positions were dead is a report that
/// cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureCoverage {
    /// Number of positions that vary across the samples.
    pub observed: usize,
    /// Number of positions that never leave [`UNKNOWN`].
    pub unknown: usize,
    /// Number of positions that vary but never leave [`UNKNOWN`].
    pub constant: usize,
    /// The whole vector width, for a completeness check.
    pub dimension: usize,
}

impl FeatureCoverage {
    /// Measure coverage over a body of samples.
    pub fn measure(samples: &[OutcomeTrainingSample]) -> Self {
        let dimension = super::features::FEATURE_DIMENSION;
        // `varying[i]` records whether position `i` ever took a known value
        // other than the first known value seen at that position.
        let mut varying = vec![false; dimension];
        let mut unknown = vec![true; dimension];
        let mut reference = [UNKNOWN; super::features::FEATURE_DIMENSION];

        for sample in samples {
            for (index, value) in sample.features.values.iter().enumerate() {
                if *value == UNKNOWN {
                    continue;
                }
                if unknown[index] {
                    unknown[index] = false;
                    reference[index] = *value;
                } else if *value != reference[index] {
                    varying[index] = true;
                }
            }
        }

        let unknown_count = unknown.iter().filter(|still| **still).count();
        let observed = varying.iter().filter(|moved| **moved).count();
        Self {
            observed,
            unknown: unknown_count,
            constant: dimension.saturating_sub(unknown_count + observed),
            dimension,
        }
    }
}

// ---------------------------------------------------------------------------
// TrainingConfig
// ---------------------------------------------------------------------------

/// Everything that changes what a training run produces.
///
/// Every field participates in [`TrainingConfig::identity`], so two runs that
/// differ in any of them have different identities and cannot be compared as
/// though they were the same experiment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainingConfig {
    /// Name the trained model is committed under.
    pub model_id: String,
    /// Shared seed. Reserved for the sampling decisions a future sharded or
    /// stochastic trainer would need; recorded now so the identity is stable
    /// across the change.
    pub seed: u64,
    /// Fraction of sample groups assigned to the training partition.
    pub train_ratio: f64,
    /// Fraction assigned to the validation partition.
    pub validation_ratio: f64,
    /// Fraction assigned to the frozen holdout. Whatever the ratios leave over
    /// is also assigned to the holdout, so the three partitions are exhaustive
    /// and no sample is silently dropped.
    pub holdout_ratio: f64,
    /// Full passes over the training partition. Each pass re-applies the whole
    /// training set in timestamp order.
    pub passes: u32,
    /// The utility weights the resulting model will be scored under. Recorded
    /// with the model because a predictor is only meaningful relative to the
    /// utility it is meant to maximise.
    pub reward_policy: RewardPolicy,
}

impl Default for TrainingConfig {
    fn default() -> Self {
        Self {
            model_id: "shadow".to_string(),
            seed: TRAINING_DEFAULT_SEED,
            train_ratio: 0.70,
            validation_ratio: 0.15,
            holdout_ratio: 0.15,
            passes: 3,
            reward_policy: RewardPolicy::default(),
        }
    }
}

/// The seed a run uses unless a caller names another.
pub const TRAINING_DEFAULT_SEED: u64 = 0x5eed_0000_0000_0001;

impl TrainingConfig {
    /// Validate the ratios before any data is touched.
    pub fn validate(&self) -> Result<(), LearningError> {
        let parts = [
            ("train", self.train_ratio),
            ("validation", self.validation_ratio),
            ("holdout", self.holdout_ratio),
        ];
        let finite = parts
            .iter()
            .all(|(_, ratio)| ratio.is_finite() && *ratio > 0.0);
        let total: f64 = parts.iter().map(|(_, ratio)| *ratio).sum();
        if !finite || total > 1.0 {
            return Err(LearningError::InvalidRatios {
                train: self.train_ratio,
                validation: self.validation_ratio,
                holdout: self.holdout_ratio,
            });
        }
        if self.passes == 0 {
            return Err(LearningError::InvalidRatios {
                train: self.train_ratio,
                validation: self.validation_ratio,
                holdout: self.holdout_ratio,
            });
        }
        Ok(())
    }

    /// A content-addressed identity for this configuration.
    pub fn identity(&self) -> String {
        let mut hash = FNV_OFFSET_BASIS;
        hash_str(&mut hash, "zroutery-training-config-v1");
        hash_str(&mut hash, &self.model_id);
        hash_u64(&mut hash, self.seed);
        hash_f64(&mut hash, self.train_ratio);
        hash_f64(&mut hash, self.validation_ratio);
        hash_f64(&mut hash, self.holdout_ratio);
        hash_u64(&mut hash, u64::from(self.passes));
        hash_f64(&mut hash, self.reward_policy.success_weight);
        hash_f64(&mut hash, self.reward_policy.latency_weight);
        hash_f64(&mut hash, self.reward_policy.cost_weight);
        hash_f64(&mut hash, self.reward_policy.fallback_penalty);
        hash_f64(&mut hash, self.reward_policy.switch_cost);
        hash_f64(&mut hash, self.reward_policy.uncertainty_weight);
        hash_u64(&mut hash, u64::from(FEATURE_SCHEMA_VERSION));
        format!("{:016x}", hash)
    }
}

// ---------------------------------------------------------------------------
// Split
// ---------------------------------------------------------------------------

/// The three partitions a training body was divided into.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Split {
    pub train: Vec<OutcomeTrainingSample>,
    pub validation: Vec<OutcomeTrainingSample>,
    /// Never fitted on. The only partition a final checkpoint may be judged on.
    pub holdout: Vec<OutcomeTrainingSample>,
}

impl Split {
    pub fn train_len(&self) -> usize {
        self.train.len()
    }
    pub fn validation_len(&self) -> usize {
        self.validation.len()
    }
    pub fn holdout_len(&self) -> usize {
        self.holdout.len()
    }
    pub fn total(&self) -> usize {
        self.train.len() + self.validation.len() + self.holdout.len()
    }
}

/// Partition a body of samples into train / validation / holdout.
///
/// Samples are grouped by `request_id` and whole groups are placed, oldest
/// group first, so a request's attempts never straddle a boundary. Grouping is
/// what makes this safe; sorting by timestamp alone does not, because a
/// failover chain emits several samples within the same second.
///
/// The split is a pure function of the body: no clock, no RNG, no hash-map
/// iteration order. The same input always yields the same three partitions,
/// which is what makes a holdout hold.
pub fn split_samples(
    samples: &[OutcomeTrainingSample],
    config: &TrainingConfig,
) -> Result<Split, LearningError> {
    config.validate()?;

    let mut groups: BTreeMap<&str, Vec<OutcomeTrainingSample>> = BTreeMap::new();
    for sample in samples {
        if sample.features.schema_version != FEATURE_SCHEMA_VERSION {
            return Err(LearningError::FeatureSchemaMismatch {
                sample_id: sample.sample_id.clone(),
                found: sample.features.schema_version,
                expected: FEATURE_SCHEMA_VERSION,
            });
        }
        check_feature_range(sample)?;
        groups
            .entry(sample.request_id.as_str())
            .or_default()
            .push(sample.clone());
    }
    if groups.is_empty() {
        return Err(LearningError::EmptySplit(
            "no request groups were formed".to_string(),
        ));
    }

    // Oldest group first; the request id breaks ties so the order is total even
    // when several requests share a timestamp.
    let mut ordered: Vec<(i64, String, Vec<OutcomeTrainingSample>)> = groups
        .into_values()
        .map(|mut group| {
            group.sort_by(|a, b| {
                a.timestamp
                    .cmp(&b.timestamp)
                    .then_with(|| a.sample_id.cmp(&b.sample_id))
            });
            let oldest = group.first().map(|sample| sample.timestamp).unwrap_or(0);
            let request_id = group
                .first()
                .map(|sample| sample.request_id.clone())
                .unwrap_or_default();
            (oldest, request_id, group)
        })
        .collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let total = ordered.len();
    let train_end = ((total as f64) * config.train_ratio).round() as usize;
    let validation_end =
        ((total as f64) * (config.train_ratio + config.validation_ratio)).round() as usize;

    let mut split = Split::default();
    for (index, (_, _, group)) in ordered.into_iter().enumerate() {
        if index < train_end {
            split.train.extend(group);
        } else if index < validation_end {
            split.validation.extend(group);
        } else {
            split.holdout.extend(group);
        }
    }

    if split.train.is_empty() {
        return Err(LearningError::EmptySplit(
            "the training partition received no request group".to_string(),
        ));
    }
    Ok(split)
}

// ---------------------------------------------------------------------------
// Per-pass training metrics
// ---------------------------------------------------------------------------

/// What one pass over the training partition produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PassReport {
    /// One-based pass number.
    pub pass: u32,
    /// Cross-entropy of the success head over the training partition, measured
    /// before this pass was applied and after it, so the direction of movement
    /// is visible even when the optimiser has overshot.
    pub train_loss_before: f64,
    pub train_loss_after: f64,
    /// The same measure over the validation partition, after the pass.
    pub validation_loss: f64,
    /// Full model metrics over the validation partition after the pass.
    pub validation: PredictionMetrics,
    /// The commit this pass produced.
    pub commit_id: String,
}

// ---------------------------------------------------------------------------
// TrainingReport
// ---------------------------------------------------------------------------

/// Everything needed to say what a trained model is and where it came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingReport {
    /// Total samples handed in, across every partition.
    pub sample_count: usize,
    /// Distinct requests those samples belong to.
    pub request_count: usize,
    pub train_size: usize,
    pub validation_size: usize,
    pub holdout_size: usize,
    /// Number of request groups in each partition. Reported beside the sample
    /// counts because two partitions with the same size can hold very
    /// different numbers of independent observations.
    pub train_groups: usize,
    pub validation_groups: usize,
    pub holdout_groups: usize,
    /// One entry per pass.
    pub passes: Vec<PassReport>,
    /// Final cross-entropy on the frozen holdout. The headline number.
    pub holdout_loss: f64,
    /// Full model metrics on the frozen holdout.
    pub holdout: PredictionMetrics,
    /// The verified commit the returned ensemble is pinned to.
    pub final_commit: String,
    /// The commit the run started from.
    pub base_commit: String,
    /// Number of learning events the final commit records.
    pub learning_event_count: u64,
    /// Identity of the exact body of samples this run fitted on.
    pub dataset_fingerprint: DatasetFingerprint,
    /// Identity of the training configuration.
    pub config_identity: String,
    /// The feature schema the samples and the model must agree on.
    pub feature_schema_version: u32,
    /// The utility weights the model will be scored under.
    pub reward_policy: RewardPolicy,
    /// Per-head feature coverage of the training partition.
    pub coverage: FeatureCoverage,
    /// Unix seconds when the report was produced. Recorded, and deliberately
    /// excluded from both the dataset and the commit identity.
    pub produced_at: i64,
}

// ---------------------------------------------------------------------------
// TrainingOutcome
// ---------------------------------------------------------------------------

/// A trained ensemble plus everything known about how it was produced.
pub struct TrainingOutcome {
    /// The name the run committed the model under.
    pub model_id: String,
    pub ensemble: ModelEnsemble,
    pub commit_id: CommitId,
    pub checkpoint: ModelCheckpoint,
    pub report: TrainingReport,
}

impl TrainingOutcome {
    /// The commit record pinning this ensemble, with the run's model name.
    ///
    /// Built as a genesis-rooted record: the training pipeline refits from the
    /// cold root every run, so the resulting commit genuinely has no parent and
    /// claiming otherwise would be a fabricated lineage.
    pub fn commit_record(&self) -> super::model_identity::ModelCommit {
        super::model_identity::ModelCommit::new(
            super::model_identity::ModelId::new(self.model_id.clone()),
            self.checkpoint.clone(),
            None,
            self.report.learning_event_count,
        )
    }

    /// A predictor pinned to this outcome's verified commit.
    pub fn predictor(&self) -> ModelEnsemblePredictor {
        let commit = self.commit_record();
        ModelEnsemblePredictor::from_verified_commit_with_lineage(
            &commit,
            std::slice::from_ref(&commit),
        )
        .expect("a checkpoint produced by run_training must load and verify")
    }
}

// ---------------------------------------------------------------------------
// Feature range guard
// ---------------------------------------------------------------------------

/// Refuse a sample whose feature vector leaves the range the extractor promises.
///
/// [`super::features::extract_features`] clamps every populated feature into
/// `[-1, 1]` and represents a missing one as [`UNKNOWN`], which is itself `-1.0`.
/// A vector outside that range therefore did not come from this router: it came
/// from an import, a hand-built fixture, or a corrupted record.
///
/// This is not pedantry. The success head is an unbounded online logistic
/// regression, and on linearly separable data with an out-of-range feature its
/// weights walk to infinity, at which point the checkpoint stops verifying and
/// the entire run is lost — a poisoned sample takes the training run down with
/// it instead of being one bad row. Refusing the row costs the row.
fn check_feature_range(sample: &OutcomeTrainingSample) -> Result<(), LearningError> {
    let mut count = 0usize;
    let mut first_index = 0usize;
    let mut first_value = 0.0f32;
    for (index, value) in sample.features.values.iter().enumerate() {
        if !value.is_finite() || *value < -1.0 || *value > 1.0 {
            if count == 0 {
                first_index = index;
                first_value = *value;
            }
            count += 1;
        }
    }
    if count == 0 {
        return Ok(());
    }
    Err(LearningError::FeatureOutOfRange {
        sample_id: sample.sample_id.clone(),
        count,
        first_index,
        first_value,
    })
}

// ---------------------------------------------------------------------------
// run_training
// ---------------------------------------------------------------------------

/// Train an identified candidate model from a body of canonical samples.
///
/// The returned ensemble is fitted on the training partition only. The
/// validation partition chooses nothing and is reported; the holdout is
/// untouched until the final report.
pub fn run_training(
    samples: &[OutcomeTrainingSample],
    config: &TrainingConfig,
) -> Result<TrainingOutcome, LearningError> {
    let split = split_samples(samples, config)?;
    let request_count = {
        let mut ids: BTreeSet<&str> = BTreeSet::new();
        for sample in samples {
            ids.insert(sample.request_id.as_str());
        }
        ids.len()
    };
    let dataset_fingerprint = DatasetFingerprint::of(&split.train);
    let coverage = FeatureCoverage::measure(&split.train);

    let train_legacy: Vec<TrainingSample> = split
        .train
        .iter()
        .cloned()
        .map(TrainingSample::from)
        .collect();
    let validation_legacy: Vec<TrainingSample> = split
        .validation
        .iter()
        .cloned()
        .map(TrainingSample::from)
        .collect();
    let holdout_legacy: Vec<TrainingSample> = split
        .holdout
        .iter()
        .cloned()
        .map(TrainingSample::from)
        .collect();

    let base = ModelEnsemblePredictor::genesis();
    let base_commit = base.commit_record().commit_id.clone().to_string();

    // One verified training call over the whole schedule.
    //
    // The samples are concatenated `passes` times rather than trained pass by
    // pass. A `try_train` on the genesis predictor is idempotent — the predictor
    // owns the history, so calling it repeatedly with the same slice applies
    // that slice once each time and returns the same ensemble. The schedule has
    // to live in the payload, or `passes` is a number in a config that changes
    // nothing.
    let mut schedule: Vec<TrainingSample> =
        Vec::with_capacity(train_legacy.len() * config.passes as usize);
    for _ in 0..config.passes {
        schedule.extend_from_slice(&train_legacy);
    }

    let (ensemble, commit) = base
        .try_train(&schedule)
        .map_err(|error| LearningError::Model(error.to_string()))?;
    let commit_id = commit.commit_id.clone();

    // Per-pass metrics are produced by replaying the same arithmetic one pass at
    // a time. That replay is only meaningful if it lands on the same weights
    // the verified call did, so that is checked rather than assumed.
    let mut replay = ModelEnsemble::new();
    let mut passes = Vec::with_capacity(config.passes as usize);
    let mut offset = 0usize;
    for pass in 1..=config.passes {
        let train_loss_before = success_log_loss(&replay.success, &train_legacy);
        for sample in &schedule[offset..offset + train_legacy.len()] {
            replay.update_all(sample);
        }
        offset += train_legacy.len();
        passes.push(PassReport {
            pass,
            train_loss_before,
            train_loss_after: success_log_loss(&replay.success, &train_legacy),
            validation_loss: success_log_loss(&replay.success, &validation_legacy),
            validation: evaluate_success(&replay.success, &validation_legacy),
            commit_id: {
                let pass_commit = super::model_identity::ModelCommit::new(
                    super::model_identity::ModelId::new(config.model_id.clone()),
                    replay.save_all(),
                    None,
                    (pass as u64) * (train_legacy.len() as u64),
                );
                if !pass_commit.verify() {
                    return Err(LearningError::Model(format!(
                        "pass {pass} did not produce a verifiable commit"
                    )));
                }
                pass_commit.commit_id.to_string()
            },
        });
    }
    if replay.save_all().content_hash() != ensemble.save_all().content_hash() {
        return Err(LearningError::Model(
            "the per-pass replay diverged from the verified training result".to_string(),
        ));
    }

    let checkpoint = ensemble.save_all();
    let holdout = evaluate_success(&ensemble.success, &holdout_legacy);

    let report = TrainingReport {
        sample_count: samples.len(),
        request_count,
        train_size: split.train_len(),
        validation_size: split.validation_len(),
        holdout_size: split.holdout_len(),
        train_groups: distinct_requests(&split.train),
        validation_groups: distinct_requests(&split.validation),
        holdout_groups: distinct_requests(&split.holdout),
        passes,
        holdout_loss: success_log_loss(&ensemble.success, &holdout_legacy),
        holdout,
        final_commit: commit_id.to_string(),
        base_commit,
        learning_event_count: config.passes as u64 * train_legacy.len() as u64,
        dataset_fingerprint,
        config_identity: config.identity(),
        feature_schema_version: FEATURE_SCHEMA_VERSION,
        reward_policy: config.reward_policy.clone(),
        coverage,
        produced_at: now_seconds(),
    };

    Ok(TrainingOutcome {
        model_id: config.model_id.clone(),
        ensemble,
        commit_id,
        checkpoint,
        report,
    })
}

fn distinct_requests(samples: &[OutcomeTrainingSample]) -> usize {
    let mut ids: BTreeSet<&str> = BTreeSet::new();
    for sample in samples {
        ids.insert(sample.request_id.as_str());
    }
    ids.len()
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Head evaluation
// ---------------------------------------------------------------------------

/// Evaluate a success head over a body of legacy samples.
pub fn evaluate_success(model: &dyn RoutingModel, samples: &[TrainingSample]) -> PredictionMetrics {
    if samples.is_empty() {
        return PredictionMetrics::default();
    }
    let mut predictions = Vec::with_capacity(samples.len());
    let mut actuals = Vec::with_capacity(samples.len());
    for sample in samples {
        let prediction = model.predict(&sample.features);
        predictions.push(prediction.value);
        actuals.push(u64::from(sample.targets.success));
    }
    prediction_metrics(&predictions, &actuals)
}

/// Cross-entropy of a success head, the training objective the heads share.
///
/// Only the success head is scored as a loss. The latency, TTFT and cost heads
/// are squared-error regressors whose loss is already reflected in their
/// reported MAE and RMSE, and inventing a second number for them here would be
/// one more thing to keep in sync for no reader.
pub fn success_log_loss(model: &dyn RoutingModel, samples: &[TrainingSample]) -> f64 {
    if samples.is_empty() {
        return f64::NAN;
    }
    let mut total = 0.0;
    for sample in samples {
        let prediction = model.predict(&sample.features);
        let clamped = prediction.value.clamp(1e-9, 1.0 - 1e-9);
        let actual = if sample.targets.success { 1.0 } else { 0.0 };
        total -= actual * clamped.ln() + (1.0 - actual) * (1.0 - clamped).ln();
    }
    total / samples.len() as f64
}

fn prediction_metrics(predictions: &[f64], actuals: &[u64]) -> PredictionMetrics {
    let n = predictions.len();
    if n == 0 {
        return PredictionMetrics::default();
    }
    let mut log_loss = 0.0;
    let mut brier = 0.0;
    let mut sum_prediction = 0.0;
    for (prediction, actual) in predictions.iter().zip(actuals) {
        let clamped = prediction.clamp(1e-9, 1.0 - 1e-9);
        let target = *actual as f64;
        log_loss -= target * clamped.ln() + (1.0 - target) * (1.0 - clamped).ln();
        let error = clamped - target;
        brier += error * error;
        sum_prediction += clamped;
    }
    let n_f = n as f64;
    PredictionMetrics {
        sample_count: n,
        log_loss: Some(log_loss / n_f),
        brier_score: Some(brier / n_f),
        mae: None,
        rmse: None,
        mean_prediction: sum_prediction / n_f,
        mean_actual: actuals.iter().map(|value| *value as f64).sum::<f64>() / n_f,
    }
}

/// Predict one candidate with a trained ensemble.
///
/// Thin, named, and shared: the serving path, the comparison and the shadow
/// analysis must all predict through one function or they will drift into
/// measuring different models.
pub fn predict_bundle(
    ensemble: &ModelEnsemble,
    candidate_model: &str,
    candidate_provider: &str,
    features: &super::features::RoutingFeatures,
) -> super::reward::PredictionBundle {
    let predict = |model: &dyn RoutingModel| model.predict(features);
    super::reward::PredictionBundle {
        candidate_model: candidate_model.to_string(),
        candidate_provider: candidate_provider.to_string(),
        success: predict(&ensemble.success),
        latency: predict(&ensemble.latency),
        ttft: predict(&ensemble.ttft),
        cost: predict(&ensemble.cost),
    }
}

/// Whether a prediction is usable for a decision.
///
/// Non-finite values never reach the decision logic; this is the single place
/// that says so, so a caller does not have to re-derive it.
pub fn prediction_is_finite(prediction: &Prediction) -> bool {
    prediction.value.is_finite() && prediction.confidence.is_finite()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::DataOrigin;
    use crate::ml::dataset::{SampleScope, Targets};
    use crate::ml::features::{RoutingFeatures, FEATURE_DIMENSION};
    use crate::outcome::{FinalStatus, OutcomeIdentity};

    /// A body of samples across `requests` requests and `providers` providers.
    ///
    /// Feature magnitudes stay inside the `[-1, 1]` range
    /// [`crate::ml::features::extract_features`] promises. That is not
    /// cosmetic: the heads are unbounded online regressions, and a fixture
    /// outside the range the extractor guarantees diverges them instead of
    /// training them.
    ///
    /// Provider `p` succeeds when `(request + p) % 2 == 0`, encoded in the
    /// features as a two-bit pattern, so the relationship is genuinely learnable
    /// and genuinely checkable.
    fn body(requests: usize, providers: usize) -> Vec<OutcomeTrainingSample> {
        let mut samples = Vec::new();
        for request in 0..requests {
            for provider in 0..providers {
                let success = (request + provider) % 2 == 0;
                let mut values = [UNKNOWN; FEATURE_DIMENSION];
                values[0] = provider as f32 / providers as f32;
                values[1] = if success { 0.5 } else { -0.5 };
                values[2] = ((request % 7) as f32 / 7.0) - 0.5;
                samples.push(OutcomeTrainingSample {
                    sample_id: format!("s-{request}-{provider}"),
                    schema_version: 1,
                    timestamp: 1_700_000_000 + (request * providers + provider) as i64,
                    streaming: false,
                    dialect: "anthropic".to_string(),
                    features: RoutingFeatures {
                        schema_version: FEATURE_SCHEMA_VERSION,
                        values,
                    },
                    targets: Targets {
                        success,
                        latency_ms: Some(100.0 + provider as f64 * 10.0),
                        ttft_ms: Some(20.0),
                        cost: Some(0.001 * (provider + 1) as f64),
                        failure_class: None,
                        fallback_count: 0,
                    },
                    provider_id: format!("p{provider}"),
                    model_id: format!("m{provider}"),
                    origin: DataOrigin::Native,
                    outcome_id: format!("out-{request}"),
                    request_id: format!("r{request}"),
                    decision_id: Some(format!("d{request}")),
                    response_id: None,
                    final_status: if success {
                        FinalStatus::Success
                    } else {
                        FinalStatus::Failed
                    },
                    success,
                    identity: OutcomeIdentity::default(),
                    scope: SampleScope::Attempt {
                        index: 0,
                        attempt_id: format!("a-{request}-{provider}"),
                    },
                    attempt_id: Some(format!("a-{request}-{provider}")),
                    rectified: false,
                    attempts: Vec::new(),
                    usage: None,
                    estimated_cost: None,
                    actual_cost: None,
                    terminal_error: None,
                    feedback: None,
                });
            }
        }
        samples
    }

    #[test]
    fn split_is_group_disjoint() {
        let samples = body(20, 2);
        let split = split_samples(&samples, &TrainingConfig::default()).expect("split");

        // Partition-level disjointness, which is the invariant that matters: a
        // request's several attempt samples may all sit in one partition, and
        // must not appear in two.
        let mut owner: BTreeMap<String, &str> = BTreeMap::new();
        for (name, part) in [
            ("train", &split.train),
            ("validation", &split.validation),
            ("holdout", &split.holdout),
        ] {
            for sample in part {
                match owner.get(&sample.request_id) {
                    Some(previous) => assert_eq!(
                        *previous, name,
                        "request {} landed in both {previous} and {name}",
                        sample.request_id
                    ),
                    None => {
                        owner.insert(sample.request_id.clone(), name);
                    }
                }
            }
        }
        assert_eq!(owner.len(), 20);
    }

    #[test]
    fn an_out_of_range_feature_is_refused_rather_than_trained() {
        let mut samples = body(20, 2);
        samples[3].features.values[7] = 4_096.0;
        assert!(matches!(
            split_samples(&samples, &TrainingConfig::default()),
            Err(LearningError::FeatureOutOfRange { first_index: 7, .. })
        ));
    }

    #[test]
    fn a_non_finite_feature_is_refused() {
        let mut samples = body(20, 2);
        samples[0].features.values[2] = f32::NAN;
        assert!(matches!(
            split_samples(&samples, &TrainingConfig::default()),
            Err(LearningError::FeatureOutOfRange { .. })
        ));
    }

    #[test]
    fn split_is_exhaustive() {
        let samples = body(13, 3);
        let split = split_samples(&samples, &TrainingConfig::default()).expect("split");
        assert_eq!(split.total(), samples.len());
    }

    #[test]
    fn split_is_deterministic() {
        let samples = body(17, 2);
        let config = TrainingConfig::default();
        let first = split_samples(&samples, &config).expect("split");
        let second = split_samples(&samples, &config).expect("split");
        assert_eq!(
            first
                .train
                .iter()
                .map(|s| s.sample_id.clone())
                .collect::<Vec<_>>(),
            second
                .train
                .iter()
                .map(|s| s.sample_id.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn split_is_temporal() {
        let samples = body(10, 1);
        let split = split_samples(&samples, &TrainingConfig::default()).expect("split");
        let newest_train = split.train.iter().map(|s| s.timestamp).max().unwrap();
        let oldest_holdout = split.holdout.iter().map(|s| s.timestamp).min().unwrap();
        assert!(
            newest_train <= oldest_holdout,
            "train reached {} while holdout began at {}",
            newest_train,
            oldest_holdout
        );
    }

    #[test]
    fn a_foreign_feature_schema_is_refused() {
        let mut samples = body(4, 2);
        samples[0].features.schema_version = FEATURE_SCHEMA_VERSION + 1;
        assert!(matches!(
            split_samples(&samples, &TrainingConfig::default()),
            Err(LearningError::FeatureSchemaMismatch { .. })
        ));
    }

    #[test]
    fn ratios_that_do_not_fit_are_refused() {
        let config = TrainingConfig {
            train_ratio: 0.9,
            validation_ratio: 0.9,
            ..TrainingConfig::default()
        };
        assert!(matches!(
            split_samples(&body(4, 2), &config),
            Err(LearningError::InvalidRatios { .. })
        ));
    }

    #[test]
    fn training_is_reproducible_from_the_same_body() {
        let samples = body(30, 3);
        let config = TrainingConfig::default();
        let first = run_training(&samples, &config).expect("train");
        let second = run_training(&samples, &config).expect("train");
        assert_eq!(first.commit_id, second.commit_id);
        assert_eq!(
            first.report.dataset_fingerprint,
            second.report.dataset_fingerprint
        );
        assert_eq!(
            first.checkpoint.content_hash(),
            second.checkpoint.content_hash()
        );
    }

    #[test]
    fn a_different_body_produces_a_different_identity() {
        let config = TrainingConfig::default();
        let first = run_training(&body(30, 3), &config).expect("train");
        let second = run_training(&body(30, 3), &config).expect("train");
        let mut changed = body(30, 3);
        changed[0].targets.success = !changed[0].targets.success;
        changed[0].success = !changed[0].success;
        let third = run_training(&changed, &config).expect("train");
        assert_ne!(
            first.report.dataset_fingerprint,
            third.report.dataset_fingerprint
        );
        assert_ne!(second.report.final_commit, third.report.final_commit);
    }

    #[test]
    fn a_different_config_produces_a_different_identity() {
        let samples = body(30, 3);
        let first = run_training(&samples, &TrainingConfig::default()).expect("train");
        let five_passes = run_training(
            &samples,
            &TrainingConfig {
                passes: 5,
                ..TrainingConfig::default()
            },
        )
        .expect("train");
        assert_ne!(
            first.report.config_identity,
            five_passes.report.config_identity
        );
        // The two runs are genuinely different experiments: they record
        // different numbers of learning events and, at this volume, different
        // weights. Asserting a different *commit* would over-claim: a saturating
        // head can legitimately converge, and two schedules that reach the same
        // fixed point are the same model honestly labelled.
        assert_ne!(
            first.report.learning_event_count,
            five_passes.report.learning_event_count
        );
    }

    #[test]
    fn more_passes_actually_change_the_model() {
        // The regression this protects: a schedule that repeats `try_train` on a
        // genesis predictor instead of extending the payload applies the training
        // set once no matter what `passes` says, so every pass count produced the
        // same commit.
        let samples = body(60, 3);
        let one = run_training(
            &samples,
            &TrainingConfig {
                passes: 1,
                ..TrainingConfig::default()
            },
        )
        .expect("train");
        let five = run_training(
            &samples,
            &TrainingConfig {
                passes: 5,
                ..TrainingConfig::default()
            },
        )
        .expect("train");
        assert_ne!(one.commit_id, five.commit_id);
        assert_ne!(
            one.checkpoint.content_hash(),
            five.checkpoint.content_hash()
        );
    }

    #[test]
    fn the_holdout_is_never_fitted() {
        let samples = body(40, 3);
        let config = TrainingConfig::default();
        let split = split_samples(&samples, &config).expect("split");
        let holdout_ids: BTreeSet<String> = split
            .holdout
            .iter()
            .map(|sample| sample.request_id.clone())
            .collect();
        assert!(!holdout_ids.is_empty());
        // Every commit the run produced reports a learning event count equal to
        // the passes times the *training* partition size, never the whole body.
        let outcome = run_training(&samples, &config).expect("train");
        assert_eq!(
            outcome.report.learning_event_count,
            u64::from(config.passes) * split.train_len() as u64
        );
        assert_ne!(
            outcome.report.learning_event_count,
            u64::from(config.passes) * samples.len() as u64
        );
    }

    #[test]
    fn the_report_names_every_input() {
        let samples = body(40, 3);
        let config = TrainingConfig::default();
        let outcome = run_training(&samples, &config).expect("train");
        let report = &outcome.report;
        assert_eq!(report.sample_count, samples.len());
        assert_eq!(report.request_count, 40);
        assert_eq!(
            report.train_size + report.validation_size + report.holdout_size,
            40 * 3
        );
        assert_eq!(report.passes.len(), config.passes as usize);
        assert_eq!(report.feature_schema_version, FEATURE_SCHEMA_VERSION);
        assert!(!report.final_commit.is_empty());
        assert!(!report.base_commit.is_empty());
        assert!(!report.dataset_fingerprint.as_str().is_empty());
        assert!(!report.config_identity.is_empty());
        assert!(report.holdout_loss.is_finite());
        assert!(report.coverage.observed > 0);
    }

    #[test]
    fn a_learnable_body_produces_a_smaller_holdout_loss_than_a_coin_flip() {
        // log(2) is the loss of a model that has learned nothing and predicts
        // the base rate. Learning the relationship must beat it.
        let samples = body(120, 4);
        let outcome = run_training(&samples, &TrainingConfig::default()).expect("train");
        assert!(
            outcome.report.holdout_loss < std::f64::consts::LN_2,
            "holdout loss {} did not beat an uninformed model",
            outcome.report.holdout_loss
        );
    }

    #[test]
    fn coverage_reports_dead_positions() {
        let samples = body(6, 2);
        let coverage = FeatureCoverage::measure(&samples);
        assert_eq!(coverage.dimension, FEATURE_DIMENSION);
        // Positions 0..3 carry the relationship; the rest are UNKNOWN.
        assert_eq!(coverage.observed, 3);
        assert_eq!(coverage.unknown, FEATURE_DIMENSION - 3);
        assert_eq!(coverage.constant, 0);
        assert_eq!(
            coverage.observed + coverage.unknown + coverage.constant,
            FEATURE_DIMENSION
        );
    }

    #[test]
    fn coverage_separates_constant_from_never_seen() {
        // A position that is always the same known value is constant evidence,
        // not missing evidence. Collapsing the two would hide a feature the model
        // has learned to ignore for a reason nobody recorded.
        let mut samples = body(6, 2);
        for sample in &mut samples {
            sample.features.values[9] = 0.25;
        }
        let coverage = FeatureCoverage::measure(&samples);
        assert_eq!(coverage.constant, 1);
        assert_eq!(coverage.unknown, FEATURE_DIMENSION - 4);
    }

    #[test]
    fn config_identity_is_stable_and_sensitive() {
        let base = TrainingConfig::default();
        assert_eq!(base.identity(), base.clone().identity());
        assert_ne!(
            base.identity(),
            TrainingConfig {
                seed: 7,
                ..base.clone()
            }
            .identity()
        );
        assert_ne!(
            base.identity(),
            TrainingConfig {
                reward_policy: RewardPolicy {
                    success_weight: 2.0,
                    ..RewardPolicy::default()
                },
                ..base.clone()
            }
            .identity()
        );
    }
}
