use std::collections::HashSet;
use std::sync::Arc;

use crate::ml::dataset::TrainingSample as DatasetTrainingSample;
use crate::ml::features::{RoutingFeatures, FEATURE_SCHEMA_VERSION};
use crate::ml::model::RoutingModel;
use crate::ml::model_identity::{
    validate_replay_sample, CommitId, ModelCheckpoint, ModelCommit, ModelEnsemble, ModelId,
    ReplayError,
};
use crate::ml::reward::PredictionBundle;

// ---------------------------------------------------------------------------
// EnsemblePredictor — prediction seam
// ---------------------------------------------------------------------------

/// Prediction seam — production adapter + test fault injection.
pub trait EnsemblePredictor: Send + Sync {
    fn predict(&self, model: &str, provider: &str, features: &RoutingFeatures) -> PredictionBundle;
    fn commit_id(&self) -> CommitId;
}

/// Production [`EnsemblePredictor`]: an immutable [`ModelEnsemble`] snapshot
/// pinned to a complete, verified [`ModelCommit`].
pub struct ModelEnsemblePredictor {
    ensemble: Arc<ModelEnsemble>,
    commit: ModelCommit,
    /// Ordered training payload retained to rebuild a canonical lineage from
    /// genesis. It is never exposed as a mutable journal and is used only by
    /// this predictor's train/swap seam.
    history: Option<Vec<DatasetTrainingSample>>,
    /// Verified commit records from the known root through the held commit.
    lineage: Vec<ModelCommit>,
}

type TrainingLineageResult = Result<
    (
        ModelEnsemble,
        ModelCommit,
        Option<Vec<DatasetTrainingSample>>,
        Vec<ModelCommit>,
    ),
    ReplayError,
>;

impl ModelEnsemblePredictor {
    /// The genesis predictor: a cold-start ensemble committed as the root of
    /// the `shadow` model lineage.
    pub fn genesis() -> Self {
        let ensemble = ModelEnsemble::new();
        let checkpoint = ensemble.save_all();
        let commit = ModelCommit::new(ModelId::new("shadow"), checkpoint, None, 0);
        Self::from_verified_parts(ensemble, commit.clone(), Some(Vec::new()), vec![commit])
            .expect("the deterministic shadow genesis commit must verify")
    }

    /// Compatibility constructor for a root shadow commit.
    ///
    /// A checkpoint alone cannot describe a child lineage, so this method
    /// accepts only the canonical root `shadow` commit. New callers loading a
    /// child should use [`Self::from_model_commit`], which verifies the full
    /// model/schema/parent/lineage record.
    ///
    /// A root commit record carries no parent and no learning-event count, so
    /// the checkpoint is the only evidence of whether the ensemble was ever
    /// trained. Only a checkpoint identical to the cold genesis ensemble is
    /// treated as a complete empty history (it really is the genesis); a
    /// checkpoint that already holds training is loaded with *unknown* history,
    /// exactly like [`Self::from_model_commit`], so the next training run
    /// continues from it instead of rebuilding a cold ensemble from genesis and
    /// silently discarding the loaded training.
    pub fn from_commit(
        checkpoint: &ModelCheckpoint,
        commit: CommitId,
    ) -> Result<Self, ReplayError> {
        // Load first so malformed state gets the detailed loader error rather
        // than being hidden behind a generic id mismatch.
        let ensemble = ModelEnsemble::load_all(checkpoint)?;
        let expected = ModelCommit::new(ModelId::new("shadow"), checkpoint.clone(), None, 0);
        if expected.commit_id != commit {
            return Err(ReplayError::CommitMismatch {
                expected: expected.commit_id,
                actual: commit,
            });
        }
        if checkpoint.content_hash() == ModelEnsemble::new().save_all().content_hash() {
            Self::from_verified_parts(ensemble, expected.clone(), Some(Vec::new()), vec![expected])
        } else {
            Self::from_verified_parts(ensemble, expected.clone(), None, vec![expected])
        }
    }

    /// Build a predictor from a complete immutable commit record.
    ///
    /// A root commit is self-contained and remains supported here. A child
    /// commit must be supplied through [`Self::from_model_commit_with_lineage`]
    /// so a parent reference can never be silently truncated.
    pub fn from_model_commit(commit: &ModelCommit) -> Result<Self, ReplayError> {
        Self::from_model_commit_with_lineage(commit, std::slice::from_ref(commit))
    }

    /// Build a predictor from a commit plus its complete root-to-current
    /// lineage. Every record is verified, model-consistent, linked, and
    /// cycle-free before the predictor is retained.
    pub fn from_model_commit_with_lineage(
        commit: &ModelCommit,
        lineage: &[ModelCommit],
    ) -> Result<Self, ReplayError> {
        if !commit.verify() {
            return Err(ReplayError::InvalidCommit {
                commit_id: commit.commit_id.clone(),
                reason: "predictor received an unverified commit".to_string(),
            });
        }
        Self::from_verified_parts(
            ModelEnsemble::load_all(&commit.checkpoint)?,
            commit.clone(),
            None,
            lineage.to_vec(),
        )
    }

    /// Alias emphasizing that the input has already passed the artifact
    /// boundary but is still checked again at this predictor boundary.
    pub fn from_verified_commit(commit: &ModelCommit) -> Result<Self, ReplayError> {
        Self::from_model_commit(commit)
    }

    /// Complete-lineage alias for callers that already named their artifact
    /// boundary explicitly.
    pub fn from_verified_commit_with_lineage(
        commit: &ModelCommit,
        lineage: &[ModelCommit],
    ) -> Result<Self, ReplayError> {
        Self::from_model_commit_with_lineage(commit, lineage)
    }

    fn validate_complete_lineage(
        commit: &ModelCommit,
        lineage: &[ModelCommit],
    ) -> Result<(), ReplayError> {
        if lineage.is_empty() {
            return Err(ReplayError::LineageCorrupt {
                commit_id: commit.commit_id.clone(),
                reason: "lineage is empty".to_string(),
            });
        }
        if lineage.last().map(|entry| entry.commit_id.clone()) != Some(commit.commit_id.clone()) {
            return Err(ReplayError::LineageCorrupt {
                commit_id: commit.commit_id.clone(),
                reason: "lineage does not end at the requested commit".to_string(),
            });
        }
        if commit.parent.is_none() && lineage.len() != 1 {
            return Err(ReplayError::LineageCorrupt {
                commit_id: commit.commit_id.clone(),
                reason: "root commit has extra lineage records".to_string(),
            });
        }
        if commit.parent.is_some() && lineage.len() < 2 {
            return Err(ReplayError::LineageCorrupt {
                commit_id: commit.commit_id.clone(),
                reason: "non-root commit is missing its parent record".to_string(),
            });
        }

        let mut seen = HashSet::new();
        for entry in lineage {
            if !seen.insert(entry.commit_id.clone()) {
                return Err(ReplayError::LineageCorrupt {
                    commit_id: commit.commit_id.clone(),
                    reason: format!("cycle or duplicate at '{}'", entry.commit_id),
                });
            }
            if !entry.verify() {
                return Err(ReplayError::LineageCorrupt {
                    commit_id: entry.commit_id.clone(),
                    reason: "lineage record failed commit/checkpoint verification".to_string(),
                });
            }
            if entry.model_id != commit.model_id {
                return Err(ReplayError::LineageCorrupt {
                    commit_id: entry.commit_id.clone(),
                    reason: format!(
                        "model '{}' does not match '{}'",
                        entry.model_id, commit.model_id
                    ),
                });
            }
        }
        if lineage[0].parent.is_some() {
            return Err(ReplayError::LineageCorrupt {
                commit_id: lineage[0].commit_id.clone(),
                reason: "lineage does not start at a root commit".to_string(),
            });
        }
        for pair in lineage.windows(2) {
            if pair[1].parent.as_ref() != Some(&pair[0].commit_id) {
                return Err(ReplayError::LineageCorrupt {
                    commit_id: pair[1].commit_id.clone(),
                    reason: format!("parent link does not point to '{}'", pair[0].commit_id),
                });
            }
        }
        if lineage.last().map(|entry| entry.parent.clone()) != Some(commit.parent.clone()) {
            return Err(ReplayError::LineageCorrupt {
                commit_id: commit.commit_id.clone(),
                reason: "requested commit parent differs from retained lineage".to_string(),
            });
        }
        Ok(())
    }

    fn from_verified_parts(
        ensemble: ModelEnsemble,
        commit: ModelCommit,
        history: Option<Vec<DatasetTrainingSample>>,
        lineage: Vec<ModelCommit>,
    ) -> Result<Self, ReplayError> {
        if !commit.verify() {
            return Err(ReplayError::InvalidCommit {
                commit_id: commit.commit_id,
                reason: "predictor received an unverified commit".to_string(),
            });
        }
        let held_checkpoint = ensemble.save_all();
        if held_checkpoint.content_hash() != commit.checkpoint.content_hash() {
            return Err(ReplayError::CommitMismatch {
                expected: commit.commit_id,
                actual: CommitId::from_hash(held_checkpoint.content_hash()),
            });
        }
        Self::validate_complete_lineage(&commit, &lineage)?;
        Ok(Self {
            ensemble: Arc::new(ensemble),
            commit,
            history,
            lineage,
        })
    }

    /// Pure training: clone the current verified ensemble, apply samples in
    /// order, and retain the complete commit/parent/checkpoint lineage.
    pub fn train(&self, samples: &[DatasetTrainingSample]) -> (ModelEnsemble, CommitId) {
        self.try_train(samples)
            .map(|(ensemble, commit)| (ensemble, commit.commit_id))
            .expect("shadow training received an invalid sample")
    }

    /// Fallible training boundary used by swap-capable callers.
    pub fn try_train(
        &self,
        samples: &[DatasetTrainingSample],
    ) -> Result<(ModelEnsemble, ModelCommit), ReplayError> {
        self.try_train_with_history(samples)
            .map(|(ensemble, commit, _, _)| (ensemble, commit))
    }

    /// Training implementation that also returns the ordered payload retained
    /// by the predictor. `Some(history)` means the predictor owns a complete
    /// genesis-rebuildable lineage; `None` means the caller supplied only a
    /// child checkpoint and the current commit must remain the parent.
    pub(super) fn try_train_with_history(
        &self,
        samples: &[DatasetTrainingSample],
    ) -> TrainingLineageResult {
        for sample in samples {
            if sample.schema_version != FEATURE_SCHEMA_VERSION {
                return Err(ReplayError::UnsupportedSchema {
                    component: "shadow training sample".to_string(),
                    version: sample.schema_version,
                    supported: FEATURE_SCHEMA_VERSION,
                });
            }
            validate_replay_sample(sample).map_err(|reason| ReplayError::InvalidEvent {
                event_id: "shadow-train".to_string(),
                reason,
            })?;
        }

        if let Some(previous_history) = &self.history {
            let expected_lineage_len =
                self.commit
                    .learning_event_count
                    .checked_add(1)
                    .ok_or_else(|| ReplayError::InvalidCommit {
                        commit_id: self.commit.commit_id.clone(),
                        reason: "retained lineage count overflow".to_string(),
                    })?;
            if previous_history.len() as u64 != self.commit.learning_event_count
                || self.lineage.len() as u64 != expected_lineage_len
            {
                return Err(ReplayError::InvalidCommit {
                    commit_id: self.commit.commit_id.clone(),
                    reason: "retained history and commit count disagree".to_string(),
                });
            }
            let mut history = previous_history.clone();
            history.extend_from_slice(samples);
            if history.is_empty() {
                return Ok((
                    ModelEnsemble::load_all(&self.commit.checkpoint)?,
                    self.commit.clone(),
                    Some(history),
                    self.lineage.clone(),
                ));
            }

            let mut ensemble = ModelEnsemble::new();
            let mut commit =
                ModelCommit::new(self.commit.model_id.clone(), ensemble.save_all(), None, 0);
            let mut lineage = vec![commit.clone()];
            for sample in &history {
                ensemble.update_all(sample);
                let count = commit.learning_event_count.checked_add(1).ok_or_else(|| {
                    ReplayError::InvalidCommit {
                        commit_id: commit.commit_id.clone(),
                        reason: "learning event count overflow".to_string(),
                    }
                })?;
                commit = ModelCommit::new(
                    self.commit.model_id.clone(),
                    ensemble.save_all(),
                    Some(commit.commit_id.clone()),
                    count,
                );
                if !commit.verify() {
                    return Err(ReplayError::InvalidCommit {
                        commit_id: commit.commit_id,
                        reason: "shadow training produced an invalid commit".to_string(),
                    });
                }
                lineage.push(commit.clone());
            }
            return Ok((ensemble, commit, Some(history), lineage));
        }

        let mut ensemble = ModelEnsemble::load_all(&self.commit.checkpoint)?;
        for sample in samples {
            ensemble.update_all(sample);
        }
        let count = self
            .commit
            .learning_event_count
            .checked_add(samples.len() as u64)
            .ok_or_else(|| ReplayError::InvalidCommit {
                commit_id: self.commit.commit_id.clone(),
                reason: "learning event count overflow".to_string(),
            })?;
        let commit = ModelCommit::new(
            self.commit.model_id.clone(),
            ensemble.save_all(),
            Some(self.commit.commit_id.clone()),
            count,
        );
        if !commit.verify() {
            return Err(ReplayError::InvalidCommit {
                commit_id: commit.commit_id,
                reason: "shadow training produced an invalid commit".to_string(),
            });
        }
        let mut lineage = self.lineage.clone();
        lineage.push(commit.clone());
        Ok((ensemble, commit, None, lineage))
    }

    /// Return a fully verified predictor for an already trained ensemble and
    /// commit. This is the only path used by `ShadowEngine` when swapping.
    pub(super) fn from_trained_parts(
        ensemble: ModelEnsemble,
        commit: ModelCommit,
        history: Option<Vec<DatasetTrainingSample>>,
        lineage: Vec<ModelCommit>,
    ) -> Result<Self, ReplayError> {
        Self::from_verified_parts(ensemble, commit, history, lineage)
    }

    /// Verify the held commit and checkpoint.
    pub fn verify(&self) -> bool {
        self.commit.verify()
            && self.ensemble.save_all().content_hash() == self.commit.checkpoint.content_hash()
            && Self::validate_complete_lineage(&self.commit, &self.lineage).is_ok()
    }

    /// Checkpoint retained by the verified commit.
    pub fn ensemble_checkpoint(&self) -> ModelCheckpoint {
        self.commit.checkpoint.clone()
    }

    /// Commit id of the held ensemble snapshot.
    pub fn commit(&self) -> CommitId {
        self.commit.commit_id.clone()
    }

    /// Complete commit record retained by this predictor.
    pub fn commit_record(&self) -> ModelCommit {
        self.commit.clone()
    }

    /// Parent of the held commit, if it is not a root.
    pub fn parent(&self) -> Option<CommitId> {
        self.commit.parent.clone()
    }

    /// Full verified parent record when it is retained locally.
    pub fn parent_record(&self) -> Option<ModelCommit> {
        if self.lineage.len() < 2 {
            None
        } else {
            self.lineage.get(self.lineage.len() - 2).cloned()
        }
    }

    /// Ordered verified commit records retained by this predictor.
    pub fn lineage(&self) -> Vec<ModelCommit> {
        self.lineage.clone()
    }

    /// Number of ordered training samples retained for genesis replay.
    pub fn history_len(&self) -> usize {
        self.history.as_ref().map_or(0, Vec::len)
    }

    /// Checkpoint alias for callers that treat the predictor as a snapshot.
    pub fn checkpoint(&self) -> ModelCheckpoint {
        self.ensemble_checkpoint()
    }
}

impl EnsemblePredictor for ModelEnsemblePredictor {
    fn predict(&self, model: &str, provider: &str, features: &RoutingFeatures) -> PredictionBundle {
        PredictionBundle {
            candidate_model: model.to_string(),
            candidate_provider: provider.to_string(),
            success: self.ensemble.success.predict(features),
            latency: self.ensemble.latency.predict(features),
            ttft: self.ensemble.ttft.predict(features),
            cost: self.ensemble.cost.predict(features),
        }
    }

    fn commit_id(&self) -> CommitId {
        self.commit.commit_id.clone()
    }
}
