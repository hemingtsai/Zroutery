//! Shadow decision infrastructure for ML routing (Stage 7E-1).
//!
//! Records what the ML routing stack *would have done* for policy-routed
//! main traffic — without influencing production routing. Every
//! [`ShadowDecision`] correlates a [`ProductionDecisionRef`] (what production
//! actually did) with a [`ShadowVerdict`] (the hypothetical ML decision) plus
//! the per-candidate [`ShadowCandidate`] evidence that produced it.
//!
//! Design invariants:
//!
//! - The engine never touches runtime stores: all inputs arrive as an
//!   immutable [`ShadowInput`] snapshot.
//! - A shadow verdict is evidence only — it can never become a routing
//!   action.
//! - Faults are contained: a panicking predictor or failing step increments
//!   the fault counter, logs one non-sensitive diagnostic line, and returns
//!   `None`; production continues either way.
//! - The shadow ensemble is content-addressed: [`ShadowEngine::train`]
//!   advances a deterministic commit chain equivalent to replaying all
//!   samples from genesis.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

use super::coordinator::RoutingAction;
use super::dataset::TrainingSample as DatasetTrainingSample;
use super::decision_engine::{DecisionEngine, EngineCandidate, EngineInput};
use super::features::{extract_features, FeatureContext, RoutingFeatures, FEATURE_SCHEMA_VERSION};
use super::model::{Prediction, RoutingModel};
use super::model_identity::{
    validate_replay_sample, CommitId, ModelCheckpoint, ModelCommit, ModelEnsemble, ModelId,
    ReplayError,
};
use super::reward::{PredictionBundle, UtilityBreakdown};
use crate::config::ModelTier;
use crate::observation::ObservationStore;
use crate::policy::{PolicyRevision, RouteDecision, TaskProfile, TaskProfileSummary};
use crate::router::Candidate;
use crate::session::SessionRoutingMode;
use crate::stats_ext::StatsStore;

// ---------------------------------------------------------------------------
// FNV-1a checksum helpers (pattern from model_identity::content_hash)
// ---------------------------------------------------------------------------

/// FNV-1a offset basis.
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
/// FNV-1a prime.
const FNV_PRIME: u64 = 0x100000001b3;

/// Mix `bytes` into `hash` following the FNV-1a pattern used by
/// [`ModelCheckpoint::content_hash`](super::model_identity::ModelCheckpoint::content_hash).
pub(crate) fn fnv_mix_u64(hash: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *hash ^= u64::from(*b);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

/// Mix a stable, quantized representation of an `f32` into `hash`.
///
/// Values in the normal ML range are quantized to 1e-12 before hashing. This
/// removes harmless last-bit differences introduced by serializing a computed
/// utility and parsing it again, without allocating on the evaluation hot
/// path. Values outside that range retain their exact bit pattern; the JSON
/// representation of those finite values is already round-trip exact, and
/// non-finite values are rejected before a record is stored.
fn fnv_mix_f32(hash: &mut u64, value: f32) {
    if value == 0.0 {
        fnv_mix_u64(hash, &0i64.to_le_bytes());
    } else if value.is_finite() && value.abs() <= 1_000_000.0 {
        let quantized = (f64::from(value) * 1e12).round() as i64;
        fnv_mix_u64(hash, &quantized.to_le_bytes());
    } else {
        fnv_mix_u64(hash, &value.to_bits().to_le_bytes());
    }
}

/// Mix a stable, quantized representation of an `f64` into `hash` using the
/// same replay precision as [`fnv_mix_f32`].
fn fnv_mix_f64(hash: &mut u64, value: f64) {
    if value == 0.0 {
        fnv_mix_u64(hash, &0i64.to_le_bytes());
    } else if value.is_finite() && value.abs() <= 1_000_000.0 {
        let quantized = (value * 1e12).round() as i64;
        fnv_mix_u64(hash, &quantized.to_le_bytes());
    } else {
        fnv_mix_u64(hash, &value.to_bits().to_le_bytes());
    }
}

/// Mix a length-delimited string into `hash`.
///
/// Length delimiting matters here because the input record contains several
/// adjacent identity strings. Without it, moving bytes from one identity into
/// the next could produce the same byte stream.
fn fnv_mix_string(hash: &mut u64, value: &str) {
    fnv_mix_u64(hash, &(value.len() as u64).to_le_bytes());
    fnv_mix_u64(hash, value.as_bytes());
}

/// Mix an optional string into `hash`, preserving the distinction between an
/// absent value and an empty value.
fn fnv_mix_optional_string(hash: &mut u64, value: Option<&str>) {
    match value {
        Some(value) => {
            fnv_mix_u64(hash, &[1]);
            fnv_mix_string(hash, value);
        }
        None => fnv_mix_u64(hash, &[0]),
    }
}

/// Mix the policy revision used to produce a production decision.
fn fnv_mix_policy_revision(hash: &mut u64, revision: &PolicyRevision) {
    fnv_mix_string(hash, &revision.policy_id);
    fnv_mix_u64(hash, &[u8::from(revision.policy_enabled)]);
    fnv_mix_u64(hash, &revision.requirements_hash.to_le_bytes());
    fnv_mix_u64(hash, &revision.preference_hash.to_le_bytes());
}

/// Mix the task identity/summary used to produce a production decision.
fn fnv_mix_task_summary(hash: &mut u64, task: &TaskProfileSummary) {
    fnv_mix_string(hash, &task.complexity);
    fnv_mix_string(hash, &task.task_type);
    fnv_mix_u64(hash, &task.context_tokens.to_le_bytes());
    fnv_mix_u64(hash, &task.estimated_output_tokens.to_le_bytes());
    fnv_mix_u64(hash, &[u8::from(task.streaming)]);
    fnv_mix_u64(hash, &[u8::from(task.has_tools)]);
    fnv_mix_u64(hash, &[u8::from(task.has_vision)]);
    fnv_mix_u64(
        hash,
        &(task.required_capabilities.len() as u64).to_le_bytes(),
    );
    for capability in &task.required_capabilities {
        fnv_mix_string(hash, capability);
    }
}

/// Discriminant of [`ModelTier`] for checksums: 0 = none, 1..=4 = tiers.
fn tier_discriminant(tier: Option<ModelTier>) -> u8 {
    match tier {
        None => 0,
        Some(ModelTier::Fast) => 1,
        Some(ModelTier::Standard) => 2,
        Some(ModelTier::Reasoning) => 3,
        Some(ModelTier::Frontier) => 4,
    }
}

/// Discriminant of [`SessionRoutingMode`] for checksums.
fn session_mode_discriminant(mode: SessionRoutingMode) -> u8 {
    match mode {
        SessionRoutingMode::Free => 0,
        SessionRoutingMode::Sticky => 1,
        SessionRoutingMode::Pinned => 2,
    }
}

/// Discriminant of [`RoutingAction`] for checksums.
fn action_discriminant(action: RoutingAction) -> u8 {
    match action {
        RoutingAction::Keep => 0,
        RoutingAction::Switch => 1,
        RoutingAction::Explore => 2,
    }
}

/// Identity of the INPUT snapshot a shadow decision was computed from.
///
/// Every decision-time field which can affect the prediction or the
/// counterfactual is included: the model commit, policy/task identity, the
/// planned production selection, and every candidate's ordered identity,
/// eligibility/rejection evidence, feature schema, and feature bits. Volatile
/// correlation fields (timestamp, shadow id, request id, and decision id) are
/// intentionally excluded: replaying the same decision-time input must produce
/// the same checksum.
fn shadow_input_checksum(input: &ShadowInput, model_commit: &CommitId) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    fnv_mix_string(&mut hash, model_commit.as_str());
    fnv_mix_string(&mut hash, &input.policy_id);
    fnv_mix_optional_string(&mut hash, input.client_id.as_deref());
    fnv_mix_policy_revision(&mut hash, &input.policy_revision);
    fnv_mix_task_summary(&mut hash, &input.task);
    fnv_mix_u64(&mut hash, &input.feature_schema.to_le_bytes());
    fnv_mix_string(&mut hash, &input.production_selected);
    fnv_mix_u64(&mut hash, &(input.candidates.len() as u64).to_le_bytes());
    for candidate in &input.candidates {
        fnv_mix_string(&mut hash, &candidate.candidate_id);
        fnv_mix_string(&mut hash, &candidate.provider_id);
        fnv_mix_u64(&mut hash, &[tier_discriminant(candidate.tier)]);
        fnv_mix_u64(&mut hash, &[u8::from(candidate.eligible)]);
        fnv_mix_optional_string(&mut hash, candidate.rejection_reason.as_deref());
        fnv_mix_u64(&mut hash, &candidate.features.schema_version.to_le_bytes());
        for value in &candidate.features.values {
            fnv_mix_f32(&mut hash, *value);
        }
    }
    fnv_mix_u64(&mut hash, &[session_mode_discriminant(input.session_mode)]);
    fnv_mix_u64(&mut hash, &input.session_switch_count.to_le_bytes());
    fnv_mix_u64(&mut hash, &[u8::from(input.is_fallback)]);
    hash
}

/// Identity of the resulting decision: the input checksum plus the ordered
/// candidate evidence, explicit ML ranking, and verdict. The served identity is
/// intentionally not mixed here: it is not known at decision time in this pure
/// seam and must not change the replayable counterfactual.
fn shadow_decision_checksum(
    input_checksum: u64,
    candidates: &[ShadowCandidate],
    ranked_candidates: &[String],
    selected: &str,
    action: RoutingAction,
    reason: &str,
) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    fnv_mix_u64(&mut hash, &input_checksum.to_le_bytes());
    fnv_mix_u64(&mut hash, &(candidates.len() as u64).to_le_bytes());
    for candidate in candidates {
        fnv_mix_string(&mut hash, &candidate.candidate_id);
        fnv_mix_string(&mut hash, &candidate.provider_id);
        fnv_mix_u64(&mut hash, &[tier_discriminant(candidate.tier)]);
        fnv_mix_u64(&mut hash, &[u8::from(candidate.eligible)]);
        fnv_mix_u64(&mut hash, &[u8::from(candidate.valid)]);
        fnv_mix_optional_string(&mut hash, candidate.rejection_reason.as_deref());
        let bundle = &candidate.prediction;
        for prediction in [&bundle.success, &bundle.latency, &bundle.ttft, &bundle.cost] {
            fnv_mix_f64(&mut hash, prediction.value);
            fnv_mix_f64(&mut hash, prediction.confidence);
            fnv_mix_u64(&mut hash, &prediction.sample_count.to_le_bytes());
            fnv_mix_u64(&mut hash, &[u8::from(prediction.cold)]);
        }
        let utility = &candidate.utility;
        for component in [
            utility.success,
            utility.latency,
            utility.ttft,
            utility.cost,
            utility.fallback,
            utility.uncertainty,
            utility.switch_cost,
            utility.total,
        ] {
            fnv_mix_f64(&mut hash, component);
        }
    }
    fnv_mix_u64(&mut hash, &(ranked_candidates.len() as u64).to_le_bytes());
    for candidate_id in ranked_candidates {
        fnv_mix_string(&mut hash, candidate_id);
    }
    fnv_mix_string(&mut hash, selected);
    fnv_mix_u64(&mut hash, &[action_discriminant(action)]);
    fnv_mix_string(&mut hash, reason);
    hash
}

// ---------------------------------------------------------------------------
// Shadow scope / decision records
// ---------------------------------------------------------------------------

/// Explicit scoping of shadow mode (7E-1 covers policy-routed main traffic
/// only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowScope {
    PolicyRouted,
}

/// What production planned and, separately, what actually served.
///
/// `selected` is the initial `RouteDecision.selected` identity. It is not a
/// claim about the final response after runtime fallback. The pure shadow seam
/// has no final outcome input, so `served` is explicitly `None` until the
/// production integration node supplies that identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductionDecisionRef {
    pub request_id: String,
    pub decision_id: String,
    /// Initial planned selection, before runtime failover.
    pub selected: String,
    /// Final served model identity, or `None` when not integrated/known.
    #[serde(default)]
    pub served: Option<String>,
}

impl ProductionDecisionRef {
    /// Explicit name for the legacy `selected` field.
    pub fn planned_selected(&self) -> &str {
        &self.selected
    }

    /// Whether a final served identity has been supplied by an integration
    /// boundary. The pure ML seam intentionally returns `false`.
    pub fn has_served_identity(&self) -> bool {
        self.served.is_some()
    }
}

/// What the ML stack would have done (hypothetical — never a real routing
/// action).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowVerdict {
    pub model_commit: CommitId,
    pub feature_schema: u32,
    pub selected: String,
    pub action: RoutingAction,
    pub reason: String,
    /// Valid candidates in the explicit ML utility ranking used for the
    /// counterfactual. This is independent of production plan order.
    #[serde(default)]
    pub ranked_candidates: Vec<String>,
}

/// Per-candidate evidence captured during a shadow evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowCandidate {
    pub candidate_id: String,
    pub provider_id: String,
    pub tier: Option<ModelTier>,
    /// Hard eligibility as reported by production.
    pub eligible: bool,
    /// Whether the predictions were finite (usable evidence).
    pub valid: bool,
    /// Why this candidate was not considered (`None` when it was).
    pub rejection_reason: Option<String>,
    pub prediction: PredictionBundle,
    pub utility: UtilityBreakdown,
}

/// The immutable, serializable decision-time observation used to produce a
/// shadow verdict.
///
/// Keeping this as a separate object makes the boundary explicit: a decision
/// record can be round-tripped and replayed without consulting router/session
/// state. The input contains the complete ordered candidate snapshot and the
/// policy/task identity; the commit is pinned beside it so the exact model
/// artifact is part of the observation identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowObservation {
    pub input: ShadowInput,
    pub model_commit: CommitId,
}

impl Default for ShadowObservation {
    fn default() -> Self {
        Self {
            input: ShadowInput::default(),
            model_commit: CommitId::new(""),
        }
    }
}

/// One recorded shadow decision: production's planned identity, the explicit
/// served-identity state, the exact observation, the hypothetical ML verdict,
/// and the evidence between them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowDecision {
    /// `shadow-<uuid simple>`.
    pub shadow_id: String,
    pub timestamp: i64,
    pub scope: ShadowScope,
    pub actual: ProductionDecisionRef,
    #[serde(default)]
    pub observation: ShadowObservation,
    pub shadow: ShadowVerdict,
    pub candidates: Vec<ShadowCandidate>,
    /// Identity of the INPUT snapshot.
    pub decision_input_checksum: u64,
    /// Identity of the resulting decision.
    pub decision_checksum: u64,
}

impl ShadowDecision {
    /// Borrow the exact decision-time input retained by this record.
    pub fn input(&self) -> &ShadowInput {
        &self.observation.input
    }
}

// ---------------------------------------------------------------------------
// Shadow input — immutable snapshot
// ---------------------------------------------------------------------------

/// One candidate as seen at decision time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowCandidateInput {
    pub candidate_id: String,
    pub provider_id: String,
    pub tier: Option<ModelTier>,
    pub eligible: bool,
    /// Exact feature vector captured for this candidate. A policy-only
    /// candidate (present in `RouteDecision` but absent from the executable
    /// plan) uses the explicit UNKNOWN vector and carries its rejection reason;
    /// it is never made executable.
    pub features: RoutingFeatures,
    /// Policy or execution rejection evidence from the production decision.
    #[serde(default)]
    pub rejection_reason: Option<String>,
}

/// Immutable input snapshot — the engine NEVER touches runtime stores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowInput {
    pub decision_id: String,
    /// Policy identity and exact revision used by production.
    #[serde(default)]
    pub policy_id: String,
    /// Client-profile identity which selected the policy, when present.
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub policy_revision: PolicyRevision,
    /// Task identity/summary used by production at decision time.
    #[serde(default)]
    pub task: TaskProfileSummary,
    /// Initial planned selection. This is not the final served identity.
    pub production_selected: String,
    pub feature_schema: u32,
    pub candidates: Vec<ShadowCandidateInput>,
    pub session_mode: SessionRoutingMode,
    pub session_switch_count: u32,
    pub is_fallback: bool,
}

impl Default for ShadowInput {
    fn default() -> Self {
        Self {
            decision_id: String::new(),
            policy_id: String::new(),
            client_id: None,
            policy_revision: PolicyRevision::default(),
            task: TaskProfileSummary::default(),
            production_selected: String::new(),
            feature_schema: FEATURE_SCHEMA_VERSION,
            candidates: Vec::new(),
            session_mode: SessionRoutingMode::Free,
            session_switch_count: 0,
            is_fallback: false,
        }
    }
}

impl ShadowInput {
    /// Build the immutable snapshot for one shadow evaluation of a
    /// policy-routed request.
    ///
    /// Pure with respect to routing state BY SIGNATURE: the only inputs are
    /// read-only store handles plus the plan and [`RouteDecision`]
    /// production already computed. No [`AppState`](crate::server::AppState)
    /// and no [`Router`](crate::router::Router) is reachable here, so no
    /// ranking path can be invoked from this module — the type system
    /// enforces it, and an identical request produces an identical snapshot.
    ///
    /// Eligibility keying mirrors the decision trace: a candidate counts as
    /// eligible when the trace marked its exposed id eligible.
    pub fn from_policy_plan(
        observations: &ObservationStore,
        stats: &StatsStore,
        plan: &[Candidate],
        decision: &RouteDecision,
        task_profile: &TaskProfile,
        message_count: usize,
    ) -> Self {
        let mut candidates: Vec<ShadowCandidateInput> = plan
            .iter()
            .map(|candidate| {
                let entry = &candidate.entry;
                // Production ranks by the exposed id, and the decision trace
                // keys its per-candidate eligibility on the same identity.
                // Keep the route evidence alongside the executable-plan entry;
                // a policy-rejected candidate must not silently become
                // executable merely because it was present in the plan.
                let evidence = decision.candidates.iter().find(|candidate_decision| {
                    candidate_decision.model_id == candidate.exposed_id
                        && candidate_decision.provider_id == entry.provider_id
                });
                let eligible =
                    evidence.is_some_and(|candidate_decision| candidate_decision.eligible);
                let rejection_reason = evidence
                    .and_then(|candidate_decision| candidate_decision.rejection.clone())
                    .or_else(|| {
                        if evidence.is_none() {
                            Some("missing policy evidence".to_string())
                        } else if !eligible {
                            Some("policy rejected".to_string())
                        } else {
                            None
                        }
                    });
                let observation = observations.get(&candidate.exposed_id, &entry.provider_id);
                let stats = stats.get(&candidate.exposed_id, &entry.provider_id);
                let features = extract_features(&FeatureContext {
                    task: Some(task_profile),
                    message_count: Some(message_count),
                    tier: entry.tier,
                    capabilities: Some(&entry.capabilities),
                    priority: entry.priority,
                    observation: Some(&observation),
                    stats: Some(&stats),
                    #[cfg(feature = "account")]
                    account: None,
                });
                ShadowCandidateInput {
                    candidate_id: candidate.exposed_id.clone(),
                    provider_id: entry.provider_id.clone(),
                    tier: entry.tier,
                    eligible,
                    features,
                    rejection_reason,
                }
            })
            .collect();

        // `RouteDecision.candidates` is the authoritative policy evidence. It
        // can contain candidates which policy rejected and which therefore
        // never made it into the executable plan. Retain those identities and
        // rejection reasons as non-executable observations. The full model
        // entry is not available through this pure boundary, so their feature
        // vector is the explicit UNKNOWN snapshot rather than fabricated data.
        for candidate_decision in &decision.candidates {
            let already_present = candidates.iter().any(|candidate| {
                candidate.candidate_id == candidate_decision.model_id
                    && candidate.provider_id == candidate_decision.provider_id
            });
            if already_present {
                continue;
            }
            candidates.push(ShadowCandidateInput {
                candidate_id: candidate_decision.model_id.clone(),
                provider_id: candidate_decision.provider_id.clone(),
                tier: candidate_decision.tier.as_deref().and_then(|tier| {
                    ModelTier::ALL
                        .into_iter()
                        .find(|known| known.as_str() == tier)
                }),
                eligible: false,
                features: RoutingFeatures::default(),
                rejection_reason: Some(
                    candidate_decision
                        .rejection
                        .clone()
                        .unwrap_or_else(|| "not in executable plan".to_string()),
                ),
            });
        }

        Self {
            decision_id: decision.decision_id.clone(),
            policy_id: decision.policy_id.clone(),
            client_id: decision.client_id.clone(),
            policy_revision: decision.policy_revision.clone(),
            task: decision.task.clone(),
            production_selected: decision.selected.clone().unwrap_or_default(),
            feature_schema: FEATURE_SCHEMA_VERSION,
            candidates,
            // The session store is not wired into the production pipeline
            // yet, so shadow evaluation always sees a fresh session: free
            // mode, no switches yet, and this is the initial decision (not
            // a fallback).
            session_mode: SessionRoutingMode::Free,
            session_switch_count: 0,
            is_fallback: false,
        }
    }
}

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
        Self::from_verified_parts(ensemble, expected.clone(), Some(Vec::new()), vec![expected])
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
    fn try_train_with_history(&self, samples: &[DatasetTrainingSample]) -> TrainingLineageResult {
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
    fn from_trained_parts(
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

// ---------------------------------------------------------------------------
// Finiteness helpers
// ---------------------------------------------------------------------------

/// Whether both f64 fields of a prediction are finite.
fn prediction_finite(prediction: &Prediction) -> bool {
    prediction.value.is_finite() && prediction.confidence.is_finite()
}

/// Name of the first non-finite utility component, or `None` when all are
/// finite.
fn utility_non_finite(utility: &UtilityBreakdown) -> Option<&'static str> {
    [
        ("success", utility.success),
        ("latency", utility.latency),
        ("ttft", utility.ttft),
        ("cost", utility.cost),
        ("fallback", utility.fallback),
        ("uncertainty", utility.uncertainty),
        ("switch_cost", utility.switch_cost),
        ("total", utility.total),
    ]
    .into_iter()
    .find(|(_, value)| !value.is_finite())
    .map(|(name, _)| name)
}

/// Replace non-finite predictions with cold zeros so the stored evidence
/// stays finite; the candidate's rejection reason records why.
fn sanitized_bundle(bundle: &PredictionBundle) -> PredictionBundle {
    let sanitize = |prediction: &Prediction| {
        if prediction_finite(prediction) {
            prediction.clone()
        } else {
            Prediction::cold(0.0)
        }
    };
    PredictionBundle {
        candidate_model: bundle.candidate_model.clone(),
        candidate_provider: bundle.candidate_provider.clone(),
        success: sanitize(&bundle.success),
        latency: sanitize(&bundle.latency),
        ttft: sanitize(&bundle.ttft),
        cost: sanitize(&bundle.cost),
    }
}

// ---------------------------------------------------------------------------
// ShadowStore — bounded storage for shadow decisions
// ---------------------------------------------------------------------------

/// Bounded store for shadow decisions with retention, mirroring
/// [`DatasetStore`](super::dataset::DatasetStore).
pub struct ShadowStore {
    decisions: Mutex<VecDeque<ShadowDecision>>,
    max_decisions: usize,
    max_age_secs: i64,
}

impl ShadowStore {
    pub fn new(max_decisions: usize, max_age_secs: i64) -> Self {
        Self {
            decisions: Mutex::new(VecDeque::with_capacity(max_decisions.min(10_000))),
            max_decisions,
            max_age_secs,
        }
    }

    /// Store-boundary gate enforcement.
    ///
    /// A stored record is a replayable observation, not merely a rendered
    /// verdict. The store therefore verifies the retained input/commit
    /// relationship, candidate evidence identity, finite values, ranking
    /// eligibility, and both checksums before accepting the record.
    pub fn push(&self, decision: ShadowDecision) -> Result<(), String> {
        if decision.actual.request_id.is_empty() {
            return Err("empty request_id".to_string());
        }
        if decision.actual.decision_id.is_empty() {
            return Err("empty decision_id".to_string());
        }
        if decision.actual.selected.is_empty() {
            return Err("empty production selected model".to_string());
        }
        if decision.shadow.selected.is_empty() {
            return Err("empty shadow selected identity".to_string());
        }
        if decision.shadow.model_commit.as_str().is_empty() {
            return Err("empty model_commit".to_string());
        }
        if decision.observation.model_commit.as_str().is_empty() {
            return Err("empty observation model_commit".to_string());
        }
        if decision.shadow.model_commit != decision.observation.model_commit {
            return Err("observation model_commit does not match shadow verdict".to_string());
        }
        if decision.actual.decision_id != decision.observation.input.decision_id {
            return Err("observation decision_id does not match actual decision".to_string());
        }
        if decision.actual.selected != decision.observation.input.production_selected {
            return Err("observation planned selection does not match actual decision".to_string());
        }
        if decision.shadow.feature_schema != FEATURE_SCHEMA_VERSION {
            return Err(format!(
                "feature schema mismatch: {} != {}",
                decision.shadow.feature_schema, FEATURE_SCHEMA_VERSION
            ));
        }
        if decision.observation.input.feature_schema != FEATURE_SCHEMA_VERSION {
            return Err(format!(
                "observation feature schema mismatch: {} != {}",
                decision.observation.input.feature_schema, FEATURE_SCHEMA_VERSION
            ));
        }
        if decision.shadow.feature_schema != decision.observation.input.feature_schema {
            return Err("shadow and observation feature schemas differ".to_string());
        }
        if decision.observation.input.candidates.is_empty() {
            return Err("observation has no candidates".to_string());
        }
        if decision.candidates.is_empty() {
            return Err("no candidates".to_string());
        }
        if decision.observation.input.candidates.len() != decision.candidates.len() {
            return Err("observation and evidence candidate counts differ".to_string());
        }
        if decision.decision_input_checksum == 0 {
            return Err("zero decision_input_checksum".to_string());
        }
        if decision.decision_checksum == 0 {
            return Err("zero decision_checksum".to_string());
        }

        let mut ranked = HashSet::new();
        for candidate_id in &decision.shadow.ranked_candidates {
            if !ranked.insert(candidate_id.clone()) {
                return Err(format!("duplicate ranked candidate '{candidate_id}'"));
            }
            let Some(candidate) = decision
                .candidates
                .iter()
                .find(|candidate| &candidate.candidate_id == candidate_id)
            else {
                return Err(format!("ranked candidate '{candidate_id}' has no evidence"));
            };
            if !candidate.eligible || !candidate.valid {
                return Err(format!(
                    "ineligible or invalid candidate '{candidate_id}' cannot be ranked"
                ));
            }
        }
        for (input_candidate, candidate) in decision
            .observation
            .input
            .candidates
            .iter()
            .zip(decision.candidates.iter())
        {
            if input_candidate.candidate_id != candidate.candidate_id
                || input_candidate.provider_id != candidate.provider_id
                || input_candidate.tier != candidate.tier
                || input_candidate.eligible != candidate.eligible
            {
                return Err(format!(
                    "candidate '{}' evidence does not match observation input",
                    candidate.candidate_id
                ));
            }
            if input_candidate.rejection_reason.is_some()
                && input_candidate.rejection_reason != candidate.rejection_reason
            {
                return Err(format!(
                    "candidate '{}' rejection evidence does not match observation input",
                    candidate.candidate_id
                ));
            }
            if input_candidate.features.schema_version != FEATURE_SCHEMA_VERSION
                || input_candidate
                    .features
                    .values
                    .iter()
                    .any(|value| !value.is_finite())
            {
                return Err(format!(
                    "candidate '{}' has an invalid feature snapshot",
                    candidate.candidate_id
                ));
            }
            if candidate.prediction.candidate_model != candidate.candidate_id
                || candidate.prediction.candidate_provider != candidate.provider_id
            {
                return Err(format!(
                    "candidate '{}' prediction identity does not match evidence identity",
                    candidate.candidate_id
                ));
            }
            let bundle = &candidate.prediction;
            for (model, prediction) in [
                ("success", &bundle.success),
                ("latency", &bundle.latency),
                ("ttft", &bundle.ttft),
                ("cost", &bundle.cost),
            ] {
                if !prediction_finite(prediction) {
                    return Err(format!(
                        "candidate '{}' has a non-finite {} prediction",
                        candidate.candidate_id, model
                    ));
                }
            }
            if let Some(component) = utility_non_finite(&candidate.utility) {
                return Err(format!(
                    "candidate '{}' has a non-finite {} utility",
                    candidate.candidate_id, component
                ));
            }
        }

        // The selected identity is a semantic part of the record, not merely
        // a checksum field. Resolve it explicitly before checksum validation so
        // an unknown identity cannot be hidden behind a checksum mismatch.
        let selected_candidate = decision
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_id == decision.shadow.selected)
            .ok_or_else(|| {
                format!(
                    "unknown shadow selected identity '{}': candidate evidence is missing",
                    decision.shadow.selected
                )
            })?;

        match decision.shadow.action {
            RoutingAction::Keep => {
                if decision.shadow.selected != decision.actual.selected {
                    return Err(
                        "Keep action must select the planned production identity".to_string()
                    );
                }
                // Keep deliberately permits the planned identity to be invalid;
                // it is the non-executable terminal state used when no valid
                // candidate or a session guard applies.
            }
            RoutingAction::Switch | RoutingAction::Explore => {
                if !selected_candidate.eligible || !selected_candidate.valid {
                    return Err(format!(
                        "{:?} action selected identity '{}' must be eligible and valid",
                        decision.shadow.action, decision.shadow.selected
                    ));
                }
                if !ranked.contains(&decision.shadow.selected) {
                    return Err(format!(
                        "{:?} action selected identity '{}' is not in the ranked valid set",
                        decision.shadow.action, decision.shadow.selected
                    ));
                }
                if decision.shadow.selected == decision.actual.selected {
                    return Err(format!(
                        "{:?} action must select an identity different from the planned selection",
                        decision.shadow.action
                    ));
                }
            }
        }

        let valid_candidate_ids: HashSet<String> = decision
            .candidates
            .iter()
            .filter(|candidate| candidate.eligible && candidate.valid)
            .map(|candidate| candidate.candidate_id.clone())
            .collect();
        if ranked != valid_candidate_ids {
            return Err("ranked candidates do not match the valid evidence set".to_string());
        }

        let expected_input_checksum = shadow_input_checksum(
            &decision.observation.input,
            &decision.observation.model_commit,
        );
        if expected_input_checksum != decision.decision_input_checksum {
            return Err("decision_input_checksum does not match retained observation".to_string());
        }
        let expected_decision_checksum = shadow_decision_checksum(
            decision.decision_input_checksum,
            &decision.candidates,
            &decision.shadow.ranked_candidates,
            &decision.shadow.selected,
            decision.shadow.action,
            &decision.shadow.reason,
        );
        if expected_decision_checksum != decision.decision_checksum {
            return Err(format!(
                "decision_checksum does not match retained verdict evidence: expected {}, got {}",
                expected_decision_checksum, decision.decision_checksum
            ));
        }

        let mut decisions = crate::sync::lock(&self.decisions);
        if decisions.len() >= self.max_decisions {
            decisions.pop_front();
        }
        decisions.push_back(decision);
        Ok(())
    }

    /// Attach a production served identity to one already-stored decision.
    ///
    /// The served identity is a *terminal* fact: it only exists once the
    /// request has actually delivered an answer, which is necessarily after the
    /// counterfactual was recorded. This attaches that fact to the record the
    /// decision-time evaluation produced, and deliberately does nothing else —
    /// the decision identity (both checksums) was computed without it, so the
    /// stored counterfactual stays exactly replayable.
    ///
    /// `None` means "nothing was served" and leaves the record untouched, so a
    /// failed, cancelled or interrupted request can never be correlated as a
    /// success. An unknown `shadow_id` is a no-op rather than an error: the
    /// bounded store may legitimately have evicted the record by then, and that
    /// is never a fact about the request.
    pub fn correlate_served(&self, shadow_id: &str, served: Option<&str>) -> Result<bool, String> {
        let Some(served) = served else {
            return Ok(false);
        };
        if served.trim().is_empty() {
            return Err("empty served identity".to_string());
        }
        let mut decisions = crate::sync::lock(&self.decisions);
        let Some(decision) = decisions
            .iter_mut()
            .find(|entry| entry.shadow_id == shadow_id)
        else {
            return Ok(false);
        };
        // The served identity has to be one production actually chose from the
        // candidates this very record observed. Anything else is a wiring fault,
        // not evidence, and is refused rather than stored.
        if !decision
            .observation
            .input
            .candidates
            .iter()
            .any(|candidate| candidate.candidate_id == served)
        {
            return Err(format!(
                "served identity '{served}' is not among the observed candidates"
            ));
        }
        if let Some(existing) = decision.actual.served.as_deref() {
            if existing != served {
                return Err(format!(
                    "decision already correlates served identity '{existing}'"
                ));
            }
            return Ok(false);
        }
        decision.actual.served = Some(served.to_string());
        Ok(true)
    }

    /// Number of decisions currently stored (regardless of age).
    pub fn len(&self) -> usize {
        crate::sync::lock(&self.decisions).len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        crate::sync::lock(&self.decisions).is_empty()
    }

    /// Clear all decisions.
    pub fn clear(&self) {
        crate::sync::lock(&self.decisions).clear();
    }

    /// Decisions within the retention window, oldest first.
    pub fn decisions(&self) -> Vec<ShadowDecision> {
        let now = chrono::Utc::now().timestamp();
        crate::sync::lock(&self.decisions)
            .iter()
            .filter(|d| now - d.timestamp < self.max_age_secs)
            .cloned()
            .collect()
    }
}

impl Default for ShadowStore {
    fn default() -> Self {
        Self::new(100_000, 30 * 24 * 3600) // 100k decisions, 30 days
    }
}

// ---------------------------------------------------------------------------
// ShadowEngine
// ---------------------------------------------------------------------------

/// Shadow decision engine — evaluates what the ML stack would have routed,
/// without influencing production.
///
/// Holds an immutable [`DecisionEngine`] plus a swappable ensemble predictor.
/// Readers clone the [`Arc`] under the read lock and then predict lock-free;
/// only [`ShadowEngine::train`] takes the write lock.
pub struct ShadowEngine {
    engine: DecisionEngine,
    predictor: RwLock<Arc<ModelEnsemblePredictor>>,
    store: ShadowStore,
    enabled: bool,
    faults: AtomicU64,
}

impl ShadowEngine {
    pub fn new(engine: DecisionEngine, enabled: bool) -> Self {
        Self {
            engine,
            predictor: RwLock::new(Arc::new(ModelEnsemblePredictor::genesis())),
            store: ShadowStore::default(),
            enabled,
            faults: AtomicU64::new(0),
        }
    }

    /// Whether shadow evaluation is active.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Number of evaluation faults (panics, step failures, store rejections).
    pub fn fault_count(&self) -> u64 {
        self.faults.load(Ordering::Relaxed)
    }

    /// The shadow decision store.
    pub fn store(&self) -> &ShadowStore {
        &self.store
    }

    /// Correlate one stored decision with the served identity the request
    /// actually delivered, as read from that request's terminal outcome.
    ///
    /// This is the production-integration boundary for
    /// [`ProductionDecisionRef::served`], which the pure evaluation seam cannot
    /// know. It is fault-contained exactly like evaluation: a refused
    /// correlation is counted and logged, never propagated, and the record it
    /// concerned is diagnostic evidence anyway.
    ///
    /// Returns whether a stored decision was updated.
    pub fn correlate_served(&self, shadow_id: &str, served: Option<&str>) -> bool {
        match self.store.correlate_served(shadow_id, served) {
            Ok(correlated) => correlated,
            Err(reason) => {
                self.count_fault();
                tracing::debug!(
                    shadow_id = %shadow_id,
                    fault = %reason,
                    "shadow served-identity correlation fault"
                );
                false
            }
        }
    }

    /// Advance the shadow ensemble (TEST/7E-2 seam — NOT wired to production
    /// outcomes in 7E-1).
    ///
    /// The compatibility wrapper panics before swapping if a sample is
    /// invalid; operational callers should use [`Self::try_train`] to handle
    /// the error without touching the predictor.
    pub fn train(&self, samples: &[DatasetTrainingSample]) -> CommitId {
        self.try_train(samples)
            .expect("shadow training received an invalid sample")
    }

    /// Fallible training and atomic predictor swap. The new predictor retains
    /// the complete verified commit, its parent, and its checkpoint.
    pub fn try_train(&self, samples: &[DatasetTrainingSample]) -> Result<CommitId, ReplayError> {
        let current = crate::sync::read(&self.predictor).clone();
        let (ensemble, commit, history, lineage) = current.try_train_with_history(samples)?;
        let predictor =
            ModelEnsemblePredictor::from_trained_parts(ensemble, commit.clone(), history, lineage)?;
        let commit_id = predictor.commit();
        *crate::sync::write(&self.predictor) = Arc::new(predictor);
        Ok(commit_id)
    }

    /// Swap in an already verified predictor while retaining its lineage.
    pub fn swap(&self, predictor: ModelEnsemblePredictor) -> Result<CommitId, ReplayError> {
        if !predictor.verify() {
            return Err(ReplayError::InvalidCommit {
                commit_id: predictor.commit(),
                reason: "refusing to swap an unverified predictor".to_string(),
            });
        }
        let current = crate::sync::read(&self.predictor).clone();
        let current_commit = current.commit_record();
        let replacement_commit = predictor.commit_record();
        if replacement_commit.model_id != current_commit.model_id {
            return Err(ReplayError::InvalidCommit {
                commit_id: replacement_commit.commit_id,
                reason: "predictor model does not match the active shadow lineage".to_string(),
            });
        }
        if !predictor
            .lineage()
            .iter()
            .any(|entry| entry.commit_id == current_commit.commit_id)
        {
            return Err(ReplayError::LineageCorrupt {
                commit_id: replacement_commit.commit_id,
                reason: "replacement lineage is unrelated to the active commit".to_string(),
            });
        }
        let commit_id = replacement_commit.commit_id;
        *crate::sync::write(&self.predictor) = Arc::new(predictor);
        Ok(commit_id)
    }

    /// Complete commit currently pinned by the predictor.
    pub fn predictor_commit(&self) -> ModelCommit {
        crate::sync::read(&self.predictor).commit_record()
    }

    /// Checkpoint currently pinned by the predictor.
    pub fn predictor_checkpoint(&self) -> ModelCheckpoint {
        crate::sync::read(&self.predictor).ensemble_checkpoint()
    }

    /// Verified commit lineage currently retained by the predictor.
    pub fn predictor_lineage(&self) -> Vec<ModelCommit> {
        crate::sync::read(&self.predictor).lineage()
    }

    /// Full shadow evaluation for one request.
    ///
    /// Snapshots the predictor under the read lock (prediction is then
    /// lock-free) and runs the evaluation block. Returns `None` — without
    /// recording a fault — while the engine is disabled.
    pub fn evaluate(&self, request_id: &str, input: &ShadowInput) -> Option<ShadowDecision> {
        let predictor = crate::sync::read(&self.predictor).clone();
        self.evaluate_with(request_id, input, predictor.as_ref())
    }

    /// Evaluation core behind the [`EnsemblePredictor`] seam.
    ///
    /// Public so the gate suite can inject fault predictors (panics,
    /// non-finite predictions) through the same seam production uses;
    /// production callers use [`ShadowEngine::evaluate`], which pins the
    /// engine's own ensemble predictor.
    ///
    /// `catch_unwind` wraps ONLY this evaluation block: a panicking predictor
    /// or any failing step is isolated from production, counted as a fault,
    /// and logged as one non-sensitive diagnostic line (decision id + reason
    /// only — never request payloads).
    pub fn evaluate_with(
        &self,
        request_id: &str,
        input: &ShadowInput,
        predictor: &dyn EnsemblePredictor,
    ) -> Option<ShadowDecision> {
        if !self.enabled {
            return None;
        }
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.evaluate_block(request_id, input, predictor)
        }));
        match outcome {
            Ok(Ok(decision)) => Some(decision),
            Ok(Err(reason)) => {
                self.record_fault(&input.decision_id, &reason);
                None
            }
            Err(panic) => {
                self.record_fault(&input.decision_id, &panic_reason(&panic));
                None
            }
        }
    }

    /// The evaluation block proper. Every fallible step returns `Err(reason)`.
    fn evaluate_block(
        &self,
        request_id: &str,
        input: &ShadowInput,
        predictor: &dyn EnsemblePredictor,
    ) -> Result<ShadowDecision, String> {
        if input.candidates.is_empty() {
            return Err("shadow input has no candidates".to_string());
        }
        if input.feature_schema != FEATURE_SCHEMA_VERSION {
            return Err(format!(
                "feature schema mismatch: {} != {}",
                input.feature_schema, FEATURE_SCHEMA_VERSION
            ));
        }
        if input.production_selected.is_empty() {
            return Err("shadow input has no planned production selection".to_string());
        }
        for candidate in &input.candidates {
            if candidate.candidate_id.is_empty() || candidate.provider_id.is_empty() {
                return Err("shadow input contains an empty candidate identity".to_string());
            }
            if candidate.features.schema_version != input.feature_schema {
                return Err(format!(
                    "candidate '{}' feature schema mismatch: {} != {}",
                    candidate.candidate_id, candidate.features.schema_version, input.feature_schema
                ));
            }
            if candidate
                .features
                .values
                .iter()
                .any(|value| !value.is_finite())
            {
                return Err(format!(
                    "candidate '{}' has a non-finite feature snapshot",
                    candidate.candidate_id
                ));
            }
            if candidate.eligible && candidate.rejection_reason.is_some() {
                return Err(format!(
                    "eligible candidate '{}' carries rejection evidence",
                    candidate.candidate_id
                ));
            }
        }

        let commit = predictor.commit_id();
        if commit.as_str().is_empty() {
            return Err("predictor returned an empty commit id".to_string());
        }

        // Predict once per candidate. The decision engine owns classification
        // (eligibility, finiteness) and utility computation; this layer only
        // records the evidence. Prediction identity is normalized back to the
        // immutable input identity so a faulty predictor cannot rewrite the
        // candidate key in the observation.
        let mut bundles: Vec<PredictionBundle> = Vec::with_capacity(input.candidates.len());
        let mut engine_candidates: Vec<EngineCandidate> =
            Vec::with_capacity(input.candidates.len());
        for candidate in &input.candidates {
            let mut bundle = predictor.predict(
                &candidate.candidate_id,
                &candidate.provider_id,
                &candidate.features,
            );
            bundle.candidate_model = candidate.candidate_id.clone();
            bundle.candidate_provider = candidate.provider_id.clone();
            engine_candidates.push(EngineCandidate {
                candidate_id: candidate.candidate_id.clone(),
                bundle: bundle.clone(),
                eligible: candidate.eligible,
            });
            bundles.push(bundle);
        }

        let engine_input = EngineInput {
            current_candidate: &input.production_selected,
            candidates: &engine_candidates,
            session_mode: input.session_mode,
            session_switch_count: input.session_switch_count,
            is_fallback: input.is_fallback,
        };
        // The production plan order is retained as evidence, but the shadow
        // path explicitly ranks valid candidates by ML utility.
        let output = self.engine.decide_with_ml_ranking(&engine_input);

        if output.candidates.len() != input.candidates.len() {
            return Err(format!(
                "decision engine returned {} outcomes for {} candidates",
                output.candidates.len(),
                input.candidates.len()
            ));
        }

        // Evidence in input order: identity comes from the snapshot,
        // classification and utility from the engine. Stored predictions stay
        // finite even when the engine rejected the raw bundle (store gate).
        let mut candidates: Vec<ShadowCandidate> = Vec::with_capacity(input.candidates.len());
        for ((candidate, bundle), outcome) in input
            .candidates
            .iter()
            .zip(bundles.iter())
            .zip(output.candidates.iter())
        {
            let rejection_reason = if !candidate.eligible {
                candidate
                    .rejection_reason
                    .clone()
                    .or_else(|| outcome.rejection_reason.clone())
            } else {
                outcome.rejection_reason.clone()
            };
            candidates.push(ShadowCandidate {
                candidate_id: candidate.candidate_id.clone(),
                provider_id: candidate.provider_id.clone(),
                tier: candidate.tier,
                eligible: candidate.eligible,
                valid: outcome.valid,
                rejection_reason,
                prediction: if outcome.valid {
                    bundle.clone()
                } else {
                    sanitized_bundle(bundle)
                },
                utility: outcome.utility.clone(),
            });
        }

        let decision = &output.decision;
        let decision_input_checksum = shadow_input_checksum(input, &commit);
        let decision_checksum = shadow_decision_checksum(
            decision_input_checksum,
            &candidates,
            &output.ranked_candidates,
            &decision.selected_candidate,
            decision.action,
            &decision.reason,
        );
        let observation = ShadowObservation {
            input: input.clone(),
            model_commit: commit.clone(),
        };
        let shadow_decision = ShadowDecision {
            shadow_id: format!("shadow-{}", uuid::Uuid::new_v4().simple()),
            timestamp: chrono::Utc::now().timestamp(),
            scope: ShadowScope::PolicyRouted,
            actual: ProductionDecisionRef {
                request_id: request_id.to_string(),
                decision_id: input.decision_id.clone(),
                selected: input.production_selected.clone(),
                // Final served identity is intentionally unknown in this pure
                // seam. The full integration node may attach it later.
                served: None,
            },
            observation,
            shadow: ShadowVerdict {
                model_commit: commit,
                feature_schema: input.feature_schema,
                selected: decision.selected_candidate.clone(),
                action: decision.action,
                reason: decision.reason.clone(),
                ranked_candidates: output.ranked_candidates.clone(),
            },
            candidates,
            decision_input_checksum,
            decision_checksum,
        };
        self.store
            .push(shadow_decision.clone())
            .map_err(|reason| format!("store rejected shadow decision: {}", reason))?;
        Ok(shadow_decision)
    }

    /// Count a fault and emit a one-line diagnostic (decision id + reason).
    fn record_fault(&self, decision_id: &str, reason: &str) {
        self.count_fault();
        tracing::debug!(decision_id = %decision_id, fault = %reason, "shadow evaluation fault");
    }

    /// Count one contained fault. Every step the shadow path can fail in —
    /// evaluation, storing, correlating — reports through this one counter.
    fn count_fault(&self) {
        self.faults.fetch_add(1, Ordering::Relaxed);
    }
}

/// Extract a human-readable reason from a caught panic payload.
fn panic_reason(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(reason) = panic.downcast_ref::<&str>() {
        format!("panic: {}", reason)
    } else if let Some(reason) = panic.downcast_ref::<String>() {
        format!("panic: {}", reason)
    } else {
        "panic: unknown payload".to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::DataOrigin;
    use crate::ml::coordinator::CoordinatorConfig;
    use crate::ml::dataset::Targets;
    use crate::ml::reward::RewardPolicy;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn features(seed: f32) -> RoutingFeatures {
        let mut values = [0.0f32; super::super::features::FEATURE_DIMENSION];
        for (i, value) in values.iter_mut().enumerate() {
            *value = seed + i as f32 * 0.01;
        }
        RoutingFeatures {
            values,
            schema_version: FEATURE_SCHEMA_VERSION,
        }
    }

    fn candidate_input(id: &str, provider: &str, seed: f32) -> ShadowCandidateInput {
        ShadowCandidateInput {
            candidate_id: id.to_string(),
            provider_id: provider.to_string(),
            tier: Some(ModelTier::Standard),
            eligible: true,
            features: features(seed),
            rejection_reason: None,
        }
    }

    fn shadow_input() -> ShadowInput {
        ShadowInput {
            decision_id: "dec-1".to_string(),
            policy_id: "policy-1".to_string(),
            client_id: None,
            policy_revision: PolicyRevision::default(),
            task: TaskProfileSummary::default(),
            production_selected: "model-a".to_string(),
            feature_schema: FEATURE_SCHEMA_VERSION,
            candidates: vec![
                candidate_input("model-a", "prov-a", 0.10),
                candidate_input("model-b", "prov-b", 0.20),
            ],
            session_mode: SessionRoutingMode::Free,
            session_switch_count: 0,
            is_fallback: false,
        }
    }

    fn training_samples(range: std::ops::Range<usize>) -> Vec<DatasetTrainingSample> {
        range
            .map(|i| {
                let mut values = [0.0f32; super::super::features::FEATURE_DIMENSION];
                for (j, value) in values.iter_mut().enumerate() {
                    *value = ((i * 7 + j * 13) % 100) as f32 / 100.0;
                }
                DatasetTrainingSample {
                    sample_id: format!("samp-{}", i),
                    schema_version: FEATURE_SCHEMA_VERSION,
                    timestamp: 1_000 + i as i64,
                    features: RoutingFeatures {
                        values,
                        schema_version: FEATURE_SCHEMA_VERSION,
                    },
                    targets: Targets {
                        success: i % 2 == 0,
                        latency_ms: if i % 2 == 0 {
                            Some(200.0 + i as f64)
                        } else {
                            None
                        },
                        ttft_ms: if i % 2 == 0 {
                            Some(50.0 + i as f64)
                        } else {
                            None
                        },
                        cost: Some(0.01 + i as f64 * 0.001),
                        failure_class: None,
                        fallback_count: 0,
                    },
                    provider_id: "prov-a".into(),
                    model_id: "model-a".into(),
                    origin: DataOrigin::Native,
                    outcome_id: format!("out-{}", i),
                    feedback: vec![],
                }
            })
            .collect()
    }

    fn valid_decision() -> ShadowDecision {
        let input = shadow_input();
        ShadowEngine::new(decision_engine(), true)
            .evaluate("req-1", &input)
            .expect("fixture evaluation should succeed")
    }

    fn store_push_err(decision: ShadowDecision) -> String {
        ShadowStore::new(10, 3600).push(decision).unwrap_err()
    }

    fn recompute_decision_checksum(decision: &mut ShadowDecision) {
        decision.decision_checksum = shadow_decision_checksum(
            decision.decision_input_checksum,
            &decision.candidates,
            &decision.shadow.ranked_candidates,
            &decision.shadow.selected,
            decision.shadow.action,
            &decision.shadow.reason,
        );
    }

    fn decision_engine() -> DecisionEngine {
        DecisionEngine::new(CoordinatorConfig::default(), RewardPolicy::default())
    }

    fn engine() -> ShadowEngine {
        ShadowEngine::new(decision_engine(), true)
    }

    // Fault-injecting predictor.
    enum FaultMode {
        Panic,
        Nan,
    }

    struct FaultyPredictor {
        mode: FaultMode,
        commit: CommitId,
    }

    impl EnsemblePredictor for FaultyPredictor {
        fn predict(
            &self,
            _model: &str,
            _provider: &str,
            _features: &RoutingFeatures,
        ) -> PredictionBundle {
            match self.mode {
                FaultMode::Panic => panic!("injected predictor fault"),
                FaultMode::Nan => PredictionBundle {
                    candidate_model: "faulty".to_string(),
                    candidate_provider: "faulty".to_string(),
                    success: Prediction {
                        value: f64::NAN,
                        confidence: 0.5,
                        sample_count: 1,
                        cold: false,
                    },
                    latency: Prediction {
                        value: 100.0,
                        confidence: 0.5,
                        sample_count: 1,
                        cold: false,
                    },
                    ttft: Prediction {
                        value: 50.0,
                        confidence: 0.5,
                        sample_count: 1,
                        cold: false,
                    },
                    cost: Prediction {
                        value: 0.01,
                        confidence: 0.5,
                        sample_count: 1,
                        cold: false,
                    },
                },
            }
        }

        fn commit_id(&self) -> CommitId {
            self.commit.clone()
        }
    }

    // -----------------------------------------------------------------------
    // ShadowStore.push validation
    // -----------------------------------------------------------------------

    #[test]
    fn store_push_accepts_valid_decision() {
        let store = ShadowStore::new(10, 3600);
        assert!(store.push(valid_decision()).is_ok());
        assert_eq!(store.len(), 1);
        assert!(!store.is_empty());
    }

    #[test]
    fn store_push_rejects_empty_request_id() {
        let mut decision = valid_decision();
        decision.actual.request_id.clear();
        assert_eq!(store_push_err(decision), "empty request_id");
    }

    #[test]
    fn store_push_rejects_empty_decision_id() {
        let mut decision = valid_decision();
        decision.actual.decision_id.clear();
        assert_eq!(store_push_err(decision), "empty decision_id");
    }

    #[test]
    fn store_push_rejects_empty_selected() {
        let mut decision = valid_decision();
        decision.actual.selected.clear();
        assert_eq!(store_push_err(decision), "empty production selected model");
    }

    #[test]
    fn store_push_rejects_empty_model_commit() {
        let mut decision = valid_decision();
        decision.shadow.model_commit = CommitId::new("");
        assert_eq!(store_push_err(decision), "empty model_commit");
    }

    #[test]
    fn store_push_rejects_wrong_feature_schema() {
        let mut decision = valid_decision();
        decision.shadow.feature_schema = FEATURE_SCHEMA_VERSION + 1;
        let err = store_push_err(decision);
        assert!(err.starts_with("feature schema mismatch"), "got: {}", err);
    }

    #[test]
    fn store_push_rejects_zero_input_checksum() {
        let mut decision = valid_decision();
        decision.decision_input_checksum = 0;
        assert_eq!(store_push_err(decision), "zero decision_input_checksum");
    }

    #[test]
    fn store_push_rejects_zero_decision_checksum() {
        let mut decision = valid_decision();
        decision.decision_checksum = 0;
        assert_eq!(store_push_err(decision), "zero decision_checksum");
    }

    #[test]
    fn store_push_rejects_nan_prediction() {
        let mut decision = valid_decision();
        decision.candidates[0].prediction.success.value = f64::NAN;
        let err = store_push_err(decision);
        assert!(
            err.contains("non-finite success prediction"),
            "got: {}",
            err
        );
    }

    #[test]
    fn store_push_rejects_nan_utility() {
        let mut decision = valid_decision();
        decision.candidates[0].utility.total = f64::NAN;
        let err = store_push_err(decision);
        assert!(err.contains("non-finite total utility"), "got: {}", err);
    }

    #[test]
    fn store_push_rejects_empty_candidates() {
        let mut decision = valid_decision();
        decision.candidates.clear();
        assert_eq!(store_push_err(decision), "no candidates");
    }

    #[test]
    fn store_rejects_unknown_selected_before_checksum() {
        let mut decision = valid_decision();
        decision.shadow.selected = "unknown-model".to_string();
        decision.shadow.action = RoutingAction::Switch;
        recompute_decision_checksum(&mut decision);

        let error = store_push_err(decision);
        assert_eq!(
            error,
            "unknown shadow selected identity 'unknown-model': candidate evidence is missing"
        );
    }

    #[test]
    fn store_rejects_keep_selected_that_is_not_planned() {
        let mut decision = valid_decision();
        decision.shadow.selected = "model-b".to_string();
        decision.shadow.action = RoutingAction::Keep;
        recompute_decision_checksum(&mut decision);

        assert_eq!(
            store_push_err(decision),
            "Keep action must select the planned production identity"
        );
    }

    #[test]
    fn store_rejects_switch_selected_outside_ranked_valid_set() {
        let mut decision = valid_decision();
        decision.shadow.selected = "model-a".to_string();
        decision.shadow.action = RoutingAction::Switch;
        decision.shadow.ranked_candidates = vec!["model-b".to_string()];
        recompute_decision_checksum(&mut decision);

        assert_eq!(
            store_push_err(decision),
            "Switch action selected identity 'model-a' is not in the ranked valid set"
        );
    }

    #[test]
    fn store_rejects_explore_selected_invalid_candidate() {
        let mut input = shadow_input();
        input.candidates[1].eligible = false;
        input.candidates[1].rejection_reason = Some("policy rejected".to_string());
        let mut decision = engine()
            .evaluate("req-explore-invalid", &input)
            .expect("baseline decision");
        decision.shadow.selected = "model-b".to_string();
        decision.shadow.action = RoutingAction::Explore;
        recompute_decision_checksum(&mut decision);

        assert_eq!(
            store_push_err(decision),
            "Explore action selected identity 'model-b' must be eligible and valid"
        );
    }

    #[test]
    fn store_allows_keep_for_invalid_planned_identity() {
        let mut input = shadow_input();
        for candidate in &mut input.candidates {
            candidate.eligible = false;
            candidate.rejection_reason = Some("policy rejected".to_string());
        }
        let decision = engine()
            .evaluate("req-keep-invalid", &input)
            .expect("invalid planned identity is still explicit Keep evidence");
        assert_eq!(decision.shadow.action, RoutingAction::Keep);
        assert_eq!(decision.shadow.selected, decision.actual.selected);
        assert!(decision.candidates.iter().all(|candidate| !candidate.valid));
        assert!(ShadowStore::new(2, 3600).push(decision).is_ok());
    }

    #[test]
    fn store_push_evicts_oldest_at_capacity() {
        let store = ShadowStore::new(2, 3600);
        let first = valid_decision();
        let first_id = first.shadow_id.clone();
        store.push(first).unwrap();
        store.push(valid_decision()).unwrap();
        assert_eq!(store.len(), 2);
        store.push(valid_decision()).unwrap();
        assert_eq!(store.len(), 2);
        let all = store.decisions();
        assert_eq!(all.len(), 2);
        assert_ne!(
            all[0].shadow_id, first_id,
            "oldest decision must be evicted"
        );
    }

    #[test]
    fn store_decisions_filters_by_age() {
        let store = ShadowStore::new(10, 3600);
        let mut stale = valid_decision();
        stale.timestamp = chrono::Utc::now().timestamp() - 7200; // 2h old, 1h window
        store.push(stale).unwrap();
        store.push(valid_decision()).unwrap();
        assert_eq!(store.len(), 2, "len ignores age");
        assert_eq!(
            store.decisions().len(),
            1,
            "decisions() applies the age filter"
        );
    }

    #[test]
    fn store_clear_empties() {
        let store = ShadowStore::new(10, 3600);
        store.push(valid_decision()).unwrap();
        assert!(!store.is_empty());
        store.clear();
        assert!(store.is_empty());
    }

    #[test]
    fn store_default_matches_dataset_retention() {
        let store = ShadowStore::default();
        assert!(store.is_empty());
        store.push(valid_decision()).unwrap();
        assert_eq!(store.decisions().len(), 1);
    }

    // -----------------------------------------------------------------------
    // Checksums
    // -----------------------------------------------------------------------

    #[test]
    fn input_checksum_deterministic_for_same_input() {
        let input = shadow_input();
        let commit = CommitId::new("abc123");
        assert_eq!(
            shadow_input_checksum(&input, &commit),
            shadow_input_checksum(&input, &commit)
        );
        assert_ne!(shadow_input_checksum(&input, &commit), 0);
    }

    #[test]
    fn input_checksum_ignores_decision_id() {
        // The input checksum identities the decision INPUTS, not the request.
        let commit = CommitId::new("abc123");
        let mut input = shadow_input();
        let baseline = shadow_input_checksum(&input, &commit);
        input.decision_id = "dec-other".to_string();
        assert_eq!(shadow_input_checksum(&input, &commit), baseline);
    }

    #[test]
    fn input_checksum_changes_on_single_feature_bit() {
        let commit = CommitId::new("abc123");
        let mut input = shadow_input();
        let baseline = shadow_input_checksum(&input, &commit);
        input.candidates[0].features.values[17] += 0.001;
        assert_ne!(shadow_input_checksum(&input, &commit), baseline);
    }

    #[test]
    fn input_checksum_changes_on_candidate_id() {
        let commit = CommitId::new("abc123");
        let mut input = shadow_input();
        let baseline = shadow_input_checksum(&input, &commit);
        input.candidates[0].candidate_id = "model-z".to_string();
        assert_ne!(shadow_input_checksum(&input, &commit), baseline);
    }

    #[test]
    fn input_checksum_changes_on_production_selected() {
        let commit = CommitId::new("abc123");
        let mut input = shadow_input();
        let baseline = shadow_input_checksum(&input, &commit);
        input.production_selected = "model-z".to_string();
        assert_ne!(shadow_input_checksum(&input, &commit), baseline);
    }

    #[test]
    fn input_checksum_changes_on_session_mode() {
        let commit = CommitId::new("abc123");
        let mut input = shadow_input();
        let baseline = shadow_input_checksum(&input, &commit);
        input.session_mode = SessionRoutingMode::Pinned;
        assert_ne!(shadow_input_checksum(&input, &commit), baseline);
    }

    #[test]
    fn input_checksum_changes_on_model_commit() {
        let input = shadow_input();
        assert_ne!(
            shadow_input_checksum(&input, &CommitId::new("commit-a")),
            shadow_input_checksum(&input, &CommitId::new("commit-b"))
        );
    }

    #[test]
    fn decision_checksum_changes_when_predictions_change() {
        let mut decision = valid_decision();
        let baseline = shadow_decision_checksum(
            decision.decision_input_checksum,
            &decision.candidates,
            &decision.shadow.ranked_candidates,
            &decision.shadow.selected,
            decision.shadow.action,
            &decision.shadow.reason,
        );
        // Same input checksum, one prediction changed -> different decision.
        decision.candidates[0].prediction.success.value += 0.05;
        let changed = shadow_decision_checksum(
            decision.decision_input_checksum,
            &decision.candidates,
            &decision.shadow.ranked_candidates,
            &decision.shadow.selected,
            decision.shadow.action,
            &decision.shadow.reason,
        );
        assert_ne!(changed, baseline);
    }

    #[test]
    fn decision_checksum_changes_when_verdict_changes() {
        let decision = valid_decision();
        let baseline = shadow_decision_checksum(
            decision.decision_input_checksum,
            &decision.candidates,
            &decision.shadow.ranked_candidates,
            &decision.shadow.selected,
            decision.shadow.action,
            &decision.shadow.reason,
        );
        let switched = shadow_decision_checksum(
            decision.decision_input_checksum,
            &decision.candidates,
            &decision.shadow.ranked_candidates,
            "model-b",
            RoutingAction::Switch,
            &decision.shadow.reason,
        );
        assert_ne!(switched, baseline);
    }

    #[test]
    fn decision_checksum_stable_across_evaluations() {
        // Two evaluations of the same input: different shadow ids (and
        // possibly timestamps), identical checksums — volatile fields are
        // not part of either checksum.
        let engine = engine();
        let input = shadow_input();
        let first = engine.evaluate("req-1", &input).unwrap();
        let second = engine.evaluate("req-1", &input).unwrap();
        assert_ne!(first.shadow_id, second.shadow_id);
        assert_eq!(
            first.decision_input_checksum,
            second.decision_input_checksum
        );
        assert_eq!(first.decision_checksum, second.decision_checksum);
        assert_eq!(engine.store().len(), 2);
    }

    // -----------------------------------------------------------------------
    // ModelEnsemblePredictor
    // -----------------------------------------------------------------------

    #[test]
    fn genesis_predictor_is_deterministic() {
        let a = ModelEnsemblePredictor::genesis();
        let b = ModelEnsemblePredictor::genesis();
        assert_eq!(a.commit(), b.commit());
        assert!(!a.commit().as_str().is_empty());
    }

    #[test]
    fn from_commit_round_trips_predictions() {
        let mut ensemble = ModelEnsemble::new();
        for sample in &training_samples(0..10) {
            ensemble.update_all(sample);
        }
        let checkpoint = ensemble.save_all();
        let commit = ModelCommit::new(ModelId::new("shadow"), checkpoint.clone(), None, 0);
        let predictor = ModelEnsemblePredictor::from_model_commit(&commit)
            .expect("round-trip commit must load");
        assert_eq!(predictor.commit(), commit.commit_id);

        let features = features(0.5);
        let expected = ensemble.success.predict(&features);
        let bundle = EnsemblePredictor::predict(&predictor, "model-a", "prov-a", &features);
        assert_eq!(bundle.candidate_model, "model-a");
        assert_eq!(bundle.candidate_provider, "prov-a");
        assert!(
            (bundle.success.value - expected.value).abs() < 1e-10,
            "success prediction changed across the checkpoint round-trip: {} vs {}",
            bundle.success.value,
            expected.value
        );
        assert_eq!(bundle.success.sample_count, expected.sample_count);
    }

    #[test]
    fn predictor_train_never_mutates_the_snapshot() {
        let predictor = ModelEnsemblePredictor::genesis();
        let checkpoint_before = predictor.ensemble_checkpoint();
        let (_ensemble, commit) = predictor.train(&training_samples(0..5));
        assert_ne!(commit, predictor.commit());
        assert_eq!(
            predictor.ensemble_checkpoint().content_hash(),
            checkpoint_before.content_hash(),
            "train must not mutate the held snapshot"
        );
    }

    // -----------------------------------------------------------------------
    // ShadowEngine::evaluate
    // -----------------------------------------------------------------------

    #[test]
    fn evaluate_happy_path_records_full_decision() {
        let engine = engine();
        let input = shadow_input();
        let decision = engine
            .evaluate("req-1", &input)
            .expect("evaluation succeeds");
        assert_eq!(decision.scope, ShadowScope::PolicyRouted);
        assert!(decision.shadow_id.starts_with("shadow-"));
        assert_eq!(decision.actual.request_id, "req-1");
        assert_eq!(decision.actual.decision_id, "dec-1");
        assert_eq!(decision.actual.selected, "model-a");
        assert!(!decision.shadow.model_commit.as_str().is_empty());
        assert_eq!(decision.shadow.feature_schema, FEATURE_SCHEMA_VERSION);
        assert!(!decision.shadow.selected.is_empty());
        assert!(!decision.shadow.reason.is_empty());
        assert_eq!(decision.candidates.len(), 2);
        assert_eq!(decision.candidates[0].candidate_id, "model-a");
        assert!(decision.candidates.iter().all(|c| c.valid));
        assert!(decision
            .candidates
            .iter()
            .all(|c| c.rejection_reason.is_none()));
        assert_ne!(decision.decision_input_checksum, 0);
        assert_ne!(decision.decision_checksum, 0);
        assert_eq!(engine.fault_count(), 0);
        assert_eq!(engine.store().len(), 1);
    }

    #[test]
    fn disabled_engine_skips_evaluation_without_fault() {
        let engine = ShadowEngine::new(decision_engine(), false);
        assert!(!engine.enabled());
        assert!(engine.evaluate("req-1", &shadow_input()).is_none());
        assert_eq!(engine.fault_count(), 0);
        assert!(engine.store().is_empty());
    }

    #[test]
    fn empty_candidate_input_is_a_fault() {
        let engine = engine();
        let mut input = shadow_input();
        input.candidates.clear();
        assert!(engine.evaluate("req-1", &input).is_none());
        assert_eq!(engine.fault_count(), 1);
        assert!(engine.store().is_empty());
    }

    #[test]
    fn ineligible_candidates_are_recorded_with_reason() {
        let engine = engine();
        let mut input = shadow_input();
        input.candidates[0].eligible = false;
        let decision = engine.evaluate("req-1", &input).unwrap();
        assert!(!decision.candidates[0].eligible);
        assert!(!decision.candidates[0].valid);
        assert_eq!(
            decision.candidates[0].rejection_reason.as_deref(),
            Some("ineligible")
        );
        assert_eq!(decision.candidates[1].rejection_reason, None);
        assert_eq!(engine.fault_count(), 0);
    }

    // -----------------------------------------------------------------------
    // Fault isolation (EnsemblePredictor seam)
    // -----------------------------------------------------------------------

    #[test]
    fn panicking_predictor_is_isolated_from_production() {
        let engine = engine();
        let faulty = FaultyPredictor {
            mode: FaultMode::Panic,
            commit: CommitId::new("fault-1"),
        };
        let decision = engine.evaluate_with("req-1", &shadow_input(), &faulty);
        assert!(decision.is_none());
        assert_eq!(engine.fault_count(), 1);
        assert!(engine.store().is_empty());
    }

    #[test]
    fn nan_predictions_are_sanitized_not_faulted() {
        let engine = engine();
        let faulty = FaultyPredictor {
            mode: FaultMode::Nan,
            commit: CommitId::new("fault-2"),
        };
        let decision = engine
            .evaluate_with("req-1", &shadow_input(), &faulty)
            .expect("sanitized evaluation still records a decision");
        assert_eq!(engine.fault_count(), 0, "sanitization is not a fault");
        assert_eq!(engine.store().len(), 1);
        assert_eq!(decision.candidates.len(), 2);
        for candidate in &decision.candidates {
            assert!(!candidate.valid);
            assert_eq!(
                candidate.rejection_reason.as_deref(),
                Some("non-finite prediction")
            );
            assert!(candidate.prediction.success.value.is_finite());
            assert!(candidate.utility.total.is_finite());
        }
    }

    // -----------------------------------------------------------------------
    // Commit chain: genesis -> train advance -> replay equivalence
    // -----------------------------------------------------------------------

    #[test]
    fn train_advances_commit_beyond_genesis() {
        let engine = engine();
        let input = shadow_input();
        let genesis_decision = engine.evaluate("req-1", &input).unwrap();

        let first = engine.train(&training_samples(0..5));
        assert_ne!(first, genesis_decision.shadow.model_commit);

        let second = engine.train(&training_samples(5..10));
        assert_ne!(second, first);

        // The store carries the commit id of the era each decision was made in.
        let after = engine.evaluate("req-2", &input).unwrap();
        assert_eq!(after.shadow.model_commit, second);
        let decisions = engine.store().decisions();
        assert_eq!(decisions.len(), 2);
        assert_eq!(
            decisions[0].shadow.model_commit,
            genesis_decision.shadow.model_commit
        );
        assert_eq!(decisions[1].shadow.model_commit, second);
    }

    #[test]
    fn train_swap_is_visible_to_subsequent_evaluations() {
        let engine = engine();
        let input = shadow_input();
        let before = engine.evaluate("req-1", &input).unwrap();
        let new_commit = engine.train(&training_samples(0..4));
        let after = engine.evaluate("req-2", &input).unwrap();
        assert_eq!(after.shadow.model_commit, new_commit);
        assert_ne!(after.shadow.model_commit, before.shadow.model_commit);
        // Different commit -> different input checksum -> different decision.
        assert_ne!(
            after.decision_input_checksum,
            before.decision_input_checksum
        );
    }

    #[test]
    fn train_chain_equals_batch_training() {
        let samples = training_samples(0..10);
        let (a, b) = samples.split_at(5);

        // Path 1: incremental chain.
        let chained = engine();
        chained.train(a);
        let chained_commit = chained.train(b);

        // Path 2: single batch on a fresh engine.
        let batched = engine();
        let batched_commit = batched.train(&samples);

        // Deterministic chain: same content hash -> same commit id.
        assert_eq!(chained_commit, batched_commit);
        assert_eq!(
            crate::sync::read(&chained.predictor)
                .ensemble_checkpoint()
                .content_hash(),
            crate::sync::read(&batched.predictor)
                .ensemble_checkpoint()
                .content_hash()
        );

        // Same input -> same decision checksum on both engines.
        let input = shadow_input();
        let chained_decision = chained.evaluate("req-1", &input).unwrap();
        let batched_decision = batched.evaluate("req-1", &input).unwrap();
        assert_eq!(
            chained_decision.decision_input_checksum,
            batched_decision.decision_input_checksum
        );
        assert_eq!(
            chained_decision.decision_checksum,
            batched_decision.decision_checksum
        );
    }

    // -----------------------------------------------------------------------
    // Serde
    // -----------------------------------------------------------------------

    #[test]
    fn shadow_decision_serde_round_trip() {
        let decision = valid_decision();
        let json = serde_json::to_string(&decision).unwrap();
        let restored: ShadowDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.shadow_id, decision.shadow_id);
        assert_eq!(restored.decision_checksum, decision.decision_checksum);
        assert_eq!(restored.shadow.model_commit, decision.shadow.model_commit);
        assert_eq!(restored.candidates.len(), decision.candidates.len());
    }

    #[test]
    fn shadow_input_serde_round_trip() {
        let input = shadow_input();
        let json = serde_json::to_string(&input).unwrap();
        let restored: ShadowInput = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.decision_id, input.decision_id);
        assert_eq!(restored.candidates.len(), input.candidates.len());
        assert_eq!(
            restored.candidates[0].features.values,
            input.candidates[0].features.values
        );
    }
}
