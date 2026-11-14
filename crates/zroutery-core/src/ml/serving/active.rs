use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::{ACTIVE_MODEL_AUDIT_FILE, ACTIVE_MODEL_FILE, ACTIVE_MODEL_SCHEMA_VERSION};

use crate::ml::features::RoutingFeatures;
use crate::ml::learning::predict_bundle;
use crate::ml::model_identity::{CommitId, ModelCheckpoint, ModelEnsemble, ModelId};
use crate::ml::shadow::EnsemblePredictor;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ServingError {
    #[error("active model io failed at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the active model document is unreadable: {0}")]
    Corrupt(String),
    #[error("there is no active model to operate on")]
    None,
    #[error(
        "commit {commit_id} was not promoted: the gate returned {verdict}{}",
        if blockers.is_empty() {
            String::new()
        } else {
            format!("\n  unmet criteria:\n    - {}", blockers.join("\n    - "))
        }
    )]
    NotPromoted {
        commit_id: String,
        verdict: crate::ml::promotion::PromotionVerdict,
        blockers: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// ActiveModel
// ---------------------------------------------------------------------------

/// The model currently permitted to rank routing decisions.
///
/// Not `PartialEq`: [`ModelCheckpoint`] deliberately does not implement it, and
/// two active models are compared by their commit ids, which are content
/// addresses and mean exactly that.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveModel {
    pub schema_version: u32,
    pub model_id: String,
    /// The commit the checkpoint verifies against.
    pub commit_id: String,
    /// Learning events the commit records.
    ///
    /// Carried because it is part of the identity. A commit id is a hash *over*
    /// its inputs, so re-deriving one needs the count; without this field the
    /// serving path could only ever reconstruct a commit for a model trained
    /// zero times, which is not a model.
    pub learning_event_count: u64,
    pub checkpoint: ModelCheckpoint,
    /// The gate decision that authorised this model, stored whole.
    ///
    /// Whole rather than as a digest, because the point of recording the
    /// decision is that it can be read back: which gate configuration was in
    /// force, which criteria held, what the evidence was, and when. A digest
    /// would prove the decision has not changed and tell nobody what it said.
    pub promotion: crate::ml::promotion::PromotionDecision,
    /// Unix seconds when this model was promoted.
    pub promoted_at: i64,
}

/// A verified, loadable predictor over one active model.
pub struct ActivePredictor {
    pub(super) ensemble: ModelEnsemble,
    pub(super) commit_id: CommitId,
    pub(super) model_id: String,
}

impl std::fmt::Debug for ActivePredictor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivePredictor")
            .field("model_id", &self.model_id)
            .field("commit_id", &self.commit_id.as_str())
            .finish_non_exhaustive()
    }
}

impl ActivePredictor {
    /// Load a predictor from a checkpoint, refusing anything that does not
    /// verify.
    ///
    /// The commit id is *re-derived* from the checkpoint rather than trusted, so
    /// a document whose commit field was edited away from its content is
    /// refused instead of serving under a borrowed identity.
    pub fn load(active: &ActiveModel) -> Result<Self, ServingError> {
        if active.schema_version != ACTIVE_MODEL_SCHEMA_VERSION {
            return Err(ServingError::Corrupt(format!(
                "active model schema {} is not supported",
                active.schema_version
            )));
        }
        let ensemble = ModelEnsemble::load_all(&active.checkpoint)
            .map_err(|error| ServingError::Corrupt(error.to_string()))?;
        for state in [
            &active.checkpoint.success,
            &active.checkpoint.latency,
            &active.checkpoint.ttft,
            &active.checkpoint.cost,
        ] {
            state.validate_basics().map_err(ServingError::Corrupt)?;
        }
        let derived = crate::ml::model_identity::ModelCommit::new(
            ModelId::new(active.model_id.clone()),
            active.checkpoint.clone(),
            None,
            active.learning_event_count,
        );
        if derived.commit_id.as_str() != active.commit_id {
            return Err(ServingError::Corrupt(format!(
                "active model claims commit {} but its content derives {}",
                active.commit_id, derived.commit_id
            )));
        }
        Ok(Self {
            ensemble,
            commit_id: derived.commit_id,
            model_id: active.model_id.clone(),
        })
    }

    pub fn commit_id(&self) -> &CommitId {
        &self.commit_id
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn ensemble(&self) -> &ModelEnsemble {
        &self.ensemble
    }
}

impl EnsemblePredictor for ActivePredictor {
    fn predict(
        &self,
        model: &str,
        provider: &str,
        features: &RoutingFeatures,
    ) -> crate::ml::reward::PredictionBundle {
        predict_bundle(&self.ensemble, model, provider, features)
    }

    fn commit_id(&self) -> CommitId {
        self.commit_id.clone()
    }
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

/// What happened to the active model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveModelAction {
    Promote,
    Rollback,
}

impl ActiveModelAction {
    pub fn as_str(self) -> &'static str {
        match self {
            ActiveModelAction::Promote => "promote",
            ActiveModelAction::Rollback => "rollback",
        }
    }
}

/// One durable entry in the promotion history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveModelAuditEntry {
    pub schema_version: u32,
    pub action: ActiveModelAction,
    pub commit_id: String,
    /// Identity of the gate configuration that authorised the promotion.
    pub gate_identity: String,
    /// The verdict at the time. Retained so the history can be read without
    /// reopening every stored decision.
    pub verdict: crate::ml::promotion::PromotionVerdict,
    /// Unix seconds.
    pub at: i64,
    /// Free-form note.
    pub note: String,
}

// ---------------------------------------------------------------------------
// ActiveModelStore
// ---------------------------------------------------------------------------

/// The durable pointer to the model permitted to rank decisions.
///
/// The pointer holds the previous pointer inline, so a rollback is a write of a
/// document this store already knows how to produce and needs no second
/// mechanism behind it.
pub struct ActiveModelStore {
    dir: PathBuf,
    promotions: AtomicU64,
    rollbacks: AtomicU64,
    refusals: AtomicU64,
}

impl std::fmt::Debug for ActiveModelStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActiveModelStore")
            .field("dir", &self.dir)
            .field("promotions", &self.promotions.load(Ordering::Relaxed))
            .field("rollbacks", &self.rollbacks.load(Ordering::Relaxed))
            .field("refusals", &self.refusals.load(Ordering::Relaxed))
            .finish()
    }
}

impl ActiveModelStore {
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, ServingError> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir).map_err(|source| ServingError::Io {
            path: dir.display().to_string(),
            source,
        })?;
        Ok(Self {
            dir: dir.to_path_buf(),
            promotions: AtomicU64::new(0),
            rollbacks: AtomicU64::new(0),
            refusals: AtomicU64::new(0),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn counts(&self) -> (u64, u64, u64) {
        (
            self.promotions.load(Ordering::Relaxed),
            self.rollbacks.load(Ordering::Relaxed),
            self.refusals.load(Ordering::Relaxed),
        )
    }

    fn pointer_path(&self) -> PathBuf {
        self.dir.join(ACTIVE_MODEL_FILE)
    }

    fn audit_path(&self) -> PathBuf {
        self.dir.join(ACTIVE_MODEL_AUDIT_FILE)
    }

    /// The stored pointer, without verifying it.
    ///
    /// Public because an operator surface has to be able to report what is
    /// installed without installing it again, and a pointer that only promote
    /// and rollback can reach would mean reading the state requires changing it.
    pub fn read_pointer(&self) -> Result<Option<StoredPointer>, ServingError> {
        let path = self.pointer_path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ServingError::Io {
                    path: path.display().to_string(),
                    source,
                })
            }
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| ServingError::Corrupt(error.to_string()))
    }

    /// The active model, if one is stored and it verifies.
    ///
    /// A stored-but-unverifiable model is a refusal, not a silent fallback to
    /// genesis: promoting nothing because the previous model was corrupted is
    /// exactly the state an operator needs to be told about.
    pub fn active(&self) -> Result<Option<ActivePredictor>, ServingError> {
        let Some(pointer) = self.read_pointer()? else {
            return Ok(None);
        };
        match ActivePredictor::load(&pointer.current) {
            Ok(predictor) => Ok(Some(predictor)),
            Err(_) => {
                self.refusals.fetch_add(1, Ordering::Relaxed);
                Ok(None)
            }
        }
    }

    /// The active model's identity, if one is stored.
    pub fn active_identity(&self) -> Result<Option<String>, ServingError> {
        Ok(self
            .read_pointer()?
            .map(|pointer| pointer.current.commit_id))
    }

    /// Promote a model, on the strength of a gate decision that said so.
    ///
    /// The decision is a required argument rather than a note in a string,
    /// because the whole point of this change is that a decision which said
    /// REJECTED or BLOCKED cannot install a model. The caller previously passed
    /// a free-text `promotion_digest` and a `note`, which meant the store
    /// recorded whatever the caller claimed had authorised the promotion — so
    /// "training succeeded = automatically live" was reachable by writing one
    /// string differently.
    ///
    /// The model is not constructed here from the decision alone; the checkpoint
    /// is passed too and its commit id is **re-derived** from the decision's own
    /// inputs. A decision naming commit X cannot install a checkpoint for Y even
    /// if the caller passes Y's bytes, which is what stops a stale decision from
    /// authorising a model it never saw.
    ///
    /// The write is atomic: a temp file plus a rename, so a crash leaves either
    /// the old pointer or the new one and never a torn document.
    pub fn promote(
        &self,
        decision: &crate::ml::promotion::PromotionDecision,
        checkpoint: ModelCheckpoint,
    ) -> Result<CommitId, ServingError> {
        use crate::ml::promotion::PromotionVerdict;

        if decision.verdict != PromotionVerdict::Promoted {
            return Err(ServingError::NotPromoted {
                commit_id: decision.candidate_commit.clone(),
                verdict: decision.verdict,
                blockers: decision
                    .blockers()
                    .into_iter()
                    .map(|criterion| format!("{}: {}", criterion.name, criterion.reason))
                    .collect(),
            });
        }

        let derived = crate::ml::model_identity::ModelCommit::new(
            ModelId::new(decision.model_id.clone()),
            checkpoint.clone(),
            None,
            decision.learning_event_count,
        );
        if derived.commit_id.as_str() != decision.candidate_commit {
            return Err(ServingError::Corrupt(format!(
                "the gate decision authorised commit {} but the supplied checkpoint derives {}",
                decision.candidate_commit, derived.commit_id
            )));
        }
        if !derived.verify() {
            return Err(ServingError::Corrupt(format!(
                "commit {} does not verify",
                decision.candidate_commit
            )));
        }

        // The model is constructed from the decision and then loaded, which
        // re-validates every parameter as finite. A checkpoint that verifies its
        // own identity can still be unusable as a predictor.
        let active = ActiveModel {
            schema_version: ACTIVE_MODEL_SCHEMA_VERSION,
            model_id: decision.model_id.clone(),
            commit_id: decision.candidate_commit.clone(),
            learning_event_count: decision.learning_event_count,
            checkpoint,
            promotion: decision.clone(),
            promoted_at: now_seconds(),
        };
        ActivePredictor::load(&active).map_err(|error| ServingError::Corrupt(error.to_string()))?;

        // The model being replaced is the *current* pointer, not its own
        // previous. Reading the wrong one leaves the second promotion with no
        // rollback target, which makes rollback a silent no-op in production
        // while every unit test around it passes.
        let previous = self.read_pointer()?.map(|pointer| pointer.current);
        let pointer = StoredPointer {
            schema_version: ACTIVE_MODEL_SCHEMA_VERSION,
            current: active,
            previous,
        };
        self.write_pointer(&pointer)?;
        self.append_audit(ActiveModelAuditEntry {
            schema_version: ACTIVE_MODEL_SCHEMA_VERSION,
            action: ActiveModelAction::Promote,
            commit_id: pointer.current.commit_id.clone(),
            gate_identity: decision.gate_config_identity.clone(),
            verdict: decision.verdict,
            at: pointer.current.promoted_at,
            note: format!(
                "gate held on {} of {} criteria over body {}",
                decision.criteria.iter().filter(|c| c.held).count(),
                decision.criteria.len(),
                decision.dataset_fingerprint
            ),
        })?;
        self.promotions.fetch_add(1, Ordering::Relaxed);
        Ok(derived.commit_id)
    }

    /// Restore the previous model.
    ///
    /// Returns `false` when there is nothing to roll back to, which is the state
    /// a first promotion leaves behind. A rollback is itself audited, so the
    /// history reads forward rather than requiring an inference from the
    /// absence of a later promotion.
    pub fn rollback(&self) -> Result<bool, ServingError> {
        let Some(pointer) = self.read_pointer()? else {
            return Ok(false);
        };
        let Some(previous) = pointer.previous else {
            return Ok(false);
        };
        let rolled_back = ActivePredictor::load(&previous)
            .map(|_| true)
            .unwrap_or(false);
        if !rolled_back {
            self.refusals.fetch_add(1, Ordering::Relaxed);
            return Ok(false);
        }
        let now = now_seconds();
        let restored = StoredPointer {
            schema_version: ACTIVE_MODEL_SCHEMA_VERSION,
            current: previous,
            // Rollback does not stack: rolling back twice returns to the same
            // place rather than walking further back than the store knows about.
            previous: None,
        };
        self.write_pointer(&restored)?;
        self.append_audit(ActiveModelAuditEntry {
            schema_version: ACTIVE_MODEL_SCHEMA_VERSION,
            action: ActiveModelAction::Rollback,
            commit_id: restored.current.commit_id.clone(),
            gate_identity: restored.current.promotion.gate_config_identity.clone(),
            verdict: restored.current.promotion.verdict,
            at: now,
            note: format!(
                "restored the model promoted by commit {}",
                restored.current.promotion.candidate_commit
            ),
        })?;
        self.rollbacks.fetch_add(1, Ordering::Relaxed);
        Ok(true)
    }

    /// The whole promotion history, oldest first.
    pub fn audit(&self) -> Result<Vec<ActiveModelAuditEntry>, ServingError> {
        let path = self.audit_path();
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(ServingError::Io {
                    path: path.display().to_string(),
                    source,
                })
            }
        };
        let mut entries = Vec::new();
        for (index, line) in BufReader::new(file).lines().enumerate() {
            let line = line.map_err(|source| ServingError::Io {
                path: path.display().to_string(),
                source,
            })?;
            if line.trim().is_empty() {
                continue;
            }
            entries.push(serde_json::from_str(&line).map_err(|error| {
                ServingError::Corrupt(format!("audit line {}: {error}", index + 1))
            })?);
        }
        Ok(entries)
    }

    pub(super) fn write_pointer(&self, pointer: &StoredPointer) -> Result<(), ServingError> {
        let path = self.pointer_path();
        let temporary = path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(pointer)
            .map_err(|error| ServingError::Corrupt(error.to_string()))?;
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temporary)
                .map_err(|source| ServingError::Io {
                    path: temporary.display().to_string(),
                    source,
                })?;
            file.write_all(&body).map_err(|source| ServingError::Io {
                path: temporary.display().to_string(),
                source,
            })?;
            file.sync_all().map_err(|source| ServingError::Io {
                path: temporary.display().to_string(),
                source,
            })?;
        }
        fs::rename(&temporary, &path).map_err(|source| ServingError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Ok(())
    }

    fn append_audit(&self, entry: ActiveModelAuditEntry) -> Result<(), ServingError> {
        let path = self.audit_path();
        let mut line = serde_json::to_string(&entry)
            .map_err(|error| ServingError::Corrupt(error.to_string()))?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| ServingError::Io {
                path: path.display().to_string(),
                source,
            })?;
        file.write_all(line.as_bytes())
            .map_err(|source| ServingError::Io {
                path: path.display().to_string(),
                source,
            })?;
        file.flush().map_err(|source| ServingError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Ok(())
    }
}

/// The on-disk pointer: the active model plus the one it replaced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredPointer {
    pub schema_version: u32,
    pub current: ActiveModel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<ActiveModel>,
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}
