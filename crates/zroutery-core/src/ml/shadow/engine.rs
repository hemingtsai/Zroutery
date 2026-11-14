use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::ml::dataset::TrainingSample as DatasetTrainingSample;
use crate::ml::decision_engine::{DecisionEngine, EngineCandidate, EngineInput};
use crate::ml::features::FEATURE_SCHEMA_VERSION;
use crate::ml::model_identity::{CommitId, ModelCheckpoint, ModelCommit, ReplayError};
use crate::ml::reward::PredictionBundle;

use super::predictor::{EnsemblePredictor, ModelEnsemblePredictor};
use super::records::{
    shadow_decision_checksum, shadow_input_checksum, ProductionDecisionRef, ShadowCandidate,
    ShadowDecision, ShadowInput, ShadowObservation, ShadowScope, ShadowVerdict,
};
use super::store::{sanitized_bundle, ShadowStore, DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_DECISIONS};

// ---------------------------------------------------------------------------
// ShadowEngine
// ---------------------------------------------------------------------------

/// Shadow decision engine — evaluates what the ML stack would have routed,
/// without influencing production.
///
/// Holds an immutable [`DecisionEngine`] plus a swappable ensemble predictor.
/// Readers clone the [`Arc`] under the read lock and then predict lock-free;
/// only [`ShadowEngine::train`] takes the write lock, and training runs are
/// serialised by `training` so a read-modify-write of the predictor cannot lose
/// an update.
///
/// The master switch and the store retention are configuration, not
/// construction constants: [`ShadowEngine::reconfigure`] applies a changed
/// `ShadowConfig` to an already-running engine.
pub struct ShadowEngine {
    engine: DecisionEngine,
    pub(super) predictor: RwLock<Arc<ModelEnsemblePredictor>>,
    store: ShadowStore,
    enabled: AtomicBool,
    faults: AtomicU64,
    /// Serialises the read-train-write of [`ShadowEngine::try_train`]. Training
    /// itself stays off the predictor lock (readers never block behind it), so
    /// without this a second concurrent run would start from the same commit
    /// and overwrite the first run's update.
    training: Mutex<()>,
}

impl ShadowEngine {
    pub fn new(engine: DecisionEngine, enabled: bool) -> Self {
        Self::with_retention(engine, enabled, DEFAULT_MAX_DECISIONS, DEFAULT_MAX_AGE_SECS)
    }

    /// Build an engine whose store starts with the given retention limits,
    /// e.g. the configured `ShadowConfig::max_decisions` / `max_age_secs`.
    pub fn with_retention(
        engine: DecisionEngine,
        enabled: bool,
        max_decisions: usize,
        max_age_secs: i64,
    ) -> Self {
        Self {
            engine,
            predictor: RwLock::new(Arc::new(ModelEnsemblePredictor::genesis())),
            store: ShadowStore::new(max_decisions, max_age_secs),
            enabled: AtomicBool::new(enabled),
            faults: AtomicU64::new(0),
            training: Mutex::new(()),
        }
    }

    /// Apply a configuration change to a running engine.
    ///
    /// `enabled` takes effect on the next evaluation — turning the engine off
    /// stops recording immediately and turning it back on resumes, without a
    /// restart — and the new retention limits are enforced at once, evicting
    /// the oldest decisions when they shrink. Returns how many stored decisions
    /// were dropped to satisfy the new limits.
    pub fn reconfigure(&self, enabled: bool, max_decisions: usize, max_age_secs: i64) -> usize {
        self.enabled.store(enabled, Ordering::Release);
        self.store.set_limits(max_decisions, max_age_secs)
    }

    /// Whether shadow evaluation is active.
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
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
    ///
    /// The whole read-train-write is serialised: a concurrent run waits here
    /// and then re-reads the current commit, so it continues from the run that
    /// finished first instead of racing it and losing one of the two updates.
    pub fn try_train(&self, samples: &[DatasetTrainingSample]) -> Result<CommitId, ReplayError> {
        let _training = crate::sync::lock(&self.training);
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
        if !self.enabled() {
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
