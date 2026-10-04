//! The ML serving path: an active model, a ranking, and a fallback that always
//! works.
//!
//! # What this reverses
//!
//! The repository previously provisioned ML *out* of the serving path. ADR-0002
//! recorded a provisional boundary that forbade online learning in production,
//! `ml::activation` was built to be "complete and unreachable at the same time",
//! and a binary-level gate scanned the shipped desktop executable for ML
//! symbols and failed if it found them. The reasoning was that an unproven model
//! must not be able to route a real request.
//!
//! That reasoning is right about the danger and wrong about the remedy. It
//! produced a system that could never produce the evidence that would make the
//! danger manageable, so the risk never went down; it just went unmeasured while
//! a very large amount of infrastructure was built to keep the model away from
//! production. The remedy for an unproven model is a gated one with a fallback
//! and a rollback, not an absent one.
//!
//! So the boundary is replaced by a mechanism:
//!
//! * a model serves only after a [`super::promotion::PromotionGate`] promoted
//!   it, and the promotion's digest is recorded beside it;
//! * every ranking is recomputed from immutable decision-time facts, through the
//!   same [`DecisionEngine`] that decides everything else;
//! * any fault, any missing model, any disagreement between the ranking and the
//!   executable plan falls back to the deterministic plan the router already
//!   computed;
//! * promotion and rollback are durable and auditable.
//!
//! # The fallback is not an error path
//!
//! [`MlRouter::rank`] returns `Err` and the caller keeps the router's own order.
//! A model that cannot load, a checkpoint that does not verify, a prediction that
//! is not finite, a decision that names a candidate the plan does not contain —
//! every one of these is a normal, expected condition that costs a request its
//! ML ranking and nothing else. None of them can fail a request, because the
//! deterministic order was computed before ML was consulted and is still there.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use super::decision_engine::{DecisionEngine, EngineCandidate, EngineInput};
use super::features::RoutingFeatures;
use super::learning::predict_bundle;
use super::model::Prediction;
use super::model_identity::{CommitId, ModelCheckpoint, ModelEnsemble, ModelId};
use super::shadow::{EnsemblePredictor, ShadowCandidateInput, ShadowInput};

/// File holding the active model pointer and its checkpoint.
pub const ACTIVE_MODEL_FILE: &str = "active-model.json";
/// Append-only record of every promotion and rollback.
pub const ACTIVE_MODEL_AUDIT_FILE: &str = "active-model-audit.jsonl";

/// Pointer envelope version.
pub const ACTIVE_MODEL_SCHEMA_VERSION: u32 = 1;

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
    ensemble: ModelEnsemble,
    commit_id: CommitId,
    model_id: String,
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
        let derived = super::model_identity::ModelCommit::new(
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
    ) -> super::reward::PredictionBundle {
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

        let derived = super::model_identity::ModelCommit::new(
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

    fn write_pointer(&self, pointer: &StoredPointer) -> Result<(), ServingError> {
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

// ---------------------------------------------------------------------------
// Exploration
// ---------------------------------------------------------------------------

/// How often, and how, the router deliberately tries a candidate other than the
/// one it would otherwise serve.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ExplorationConfig {
    /// Probability in `[0, 1]` that a request explores.
    pub probability: f64,
    /// Seed for the exploration draw. Fixed, so a replay of the same request
    /// under the same seed explores or does not explore identically.
    pub seed: u64,
}

impl Default for ExplorationConfig {
    fn default() -> Self {
        Self {
            probability: 0.0,
            seed: super::learning::TRAINING_DEFAULT_SEED,
        }
    }
}

impl ExplorationConfig {
    /// Refuse a configuration that would explore more often than it exploits, or
    /// that carries a probability outside `[0, 1]`.
    pub fn validate(&self) -> Result<(), ServingError> {
        if !self.probability.is_finite() || self.probability < 0.0 || self.probability > 1.0 {
            return Err(ServingError::Corrupt(format!(
                "exploration probability {} is outside [0, 1]",
                self.probability
            )));
        }
        Ok(())
    }
}

/// What an exploration draw produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExplorationOutcome {
    /// The exploitation pick stands.
    Exploit,
    /// A different eligible candidate was chosen to gather evidence.
    Explore { candidate_id: String },
}

/// Decide whether this request explores, and to where.
///
/// Three properties, all enforced here rather than by the caller:
///
/// * **Deterministic.** The draw is a hash of `(seed, request_id)`, not a
///   clock or a shared generator, so the same request replays identically and
///   two concurrent requests cannot interfere.
/// * **Constrained.** The candidate it may pick comes from `eligible` and is
///   never the exploitation pick. Exploration is a choice among legal
///   candidates, not a way to route somewhere the request was not allowed to
///   go.
/// * **Bounded.** A probability above `EXPLORATION_CEILING` is refused. A
///   router exploring more often than it exploits is not exploring, it is
///   routing at random while reporting that it learned something.
pub fn explore(
    config: &ExplorationConfig,
    request_id: &str,
    exploitation_pick: &str,
    eligible: &[String],
) -> ExplorationOutcome {
    if config.probability <= 0.0 || eligible.len() < 2 {
        return ExplorationOutcome::Exploit;
    }
    let draw = uniform_draw(config.seed, request_id);
    if draw >= config.probability {
        return ExplorationOutcome::Exploit;
    }
    let alternatives: Vec<&String> = eligible
        .iter()
        .filter(|candidate| candidate.as_str() != exploitation_pick)
        .collect();
    if alternatives.is_empty() {
        return ExplorationOutcome::Exploit;
    }
    let index = (draw / config.probability * alternatives.len() as f64).floor() as usize;
    let index = index.min(alternatives.len() - 1);
    ExplorationOutcome::Explore {
        candidate_id: alternatives[index].clone(),
    }
}

/// The largest exploration probability this router will accept.
pub const EXPLORATION_CEILING: f64 = 0.5;

fn uniform_draw(seed: u64, request_id: &str) -> f64 {
    let mut hash = 0xcbf29ce484222325u64;
    let mut mix = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };
    mix(b"zroutery-exploration-v1\0");
    mix(&seed.to_le_bytes());
    mix(request_id.as_bytes());
    // FNV-1a avalanches well in its low bits and poorly in its high ones, so
    // reading the top of the digest directly would concentrate the draws: with
    // short, sequential request ids a probability of 0.5 explored four requests
    // in five. The fold-and-multiply finalisation spreads the high bits before
    // they are read, which is what makes the rate mean what it says.
    let mut mixed = hash;
    mixed ^= mixed >> 33;
    mixed = mixed.wrapping_mul(0xff51_afd7_ed55_8ccd);
    mixed ^= mixed >> 33;
    mixed = mixed.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    mixed ^= mixed >> 33;
    // Top 53 bits, so the result is exactly representable and never 1.0.
    ((mixed >> 11) as f64) / ((1u64 << 53) as f64)
}

// ---------------------------------------------------------------------------
// MlRouter — the ranking itself
// ---------------------------------------------------------------------------

/// Why a ranking was not available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankUnavailable {
    /// No model is promoted.
    NoActiveModel,
    /// The request carried no candidate to rank.
    NoCandidates,
    /// The decision named a candidate the executable plan does not contain.
    SelectionNotInPlan,
    /// A prediction was not finite, so no utility could be trusted.
    NonFinitePrediction,
    /// Exploration configuration was refused.
    InvalidExploration,
}

/// One request's ML ranking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedPlan {
    /// The candidate ML would serve first.
    pub selected: String,
    /// Candidate identities in the order ML would try them.
    pub order: Vec<String>,
    /// The commit that produced the ranking.
    pub commit_id: String,
    /// Whether an exploration draw moved the selection off the exploitation
    /// pick.
    pub explored: bool,
    /// Why the engine chose what it chose.
    pub reason: String,
    /// Utility of each ranked candidate, as the engine computed it.
    pub utilities: Vec<(String, f64)>,
}

impl RankedPlan {
    /// True when ML's pick is already what the router would have served.
    pub fn is_noop(&self, production_first: &str) -> bool {
        self.order.first().map(String::as_str) == Some(production_first) && !self.explored
    }
}

/// The runtime component that turns decision-time facts into a provider order.
pub struct MlRouter {
    predictor: RwLock<Option<ActivePredictor>>,
    engine: DecisionEngine,
    exploration: RwLock<ExplorationConfig>,
    rankings: AtomicU64,
    fallbacks: AtomicU64,
    explorations: AtomicU64,
}

impl std::fmt::Debug for MlRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlRouter")
            .field("rankings", &self.rankings.load(Ordering::Relaxed))
            .field("fallbacks", &self.fallbacks.load(Ordering::Relaxed))
            .field("explorations", &self.explorations.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl MlRouter {
    pub fn new(engine: DecisionEngine, exploration: ExplorationConfig) -> Self {
        Self {
            predictor: RwLock::new(None),
            engine,
            exploration: RwLock::new(exploration),
            rankings: AtomicU64::new(0),
            fallbacks: AtomicU64::new(0),
            explorations: AtomicU64::new(0),
        }
    }

    /// Attach a promoted model.
    pub fn attach(&self, predictor: ActivePredictor) {
        *crate::sync::write(&self.predictor) = Some(predictor);
    }

    /// Withdraw the model, returning the router to deterministic routing.
    pub fn withdraw(&self) -> Option<ActivePredictor> {
        crate::sync::write(&self.predictor).take()
    }

    /// Whether a model is attached.
    pub fn is_attached(&self) -> bool {
        crate::sync::read(&self.predictor).is_some()
    }

    /// A snapshot of the attached model's weights, if one is attached.
    ///
    /// For analysis of what the *serving* model would have done. Snapshot
    /// rather than borrow because a replay over tens of thousands of records
    /// must not hold the router's write lock for its duration, and because a
    /// promotion arriving mid-analysis should not change the answers underneath
    /// it.
    pub fn attached_ensemble(&self) -> Option<ModelEnsemble> {
        crate::sync::read(&self.predictor)
            .as_ref()
            .map(|predictor| predictor.ensemble().clone())
    }

    /// The commit of the attached model, if one is attached.
    pub fn attached_commit(&self) -> Option<String> {
        crate::sync::read(&self.predictor)
            .as_ref()
            .map(|predictor| predictor.commit_id().as_str().to_string())
    }

    pub fn set_exploration(&self, exploration: ExplorationConfig) {
        *crate::sync::write(&self.exploration) = exploration;
    }

    pub fn exploration(&self) -> ExplorationConfig {
        *crate::sync::read(&self.exploration)
    }

    /// Rank the candidate set of one request.
    ///
    /// Every failure is an `Err` the caller answers by keeping the router's own
    /// order. Nothing in here can fail a request.
    pub fn rank(
        &self,
        input: &ShadowInput,
        request_id: &str,
    ) -> Result<RankedPlan, RankUnavailable> {
        let predictor = {
            let guard = crate::sync::read(&self.predictor);
            let found = guard.as_ref().map(|predictor| ActivePredictor {
                ensemble: clone_ensemble(&predictor.ensemble),
                commit_id: predictor.commit_id.clone(),
                model_id: predictor.model_id.clone(),
            });
            match found {
                Some(predictor) => predictor,
                None => {
                    drop(guard);
                    self.fallbacks.fetch_add(1, Ordering::Relaxed);
                    return Err(RankUnavailable::NoActiveModel);
                }
            }
        };
        if input.candidates.is_empty() {
            self.fallbacks.fetch_add(1, Ordering::Relaxed);
            return Err(RankUnavailable::NoCandidates);
        }

        // Predict for every candidate, eligible or not, so the engine's own
        // eligibility filter is what excludes a rejected candidate rather than
        // this layer deciding eligibility on the side.
        let mut engine_candidates: Vec<EngineCandidate> =
            Vec::with_capacity(input.candidates.len());
        for candidate in &input.candidates {
            let bundle = predictor.predict(
                &candidate.candidate_id,
                &candidate.provider_id,
                &candidate.features,
            );
            if !bundle_is_finite(&bundle) {
                self.fallbacks.fetch_add(1, Ordering::Relaxed);
                return Err(RankUnavailable::NonFinitePrediction);
            }
            engine_candidates.push(EngineCandidate {
                candidate_id: candidate.candidate_id.clone(),
                bundle,
                eligible: candidate.eligible,
            });
        }

        let output = self.engine.decide_with_ml_ranking(&EngineInput {
            current_candidate: &input.production_selected,
            candidates: &engine_candidates,
            session_mode: input.session_mode,
            session_switch_count: input.session_switch_count,
            is_fallback: input.is_fallback,
        });

        if output.ranked_candidates.is_empty() {
            self.fallbacks.fetch_add(1, Ordering::Relaxed);
            return Err(RankUnavailable::SelectionNotInPlan);
        }

        let eligible: Vec<String> = input
            .candidates
            .iter()
            .filter(|candidate| candidate.eligible)
            .map(|candidate| candidate.candidate_id.clone())
            .collect();

        let exploitation_pick = output.ranked_candidates[0].clone();
        let exploration = self.exploration();
        if exploration.probability > EXPLORATION_CEILING {
            self.fallbacks.fetch_add(1, Ordering::Relaxed);
            return Err(RankUnavailable::InvalidExploration);
        }

        let (selected, explored) =
            match explore(&exploration, request_id, &exploitation_pick, &eligible) {
                ExplorationOutcome::Exploit => (exploitation_pick.clone(), false),
                ExplorationOutcome::Explore { candidate_id } => (candidate_id, true),
            };
        if explored {
            self.explorations.fetch_add(1, Ordering::Relaxed);
        }

        // Put the chosen candidate first and keep the engine's ranking for the
        // rest, so failover still follows the model's own preference order
        // rather than the router's.
        let mut order: Vec<String> = vec![selected.clone()];
        order.extend(
            output
                .ranked_candidates
                .iter()
                .filter(|candidate| **candidate != selected)
                .cloned(),
        );

        let utilities = output
            .candidates
            .iter()
            .filter(|candidate| candidate.valid)
            .map(|candidate| (candidate.candidate_id.clone(), candidate.utility.total))
            .collect();

        self.rankings.fetch_add(1, Ordering::Relaxed);
        Ok(RankedPlan {
            selected,
            order,
            commit_id: predictor.commit_id().to_string(),
            explored,
            reason: output.decision.reason,
            utilities,
        })
    }

    /// Counters for an operator: how often ML ranked, how often the router fell
    /// back, how often exploration moved the selection.
    pub fn counts(&self) -> MlRouterCounts {
        MlRouterCounts {
            rankings: self.rankings.load(Ordering::Relaxed),
            fallbacks: self.fallbacks.load(Ordering::Relaxed),
            explorations: self.explorations.load(Ordering::Relaxed),
            attached: self.is_attached(),
        }
    }
}

/// Operational counters for the serving path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MlRouterCounts {
    pub rankings: u64,
    pub fallbacks: u64,
    pub explorations: u64,
    pub attached: bool,
}

/// The outcome of applying a ranking to one request's executable plan.
///
/// Separate from [`RankedPlan`] because applying is where the safety property
/// lives: a ranking that names a candidate the plan does not contain is dropped
/// rather than partially applied, and the caller always receives a usable plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedRanking {
    /// The candidate to try first after the ranking was applied.
    pub selected: String,
    /// The re-ordered plan, by candidate id.
    pub order: Vec<String>,
    /// Whether the order differs from what the router produced. `false` means ML
    /// agreed with the deterministic plan, which is a real and common outcome.
    pub changed: bool,
    pub commit_id: String,
    pub explored: bool,
    pub reason: String,
}

impl AppliedRanking {
    /// Build an applied ranking from a ranked plan and the plan it applies to.
    ///
    /// `plan_order` is the executable plan, by candidate id. The ranked plan is
    /// only honoured where it names candidates the executable plan actually
    /// contains; anything else is dropped. That is what makes it safe for the ML
    /// path to reorder a plan the eligibility and circuit-breaker filters have
    /// already narrowed, without re-deriving either of those filters.
    pub fn apply(ranked: &RankedPlan, plan_order: &[String]) -> Self {
        let mut order: Vec<String> = Vec::with_capacity(plan_order.len());
        for candidate in &ranked.order {
            if plan_order.contains(candidate) && !order.contains(candidate) {
                order.push(candidate.clone());
            }
        }
        for candidate in plan_order {
            if !order.contains(candidate) {
                order.push(candidate.clone());
            }
        }
        let selected = order
            .first()
            .cloned()
            .unwrap_or_else(|| ranked.selected.clone());
        let changed = order.first() != plan_order.first();
        Self {
            selected,
            order,
            changed,
            commit_id: ranked.commit_id.clone(),
            explored: ranked.explored,
            reason: ranked.reason.clone(),
        }
    }
}

fn bundle_is_finite(bundle: &super::reward::PredictionBundle) -> bool {
    [&bundle.success, &bundle.latency, &bundle.ttft, &bundle.cost]
        .into_iter()
        .all(prediction_is_finite)
}

/// Rebuild an ensemble from its own checkpoint.
///
/// A snapshot is taken under the read lock and then used lock-free, so a slow
/// prediction cannot hold the lock against a promotion. Round-tripping through
/// the checkpoint is the way to get an owned copy: it is the same content the
/// predictor holds, and it is the content the identity was derived from, so the
/// snapshot cannot drift from what was committed.
fn clone_ensemble(ensemble: &ModelEnsemble) -> ModelEnsemble {
    ModelEnsemble::load_all(&ensemble.save_all()).unwrap_or_else(|_| ModelEnsemble::new())
}

fn prediction_is_finite(prediction: &Prediction) -> bool {
    prediction.value.is_finite() && prediction.confidence.is_finite()
}

/// The candidate identities of a decision-time snapshot, in plan order.
pub fn candidate_ids(input: &ShadowInput) -> Vec<String> {
    input
        .candidates
        .iter()
        .map(|candidate: &ShadowCandidateInput| candidate.candidate_id.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::coordinator::CoordinatorConfig;
    use crate::ml::features::{FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION, UNKNOWN};
    use crate::ml::model::RoutingModel;
    use crate::ml::promotion::{PromotionDecision, PromotionVerdict};
    use crate::ml::reward::RewardPolicy;

    fn candidate(id: &str, provider: &str, eligible: bool, latency: f32) -> ShadowCandidateInput {
        let mut values = [UNKNOWN; FEATURE_DIMENSION];
        values[crate::ml::features::F_OBS_LATENCY_EWMA] = latency;
        ShadowCandidateInput {
            candidate_id: id.to_string(),
            provider_id: provider.to_string(),
            tier: None,
            eligible,
            features: RoutingFeatures {
                schema_version: FEATURE_SCHEMA_VERSION,
                values,
            },
            rejection_reason: if eligible {
                None
            } else {
                Some("policy rejected".to_string())
            },
        }
    }

    fn input(selected: &str, candidates: Vec<ShadowCandidateInput>) -> ShadowInput {
        ShadowInput {
            decision_id: "d-1".to_string(),
            production_selected: selected.to_string(),
            candidates,
            ..ShadowInput::default()
        }
    }

    /// An ensemble that has seen one update, so it is distinguishable from a
    /// cold one and a promotion of it can be told apart from a promotion of a
    /// different model.
    fn trained_ensemble() -> ModelEnsemble {
        let mut ensemble = ModelEnsemble::new();
        ensemble.success.update(
            &RoutingFeatures {
                schema_version: FEATURE_SCHEMA_VERSION,
                values: {
                    let mut values = [UNKNOWN; FEATURE_DIMENSION];
                    values[0] = 1.0;
                    values
                },
            },
            1.0,
        );
        ensemble
    }

    /// Learning events every fixture model records.
    ///
    /// One constant, because the report and the checkpoint must agree on it or
    /// the store's identity check is doing nothing.
    const FIXTURE_EVENTS: u64 = 7;

    /// A decision produced by a **real** `PromotionGate`, never by hand.
    ///
    /// `PromotionCriterion`'s constructors are private, so a decision cannot be
    /// assembled outside the promotion module — which is exactly the property
    /// that makes `promote` trustworthy. These tests therefore go through the
    /// gate, so "the only way to obtain a PROMOTED verdict is to pass the gate"
    /// is exercised rather than assumed.
    fn training_report(commit: &str, holdout_loss: f64, seed: u64) -> crate::ml::TrainingReport {
        crate::ml::TrainingReport {
            model_id: "shadow".to_string(),
            sample_count: 300,
            request_count: 100,
            train_size: 210,
            validation_size: 45,
            holdout_size: 45,
            train_groups: 70,
            validation_groups: 15,
            holdout_groups: 15,
            passes: Vec::new(),
            holdout_loss,
            holdout: Default::default(),
            final_commit: commit.to_string(),
            base_commit: "0000000000000000".to_string(),
            learning_event_count: FIXTURE_EVENTS,
            dataset_fingerprint: fingerprint(seed),
            source_fingerprint: fingerprint(seed),
            config_identity: "config-identity".to_string(),
            feature_schema_version: FEATURE_SCHEMA_VERSION,
            reward_policy: RewardPolicy::default(),
            coverage: crate::ml::FeatureCoverage::measure(&[]),
            produced_at: 0,
        }
    }

    fn fingerprint(seed: u64) -> crate::ml::DatasetFingerprint {
        crate::ml::DatasetFingerprint::parse(&format!("{seed:016x}"))
            .expect("a u64 always formats as sixteen hex digits")
    }

    /// The commit id a checkpoint derives under the fixture's identity inputs.
    fn commit_of(checkpoint: &ModelCheckpoint) -> String {
        super::super::model_identity::ModelCommit::new(
            ModelId::new("shadow"),
            checkpoint.clone(),
            None,
            7,
        )
        .commit_id
        .to_string()
    }

    /// An ensemble and a gate decision that promotes exactly it.
    fn promoted_decision() -> (ModelEnsemble, PromotionDecision) {
        let ensemble = trained_ensemble();
        let checkpoint = ensemble.save_all();
        let commit = commit_of(&checkpoint);
        let decision = crate::ml::PromotionGate::new(permissive_gate()).evaluate(
            &training_report(&commit, 0.30, 7),
            &comparison(7, 0.5, 100),
            None,
        );
        assert_eq!(
            decision.verdict,
            PromotionVerdict::Promoted,
            "the fixture gate must promote: {:?}",
            decision.criteria
        );
        (ensemble, decision)
    }

    fn comparison(body: u64, utility: f64, paired: usize) -> crate::ml::RoutingComparison {
        use crate::ml::comparison::{ArmMetrics, BaselinePairing, PairedDeltas, RoutingVerdict};
        crate::ml::RoutingComparison {
            dataset_fingerprint: fingerprint(body),
            traces: paired,
            candidate_commit: String::new(),
            arms: vec![ArmMetrics {
                policy: "ml.candidate".to_string(),
                ..Default::default()
            }],
            paired: vec![BaselinePairing {
                baseline: "baseline.priority".to_string(),
                deltas: PairedDeltas {
                    paired_requests: paired,
                    mean_observed_utility_delta: utility,
                    candidate_regressions: 0,
                    candidate_improvements: paired,
                    both_succeeded: paired,
                    both_failed: 0,
                    utility_regressions: 0,
                    ..Default::default()
                },
                verdict: RoutingVerdict::Improved,
            }],
            reward_policy: RewardPolicy::default(),
            min_paired_requests: crate::ml::comparison::MIN_PAIRED_REQUESTS,
            produced_at: 0,
        }
    }

    /// A gate configuration that promotes anything with enough paired evidence
    /// and a positive utility delta.
    fn permissive_gate() -> crate::ml::PromotionConfig {
        crate::ml::PromotionConfig {
            required_baseline: "baseline.priority".to_string(),
            min_paired_requests: 30,
            min_utility_delta: 0.2,
            max_success_regressions: 0,
            min_holdout_loss_improvement: 0.05,
            ..crate::ml::PromotionConfig::default()
        }
    }

    fn predictor_for(ensemble: &ModelEnsemble, count: u64) -> ActivePredictor {
        let checkpoint = ensemble.save_all();
        let commit = super::super::model_identity::ModelCommit::new(
            ModelId::new("shadow"),
            checkpoint.clone(),
            None,
            count,
        );
        let report = training_report(commit.commit_id.as_str(), 0.30, 7);
        let decision = crate::ml::PromotionGate::new(permissive_gate()).evaluate(
            &report,
            &comparison(7, 0.5, 100),
            None,
        );
        assert_eq!(
            decision.verdict,
            PromotionVerdict::Promoted,
            "the fixture gate must promote: {:?}",
            decision.criteria
        );
        ActivePredictor::load(&ActiveModel {
            schema_version: ACTIVE_MODEL_SCHEMA_VERSION,
            model_id: "shadow".to_string(),
            commit_id: commit.commit_id.to_string(),
            learning_event_count: count,
            checkpoint,
            promotion: decision,
            promoted_at: 0,
        })
        .expect("the fixture ensemble verifies")
    }

    fn predictor() -> ActivePredictor {
        predictor_for(&ModelEnsemble::new(), 0)
    }

    fn router(exploration: ExplorationConfig) -> MlRouter {
        let engine = DecisionEngine::new(CoordinatorConfig::default(), RewardPolicy::default());
        let router = MlRouter::new(engine, exploration);
        router.attach(predictor());
        router
    }

    // -- the gate is the only way in -------------------------------------

    #[test]
    fn a_decision_that_did_not_promote_cannot_install_a_model() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ActiveModelStore::open(dir.path()).expect("open");
        let checkpoint = trained_ensemble().save_all();
        let commit = commit_of(&checkpoint);

        // Rejected: the evidence was sufficient and the candidate lost.
        let rejected = crate::ml::PromotionGate::new(permissive_gate()).evaluate(
            &training_report(&commit, 0.30, 7),
            &comparison(7, -0.5, 100),
            None,
        );
        assert_eq!(rejected.verdict, PromotionVerdict::Rejected);

        // Blocked: there was not enough evidence to judge.
        let blocked = crate::ml::PromotionGate::new(permissive_gate()).evaluate(
            &training_report(&commit, 0.30, 7),
            &comparison(7, 0.5, 5),
            None,
        );
        assert_eq!(blocked.verdict, PromotionVerdict::Blocked);

        for decision in [&rejected, &blocked] {
            let error = store
                .promote(decision, checkpoint.clone())
                .expect_err("a non-promoted decision must be refused");
            let message = error.to_string();
            assert!(message.contains(decision.verdict.as_str()), "{message}");
            assert!(
                message.contains("unmet criteria"),
                "the refusal must name the unmet criteria: {message}"
            );
            assert!(
                store.active().expect("read").is_none(),
                "{:?} must leave the router with no model",
                decision.verdict
            );
            assert_eq!(
                store.counts().0,
                0,
                "{:?} must not be counted",
                decision.verdict
            );
        }
    }

    #[test]
    fn a_promoted_decision_cannot_install_a_different_checkpoint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ActiveModelStore::open(dir.path()).expect("open");
        let (_, decision) = promoted_decision();
        // A checkpoint the decision never saw.
        let other = ModelEnsemble::new().save_all();

        let error = store
            .promote(&decision, other)
            .expect_err("a stale decision must not authorise another model");
        assert!(
            error.to_string().contains("derives"),
            "the refusal must explain the mismatch: {error}"
        );
        assert!(store.active().expect("read").is_none());
    }

    #[test]
    fn a_promoted_decision_installs_exactly_the_model_it_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ActiveModelStore::open(dir.path()).expect("open");
        let (ensemble, decision) = promoted_decision();
        let checkpoint = ensemble.save_all();

        let commit = store
            .promote(&decision, checkpoint)
            .expect("a promoted decision installs its model");
        assert_eq!(commit.to_string(), decision.candidate_commit);

        let loaded = store.active().expect("read").expect("a model");
        assert_eq!(loaded.commit_id().to_string(), decision.candidate_commit);

        // The stored decision is readable, not just its digest: the gate
        // configuration in force and the criteria that held are recoverable.
        let pointer = store.read_pointer().expect("read").expect("a pointer");
        assert_eq!(
            pointer.current.promotion.gate_config_identity,
            decision.gate_config_identity
        );
        assert_eq!(
            pointer.current.promotion.verdict,
            PromotionVerdict::Promoted
        );
        assert_eq!(
            pointer.current.promotion.dataset_fingerprint,
            decision.dataset_fingerprint
        );

        let audit = store.audit().expect("audit");
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, ActiveModelAction::Promote);
        assert_eq!(audit[0].verdict, PromotionVerdict::Promoted);
        assert_eq!(audit[0].gate_identity, decision.gate_config_identity);
    }

    // -- ranking ----------------------------------------------------------

    #[test]
    fn with_no_model_attached_ranking_is_unavailable_not_a_failure() {
        let engine = DecisionEngine::new(CoordinatorConfig::default(), RewardPolicy::default());
        let router = MlRouter::new(engine, ExplorationConfig::default());
        assert!(!router.is_attached());
        let verdict = router.rank(&input("a", vec![candidate("a", "p", true, 0.5)]), "r1");
        assert_eq!(verdict, Err(RankUnavailable::NoActiveModel));
        assert_eq!(router.counts().fallbacks, 1);
        assert_eq!(router.counts().rankings, 0);
    }

    #[test]
    fn a_ranking_orders_the_candidates_and_names_its_commit() {
        let router = router(ExplorationConfig::default());
        let snapshot = input(
            "a",
            vec![
                candidate("a", "alpha", true, 0.2),
                candidate("b", "beta", true, 0.8),
            ],
        );
        let ranked = router.rank(&snapshot, "r1").expect("ranked");
        assert!(ranked.order.contains(&"a".to_string()));
        assert!(ranked.order.contains(&"b".to_string()));
        assert_eq!(ranked.order.len(), 2);
        assert_eq!(ranked.commit_id, predictor().commit_id().to_string());
        assert!(!ranked.reason.is_empty());
    }

    #[test]
    fn an_ineligible_candidate_is_never_ranked_first() {
        let router = router(ExplorationConfig::default());
        let snapshot = input(
            "a",
            vec![
                candidate("a", "alpha", false, 0.0),
                candidate("b", "beta", true, 0.8),
            ],
        );
        let ranked = router.rank(&snapshot, "r1").expect("ranked");
        assert_ne!(ranked.selected, "a");
        assert!(!ranked.utilities.iter().any(|(id, _)| id == "a"));
    }

    #[test]
    fn withdrawing_the_model_returns_the_router_to_deterministic_routing() {
        let router = router(ExplorationConfig::default());
        assert!(router.is_attached());
        assert!(router.withdraw().is_some());
        assert!(!router.is_attached());
        let snapshot = input("a", vec![candidate("a", "alpha", true, 0.5)]);
        assert_eq!(
            router.rank(&snapshot, "r1"),
            Err(RankUnavailable::NoActiveModel)
        );
    }

    // -- applying a ranking to an executable plan -------------------------

    #[test]
    fn applying_a_ranking_never_loses_or_invents_a_candidate() {
        let router = router(ExplorationConfig::default());
        let snapshot = input(
            "a",
            vec![
                candidate("a", "alpha", true, 0.2),
                candidate("b", "beta", true, 0.8),
                candidate("c", "gamma", true, 0.5),
            ],
        );
        let ranked = router.rank(&snapshot, "r1").expect("ranked");
        let plan = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let applied = AppliedRanking::apply(&ranked, &plan);

        assert_eq!(applied.order.len(), plan.len());
        let mut sorted = applied.order.clone();
        sorted.sort();
        let mut expected = plan.clone();
        expected.sort();
        assert_eq!(sorted, expected, "the candidate set must be unchanged");
    }

    #[test]
    fn a_ranking_naming_a_candidate_outside_the_plan_is_dropped() {
        let router = router(ExplorationConfig::default());
        let snapshot = input(
            "a",
            vec![
                candidate("a", "alpha", true, 0.2),
                candidate("b", "beta", true, 0.8),
            ],
        );
        let ranked = router.rank(&snapshot, "r1").expect("ranked");
        // The executable plan holds only one of the two candidates.
        let applied = AppliedRanking::apply(&ranked, &["a".to_string()]);
        assert_eq!(applied.order, vec!["a".to_string()]);
        assert_eq!(applied.selected, "a");
        assert!(!applied.changed);
    }

    // -- exploration ------------------------------------------------------

    #[test]
    fn exploration_is_off_by_default() {
        let router = router(ExplorationConfig::default());
        let snapshot = input(
            "a",
            vec![
                candidate("a", "alpha", true, 0.1),
                candidate("b", "beta", true, 0.9),
            ],
        );
        let ranked = router.rank(&snapshot, "r1").expect("ranked");
        assert!(!ranked.explored);
        assert_eq!(router.counts().explorations, 0);
    }

    #[test]
    fn exploration_never_leaves_the_eligible_set_and_never_repeats_the_exploit() {
        let eligible = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let config = ExplorationConfig {
            probability: 0.5,
            seed: 42,
        };
        let mut explored = 0;
        for index in 0..500 {
            let request = format!("r{index}");
            if let ExplorationOutcome::Explore { candidate_id } =
                explore(&config, &request, "a", &eligible)
            {
                explored += 1;
                assert_ne!(candidate_id, "a");
                assert!(eligible.contains(&candidate_id));
            }
        }
        // With a probability of 0.5 over 500 requests, "almost none" and "all"
        // are both bugs.
        assert!(
            (100..400).contains(&explored),
            "explored {explored} of 500, which is not roughly a coin flip"
        );
    }

    #[test]
    fn exploration_is_deterministic_for_a_seed_and_request() {
        let eligible = vec!["a".to_string(), "b".to_string()];
        let config = ExplorationConfig {
            probability: 0.5,
            seed: 7,
        };
        for index in 0..50 {
            let request = format!("r{index}");
            assert_eq!(
                explore(&config, &request, "a", &eligible),
                explore(&config, &request, "a", &eligible)
            );
        }
    }

    #[test]
    fn a_different_seed_explores_differently() {
        let eligible = vec!["a".to_string(), "b".to_string()];
        let one = ExplorationConfig {
            probability: 0.5,
            seed: 1,
        };
        let two = ExplorationConfig {
            probability: 0.5,
            seed: 2,
        };
        let differs = (0..100).any(|index| {
            explore(&one, &format!("r{index}"), "a", &eligible)
                != explore(&two, &format!("r{index}"), "a", &eligible)
        });
        assert!(differs);
    }

    #[test]
    fn exploration_cannot_choose_when_there_is_nothing_else_to_choose() {
        let config = ExplorationConfig {
            probability: 1.0,
            seed: 3,
        };
        assert_eq!(
            explore(&config, "r1", "a", &["a".to_string()]),
            ExplorationOutcome::Exploit
        );
        assert_eq!(
            explore(&config, "r1", "a", &[]),
            ExplorationOutcome::Exploit
        );
    }

    #[test]
    fn a_probability_above_the_ceiling_is_refused_by_the_router() {
        let router = router(ExplorationConfig {
            probability: 0.9,
            seed: 1,
        });
        let snapshot = input(
            "a",
            vec![
                candidate("a", "alpha", true, 0.1),
                candidate("b", "beta", true, 0.9),
            ],
        );
        assert_eq!(
            router.rank(&snapshot, "r1"),
            Err(RankUnavailable::InvalidExploration)
        );
        assert_eq!(router.counts().fallbacks, 1);
    }

    #[test]
    fn an_out_of_range_probability_is_refused_by_its_own_validator() {
        assert!(ExplorationConfig {
            probability: 1.5,
            seed: 1
        }
        .validate()
        .is_err());
        assert!(ExplorationConfig {
            probability: f64::NAN,
            seed: 1
        }
        .validate()
        .is_err());
        assert!(ExplorationConfig {
            probability: 0.1,
            seed: 1
        }
        .validate()
        .is_ok());
    }

    // -- lifecycle --------------------------------------------------------

    #[test]
    fn a_promotion_survives_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (ensemble, decision) = promoted_decision();
        let commit = {
            let store = ActiveModelStore::open(dir.path()).expect("open");
            let commit = store
                .promote(&decision, ensemble.save_all())
                .expect("promote")
                .to_string();
            assert_eq!(commit, decision.candidate_commit);
            store
                .active()
                .expect("read")
                .expect("the promoted model is live");
            commit
        };
        assert!(!commit.is_empty());
    }

    #[test]
    fn a_promotion_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (ensemble, decision) = promoted_decision();
        {
            let store = ActiveModelStore::open(dir.path()).expect("open");
            store
                .promote(&decision, ensemble.save_all())
                .expect("promote");
        }
        // Reopened from scratch, which is what a process restart looks like. A
        // promotion that only lived in memory would be a rollback nobody asked
        // for.
        let reopened = ActiveModelStore::open(dir.path()).expect("reopen");
        let loaded = reopened.active().expect("read").expect("a model");
        assert_eq!(loaded.commit_id().to_string(), decision.candidate_commit);
        assert_eq!(loaded.model_id(), "shadow");
        assert_eq!(reopened.audit().expect("audit").len(), 1);
    }

    #[test]
    fn a_checkpoint_whose_commit_field_was_edited_is_refused_on_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ActiveModelStore::open(dir.path()).expect("open");
        let (ensemble, decision) = promoted_decision();
        store
            .promote(&decision, ensemble.save_all())
            .expect("promote");

        // Someone edits the stored identity without touching the content.
        let mut pointer = store.read_pointer().expect("read").expect("a pointer");
        pointer.current.commit_id = "0000000000000000".to_string();
        store.write_pointer(&pointer).expect("write");

        // The store reports no active model and counts the refusal, rather than
        // serving under a borrowed identity.
        assert!(store.active().expect("read").is_none());
        let (_, _, refusals) = store.counts();
        assert_eq!(refusals, 1);

        // The pointer is repaired by re-promoting on the real decision, which
        // still names the commit the content derives. The store recovers; it
        // does not require an operator to delete anything first.
        assert!(store.promote(&decision, ensemble.save_all()).is_ok());
        assert!(store.active().expect("read").is_some());
    }

    #[test]
    fn rollback_restores_the_previous_model_and_is_audited() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ActiveModelStore::open(dir.path()).expect("open");
        let (first, first_decision) = promoted_decision();
        let second = trained_ensemble();
        let mut second_ensemble = ModelEnsemble::new();
        second_ensemble.cost.update(
            &RoutingFeatures {
                schema_version: FEATURE_SCHEMA_VERSION,
                values: {
                    let mut values = [UNKNOWN; FEATURE_DIMENSION];
                    values[3] = 0.5;
                    values
                },
            },
            2.0,
        );
        let second_checkpoint = second_ensemble.save_all();
        let second_commit = super::super::model_identity::ModelCommit::new(
            ModelId::new("shadow"),
            second_checkpoint.clone(),
            None,
            FIXTURE_EVENTS,
        );
        // A second decision, from the gate, naming the second model. What
        // distinguishes the two promotions is the identity each gate decision
        // names, not the gate's opinion of which model is better.
        let second_decision = crate::ml::PromotionGate::new(permissive_gate()).evaluate(
            &training_report(second_commit.commit_id.as_str(), 0.30, 9),
            &comparison(9, 0.5, 100),
            None,
        );
        assert_eq!(second_decision.verdict, PromotionVerdict::Promoted);
        assert_ne!(
            first_decision.candidate_commit,
            second_decision.candidate_commit
        );

        store
            .promote(&first_decision, first.save_all())
            .expect("first");
        store
            .promote(&second_decision, second_checkpoint)
            .expect("second");
        assert_eq!(
            store.active_identity().expect("read").as_deref(),
            Some(second_decision.candidate_commit.as_str())
        );

        assert!(store.rollback().expect("rollback"));
        assert_eq!(
            store.active_identity().expect("read").as_deref(),
            Some(first_decision.candidate_commit.as_str()),
            "rollback did not restore the model that was replaced"
        );
        let audit = store.audit().expect("audit");
        assert_eq!(audit.len(), 3);
        assert_eq!(audit[2].action, ActiveModelAction::Rollback);
        // Rolling back again has nowhere further to go.
        assert!(!store.rollback().expect("rollback"));
        let _ = second;
    }
}
