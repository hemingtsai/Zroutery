//! Model identity, versioning, checkpoints, and replay for ML routing.
//!
//! Provides content-addressed model commits, checkpoint management,
//! learning event tracking, and deterministic replay of training history.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::dataset::TrainingSample as DatasetTrainingSample;
use super::features::FEATURE_DIMENSION;
use super::model::{
    CostModel, LatencyModel, ModelState, RoutingModel, SuccessModel, TtftModel,
};

// ---------------------------------------------------------------------------
// ModelId — newtype String
// ---------------------------------------------------------------------------

/// Identifies a routing model by name (e.g. "success", "latency").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelId(String);

impl ModelId {
    pub fn success() -> Self {
        Self("success".into())
    }

    pub fn latency() -> Self {
        Self("latency".into())
    }

    pub fn ttft() -> Self {
        Self("ttft".into())
    }

    pub fn cost() -> Self {
        Self("cost".into())
    }

    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// CommitId — content-addressed hex string
// ---------------------------------------------------------------------------

/// Content-addressed commit identifier (16-char hex string).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CommitId(String);

impl CommitId {
    pub fn from_hash(hash: u64) -> Self {
        Self(format!("{:016x}", hash))
    }

    pub fn new(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// ModelCheckpoint — frozen snapshot of all 4 model states
// ---------------------------------------------------------------------------

/// A frozen snapshot of all four routing model states at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCheckpoint {
    pub success: ModelState,
    pub latency: ModelState,
    pub ttft: ModelState,
    pub cost: ModelState,
    pub feature_schema_version: u32,
    pub created_at: i64,
}

impl ModelCheckpoint {
    /// Compute a content hash over all 4 states' checksum, update_count, and
    /// algorithm bytes. Uses FNV-1a.
    pub fn content_hash(&self) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325; // FNV offset basis
        let states = [&self.success, &self.latency, &self.ttft, &self.cost];
        for state in &states {
            // Mix checksum
            for b in state.checksum.to_le_bytes() {
                hash ^= b as u64;
                hash = hash.wrapping_mul(0x100000001b3);
            }
            // Mix update_count
            for b in state.update_count.to_le_bytes() {
                hash ^= b as u64;
                hash = hash.wrapping_mul(0x100000001b3);
            }
            // Mix algorithm bytes
            for b in state.algorithm.as_bytes() {
                hash ^= *b as u64;
                hash = hash.wrapping_mul(0x100000001b3);
            }
        }
        // Mix feature_schema_version to distinguish schema-incompatible checkpoints
        for b in self.feature_schema_version.to_le_bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    /// Verify checksums on all 4 model states.
    pub fn verify(&self) -> bool {
        self.success.verify_checksum()
            && self.latency.verify_checksum()
            && self.ttft.verify_checksum()
            && self.cost.verify_checksum()
    }
}

// ---------------------------------------------------------------------------
// ModelCommit — immutable version of a model checkpoint
// ---------------------------------------------------------------------------

/// An immutable commit linking a checkpoint to its history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCommit {
    pub commit_id: CommitId,
    pub parent: Option<CommitId>,
    pub model_id: ModelId,
    pub checkpoint: ModelCheckpoint,
    pub learning_event_count: u64,
    pub algorithm_versions: Vec<(String, String)>,
    pub feature_schema_version: u32,
    pub created_at: i64,
    pub metadata: HashMap<String, String>,
}

impl ModelCommit {
    /// Create a new commit from a checkpoint.
    ///
    /// Computes the commit_id from `checkpoint.content_hash()`, populates
    /// algorithm versions from each state's algorithm field, and timestamps
    /// with the current wall clock.
    pub fn new(
        model_id: ModelId,
        checkpoint: ModelCheckpoint,
        parent: Option<CommitId>,
        learning_event_count: u64,
    ) -> Self {
        let commit_id = CommitId::from_hash(checkpoint.content_hash());
        let algorithm_versions = vec![
            ("success".to_string(), checkpoint.success.algorithm.clone()),
            ("latency".to_string(), checkpoint.latency.algorithm.clone()),
            ("ttft".to_string(), checkpoint.ttft.algorithm.clone()),
            ("cost".to_string(), checkpoint.cost.algorithm.clone()),
        ];
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let feature_schema_version = checkpoint.feature_schema_version;
        ModelCommit {
            commit_id,
            parent,
            model_id,
            checkpoint,
            learning_event_count,
            algorithm_versions,
            feature_schema_version,
            created_at,
            metadata: HashMap::new(),
        }
    }

    /// Verify checkpoint integrity and that the commit_id matches the
    /// recomputed content hash.
    pub fn verify(&self) -> bool {
        self.checkpoint.verify() && self.commit_id == CommitId::from_hash(self.checkpoint.content_hash())
    }
}

// ---------------------------------------------------------------------------
// ModelRef — lightweight pointer to a commit
// ---------------------------------------------------------------------------

/// A lightweight reference to a model commit, optionally tagged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRef {
    pub commit_id: CommitId,
    pub model_id: ModelId,
    pub created_at: i64,
    pub tag: Option<String>,
}

impl ModelRef {
    pub fn from_commit(commit: &ModelCommit, tag: Option<String>) -> Self {
        ModelRef {
            commit_id: commit.commit_id.clone(),
            model_id: commit.model_id.clone(),
            created_at: commit.created_at,
            tag,
        }
    }

    pub fn new(
        commit_id: CommitId,
        model_id: ModelId,
        created_at: i64,
        tag: Option<String>,
    ) -> Self {
        ModelRef {
            commit_id,
            model_id,
            created_at,
            tag,
        }
    }
}

// ---------------------------------------------------------------------------
// LearningEvent — a batch of training samples
// ---------------------------------------------------------------------------

static EVENT_COUNTER: AtomicU64 = AtomicU64::new(1);

/// A learning event containing a batch of training samples to be applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningEvent {
    pub event_id: String,
    pub model_id: ModelId,
    pub samples: Vec<DatasetTrainingSample>,
    pub parent_commit: Option<CommitId>,
    pub result_commit: Option<CommitId>,
    pub created_at: i64,
    pub source: Option<String>,
}

impl LearningEvent {
    pub fn new(
        model_id: ModelId,
        samples: Vec<DatasetTrainingSample>,
        parent_commit: Option<CommitId>,
        source: Option<String>,
    ) -> Self {
        let event_id = format!("evt-{}", EVENT_COUNTER.fetch_add(1, Ordering::Relaxed));
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        LearningEvent {
            event_id,
            model_id,
            samples,
            parent_commit,
            result_commit: None,
            created_at,
            source,
        }
    }

    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// Whether this event has been applied (has a result commit).
    pub fn is_applied(&self) -> bool {
        self.result_commit.is_some()
    }

    /// Mark this event as applied with the given result commit.
    pub fn mark_applied(&mut self, commit_id: CommitId) {
        self.result_commit = Some(commit_id);
    }
}

// ---------------------------------------------------------------------------
// ReplayError — error type for replay operations
// ---------------------------------------------------------------------------

/// Errors that can occur during replay or checkpoint operations.
#[derive(Debug, Clone)]
pub enum ReplayError {
    /// A checkpoint referenced by commit ID was not found.
    CheckpointNotFound(CommitId),
    /// A model state is incompatible with the expected format.
    IncompatibleState { model: String, reason: String },
    /// Learning events are out of expected order.
    EventOutOfOrder { expected: u64, actual: u64 },
    /// An empty event list was provided.
    EmptyEventList,
    /// A model state checksum does not match.
    ChecksumMismatch { model: String },
    /// A tag with the given name already exists.
    TagAlreadyExists(String),
    /// The store has no head commit.
    NoHead,
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplayError::CheckpointNotFound(id) => {
                write!(f, "checkpoint not found: {}", id)
            }
            ReplayError::IncompatibleState { model, reason } => {
                write!(f, "incompatible state for model '{}': {}", model, reason)
            }
            ReplayError::EventOutOfOrder { expected, actual } => {
                write!(
                    f,
                    "event out of order: expected index {}, got {}",
                    expected, actual
                )
            }
            ReplayError::EmptyEventList => {
                write!(f, "empty event list")
            }
            ReplayError::ChecksumMismatch { model } => {
                write!(f, "checksum mismatch for model '{}'", model)
            }
            ReplayError::TagAlreadyExists(tag) => {
                write!(f, "tag already exists: '{}'", tag)
            }
            ReplayError::NoHead => {
                write!(f, "no head commit")
            }
        }
    }
}

impl std::error::Error for ReplayError {}

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
            success: self.success.save(),
            latency: self.latency.save(),
            ttft: self.ttft.save(),
            cost: self.cost.save(),
            feature_schema_version: super::features::FEATURE_SCHEMA_VERSION,
            created_at: now,
        }
    }

    /// Load an ensemble from a checkpoint.
    ///
    /// Validates that all model states can be deserialized. This is a
    /// constructor — `RoutingModel::load` is a static method.
    pub fn load_all(checkpoint: &ModelCheckpoint) -> Result<Self, ReplayError> {
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
        let ttft = TtftModel::load(&checkpoint.ttft).map_err(|e| {
            ReplayError::IncompatibleState {
                model: "ttft".to_string(),
                reason: e,
            }
        })?;
        let cost = CostModel::load(&checkpoint.cost).map_err(|e| {
            ReplayError::IncompatibleState {
                model: "cost".to_string(),
                reason: e,
            }
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

/// A git-like store for model checkpoints with commit history, tags, and HEAD.
pub struct ModelStore {
    commits: HashMap<CommitId, (ModelCheckpoint, CommitInfo)>,
    tags: HashMap<String, CommitId>,
    head: Option<CommitId>,
    next_seq: u64,
}

impl ModelStore {
    pub fn new() -> Self {
        ModelStore {
            commits: HashMap::new(),
            tags: HashMap::new(),
            head: None,
            next_seq: 1,
        }
    }

    /// Commit a new checkpoint with the given message.
    ///
    /// The parent is set to the current head. Advances head to the new commit.
    pub fn commit(
        &mut self,
        checkpoint: ModelCheckpoint,
        message: String,
    ) -> CommitId {
        let seq = self.next_seq;
        self.next_seq += 1;
        let commit_id = CommitId::new(format!("cmt-{:06}", seq));
        let parent = self.head.clone();
        let event_count = 0; // ModelStore doesn't track event counts
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let info = CommitInfo {
            commit_id: commit_id.clone(),
            parent: parent.clone(),
            message,
            learning_event_count: event_count,
            created_at: now,
        };

        self.commits.insert(commit_id.clone(), (checkpoint, info));
        self.head = Some(commit_id.clone());
        commit_id
    }

    /// Checkout a checkpoint by commit ID.
    pub fn checkout(&self, commit_id: &CommitId) -> Result<ModelCheckpoint, ReplayError> {
        self.commits
            .get(commit_id)
            .map(|(checkpoint, _)| checkpoint.clone())
            .ok_or_else(|| ReplayError::CheckpointNotFound(commit_id.clone()))
    }

    /// Get the current HEAD commit ID.
    pub fn get_head(&self) -> Option<CommitId> {
        self.head.clone()
    }

    /// Set the HEAD to a specific commit ID.
    pub fn set_head(&mut self, commit_id: CommitId) -> Result<(), ReplayError> {
        if !self.commits.contains_key(&commit_id) {
            return Err(ReplayError::CheckpointNotFound(commit_id));
        }
        self.head = Some(commit_id);
        Ok(())
    }

    /// Walk the parent chain from HEAD, returning commit info newest first.
    pub fn log(&self) -> Vec<CommitInfo> {
        let mut result = Vec::new();
        let mut current = self.head.clone();
        while let Some(cid) = current {
            if let Some((_, info)) = self.commits.get(&cid) {
                let next_parent = info.parent.clone();
                result.push(info.clone());
                current = next_parent;
            } else {
                break;
            }
        }
        result
    }

    /// Tag a commit with a name. Errors if the tag already exists.
    pub fn tag(
        &mut self,
        commit_id: &CommitId,
        tag_name: &str,
    ) -> Result<(), ReplayError> {
        if self.tags.contains_key(tag_name) {
            return Err(ReplayError::TagAlreadyExists(tag_name.to_string()));
        }
        if !self.commits.contains_key(commit_id) {
            return Err(ReplayError::CheckpointNotFound(commit_id.clone()));
        }
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

/// Stateless engine for replaying learning events against a base checkpoint.
pub struct ReplayEngine;

impl ReplayEngine {
    /// Replay a sequence of learning events starting from an optional base checkpoint.
    ///
    /// 1. Validates the event list is non-empty.
    /// 2. Verifies base checkpoint integrity if provided.
    /// 3. Loads an ensemble from the base checkpoint, or creates a fresh one.
    /// 4. Applies all samples from all events in order.
    /// 5. Returns the resulting checkpoint via `save_all()`.
    pub fn replay(
        events: &[LearningEvent],
        base: Option<&ModelCheckpoint>,
    ) -> Result<ModelCheckpoint, ReplayError> {
        if events.is_empty() {
            return Err(ReplayError::EmptyEventList);
        }

        // Verify base checkpoint integrity if provided
        if let Some(checkpoint) = base {
            if !checkpoint.verify() {
                return Err(ReplayError::ChecksumMismatch {
                    model: "base checkpoint".to_string(),
                });
            }
        }

        // Load ensemble from base or create fresh
        let mut ensemble = match base {
            Some(checkpoint) => ModelEnsemble::load_all(checkpoint)?,
            None => ModelEnsemble::new(),
        };

        // Apply all samples from all events in order
        for event in events {
            for sample in &event.samples {
                ensemble.update_all(sample);
            }
        }

        Ok(ensemble.save_all())
    }

    /// Verify checkpoint integrity (checksums on all 4 states).
    pub fn verify_checkpoint_integrity(checkpoint: &ModelCheckpoint) -> bool {
        checkpoint.verify()
    }
}

// ---------------------------------------------------------------------------
// Tests (stub — populated by test agents)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::dataset::{Targets, TrainingSample as DatasetTrainingSample};
    use crate::ml::features::{RoutingFeatures, FEATURE_SCHEMA_VERSION};
    use crate::feedback::DataOrigin;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn test_sample(success: bool, seed: usize) -> DatasetTrainingSample {
        let mut feats = [0.0f32; 32];
        for i in 0..32 {
            feats[i] = ((seed * 7 + i * 13) % 100) as f32 / 100.0;
        }
        DatasetTrainingSample {
            sample_id: format!("s-{}", seed),
            schema_version: 1,
            timestamp: 1000 + seed as i64,
            features: RoutingFeatures {
                values: feats,
                schema_version: 1,
            },
            targets: Targets {
                success,
                latency_ms: if success {
                    Some(200.0 + seed as f64)
                } else {
                    None
                },
                ttft_ms: if success {
                    Some(50.0 + seed as f64)
                } else {
                    None
                },
                cost: Some(0.01 + seed as f64 * 0.001),
                failure_class: None,
                fallback_count: 0,
            },
            provider_id: "p".into(),
            model_id: "m".into(),
            origin: DataOrigin::Native,
            outcome_id: format!("o-{}", seed),
            feedback: vec![],
        }
    }

    fn make_events(n: usize) -> Vec<LearningEvent> {
        (0..n)
            .map(|i| {
                LearningEvent::new(
                    ModelId::new("ensemble"),
                    vec![test_sample(i % 2 == 0, i)],
                    None,
                    Some("test".into()),
                )
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // ModelId tests (4)
    // -----------------------------------------------------------------------

    #[test]
    fn model_id_construction() {
        assert_eq!(ModelId::success().as_str(), "success");
        assert_eq!(ModelId::latency().as_str(), "latency");
        assert_eq!(ModelId::ttft().as_str(), "ttft");
        assert_eq!(ModelId::cost().as_str(), "cost");
        assert_eq!(ModelId::new("custom").as_str(), "custom");
    }

    #[test]
    fn model_id_display() {
        let id = ModelId::new("test_model");
        assert_eq!(format!("{}", id), "test_model");
        assert_eq!(format!("{}", id), id.as_str());
    }

    #[test]
    fn model_id_equality() {
        let a = ModelId::new("same");
        let b = ModelId::new("same");
        let c = ModelId::new("different");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn model_id_serde_round_trip() {
        let id = ModelId::new("serde_test");
        let json = serde_json::to_string(&id).unwrap();
        let restored: ModelId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, restored);
    }

    // -----------------------------------------------------------------------
    // CommitId tests (3)
    // -----------------------------------------------------------------------

    #[test]
    fn commit_id_from_hash_deterministic() {
        let a = CommitId::from_hash(42);
        let b = CommitId::from_hash(42);
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "000000000000002a");
    }

    #[test]
    fn commit_id_display() {
        let id = CommitId::from_hash(0xff);
        let s = format!("{}", id);
        assert_eq!(s, "00000000000000ff");
    }

    #[test]
    fn commit_id_serde_round_trip() {
        let id = CommitId::from_hash(12345);
        let json = serde_json::to_string(&id).unwrap();
        let restored: CommitId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, restored);
    }

    // -----------------------------------------------------------------------
    // ModelCheckpoint tests (5)
    // -----------------------------------------------------------------------

    #[test]
    fn checkpoint_creation_from_ensemble() {
        let ensemble = ModelEnsemble::new();
        let cp = ensemble.save_all();
        assert!(cp.verify());
        assert_eq!(cp.feature_schema_version, FEATURE_SCHEMA_VERSION);
    }

    #[test]
    fn checkpoint_content_hash_deterministic() {
        let ensemble = ModelEnsemble::new();
        let cp = ensemble.save_all();
        let h1 = cp.content_hash();
        let h2 = cp.content_hash();
        assert_eq!(h1, h2);
    }

    #[test]
    fn checkpoint_content_hash_differs() {
        let cp1 = ModelEnsemble::new().save_all();
        let mut ensemble2 = ModelEnsemble::new();
        for i in 0..10 {
            ensemble2.update_all(&test_sample(true, i));
        }
        let cp2 = ensemble2.save_all();
        assert_ne!(cp1.content_hash(), cp2.content_hash());
    }

    #[test]
    fn checkpoint_verify_passes() {
        let ensemble = ModelEnsemble::new();
        let cp = ensemble.save_all();
        assert!(cp.verify());
    }

    #[test]
    fn checkpoint_serde_round_trip() {
        let cp = ModelEnsemble::new().save_all();
        let json = serde_json::to_string(&cp).unwrap();
        let restored: ModelCheckpoint = serde_json::from_str(&json).unwrap();
        assert!(restored.verify());
        assert_eq!(cp.content_hash(), restored.content_hash());
    }

    // -----------------------------------------------------------------------
    // ModelCommit tests (4)
    // -----------------------------------------------------------------------

    #[test]
    fn commit_creation() {
        let cp = ModelEnsemble::new().save_all();
        let commit = ModelCommit::new(
            ModelId::new("test"),
            cp.clone(),
            None,
            0,
        );
        assert_eq!(commit.commit_id, CommitId::from_hash(cp.content_hash()));
        assert!(commit.verify());
    }

    #[test]
    fn commit_verify() {
        let mut ensemble = ModelEnsemble::new();
        for i in 0..5 {
            ensemble.update_all(&test_sample(true, i));
        }
        let cp = ensemble.save_all();
        let commit = ModelCommit::new(ModelId::new("test"), cp, None, 5);
        assert!(commit.verify());
    }

    #[test]
    fn commit_parent_chain() {
        let cp1 = ModelEnsemble::new().save_all();
        let c1 = ModelCommit::new(ModelId::new("test"), cp1, None, 0);

        let mut ensemble2 = ModelEnsemble::new();
        ensemble2.update_all(&test_sample(true, 0));
        let cp2 = ensemble2.save_all();
        let c2 = ModelCommit::new(
            ModelId::new("test"),
            cp2,
            Some(c1.commit_id.clone()),
            1,
        );

        assert_eq!(c2.parent, Some(c1.commit_id));
    }

    #[test]
    fn commit_different_checkpoints_different_ids() {
        let cp1 = ModelEnsemble::new().save_all();
        let mut ensemble2 = ModelEnsemble::new();
        ensemble2.update_all(&test_sample(true, 0));
        let cp2 = ensemble2.save_all();
        let c1 = ModelCommit::new(ModelId::new("m"), cp1, None, 0);
        let c2 = ModelCommit::new(ModelId::new("m"), cp2, None, 1);
        assert_ne!(c1.commit_id, c2.commit_id);
    }

    // -----------------------------------------------------------------------
    // ModelRef tests (2)
    // -----------------------------------------------------------------------

    #[test]
    fn model_ref_from_commit() {
        let cp = ModelEnsemble::new().save_all();
        let commit = ModelCommit::new(ModelId::new("test"), cp, None, 0);
        let r = ModelRef::from_commit(&commit, Some("v1".into()));
        assert_eq!(r.commit_id, commit.commit_id);
        assert_eq!(r.model_id, commit.model_id);
        assert_eq!(r.tag, Some("v1".to_string()));
    }

    #[test]
    fn model_ref_serde_round_trip() {
        let cp = ModelEnsemble::new().save_all();
        let commit = ModelCommit::new(ModelId::new("test"), cp, None, 0);
        let r = ModelRef::from_commit(&commit, None);
        let json = serde_json::to_string(&r).unwrap();
        let restored: ModelRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r.commit_id, restored.commit_id);
        assert_eq!(r.model_id, restored.model_id);
    }

    // -----------------------------------------------------------------------
    // LearningEvent tests (4)
    // -----------------------------------------------------------------------

    #[test]
    fn learning_event_creation() {
        let samples = vec![test_sample(true, 0), test_sample(false, 1)];
        let event = LearningEvent::new(
            ModelId::new("test"),
            samples.clone(),
            None,
            Some("test".into()),
        );
        assert_eq!(event.model_id.as_str(), "test");
        assert_eq!(event.samples.len(), 2);
        assert_eq!(event.source, Some("test".to_string()));
        assert!(event.result_commit.is_none());
    }

    #[test]
    fn learning_event_sample_count() {
        let event = LearningEvent::new(
            ModelId::new("test"),
            vec![test_sample(true, 0), test_sample(true, 1), test_sample(false, 2)],
            None,
            None,
        );
        assert_eq!(event.sample_count(), 3);
    }

    #[test]
    fn learning_event_is_applied() {
        let mut event = LearningEvent::new(
            ModelId::new("test"),
            vec![test_sample(true, 0)],
            None,
            None,
        );
        assert!(!event.is_applied());
        event.mark_applied(CommitId::from_hash(42));
        assert!(event.is_applied());
    }

    #[test]
    fn learning_event_ids_unique() {
        let e1 = LearningEvent::new(
            ModelId::new("test"),
            vec![test_sample(true, 0)],
            None,
            None,
        );
        let e2 = LearningEvent::new(
            ModelId::new("test"),
            vec![test_sample(true, 0)],
            None,
            None,
        );
        assert_ne!(e1.event_id, e2.event_id);
    }

    // -----------------------------------------------------------------------
    // ModelEnsemble tests (8)
    // -----------------------------------------------------------------------

    #[test]
    fn ensemble_new_cold_start() {
        let ensemble = ModelEnsemble::new();
        assert_eq!(ensemble.success.sample_count(), 0);
        assert_eq!(ensemble.latency.sample_count(), 0);
        assert_eq!(ensemble.ttft.sample_count(), 0);
        assert_eq!(ensemble.cost.sample_count(), 0);
    }

    #[test]
    fn ensemble_save_all_checkpoint() {
        let ensemble = ModelEnsemble::new();
        let cp = ensemble.save_all();
        assert!(cp.verify());
        assert_eq!(cp.feature_schema_version, FEATURE_SCHEMA_VERSION);
    }

    #[test]
    fn ensemble_load_all_round_trip() {
        let mut ensemble = ModelEnsemble::new();
        for i in 0..20 {
            ensemble.update_all(&test_sample(i % 2 == 0, i));
        }
        let cp = ensemble.save_all();
        let loaded = ModelEnsemble::load_all(&cp).unwrap();

        let features = &test_sample(true, 99).features;
        let p_orig = ensemble.success.predict(features);
        let p_loaded = loaded.success.predict(features);
        assert!(
            (p_orig.value - p_loaded.value).abs() < 1e-10,
            "success predictions differ: orig={}, loaded={}",
            p_orig.value,
            p_loaded.value
        );
    }

    #[test]
    fn ensemble_reset_all() {
        let mut ensemble = ModelEnsemble::new();
        for i in 0..10 {
            ensemble.update_all(&test_sample(true, i));
        }
        assert!(ensemble.success.sample_count() > 0);
        ensemble.reset_all();
        assert_eq!(ensemble.success.sample_count(), 0);
        assert_eq!(ensemble.latency.sample_count(), 0);
        assert_eq!(ensemble.ttft.sample_count(), 0);
        assert_eq!(ensemble.cost.sample_count(), 0);
    }

    #[test]
    fn ensemble_update_all_success() {
        let mut ensemble = ModelEnsemble::new();
        let sample = test_sample(true, 0);
        ensemble.update_all(&sample);
        assert_eq!(ensemble.success.sample_count(), 1);
        assert_eq!(ensemble.latency.sample_count(), 1);
        assert_eq!(ensemble.ttft.sample_count(), 1);
        assert_eq!(ensemble.cost.sample_count(), 1);
    }

    #[test]
    fn ensemble_update_all_failure() {
        let mut ensemble = ModelEnsemble::new();
        let sample = test_sample(false, 0); // failure: latency_ms=None, ttft_ms=None
        ensemble.update_all(&sample);
        assert_eq!(ensemble.success.sample_count(), 1); // success model always trained
        assert_eq!(ensemble.latency.sample_count(), 0); // latency not trained (None)
        assert_eq!(ensemble.ttft.sample_count(), 0); // ttft not trained (None)
        assert_eq!(ensemble.cost.sample_count(), 1); // cost trained (Some)
    }

    #[test]
    fn ensemble_update_all_partial() {
        let mut ensemble = ModelEnsemble::new();
        let mut sample = test_sample(true, 0);
        sample.targets.latency_ms = None; // latency_ms missing
        ensemble.update_all(&sample);
        assert_eq!(ensemble.success.sample_count(), 1);
        assert_eq!(ensemble.latency.sample_count(), 0); // not updated
        assert_eq!(ensemble.ttft.sample_count(), 1);
        assert_eq!(ensemble.cost.sample_count(), 1);
    }

    #[test]
    fn ensemble_double_save_idempotent() {
        let mut ensemble = ModelEnsemble::new();
        for i in 0..10 {
            ensemble.update_all(&test_sample(true, i));
        }
        let cp1 = ensemble.save_all();
        let cp2 = ensemble.save_all();
        assert_eq!(cp1.content_hash(), cp2.content_hash());
    }

    // -----------------------------------------------------------------------
    // ReplayEngine tests (12)
    // -----------------------------------------------------------------------

    #[test]
    fn replay_determinism_same_events() {
        let events = make_events(10);
        let cp1 = ReplayEngine::replay(&events, None).unwrap();
        let cp2 = ReplayEngine::replay(&events, None).unwrap();
        assert_eq!(cp1.content_hash(), cp2.content_hash());
        assert!(cp1.verify());
        assert!(cp2.verify());
    }

    #[test]
    fn replay_cold_start_no_base() {
        let events = make_events(3);
        let cp = ReplayEngine::replay(&events, None).unwrap();
        assert!(cp.verify());
        // Should have trained 3 samples
        let ensemble = ModelEnsemble::load_all(&cp).unwrap();
        assert_eq!(ensemble.success.sample_count(), 3);
    }

    #[test]
    fn replay_with_base_checkpoint() {
        let base_events = make_events(5);
        let base = ReplayEngine::replay(&base_events, None).unwrap();

        let more_events = make_events(3);
        let cp = ReplayEngine::replay(&more_events, Some(&base)).unwrap();
        assert!(cp.verify());
        let ensemble = ModelEnsemble::load_all(&cp).unwrap();
        assert_eq!(ensemble.success.sample_count(), 8); // 5 + 3
    }

    #[test]
    fn replay_composition_property() {
        let events = make_events(10);

        // Full replay
        let full = ReplayEngine::replay(&events, None).unwrap();

        // Two-stage replay
        let stage1 = ReplayEngine::replay(&events[..5], None).unwrap();
        let stage2 = ReplayEngine::replay(&events[5..], Some(&stage1)).unwrap();

        assert_eq!(full.content_hash(), stage2.content_hash());
    }

    #[test]
    fn replay_empty_events_error() {
        let result = ReplayEngine::replay(&[], None);
        assert!(matches!(result, Err(ReplayError::EmptyEventList)));
    }

    #[test]
    fn replay_empty_events_with_base_error() {
        let base = ModelEnsemble::new().save_all();
        let result = ReplayEngine::replay(&[], Some(&base));
        assert!(matches!(result, Err(ReplayError::EmptyEventList)));
    }

    #[test]
    fn replay_preserves_sample_counts() {
        let events = make_events(7);
        let cp = ReplayEngine::replay(&events, None).unwrap();
        let ensemble = ModelEnsemble::load_all(&cp).unwrap();
        // Each event has 1 sample, 7 events, alternating success/failure
        // success model always updated => 7
        assert_eq!(ensemble.success.sample_count(), 7);
    }

    #[test]
    fn replay_checkpoint_schema_version() {
        let events = make_events(3);
        let cp = ReplayEngine::replay(&events, None).unwrap();
        assert_eq!(cp.feature_schema_version, FEATURE_SCHEMA_VERSION);
    }

    #[test]
    fn replay_checkpoint_verify() {
        let events = make_events(5);
        let cp = ReplayEngine::replay(&events, None).unwrap();
        assert!(cp.verify());
    }

    #[test]
    fn replay_event_order_matters() {
        let events_a = make_events(5);
        let mut events_b = events_a.clone();
        events_b.reverse();

        let cp_a = ReplayEngine::replay(&events_a, None).unwrap();
        let cp_b = ReplayEngine::replay(&events_b, None).unwrap();
        assert_ne!(cp_a.content_hash(), cp_b.content_hash());
    }

    #[test]
    fn replay_determinism_100_events() {
        let events = make_events(100);
        let cp1 = ReplayEngine::replay(&events, None).unwrap();
        let cp2 = ReplayEngine::replay(&events, None).unwrap();
        assert_eq!(cp1.content_hash(), cp2.content_hash());
        assert!(cp1.verify());
    }

    #[test]
    fn replay_with_corrupted_base_fails() {
        let mut base = ModelEnsemble::new().save_all();
        // Corrupt by modifying a parameter
        base.success.parameters[0] = 9999.0;
        let events = make_events(3);
        let result = ReplayEngine::replay(&events, Some(&base));
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // ModelStore tests (10)
    // -----------------------------------------------------------------------

    #[test]
    fn store_commit_returns_id() {
        let mut store = ModelStore::new();
        let cp = ModelEnsemble::new().save_all();
        let id = store.commit(cp, "first".into());
        assert!(!id.as_str().is_empty());
    }

    #[test]
    fn store_checkout_round_trip() {
        let mut store = ModelStore::new();
        let cp = ModelEnsemble::new().save_all();
        let id = store.commit(cp.clone(), "test".into());
        let checked_out = store.checkout(&id).unwrap();
        assert_eq!(cp.content_hash(), checked_out.content_hash());
    }

    #[test]
    fn store_checkout_not_found() {
        let store = ModelStore::new();
        let result = store.checkout(&CommitId::new("nonexistent"));
        assert!(matches!(result, Err(ReplayError::CheckpointNotFound(_))));
    }

    #[test]
    fn store_head_initially_none() {
        let store = ModelStore::new();
        assert!(store.get_head().is_none());
    }

    #[test]
    fn store_head_after_commit() {
        let mut store = ModelStore::new();
        let cp = ModelEnsemble::new().save_all();
        let id = store.commit(cp, "first".into());
        assert_eq!(store.get_head(), Some(id));
    }

    #[test]
    fn store_head_updates_on_subsequent_commits() {
        let mut store = ModelStore::new();
        let cp1 = ModelEnsemble::new().save_all();
        let id1 = store.commit(cp1, "first".into());

        let mut ens2 = ModelEnsemble::new();
        ens2.update_all(&test_sample(true, 0));
        let cp2 = ens2.save_all();
        let id2 = store.commit(cp2, "second".into());

        assert_eq!(store.get_head(), Some(id2));
        assert_ne!(store.get_head(), Some(id1));
    }

    #[test]
    fn store_log_walks_parents() {
        let mut store = ModelStore::new();
        for i in 0..3 {
            let cp = ModelEnsemble::new().save_all();
            store.commit(cp, format!("commit-{}", i));
        }
        let log = store.log();
        assert_eq!(log.len(), 3);
        // Newest first
        assert_eq!(log[0].message, "commit-2");
        assert_eq!(log[1].message, "commit-1");
        assert_eq!(log[2].message, "commit-0");
        // Parent chain
        assert!(log[0].parent.is_some());
        assert!(log[1].parent.is_some());
        assert!(log[2].parent.is_none()); // first commit has no parent
    }

    #[test]
    fn store_log_from_empty() {
        let store = ModelStore::new();
        let log = store.log();
        assert!(log.is_empty());
    }

    #[test]
    fn store_tag_and_resolve() {
        let mut store = ModelStore::new();
        let cp = ModelEnsemble::new().save_all();
        let id = store.commit(cp, "test".into());
        store.tag(&id, "v1").unwrap();
        let resolved = store.resolve_tag("v1");
        assert_eq!(resolved, Some(&id));
    }

    #[test]
    fn store_tag_duplicate_error() {
        let mut store = ModelStore::new();
        let cp = ModelEnsemble::new().save_all();
        let id = store.commit(cp, "test".into());
        store.tag(&id, "v1").unwrap();
        let result = store.tag(&id, "v1");
        assert!(matches!(result, Err(ReplayError::TagAlreadyExists(_))));
    }
}
