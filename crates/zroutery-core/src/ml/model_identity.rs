//! Model identity, versioning, checkpoints, and replay for ML routing.
//!
//! Provides content-addressed model commits, checkpoint management,
//! learning event tracking, and deterministic replay of training history.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::Error as SerdeError;
use serde::{Deserialize, Deserializer, Serialize};

use super::dataset::{validate_sample, TrainingSample as DatasetTrainingSample};
use super::features::{FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use super::model::{CostModel, LatencyModel, ModelState, RoutingModel, SuccessModel, TtftModel};

// ---------------------------------------------------------------------------
// Versioned identity and deterministic hashing
// ---------------------------------------------------------------------------

/// Schema version of the model-state serialization consumed by this module.
pub const MODEL_STATE_SCHEMA_VERSION: u32 = 1;
/// Schema version of the checkpoint envelope consumed by this module.
pub const MODEL_CHECKPOINT_SCHEMA_VERSION: u32 = 1;
/// Schema version of the commit envelope consumed by this module.
pub const MODEL_COMMIT_SCHEMA_VERSION: u32 = 1;
/// Schema version of the learning-event envelope consumed by this module.
pub const LEARNING_EVENT_SCHEMA_VERSION: u32 = 1;
/// Marker used only for artifacts serialized before envelope versioning.
pub const LEGACY_UNVERSIONED_SCHEMA_VERSION: u32 = 0;

fn legacy_envelope_version() -> u32 {
    LEGACY_UNVERSIONED_SCHEMA_VERSION
}

fn deserialize_supported_envelope_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version == LEGACY_UNVERSIONED_SCHEMA_VERSION
        || version == MODEL_CHECKPOINT_SCHEMA_VERSION
        || version == MODEL_COMMIT_SCHEMA_VERSION
    {
        Ok(version)
    } else {
        Err(SerdeError::custom(format!(
            "unsupported model envelope schema version {version}"
        )))
    }
}

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
const COMMIT_ID_DOMAIN: &[u8] = b"zroutery-model-commit-v1\0";

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn hash_u64(hash: &mut u64, value: u64) {
    hash_bytes(hash, &value.to_le_bytes());
}

fn hash_string(hash: &mut u64, value: &str) {
    hash_u64(hash, value.len() as u64);
    hash_bytes(hash, value.as_bytes());
}

fn hash_optional_commit(hash: &mut u64, value: Option<&CommitId>) {
    match value {
        Some(commit) => {
            hash_bytes(hash, &[1]);
            hash_string(hash, commit.as_str());
        }
        None => hash_bytes(hash, &[0]),
    }
}

fn hash_algorithm_versions(hash: &mut u64, versions: &[(String, String)]) {
    hash_u64(hash, versions.len() as u64);
    let mut ordered: Vec<&(String, String)> = versions.iter().collect();
    ordered.sort();
    for (model, version) in ordered {
        hash_string(hash, model);
        hash_string(hash, version);
    }
}

fn hash_metadata(hash: &mut u64, metadata: &HashMap<String, String>) {
    hash_u64(hash, metadata.len() as u64);
    let mut ordered: Vec<(&String, &String)> = metadata.iter().collect();
    ordered.sort();
    for (key, value) in ordered {
        hash_string(hash, key);
        hash_string(hash, value);
    }
}

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
    /// Serialized checkpoint envelope version. Missing legacy fields are
    /// represented as version 0 and require explicit migration.
    #[serde(
        default = "legacy_envelope_version",
        deserialize_with = "deserialize_supported_envelope_version"
    )]
    pub schema_version: u32,
    pub success: ModelState,
    pub latency: ModelState,
    pub ttft: ModelState,
    pub cost: ModelState,
    pub feature_schema_version: u32,
    pub created_at: i64,
}

impl ModelCheckpoint {
    /// Compute a deterministic content hash over the complete model state.
    ///
    /// The wall-clock `created_at` field is deliberately excluded: it is
    /// persistence metadata, not model identity. Every field that can change
    /// predictions is included, including raw parameters as well as their
    /// checksums, so a validly re-signed but different checkpoint cannot share
    /// an identity by accident.
    pub fn content_hash(&self) -> u64 {
        let mut hash = FNV_OFFSET_BASIS;
        hash_bytes(&mut hash, b"zroutery-model-checkpoint-v1\0");
        hash_u64(&mut hash, self.schema_version as u64);
        hash_u64(&mut hash, self.feature_schema_version as u64);

        for state in [&self.success, &self.latency, &self.ttft, &self.cost] {
            hash_u64(&mut hash, state.schema_version as u64);
            hash_string(&mut hash, &state.algorithm);
            hash_u64(&mut hash, state.update_count);
            hash_u64(&mut hash, state.checksum);
            hash_u64(&mut hash, state.parameters.len() as u64);
            for parameter in &state.parameters {
                hash_u64(&mut hash, parameter.to_bits());
            }
        }
        hash
    }

    /// Validate the checkpoint envelope and all four serialized model states.
    ///
    /// Unknown schema versions are rejected rather than being interpreted as
    /// the current representation. This is the fail-closed boundary used by
    /// stores, replay, and predictor construction.
    pub fn validate(&self) -> Result<(), ReplayError> {
        if self.schema_version != MODEL_CHECKPOINT_SCHEMA_VERSION {
            return Err(ReplayError::UnsupportedSchema {
                component: "checkpoint envelope".to_string(),
                version: self.schema_version,
                supported: MODEL_CHECKPOINT_SCHEMA_VERSION,
            });
        }
        if self.feature_schema_version != FEATURE_SCHEMA_VERSION {
            return Err(ReplayError::UnsupportedSchema {
                component: "checkpoint feature schema".to_string(),
                version: self.feature_schema_version,
                supported: FEATURE_SCHEMA_VERSION,
            });
        }
        for (name, state) in [
            ("success", &self.success),
            ("latency", &self.latency),
            ("ttft", &self.ttft),
            ("cost", &self.cost),
        ] {
            if state.schema_version != MODEL_STATE_SCHEMA_VERSION {
                return Err(ReplayError::UnsupportedSchema {
                    component: format!("{name} model state"),
                    version: state.schema_version,
                    supported: MODEL_STATE_SCHEMA_VERSION,
                });
            }
            if !state.verify_checksum() {
                return Err(ReplayError::ChecksumMismatch {
                    model: name.to_string(),
                });
            }
            if state
                .parameters
                .iter()
                .any(|parameter| !parameter.is_finite())
            {
                return Err(ReplayError::IncompatibleState {
                    model: name.to_string(),
                    reason: "parameter is not finite".to_string(),
                });
            }
        }
        Ok(())
    }

    /// Verify the checkpoint and reject incompatible or corrupt state.
    pub fn verify(&self) -> bool {
        self.validate().is_ok() && ModelEnsemble::load_all(self).is_ok()
    }

    /// Explicitly migrate an unversioned checkpoint artifact.
    ///
    /// Legacy artifacts are never accepted by `validate`/`verify`; callers
    /// must opt into this operation after reviewing the payload. The migrated
    /// checkpoint keeps its model bytes and gains the current envelope marker.
    pub fn migrate_legacy(mut self) -> Result<Self, ReplayError> {
        if self.schema_version != LEGACY_UNVERSIONED_SCHEMA_VERSION {
            return Err(ReplayError::UnsupportedSchema {
                component: "checkpoint envelope".to_string(),
                version: self.schema_version,
                supported: MODEL_CHECKPOINT_SCHEMA_VERSION,
            });
        }
        self.schema_version = MODEL_CHECKPOINT_SCHEMA_VERSION;
        self.validate()?;
        if ModelEnsemble::load_all(&self).is_err() {
            return Err(ReplayError::IncompatibleState {
                model: "checkpoint".to_string(),
                reason: "legacy checkpoint could not be loaded after migration".to_string(),
            });
        }
        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// ModelCommit — immutable version of a model checkpoint
// ---------------------------------------------------------------------------

/// An immutable commit linking a checkpoint to its history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCommit {
    /// Serialized commit envelope version. Missing legacy fields are
    /// represented as version 0 and require explicit migration.
    #[serde(
        default = "legacy_envelope_version",
        deserialize_with = "deserialize_supported_envelope_version"
    )]
    pub schema_version: u32,
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

struct CommitIdentityInput<'a> {
    schema_version: u32,
    model_id: &'a ModelId,
    parent: Option<&'a CommitId>,
    feature_schema_version: u32,
    learning_event_count: u64,
    checkpoint: &'a ModelCheckpoint,
    algorithm_versions: &'a [(String, String)],
    metadata: &'a HashMap<String, String>,
}

impl ModelCommit {
    /// Create a new commit from a checkpoint.
    ///
    /// The identity is canonical and content-addressed across the model id,
    /// feature schema, parent, cumulative learning lineage, algorithm versions,
    /// checkpoint content, and stable metadata. `created_at` is intentionally
    /// excluded so replaying the same ordered events at different wall-clock
    /// times produces the same commit id.
    pub fn new(
        model_id: ModelId,
        checkpoint: ModelCheckpoint,
        parent: Option<CommitId>,
        learning_event_count: u64,
    ) -> Self {
        Self::new_with_metadata(
            model_id,
            checkpoint,
            parent,
            learning_event_count,
            HashMap::new(),
        )
    }

    fn new_with_metadata(
        model_id: ModelId,
        checkpoint: ModelCheckpoint,
        parent: Option<CommitId>,
        learning_event_count: u64,
        metadata: HashMap<String, String>,
    ) -> Self {
        let algorithm_versions = Self::algorithm_versions(&checkpoint);
        let feature_schema_version = checkpoint.feature_schema_version;
        let mut commit = ModelCommit {
            // Filled below after every identity-bearing field is present.
            schema_version: MODEL_COMMIT_SCHEMA_VERSION,
            commit_id: CommitId::new(""),
            parent,
            model_id,
            checkpoint,
            learning_event_count,
            algorithm_versions,
            feature_schema_version,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            metadata,
        };
        commit.commit_id = commit.canonical_identity();
        commit
    }

    /// Fallible constructor for code that must never retain an invalid
    /// artifact. The infallible `new` constructor remains for source
    /// compatibility, while all operational boundaries use this method.
    pub fn try_new(
        model_id: ModelId,
        checkpoint: ModelCheckpoint,
        parent: Option<CommitId>,
        learning_event_count: u64,
    ) -> Result<Self, ReplayError> {
        let commit = Self::new(model_id, checkpoint, parent, learning_event_count);
        if commit.verify() {
            Ok(commit)
        } else {
            Err(ReplayError::InvalidCommit {
                commit_id: commit.commit_id,
                reason: "constructed commit failed identity or artifact validation".to_string(),
            })
        }
    }

    /// Construct a commit with a caller-supplied stable lineage token.
    ///
    /// The token is stored as reserved metadata so it is covered by the
    /// canonical identity and cannot be changed without invalidating the
    /// commit. The ordinary constructor uses an empty token and still binds
    /// the complete parent/count lineage.
    pub fn new_with_lineage(
        model_id: ModelId,
        checkpoint: ModelCheckpoint,
        parent: Option<CommitId>,
        learning_event_count: u64,
        lineage: u64,
    ) -> Self {
        let mut metadata = HashMap::new();
        metadata.insert("lineage".to_string(), format!("{lineage:016x}"));
        Self::new_with_metadata(model_id, checkpoint, parent, learning_event_count, metadata)
    }

    fn algorithm_versions(checkpoint: &ModelCheckpoint) -> Vec<(String, String)> {
        vec![
            ("success".to_string(), checkpoint.success.algorithm.clone()),
            ("latency".to_string(), checkpoint.latency.algorithm.clone()),
            ("ttft".to_string(), checkpoint.ttft.algorithm.clone()),
            ("cost".to_string(), checkpoint.cost.algorithm.clone()),
        ]
    }

    /// Hash the identity-bearing lineage (the parent reference and cumulative
    /// event count). A child therefore cannot share an identity with a root or
    /// with a different history length.
    pub fn lineage_hash(&self) -> u64 {
        let mut hash = FNV_OFFSET_BASIS;
        hash_bytes(&mut hash, b"zroutery-model-lineage-v1\0");
        hash_optional_commit(&mut hash, self.parent.as_ref());
        hash_u64(&mut hash, self.learning_event_count);
        if let Some(lineage) = self.metadata.get("lineage") {
            hash_string(&mut hash, lineage);
        }
        hash
    }

    /// Compute the canonical commit identity from all identity-bearing fields.
    pub fn canonical_identity(&self) -> CommitId {
        Self::canonical_id_for_input(CommitIdentityInput {
            schema_version: self.schema_version,
            model_id: &self.model_id,
            parent: self.parent.as_ref(),
            feature_schema_version: self.feature_schema_version,
            learning_event_count: self.learning_event_count,
            checkpoint: &self.checkpoint,
            algorithm_versions: &self.algorithm_versions,
            metadata: &self.metadata,
        })
    }

    /// Canonical identity function used by constructors and verification.
    ///
    /// This compatibility wrapper assumes the current commit envelope. A
    /// serialized commit's own [`Self::canonical_identity`] is the authority
    /// for legacy or migrated envelope versions.
    pub fn canonical_id_for(
        model_id: &ModelId,
        parent: Option<&CommitId>,
        feature_schema_version: u32,
        learning_event_count: u64,
        checkpoint: &ModelCheckpoint,
        algorithm_versions: &[(String, String)],
        metadata: &HashMap<String, String>,
    ) -> CommitId {
        Self::canonical_id_for_input(CommitIdentityInput {
            schema_version: MODEL_COMMIT_SCHEMA_VERSION,
            model_id,
            parent,
            feature_schema_version,
            learning_event_count,
            checkpoint,
            algorithm_versions,
            metadata,
        })
    }

    fn canonical_id_for_input(input: CommitIdentityInput<'_>) -> CommitId {
        let mut hash = FNV_OFFSET_BASIS;
        hash_bytes(&mut hash, COMMIT_ID_DOMAIN);
        hash_u64(&mut hash, input.schema_version as u64);
        hash_string(&mut hash, input.model_id.as_str());
        hash_u64(&mut hash, input.feature_schema_version as u64);
        hash_optional_commit(&mut hash, input.parent);

        let mut lineage = FNV_OFFSET_BASIS;
        hash_bytes(&mut lineage, b"zroutery-model-lineage-v1\0");
        hash_optional_commit(&mut lineage, input.parent);
        hash_u64(&mut lineage, input.learning_event_count);
        if let Some(token) = input.metadata.get("lineage") {
            hash_string(&mut lineage, token);
        }
        hash_u64(&mut hash, lineage);

        hash_u64(&mut hash, input.checkpoint.content_hash());
        hash_u64(&mut hash, input.learning_event_count);
        hash_algorithm_versions(&mut hash, input.algorithm_versions);
        hash_metadata(&mut hash, input.metadata);
        CommitId::from_hash(hash)
    }

    /// Verify the checkpoint, schema, algorithm table, and canonical identity.
    pub fn verify(&self) -> bool {
        if self.model_id.as_str().is_empty()
            || self.schema_version != MODEL_COMMIT_SCHEMA_VERSION
            || self.feature_schema_version != FEATURE_SCHEMA_VERSION
            || self.feature_schema_version != self.checkpoint.feature_schema_version
            || !self.checkpoint.verify()
        {
            return false;
        }
        if self.algorithm_versions != Self::algorithm_versions(&self.checkpoint) {
            return false;
        }
        self.commit_id == self.canonical_identity()
    }

    /// Explicitly migrate an unversioned commit and its checkpoint envelope.
    ///
    /// The commit id is recomputed because the envelope version is part of
    /// canonical identity. This operation is intentionally separate from
    /// deserialization and verification so legacy bytes are never silently
    /// reinterpreted as current artifacts.
    pub fn migrate_legacy(mut self) -> Result<Self, ReplayError> {
        if self.schema_version != LEGACY_UNVERSIONED_SCHEMA_VERSION {
            return Err(ReplayError::UnsupportedSchema {
                component: "commit envelope".to_string(),
                version: self.schema_version,
                supported: MODEL_COMMIT_SCHEMA_VERSION,
            });
        }
        if self.checkpoint.schema_version == LEGACY_UNVERSIONED_SCHEMA_VERSION {
            self.checkpoint = self.checkpoint.migrate_legacy()?;
        } else if self.checkpoint.schema_version != MODEL_CHECKPOINT_SCHEMA_VERSION {
            return Err(ReplayError::UnsupportedSchema {
                component: "checkpoint envelope".to_string(),
                version: self.checkpoint.schema_version,
                supported: MODEL_CHECKPOINT_SCHEMA_VERSION,
            });
        }
        self.schema_version = MODEL_COMMIT_SCHEMA_VERSION;
        self.commit_id = self.canonical_identity();
        if self.verify() {
            Ok(self)
        } else {
            Err(ReplayError::InvalidCommit {
                commit_id: self.commit_id,
                reason: "legacy commit could not be verified after migration".to_string(),
            })
        }
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

/// Validate the complete sample envelope used by replay, including target
/// domains that the feature-only dataset validator intentionally leaves to the
/// dataset store.
pub fn validate_replay_sample(sample: &DatasetTrainingSample) -> Result<(), String> {
    if sample.schema_version != FEATURE_SCHEMA_VERSION {
        return Err(format!(
            "sample schema version mismatch: {} vs {}",
            sample.schema_version, FEATURE_SCHEMA_VERSION
        ));
    }
    if sample.sample_id.is_empty() || sample.outcome_id.is_empty() {
        return Err("sample_id and outcome_id must not be empty".to_string());
    }
    if sample.provider_id.is_empty() || sample.model_id.is_empty() {
        return Err("provider_id and model_id must not be empty".to_string());
    }
    validate_sample(sample)?;
    for (name, value) in [
        ("latency_ms", sample.targets.latency_ms),
        ("ttft_ms", sample.targets.ttft_ms),
        ("cost", sample.targets.cost),
    ] {
        if let Some(value) = value {
            if !value.is_finite() || value < 0.0 {
                return Err(format!("invalid {name}: {value}"));
            }
        }
    }
    Ok(())
}

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

    /// Return the logical sequence encoded by the event id, when present.
    ///
    /// `LearningEvent::new` uses `evt-<sequence>` ids. The field remains a
    /// string for compatibility with pre-7E-1 callers, but replay refuses to
    /// reinterpret a decreasing or duplicate sequence as a new order.
    pub fn logical_sequence(&self) -> Option<u64> {
        self.event_id
            .strip_prefix("evt-")
            .and_then(|suffix| suffix.parse::<u64>().ok())
    }

    /// Stable hash of the ordered training payload. Volatile event id,
    /// timestamp, and result metadata are intentionally excluded.
    pub fn payload_hash(&self) -> u64 {
        let mut hash = FNV_OFFSET_BASIS;
        hash_bytes(&mut hash, b"zroutery-learning-event-payload-v1\0");
        hash_string(&mut hash, self.model_id.as_str());
        let serialized = serde_json::to_vec(&self.samples).unwrap_or_default();
        hash_u64(&mut hash, serialized.len() as u64);
        hash_bytes(&mut hash, &serialized);
        if let Some(source) = &self.source {
            hash_string(&mut hash, source);
        } else {
            hash_bytes(&mut hash, &[0]);
        }
        hash
    }

    /// Validate the event envelope and every training sample before replay.
    pub fn validate(&self) -> Result<(), ReplayError> {
        if self.event_id.is_empty() {
            return Err(ReplayError::InvalidEvent {
                event_id: self.event_id.clone(),
                reason: "event id must not be empty".to_string(),
            });
        }
        if self.model_id.as_str().is_empty() {
            return Err(ReplayError::InvalidEvent {
                event_id: self.event_id.clone(),
                reason: "model id must not be empty".to_string(),
            });
        }
        if self.samples.is_empty() {
            return Err(ReplayError::InvalidEvent {
                event_id: self.event_id.clone(),
                reason: "an event must contain at least one sample".to_string(),
            });
        }
        for (index, sample) in self.samples.iter().enumerate() {
            if let Err(reason) = validate_replay_sample(sample) {
                if sample.schema_version != FEATURE_SCHEMA_VERSION {
                    return Err(ReplayError::UnsupportedSchema {
                        component: format!("event {} sample {} schema", self.event_id, index),
                        version: sample.schema_version,
                        supported: FEATURE_SCHEMA_VERSION,
                    });
                }
                return Err(ReplayError::InvalidEvent {
                    event_id: self.event_id.clone(),
                    reason: format!("sample {index}: {reason}"),
                });
            }
        }
        Ok(())
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
    /// A serialized artifact or envelope is structurally invalid.
    InvalidCommit { commit_id: CommitId, reason: String },
    /// A checkpoint and the commit id supplied for it do not identify the
    /// same canonical commit.
    CommitMismatch {
        expected: CommitId,
        actual: CommitId,
    },
    /// An event is malformed or its sample payload cannot be replayed.
    InvalidEvent { event_id: String, reason: String },
    /// Events from different model lineages were mixed.
    EventModelMismatch { expected: ModelId, actual: ModelId },
    /// An event points at the wrong parent commit.
    EventParentMismatch {
        event_id: String,
        expected: Option<CommitId>,
        actual: Option<CommitId>,
    },
    /// An event points at the wrong result commit.
    EventResultMismatch {
        event_id: String,
        expected: CommitId,
        actual: CommitId,
    },
    /// A verified replay requires every event to carry its result.
    EventResultMissing { event_id: String },
    /// A parent chain is missing, cyclic, or otherwise unverifiable.
    LineageCorrupt { commit_id: CommitId, reason: String },
    /// A serialized schema is not supported by this implementation.
    UnsupportedSchema {
        component: String,
        version: u32,
        supported: u32,
    },
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
            ReplayError::EmptyEventList => write!(f, "empty event list"),
            ReplayError::ChecksumMismatch { model } => {
                write!(f, "checksum mismatch for model '{}'", model)
            }
            ReplayError::TagAlreadyExists(tag) => {
                write!(f, "tag already exists: '{}'", tag)
            }
            ReplayError::NoHead => write!(f, "no head commit"),
            ReplayError::InvalidCommit { commit_id, reason } => {
                write!(f, "invalid commit '{}': {}", commit_id, reason)
            }
            ReplayError::CommitMismatch { expected, actual } => {
                write!(
                    f,
                    "commit/checkpoint mismatch: checkpoint identifies '{}', got '{}'",
                    expected, actual
                )
            }
            ReplayError::InvalidEvent { event_id, reason } => {
                write!(f, "invalid learning event '{}': {}", event_id, reason)
            }
            ReplayError::EventModelMismatch { expected, actual } => {
                write!(
                    f,
                    "learning event model mismatch: expected '{}', got '{}'",
                    expected, actual
                )
            }
            ReplayError::EventParentMismatch {
                event_id,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "learning event '{}' parent mismatch: expected {:?}, got {:?}",
                    event_id, expected, actual
                )
            }
            ReplayError::EventResultMismatch {
                event_id,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "learning event '{}' result mismatch: expected '{}', got '{}'",
                    event_id, expected, actual
                )
            }
            ReplayError::EventResultMissing { event_id } => {
                write!(f, "learning event '{}' has no result commit", event_id)
            }
            ReplayError::LineageCorrupt { commit_id, reason } => {
                write!(f, "corrupt lineage at '{}': {}", commit_id, reason)
            }
            ReplayError::UnsupportedSchema {
                component,
                version,
                supported,
            } => {
                write!(
                    f,
                    "unsupported {} version {} (supported version {})",
                    component, version, supported
                )
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
            feature_schema_version: super::features::FEATURE_SCHEMA_VERSION,
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

// ---------------------------------------------------------------------------
// Tests (stub — populated by test agents)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::DataOrigin;
    use crate::ml::dataset::{Targets, TrainingSample as DatasetTrainingSample};
    use crate::ml::features::{RoutingFeatures, FEATURE_SCHEMA_VERSION};

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn test_sample(success: bool, seed: usize) -> DatasetTrainingSample {
        let mut feats = [0.0f32; 32];
        for (i, item) in feats.iter_mut().enumerate() {
            *item = ((seed * 7 + i * 13) % 100) as f32 / 100.0;
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
        let commit = ModelCommit::new(ModelId::new("test"), cp.clone(), None, 0);
        assert_eq!(commit.commit_id, commit.canonical_identity());
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
        let c2 = ModelCommit::new(ModelId::new("test"), cp2, Some(c1.commit_id.clone()), 1);

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
            vec![
                test_sample(true, 0),
                test_sample(true, 1),
                test_sample(false, 2),
            ],
            None,
            None,
        );
        assert_eq!(event.sample_count(), 3);
    }

    #[test]
    fn learning_event_is_applied() {
        let mut event =
            LearningEvent::new(ModelId::new("test"), vec![test_sample(true, 0)], None, None);
        assert!(!event.is_applied());
        event.mark_applied(CommitId::from_hash(42));
        assert!(event.is_applied());
    }

    #[test]
    fn learning_event_ids_unique() {
        let e1 = LearningEvent::new(ModelId::new("test"), vec![test_sample(true, 0)], None, None);
        let e2 = LearningEvent::new(ModelId::new("test"), vec![test_sample(true, 0)], None, None);
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

        let result = ReplayEngine::replay(&events_b, None);
        assert!(matches!(result, Err(ReplayError::EventOutOfOrder { .. })));
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
