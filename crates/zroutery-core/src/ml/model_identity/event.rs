use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::identity::{hash_bytes, hash_string, hash_u64, CommitId, ModelId, FNV_OFFSET_BASIS};
use crate::ml::dataset::{validate_sample, TrainingSample as DatasetTrainingSample};
use crate::ml::features::FEATURE_SCHEMA_VERSION;

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
