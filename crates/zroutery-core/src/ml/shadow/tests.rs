// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

use super::records::{shadow_decision_checksum, shadow_input_checksum};
use super::*;
use crate::config::ModelTier;
use crate::feedback::DataOrigin;
use crate::ml::coordinator::CoordinatorConfig;
use crate::ml::coordinator::RoutingAction;
use crate::ml::dataset::Targets;
use crate::ml::dataset::TrainingSample as DatasetTrainingSample;
use crate::ml::decision_engine::DecisionEngine;
use crate::ml::features::RoutingFeatures;
use crate::ml::features::FEATURE_SCHEMA_VERSION;
use crate::ml::model::Prediction;
use crate::ml::model::RoutingModel;
use crate::ml::model_identity::{CommitId, ModelCommit, ModelEnsemble, ModelId};
use crate::ml::reward::PredictionBundle;
use crate::ml::reward::RewardPolicy;
use crate::policy::{PolicyRevision, TaskProfileSummary};
use crate::session::SessionRoutingMode;

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
    let predictor =
        ModelEnsemblePredictor::from_model_commit(&commit).expect("round-trip commit must load");
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
