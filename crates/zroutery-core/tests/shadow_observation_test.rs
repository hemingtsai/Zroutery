#![cfg(feature = "ml")]

//! Focused 7E-1B-CORE gates for replayable shadow observations.
//!
//! These tests intentionally exercise only the pure ML seam. They do not
//! construct runtime session/outcome state or invoke a provider.

use zroutery_core::config::{ModelEntry, ModelTier, ProviderConfig, ProviderKind};
use zroutery_core::ml::coordinator::{CoordinatorConfig, RoutingAction};
use zroutery_core::ml::decision_engine::DecisionEngine;
use zroutery_core::ml::features::{
    RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION, UNKNOWN,
};
use zroutery_core::ml::model::Prediction;
use zroutery_core::ml::model_identity::CommitId;
use zroutery_core::ml::reward::{PredictionBundle, RewardPolicy};
use zroutery_core::ml::shadow::{
    EnsemblePredictor, ShadowCandidateInput, ShadowEngine, ShadowInput, ShadowStore,
};
use zroutery_core::observation::ObservationStore;
use zroutery_core::policy::{
    CandidateDecision, DecisionReason, PolicyRevision, RouteDecision, TaskProfile,
    TaskProfileSummary,
};
use zroutery_core::router::Candidate;
use zroutery_core::session::SessionRoutingMode;
use zroutery_core::stats_ext::StatsStore;

fn decision_engine() -> DecisionEngine {
    DecisionEngine::new(CoordinatorConfig::default(), RewardPolicy::default())
}

fn features(seed: f32) -> RoutingFeatures {
    let mut values = [0.0f32; FEATURE_DIMENSION];
    for (index, value) in values.iter_mut().enumerate() {
        *value = seed + index as f32 * 0.01;
    }
    RoutingFeatures {
        values,
        schema_version: FEATURE_SCHEMA_VERSION,
    }
}

fn candidate(id: &str, provider: &str, seed: f32) -> ShadowCandidateInput {
    ShadowCandidateInput {
        candidate_id: id.to_string(),
        provider_id: provider.to_string(),
        tier: Some(ModelTier::Standard),
        eligible: true,
        features: features(seed),
        rejection_reason: None,
    }
}

fn input(candidates: Vec<ShadowCandidateInput>) -> ShadowInput {
    ShadowInput {
        decision_id: "decision-1".to_string(),
        policy_id: "policy-1".to_string(),
        client_id: None,
        policy_revision: PolicyRevision {
            policy_id: "policy-1".to_string(),
            policy_enabled: true,
            requirements_hash: 11,
            preference_hash: 22,
        },
        task: TaskProfileSummary {
            complexity: "standard".to_string(),
            task_type: "chat".to_string(),
            context_tokens: 128,
            estimated_output_tokens: 64,
            streaming: false,
            has_tools: false,
            has_vision: false,
            required_capabilities: Vec::new(),
        },
        production_selected: "model-a".to_string(),
        feature_schema: FEATURE_SCHEMA_VERSION,
        candidates,
        session_mode: SessionRoutingMode::Free,
        session_switch_count: 0,
        is_fallback: false,
    }
}

/// Deterministic candidate-aware fixture. The production plan deliberately
/// puts the poor candidate first; the ML utility evidence puts model-b first.
struct DeterministicPredictor;

impl EnsemblePredictor for DeterministicPredictor {
    fn predict(
        &self,
        model: &str,
        provider: &str,
        _features: &RoutingFeatures,
    ) -> PredictionBundle {
        let (success, latency, ttft, cost) = if model == "model-b" {
            (0.99, 100.0, 40.0, 0.001)
        } else {
            (0.20, 4_000.0, 1_500.0, 0.9)
        };
        PredictionBundle {
            candidate_model: model.to_string(),
            candidate_provider: provider.to_string(),
            success: Prediction::trained(success, 0.95, 100),
            latency: Prediction::trained(latency, 0.95, 100),
            ttft: Prediction::trained(ttft, 0.95, 100),
            cost: Prediction::trained(cost, 0.95, 100),
        }
    }

    fn commit_id(&self) -> CommitId {
        CommitId::new("fixture-shadow-commit")
    }
}

fn selected_first_input() -> ShadowInput {
    input(vec![
        candidate("model-a", "provider-a", 0.10),
        candidate("model-b", "provider-b", 0.80),
    ])
}

#[test]
fn selected_first_plan_gets_non_degenerate_ml_counterfactual() {
    let engine = ShadowEngine::new(decision_engine(), true);
    let snapshot = selected_first_input();
    let decision = engine
        .evaluate_with("request-selected-first", &snapshot, &DeterministicPredictor)
        .expect("selected-first evaluation");

    assert_eq!(decision.shadow.action, RoutingAction::Switch);
    assert_eq!(decision.shadow.selected, "model-b");
    assert_eq!(
        decision.shadow.ranked_candidates,
        vec!["model-b", "model-a"]
    );
    assert_eq!(decision.actual.planned_selected(), "model-a");
    assert_eq!(decision.actual.served, None);
    assert!(!decision.actual.has_served_identity());
    assert_eq!(
        decision.observation.model_commit,
        decision.shadow.model_commit
    );
    assert_eq!(decision.input().policy_id, snapshot.policy_id);
    assert_eq!(decision.input().task.task_type, snapshot.task.task_type);
    assert_eq!(
        decision.input().candidates[0].features,
        snapshot.candidates[0].features
    );
    assert_eq!(
        decision.input().candidates[1].features,
        snapshot.candidates[1].features
    );
}

#[test]
fn route_decision_rejected_candidate_is_retained_but_never_actionable() {
    let entry = ModelEntry::for_upstream("provider-a", "model-a", Some(ModelTier::Standard));
    let provider = ProviderConfig::new("provider-a", "Provider A", ProviderKind::OpenAICompatible);
    let executable = Candidate {
        exposed_id: entry.exposed_id(),
        entry,
        provider,
        degraded: false,
    };
    let selected_id = executable.exposed_id.clone();
    let route_decision = RouteDecision {
        decision_id: "route-1".to_string(),
        timestamp: 1_000,
        task: TaskProfileSummary::default(),
        policy_id: "vision-policy".to_string(),
        client_id: None,
        candidates: vec![
            CandidateDecision {
                model_id: selected_id.clone(),
                provider_id: "provider-a".to_string(),
                tier: Some("standard".to_string()),
                eligible: true,
                rejection: None,
                score: None,
                final_score: None,
            },
            CandidateDecision {
                model_id: "provider-z-model".to_string(),
                provider_id: "provider-z".to_string(),
                tier: Some("fast".to_string()),
                eligible: false,
                rejection: Some("vision capability required".to_string()),
                score: None,
                final_score: None,
            },
        ],
        selected: Some(selected_id.clone()),
        fallback_chain: Vec::new(),
        reason: DecisionReason::PolicySelected,
        policy_revision: PolicyRevision {
            policy_id: "vision-policy".to_string(),
            policy_enabled: true,
            requirements_hash: 101,
            preference_hash: 202,
        },
        // No learned model was consulted for this fixture decision.
        ml_ranking: None,
    };

    let snapshot = ShadowInput::from_policy_plan(
        &ObservationStore::new(),
        &StatsStore::new(),
        std::slice::from_ref(&executable),
        &route_decision,
        &TaskProfile::default(),
        1,
    );

    assert_eq!(snapshot.candidates.len(), 2);
    assert_eq!(snapshot.policy_id, "vision-policy");
    assert_eq!(snapshot.policy_revision.requirements_hash, 101);
    assert_eq!(snapshot.task.task_type, route_decision.task.task_type);
    assert_eq!(
        snapshot.task.context_tokens,
        route_decision.task.context_tokens
    );
    let rejected_input = &snapshot.candidates[1];
    assert!(!rejected_input.eligible);
    assert_eq!(
        rejected_input.rejection_reason.as_deref(),
        Some("vision capability required")
    );
    assert!(rejected_input
        .features
        .values
        .iter()
        .all(|value| *value == UNKNOWN));

    let engine = ShadowEngine::new(decision_engine(), true);
    let decision = engine
        .evaluate_with("request-rejected", &snapshot, &DeterministicPredictor)
        .expect("rejected evidence evaluation");
    let rejected_evidence = &decision.candidates[1];
    assert!(!rejected_evidence.eligible);
    assert!(!rejected_evidence.valid);
    assert_eq!(
        rejected_evidence.rejection_reason.as_deref(),
        Some("vision capability required")
    );
    assert!(!decision
        .shadow
        .ranked_candidates
        .iter()
        .any(|id| id == "provider-z-model"));
    assert_ne!(decision.shadow.selected, "provider-z-model");
}

#[test]
fn rejected_candidate_cannot_be_promoted_by_a_tampered_verdict() {
    let engine = ShadowEngine::new(decision_engine(), true);
    let mut snapshot = selected_first_input();
    snapshot.candidates.push(ShadowCandidateInput {
        candidate_id: "model-z".to_string(),
        provider_id: "provider-z".to_string(),
        tier: Some(ModelTier::Fast),
        eligible: false,
        features: RoutingFeatures::default(),
        rejection_reason: Some("policy rejected".to_string()),
    });
    let mut decision = engine
        .evaluate_with("request-tamper", &snapshot, &DeterministicPredictor)
        .expect("baseline evaluation");
    decision.shadow.selected = "model-z".to_string();
    decision.shadow.action = RoutingAction::Switch;
    decision.shadow.ranked_candidates = vec!["model-b".to_string(), "model-a".to_string()];

    let error = ShadowStore::new(2, 3600)
        .push(decision)
        .expect_err("a rejected/non-planned action must not be stored");
    assert!(error.contains("must be eligible and valid"));
}

#[test]
fn observation_round_trip_replays_same_checksums() {
    let engine = ShadowEngine::new(decision_engine(), true);
    let snapshot = selected_first_input();
    let first = engine
        .evaluate_with("request-round-trip", &snapshot, &DeterministicPredictor)
        .expect("first evaluation");

    let json = serde_json::to_string(&first).expect("serialize shadow decision");
    let restored: zroutery_core::ml::ShadowDecision =
        serde_json::from_str(&json).expect("deserialize shadow decision");
    assert_eq!(
        restored.decision_input_checksum,
        first.decision_input_checksum
    );
    assert_eq!(restored.decision_checksum, first.decision_checksum);
    assert_eq!(
        restored.observation.input.candidates[0].features,
        snapshot.candidates[0].features
    );
    assert_eq!(
        restored.observation.input.policy_revision,
        snapshot.policy_revision
    );
    assert_eq!(restored.actual.served, None);

    ShadowStore::new(4, 3600)
        .push(restored.clone())
        .expect("round-tripped observation remains a valid stored record");

    let replay_engine = ShadowEngine::new(decision_engine(), true);
    let replay = replay_engine
        .evaluate_with(
            "request-replay",
            &restored.observation.input,
            &DeterministicPredictor,
        )
        .expect("replay evaluation");
    assert_eq!(
        replay.decision_input_checksum,
        first.decision_input_checksum
    );
    assert_eq!(replay.decision_checksum, first.decision_checksum);
    assert_eq!(replay.shadow.action, first.shadow.action);
    assert_eq!(replay.shadow.selected, first.shadow.selected);
    assert_eq!(
        replay.shadow.ranked_candidates,
        first.shadow.ranked_candidates
    );
}

#[test]
fn policy_and_task_identity_are_part_of_replay_checksum() {
    let engine = ShadowEngine::new(decision_engine(), true);
    let baseline_input = selected_first_input();
    let baseline = engine
        .evaluate_with("request-baseline", &baseline_input, &DeterministicPredictor)
        .expect("baseline evaluation");

    let mut policy_changed = baseline_input.clone();
    policy_changed.policy_id.push_str("-v2");
    let policy = engine
        .evaluate_with("request-policy", &policy_changed, &DeterministicPredictor)
        .expect("policy variant evaluation");
    assert_ne!(
        policy.decision_input_checksum,
        baseline.decision_input_checksum
    );

    let mut client_changed = baseline_input.clone();
    client_changed.client_id = Some("client-a".to_string());
    let client = engine
        .evaluate_with("request-client", &client_changed, &DeterministicPredictor)
        .expect("client variant evaluation");
    assert_ne!(
        client.decision_input_checksum,
        baseline.decision_input_checksum
    );

    let mut task_changed = baseline_input;
    task_changed.task.task_type = "code".to_string();
    let task = engine
        .evaluate_with("request-task", &task_changed, &DeterministicPredictor)
        .expect("task variant evaluation");
    assert_ne!(
        task.decision_input_checksum,
        baseline.decision_input_checksum
    );
    assert_ne!(task.decision_checksum, baseline.decision_checksum);
}

#[test]
fn evaluation_does_not_mutate_the_input_snapshot() {
    let engine = ShadowEngine::new(decision_engine(), true);
    let snapshot = selected_first_input();
    let before_values = snapshot.candidates[0].features.values;
    let before_policy = snapshot.policy_id.clone();
    let before_selection = snapshot.production_selected.clone();

    let decision = engine
        .evaluate_with("request-pure", &snapshot, &DeterministicPredictor)
        .expect("pure evaluation");

    assert_eq!(snapshot.candidates[0].features.values, before_values);
    assert_eq!(snapshot.policy_id, before_policy);
    assert_eq!(snapshot.production_selected, before_selection);
    assert_eq!(decision.actual.served, None);
}
