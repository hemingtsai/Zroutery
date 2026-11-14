use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::event::ReplayError;
use super::identity::{
    deserialize_supported_envelope_version, hash_algorithm_versions, hash_bytes, hash_metadata,
    hash_optional_commit, hash_string, hash_u64, legacy_envelope_version, CommitId, ModelId,
    COMMIT_ID_DOMAIN, FNV_OFFSET_BASIS,
};
use super::store::ModelEnsemble;
use super::{
    LEGACY_UNVERSIONED_SCHEMA_VERSION, MODEL_CHECKPOINT_SCHEMA_VERSION,
    MODEL_COMMIT_SCHEMA_VERSION, MODEL_STATE_SCHEMA_VERSION,
};
use crate::ml::features::FEATURE_SCHEMA_VERSION;
use crate::ml::model::ModelState;

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

    pub(super) fn algorithm_versions(checkpoint: &ModelCheckpoint) -> Vec<(String, String)> {
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
