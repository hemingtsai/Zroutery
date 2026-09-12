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

use std::time::Duration;

use zroutery_core::config::ModelTier;
use zroutery_core::feedback::DataOrigin;
use zroutery_core::ml::coordinator::{CoordinatorConfig, RoutingAction};
use zroutery_core::ml::dataset::{Targets, TrainingSample as DatasetTrainingSample};
use zroutery_core::ml::decision_engine::DecisionEngine;
use zroutery_core::ml::features::{
    RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION,
};
use zroutery_core::ml::model::ModelState;
use zroutery_core::ml::model_identity::{CommitId, ReplayError};
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
    }
}

fn shadow_input_with(candidates: Vec<ShadowCandidateInput>) -> ShadowInput {
    ShadowInput {
        decision_id: "dec-1".to_string(),
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
            assert!(reason.contains("latency_linear"), "unexpected reason: {reason}");
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
    assert!(matches!(err, ReplayError::IncompatibleState { .. }));

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
    assert_eq!(batched_decision.shadow.action, chained_decision.shadow.action);
    assert_eq!(batched_decision.shadow.reason, chained_decision.shadow.reason);
}

/// GATE 7E-1 (determinism): training materially changes the commit and the
/// decision for an input whose feature regions were trained.
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
        },
        ShadowCandidateInput {
            candidate_id: "model-b".to_string(),
            provider_id: "prov-b".to_string(),
            tier: Some(ModelTier::Standard),
            eligible: true,
            features: make_features(0.30),
        },
        ShadowCandidateInput {
            candidate_id: "model-c".to_string(),
            provider_id: "prov-c".to_string(),
            tier: Some(ModelTier::Reasoning),
            eligible: true,
            features: make_features(0.50),
        },
        ShadowCandidateInput {
            candidate_id: "model-d".to_string(),
            provider_id: "prov-d".to_string(),
            tier: None,
            eligible: false,
            features: make_features(0.70),
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
        Some("ineligible")
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
/// 8-candidate input stays under P95 1ms / P99 3ms.
#[test]
fn shadow_overhead_p95_under_1ms_p99_under_3ms() {
    let engine = engine();
    engine.train(&training_samples(0..100));

    let candidates: Vec<ShadowCandidateInput> = (0..8)
        .map(|i| {
            candidate_input(
                &format!("model-{}", i),
                &format!("prov-{}", i % 3),
                0.10 + i as f32 * 0.08,
            )
        })
        .collect();
    let mut input = shadow_input_with(candidates);
    input.production_selected = "model-3".to_string();

    // Warm-up: allocator, caches, store growth.
    for _ in 0..50 {
        assert!(engine.evaluate("req-perf", &input).is_some());
    }

    const SAMPLES: usize = 1_000;
    let mut durations: Vec<Duration> = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = std::time::Instant::now();
        let decision = engine.evaluate("req-perf", &input);
        durations.push(start.elapsed());
        assert!(decision.is_some());
    }

    durations.sort_unstable();
    let p95 = durations[SAMPLES * 95 / 100];
    let p99 = durations[SAMPLES * 99 / 100];
    let max = durations[SAMPLES - 1];
    println!(
        "shadow evaluate overhead (8 candidates, {} samples): p95={:?} p99={:?} max={:?}",
        SAMPLES, p95, p99, max
    );
    assert!(
        p95 <= Duration::from_millis(1),
        "p95 {:?} exceeds the 1ms budget",
        p95
    );
    assert!(
        p99 <= Duration::from_millis(3),
        "p99 {:?} exceeds the 3ms budget",
        p99
    );
}

// ---------------------------------------------------------------------------
// Coordinator semantics through the full shadow path
// ---------------------------------------------------------------------------

/// GATE 7E-1 (coordinator semantics): the frozen session guard survives the
/// full shadow path — sticky session + low confidence forces Keep with the
/// session reason even when the trained utility favors a switch.
#[test]
fn shadow_respects_coordinator_semantics() {
    let engine = engine();
    let features_b = polarized_features(true);
    let features_a = polarized_features(false);
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
    engine.train(&samples);

    let candidates = vec![
        candidate_input_with_features("model-b", "prov-b", features_b),
        candidate_input_with_features("model-a", "prov-a", features_a),
    ];

    // Control (Free session): the utility path switches to model-b, proving
    // the training really favors it.
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

    for (name, src) in [("shadow.rs", shadow_src), ("decision_engine.rs", engine_src)] {
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
