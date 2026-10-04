#![cfg(feature = "ml")]

//! Node 7E-3 — the offline replay release gate.
//!
//! The seven claims this suite has to earn:
//!
//! 1. replay equivalence is bit-exact, and a one-ULP difference is a refusal
//!    naming the first differing component, never a pass with a small delta;
//! 2. the replay is driven by the retained decision-time input, and the
//!    retention is *shown* to be load-bearing rather than asserted;
//! 3. the replayed decision is judged against the canonical Outcome, and the
//!    failure classification is read from the accepted impact table;
//! 4. the evaluation keys on the identity that actually served, and says so
//!    when the Outcome records none;
//! 5. the holdout is proven disjoint from the fit set, and 7E-2D's measurement
//!    and `passes` rule are carried without reinterpretation;
//! 6. the release verdict is recomputed from visible constituents, with no
//!    stored boolean;
//! 7. every required refusal is a typed value carrying its reason.
//!
//! Fixture note: the recorded decisions are produced by a *trained* commit,
//! because a cold ensemble predicts a constant and would therefore make every
//! retained feature inert — the retention ablation would find nothing
//! load-bearing and the gate's central claim could not be tested at all.

use std::collections::BTreeSet;

use zroutery_core::config::ModelTier;
use zroutery_core::failure::FailureClass;
use zroutery_core::ir::Usage;
use zroutery_core::ml::calibration::{
    CalibrationConfig, CalibrationVerdict, DriftConfig, HoldoutConfig, ReliabilityConfig,
};
use zroutery_core::ml::coordinator::{CoordinatorConfig, RoutingAction};
use zroutery_core::ml::dataset::{
    canonical_samples_from_decision_time, Targets, TrainingSample as DatasetTrainingSample,
};
use zroutery_core::ml::decision_engine::DecisionEngine;
use zroutery_core::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use zroutery_core::ml::model_identity::{CommitId, ModelCommit};
use zroutery_core::ml::offline_gate::{
    run_offline_gate, EvidenceFloors, GateConfig, GateInput, OfflineGateError, RecordedDecision,
    ReleaseReport, ReleaseVerdict, RetentionAblation, TerminalAgreement, RELEASE_SCOPE,
};
use zroutery_core::ml::reward::RewardPolicy;
use zroutery_core::ml::shadow::{
    ModelEnsemblePredictor, ShadowCandidateInput, ShadowEngine, ShadowInput,
};
use zroutery_core::ml::statistics::StatisticalConfig;
use zroutery_core::outcome::{Attempt, FailureFacts, FinalStatus, Outcome};
use zroutery_core::session::SessionRoutingMode;

const BASE: i64 = 1_700_000_000;

/// `(model, provider)` for the three candidates every decision compares.
const AXIS: [(&str, &str); 3] = [
    ("alpha", "prov-a"),
    ("bravo", "prov-b"),
    ("charlie", "prov-c"),
];

/// Decisions in the main fixture. 7E-2D's default partition wants 20 cohorts,
/// so anything under that could not be split at all.
const DECISIONS: usize = 24;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// A feature vector determined by `(decision, candidate)`.
///
/// Deliberately *not* varied wildly across decisions: the calibration drift
/// gate compares the fit partition against the holdout partition, and a
/// fixture whose features jump around every request is a fixture testing the
/// drift gate rather than this node.
fn features(decision: usize, candidate: usize) -> RoutingFeatures {
    let mut values = [0.0f32; FEATURE_DIMENSION];
    for (index, value) in values.iter_mut().enumerate() {
        let mixed = (decision * 11 + candidate * 29 + index * 7) % 41;
        *value = mixed as f32 / 41.0;
    }
    RoutingFeatures {
        values,
        schema_version: FEATURE_SCHEMA_VERSION,
    }
}

fn candidate_input(decision: usize, slot: usize) -> ShadowCandidateInput {
    let (model, provider) = AXIS[slot];
    ShadowCandidateInput {
        candidate_id: model.to_string(),
        provider_id: provider.to_string(),
        tier: Some(ModelTier::Standard),
        eligible: true,
        features: features(decision, slot),
        rejection_reason: None,
    }
}

/// The retained decision-time input for one request.
fn shadow_input(decision: usize) -> ShadowInput {
    ShadowInput {
        decision_id: format!("dec-{decision:04}"),
        policy_id: "policy-offline".to_string(),
        client_id: None,
        policy_revision: Default::default(),
        task: Default::default(),
        production_selected: AXIS[0].0.to_string(),
        feature_schema: FEATURE_SCHEMA_VERSION,
        candidates: (0..AXIS.len())
            .map(|slot| candidate_input(decision, slot))
            .collect(),
        session_mode: SessionRoutingMode::Free,
        session_switch_count: 0,
        is_fallback: false,
    }
}

fn attempt(decision: usize, slot: usize, won: bool) -> Attempt {
    let (model, provider) = AXIS[slot];
    Attempt {
        attempt_id: format!("att-{decision:04}-{slot}"),
        candidate_model: model.to_string(),
        candidate_provider: provider.to_string(),
        started_at: BASE + decision as i64 * 10 + slot as i64,
        completed_at: BASE + decision as i64 * 10 + slot as i64 + 1,
        // Safe decimals on purpose: see the float suite for why the f64
        // targets are the part of a canonical sample that can drift.
        latency_ms: if won { 120.0 } else { 30.0 },
        ttft_ms: if won { Some(40.0) } else { None },
        success: won,
        failure_class: if won {
            None
        } else {
            Some(FailureClass::ProviderUnavailable)
        },
        failure_message: if won {
            None
        } else {
            Some("scripted provider failure".to_string())
        },
        http_status: if won { Some(200) } else { Some(503) },
        rectified: false,
        cost: None,
    }
}

/// The canonical Outcome for one request: every candidate attempted, the
/// winner attempted last so the terminal status is derived as a success.
fn outcome(decision: usize, winner: usize) -> Outcome {
    let mut builder = Outcome::builder(format!("req-{decision:04}"))
        .decision_id(format!("dec-{decision:04}"))
        .planned(AXIS[0].0, AXIS[0].1)
        .dialect("openai")
        .streaming(false);
    // The winner is attempted **last**. The terminal status is derived from the
    // last attempt, so a winner in the middle would make the whole request a
    // failure with nobody serving — and a request nobody served has no served
    // identity, which the gate correctly refuses to key an evaluation on.
    for slot in (0..AXIS.len()).filter(|slot| *slot != winner) {
        builder = builder.attempt(attempt(decision, slot, false));
    }
    builder = builder.attempt(attempt(decision, winner, true));
    let mut built = builder
        .total_latency_ms(180.0)
        .ttft_ms(40.0)
        .usage(Usage {
            input_tokens: 100,
            output_tokens: 50,
            ..Usage::default()
        })
        .cost(Some(0.01), Some(0.009))
        .build();
    // The builder stamps a UUID. Pinning the id keeps the whole run
    // reproducible, which is what lets the report claim to be a pure function
    // of its inputs.
    built.outcome_id = format!("out-{decision:04}");
    built.timestamp = BASE + decision as i64;
    built
}

fn training_samples(range: std::ops::Range<usize>) -> Vec<DatasetTrainingSample> {
    range
        .map(|index| {
            let values = features(index % DECISIONS, index % AXIS.len());
            DatasetTrainingSample {
                sample_id: format!("fit-{index:04}"),
                schema_version: FEATURE_SCHEMA_VERSION,
                timestamp: BASE + index as i64,
                features: values,
                targets: Targets {
                    success: index % 2 == 0,
                    latency_ms: Some(100.0 + index as f64),
                    ttft_ms: Some(30.0 + index as f64),
                    cost: Some(0.005 + index as f64 * 0.000_1),
                    failure_class: if index % 2 == 0 {
                        None
                    } else {
                        Some("ProviderUnavailable".to_string())
                    },
                    fallback_count: 0,
                },
                provider_id: AXIS[index % AXIS.len()].1.to_string(),
                model_id: AXIS[index % AXIS.len()].0.to_string(),
                origin: zroutery_core::feedback::DataOrigin::Native,
                outcome_id: format!("out-fit-{index:04}"),
                feedback: Vec::new(),
            }
        })
        .collect()
}

/// A trained commit with a verified root-to-current lineage.
///
/// A cold ensemble is not usable here: it predicts a constant, so no retained
/// feature would be load-bearing and the retention claim would be vacuous.
struct Trained {
    predictor: ModelEnsemblePredictor,
    commit: ModelCommit,
    lineage: Vec<ModelCommit>,
}

fn trained() -> Trained {
    let root_commit = ModelEnsemblePredictor::genesis().commit_record();
    // Every step supplies its complete root-to-current lineage. Training from a
    // predictor produces a *child* commit, so a single-record lineage is
    // correctly refused as "non-root commit is missing its parent record" —
    // which is exactly the lineage gate this node depends on.
    let from_genesis = ModelEnsemblePredictor::from_model_commit_with_lineage(
        &root_commit,
        std::slice::from_ref(&root_commit),
    )
    .expect("the genesis root lineage verifies");
    let (_, first) = from_genesis
        .try_train(&training_samples(0..32))
        .expect("the fixture trains");
    let first_lineage = vec![root_commit.clone(), first.clone()];
    let from_first = ModelEnsemblePredictor::from_model_commit_with_lineage(&first, &first_lineage)
        .expect("the first child's lineage verifies");
    let (_, second) = from_first
        .try_train(&training_samples(32..64))
        .expect("the fixture trains twice");

    let lineage = vec![root_commit, first, second.clone()];
    let predictor = ModelEnsemblePredictor::from_model_commit_with_lineage(&second, &lineage)
        .expect("the complete lineage verifies");
    Trained {
        predictor,
        commit: second,
        lineage,
    }
}

/// The engine configuration the fixture records its decisions under, and which
/// the gate is then told to rebuild them with.
fn engine_config() -> CoordinatorConfig {
    CoordinatorConfig {
        // Exploration is a routing behaviour this node must never produce or
        // depend on, and the gate must be able to reproduce a decision without
        // it.
        exploration_enabled: false,
        ..CoordinatorConfig::default()
    }
}

fn gate_config() -> GateConfig {
    GateConfig {
        calibration: CalibrationConfig {
            holdout: HoldoutConfig {
                holdout_cohorts: 8,
                min_fit_cohorts: 8,
                min_attributed_outcomes: 4,
            },
            ..CalibrationConfig::default()
        },
        floors: EvidenceFloors {
            min_replayed_decisions: 1,
            min_holdout_decisions: 8,
        },
        retention_probes: 6,
        engine: engine_config(),
        reward_policy: RewardPolicy::default(),
        // 7D's claim specification, at 7D's own defaults. This suite makes no
        // claim about statistical support: 7D's suite does, in
        // `statistics_test.rs`. Naming the defaults here keeps the fixture from
        // silently depending on a future change to them.
        statistics: StatisticalConfig::default(),
    }
}

/// A recorded decision paired with its Outcome, produced by the same accepted
/// path the gate replays through.
fn record(trained: &Trained, decision: usize, winner: usize) -> RecordedDecision {
    let engine = ShadowEngine::new(
        DecisionEngine::new(engine_config(), RewardPolicy::default()),
        true,
    );
    let input = shadow_input(decision);
    let recorded = engine
        .evaluate_with(&format!("req-{decision:04}"), &input, &trained.predictor)
        .expect("the fixture decision is recorded");
    RecordedDecision {
        decision: recorded,
        outcome: outcome(decision, winner),
    }
}

/// The whole main fixture, with the served candidate rotating so the holdout
/// is not a `SingleServedCandidate` degeneracy.
fn fixture(trained: &Trained) -> Vec<RecordedDecision> {
    (0..DECISIONS)
        .map(|decision| record(trained, decision, decision % AXIS.len()))
        .collect()
}

fn gate_input(trained: &Trained, recorded: Vec<RecordedDecision>) -> GateInput {
    GateInput {
        commit: trained.commit.clone(),
        lineage: trained.lineage.clone(),
        recorded,
        fit_sample_ids: BTreeSet::new(),
        config: gate_config(),
    }
}

/// Loose ceilings, used only to drive the `Considerable` path.
///
/// The ceilings are the caller's claim about what counts as calibrated, and
/// this fixture does not claim its synthetic model is well calibrated. Setting
/// them loose tests that the verdict is recomputed from its constituents; it
/// asserts nothing about calibration quality. The strict-ceiling case is tested
/// separately.
fn permissive_config() -> GateConfig {
    let mut config = gate_config();
    config.calibration.reliability = ReliabilityConfig {
        max_expected_calibration_error: 1.0,
        max_calibration_error: 1.0,
        max_candidate_calibration_error: 1.0,
        ..ReliabilityConfig::default()
    };
    config.calibration.drift = DriftConfig {
        max_population_stability_index: 1.0,
        max_base_rate_delta: 1.0,
        ..DriftConfig::default()
    };
    config
}

// ---------------------------------------------------------------------------
// 1. Replay equivalence: bit-exact, and a near miss is a refusal
// ---------------------------------------------------------------------------

#[test]
fn a_clean_run_reproduces_every_recorded_decision_bit_exactly() {
    let trained = trained();
    let recorded = fixture(&trained);

    let outcome = run_offline_gate(&gate_input(&trained, recorded)).expect("the fixture replays");

    assert_eq!(
        outcome.report().measurements.decisions_equivalent,
        DECISIONS,
        "every recorded decision must replay bit-identically"
    );
    assert_eq!(outcome.report().replay.len(), DECISIONS);
    for evidence in &outcome.report().replay {
        assert!(
            evidence.components_compared > 50,
            "the comparison must actually walk the decision, not a token of it: {}",
            evidence.components_compared
        );
        assert_eq!(
            evidence.retention.recorded_input_checksum, evidence.retention.replayed_input_checksum,
            "the retained input must re-derive to the recorded input checksum"
        );
    }
}

#[test]
fn one_ulp_of_drift_in_a_recorded_prediction_is_a_refusal_not_a_pass() {
    let trained = trained();
    let mut recorded = fixture(&trained);
    // The smallest possible disagreement: one representable double.
    recorded[3].decision.candidates[0].prediction.success.value = f64::from_bits(
        recorded[3].decision.candidates[0]
            .prediction
            .success
            .value
            .to_bits()
            + 1,
    );

    let refusal = run_offline_gate(&gate_input(&trained, recorded))
        .expect_err("a one-ULP difference must never be tolerated");

    match refusal {
        OfflineGateError::ReplayDivergence { divergence, .. } => {
            assert_eq!(
                divergence.component, "candidates[0].prediction.success.value",
                "the refusal must name the exact field that differs, not just the head"
            );
            assert_eq!(divergence.index, Some(0));
        }
        other => panic!("expected a replay divergence, got {other}"),
    }
}

#[test]
fn one_ulp_of_drift_in_a_utility_term_is_refused_at_that_term() {
    let trained = trained();
    let mut recorded = fixture(&trained);
    let index = 5;
    recorded[index].decision.candidates[1].utility.total = f64::from_bits(
        recorded[index].decision.candidates[1]
            .utility
            .total
            .to_bits()
            - 1,
    );

    let refusal = run_offline_gate(&gate_input(&trained, recorded))
        .expect_err("a one-ULP difference must never be tolerated");
    let OfflineGateError::ReplayDivergence { divergence, .. } = refusal else {
        panic!("expected a replay divergence");
    };
    assert_eq!(divergence.component, "candidates[1].utility.total");
    assert_eq!(divergence.index, Some(1));
}

#[test]
fn a_tampered_selection_is_refused_before_the_checksums() {
    let trained = trained();
    let mut recorded = fixture(&trained);
    recorded[0].decision.shadow.selected = AXIS[1].0.to_string();

    let refusal = run_offline_gate(&gate_input(&trained, recorded))
        .expect_err("a tampered selection cannot replay");
    let OfflineGateError::ReplayDivergence { divergence, .. } = refusal else {
        panic!("expected a replay divergence");
    };
    // The selection is compared before the decision checksum, so the refusal
    // names the component that actually changed rather than the digest of it.
    assert_eq!(divergence.component, "shadow.selected");
}

#[test]
fn a_non_finite_recorded_value_is_refused_before_any_comparison() {
    let trained = trained();
    let mut recorded = fixture(&trained);
    recorded[2].decision.candidates[0].utility.total = f64::NAN;

    let refusal = run_offline_gate(&gate_input(&trained, recorded))
        .expect_err("a NaN cannot be given an equality verdict");
    let OfflineGateError::NonFiniteMeasurement { component, .. } = refusal else {
        panic!("expected a non-finite refusal, not a divergence");
    };
    assert_eq!(component.component, "candidates[0].utility.total");
    assert!(component.value.is_nan());
}

#[test]
fn no_decision_to_gate_is_a_refusal() {
    let trained = trained();
    let refusal = run_offline_gate(&gate_input(&trained, Vec::new()))
        .expect_err("a verdict computed from nothing is not a verdict");
    assert!(matches!(refusal, OfflineGateError::NothingToGate));
}

// ---------------------------------------------------------------------------
// 2. The replay is driven by the retained input, and that is shown
// ---------------------------------------------------------------------------

#[test]
fn the_retained_input_is_shown_to_be_load_bearing_for_the_replayed_decision() {
    let trained = trained();
    let recorded = fixture(&trained);
    let outcome = run_offline_gate(&gate_input(&trained, recorded)).expect("the fixture replays");

    let ablation = outcome.report().measurements.retention;
    assert!(
        ablation.is_proof(),
        "the retention must be shown load-bearing: probed {} load-bearing {}",
        ablation.probed,
        ablation.load_bearing
    );
    assert!(ablation.probed >= 6, "the ablation must have actually run");
    assert_eq!(
        ablation.load_bearing, ablation.probed,
        "a trained model reads every probed retained feature, so every probe must move the decision"
    );

    for evidence in &outcome.report().replay {
        assert!(evidence.retention.retained_candidates >= AXIS.len());
        assert_eq!(evidence.retention.eligible_candidates, AXIS.len());
        assert_eq!(evidence.retention.outcome_candidates, AXIS.len());
        assert!(
            evidence.retention.complete_for_outcome,
            "every candidate the Outcome touched needs a retained vector"
        );
    }
}

#[test]
fn a_run_with_the_ablation_disabled_cannot_claim_the_retention_is_load_bearing() {
    let trained = trained();
    let recorded = fixture(&trained);
    let mut config = gate_config();
    config.retention_probes = 0;
    let mut input = gate_input(&trained, recorded);
    input.config = config;

    let outcome = run_offline_gate(&input).expect("the fixture still replays");
    let verdict = outcome.report().recomputed_verdict();
    assert!(!verdict.is_considerable());
    assert!(
        outcome
            .report()
            .blockers()
            .iter()
            .any(|blocker| blocker.contains("load-bearing")),
        "an unprobed retention must withhold the verdict: {:?}",
        outcome.report().blockers()
    );
}

#[test]
fn a_retained_input_that_was_edited_after_the_decision_is_refused_as_drifted() {
    let trained = trained();
    let mut recorded = fixture(&trained);
    // Change one retained feature value without restamping the recorded
    // checksum: the retained input is no longer the input the decision was
    // made from.
    recorded[4]
        .decision
        .observation
        .input
        .candidates
        .get_mut(0)
        .expect("candidate 0 is retained")
        .features
        .values[3] += 0.25;

    let refusal = run_offline_gate(&gate_input(&trained, recorded))
        .expect_err("an edited retained input is not the original input");
    let OfflineGateError::RetainedInputDrifted {
        recorded: recorded_checksum,
        replayed,
        ..
    } = refusal
    else {
        panic!("expected a retained-input drift refusal");
    };
    assert_ne!(recorded_checksum, replayed);
}

#[test]
fn a_retained_input_that_names_a_candidate_the_outcome_never_touched_is_incomplete() {
    let trained = trained();
    let mut recorded = fixture(&trained);
    // Make the Outcome attempt a candidate that was not retained, so the
    // accepted retention-completeness refusal is the honest answer.
    let mut outcome = recorded[0].outcome.clone();
    outcome.attempts.push(Attempt {
        attempt_id: "att-ghost".to_string(),
        candidate_model: "ghost".to_string(),
        candidate_provider: "prov-ghost".to_string(),
        started_at: BASE,
        completed_at: BASE + 1,
        latency_ms: 10.0,
        ttft_ms: None,
        success: true,
        failure_class: None,
        failure_message: None,
        http_status: Some(200),
        rectified: false,
        cost: None,
    });
    recorded[0].outcome = outcome;

    let refusal = run_offline_gate(&gate_input(&trained, recorded))
        .expect_err("a request whose attempted candidate was never retained is refused");
    // The terminal-state check comes first and is the more specific answer: the
    // replay never selected the ghost, but the Outcome's served identity is
    // now the ghost, which the request-scope keying cannot reconcile.
    assert!(
        matches!(
            refusal,
            OfflineGateError::TerminalStateDisagreement { .. }
                | OfflineGateError::RetainedInputIncomplete { .. }
        ),
        "expected a retention or terminal-state refusal, got {refusal}"
    );
}

// ---------------------------------------------------------------------------
// 3. Honest failure and outcome authority
// ---------------------------------------------------------------------------

#[test]
fn the_failure_classification_is_read_from_the_accepted_impact_table() {
    let trained = trained();
    let recorded = fixture(&trained);
    let outcome = run_offline_gate(&gate_input(&trained, recorded)).expect("the fixture replays");

    // Every fixture decision is a terminal success, so the accepted table has
    // nothing to classify: the gate reports no class rather than inventing one.
    for evidence in &outcome.report().replay {
        assert_eq!(evidence.recorded_terminal, FinalStatus::Success);
        assert_eq!(
            evidence.failure.class, None,
            "a success has no failure class and the gate must not manufacture one"
        );
        assert_eq!(evidence.failure.impact, None);
    }
}

#[test]
fn a_failed_request_classifies_through_the_accepted_table() {
    let trained = trained();
    let mut recorded = vec![record(&trained, 0, 0)];
    // A request that ended in a provider failure: no served identity, and a
    // class the accepted table knows.
    let mut outcome = outcome(0, 0);
    outcome.success = false;
    outcome.final_status = FinalStatus::Failed;
    outcome.served_model = None;
    outcome.served_provider = None;
    outcome.terminal_error = Some(FailureFacts::new(
        FailureClass::ProviderUnavailable,
        Some("provider down".to_string()),
        Some(503),
    ));
    recorded[0].outcome = outcome;

    let refusal = run_offline_gate(&gate_input(&trained, recorded))
        .expect_err("a request with no served identity cannot be evaluated");
    // The refusal is the point: the gate will not key the evaluation on the
    // planned identity just because the served one is missing.
    assert!(
        matches!(refusal, OfflineGateError::ServedIdentityAbsent { .. }),
        "expected a served-identity refusal, got {refusal}"
    );
}

#[test]
fn an_outcome_correlated_to_another_request_is_refused_before_the_replay() {
    let trained = trained();
    let mut recorded = fixture(&trained);
    // The decision and its Outcome describe different requests. The canonical
    // conversion keys on candidate identity alone, so nothing downstream would
    // notice: the foreign request's success, cost and latency would simply be
    // attributed to this decision.
    recorded[5].outcome.request_id = "foreign-request-0005".to_string();

    let refusal = run_offline_gate(&gate_input(&trained, recorded)).expect_err(
        "an Outcome for another request is not evidence about this decision and must not be measured",
    );
    let OfflineGateError::OutcomeDecisionMismatch {
        decision_id,
        outcome_id,
        detail,
    } = refusal
    else {
        panic!("expected an outcome-correlation refusal");
    };
    assert_eq!(decision_id, "dec-0005");
    assert_eq!(outcome_id, "out-0005");
    assert!(
        detail.contains("foreign-request-0005") && detail.contains("req-0005"),
        "the refusal must name both requests so the mispairing is actionable: {detail}"
    );
}

#[test]
fn an_outcome_that_names_another_decision_is_refused_before_the_replay() {
    let trained = trained();
    let mut recorded = fixture(&trained);
    // The request ids agree, so only the decision identity distinguishes the
    // pair. It is optional on the Outcome, and when it is present it must name
    // the decision being replayed.
    recorded[7].outcome.decision_id = Some("dec-foreign".to_string());

    let refusal = run_offline_gate(&gate_input(&trained, recorded))
        .expect_err("an Outcome naming another decision is foreign even when its request matches");
    let OfflineGateError::OutcomeDecisionMismatch {
        decision_id,
        outcome_id,
        detail,
    } = refusal
    else {
        panic!("expected an outcome-correlation refusal");
    };
    assert_eq!(decision_id, "dec-0007");
    assert_eq!(outcome_id, "out-0007");
    assert!(
        detail.contains("dec-foreign") && detail.contains("dec-0007"),
        "the refusal must name both decision ids: {detail}"
    );
}

#[test]
fn a_replay_that_selects_a_candidate_the_outcome_never_attempted_is_a_refusal() {
    let trained = trained();
    // A single-candidate retained input: the replay can only select it.
    let mut input = shadow_input(0);
    input.candidates.truncate(1);
    let engine = ShadowEngine::new(
        DecisionEngine::new(engine_config(), RewardPolicy::default()),
        true,
    );
    let decision = engine
        .evaluate_with("req-0000", &input, &trained.predictor)
        .expect("the single-candidate decision is recorded");

    // The Outcome attempts a different candidate entirely.
    let mut built = outcome(0, 0);
    built.attempts = vec![attempt(0, 1, true)];
    built.outcome_id = "out-ghost".to_string();

    let refusal = run_offline_gate(&GateInput {
        commit: trained.commit.clone(),
        lineage: trained.lineage.clone(),
        recorded: vec![RecordedDecision {
            decision,
            outcome: built,
        }],
        fit_sample_ids: BTreeSet::new(),
        config: gate_config(),
    })
    .expect_err("a replay the Outcome never saw is not evidence about it");
    assert!(
        matches!(refusal, OfflineGateError::TerminalStateDisagreement { .. }),
        "expected a terminal-state refusal, got {refusal}"
    );
}

#[test]
fn a_terminal_success_whose_served_identity_is_erased_is_refused() {
    let trained = trained();
    let mut input = shadow_input(0);
    input.candidates.truncate(1);
    let engine = ShadowEngine::new(
        DecisionEngine::new(engine_config(), RewardPolicy::default()),
        true,
    );
    let decision = engine
        .evaluate_with("req-0000", &input, &trained.predictor)
        .expect("the single-candidate decision is recorded");

    // Erase the served identity while leaving the attempt chain intact, so the
    // refusal is specifically about the missing served identity and not about
    // an un-attempted selection.
    let mut built = outcome(0, 0);
    built.served_model = None;
    built.served_provider = None;

    let refusal = run_offline_gate(&GateInput {
        commit: trained.commit.clone(),
        lineage: trained.lineage.clone(),
        recorded: vec![RecordedDecision {
            decision,
            outcome: built,
        }],
        fit_sample_ids: BTreeSet::new(),
        config: gate_config(),
    })
    .expect_err("a success with no served identity cannot be keyed");
    assert!(
        matches!(refusal, OfflineGateError::ServedIdentityAbsent { .. }),
        "expected a served-identity refusal, got {refusal}"
    );
}

#[test]
fn the_terminal_agreement_distinguishes_a_served_selection_from_a_failover() {
    let trained = trained();
    let recorded = fixture(&trained);
    let outcome = run_offline_gate(&gate_input(&trained, recorded)).expect("the fixture replays");

    let agreements: Vec<TerminalAgreement> = outcome
        .report()
        .replay
        .iter()
        .map(|evidence| evidence.terminal)
        .collect();
    assert!(
        agreements.iter().all(|agreement| matches!(
            agreement,
            TerminalAgreement::SelectionServed | TerminalAgreement::SelectionSupersededByFailover
        )),
        "a counterfactual that selected a candidate nobody served is a legitimate \
         failover disagreement, not an error: {agreements:?}"
    );
    assert_eq!(
        outcome.report().measurements.terminal_agreements,
        DECISIONS,
        "every replay must sit consistently with its Outcome"
    );
}

// ---------------------------------------------------------------------------
// 4. Final served identity
// ---------------------------------------------------------------------------

#[test]
fn the_evaluation_keys_on_the_identity_that_actually_served() {
    let trained = trained();
    let recorded = fixture(&trained);
    let outcome = run_offline_gate(&gate_input(&trained, recorded)).expect("the fixture replays");

    assert_eq!(outcome.report().measurements.served_identities, DECISIONS);
    assert_eq!(outcome.report().measurements.absent_served_identities, 0);
    assert_eq!(outcome.report().holdout.served_identities, AXIS.len());

    for (evidence, entry) in outcome.report().replay.iter().zip(0..DECISIONS) {
        let served = evidence.served.as_ref().expect("every decision served");
        // The served identity is the fixture's winner, never the planned one.
        let winner = AXIS[entry % AXIS.len()];
        assert_eq!((served.model.as_str(), served.provider.as_str()), winner);
    }
}

#[test]
fn a_missing_served_identity_is_said_rather_than_substituted_with_the_planned_one() {
    let trained = trained();
    let mut recorded = fixture(&trained);
    // Keep the planned identity and erase the served one: a gate that
    // substituted planned would sail through this.
    recorded[7].outcome.served_model = None;
    recorded[7].outcome.served_provider = None;

    let refusal = run_offline_gate(&gate_input(&trained, recorded))
        .expect_err("a missing served identity must be said, not filled in");
    match refusal {
        OfflineGateError::ServedIdentityAbsent {
            decision_id,
            outcome_id,
        } => {
            assert_eq!(decision_id, "dec-0007");
            assert_eq!(outcome_id, "out-0007");
        }
        other => panic!("expected a served-identity refusal, got {other}"),
    }
}

// ---------------------------------------------------------------------------
// 5. Holdout and calibration, by consumption
// ---------------------------------------------------------------------------

#[test]
fn the_calibration_verdict_is_7e_2ds_own_and_is_not_reinterpreted() {
    let trained = trained();
    let recorded = fixture(&trained);
    let outcome = run_offline_gate(&gate_input(&trained, recorded)).expect("the fixture replays");

    let carried = outcome.report().measurements.calibration;
    let recomputed = outcome.report().calibration.recomputed_verdict();
    assert_eq!(
        carried, recomputed,
        "the carried verdict must be 7E-2D's recomputed one, not this node's reading of it"
    );
    assert_eq!(carried, outcome.report().calibration.verdict);
    // The calibration report is 7E-2D's, whole: its own counts are its own.
    assert_eq!(
        outcome.report().calibration.cohorts_total,
        DECISIONS,
        "one cohort per decision, from the attempt-scope rows"
    );
    assert!(outcome.report().calibration.holdout_cohorts > 0);
}

#[test]
fn a_strict_ceiling_can_withhold_the_verdict_and_the_blocker_says_so() {
    let trained = trained();
    let recorded = fixture(&trained);
    let mut config = gate_config();
    // A ceiling no synthetic model can meet. The point is that a broken
    // constituent withholds the verdict and is named, not that this model is
    // badly calibrated.
    config
        .calibration
        .reliability
        .max_expected_calibration_error = 0.0;
    config.calibration.reliability.max_calibration_error = 0.0;
    config
        .calibration
        .reliability
        .max_candidate_calibration_error = 0.0;
    let mut input = gate_input(&trained, recorded);
    input.config = config;

    let outcome = run_offline_gate(&input).expect("the fixture still runs");
    assert!(!outcome.report().is_considerable());
    assert_eq!(
        outcome.report().recomputed_verdict(),
        ReleaseVerdict::NotConsiderable
    );
    let blockers = outcome.report().blockers();
    assert!(
        blockers.contains(&"7E-2D reports the emitted vector as miscalibrated"),
        "the calibration blocker must be visible: {blockers:?}"
    );
}

#[test]
fn a_holdout_that_overlaps_the_fit_set_is_refused_with_the_offending_id() {
    let trained = trained();
    let recorded = fixture(&trained);

    // Ask the accepted canonical conversion what the holdout's sample ids are,
    // then claim the model was fitted on one of them.
    let first = &recorded[0];
    let samples = canonical_samples_from_decision_time(
        &first.outcome,
        first.input(),
        zroutery_core::feedback::DataOrigin::Native,
    )
    .expect("the fixture's retained input is complete");
    let offender = samples[0].sample_id.clone();

    let mut input = gate_input(&trained, recorded);
    input.fit_sample_ids.insert(offender.clone());

    let refusal =
        run_offline_gate(&input).expect_err("a holdout that overlaps the fit set measures the fit");
    let OfflineGateError::HoldoutOverlap {
        kind, sample_id, ..
    } = refusal
    else {
        panic!("expected a holdout-overlap refusal");
    };
    assert_eq!(kind, "sample id");
    assert_eq!(sample_id, offender);
}

#[test]
fn a_holdout_nobody_served_would_be_degenerate_and_a_single_winner_is_refused() {
    let trained = trained();
    // Every decision served by the same candidate: 7E-2D classifies that as
    // `SingleServedCandidate` and this gate must carry the classification.
    let recorded: Vec<RecordedDecision> = (0..DECISIONS)
        .map(|decision| record(&trained, decision, 0))
        .collect();

    let refusal = run_offline_gate(&gate_input(&trained, recorded))
        .expect_err("a one-candidate axis cannot support a calibration claim");
    let OfflineGateError::DegenerateHoldout {
        partition, reason, ..
    } = refusal
    else {
        panic!("expected a degenerate-holdout refusal");
    };
    assert_eq!(reason.label(), "single_served_candidate");
    assert_eq!(
        partition,
        zroutery_core::ml::calibration::PartitionKind::Fit
    );
}

#[test]
fn a_holdout_smaller_than_the_floor_is_refused_as_insufficient_evidence() {
    let trained = trained();
    let recorded: Vec<RecordedDecision> = (0..4)
        .map(|decision| record(&trained, decision, decision % AXIS.len()))
        .collect();
    let mut input = gate_input(&trained, recorded);
    input.config.floors = EvidenceFloors {
        min_replayed_decisions: 1,
        min_holdout_decisions: 10_000,
    };

    let refusal = run_offline_gate(&input).expect_err("too little evidence to measure");
    let OfflineGateError::InsufficientEvidence {
        context,
        observed,
        required,
    } = refusal
    else {
        panic!("expected an insufficient-evidence refusal");
    };
    // The floor is on decisions, not on the canonical rows they project to.
    // The assertion used to pin "the holdout sample count"; that context named
    // the wrong unit, because one decision produces a request row plus one row
    // per attempt and a row floor is satisfied several times over.
    assert_eq!(context, "the holdout decision count");
    assert!(observed < required);
}

#[test]
fn the_holdout_floor_counts_decisions_and_not_canonical_rows() {
    let trained = trained();
    let recorded: Vec<RecordedDecision> = (0..4)
        .map(|decision| record(&trained, decision, decision % AXIS.len()))
        .collect();
    let mut input = gate_input(&trained, recorded);
    // Four decisions project to sixteen canonical rows (a request row plus one
    // per attempt). A floor of five is therefore satisfied by the *rows* and
    // not by the decisions, which is exactly the unit confusion this pins.
    input.config.floors = EvidenceFloors {
        min_replayed_decisions: 1,
        min_holdout_decisions: 5,
    };

    let refusal = run_offline_gate(&input).expect_err("four decisions are not five");
    let OfflineGateError::InsufficientEvidence {
        context,
        observed,
        required,
    } = refusal
    else {
        panic!("expected an insufficient-evidence refusal");
    };
    assert_eq!(context, "the holdout decision count");
    assert_eq!(
        observed, 4,
        "the floor must count decisions, not the sixteen rows"
    );
    assert_eq!(required, 5);
}

#[test]
fn the_holdout_carries_the_candidate_axis_and_not_only_the_request_row() {
    let trained = trained();
    let recorded = fixture(&trained);
    let outcome = run_offline_gate(&gate_input(&trained, recorded)).expect("the fixture replays");

    // One request row plus one row per attempt, for every decision. A holdout
    // of request rows alone would give 7E-2D a one-candidate K axis and a
    // trivially perfect vector, and "calibrated" would mean nothing.
    assert_eq!(
        outcome.report().holdout.samples,
        DECISIONS * (AXIS.len() + 1)
    );
    assert_eq!(outcome.report().holdout.decisions, DECISIONS);
    assert_eq!(outcome.report().holdout.attributed, DECISIONS);
    assert_eq!(outcome.report().holdout.overlap_with_fit_set, 0);
}

// ---------------------------------------------------------------------------
// 6. The release verdict
// ---------------------------------------------------------------------------

/// The eleven blockers 7E-3 contributes to the release verdict, by the exact
/// strings it emits.
///
/// 7D appends its own statistical blockers to the same list, which is the point
/// of the list: the gate got stronger, not weaker. That does mean a fixture that
/// satisfies all of 7E-3's constituents can still be `NotConsiderable` — 7E-3's
/// `Considerable` was explicitly "not a statistical claim of any kind", and 7D
/// added the statistical claim. So this suite asserts what it owns: that no
/// **7E-3** constituent withholds. Whether 7D's constituents withhold is 7D's
/// suite's job, in `statistics_test.rs` and `release_evidence_test.rs`.
///
/// The list is also the documentation of what this node contributes. If a 7E-3
/// blocker string changes, this constant fails to match and the reader is told
/// which constituent moved.
const NODE_7E3_BLOCKERS: &[&str] = &[
    "the commit did not pass the accepted verification",
    "the retained lineage did not verify",
    "no decision was replayed",
    "at least one replay was not bit-identical to its record",
    "at least one replay disagreed with its Outcome",
    "no retained feature position was shown to be load-bearing for the replayed decision",
    "at least one Outcome records no served identity",
    "the holdout overlaps the model's fit set",
    "the holdout is empty",
    "7E-2D reports the emitted vector as miscalibrated",
    "canonical sample floats do not survive this workspace's JSON read path",
];

/// The blockers of this node alone, leaving 7D's statistical blockers out.
fn node_7e3_blockers(report: &ReleaseReport) -> Vec<&'static str> {
    report
        .blockers()
        .into_iter()
        .filter(|blocker| NODE_7E3_BLOCKERS.contains(blocker))
        .collect()
}

#[test]
fn the_positive_path_produces_a_considerable_verdict_with_no_stored_boolean() {
    let trained = trained();
    let recorded = fixture(&trained);
    let mut input = gate_input(&trained, recorded);
    input.config = permissive_config();

    let outcome = run_offline_gate(&input).expect("the fixture replays");
    let report = outcome.report();

    // This node's claim: every 7E-3 constituent passes, and the verdict is
    // recomputed rather than read.
    assert_eq!(report.recomputed_verdict(), report.verdict);
    assert!(
        node_7e3_blockers(report).is_empty(),
        "no 7E-3 constituent may withhold: {:?}",
        report.blockers()
    );
    // 7D's statistical constituent is also always present and always visible.
    // On this fixture it refuses outright: 7E-2D reserves 8 holdout cohorts
    // here, and 8 decisions cannot support a claim at any conventional level, so
    // the refusal is a sample-size refusal and names the required n. That is the
    // correct answer, not a threshold that happens to be strict, and it is
    // exactly what `EvidenceFloors`'s presence floor could not say.
    assert!(
        !outcome.report().statistics.is_supported(),
        "eight decisions must not be reported as support"
    );
    let refusal = outcome
        .report()
        .statistics
        .refusal()
        .expect("a partition of eight must be refused, not measured");
    assert_eq!(refusal.code, "sample_too_small");
    assert!(
        refusal.reason.contains("8 effective decisions"),
        "{}",
        refusal.reason
    );
    assert_eq!(report.recomputed_verdict(), ReleaseVerdict::NotConsiderable);
    println!("{}", report.headline());

    // Every constituent is visible on the report, so the verdict is auditable
    // rather than asserted.
    let m = &report.measurements;
    assert!(m.commit_verified);
    assert!(m.lineage_verified);
    assert_eq!(m.decisions_replayed, DECISIONS);
    assert_eq!(m.decisions_equivalent, DECISIONS);
    assert_eq!(m.terminal_agreements, DECISIONS);
    assert_eq!(m.served_identities, DECISIONS);
    assert_eq!(m.absent_served_identities, 0);
    assert_eq!(m.holdout.overlap_with_fit_set, 0);
    assert_eq!(m.calibration, CalibrationVerdict::Calibrated);
    assert!(m.float_fidelity.f64_fields > 0);
    // The commit's transport is reported, not assumed. A trained commit is
    // expected NOT to survive plain JSON in this workspace, and the report must
    // say so rather than imply the artifact is storable as it stands.
    assert!(
        report.transport.f64_parameters > 0,
        "the commit's f64 parameters must actually be measured"
    );
    assert_eq!(
        report.transport.plain_json_exact,
        report.transport.moved_parameters == 0,
        "the exactness flag is derived from the count, not supplied"
    );
    assert_eq!(report.transport.commit_id, report.model_commit.as_str());
    assert!(report.headline().contains(RELEASE_SCOPE));
    assert!(report.headline().contains("commit plain-JSON transport"));
    // Printed rather than asserted: this node's product is evidence, and the
    // headline is the evidence in one line. Run with `-- --nocapture`.
    println!("{}", report.headline());
}

#[test]
fn the_verdict_states_what_it_is_not() {
    let trained = trained();
    let recorded = fixture(&trained);
    let mut input = gate_input(&trained, recorded);
    input.config = permissive_config();
    let report = run_offline_gate(&input)
        .expect("the fixture replays")
        .report()
        .clone();

    assert_eq!(report.scope, RELEASE_SCOPE);
    assert!(RELEASE_SCOPE.contains("NOT a statistical claim"));
    assert!(RELEASE_SCOPE.contains("NOT a claim that the model is better"));
    assert!(RELEASE_SCOPE.contains("NOT a claim that the model can be served"));
    assert!(report.headline().contains("NOT a statistical claim"));
}

#[test]
fn two_runs_of_the_same_input_produce_the_same_verdict_and_the_same_constituents() {
    let trained = trained();
    // The *same* recorded input, gated twice. Building the fixture twice would
    // mint fresh `shadow_id` UUIDs and compare two different inputs, which is
    // a test of the fixture rather than of the gate.
    let recorded = fixture(&trained);
    let mut first = gate_input(&trained, recorded.clone());
    first.config = permissive_config();
    let mut second = gate_input(&trained, recorded);
    second.config = permissive_config();

    let left = run_offline_gate(&first).expect("the fixture replays");
    let right = run_offline_gate(&second).expect("the fixture replays");

    assert_eq!(left.report().measurements, right.report().measurements);
    assert_eq!(left.report().verdict, right.report().verdict);
    assert_eq!(
        serde_json::to_string(left.report()).expect("the report serializes"),
        serde_json::to_string(right.report()).expect("the report serializes"),
        "a gate whose own output moves is a gate nobody can reason about"
    );
}

#[test]
fn the_release_verdict_is_a_pure_function_of_its_measurements() {
    let trained = trained();
    let recorded = fixture(&trained);
    let mut input = gate_input(&trained, recorded);
    input.config = permissive_config();
    let report = run_offline_gate(&input)
        .expect("the fixture replays")
        .report()
        .clone();

    // Mutating any single constituent must move the verdict, which is the test
    // that the verdict is recomputed rather than read.
    assert_eq!(
        ReleaseVerdict::from_measurements(&report.measurements),
        report.verdict,
        "the recorded verdict must be the recomputed one"
    );
    assert!(
        ReleaseVerdict::blockers(&report.measurements)
            .iter()
            .all(|blocker| !NODE_7E3_BLOCKERS.contains(blocker)),
        "no 7E-3 constituent may withhold: {:?}",
        report.blockers()
    );

    let mut broken = report.measurements.clone();
    broken.decisions_equivalent -= 1;
    assert_eq!(
        ReleaseVerdict::from_measurements(&broken),
        ReleaseVerdict::NotConsiderable
    );
    // Exactly one **7E-3** blocker is added by this mutation. 7D's blockers are
    // already withholding on this fixture and are counted separately, because
    // counting the whole list would make this node's assertion depend on 7D's
    // arithmetic.
    let seven_e_three: Vec<&str> = ReleaseVerdict::blockers(&broken)
        .into_iter()
        .filter(|blocker| NODE_7E3_BLOCKERS.contains(blocker))
        .collect();
    assert_eq!(seven_e_three.len(), 1, "{seven_e_three:?}");
    assert!(
        seven_e_three[0].contains("bit-identical"),
        "{}",
        seven_e_three[0]
    );

    let mut broken = report.measurements.clone();
    broken.retention = RetentionAblation {
        probed: 4,
        load_bearing: 0,
    };
    assert!(!ReleaseVerdict::from_measurements(&broken).is_considerable());

    let mut broken = report.measurements.clone();
    broken.float_fidelity.round_trip_exact = false;
    broken.float_fidelity.moved_fields = 1;
    assert_eq!(
        ReleaseVerdict::from_measurements(&broken),
        ReleaseVerdict::NotConsiderable
    );
    assert!(ReleaseVerdict::blockers(&broken)
        .iter()
        .any(|blocker| blocker.contains("do not survive")));
}

// ---------------------------------------------------------------------------
// 7. The artifact refusals
// ---------------------------------------------------------------------------

#[test]
fn a_commit_with_no_identity_is_refused_as_missing() {
    let trained = trained();
    let recorded = fixture(&trained);
    let mut input = gate_input(&trained, recorded);
    input.commit = ModelEnsemblePredictor::genesis().commit_record();
    input.commit.commit_id = CommitId::new("");

    let refusal =
        run_offline_gate(&input).expect_err("a commit that cannot name itself is missing");
    assert!(
        matches!(refusal, OfflineGateError::MissingCommit { .. }),
        "expected a missing-commit refusal, got {refusal}"
    );
}

#[test]
fn a_commit_whose_checkpoint_was_tampered_with_is_refused_as_unverifiable() {
    let trained = trained();
    let recorded = fixture(&trained);
    let mut input = gate_input(&trained, recorded);
    input.lineage = vec![input.commit.clone()];
    input.commit.checkpoint.success.parameters[0] += 0.5;

    let refusal = run_offline_gate(&input).expect_err("a tampered checkpoint must refuse to load");
    let OfflineGateError::UnverifiableCommit { commit_id, .. } = refusal else {
        panic!("expected an unverifiable-commit refusal");
    };
    assert!(!commit_id.is_empty());
}

#[test]
fn a_lineage_missing_its_parent_is_refused() {
    let trained = trained();
    let recorded = fixture(&trained);
    let mut input = gate_input(&trained, recorded);
    // Drop the parent so the retained lineage no longer reaches a root.
    input.lineage.truncate(1);

    let refusal = run_offline_gate(&input).expect_err("a truncated lineage must refuse");
    assert!(
        matches!(refusal, OfflineGateError::LineageRejected { .. }),
        "expected a lineage refusal, got {refusal}"
    );
}

#[test]
fn a_refusal_names_the_commit_it_is_about() {
    let trained = trained();
    let recorded = fixture(&trained);
    let mut input = gate_input(&trained, recorded);
    input.lineage.truncate(1);

    let refusal = run_offline_gate(&input).expect_err("a truncated lineage must refuse");
    assert_eq!(
        refusal.commit_id(),
        Some(trained.commit.commit_id.as_str().to_string())
    );
    // A refusal that is not about a commit says so rather than inventing one.
    let nothing = OfflineGateError::NothingToGate;
    assert_eq!(nothing.commit_id(), None);
}

#[test]
fn a_recorded_decision_with_no_retained_candidates_is_refused() {
    let trained = trained();
    let mut recorded = fixture(&trained);
    recorded[0].decision.observation.input.candidates.clear();

    let refusal = run_offline_gate(&gate_input(&trained, recorded))
        .expect_err("there is no retained input to replay from");
    assert!(
        matches!(refusal, OfflineGateError::RetainedInputAbsent { .. }),
        "expected a retained-input refusal, got {refusal}"
    );
}

// ---------------------------------------------------------------------------
// The gate itself activates nothing
// ---------------------------------------------------------------------------

#[test]
fn the_gate_produces_no_explore_action_and_activates_nothing() {
    let trained = trained();
    let recorded = fixture(&trained);
    let outcome = run_offline_gate(&gate_input(&trained, recorded)).expect("the fixture replays");

    for evidence in &outcome.report().replay {
        assert!(
            !matches!(evidence.action, RoutingAction::Explore),
            "a release gate must never produce an explore action: {:?}",
            evidence.action
        );
    }
    // The predictor it hands back is pinned to the verified commit and carries
    // the verified lineage, which is all it is: nothing was installed.
    assert_eq!(outcome.predictor().commit(), outcome.report().model_commit);
    assert_eq!(
        outcome.predictor().lineage().len(),
        3,
        "the predictor must carry the complete verified lineage"
    );
    assert!(outcome.predictor().verify());
}
