use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::checkpoint::{ModelCheckpoint, ModelCommit};
use super::event::{LearningEvent, ReplayError};
use super::identity::{CommitId, ModelId};
use super::MODEL_CHECKPOINT_SCHEMA_VERSION;
use crate::ml::dataset::TrainingSample as DatasetTrainingSample;
use crate::ml::features::FEATURE_DIMENSION;
use crate::ml::model::{CostModel, LatencyModel, RoutingModel, SuccessModel, TtftModel};

// ---------------------------------------------------------------------------
// CommitInfo — metadata for log display
// ---------------------------------------------------------------------------

/// Metadata for a commit, used in log display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitInfo {
    pub commit_id: CommitId,
    pub parent: Option<CommitId>,
    pub message: String,
    pub learning_event_count: u64,
    pub created_at: i64,
}

// ---------------------------------------------------------------------------
// ModelEnsemble — holds all 4 concrete models
// ---------------------------------------------------------------------------

/// An ensemble holding all four routing models.
///
/// `Clone` snapshots the weights so an analysis can run against exactly what is
/// serving, without waiting for the router's read lock to be free for the whole
/// replay.
#[derive(Debug, Clone)]
pub struct ModelEnsemble {
    pub success: SuccessModel,
    pub latency: LatencyModel,
    pub ttft: TtftModel,
    pub cost: CostModel,
}

impl ModelEnsemble {
    /// Create a fresh ensemble with all models at cold-start.
    pub fn new() -> Self {
        ModelEnsemble {
            success: SuccessModel::new(FEATURE_DIMENSION),
            latency: LatencyModel::new(FEATURE_DIMENSION),
            ttft: TtftModel::new(FEATURE_DIMENSION),
            cost: CostModel::new(FEATURE_DIMENSION),
        }
    }

    /// Save all models into a single checkpoint.
    pub fn save_all(&self) -> ModelCheckpoint {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        ModelCheckpoint {
            schema_version: MODEL_CHECKPOINT_SCHEMA_VERSION,
            success: self.success.save(),
            latency: self.latency.save(),
            ttft: self.ttft.save(),
            cost: self.cost.save(),
            feature_schema_version: crate::ml::features::FEATURE_SCHEMA_VERSION,
            created_at: now,
        }
    }

    /// Load an ensemble from a checkpoint.
    ///
    /// Validates that all model states can be deserialized. This is a
    /// constructor — `RoutingModel::load` is a static method.
    pub fn load_all(checkpoint: &ModelCheckpoint) -> Result<Self, ReplayError> {
        checkpoint.validate()?;
        let success = SuccessModel::load(&checkpoint.success).map_err(|e| {
            ReplayError::IncompatibleState {
                model: "success".to_string(),
                reason: e,
            }
        })?;
        let latency = LatencyModel::load(&checkpoint.latency).map_err(|e| {
            ReplayError::IncompatibleState {
                model: "latency".to_string(),
                reason: e,
            }
        })?;
        let ttft =
            TtftModel::load(&checkpoint.ttft).map_err(|e| ReplayError::IncompatibleState {
                model: "ttft".to_string(),
                reason: e,
            })?;
        let cost =
            CostModel::load(&checkpoint.cost).map_err(|e| ReplayError::IncompatibleState {
                model: "cost".to_string(),
                reason: e,
            })?;
        Ok(ModelEnsemble {
            success,
            latency,
            ttft,
            cost,
        })
    }

    /// Reset all models to cold-start.
    pub fn reset_all(&mut self) {
        self.success.reset();
        self.latency.reset();
        self.ttft.reset();
        self.cost.reset();
    }

    /// Update all models from a training sample.
    ///
    /// - success: always updated, target = 1.0 if success, 0.0 otherwise.
    /// - latency: only if latency_ms is Some, finite, and >= 0.
    /// - ttft: only if ttft_ms is Some, finite, and >= 0.
    /// - cost: only if cost is Some, finite, and >= 0.
    pub fn update_all(&mut self, sample: &DatasetTrainingSample) {
        let features = &sample.features;
        let targets = &sample.targets;

        // Success model: always
        let success_target = if targets.success { 1.0 } else { 0.0 };
        self.success.update(features, success_target);

        // Latency model: only if latency_ms present and valid
        if let Some(latency) = targets.latency_ms {
            if latency.is_finite() && latency >= 0.0 {
                self.latency.update(features, latency);
            }
        }

        // TTFT model: only if ttft_ms present and valid
        if let Some(ttft) = targets.ttft_ms {
            if ttft.is_finite() && ttft >= 0.0 {
                self.ttft.update(features, ttft);
            }
        }

        // Cost model: only if cost present and valid
        if let Some(cost) = targets.cost {
            if cost.is_finite() && cost >= 0.0 {
                self.cost.update(features, cost);
            }
        }
    }
}

impl Default for ModelEnsemble {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ModelStore — git-like commit storage
// ---------------------------------------------------------------------------

/// The immutable record and user-facing message retained by the store.
struct StoredCommit {
    commit: ModelCommit,
    message: String,
}

/// A git-like store for verified model commits with tags and HEAD.
pub struct ModelStore {
    commits: HashMap<CommitId, StoredCommit>,
    tags: HashMap<String, CommitId>,
    head: Option<CommitId>,
    model_id: ModelId,
}

impl ModelStore {
    pub fn new() -> Self {
        Self::with_model_id(ModelId::new("ensemble"))
    }

    /// Create a store for a named model lineage.
    pub fn with_model_id(model_id: ModelId) -> Self {
        ModelStore {
            commits: HashMap::new(),
            tags: HashMap::new(),
            head: None,
            model_id,
        }
    }

    /// The model lineage used by the convenience `commit` method.
    pub fn model_id(&self) -> &ModelId {
        &self.model_id
    }

    /// Commit a checkpoint using the store's model lineage and current head.
    ///
    /// This compatibility wrapper preserves the original infallible API. It
    /// panics before mutation when the artifact is invalid; operational code
    /// should use [`Self::try_commit`] and handle the error explicitly.
    pub fn commit(&mut self, checkpoint: ModelCheckpoint, message: String) -> CommitId {
        self.try_commit(checkpoint, message)
            .expect("ModelStore::commit received an invalid checkpoint")
    }

    /// Fallible, fail-closed commit operation.
    pub fn try_commit(
        &mut self,
        checkpoint: ModelCheckpoint,
        message: String,
    ) -> Result<CommitId, ReplayError> {
        let parent = self.head.clone();
        self.try_commit_from(self.model_id.clone(), checkpoint, parent, 0, message)
    }

    /// Commit a checkpoint with explicit lineage metadata.
    pub fn try_commit_from(
        &mut self,
        model_id: ModelId,
        checkpoint: ModelCheckpoint,
        parent: Option<CommitId>,
        learning_event_count: u64,
        message: String,
    ) -> Result<CommitId, ReplayError> {
        let commit = ModelCommit::new(model_id, checkpoint, parent, learning_event_count);
        if !commit.verify() {
            return Err(ReplayError::InvalidCommit {
                commit_id: commit.commit_id,
                reason: "checkpoint or lineage does not verify".to_string(),
            });
        }
        self.try_insert_commit(commit, message)
    }

    /// Insert an already constructed commit after validating its lineage.
    pub fn try_insert_commit(
        &mut self,
        commit: ModelCommit,
        message: String,
    ) -> Result<CommitId, ReplayError> {
        if !commit.verify() {
            return Err(ReplayError::InvalidCommit {
                commit_id: commit.commit_id,
                reason: "commit identity, schema, or checkpoint is invalid".to_string(),
            });
        }
        if commit.model_id != self.model_id {
            return Err(ReplayError::InvalidCommit {
                commit_id: commit.commit_id,
                reason: "commit model does not belong to this store".to_string(),
            });
        }
        if let Some(parent_id) = commit.parent.clone() {
            let parent =
                self.commits
                    .get(&parent_id)
                    .ok_or_else(|| ReplayError::LineageCorrupt {
                        commit_id: commit.commit_id.clone(),
                        reason: format!("parent '{}' is not present", parent_id),
                    })?;
            if parent.commit.model_id != commit.model_id {
                return Err(ReplayError::InvalidCommit {
                    commit_id: commit.commit_id,
                    reason: "parent belongs to a different model lineage".to_string(),
                });
            }
            self.verify_lineage(&parent_id)?;
        }

        let commit_id = commit.commit_id.clone();
        if let Some(existing) = self.commits.get(&commit_id) {
            if existing.commit.canonical_identity() != commit.canonical_identity() {
                return Err(ReplayError::InvalidCommit {
                    commit_id,
                    reason: "content-address collision or altered commit".to_string(),
                });
            }
        } else {
            self.commits
                .insert(commit_id.clone(), StoredCommit { commit, message });
        }
        self.head = Some(commit_id.clone());
        Ok(commit_id)
    }

    /// Checkout a verified checkpoint by commit ID.
    pub fn checkout(&self, commit_id: &CommitId) -> Result<ModelCheckpoint, ReplayError> {
        self.checkout_commit(commit_id)
            .map(|commit| commit.checkpoint)
    }

    /// Checkout the complete immutable commit record.
    pub fn checkout_commit(&self, commit_id: &CommitId) -> Result<ModelCommit, ReplayError> {
        if !self.commits.contains_key(commit_id) {
            return Err(ReplayError::CheckpointNotFound(commit_id.clone()));
        }
        self.verify_lineage(commit_id)?;
        self.commits
            .get(commit_id)
            .map(|stored| stored.commit.clone())
            .ok_or_else(|| ReplayError::CheckpointNotFound(commit_id.clone()))
    }

    /// Verify a complete parent chain, including artifact identity at every
    /// node. Missing parents and cycles are errors; they are never silently
    /// truncated from a log or checkout.
    pub fn verify_lineage(&self, commit_id: &CommitId) -> Result<(), ReplayError> {
        let mut current = Some(commit_id.clone());
        let mut seen = HashSet::new();
        while let Some(id) = current {
            if !seen.insert(id.clone()) {
                return Err(ReplayError::LineageCorrupt {
                    commit_id: id,
                    reason: "cycle detected in parent chain".to_string(),
                });
            }
            let stored = self
                .commits
                .get(&id)
                .ok_or_else(|| ReplayError::LineageCorrupt {
                    commit_id: id.clone(),
                    reason: "commit is not present in the store".to_string(),
                })?;
            if !stored.commit.verify() {
                return Err(ReplayError::LineageCorrupt {
                    commit_id: id,
                    reason: "commit or checkpoint verification failed".to_string(),
                });
            }
            current = stored.commit.parent.clone();
        }
        Ok(())
    }

    /// Get the current HEAD commit ID.
    pub fn get_head(&self) -> Option<CommitId> {
        self.head.clone()
    }

    /// Set the HEAD to a specific verified commit ID.
    pub fn set_head(&mut self, commit_id: CommitId) -> Result<(), ReplayError> {
        if !self.commits.contains_key(&commit_id) {
            return Err(ReplayError::CheckpointNotFound(commit_id));
        }
        self.verify_lineage(&commit_id)?;
        self.head = Some(commit_id);
        Ok(())
    }

    /// Walk the parent chain from HEAD, returning commit info newest first.
    pub fn log_checked(&self) -> Result<Vec<CommitInfo>, ReplayError> {
        let Some(head) = self.head.clone() else {
            return Ok(Vec::new());
        };
        self.verify_lineage(&head)?;
        let mut result = Vec::new();
        let mut current = Some(head);
        while let Some(id) = current {
            let Some(stored) = self.commits.get(&id) else {
                break;
            };
            current = stored.commit.parent.clone();
            result.push(CommitInfo {
                commit_id: stored.commit.commit_id.clone(),
                parent: stored.commit.parent.clone(),
                message: stored.message.clone(),
                learning_event_count: stored.commit.learning_event_count,
                created_at: stored.commit.created_at,
            });
        }
        Ok(result)
    }

    /// Compatibility log view. Corrupt lineage returns no partial log; use
    /// [`Self::log_checked`] when the error is required.
    pub fn log(&self) -> Vec<CommitInfo> {
        self.log_checked().unwrap_or_default()
    }

    /// Tag a commit with a name. Errors if the tag already exists or the
    /// target lineage is corrupt.
    pub fn tag(&mut self, commit_id: &CommitId, tag_name: &str) -> Result<(), ReplayError> {
        if self.tags.contains_key(tag_name) {
            return Err(ReplayError::TagAlreadyExists(tag_name.to_string()));
        }
        if !self.commits.contains_key(commit_id) {
            return Err(ReplayError::CheckpointNotFound(commit_id.clone()));
        }
        self.verify_lineage(commit_id)?;
        self.tags.insert(tag_name.to_string(), commit_id.clone());
        Ok(())
    }

    /// Resolve a tag name to a commit ID.
    pub fn resolve_tag(&self, tag_name: &str) -> Option<&CommitId> {
        self.tags.get(tag_name)
    }

    pub fn len(&self) -> usize {
        self.commits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }
}

impl Default for ModelStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ReplayEngine — stateless replay of learning events
// ---------------------------------------------------------------------------

/// Stateless engine for replaying learning events against a verified base.
pub struct ReplayEngine;

impl ReplayEngine {
    /// Replay events and return only the resulting checkpoint.
    ///
    /// This compatibility API accepts legacy events whose parent/result fields
    /// are not populated yet, but it still validates their model, payload,
    /// schema, and order. Use [`Self::replay_verified`] when a complete,
    /// fully checked event chain is required.
    pub fn replay(
        events: &[LearningEvent],
        base: Option<&ModelCheckpoint>,
    ) -> Result<ModelCheckpoint, ReplayError> {
        let commit = Self::replay_internal(events, base, None, false)?;
        Ok(commit.checkpoint)
    }

    /// Replay events against a verified base commit and return the resulting
    /// commit. Unpopulated parent fields are tolerated for migration from the
    /// original 7E-0 API; any supplied parent/result is checked exactly.
    pub fn replay_commit(
        events: &[LearningEvent],
        base: Option<&ModelCommit>,
    ) -> Result<ModelCommit, ReplayError> {
        Self::replay_internal(events, None, base, false)
    }

    /// Strict replay: every event must explicitly carry the expected parent
    /// and result, and the complete base commit must verify first.
    pub fn replay_verified(
        events: &[LearningEvent],
        base: Option<&ModelCommit>,
    ) -> Result<ModelCommit, ReplayError> {
        Self::replay_internal(events, None, base, true)
    }

    /// Replay against a checkpoint while retaining the resulting commit
    /// identity. This is useful to callers migrating from the old API without
    /// discarding the newly repaired lineage.
    pub fn replay_with_checkpoint(
        events: &[LearningEvent],
        base: Option<&ModelCheckpoint>,
    ) -> Result<ModelCommit, ReplayError> {
        Self::replay_internal(events, base, None, false)
    }

    fn replay_internal(
        events: &[LearningEvent],
        base_checkpoint: Option<&ModelCheckpoint>,
        base_commit: Option<&ModelCommit>,
        strict: bool,
    ) -> Result<ModelCommit, ReplayError> {
        if events.is_empty() {
            return Err(ReplayError::EmptyEventList);
        }
        for event in events {
            event.validate()?;
        }
        Self::validate_event_order(events)?;

        if let Some(checkpoint) = base_checkpoint {
            checkpoint.validate()?;
        }
        if let Some(commit) = base_commit {
            if !commit.verify() {
                return Err(ReplayError::InvalidCommit {
                    commit_id: commit.commit_id.clone(),
                    reason: "base commit failed verification".to_string(),
                });
            }
            if let Some(checkpoint) = base_checkpoint {
                if checkpoint.content_hash() != commit.checkpoint.content_hash() {
                    return Err(ReplayError::CommitMismatch {
                        expected: commit.commit_id.clone(),
                        actual: Self::checkpoint_identity(
                            checkpoint,
                            &commit.model_id,
                            commit.parent.as_ref(),
                            commit.learning_event_count,
                        ),
                    });
                }
            }
        }

        let model_id = base_commit
            .map(|commit| commit.model_id.clone())
            .unwrap_or_else(|| events[0].model_id.clone());
        let mut ensemble = match base_checkpoint {
            Some(checkpoint) => ModelEnsemble::load_all(checkpoint)?,
            None => match base_commit {
                Some(commit) => ModelEnsemble::load_all(&commit.checkpoint)?,
                None => ModelEnsemble::new(),
            },
        };
        let mut current_commit = base_commit.cloned();
        let mut learning_event_count = base_commit
            .map(|commit| commit.learning_event_count)
            .unwrap_or(0);

        for event in events {
            if event.model_id != model_id {
                return Err(ReplayError::EventModelMismatch {
                    expected: model_id,
                    actual: event.model_id.clone(),
                });
            }

            let expected_parent = current_commit
                .as_ref()
                .map(|commit| commit.commit_id.clone());
            match event.parent_commit.clone() {
                Some(actual) if expected_parent.as_ref() != Some(&actual) => {
                    return Err(ReplayError::EventParentMismatch {
                        event_id: event.event_id.clone(),
                        expected: expected_parent,
                        actual: Some(actual),
                    });
                }
                None if strict && expected_parent.is_some() => {
                    return Err(ReplayError::EventParentMismatch {
                        event_id: event.event_id.clone(),
                        expected: expected_parent,
                        actual: None,
                    });
                }
                _ => {}
            }

            for sample in &event.samples {
                ensemble.update_all(sample);
            }
            learning_event_count = learning_event_count
                .checked_add(event.samples.len() as u64)
                .ok_or_else(|| ReplayError::InvalidEvent {
                    event_id: event.event_id.clone(),
                    reason: "learning event count overflow".to_string(),
                })?;
            let child = ModelCommit::new(
                model_id.clone(),
                ensemble.save_all(),
                current_commit
                    .as_ref()
                    .map(|commit| commit.commit_id.clone()),
                learning_event_count,
            );
            if !child.verify() {
                return Err(ReplayError::InvalidCommit {
                    commit_id: child.commit_id,
                    reason: "replayed child commit failed verification".to_string(),
                });
            }
            if let Some(actual) = event.result_commit.clone() {
                if actual != child.commit_id {
                    return Err(ReplayError::EventResultMismatch {
                        event_id: event.event_id.clone(),
                        expected: child.commit_id,
                        actual,
                    });
                }
            } else if strict {
                return Err(ReplayError::EventResultMissing {
                    event_id: event.event_id.clone(),
                });
            }
            current_commit = Some(child);
        }

        current_commit.ok_or(ReplayError::EmptyEventList)
    }

    /// Validate sequence-bearing event ids before applying any samples.
    fn validate_event_order(events: &[LearningEvent]) -> Result<(), ReplayError> {
        let mut seen_ids = HashSet::new();
        let mut previous_sequence = None;
        for (index, event) in events.iter().enumerate() {
            if !seen_ids.insert(event.event_id.clone()) {
                return Err(ReplayError::EventOutOfOrder {
                    expected: index as u64,
                    actual: index as u64,
                });
            }
            if let Some(sequence) = event.logical_sequence() {
                if let Some(previous) = previous_sequence {
                    if sequence <= previous {
                        return Err(ReplayError::EventOutOfOrder {
                            expected: previous.saturating_add(1),
                            actual: sequence,
                        });
                    }
                }
                previous_sequence = Some(sequence);
            }
        }
        Ok(())
    }

    fn checkpoint_identity(
        checkpoint: &ModelCheckpoint,
        model_id: &ModelId,
        parent: Option<&CommitId>,
        learning_event_count: u64,
    ) -> CommitId {
        let algorithm_versions = ModelCommit::algorithm_versions(checkpoint);
        ModelCommit::canonical_id_for(
            model_id,
            parent,
            checkpoint.feature_schema_version,
            learning_event_count,
            checkpoint,
            &algorithm_versions,
            &HashMap::new(),
        )
    }

    /// Verify checkpoint integrity and schema compatibility.
    pub fn verify_checkpoint_integrity(checkpoint: &ModelCheckpoint) -> bool {
        checkpoint.verify()
    }
}
