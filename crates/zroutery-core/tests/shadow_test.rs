#![cfg(feature = "ml")]

//! Stage 7E-1 — Shadow Mode gate tests.
//!
//! Engine-level verification of the shadow decision infrastructure:
//! 1. Fault isolation — panicking / non-finite predictors, corrupt
//!    checkpoints, store failures (contained, never propagated)
//! 2. Determinism — input/decision checksums, commit-chain replay equivalence
//! 3. Scope + correlation structure of `ShadowDecision`
//! 4. Performance — P95/P99 evaluate overhead budget
//! 5. Coordinator semantics surviving the full shadow path
//! 6. Structural purity — the shadow modules cannot reach any production
//!    mutation surface (source-level tripwires)

use zroutery_core::config::ModelTier;
use zroutery_core::feedback::DataOrigin;
use zroutery_core::ml::coordinator::{CoordinatorConfig, RoutingAction};
use zroutery_core::ml::dataset::{Targets, TrainingSample as DatasetTrainingSample};
use zroutery_core::ml::decision_engine::DecisionEngine;
use zroutery_core::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use zroutery_core::ml::model::ModelState;
use zroutery_core::ml::model_identity::{
    CommitId, ModelCheckpoint, ModelCommit, ModelEnsemble, ModelId, ReplayError,
};
use zroutery_core::ml::reward::{PredictionBundle, RewardPolicy};
use zroutery_core::ml::shadow::{
    EnsemblePredictor, ModelEnsemblePredictor, ShadowCandidateInput, ShadowEngine, ShadowInput,
    ShadowScope, ShadowStore,
};
use zroutery_core::session::SessionRoutingMode;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn decision_engine() -> DecisionEngine {
    DecisionEngine::new(CoordinatorConfig::default(), RewardPolicy::default())
}

fn engine() -> ShadowEngine {
    ShadowEngine::new(decision_engine(), true)
}

/// Feature ramp: `seed + i * 0.01` per dimension (pattern from the shadow
/// module's unit tests).
fn make_features(seed: f32) -> RoutingFeatures {
    let mut values = [0.0f32; FEATURE_DIMENSION];
    for (i, value) in values.iter_mut().enumerate() {
        *value = seed + i as f32 * 0.01;
    }
    RoutingFeatures {
        values,
        schema_version: FEATURE_SCHEMA_VERSION,
    }
}

/// Features supported on only one half of the vector: `first_half` lights up
/// dims 0..16, otherwise dims 16..32. Two candidates built from opposite
/// halves occupy disjoint feature regions, so the shared linear ensemble can
/// learn opposite targets for them without interference.
fn polarized_features(first_half: bool) -> RoutingFeatures {
    let mut values = [0.0f32; FEATURE_DIMENSION];
    let (lo, hi) = if first_half {
        (0, FEATURE_DIMENSION / 2)
    } else {
        (FEATURE_DIMENSION / 2, FEATURE_DIMENSION)
    };
    for value in &mut values[lo..hi] {
        *value = 0.9;
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
        features: make_features(seed),
        rejection_reason: None,
    }
}

fn candidate_input_with_features(
    id: &str,
    provider: &str,
    features: RoutingFeatures,
) -> ShadowCandidateInput {
    ShadowCandidateInput {
        candidate_id: id.to_string(),
        provider_id: provider.to_string(),
        tier: Some(ModelTier::Standard),
        eligible: true,
        features,
        rejection_reason: None,
    }
}

fn shadow_input_with(candidates: Vec<ShadowCandidateInput>) -> ShadowInput {
    ShadowInput {
        decision_id: "dec-1".to_string(),
        policy_id: "policy-1".to_string(),
        client_id: None,
        policy_revision: Default::default(),
        task: Default::default(),
        production_selected: "model-a".to_string(),
        feature_schema: FEATURE_SCHEMA_VERSION,
        candidates,
        session_mode: SessionRoutingMode::Free,
        session_switch_count: 0,
        is_fallback: false,
    }
}

fn default_shadow_input() -> ShadowInput {
    shadow_input_with(vec![
        candidate_input("model-b", "prov-b", 0.20),
        candidate_input("model-a", "prov-a", 0.10),
    ])
}

fn training_samples(range: std::ops::Range<usize>) -> Vec<DatasetTrainingSample> {
    range
        .map(|i| {
            let mut values = [0.0f32; FEATURE_DIMENSION];
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

/// Samples pinned to one feature vector with fixed targets — used to train
/// one candidate's region of feature space toward a known outcome.
fn biased_samples(
    tag: &str,
    features: &RoutingFeatures,
    success: bool,
    latency_ms: f64,
    ttft_ms: f64,
    cost: f64,
    count: usize,
) -> Vec<DatasetTrainingSample> {
    (0..count)
        .map(|i| DatasetTrainingSample {
            sample_id: format!("samp-{}-{}", tag, i),
            schema_version: FEATURE_SCHEMA_VERSION,
            timestamp: 1_000 + i as i64,
            features: features.clone(),
            targets: Targets {
                success,
                latency_ms: Some(latency_ms),
                ttft_ms: Some(ttft_ms),
                cost: Some(cost),
                failure_class: None,
                fallback_count: 0,
            },
            provider_id: "prov-shadow".into(),
            model_id: "model-shadow".into(),
            origin: DataOrigin::Native,
            outcome_id: format!("out-{}-{}", tag, i),
            feedback: vec![],
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Fault isolation
// ---------------------------------------------------------------------------

/// GATE 7E-1 (fault isolation): a panicking predictor is contained — the
/// evaluation returns `None`, the fault is counted once, and the engine stays
/// usable for production traffic.
#[test]
fn predictor_panic_does_not_reach_production() {
    struct PanickingPredictor {
        commit: CommitId,
    }
    impl EnsemblePredictor for PanickingPredictor {
        fn predict(
            &self,
            _model: &str,
            _provider: &str,
            _features: &RoutingFeatures,
        ) -> PredictionBundle {
            panic!("injected predictor fault");
        }

        fn commit_id(&self) -> CommitId {
            self.commit.clone()
        }
    }

    let engine = engine();
    let input = default_shadow_input();
    let faulty = PanickingPredictor {
        commit: CommitId::new("fault-panic"),
    };

    let decision = engine.evaluate_with("req-panic", &input, &faulty);
    assert!(
        decision.is_none(),
        "a panicking predictor must yield None, not an unwind"
    );
    assert_eq!(engine.fault_count(), 1);
    assert!(
        engine.store().is_empty(),
        "no decision may be recorded for a faulted evaluation"
    );

    // The fault does not propagate: the engine keeps evaluating normally.
    let after = engine.evaluate("req-after", &input);
    assert!(after.is_some());
    assert_eq!(
        engine.fault_count(),
        1,
        "the healthy evaluation must not add faults"
    );
    assert_eq!(engine.store().len(), 1);
}

/// GATE 7E-1 (fault isolation): a NaN prediction for one candidate is
/// sanitized (candidate rejected with a reason), not fatal — the decision is
/// still recorded and the fault counter stays at zero.
#[test]
fn nan_prediction_rejected_not_fatal() {
    struct NanForCandidatePredictor {
        poisoned: String,
        inner: ModelEnsemblePredictor,
    }
    impl EnsemblePredictor for NanForCandidatePredictor {
        fn predict(
            &self,
            model: &str,
            provider: &str,
            features: &RoutingFeatures,
        ) -> PredictionBundle {
            let mut bundle = self.inner.predict(model, provider, features);
            if model == self.poisoned {
                bundle.success.value = f64::NAN;
            }
            bundle
        }

        fn commit_id(&self) -> CommitId {
            self.inner.commit_id()
        }
    }

    let engine = engine();
    let input = default_shadow_input();
    let faulty = NanForCandidatePredictor {
        poisoned: "model-b".to_string(),
        inner: ModelEnsemblePredictor::genesis(),
    };

    let decision = engine
        .evaluate_with("req-nan", &input, &faulty)
        .expect("a NaN prediction must not kill the evaluation");
    assert_eq!(engine.fault_count(), 0, "sanitization is not a fault");
    assert_eq!(engine.store().len(), 1);

    let poisoned = &decision.candidates[0];
    assert_eq!(poisoned.candidate_id, "model-b");
    assert!(!poisoned.valid);
    assert_eq!(
        poisoned.rejection_reason.as_deref(),
        Some("non-finite prediction")
    );
    // The stored evidence stays finite: the poisoned slot became a cold zero.
    assert!(poisoned.prediction.success.value.is_finite());
    assert!(poisoned.prediction.success.cold);
    assert_eq!(poisoned.prediction.success.value, 0.0);
    assert_eq!(
        poisoned.utility.total, 0.0,
        "rejected candidates carry the default utility"
    );

    let healthy = &decision.candidates[1];
    assert_eq!(healthy.candidate_id, "model-a");
    assert!(healthy.valid);
    assert!(healthy.rejection_reason.is_none());
    assert!(healthy.utility.total.is_finite());
}

/// GATE 7E-1 (fault isolation): a corrupted checkpoint fails
/// `from_commit` with a `Result` (never a panic), and a failed load leaves
/// the shadow engine running on its own (genesis) predictor.
#[test]
fn invalid_model_state_falls_back() {
    // (a) NaN parameter with a matching checksum — detected by finiteness.
    let mut checkpoint = ModelEnsemblePredictor::genesis().ensemble_checkpoint();
    checkpoint.success.parameters[0] = f64::NAN;
    checkpoint.success.checksum = ModelState::compute_checksum(&checkpoint.success.parameters);
    let err = ModelEnsemblePredictor::from_commit(&checkpoint, CommitId::new("bogus"))
        .err()
        .expect("a NaN parameter must fail construction");
    match &err {
        ReplayError::IncompatibleState { model, reason } => {
            assert_eq!(model.as_str(), "success");
            assert!(reason.contains("not finite"), "unexpected reason: {reason}");
        }
        other => panic!("expected IncompatibleState, got: {other:?}"),
    }

    // (b) Wrong algorithm string — detected by the loader.
    let mut checkpoint = ModelEnsemblePredictor::genesis().ensemble_checkpoint();
    checkpoint.latency.algorithm = "bogus_linear".to_string();
    let err = ModelEnsemblePredictor::from_commit(&checkpoint, CommitId::new("bogus"))
        .err()
        .expect("a wrong algorithm must fail construction");
    match &err {
        ReplayError::IncompatibleState { model, reason } => {
            assert_eq!(model.as_str(), "latency");
            assert!(
                reason.contains("latency_linear"),
                "unexpected reason: {reason}"
            );
        }
        other => panic!("expected IncompatibleState, got: {other:?}"),
    }

    // (c) Tampered parameter WITHOUT fixing the checksum — detected by
    //     integrity verification.
    let mut checkpoint = ModelEnsemblePredictor::genesis().ensemble_checkpoint();
    checkpoint.cost.parameters[0] = 9999.0;
    let err = ModelEnsemblePredictor::from_commit(&checkpoint, CommitId::new("bogus"))
        .err()
        .expect("a stale checksum must fail construction");
    assert!(matches!(
        err,
        ReplayError::IncompatibleState { .. } | ReplayError::ChecksumMismatch { .. }
    ));

    // A valid checkpoint still loads, and the engine is unaffected by the
    // rejected ones: it keeps evaluating on its own predictor.
    let genesis = ModelEnsemblePredictor::genesis();
    assert!(
        ModelEnsemblePredictor::from_commit(&genesis.ensemble_checkpoint(), genesis.commit())
            .is_ok()
    );
    let engine = engine();
    let decision = engine
        .evaluate("req-fallback", &default_shadow_input())
        .expect("the engine must keep evaluating after a rejected checkpoint");
    assert_eq!(decision.shadow.model_commit, genesis.commit());
}

/// GATE 7E-1 (cold start): an all-UNKNOWN feature snapshot still produces a
/// full decision with cold (low-confidence) predictions.
#[test]
fn missing_feature_cold_predictions() {
    let engine = engine();
    let input = shadow_input_with(vec![
        candidate_input_with_features("model-a", "prov-a", RoutingFeatures::default()),
        candidate_input_with_features("model-b", "prov-b", RoutingFeatures::default()),
    ]);

    let decision = engine
        .evaluate("req-cold", &input)
        .expect("cold features must still produce a decision");

    assert_eq!(decision.candidates.len(), 2);
    for candidate in &decision.candidates {
        assert!(
            candidate.valid,
            "UNKNOWN features must not invalidate a candidate"
        );
        for (slot, prediction) in [
            ("success", &candidate.prediction.success),
            ("latency", &candidate.prediction.latency),
            ("ttft", &candidate.prediction.ttft),
            ("cost", &candidate.prediction.cost),
        ] {
            assert!(
                prediction.confidence <= 0.1,
                "{} confidence {} must be cold (<= 0.1)",
                slot,
                prediction.confidence
            );
            assert_eq!(
                prediction.sample_count, 0,
                "{slot} must be untrained at genesis"
            );
            assert!(prediction.value.is_finite(), "{slot} value must be finite");
        }
    }

    // Genesis models are bias-only: both candidates score identically, so
    // production's choice is kept.
    assert_eq!(decision.shadow.action, RoutingAction::Keep);
    assert_eq!(decision.shadow.selected, "model-a");
    assert!(!decision.shadow.reason.is_empty());
    assert_eq!(engine.fault_count(), 0);
}

/// GATE 7E-1 (fault isolation): the bounded store never breaks evaluation —
/// capacity evicts the oldest decision instead of erroring, and a store-gate
/// rejection is contained as one fault rather than propagating.
#[test]
fn store_failure_does_not_propagate() {
    // (a) Overflow at capacity evicts the oldest entry; push stays Ok.
    let source = engine();
    let input = default_shadow_input();
    let first = source.evaluate("req-store-1", &input).unwrap();
    let second = source.evaluate("req-store-2", &input).unwrap();
    let third = source.evaluate("req-store-3", &input).unwrap();
    let first_id = first.shadow_id.clone();
    let third_id = third.shadow_id.clone();

    let store = ShadowStore::new(2, 24 * 3600);
    assert_eq!(store.len(), 0);
    store.push(first).expect("push below capacity");
    store.push(second).expect("push at capacity");
    assert_eq!(store.len(), 2);
    store
        .push(third)
        .expect("overflow evicts instead of erroring");
    assert_eq!(store.len(), 2);
    let kept = store.decisions();
    assert_eq!(kept.len(), 2);
    assert!(
        kept.iter().all(|d| d.shadow_id != first_id),
        "the oldest decision must be evicted"
    );
    assert!(
        kept.iter().any(|d| d.shadow_id == third_id),
        "the newest decision must be kept"
    );

    // (b) A store-gate rejection through the engine is contained as a fault.
    let strict = engine();
    assert!(
        strict.evaluate("", &input).is_none(),
        "an empty request id fails the store gate and must yield None"
    );
    assert_eq!(strict.fault_count(), 1);
    assert!(strict.store().is_empty());
    let recovered = strict.evaluate("req-store-recovered", &input);
    assert!(
        recovered.is_some(),
        "the engine must stay usable after a store rejection"
    );
    assert_eq!(strict.fault_count(), 1);
}

// ---------------------------------------------------------------------------
// Configuration-driven retention (ML-01)
// ---------------------------------------------------------------------------

/// The configured retention is what the store enforces: the limits supplied at
/// construction bound the store, and a smaller limit applied to a live engine
/// evicts the oldest decisions immediately.
#[test]
fn configured_retention_bounds_the_store_and_hot_shrinks_it() {
    let engine = ShadowEngine::with_retention(decision_engine(), true, 2, 24 * 3600);
    let input = default_shadow_input();
    let first = engine.evaluate("req-cap-1", &input).unwrap();
    let second = engine.evaluate("req-cap-2", &input).unwrap();
    assert_eq!(
        engine.store().len(),
        2,
        "the configured cap must be the store's cap from construction"
    );

    // A smaller limit reaches the already-running engine and evicts down to it.
    let dropped = engine.reconfigure(true, 1, 24 * 3600);
    assert_eq!(
        dropped, 1,
        "one decision must be evicted to reach the new cap"
    );
    assert_eq!(engine.store().len(), 1);
    let kept = engine.store().decisions();
    assert_eq!(kept.len(), 1);
    assert_ne!(
        kept[0].shadow_id, first.shadow_id,
        "the oldest decision must be the one evicted"
    );
    assert_eq!(
        kept[0].shadow_id, second.shadow_id,
        "the newest decision must survive the shrink"
    );
}

/// The configured age window is what the store serves: shrinking it drops the
/// decisions that are already outside the new window, and a later evaluation
/// is filtered by it.
#[test]
fn configured_age_window_drops_decisions_outside_it() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let source = engine();
    let input = default_shadow_input();
    let mut stale = source.evaluate("req-age-stale", &input).unwrap();
    stale.timestamp = now - 7200;
    let fresh = source.evaluate("req-age-fresh", &input).unwrap();

    let store = ShadowStore::new(10, 30 * 24 * 3600);
    store.push(stale).unwrap();
    store.push(fresh).unwrap();
    assert_eq!(
        store.decisions().len(),
        2,
        "the 30-day window keeps both decisions"
    );

    // One hour: the two-hour-old decision falls outside and is dropped.
    let dropped = store.set_limits(10, 3600);
    assert_eq!(dropped, 1, "the aged decision must be dropped");
    assert_eq!(store.len(), 1, "the aged decision is physically evicted");
    let kept = store.decisions();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].actual.request_id, "req-age-fresh");
}

/// The master switch is configuration, not a construction constant: turning the
/// engine off on a running instance stops recording without a fault, and
/// turning it back on resumes.
#[test]
fn reconfigure_switches_evaluation_off_and_on() {
    let engine = engine();
    let input = default_shadow_input();
    assert!(engine.evaluate("req-switch-1", &input).is_some());

    engine.reconfigure(false, 10_000, 24 * 3600);
    assert!(
        !engine.enabled(),
        "the running engine adopts the new switch"
    );
    assert!(
        engine.evaluate("req-switch-off", &input).is_none(),
        "a disabled engine must record nothing"
    );
    assert_eq!(engine.store().len(), 1, "nothing was recorded while off");

    engine.reconfigure(true, 10_000, 24 * 3600);
    assert!(engine.enabled());
    assert!(engine.evaluate("req-switch-2", &input).is_some());
    assert_eq!(
        engine.store().len(),
        2,
        "recording resumed after re-enabling"
    );
    assert_eq!(engine.fault_count(), 0, "a switch is not a fault");
}

// ---------------------------------------------------------------------------
// Concurrent training (ML-14)
// ---------------------------------------------------------------------------

/// Two barrier-synchronised training runs must not lose an update. The engine
/// serialises the read-train-write, so the second run starts from the first
/// run's commit and both batches of learned events survive to the final commit.
#[test]
fn concurrent_training_keeps_every_update() {
    use std::sync::{Arc, Barrier};

    let engine = Arc::new(engine());
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for range in [0..1000, 1000..2000] {
        let engine = Arc::clone(&engine);
        let barrier = Arc::clone(&barrier);
        let samples = training_samples(range);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            engine.try_train(&samples)
        }));
    }
    for handle in handles {
        let _ = handle
            .join()
            .expect("a training thread must not panic")
            .expect("a concurrent training run must not lose its samples");
    }

    let commit = engine.predictor_commit();
    assert_eq!(
        commit.learning_event_count, 2000,
        "both 1000-sample batches must be in the final commit"
    );
    assert_eq!(
        commit.checkpoint.success.update_count, 2000,
        "both batches must be applied to the final ensemble"
    );
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

/// GATE 7E-1 (determinism): the same input snapshot always yields the same
/// checksums — volatile fields (shadow id, timestamp) are excluded.
#[test]
fn same_input_same_decision_checksum() {
    let engine = engine();
    let input = default_shadow_input();
    let first = engine.evaluate("req-determinism", &input).unwrap();
    let second = engine.evaluate("req-determinism", &input).unwrap();

    assert_ne!(
        first.shadow_id, second.shadow_id,
        "each evaluation gets a fresh shadow id"
    );
    assert_eq!(first.shadow.model_commit, second.shadow.model_commit);
    assert_eq!(
        first.decision_input_checksum, second.decision_input_checksum,
        "the input checksum must ignore volatile fields"
    );
    assert_eq!(
        first.decision_checksum, second.decision_checksum,
        "the decision checksum must ignore volatile fields"
    );
    assert_eq!(engine.fault_count(), 0);
}

/// GATE 7E-1 (determinism): replay equivalence — training A then B on a
/// fresh engine lands on the same model commit (and therefore the same
/// decision checksum) as training A+B in one batch.
#[test]
fn same_commit_same_checksum_via_replay() {
    let samples = training_samples(0..10);
    let (early, late) = samples.split_at(5);

    let batched = engine();
    let batched_commit = batched.train(&samples);

    let chained = engine();
    chained.train(early);
    let chained_commit = chained.train(late);

    assert_eq!(
        batched_commit, chained_commit,
        "replay(A then B) must equal batch(A+B)"
    );

    let input = default_shadow_input();
    let batched_decision = batched.evaluate("req-replay", &input).unwrap();
    let chained_decision = chained.evaluate("req-replay", &input).unwrap();

    assert_eq!(
        batched_decision.shadow.model_commit,
        chained_decision.shadow.model_commit
    );
    assert_eq!(
        batched_decision.decision_input_checksum,
        chained_decision.decision_input_checksum
    );
    assert_eq!(
        batched_decision.decision_checksum, chained_decision.decision_checksum,
        "same commit + same input must yield the same decision checksum"
    );
    assert_eq!(
        batched_decision.shadow.selected,
        chained_decision.shadow.selected
    );
    assert_eq!(
        batched_decision.shadow.action,
        chained_decision.shadow.action
    );
    assert_eq!(
        batched_decision.shadow.reason,
        chained_decision.shadow.reason
    );
}

/// GATE 7E-1 (determinism): training materially changes the commit and the
/// decision for an input whose feature regions were trained.
#[test]
fn same_ordered_training_events_same_commit_and_decision_checksum() {
    let samples = training_samples(0..12);
    let first = engine();
    let second = engine();
    let first_commit = first.train(&samples);
    let second_commit = second.train(&samples);
    assert_eq!(first_commit, second_commit);

    let input = default_shadow_input();
    let first_decision = first.evaluate("req-same-events", &input).unwrap();
    let second_decision = second.evaluate("req-same-events", &input).unwrap();
    assert_eq!(
        first_decision.decision_input_checksum,
        second_decision.decision_input_checksum
    );
    assert_eq!(
        first_decision.decision_checksum,
        second_decision.decision_checksum
    );
}

#[test]
fn distinct_commits_distinct_decisions() {
    let engine = engine();
    let features_b = polarized_features(true);
    let features_a = polarized_features(false);
    let input = shadow_input_with(vec![
        candidate_input_with_features("model-b", "prov-b", features_b.clone()),
        candidate_input_with_features("model-a", "prov-a", features_a.clone()),
    ]);

    // Before training: genesis models are bias-only, both candidates score
    // identically, production's choice is kept.
    let before = engine.evaluate("req-distinct", &input).unwrap();
    assert_eq!(before.shadow.action, RoutingAction::Keep);
    assert_eq!(before.shadow.selected, "model-a");

    // Train opposite outcomes in the two candidates' disjoint regions.
    let mut samples = biased_samples("win", &features_b, true, 200.0, 100.0, 0.01, 1000);
    samples.extend(biased_samples(
        "lose",
        &features_a,
        false,
        4000.0,
        1500.0,
        0.9,
        1000,
    ));
    let commit = engine.train(&samples);

    let after = engine.evaluate("req-distinct", &input).unwrap();
    assert_ne!(after.shadow.model_commit, before.shadow.model_commit);
    assert_eq!(after.shadow.model_commit, commit);
    assert_ne!(
        after.decision_input_checksum, before.decision_input_checksum,
        "the model commit feeds the input checksum"
    );
    assert_ne!(
        after.decision_checksum, before.decision_checksum,
        "materially different predictions must change the decision checksum"
    );

    // The utility delta now clears the switch threshold: model-b wins.
    assert_eq!(after.shadow.action, RoutingAction::Switch);
    assert_eq!(after.shadow.selected, "model-b");
    assert!(after.shadow.reason.contains("utility delta"));
}

// ---------------------------------------------------------------------------
// Scope + correlation structure
// ---------------------------------------------------------------------------

/// GATE 7E-1 (scope + correlation): the recorded decision carries the full
/// correlation triple, scope, per-candidate evidence, and identity fields.
#[test]
fn shadow_decision_shape() {
    let engine = engine();
    let candidates = vec![
        ShadowCandidateInput {
            candidate_id: "model-a".to_string(),
            provider_id: "prov-a".to_string(),
            tier: Some(ModelTier::Fast),
            eligible: true,
            features: make_features(0.10),
            rejection_reason: None,
        },
        ShadowCandidateInput {
            candidate_id: "model-b".to_string(),
            provider_id: "prov-b".to_string(),
            tier: Some(ModelTier::Standard),
            eligible: true,
            features: make_features(0.30),
            rejection_reason: None,
        },
        ShadowCandidateInput {
            candidate_id: "model-c".to_string(),
            provider_id: "prov-c".to_string(),
            tier: Some(ModelTier::Reasoning),
            eligible: true,
            features: make_features(0.50),
            rejection_reason: None,
        },
        ShadowCandidateInput {
            candidate_id: "model-d".to_string(),
            provider_id: "prov-d".to_string(),
            tier: None,
            eligible: false,
            features: make_features(0.70),
            rejection_reason: Some("policy rejected".to_string()),
        },
    ];
    let input = shadow_input_with(candidates);
    let before = chrono::Utc::now().timestamp();
    let decision = engine
        .evaluate("req-shape", &input)
        .expect("happy-path evaluation");
    let after = chrono::Utc::now().timestamp();

    // Identity + scope.
    let suffix = decision
        .shadow_id
        .strip_prefix("shadow-")
        .expect("shadow id carries the 'shadow-' prefix");
    assert_eq!(suffix.len(), 32, "shadow id suffix is a simple uuid");
    assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    assert!((before..=after).contains(&decision.timestamp));
    assert_eq!(decision.scope, ShadowScope::PolicyRouted);

    // Correlation triple: what production actually did.
    assert_eq!(decision.actual.request_id, "req-shape");
    assert_eq!(decision.actual.decision_id, input.decision_id);
    assert_eq!(decision.actual.selected, input.production_selected);

    // Verdict: what the ML stack would have done.
    assert!(!decision.shadow.model_commit.as_str().is_empty());
    assert_eq!(decision.shadow.feature_schema, FEATURE_SCHEMA_VERSION);
    assert!(!decision.shadow.selected.is_empty());
    assert!(matches!(
        decision.shadow.action,
        RoutingAction::Keep | RoutingAction::Switch | RoutingAction::Explore
    ));
    assert!(!decision.shadow.reason.is_empty());

    // Evidence: one entry per candidate, in input order, with prediction +
    // utility per candidate.
    assert_eq!(decision.candidates.len(), input.candidates.len());
    for (candidate, source) in decision.candidates.iter().zip(input.candidates.iter()) {
        assert_eq!(candidate.candidate_id, source.candidate_id);
        assert_eq!(candidate.provider_id, source.provider_id);
        assert_eq!(candidate.tier, source.tier);
        assert_eq!(candidate.eligible, source.eligible);
        assert_eq!(candidate.prediction.candidate_model, source.candidate_id);
        assert_eq!(candidate.prediction.candidate_provider, source.provider_id);
        for component in [
            candidate.utility.success,
            candidate.utility.latency,
            candidate.utility.ttft,
            candidate.utility.cost,
            candidate.utility.fallback,
            candidate.utility.uncertainty,
            candidate.utility.switch_cost,
            candidate.utility.total,
        ] {
            assert!(component.is_finite());
        }
    }
    // The ineligible candidate is recorded as evidence with a reason.
    assert!(decision.candidates.iter().take(3).all(|c| c.valid));
    assert!(!decision.candidates[3].valid);
    assert_eq!(
        decision.candidates[3].rejection_reason.as_deref(),
        Some("policy rejected")
    );

    // Identity checksums + store round-trip.
    assert_ne!(decision.decision_input_checksum, 0);
    assert_ne!(decision.decision_checksum, 0);
    assert_eq!(engine.store().len(), 1);
    let stored = engine.store().decisions();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].shadow_id, decision.shadow_id);
    assert_eq!(stored[0].decision_checksum, decision.decision_checksum);
    assert_eq!(engine.fault_count(), 0);
}

// ---------------------------------------------------------------------------
// Performance
// ---------------------------------------------------------------------------

/// GATE 7E-1 (performance): per-evaluate overhead over a realistic
/// The 8-candidate shadow path stays fast enough to be worth running inline.
///
/// The marginal cost of a candidate must not itself grow with the candidate count.
///
/// # Why this replaced a wall-clock gate
///
/// This was a fixed ceiling: p95 <= 10ms, p99 <= 30ms, against a path that measures
/// tens of microseconds. It failed under concurrent build load roughly one run in
/// eight, and the failure was pure scheduling noise — the observed p95 was a
/// preemption, not a regression.
///
/// Widening the number does not fix that and neither does tightening it. A fixed
/// ceiling cannot distinguish *a slow machine* from *a slow regression*, because
/// both produce the same number. Any ceiling loose enough not to fire on a loaded
/// machine is too loose to catch a 10x regression; any ceiling tight enough to catch
/// one fires on a busy one. That is not a tuning problem, which is why the previous
/// version's own comment said loosening would not fix it.
///
/// # Why not a ratio between two candidate counts
///
/// Because two points cannot see the difference. Measured on this path, cost is
/// linear at about 20us per candidate plus a ~15us fixed cost, so a *quadratic*
/// term small enough to be invisible between n=1 and n=8 fits that data exactly.
/// An endpoint ratio passes a genuinely quadratic regression.
///
/// Measured, so this is not a claim about a hypothetical. Putting a pairwise
/// comparison where the sort belongs — score every candidate against every other
/// candidate — fits `T(n) = 80us + 3.5us*n^2` and produces:
///
/// ```text
///   4c p50=  137us    8c p50=  306us    32c p50= 2818us    64c p50=10016us
/// ```
///
/// As an endpoint ratio that is `306/84 = 3.6x`, which sails past any bound loose
/// enough not to fire on a loaded machine. Under the marginal comparison below the
/// same mutation reports 42.4us against 224.9us and fails. The old gate would not
/// have caught a 5x regression at 64 candidates, and neither would a wider one.
///
/// # How the comparison stays honest under load
///
/// Differentiating the curve divides out the fixed per-request cost, and comparing
/// the marginal cost at a low count against the marginal cost at a high count
/// compares the shape directly: flat means linear, rising means the per-candidate
/// work is itself growing.
///
/// The four measurements are **interleaved in one loop**, so all of them see the same
/// scheduler. Run as separate phases, a load spike lands on one of them and
/// manufactures a curvature that is not in the code. The comparison is on
/// **medians**, because a median is unmoved by an occasional preemption where a tail
/// percentile is defined by exactly those.
///
/// # What this does not catch
///
/// A *constant-factor* slowdown: making per-candidate work twice as expensive
/// doubles the whole curve and leaves its shape alone. Measured, by mutation — a
/// duplicated prediction per candidate moved the 1-to-8 ratio from 4.69 to 4.83,
/// which is nothing. Catching that needs an absolute ceiling, which is the flaky
/// thing this replaced. `shadow_evaluate_absolute_cost` measures the magnitude for a
/// human; this gates the shape.
#[test]
fn shadow_marginal_cost_per_candidate_does_not_grow() {
    let engine = engine();
    engine.train(&training_samples(0..100));

    let at = |count: usize| {
        let candidates: Vec<ShadowCandidateInput> = (0..count)
            .map(|i| {
                candidate_input(
                    &format!("model-{}", i),
                    &format!("prov-{}", i % 3),
                    0.10 + i as f32 * 0.08,
                )
            })
            .collect();
        let mut input = shadow_input_with(candidates);
        input.production_selected = "model-0".to_string();
        input
    };
    // Wide enough that a quadratic term is unmistakable, narrow enough to stay fast:
    // measured cost at 64 candidates is ~1.3ms, so 300 interleaved rounds of all four
    // arms is well under a second.
    const COUNTS: [usize; 4] = [4, 8, 32, 64];
    const SAMPLES: usize = 300;
    let inputs: Vec<ShadowInput> = COUNTS.iter().map(|n| at(*n)).collect();

    // Warm-up: allocator, caches, store growth.
    for _ in 0..50 {
        for input in &inputs {
            assert!(engine.evaluate("req-warm", input).is_some());
        }
    }

    let mut samples: Vec<Vec<u64>> = vec![Vec::with_capacity(SAMPLES); COUNTS.len()];
    for _ in 0..SAMPLES {
        // Interleaved: every arm sees the same scheduler as every other.
        for (arm, input) in inputs.iter().enumerate() {
            let start = std::time::Instant::now();
            let decision = engine.evaluate("req-perf", input);
            samples[arm].push(start.elapsed().as_nanos() as u64);
            assert!(decision.is_some());
        }
    }
    for arm in &mut samples {
        arm.sort_unstable();
    }
    let p50 = |arm: usize| samples[arm][SAMPLES / 2] as f64 / 1_000.0;

    println!(
        "shadow evaluate cost ({} interleaved rounds):\n  \
         4c p50={:.1}us   8c p50={:.1}us   32c p50={:.1}us   64c p50={:.1}us\n  \
         marginal per candidate: {:.1}us low, {:.1}us high",
        SAMPLES,
        p50(0),
        p50(1),
        p50(2),
        p50(3),
        (p50(1) - p50(0)) / (COUNTS[1] - COUNTS[0]) as f64,
        (p50(3) - p50(2)) / (COUNTS[3] - COUNTS[2]) as f64,
    );

    let low = (p50(1) - p50(0)) / (COUNTS[1] - COUNTS[0]) as f64;
    let high = (p50(3) - p50(2)) / (COUNTS[3] - COUNTS[2]) as f64;
    assert!(
        high <= low * 3.0,
        "the marginal cost of a candidate grows from {low:.1}us at 4-8 candidates \
         to {high:.1}us at 32-64. Flat means the per-candidate work is constant; \
         rising means it is growing with the count, which is how a microsecond \
         request path becomes a millisecond one. Unlike a fixed ceiling this is \
         independent of machine speed and load, because every arm is measured \
         interleaved under the same scheduler."
    );
}

/// Absolute cost of the shadow path, measured deliberately rather than gated.
///
/// The gate above guards the *shape* of this cost, not its magnitude, because a
/// wall-clock ceiling cannot tell a slow machine from a slow regression. This is
/// where the magnitude is read: run it when you want to know what the path costs,
/// or after changing anything on it.
///
///     cargo test -p zroutery-core --features ml --test shadow_test \
///         -- --ignored --nocapture shadow_evaluate_absolute_cost
#[test]
#[ignore = "measures; run it deliberately"]
fn shadow_evaluate_absolute_cost() {
    let engine = engine();
    engine.train(&training_samples(0..100));

    println!("candidates   p50      p95      p99");
    for count in [1usize, 4, 16, 64] {
        let candidates: Vec<ShadowCandidateInput> = (0..count)
            .map(|i| {
                candidate_input(
                    &format!("model-{}", i),
                    &format!("prov-{}", i % 3),
                    0.10 + i as f32 * 0.08,
                )
            })
            .collect();
        let mut input = shadow_input_with(candidates);
        input.production_selected = "model-0".to_string();

        for _ in 0..50 {
            engine.evaluate("warm", &input);
        }
        let mut durations: Vec<u64> = Vec::with_capacity(500);
        for _ in 0..500 {
            let start = std::time::Instant::now();
            let decision = engine.evaluate("perf", &input);
            durations.push(start.elapsed().as_nanos() as u64);
            assert!(decision.is_some());
        }
        durations.sort_unstable();
        println!(
            "{count:>10}   {:>6}us {:>7}us {:>7}us",
            durations[250] / 1_000,
            durations[475] / 1_000,
            durations[495] / 1_000
        );
    }
}

// ---------------------------------------------------------------------------
// Coordinator semantics through the full shadow path
// ---------------------------------------------------------------------------

/// GATE 7E-1 (coordinator semantics): the frozen session guard survives the
/// full shadow path — sticky session + low confidence forces Keep with the
/// session reason even when the trained utility favors a switch.
///
/// "Low confidence" has to mean an uncertain prediction, not a certain one.
/// model-b is the utility favorite on latency and cost, but it won and lost the
/// same requests, so its success head stays undecided (within 0.2 of a coin
/// flip) and the guard's confidence input is low. Training it to near-certain
/// success and still asserting a low confidence is what the previous
/// `4p(1-p)` confidence reported, and it read a confident prediction as an
/// uncertain one.
#[test]
fn shadow_respects_coordinator_semantics() {
    let engine = engine();
    let features_b = polarized_features(true);
    let features_a = polarized_features(false);
    // model-b: the same latency and cost won and lost, interleaved so the
    // online success head sees a balanced signal rather than a block of wins
    // followed by a block of losses. Success there is a coin flip the utility
    // still prefers over model-a's failing record.
    let wins = biased_samples("win-b", &features_b, true, 200.0, 100.0, 0.01, 1_000);
    let losses = biased_samples("lose-b", &features_b, false, 200.0, 100.0, 0.01, 1_000);
    let mut samples = Vec::with_capacity(3_000);
    for (win, loss) in wins.into_iter().zip(losses) {
        samples.push(win);
        samples.push(loss);
    }
    samples.extend(biased_samples(
        "lose-a",
        &features_a,
        false,
        4000.0,
        1500.0,
        0.9,
        1000,
    ));
    engine.train(&samples);

    let candidates = vec![
        candidate_input_with_features("model-b", "prov-b", features_b),
        candidate_input_with_features("model-a", "prov-a", features_a),
    ];

    // Control (Free session): the utility path switches to model-b, proving
    // the training really favors it even though its success head is undecided.
    let free_input = shadow_input_with(candidates.clone());
    let free = engine
        .evaluate("req-free", &free_input)
        .expect("free evaluation");
    assert_eq!(free.shadow.action, RoutingAction::Switch);
    assert_eq!(free.shadow.selected, "model-b");

    // Sticky session + low confidence: the guard forces Keep before any
    // utility comparison, exactly as the frozen Coordinator decides.
    let mut sticky_input = shadow_input_with(candidates);
    sticky_input.session_mode = SessionRoutingMode::Sticky;
    let sticky = engine
        .evaluate("req-sticky", &sticky_input)
        .expect("sticky evaluation");
    assert!(
        (sticky.candidates[0].prediction.success.value - 0.5).abs() < 0.2,
        "the guard's confidence input must come from an undecided success head, got {}",
        sticky.candidates[0].prediction.success.value
    );
    assert!(
        sticky.candidates[0].prediction.success.confidence < 0.8,
        "the guard's confidence input (first valid candidate) must be low, got {}",
        sticky.candidates[0].prediction.success.confidence
    );
    assert_eq!(sticky.candidates[0].candidate_id, "model-b");
    assert_eq!(sticky.shadow.action, RoutingAction::Keep);
    assert_eq!(sticky.shadow.selected, "model-a");
    assert_eq!(sticky.shadow.reason, "session constraint: pinned/sticky");
    assert_eq!(engine.fault_count(), 0);
}

// ---------------------------------------------------------------------------
// Structural purity tripwires
// ---------------------------------------------------------------------------

/// Extract every `// shadow-block-begin` .. `// shadow-block-end` region from
/// pipeline.rs. The marked blocks are the complete production surface of the
/// shadow path: the snapshot construction in `handle_chat` plus the two
/// record-only evaluate hooks (buffered and streaming).
fn shadow_blocks(pipeline_src: &str) -> Vec<&str> {
    const BEGIN: &str = "// shadow-block-begin";
    const END: &str = "// shadow-block-end";
    let mut blocks = Vec::new();
    let mut rest = pipeline_src;
    while let Some(start) = rest.find(BEGIN) {
        let after_begin = &rest[start + BEGIN.len()..];
        let Some(end) = after_begin.find(END) else {
            panic!("pipeline.rs has a // shadow-block-begin without its end");
        };
        blocks.push(&after_begin[..end]);
        rest = &after_begin[end + END.len()..];
    }
    assert!(
        !blocks.is_empty(),
        "pipeline.rs must mark its shadow blocks with // shadow-block-begin/end"
    );
    blocks
}

/// GATE 7E-1 (structural purity): the shadow modules must not reference any
/// production mutation surface. This is a source-level tripwire — it fails
/// the moment someone adds a call from the shadow path into ranking,
/// egress, health, session, account, migration or spend state, all of which
/// the shadow design forbids. Compile-time reachability is already narrowed
/// by [`ShadowInput::from_policy_plan`]'s signature (read-only stores + the
/// already-computed plan/decision, no AppState, no Router); this test keeps
/// the remaining textual surface honest.
#[test]
fn shadow_modules_reference_no_mutation_surface() {
    let shadow_src = include_str!("../src/ml/shadow.rs");
    let engine_src = include_str!("../src/ml/decision_engine.rs");

    for (name, src) in [
        ("shadow.rs", shadow_src),
        ("decision_engine.rs", engine_src),
    ] {
        for forbidden in [
            // Ranking entry points.
            "plan_with_policy",
            "plan_classifier",
            // Budget gate.
            "allow_request",
            // Egress.
            "upstream",
            // Session / account / migration state.
            "SessionStore",
            "AccountStore",
            "agent_takeover",
            "MigrationExecutor",
            // Health mutation.
            "record_success",
            "record_failure",
            "clear_affinity",
            // Spend.
            "Ledger",
            "charge",
        ] {
            assert!(
                !src.contains(forbidden),
                "{name} must not reference {forbidden}"
            );
        }
    }

    // The production side: the marked shadow blocks in pipeline.rs are the
    // entire server-side surface of the shadow path, and they must not touch
    // any mutation surface either. (`state.upstream` / `Upstream::` are the
    // precise egress tokens — pipeline.rs legitimately mentions upstream
    // transports elsewhere, just never from the shadow blocks.)
    let pipeline_src = include_str!("../src/server/pipeline.rs");
    for block in shadow_blocks(pipeline_src) {
        for forbidden in [
            "plan_with_policy",
            "plan_classifier",
            "allow_request",
            "state.upstream",
            "Upstream::",
            "SessionStore",
            "AccountStore",
            "agent_takeover",
            "MigrationExecutor",
            "record_success",
            "record_failure",
            "clear_affinity",
            "Ledger",
            "charge",
        ] {
            assert!(
                !block.contains(forbidden),
                "pipeline shadow block must not reference {forbidden}"
            );
        }
    }
}

/// GATE 7E-1 (session/account purity): the stores behind session, account,
/// takeover and migration state are not wired into the production shadow
/// path at all. Structural: every marked shadow block in pipeline.rs —
/// snapshot construction plus both evaluate hooks — references none of
/// them, and the shadow modules cannot reach them (see
/// [`shadow_modules_reference_no_mutation_surface`]).
#[test]
fn production_shadow_path_touches_no_session_or_account_state() {
    let pipeline_src = include_str!("../src/server/pipeline.rs");
    for block in shadow_blocks(pipeline_src) {
        for forbidden in ["session", "account", "takeover", "migration"] {
            assert!(
                !block.contains(forbidden),
                "pipeline shadow block must not reference {forbidden}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Gate 7E-1A — predictor identity and checkpoint lineage
// ---------------------------------------------------------------------------

#[test]
fn predictor_rejects_wrong_checkpoint_commit_pairing() {
    let first = ModelEnsemblePredictor::genesis();
    let mut other_ensemble = ModelEnsemble::new();
    other_ensemble.update_all(&training_samples(0..1)[0]);
    let other_checkpoint = other_ensemble.save_all();
    let other_commit = ModelCommit::new(ModelId::new("shadow"), other_checkpoint.clone(), None, 0);

    let error = ModelEnsemblePredictor::from_commit(&other_checkpoint, first.commit())
        .err()
        .expect("a valid checkpoint paired with a foreign commit must fail");
    assert!(matches!(error, ReplayError::CommitMismatch { .. }));

    // The full-record constructor is the supported path for child commits.
    assert!(ModelEnsemblePredictor::from_model_commit(&other_commit).is_ok());
}

/// ML-13: the compatibility loader must not present an already trained root
/// checkpoint as an empty genesis history. The loaded samples survive the next
/// training run and the loaded commit stays its parent; only the canonical cold
/// genesis keeps the empty-history path.
#[test]
fn from_commit_continues_from_a_trained_root_checkpoint() {
    // A root commit record over an already trained checkpoint: the record
    // cannot describe the training, but it must not erase it either.
    let mut trained = ModelEnsemble::new();
    for sample in &training_samples(0..1) {
        trained.update_all(sample);
    }
    let checkpoint = trained.save_all();
    let loaded_commit = ModelCommit::new(ModelId::new("shadow"), checkpoint.clone(), None, 0);
    let predictor =
        ModelEnsemblePredictor::from_commit(&checkpoint, loaded_commit.commit_id.clone())
            .expect("a trained root checkpoint must load");

    let (ensemble, next) = predictor
        .try_train(&training_samples(1..2))
        .expect("training from a loaded checkpoint must succeed");
    assert_eq!(
        next.parent,
        Some(loaded_commit.commit_id.clone()),
        "the loaded commit must be the parent of the next training commit"
    );
    assert_eq!(
        ensemble.save_all().success.update_count,
        2,
        "the loaded sample and the new sample must both be applied"
    );

    // The cold genesis is unchanged: it is the one checkpoint that really is an
    // empty, complete history, so it keeps rebuilding from itself.
    let genesis = ModelEnsemblePredictor::genesis();
    let cold =
        ModelEnsemblePredictor::from_commit(&genesis.ensemble_checkpoint(), genesis.commit())
            .expect("the cold genesis must load through the compatibility path");
    let (_, cold_commit) = cold.train(&training_samples(0..1));
    let (_, inline_commit) = genesis.train(&training_samples(0..1));
    assert_eq!(
        cold_commit, inline_commit,
        "the cold genesis must keep its empty-history training path"
    );
}

#[test]
fn predictor_train_and_swap_retain_verified_lineage() {
    let engine = engine();
    let root = engine.predictor_commit();
    assert_eq!(root.parent, None);
    assert!(root.verify());
    assert_eq!(
        engine.predictor_checkpoint().content_hash(),
        root.checkpoint.content_hash()
    );

    let first_id = engine.train(&training_samples(0..2));
    let first = engine.predictor_commit();
    assert_eq!(first.commit_id, first_id);
    let parent = engine
        .predictor_lineage()
        .into_iter()
        .find(|entry| Some(entry.commit_id.clone()) == first.parent)
        .expect("the retained lineage must include the parent commit");
    assert!(parent.verify());
    assert_eq!(first.learning_event_count, 2);
    assert_eq!(
        first.checkpoint.content_hash(),
        engine.predictor_checkpoint().content_hash()
    );

    let mut lineage = engine.predictor_lineage();
    let predictor =
        ModelEnsemblePredictor::from_model_commit_with_lineage(&first, &lineage).unwrap();
    let (ensemble, second) = predictor.try_train(&training_samples(2..4)).unwrap();
    lineage.push(second.clone());
    let replacement =
        ModelEnsemblePredictor::from_model_commit_with_lineage(&second, &lineage).unwrap();
    assert_eq!(replacement.parent(), Some(first.commit_id.clone()));
    assert_eq!(
        replacement.checkpoint().content_hash(),
        ensemble.save_all().content_hash()
    );
    assert_eq!(engine.swap(replacement).unwrap(), second.commit_id);
    assert_eq!(engine.predictor_commit().parent, Some(first.commit_id));
    assert!(engine.predictor_commit().verify());
}

#[test]
fn predictor_rejects_corrupt_full_commit_record() {
    let predictor = ModelEnsemblePredictor::genesis();
    let mut commit = predictor.commit_record();
    commit.checkpoint.success.parameters[0] = 999.0;
    let error = ModelEnsemblePredictor::from_model_commit(&commit)
        .err()
        .expect("a tampered full commit must fail closed");
    assert!(matches!(error, ReplayError::InvalidCommit { .. }));

    let mut checkpoint: ModelCheckpoint = predictor.checkpoint();
    checkpoint.feature_schema_version = FEATURE_SCHEMA_VERSION + 1;
    assert!(ModelEnsemblePredictor::from_commit(&checkpoint, predictor.commit()).is_err());
}

#[test]
fn predictor_requires_complete_lineage_for_children() {
    let root = ModelCommit::new(
        ModelId::new("shadow"),
        ModelEnsemble::new().save_all(),
        None,
        0,
    );
    let mut child_ensemble = ModelEnsemble::new();
    child_ensemble.update_all(&training_samples(0..1)[0]);
    let child = ModelCommit::new(
        ModelId::new("shadow"),
        child_ensemble.save_all(),
        Some(root.commit_id.clone()),
        1,
    );

    let error = ModelEnsemblePredictor::from_model_commit(&child)
        .err()
        .expect("a child without its parent chain must fail closed");
    assert!(matches!(error, ReplayError::LineageCorrupt { .. }));

    let mut wrong_model_ensemble = ModelEnsemble::new();
    wrong_model_ensemble.update_all(&training_samples(1..2)[0]);
    let wrong_model = ModelCommit::new(
        ModelId::new("other-shadow"),
        wrong_model_ensemble.save_all(),
        Some(root.commit_id.clone()),
        1,
    );
    let error = ModelEnsemblePredictor::from_model_commit_with_lineage(
        &wrong_model,
        &[root.clone(), wrong_model.clone()],
    )
    .err()
    .expect("a mixed-model lineage must fail closed");
    assert!(matches!(error, ReplayError::LineageCorrupt { .. }));

    let error = ModelEnsemblePredictor::from_model_commit_with_lineage(
        &child,
        &[child.clone(), child.clone()],
    )
    .err()
    .expect("a cyclic/duplicate lineage must fail closed");
    assert!(matches!(error, ReplayError::LineageCorrupt { .. }));
}

#[test]
fn shadow_swap_rejects_unrelated_lineage_and_model() {
    let engine = engine();
    engine.train(&training_samples(0..1));
    let current = engine.predictor_commit();
    let root = engine
        .predictor_lineage()
        .into_iter()
        .next()
        .expect("the engine must retain a root");

    let mut branch_ensemble = ModelEnsemble::new();
    branch_ensemble.update_all(&training_samples(100..101)[0]);
    let branch = ModelCommit::new(
        ModelId::new("shadow"),
        branch_ensemble.save_all(),
        Some(root.commit_id.clone()),
        1,
    );
    let branch_predictor = ModelEnsemblePredictor::from_model_commit_with_lineage(
        &branch,
        &[root.clone(), branch.clone()],
    )
    .unwrap();
    let error = match engine.swap(branch_predictor) {
        Ok(_) => panic!(
            "a branch that does not contain the active commit must not replace the predictor"
        ),
        Err(error) => error,
    };
    assert!(matches!(error, ReplayError::LineageCorrupt { .. }));
    assert_eq!(engine.predictor_commit().commit_id, current.commit_id);

    let other_root = ModelCommit::new(
        ModelId::new("other-shadow"),
        ModelEnsemble::new().save_all(),
        None,
        0,
    );
    let other_predictor = ModelEnsemblePredictor::from_model_commit(&other_root).unwrap();
    let error = match engine.swap(other_predictor) {
        Ok(_) => panic!("a predictor from another model lineage must not replace the predictor"),
        Err(error) => error,
    };
    assert!(matches!(error, ReplayError::InvalidCommit { .. }));
    assert_eq!(engine.predictor_commit().commit_id, current.commit_id);
}
