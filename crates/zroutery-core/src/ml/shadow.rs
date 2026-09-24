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

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

use super::coordinator::RoutingAction;
use super::dataset::TrainingSample as DatasetTrainingSample;
use super::decision_engine::{DecisionEngine, EngineCandidate, EngineInput};
use super::features::{
    FeatureContext, RoutingFeatures, extract_features, FEATURE_SCHEMA_VERSION,
};
use super::model::{Prediction, RoutingModel};
use super::model_identity::{
    CommitId, ModelCheckpoint, ModelCommit, ModelEnsemble, ModelId, ReplayError,
};
use super::reward::{PredictionBundle, UtilityBreakdown};
use crate::config::ModelTier;
use crate::observation::ObservationStore;
use crate::policy::{RouteDecision, TaskProfile};
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

/// Mix the bit pattern of an `f32` into `hash`.
fn fnv_mix_f32(hash: &mut u64, value: f32) {
    fnv_mix_u64(hash, &value.to_bits().to_le_bytes());
}

/// Mix the bit pattern of an `f64` into `hash`.
fn fnv_mix_f64(hash: &mut u64, value: f64) {
    fnv_mix_u64(hash, &value.to_bits().to_le_bytes());
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
/// Mixes the model commit, feature schema, production selection, every
/// candidate (identity, tier, eligibility, feature bits), and the session
/// context. Volatile fields (timestamp, shadow id, request/decision ids) are
/// excluded: the same input always yields the same checksum.
fn shadow_input_checksum(input: &ShadowInput, model_commit: &CommitId) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    fnv_mix_u64(&mut hash, model_commit.as_str().as_bytes());
    fnv_mix_u64(&mut hash, &input.feature_schema.to_le_bytes());
    fnv_mix_u64(&mut hash, input.production_selected.as_bytes());
    for candidate in &input.candidates {
        fnv_mix_u64(&mut hash, candidate.candidate_id.as_bytes());
        fnv_mix_u64(&mut hash, candidate.provider_id.as_bytes());
        fnv_mix_u64(&mut hash, &[tier_discriminant(candidate.tier)]);
        fnv_mix_u64(&mut hash, &[u8::from(candidate.eligible)]);
        for value in &candidate.features.values {
            fnv_mix_f32(&mut hash, *value);
        }
    }
    fnv_mix_u64(&mut hash, &[session_mode_discriminant(input.session_mode)]);
    fnv_mix_u64(&mut hash, &input.session_switch_count.to_le_bytes());
    fnv_mix_u64(&mut hash, &[u8::from(input.is_fallback)]);
    hash
}

/// Identity of the resulting decision: the input checksum plus the
/// per-candidate predictions, utilities, and the verdict itself.
fn shadow_decision_checksum(
    input_checksum: u64,
    candidates: &[ShadowCandidate],
    selected: &str,
    action: RoutingAction,
    reason: &str,
) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    fnv_mix_u64(&mut hash, &input_checksum.to_le_bytes());
    for candidate in candidates {
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
    fnv_mix_u64(&mut hash, selected.as_bytes());
    fnv_mix_u64(&mut hash, &[action_discriminant(action)]);
    fnv_mix_u64(&mut hash, reason.as_bytes());
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

/// What production actually did (correlation triple — all non-Option by
/// design).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductionDecisionRef {
    pub request_id: String,
    pub decision_id: String,
    /// Model id production picked.
    pub selected: String,
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

/// One recorded shadow decision: production's choice, the hypothetical ML
/// verdict, and the evidence between them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowDecision {
    /// `shadow-<uuid simple>`.
    pub shadow_id: String,
    pub timestamp: i64,
    pub scope: ShadowScope,
    pub actual: ProductionDecisionRef,
    pub shadow: ShadowVerdict,
    pub candidates: Vec<ShadowCandidate>,
    /// Identity of the INPUT snapshot.
    pub decision_input_checksum: u64,
    /// Identity of the resulting decision.
    pub decision_checksum: u64,
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
    pub features: RoutingFeatures,
}

/// Immutable input snapshot — the engine NEVER touches runtime stores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowInput {
    pub decision_id: String,
    pub production_selected: String,
    pub feature_schema: u32,
    pub candidates: Vec<ShadowCandidateInput>,
    pub session_mode: SessionRoutingMode,
    pub session_switch_count: u32,
    pub is_fallback: bool,
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
        let candidates: Vec<ShadowCandidateInput> = plan
            .iter()
            .map(|candidate| {
                let entry = &candidate.entry;
                // Production ranks by the exposed id, and the decision trace
                // keys its per-candidate eligibility on the same id.
                let eligible = decision
                    .candidates
                    .iter()
                    .any(|c| c.model_id == candidate.exposed_id && c.eligible);
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
                }
            })
            .collect();
        Self {
            decision_id: decision.decision_id.clone(),
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
/// pinned to a [`CommitId`].
pub struct ModelEnsemblePredictor {
    ensemble: Arc<ModelEnsemble>,
    commit: CommitId,
}

impl ModelEnsemblePredictor {
    /// The genesis predictor: a cold-start ensemble committed as the root of
    /// the "shadow" model lineage.
    ///
    /// The commit id is derived from the checkpoint content hash, so two
    /// genesis predictors always share the same (deterministic) commit id.
    pub fn genesis() -> Self {
        let ensemble = ModelEnsemble::new();
        let commit = ModelCommit::new(ModelId::new("shadow"), ensemble.save_all(), None, 0);
        Self {
            ensemble: Arc::new(ensemble),
            commit: commit.commit_id,
        }
    }

    /// Build a predictor from a checkpoint and its commit id.
    ///
    /// Returns [`ReplayError`] when the checkpoint cannot be loaded — the
    /// commit id and the ensemble must never disagree, so a corrupt or
    /// foreign checkpoint surfaces as a `Result` (the caller keeps running
    /// on its previous predictor) instead of a panic.
    pub fn from_commit(checkpoint: &ModelCheckpoint, commit: CommitId) -> Result<Self, ReplayError> {
        let ensemble = ModelEnsemble::load_all(checkpoint)?;
        Ok(Self {
            ensemble: Arc::new(ensemble),
            commit,
        })
    }

    /// Pure training: clone the current ensemble state (through a checkpoint
    /// round-trip), apply `samples`, and return the new ensemble with its
    /// commit id. NEVER mutates `self`.
    ///
    /// The new commit's parent is the current commit, so repeated `train`
    /// calls build a deterministic chain equivalent to replaying all samples
    /// from genesis.
    pub fn train(&self, samples: &[DatasetTrainingSample]) -> (ModelEnsemble, CommitId) {
        // ModelEnsemble is not Clone — clone through a checkpoint round-trip.
        let checkpoint = self.ensemble.save_all();
        let mut ensemble =
            ModelEnsemble::load_all(&checkpoint).expect("shadow ensemble checkpoint is loadable");
        for sample in samples {
            ensemble.update_all(sample);
        }
        let commit = ModelCommit::new(
            ModelId::new("shadow"),
            ensemble.save_all(),
            Some(self.commit.clone()),
            samples.len() as u64,
        );
        (ensemble, commit.commit_id)
    }

    /// Checkpoint of the held ensemble snapshot.
    pub fn ensemble_checkpoint(&self) -> ModelCheckpoint {
        self.ensemble.save_all()
    }

    /// Commit id of the held ensemble snapshot.
    pub fn commit(&self) -> CommitId {
        self.commit.clone()
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
        self.commit.clone()
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
    /// Rejects (with a reason) any decision that:
    /// - has an empty request id, decision id, or production-selected model,
    /// - has an empty model commit,
    /// - carries a feature schema other than [`FEATURE_SCHEMA_VERSION`],
    /// - has a zero input or decision checksum,
    /// - carries a non-finite prediction field or utility component on any
    ///   candidate,
    /// - has no candidates.
    ///
    /// On success the decision is pushed, evicting the oldest when at
    /// capacity.
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
        if decision.shadow.model_commit.as_str().is_empty() {
            return Err("empty model_commit".to_string());
        }
        if decision.shadow.feature_schema != FEATURE_SCHEMA_VERSION {
            return Err(format!(
                "feature schema mismatch: {} != {}",
                decision.shadow.feature_schema, FEATURE_SCHEMA_VERSION
            ));
        }
        if decision.decision_input_checksum == 0 {
            return Err("zero decision_input_checksum".to_string());
        }
        if decision.decision_checksum == 0 {
            return Err("zero decision_checksum".to_string());
        }
        for candidate in &decision.candidates {
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
        if decision.candidates.is_empty() {
            return Err("no candidates".to_string());
        }
        let mut decisions = crate::sync::lock(&self.decisions);
        if decisions.len() >= self.max_decisions {
            decisions.pop_front();
        }
        decisions.push_back(decision);
        Ok(())
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

    /// Advance the shadow ensemble (TEST/7E-2 seam — NOT wired to production
    /// outcomes in 7E-1).
    ///
    /// Trains a copy of the current ensemble, swaps the predictor in under
    /// the write lock, and returns the new commit id.
    pub fn train(&self, samples: &[DatasetTrainingSample]) -> CommitId {
        let current = crate::sync::read(&self.predictor).clone();
        let (ensemble, commit) = current.train(samples);
        *crate::sync::write(&self.predictor) = Arc::new(ModelEnsemblePredictor {
            ensemble: Arc::new(ensemble),
            commit: commit.clone(),
        });
        commit
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
        let commit = predictor.commit_id();
        if commit.as_str().is_empty() {
            return Err("predictor returned an empty commit id".to_string());
        }

        // Predict once per candidate. The decision engine owns classification
        // (eligibility, finiteness) and utility computation; this layer only
        // records the evidence.
        let mut bundles: Vec<PredictionBundle> = Vec::with_capacity(input.candidates.len());
        let mut engine_candidates: Vec<EngineCandidate> =
            Vec::with_capacity(input.candidates.len());
        for candidate in &input.candidates {
            let bundle = predictor.predict(
                &candidate.candidate_id,
                &candidate.provider_id,
                &candidate.features,
            );
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
        let output = self.engine.decide(&engine_input);

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
            candidates.push(ShadowCandidate {
                candidate_id: candidate.candidate_id.clone(),
                provider_id: candidate.provider_id.clone(),
                tier: candidate.tier,
                eligible: candidate.eligible,
                valid: outcome.valid,
                rejection_reason: outcome.rejection_reason.clone(),
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
            &decision.selected_candidate,
            decision.action,
            &decision.reason,
        );
        let shadow_decision = ShadowDecision {
            shadow_id: format!("shadow-{}", uuid::Uuid::new_v4().simple()),
            timestamp: chrono::Utc::now().timestamp(),
            scope: ShadowScope::PolicyRouted,
            actual: ProductionDecisionRef {
                request_id: request_id.to_string(),
                decision_id: input.decision_id.clone(),
                selected: input.production_selected.clone(),
            },
            shadow: ShadowVerdict {
                model_commit: commit,
                feature_schema: input.feature_schema,
                selected: decision.selected_candidate.clone(),
                action: decision.action,
                reason: decision.reason.clone(),
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
        self.faults.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(decision_id = %decision_id, fault = %reason, "shadow evaluation fault");
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
    use crate::ml::reward::{compute_utility, RewardPolicy};

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
        }
    }

    fn shadow_input() -> ShadowInput {
        ShadowInput {
            decision_id: "dec-1".to_string(),
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
        let predictor = ModelEnsemblePredictor::genesis();
        let commit = predictor.commit();
        let bundle = predictor.predict("model-a", "prov-a", &input.candidates[0].features);
        let utility = compute_utility(&bundle, &RewardPolicy::default(), false, 0);
        let candidates = vec![ShadowCandidate {
            candidate_id: "model-a".to_string(),
            provider_id: "prov-a".to_string(),
            tier: Some(ModelTier::Standard),
            eligible: true,
            valid: true,
            rejection_reason: None,
            prediction: bundle,
            utility,
        }];
        let input_checksum = shadow_input_checksum(&input, &commit);
        let decision_checksum = shadow_decision_checksum(
            input_checksum,
            &candidates,
            "model-a",
            RoutingAction::Keep,
            "test",
        );
        ShadowDecision {
            shadow_id: format!("shadow-{}", uuid::Uuid::new_v4().simple()),
            timestamp: chrono::Utc::now().timestamp(),
            scope: ShadowScope::PolicyRouted,
            actual: ProductionDecisionRef {
                request_id: "req-1".to_string(),
                decision_id: "dec-1".to_string(),
                selected: "model-a".to_string(),
            },
            shadow: ShadowVerdict {
                model_commit: commit,
                feature_schema: FEATURE_SCHEMA_VERSION,
                selected: "model-a".to_string(),
                action: RoutingAction::Keep,
                reason: "test".to_string(),
            },
            candidates,
            decision_input_checksum: input_checksum,
            decision_checksum,
        }
    }

    fn store_push_err(decision: ShadowDecision) -> String {
        ShadowStore::new(10, 3600).push(decision).unwrap_err()
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
        assert!(err.contains("non-finite success prediction"), "got: {}", err);
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
        assert_ne!(all[0].shadow_id, first_id, "oldest decision must be evicted");
    }

    #[test]
    fn store_decisions_filters_by_age() {
        let store = ShadowStore::new(10, 3600);
        let mut stale = valid_decision();
        stale.timestamp = chrono::Utc::now().timestamp() - 7200; // 2h old, 1h window
        store.push(stale).unwrap();
        store.push(valid_decision()).unwrap();
        assert_eq!(store.len(), 2, "len ignores age");
        assert_eq!(store.decisions().len(), 1, "decisions() applies the age filter");
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
            &decision.shadow.selected,
            decision.shadow.action,
            &decision.shadow.reason,
        );
        // Same input checksum, one prediction changed -> different decision.
        decision.candidates[0].prediction.success.value += 0.05;
        let changed = shadow_decision_checksum(
            decision.decision_input_checksum,
            &decision.candidates,
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
            &decision.shadow.selected,
            decision.shadow.action,
            &decision.shadow.reason,
        );
        let switched = shadow_decision_checksum(
            decision.decision_input_checksum,
            &decision.candidates,
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
        assert_eq!(first.decision_input_checksum, second.decision_input_checksum);
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
        let commit = CommitId::from_hash(checkpoint.content_hash());
        let predictor = ModelEnsemblePredictor::from_commit(&checkpoint, commit.clone())
            .expect("round-trip checkpoint must load");
        assert_eq!(predictor.commit(), commit);

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
        let decision = engine.evaluate("req-1", &input).expect("evaluation succeeds");
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
        assert_ne!(after.decision_input_checksum, before.decision_input_checksum);
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
        assert_eq!(
            restored.shadow.model_commit,
            decision.shadow.model_commit
        );
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
