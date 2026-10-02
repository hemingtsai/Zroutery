#![cfg(feature = "ml")]

//! Focused gates for the candidate-aware decision contract.
//!
//! These tests exercise only the pure ML seam: a retained [`ShadowInput`], the
//! typed contract built from it, and the accepted [`DecisionEngine`] it feeds.
//! No runtime store, provider, or pipeline is involved.
//!
//! Coverage:
//!
//! - `ModelInput` refuses malformed and wrong-dimension snapshots instead of
//!   repairing them, and two builds from one retained snapshot are equal.
//! - `DecisionModel` refuses a wrong-dimension or wrong-schema contract, and
//!   scores a retained input over its exact feature vectors.
//! - Every illegal `DecisionState` transition is rejected, and the exhaustive
//!   4x4 phase matrix is checked rather than sampled.
//! - `DecisionCandidate` keeps the planned / last-attempted / served identity
//!   slots distinct.
//! - `DecisionDistribution` refuses a wrong-arity, non-normalized, negative,
//!   non-finite, duplicated, or unattached distribution.
//! - The contract's scores project onto the accepted engine's bundle shape with
//!   the same identity and eligibility the engine then classifies.

use zroutery_core::config::ModelTier;
use zroutery_core::ml::coordinator::{CoordinatorConfig, RoutingAction};
use zroutery_core::ml::decision_contract::{
    CandidateEligibility, CandidateScore, DecisionCandidate, DecisionContractError,
    DecisionDimension, DecisionDistribution, DecisionModel, DecisionModelStates, DecisionPhase,
    DecisionState, ModelInput, OpenStep, SettledStep, DISTRIBUTION_NORMALIZATION_TOLERANCE,
};
use zroutery_core::ml::decision_engine::{DecisionEngine, EngineCandidate, EngineInput};
use zroutery_core::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use zroutery_core::ml::model_identity::CommitId;
use zroutery_core::ml::reward::RewardPolicy;
use zroutery_core::ml::shadow::{ShadowCandidateInput, ShadowInput, ShadowObservation};
use zroutery_core::outcome::{CandidateIdentity, OutcomeIdentity};
use zroutery_core::policy::{PolicyRevision, TaskProfileSummary};
use zroutery_core::session::SessionRoutingMode;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

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

fn candidate_snapshot(id: &str, provider: &str, seed: f32) -> ShadowCandidateInput {
    ShadowCandidateInput {
        candidate_id: id.to_string(),
        provider_id: provider.to_string(),
        tier: Some(ModelTier::Standard),
        eligible: true,
        features: features(seed),
        rejection_reason: None,
    }
}

fn rejected_snapshot(id: &str, provider: &str, reason: &str) -> ShadowCandidateInput {
    ShadowCandidateInput {
        candidate_id: id.to_string(),
        provider_id: provider.to_string(),
        tier: None,
        eligible: false,
        features: features(0.5),
        rejection_reason: Some(reason.to_string()),
    }
}

/// A retained decision-time snapshot: two eligible candidates plus one the
/// production decision rejected, in that order.
fn snapshot() -> ShadowInput {
    ShadowInput {
        decision_id: "decision-7e2a".to_string(),
        policy_id: "policy-1".to_string(),
        client_id: Some("client-1".to_string()),
        policy_revision: PolicyRevision {
            policy_id: "policy-1".to_string(),
            policy_enabled: true,
            requirements_hash: 11,
            preference_hash: 22,
        },
        task: TaskProfileSummary {
            complexity: "standard".to_string(),
            task_type: "code".to_string(),
            context_tokens: 2048,
            estimated_output_tokens: 512,
            streaming: true,
            has_tools: true,
            has_vision: false,
            required_capabilities: vec!["tools".to_string()],
        },
        production_selected: "model-a".to_string(),
        feature_schema: FEATURE_SCHEMA_VERSION,
        candidates: vec![
            candidate_snapshot("model-a", "provider-a", 0.10),
            candidate_snapshot("model-b", "provider-b", 0.80),
            rejected_snapshot("model-c", "provider-c", "policy rejected"),
        ],
        session_mode: SessionRoutingMode::Free,
        session_switch_count: 0,
        is_fallback: false,
    }
}

fn model_input() -> ModelInput {
    ModelInput::try_from_shadow_input(&snapshot()).expect("the fixture snapshot is well formed")
}

fn contract() -> DecisionModel {
    DecisionModel::try_cold(
        FEATURE_DIMENSION,
        FEATURE_SCHEMA_VERSION,
        CommitId::new("decision-contract-fixture"),
    )
    .expect("the fixture contract is well formed")
}

fn identity(model: &str, provider: &str) -> CandidateIdentity {
    CandidateIdentity::new(model, provider)
}

/// The engine's candidate shape, built from a contract score. This adapter
/// lives in the test on purpose: it is the only place the two surfaces meet,
/// and the accepted engine stays authoritative for everything it decides.
fn engine_candidate(score: &CandidateScore) -> EngineCandidate {
    EngineCandidate {
        candidate_id: score.identity.model().to_string(),
        bundle: score.to_prediction_bundle(),
        eligible: score.eligibility.is_eligible(),
    }
}

fn scored_step(input: &ModelInput) -> SettledStep {
    SettledStep::CandidatesScored {
        scored: input.candidate_count(),
        eligible: input.eligible_candidates().len(),
    }
}

/// The settled step a legal transition *to* `phase` must carry.
fn step_for(phase: DecisionPhase, input: &ModelInput) -> SettledStep {
    match phase {
        DecisionPhase::InputAccepted => SettledStep::InputAccepted {
            candidates: input.candidate_count(),
        },
        DecisionPhase::CandidatesScored => scored_step(input),
        DecisionPhase::CandidateSelected => SettledStep::CandidateSelected {
            selected: identity("model-b", "provider-b"),
        },
        DecisionPhase::Committed => SettledStep::Committed,
    }
}

// ---------------------------------------------------------------------------
// ModelInput: construction, rejection, replay
// ---------------------------------------------------------------------------

#[test]
fn model_input_is_built_from_retained_vectors_without_re_deriving_them() {
    let retained = snapshot();
    let input = ModelInput::try_from_shadow_input(&retained).expect("valid snapshot");

    assert_eq!(input.feature_schema, FEATURE_SCHEMA_VERSION);
    assert_eq!(input.feature_dimension, FEATURE_DIMENSION);
    assert_eq!(input.candidate_count(), 3);
    // Production order is evidence and is preserved, never sorted.
    let models: Vec<&str> = input.candidates.iter().map(|c| c.model()).collect();
    assert_eq!(models, vec!["model-a", "model-b", "model-c"]);

    // Every candidate's feature vector is the retained one, bit for bit.
    for (typed, source) in input.candidates.iter().zip(retained.candidates.iter()) {
        assert_eq!(typed.features, source.features);
        assert_eq!(typed.identity.model(), source.candidate_id);
        assert_eq!(typed.identity.provider(), source.provider_id);
    }

    // The planned selection is the production plan's, with the provider half
    // resolved from the candidate production actually observed.
    assert_eq!(input.planned_model(), "model-a");
    assert_eq!(input.identities.planned, identity("model-a", "provider-a"));
    assert_eq!(input.identities.last_attempted, None);
    assert_eq!(input.identities.served, None);

    // The rejected candidate keeps its production reason and is not eligible.
    let rejected = &input.candidates[2];
    assert!(!rejected.is_eligible());
    assert_eq!(rejected.rejection_reason(), Some("policy rejected"));
    assert_eq!(input.eligible_candidates().len(), 2);
}

#[test]
fn model_input_rejects_wrong_dimension() {
    let retained = snapshot();
    for dimension in [0usize, 1, FEATURE_DIMENSION - 1, FEATURE_DIMENSION + 1] {
        let error = ModelInput::try_new(dimension, retained.clone())
            .expect_err("a wrong dimension must be refused");
        assert!(
            matches!(
                error,
                DecisionContractError::FeatureDimensionMismatch { found, expected }
                    if found == dimension && expected == FEATURE_DIMENSION
            ),
            "unexpected error for dimension {dimension}: {error:?}"
        );
    }
    // The declared dimension is the only accepted one.
    assert!(ModelInput::try_new(FEATURE_DIMENSION, retained).is_ok());
}

#[test]
fn model_input_rejects_malformed_snapshots() {
    // Unsupported feature schema on the input.
    let mut wrong_schema = snapshot();
    wrong_schema.feature_schema = FEATURE_SCHEMA_VERSION + 1;
    assert_eq!(
        ModelInput::try_from_shadow_input(&wrong_schema).err(),
        Some(DecisionContractError::UnsupportedFeatureSchema {
            found: FEATURE_SCHEMA_VERSION + 1,
            expected: FEATURE_SCHEMA_VERSION,
        })
    );

    // A candidate vector stamped with a different schema than the input.
    let mut wrong_candidate_schema = snapshot();
    wrong_candidate_schema.candidates[1].features.schema_version = FEATURE_SCHEMA_VERSION + 7;
    assert!(matches!(
        ModelInput::try_from_shadow_input(&wrong_candidate_schema).err(),
        Some(DecisionContractError::CandidateFeatureSchemaMismatch { found, expected, .. })
            if found == FEATURE_SCHEMA_VERSION + 7 && expected == FEATURE_SCHEMA_VERSION
    ));

    // No candidates at all.
    let mut empty = snapshot();
    empty.candidates.clear();
    assert_eq!(
        ModelInput::try_from_shadow_input(&empty).err(),
        Some(DecisionContractError::EmptyCandidateSet)
    );

    // An empty candidate identity.
    let mut empty_identity = snapshot();
    empty_identity.candidates[0].candidate_id = "   ".to_string();
    assert_eq!(
        ModelInput::try_from_shadow_input(&empty_identity).err(),
        Some(DecisionContractError::EmptyCandidateIdentity)
    );
    let mut empty_provider = snapshot();
    empty_provider.candidates[0].provider_id = String::new();
    assert_eq!(
        ModelInput::try_from_shadow_input(&empty_provider).err(),
        Some(DecisionContractError::EmptyCandidateIdentity)
    );

    // A duplicate identity in the ordered set.
    let mut duplicate = snapshot();
    duplicate.candidates[1].candidate_id = "model-a".to_string();
    duplicate.candidates[1].provider_id = "provider-a".to_string();
    assert_eq!(
        ModelInput::try_from_shadow_input(&duplicate).err(),
        Some(DecisionContractError::DuplicateCandidateIdentity(
            "model-a".to_string()
        ))
    );

    // A planned selection production never observed.
    let mut unobserved_plan = snapshot();
    unobserved_plan.production_selected = "model-zz".to_string();
    assert_eq!(
        ModelInput::try_from_shadow_input(&unobserved_plan).err(),
        Some(DecisionContractError::PlannedIdentityNotObserved(
            "model-zz".to_string()
        ))
    );
    let mut empty_plan = snapshot();
    empty_plan.production_selected = String::new();
    assert_eq!(
        ModelInput::try_from_shadow_input(&empty_plan).err(),
        Some(DecisionContractError::PlannedIdentityNotObserved(
            String::new()
        ))
    );
}

#[test]
fn model_input_rejects_non_finite_features_in_every_slot() {
    for slot in 0..FEATURE_DIMENSION {
        let mut poisoned = snapshot();
        poisoned.candidates[1].features.values[slot] = f32::NAN;
        let error = ModelInput::try_from_shadow_input(&poisoned)
            .expect_err("a non-finite feature must be refused");
        assert!(
            matches!(
                error,
                DecisionContractError::NonFiniteFeature { index, value, .. }
                    if index == slot && value.is_nan()
            ),
            "slot {slot} produced {error:?}"
        );
    }
    for poison in [f32::INFINITY, f32::NEG_INFINITY] {
        let mut poisoned = snapshot();
        poisoned.candidates[0].features.values[3] = poison;
        assert!(matches!(
            ModelInput::try_from_shadow_input(&poisoned).err(),
            Some(DecisionContractError::NonFiniteFeature { index: 3, .. })
        ));
    }
}

#[test]
fn model_input_refuses_contradictory_eligibility() {
    // Eligible and rejected at the same time.
    let mut both = snapshot();
    both.candidates[0].rejection_reason = Some("policy rejected".to_string());
    assert!(matches!(
        ModelInput::try_from_shadow_input(&both).err(),
        Some(DecisionContractError::EligibleWithRejectionEvidence { .. })
    ));

    // Ineligible with no reason at all.
    let mut no_reason = snapshot();
    no_reason.candidates[2].rejection_reason = None;
    assert!(matches!(
        ModelInput::try_from_shadow_input(&no_reason).err(),
        Some(DecisionContractError::MissingRejectionEvidence { .. })
    ));

    // Ineligible with a blank reason.
    let mut blank = snapshot();
    blank.candidates[2].rejection_reason = Some("   ".to_string());
    assert!(matches!(
        ModelInput::try_from_shadow_input(&blank).err(),
        Some(DecisionContractError::EmptyRejectionReason { .. })
    ));
}

#[test]
fn two_builds_from_one_retained_snapshot_are_equal() {
    let retained = snapshot();
    let first = ModelInput::try_from_shadow_input(&retained).expect("first build");
    let second = ModelInput::try_from_shadow_input(&retained).expect("second build");
    assert_eq!(first, second);

    // The same is true through the observation seam, which pairs the same
    // snapshot with a pinned commit.
    let observation = ShadowObservation {
        input: retained.clone(),
        model_commit: CommitId::new("decision-contract-fixture"),
    };
    let from_observation =
        ModelInput::try_from_shadow_observation(&observation).expect("observation build");
    assert_eq!(first, from_observation);
    assert_eq!(second, from_observation);

    // The retained snapshot is not disturbed by building from it.
    let pristine = snapshot();
    assert_eq!(retained.production_selected, pristine.production_selected);
    assert_eq!(retained.decision_id, pristine.decision_id);
    assert_eq!(retained.candidates.len(), pristine.candidates.len());
    for (retained_candidate, pristine_candidate) in
        retained.candidates.iter().zip(pristine.candidates.iter())
    {
        assert_eq!(
            retained_candidate.candidate_id,
            pristine_candidate.candidate_id
        );
        assert_eq!(
            retained_candidate.provider_id,
            pristine_candidate.provider_id
        );
        assert_eq!(retained_candidate.eligible, pristine_candidate.eligible);
        assert_eq!(
            retained_candidate.rejection_reason,
            pristine_candidate.rejection_reason
        );
        assert_eq!(retained_candidate.features, pristine_candidate.features);
    }

    // A different decision-time input is genuinely different, so equality is
    // not trivially true.
    let mut other = snapshot();
    other.session_switch_count = 2;
    let other_input = ModelInput::try_from_shadow_input(&other).expect("other build");
    assert_ne!(first, other_input);
}

#[test]
fn two_builds_score_identically() {
    let retained = snapshot();
    let model = contract();
    let first = ModelInput::try_from_shadow_input(&retained).expect("first build");
    let second = ModelInput::try_from_shadow_input(&retained).expect("second build");

    let first_scores = model.try_score_input(&first).expect("first scores");
    let second_scores = model.try_score_input(&second).expect("second scores");
    assert_eq!(first_scores.len(), second_scores.len());
    for (left, right) in first_scores.iter().zip(second_scores.iter()) {
        assert_eq!(left.identity, right.identity);
        for dimension in DecisionDimension::ALL {
            let left_prediction = left.predictions.get(dimension);
            let right_prediction = right.predictions.get(dimension);
            assert_eq!(left_prediction.value, right_prediction.value);
            assert_eq!(left_prediction.confidence, right_prediction.confidence);
            assert_eq!(left_prediction.sample_count, right_prediction.sample_count);
            assert_eq!(left_prediction.cold, right_prediction.cold);
        }
    }
}

// ---------------------------------------------------------------------------
// DecisionModel
// ---------------------------------------------------------------------------

#[test]
fn decision_model_refuses_a_wrong_dimension_schema_or_commit() {
    assert!(matches!(
        DecisionModel::try_cold(
            FEATURE_DIMENSION + 1,
            FEATURE_SCHEMA_VERSION,
            CommitId::new("c")
        )
        .err(),
        Some(DecisionContractError::FeatureDimensionMismatch {
            found,
            expected: FEATURE_DIMENSION
        }) if found == FEATURE_DIMENSION + 1
    ));
    assert!(matches!(
        DecisionModel::try_cold(
            FEATURE_DIMENSION,
            FEATURE_SCHEMA_VERSION + 1,
            CommitId::new("c")
        )
        .err(),
        Some(DecisionContractError::UnsupportedFeatureSchema { .. })
    ));
    assert!(matches!(
        DecisionModel::try_cold(FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION, CommitId::new("")).err(),
        Some(DecisionContractError::EmptyCommitId)
    ));
}

#[test]
fn decision_model_loads_only_verified_per_dimension_states() {
    let commit = CommitId::new("decision-contract-states");
    let states = DecisionModelStates::cold(FEATURE_DIMENSION).expect("cold states");
    let loaded = DecisionModel::try_from_states(
        FEATURE_DIMENSION,
        FEATURE_SCHEMA_VERSION,
        commit.clone(),
        &states,
    )
    .expect("cold states load");
    assert_eq!(loaded.dimension(), FEATURE_DIMENSION);
    assert_eq!(loaded.feature_schema(), FEATURE_SCHEMA_VERSION);
    assert_eq!(loaded.commit(), &commit);
    assert_eq!(loaded.sample_counts(), [0, 0, 0, 0]);

    // A state whose algorithm is not the one that dimension owns is refused.
    let mut corrupt = DecisionModelStates::cold(FEATURE_DIMENSION).expect("cold states");
    corrupt.cost = zroutery_core::ml::model::ModelState::new("cost_linear_wrong", vec![0.0, 0.0]);
    assert!(matches!(
        DecisionModel::try_from_states(
            FEATURE_DIMENSION,
            FEATURE_SCHEMA_VERSION,
            commit.clone(),
            &corrupt
        )
        .err(),
        Some(DecisionContractError::InvalidModelState {
            dimension: "cost",
            ..
        })
    ));

    // A state carrying a non-finite parameter is refused.
    let mut poisoned = DecisionModelStates::cold(FEATURE_DIMENSION).expect("cold states");
    let parameters = vec![0.0, f64::NAN];
    poisoned.latency.checksum = zroutery_core::ml::model::ModelState::compute_checksum(&parameters);
    poisoned.latency.parameters = parameters;
    assert!(matches!(
        DecisionModel::try_from_states(
            FEATURE_DIMENSION,
            FEATURE_SCHEMA_VERSION,
            commit,
            &poisoned
        )
        .err(),
        Some(DecisionContractError::InvalidModelState {
            dimension: "latency",
            ..
        })
    ));
}

#[test]
fn decision_model_scores_every_candidate_over_its_exact_vector() {
    let model = contract();
    let input = model_input();
    let scores = model.try_score_input(&input).expect("scores");

    assert_eq!(scores.len(), input.candidate_count());
    for (score, candidate) in scores.iter().zip(input.candidates.iter()) {
        assert_eq!(score.identity, candidate.identity);
        // Eligibility is carried through untouched, including the rejected one.
        assert_eq!(score.eligibility.is_eligible(), candidate.is_eligible());
        for dimension in DecisionDimension::ALL {
            let prediction = score.predictions.get(dimension);
            assert!(prediction.value.is_finite(), "{dimension:?} value");
            assert!(
                prediction.confidence.is_finite(),
                "{dimension:?} confidence"
            );
        }
        // A cold contract still returns the frozen per-dimension cold biases.
        assert!((score.predictions.latency.value - 500.0).abs() < 1.0);
        assert!((score.predictions.ttft.value - 200.0).abs() < 1.0);
    }
}

#[test]
fn decision_model_refuses_a_candidate_whose_schema_disagrees() {
    let model = contract();
    let input = model_input();
    let mut candidate = input.candidates[1].clone();
    candidate.features.schema_version = FEATURE_SCHEMA_VERSION + 3;
    assert!(matches!(
        model.try_score_candidate(&candidate).err(),
        Some(DecisionContractError::CandidateFeatureSchemaMismatch {
            found,
            expected: FEATURE_SCHEMA_VERSION,
            ..
        }) if found == FEATURE_SCHEMA_VERSION + 3
    ));
}

#[test]
fn decision_model_agrees_with_the_accepted_engine_on_identity_and_eligibility() {
    // The two surfaces meet exactly here: the contract scores, the engine
    // classifies and decides. Nothing in the contract's projection may change
    // the engine's view of a candidate.
    let model = contract();
    let input = model_input();
    let scores = model.try_score_input(&input).expect("scores");
    let candidates: Vec<EngineCandidate> = scores.iter().map(engine_candidate).collect();

    let engine = DecisionEngine::new(CoordinatorConfig::default(), RewardPolicy::default());
    let engine_input = EngineInput {
        current_candidate: input.planned_model(),
        candidates: &candidates,
        session_mode: input.session_mode,
        session_switch_count: input.session_switch_count,
        is_fallback: input.is_fallback,
    };
    let output = engine.decide(&engine_input);

    assert_eq!(output.candidates.len(), scores.len());
    for (score, outcome) in scores.iter().zip(output.candidates.iter()) {
        assert_eq!(outcome.candidate_id, score.identity.model());
        assert_eq!(outcome.eligible, score.eligibility.is_eligible());
    }
    // The rejected candidate never joins the valid set, and the engine still
    // makes the decision over the survivors only.
    let rejected = &output.candidates[2];
    assert!(!rejected.valid);
    assert_eq!(rejected.rejection_reason.as_deref(), Some("ineligible"));
    // The engine's ranked valid set is exactly the contract's eligible set, in
    // production order. A cold contract scores the candidates almost
    // identically, so the decision itself is the engine's business alone.
    assert_eq!(output.ranked_candidates, vec!["model-a", "model-b"]);
    assert!(matches!(output.decision.action, RoutingAction::Keep));
}

#[test]
fn candidate_eligibility_is_typed_and_exclusive() {
    let input = model_input();
    let eligible: &DecisionCandidate = &input.candidates[0];
    assert!(matches!(
        eligible.eligibility,
        CandidateEligibility::Eligible
    ));
    assert_eq!(eligible.eligibility.rejection_reason(), None);

    let rejected: &DecisionCandidate = &input.candidates[2];
    assert!(!rejected.is_eligible());
    assert_eq!(rejected.rejection_reason(), Some("policy rejected"));
    assert!(!DecisionCandidate::new(
        eligible.identity.clone(),
        CandidateEligibility::Rejected {
            reason: "policy rejected".to_string()
        },
        eligible.features.clone(),
    )
    .is_eligible());
}

// ---------------------------------------------------------------------------
// DecisionCandidate: the planned / attempted / served distinction
// ---------------------------------------------------------------------------

#[test]
fn planned_attempted_and_served_stay_three_separate_facts() {
    let mut input = model_input();
    // Decision time establishes exactly one fact: the plan.
    assert!(input.identities.planned == identity("model-a", "provider-a"));
    assert!(input.identities.last_attempted.is_none());
    assert!(input.identities.served.is_none());
    assert!(input.candidates[0].roles.planned);
    assert!(!input.candidates[0].roles.last_attempted);
    assert!(!input.candidates[0].roles.served);
    assert!(input.candidates[1].roles.is_empty());

    // A terminal outcome which attempted and served a different identity.
    input
        .try_record_outcome_identities(&OutcomeIdentity {
            planned: Some(identity("model-a", "provider-a")),
            last_attempted: Some(identity("model-b", "provider-b")),
            served: Some(identity("model-b", "provider-b")),
        })
        .expect("a consistent outcome records cleanly");

    // The plan is untouched: three slots, three separate facts.
    assert!(input.identities.planned == identity("model-a", "provider-a"));
    assert!(input.identities.last_attempted == Some(identity("model-b", "provider-b")));
    assert!(input.identities.served == Some(identity("model-b", "provider-b")));

    // A candidate can legitimately hold more than one slot.
    let planned = input.candidate(&identity("model-a", "provider-a")).unwrap();
    assert!(planned.roles.planned);
    assert!(!planned.roles.last_attempted);
    assert!(!planned.roles.served);
    let served = input.candidate(&identity("model-b", "provider-b")).unwrap();
    assert!(!served.roles.planned);
    assert!(served.roles.last_attempted);
    assert!(served.roles.served);

    // Recording the same facts again is idempotent.
    let before = input.clone();
    input
        .try_record_outcome_identities(&OutcomeIdentity {
            planned: Some(identity("model-a", "provider-a")),
            last_attempted: Some(identity("model-b", "provider-b")),
            served: Some(identity("model-b", "provider-b")),
        })
        .expect("re-stating the same facts is not a contradiction");
    assert_eq!(before, input);
}

#[test]
fn outcome_identities_that_contradict_or_were_never_observed_are_refused() {
    // A different plan is a different decision, not a newer plan.
    let mut input = model_input();
    assert!(matches!(
        input
            .try_record_outcome_identities(&OutcomeIdentity {
                planned: Some(identity("model-b", "provider-b")),
                last_attempted: None,
                served: None,
            })
            .err(),
        Some(DecisionContractError::PlannedIdentityContradicts { .. })
    ));
    assert!(input.identities.planned == identity("model-a", "provider-a"));

    // An identity the decision never observed cannot be attempted or served.
    let mut input = model_input();
    assert!(matches!(
        input
            .try_record_outcome_identities(&OutcomeIdentity {
                planned: None,
                last_attempted: Some(identity("model-zz", "provider-zz")),
                served: None,
            })
            .err(),
        Some(DecisionContractError::UnobservedRoleIdentity {
            role: "last_attempted"
        })
    ));
    assert!(input.identities.last_attempted.is_none());

    // A slot that has been established cannot be re-assigned.
    input
        .try_record_outcome_identities(&OutcomeIdentity {
            planned: None,
            last_attempted: Some(identity("model-b", "provider-b")),
            served: Some(identity("model-b", "provider-b")),
        })
        .expect("first record");
    assert!(matches!(
        input
            .try_record_outcome_identities(&OutcomeIdentity {
                planned: None,
                last_attempted: Some(identity("model-c", "provider-c")),
                served: None,
            })
            .err(),
        Some(DecisionContractError::ContradictoryRoleIdentity {
            role: "last_attempted"
        })
    ));
    assert!(input.identities.last_attempted == Some(identity("model-b", "provider-b")));
    assert!(input.identities.served == Some(identity("model-b", "provider-b")));
}

// ---------------------------------------------------------------------------
// DecisionState
// ---------------------------------------------------------------------------

#[test]
fn a_fresh_decision_knows_what_is_decided_open_and_next() {
    let state = DecisionState::begin(model_input());
    assert_eq!(state.phase(), DecisionPhase::InputAccepted);
    assert_eq!(state.planned(), &identity("model-a", "provider-a"));
    assert_eq!(
        state.settled_steps(),
        &[SettledStep::InputAccepted { candidates: 3 }]
    );
    assert_eq!(
        state.open_steps(),
        &[
            OpenStep::ScoreCandidates,
            OpenStep::SelectCandidate,
            OpenStep::Commit
        ]
    );
    assert_eq!(
        state.allowed_transitions(),
        &[DecisionPhase::CandidatesScored]
    );
    assert!(!state.is_terminal());
}

#[test]
fn the_only_legal_path_runs_input_scored_selected_committed() {
    let input = model_input();
    let mut state = DecisionState::begin(input.clone());
    let selected = identity("model-b", "provider-b");

    for phase in [
        DecisionPhase::CandidatesScored,
        DecisionPhase::CandidateSelected,
        DecisionPhase::Committed,
    ] {
        assert!(state.can_advance(phase), "{phase:?} must be reachable");
        let step = match phase {
            DecisionPhase::CandidatesScored => scored_step(&input),
            DecisionPhase::CandidateSelected => SettledStep::CandidateSelected {
                selected: selected.clone(),
            },
            DecisionPhase::Committed => SettledStep::Committed,
            DecisionPhase::InputAccepted => unreachable!("the first phase is already settled"),
        };
        state.advance(phase, step).expect("legal transition");
        assert_eq!(state.phase(), phase);
    }

    assert!(state.is_terminal());
    assert!(state.open_steps().is_empty());
    assert!(state.allowed_transitions().is_empty());
    assert_eq!(state.settled_steps().len(), 4);
    assert_eq!(state.settled_steps().last(), Some(&SettledStep::Committed));
    // The state is bound to the input it began from.
    assert_eq!(state.input(), &input);
}

/// The exhaustive phase matrix. Every ordered pair is exercised; only the
/// immediate successor of a phase is legal, and nothing else is.
#[test]
fn every_illegal_decision_transition_is_rejected() {
    let phases = DecisionPhase::ORDER;
    let mut legal_pairs = 0usize;

    for from in phases {
        for to in phases {
            let input = model_input();
            let mut state = DecisionState::begin(input.clone());
            // Walk to `from` through the only legal route.
            let mut walked = 0usize;
            while phases[walked] != from {
                let next = phases[walked + 1];
                state
                    .advance(next, step_for(next, &input))
                    .expect("fixture walk is legal");
                walked += 1;
            }
            assert_eq!(state.phase(), from);
            let phase_before = state.phase();
            let settled_before = state.settled_steps().to_vec();
            let open_before = state.open_steps();

            let outcome = state.advance(to, step_for(to, &input));

            if from == DecisionPhase::InputAccepted && to == DecisionPhase::CandidatesScored
                || from == DecisionPhase::CandidatesScored && to == DecisionPhase::CandidateSelected
                || from == DecisionPhase::CandidateSelected && to == DecisionPhase::Committed
            {
                legal_pairs += 1;
                assert!(outcome.is_ok(), "{from:?} -> {to:?} must be legal");
            } else {
                let error = outcome.expect_err("must be refused");
                assert!(
                    matches!(
                        error,
                        DecisionContractError::IllegalTransition {
                            from: reported_from,
                            to: reported_to,
                            ..
                        } if reported_from == from && reported_to == to
                    ),
                    "{from:?} -> {to:?} produced {error:?}"
                );
                // A refused transition changes nothing.
                assert_eq!(state.phase(), phase_before);
                assert_eq!(state.settled_steps(), settled_before.as_slice());
                assert_eq!(state.open_steps(), open_before);
                assert!(!state.can_advance(to));
            }
        }
    }

    assert_eq!(legal_pairs, 3, "exactly three transitions are legal");
}

#[test]
fn a_step_cannot_be_recorded_under_another_phase() {
    let input = model_input();
    let mut state = DecisionState::begin(input);
    assert!(matches!(
        state
            .advance(DecisionPhase::CandidatesScored, SettledStep::Committed)
            .err(),
        Some(DecisionContractError::SettledStepMismatch {
            to: DecisionPhase::CandidatesScored,
            expected: "CandidatesScored"
        })
    ));
    // The refused transition left the state alone.
    assert_eq!(state.phase(), DecisionPhase::InputAccepted);
    assert_eq!(state.settled_steps().len(), 1);
}

#[test]
fn a_settled_step_must_match_the_input_it_claims_to_describe() {
    let input = model_input();
    let mut wrong_score_count = DecisionState::begin(input.clone());
    assert!(matches!(
        wrong_score_count
            .advance(
                DecisionPhase::CandidatesScored,
                SettledStep::CandidatesScored {
                    scored: 2,
                    eligible: 2
                },
            )
            .err(),
        Some(DecisionContractError::ScoredCountMismatch {
            scored: 2,
            observed: 3
        })
    ));

    let mut wrong_eligible_count = DecisionState::begin(input.clone());
    assert!(matches!(
        wrong_eligible_count
            .advance(
                DecisionPhase::CandidatesScored,
                SettledStep::CandidatesScored {
                    scored: 3,
                    eligible: 3
                },
            )
            .err(),
        Some(DecisionContractError::EligibleCountMismatch {
            eligible: 3,
            observed: 2
        })
    ));
    assert_eq!(wrong_eligible_count.phase(), DecisionPhase::InputAccepted);

    // A selection must name an observed, eligible candidate.
    let mut unobserved = DecisionState::begin(input.clone());
    unobserved
        .advance(DecisionPhase::CandidatesScored, scored_step(&input))
        .expect("scored");
    assert!(matches!(
        unobserved
            .advance(
                DecisionPhase::CandidateSelected,
                SettledStep::CandidateSelected {
                    selected: identity("model-zz", "provider-zz")
                },
            )
            .err(),
        Some(DecisionContractError::SelectedIdentityNotObserved(_))
    ));
    assert_eq!(unobserved.phase(), DecisionPhase::CandidatesScored);

    let mut ineligible = DecisionState::begin(input.clone());
    ineligible
        .advance(DecisionPhase::CandidatesScored, scored_step(&input))
        .expect("scored");
    assert!(matches!(
        ineligible
            .advance(
                DecisionPhase::CandidateSelected,
                SettledStep::CandidateSelected {
                    selected: identity("model-c", "provider-c")
                },
            )
            .err(),
        Some(DecisionContractError::SelectedIdentityIneligible(_))
    ));
    assert_eq!(ineligible.phase(), DecisionPhase::CandidatesScored);

    // Keeping the planned candidate is a legal selection.
    let mut keep = DecisionState::begin(input);
    keep.advance(DecisionPhase::CandidatesScored, scored_step(keep.input()))
        .expect("scored");
    keep.advance(
        DecisionPhase::CandidateSelected,
        SettledStep::CandidateSelected {
            selected: identity("model-a", "provider-a"),
        },
    )
    .expect("the planned candidate may be selected");
    assert_eq!(keep.phase(), DecisionPhase::CandidateSelected);
}

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

#[test]
fn a_validated_contract_can_be_recorded() {
    let input = model_input();
    let state = DecisionState::begin(input.clone());
    let scores = contract()
        .try_score_input(&input)
        .expect("a validated input scores");

    // `DecisionDistribution` is deliberately absent here: it derives neither
    // `Serialize` nor `Deserialize`, because a round-tripped distribution would
    // bypass the arity and normalization checks it exists to enforce.
    for record in [
        serde_json::to_string(&input).expect("input records"),
        serde_json::to_string(&state).expect("state records"),
        serde_json::to_string(&scores).expect("scores record"),
    ] {
        assert!(
            serde_json::from_str::<serde_json::Value>(&record).is_ok(),
            "a recorded contract must be valid json"
        );
    }

    let value: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
    // The three identity slots stay three slots on the record: the two that
    // were never established are absent, not defaulted to the planned one.
    assert_eq!(value["input"]["identities"]["planned"]["model"], "model-a");
    assert!(value["input"]["identities"]["last_attempted"].is_null());
    assert!(value["input"]["identities"]["served"].is_null());
    // Production order and typed eligibility survive the record.
    let candidates = value["input"]["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 3);
    assert_eq!(candidates[0]["identity"]["model"], "model-a");
    // An eligible candidate records a bare tag and no reason at all, so the
    // record cannot say "eligible" and "rejected" at the same time.
    assert_eq!(candidates[0]["eligibility"], "eligible");
    assert_eq!(candidates[0]["roles"]["planned"], true);
    assert_eq!(candidates[1]["roles"]["planned"], false);
    assert_eq!(
        candidates[2]["eligibility"]["rejected"]["reason"],
        "policy rejected"
    );
    // The exact retained feature vector is what was recorded.
    assert_eq!(candidates[1]["features"]["values"][0], 0.80);
    assert_eq!(
        value["input"]["feature_dimension"],
        serde_json::json!(FEATURE_DIMENSION)
    );
}

// ---------------------------------------------------------------------------
// DecisionDistribution
// ---------------------------------------------------------------------------

fn outcomes() -> Vec<CandidateIdentity> {
    vec![
        identity("model-a", "provider-a"),
        identity("model-b", "provider-b"),
        identity("model-c", "provider-c"),
    ]
}

#[test]
fn a_valid_distribution_reports_its_shape_and_normalization() {
    let outcomes = outcomes();
    let distribution = DecisionDistribution::try_new(
        identity("model-a", "provider-a"),
        outcomes.clone(),
        vec![0.5, 0.25, 0.25],
    )
    .expect("a normalized distribution is accepted");

    assert_eq!(distribution.k(), 3);
    assert_eq!(distribution.outcomes(), outcomes.as_slice());
    assert_eq!(distribution.probabilities(), &[0.5, 0.25, 0.25]);
    assert!(distribution.subject() == &identity("model-a", "provider-a"));
    assert!((distribution.total_mass() - 1.0).abs() <= DISTRIBUTION_NORMALIZATION_TOLERANCE);
    assert_eq!(
        distribution.probability_of(&identity("model-b", "provider-b")),
        Some(0.25)
    );
    assert_eq!(
        distribution.probability_of(&identity("model-zz", "provider-zz")),
        None
    );
}

#[test]
fn a_distribution_of_the_wrong_arity_is_rejected() {
    let outcomes = outcomes();
    for probabilities in [
        vec![],
        vec![1.0],
        vec![0.5, 0.5],
        vec![0.25, 0.25, 0.25, 0.25],
    ] {
        let error = DecisionDistribution::try_new(
            identity("model-a", "provider-a"),
            outcomes.clone(),
            probabilities.clone(),
        )
        .expect_err("a wrong arity must be refused");
        assert!(
            matches!(
                error,
                DecisionContractError::DistributionArity {
                    outcomes: reported,
                    probabilities: reported_probabilities
                } if reported == 3 && reported_probabilities == probabilities.len()
            ),
            "arity {probabilities:?} produced {error:?}"
        );
    }
    // No outcomes at all is refused before arity is even considered.
    assert_eq!(
        DecisionDistribution::try_new(identity("model-a", "provider-a"), Vec::new(), Vec::new())
            .err(),
        Some(DecisionContractError::EmptyDistribution)
    );
}

#[test]
fn a_distribution_that_does_not_normalize_is_rejected() {
    for probabilities in [
        vec![0.5, 0.25, 0.1],        // short of 1
        vec![0.5, 0.25, 0.5],        // over 1
        vec![0.0, 0.0, 0.0],         // no mass at all
        vec![1.0 - 1e-6, 2e-6, 0.0], // drift above the tolerance
        vec![0.5, 0.5 + 1e-6, 0.0],  // drift above the tolerance
        vec![0.3, 0.3, 0.3],         // short of 1 by a wide margin
    ] {
        let error = DecisionDistribution::try_new(
            identity("model-a", "provider-a"),
            outcomes(),
            probabilities.clone(),
        )
        .expect_err("a distribution that does not normalize must be refused");
        assert!(
            matches!(
                error,
                DecisionContractError::DistributionNotNormalized {
                    total,
                    tolerance: DISTRIBUTION_NORMALIZATION_TOLERANCE
                } if (total - probabilities.iter().sum::<f64>()).abs() < f64::EPSILON
            ),
            "probabilities {probabilities:?} produced {error:?}"
        );
    }
    // Drift inside the tolerance is accepted, not rounded away.
    let drift = 0.5 - DISTRIBUTION_NORMALIZATION_TOLERANCE / 2.0;
    assert!(DecisionDistribution::try_new(
        identity("model-a", "provider-a"),
        outcomes(),
        vec![drift, drift, 1.0 - 2.0 * drift],
    )
    .is_ok());
}

#[test]
fn a_distribution_with_impossible_mass_is_rejected() {
    assert!(matches!(
        DecisionDistribution::try_new(
            identity("model-a", "provider-a"),
            outcomes(),
            vec![f64::NAN, 0.5, 0.5]
        )
        .err(),
        Some(DecisionContractError::NonFiniteProbability { index: 0, .. })
    ));
    assert!(matches!(
        DecisionDistribution::try_new(
            identity("model-a", "provider-a"),
            outcomes(),
            vec![0.5, f64::INFINITY, 0.5]
        )
        .err(),
        Some(DecisionContractError::NonFiniteProbability { index: 1, .. })
    ));
    let negative = DecisionDistribution::try_new(
        identity("model-a", "provider-a"),
        outcomes(),
        vec![0.5, -0.5, 1.0],
    )
    .expect_err("negative mass must be refused");
    match negative {
        DecisionContractError::NegativeProbability { index, value } => {
            assert_eq!(index, 1);
            assert!(value < 0.0, "unexpected reported value {value}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn a_distribution_over_a_malformed_outcome_axis_is_rejected() {
    // A duplicate outcome makes the axis ambiguous.
    let mut duplicated = outcomes();
    duplicated[2] = identity("model-a", "provider-a");
    assert!(matches!(
        DecisionDistribution::try_new(
            identity("model-a", "provider-a"),
            duplicated,
            vec![0.5, 0.25, 0.25]
        )
        .err(),
        Some(DecisionContractError::DuplicateDistributionOutcome(_))
    ));

    // An empty identity on the axis.
    let mut empty = outcomes();
    empty[1] = identity("", "provider-b");
    assert_eq!(
        DecisionDistribution::try_new(
            identity("model-a", "provider-a"),
            empty,
            vec![0.5, 0.25, 0.25]
        )
        .err(),
        Some(DecisionContractError::EmptyDistributionOutcome)
    );

    // A distribution about a decision that never considered its subject.
    assert!(matches!(
        DecisionDistribution::try_new(
            identity("model-zz", "provider-zz"),
            outcomes(),
            vec![0.5, 0.25, 0.25]
        )
        .err(),
        Some(DecisionContractError::DistributionSubjectNotAnOutcome(_))
    ));
}
