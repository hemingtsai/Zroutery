// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

use super::*;
use crate::ml::coordinator::CoordinatorConfig;
use crate::ml::decision_engine::DecisionEngine;
use crate::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION, UNKNOWN};
use crate::ml::model::RoutingModel;
use crate::ml::model_identity::{ModelCheckpoint, ModelEnsemble, ModelId};
use crate::ml::promotion::{PromotionDecision, PromotionVerdict};
use crate::ml::reward::RewardPolicy;
use crate::ml::shadow::{EnsemblePredictor, ShadowCandidateInput, ShadowInput};

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
        // A sufficient measurement. These tests are about what happens
        // *after* a promotion, not about whether the evidence supports one, and
        // the statistical criterion would otherwise decide every one of them.
        // Production always carries evidence: it is attached by
        // `run_comparison`, so `None` is a fixture-only shape.
        evidence: supporting_evidence(),
    }
}

/// Evidence sufficient to pass the statistical criterion, for fixtures that are
/// about something other than whether the evidence supports a promotion.
fn supporting_evidence(
) -> std::collections::BTreeMap<String, crate::ml::eval_stats::RoutingEvidence> {
    let observations: Vec<crate::ml::eval_stats::PairedObservation> = (0..200)
        .map(|index| crate::ml::eval_stats::PairedObservation {
            recorded_at: 1_700_000_000 + index as i64 * 3_600,
            utility_delta: 0.5,
            candidate_only_success: true,
            baseline_only_success: false,
        })
        .collect();
    [crate::ml::eval_stats::measure(
        &observations,
        &crate::ml::eval_stats::EvidenceConfig::default(),
    )]
    .into_iter()
    // Keyed under every baseline these fixtures might be judged against:
    // `permissive_gate` names `baseline.priority` while the default names
    // `baseline.lowest_latency`, and the gate reads whichever its config says.
    .flat_map(|evidence| {
        ["baseline.priority", "baseline.lowest_latency"]
            .into_iter()
            .map(move |name| (name.to_string(), evidence.clone()))
    })
    .collect()
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

// -- does the cost axis survive all the way to a served decision? -------
//
// Everything above this line tests the cost head as a component. These two
// test it as a *served* quantity, and they exist because of a measured hole:
// zeroing `bundle.cost` inside `ActivePredictor::predict` — between the
// ensemble and the ranking — leaves all 2129 tests in the workspace green.
//
// The reason nothing caught it is structural rather than accidental. The head
// is trained, checkpointed, loaded and bundled correctly; the cost term is
// three orders of magnitude below the success term; and `RankedPlan` reports
// only total utility. So a serving path that discards cost entirely produces
// byte-identical rankings and no test in the repository can see it.

/// Features that differ only in tier, which is the coordinate this fixture
/// teaches the cost head to read.
fn tier_features(tier: f32) -> RoutingFeatures {
    let mut values = [UNKNOWN; FEATURE_DIMENSION];
    values[crate::ml::features::F_TIER] = tier;
    RoutingFeatures {
        schema_version: FEATURE_SCHEMA_VERSION,
        values,
    }
}

/// A model that has learned *a higher tier costs more*, and in which nothing
/// else varies with tier.
///
/// Every sample carries the same success, latency and TTFT target, so those
/// three heads converge to a constant and the only thing distinguishing the
/// two probe vectors is the cost axis. Without that, an assertion about cost
/// would be an assertion about a correlation.
fn cost_separated_ensemble() -> ModelEnsemble {
    let mut ensemble = ModelEnsemble::new();
    for pass in 0..8 {
        for (tier, cost) in [(0.0f32, 0.001f64), (8.0f32, 0.010f64)] {
            for repeat in 0..20 {
                let mut values = [UNKNOWN; FEATURE_DIMENSION];
                values[crate::ml::features::F_TIER] = tier;
                ensemble.update_all(&crate::ml::DatasetTrainingSample {
                    sample_id: format!("s-{pass}-{tier}-{repeat}"),
                    schema_version: 1,
                    timestamp: 1_700_000_000 + pass as i64,
                    features: RoutingFeatures {
                        schema_version: FEATURE_SCHEMA_VERSION,
                        values,
                    },
                    targets: crate::ml::dataset::Targets {
                        success: true,
                        latency_ms: Some(100.0),
                        ttft_ms: Some(50.0),
                        cost: Some(cost),
                        failure_class: None,
                        fallback_count: 0,
                    },
                    provider_id: "p".to_string(),
                    model_id: "m".to_string(),
                    origin: crate::feedback::DataOrigin::Native,
                    outcome_id: format!("o-{pass}-{tier}-{repeat}"),
                    feedback: Vec::new(),
                });
            }
        }
    }
    ensemble
}

/// **The promoted model's cost is the head's cost.**
///
/// Goes through the gate and `ActivePredictor::load`, so this also covers
/// the checkpoint round trip: a cost head dropped by `save_all`/`load_all`
/// would load as the seeded 0.01 and the ratio below would collapse.
#[test]
fn a_promoted_model_delivers_the_cost_its_head_learned() {
    let ensemble = cost_separated_ensemble();
    let predictor = predictor_for(&ensemble, FIXTURE_EVENTS);

    let cheap = predictor.predict("m", "p", &tier_features(0.0));
    let dear = predictor.predict("m", "p", &tier_features(8.0));

    assert!(
        cheap.cost.value > 0.0,
        "the promoted model reports cost {} for the cheap tier; a head that \
         never trained would report its seeded bias of 0.01 for both tiers \
         and the comparison below would be meaningless",
        cheap.cost.value
    );
    assert!(
        dear.cost.value > cheap.cost.value * 2.0,
        "a head trained on costs 0.001 and 0.010 predicts {} and {} across the \
         two tiers. A promoted model that reports the same cost for both has \
         lost the cost axis somewhere between the checkpoint and the ranking.",
        cheap.cost.value,
        dear.cost.value
    );

    // And it is the head's own figure rather than something reconstructed
    // from the checkpoint, which is what makes this a fidelity check and not
    // a plausibility check.
    assert_eq!(
        dear.cost.value,
        ensemble.cost.predict(&tier_features(8.0)).value,
        "the served cost must be exactly what the cost head predicts"
    );
}

/// **The cost reaches the number the router acts on.**
///
/// `RankedPlan::utilities` reports each candidate's total, and that total is
/// `compute_utility` over the bundle the serving path built. So recomputing
/// the utility from the ensemble's own prediction is a per-candidate
/// fidelity check on the whole chain — trained head, checkpoint, attached
/// predictor, bundle, engine, reported total — and it needs no outcome flip
/// to be decisive.
///
/// The check is deliberately per candidate rather than a comparison between
/// them: the other three heads converge to a constant but not to *exactly* a
/// constant, and this way their residual drift cancels out instead of
/// needing a tolerance wide enough to hide a real cost regression.
#[test]
fn the_reported_utility_carries_the_cost_the_model_predicted() {
    let ensemble = cost_separated_ensemble();
    let router = {
        let engine = DecisionEngine::new(CoordinatorConfig::default(), RewardPolicy::default());
        let router = MlRouter::new(engine, ExplorationConfig::default());
        router.attach(predictor_for(&ensemble, FIXTURE_EVENTS));
        router
    };

    let probes = [("cheap", 0.0f32), ("dear", 8.0f32)];
    let candidates: Vec<ShadowCandidateInput> = probes
        .iter()
        .map(|(id, tier)| ShadowCandidateInput {
            candidate_id: (*id).to_string(),
            provider_id: "p".to_string(),
            tier: None,
            eligible: true,
            features: tier_features(*tier),
            rejection_reason: None,
        })
        .collect();
    let plan = router
        .rank(&input("cheap", candidates), "r1")
        .expect("ranked");

    let policy = RewardPolicy::default();
    for (id, tier) in probes {
        let bundle = crate::ml::predict_bundle(&ensemble, id, "p", &tier_features(tier));
        let expected = crate::ml::compute_utility(&bundle, &policy, false, 0).total;
        let reported = plan
            .utilities
            .iter()
            .find(|(candidate_id, _)| candidate_id == id)
            .map(|(_, total)| *total)
            .unwrap_or_else(|| {
                panic!(
                    "the ranking reported no utility for {id}: {:?}",
                    plan.utilities
                )
            });

        // `expected` here was built from a prediction of {} dollars, so the
        // cost term in it is {}. A serving path that dropped cost would
        // report {reported} instead — a difference of exactly that term.
        assert!(
            (reported - expected).abs() < 1e-12,
            "the utility reported for {id} is {reported}, but the bundle the \
             ensemble produces for it scores {expected}. The gap is {} — which is \
             the cost term, so the cost axis did not reach the ranking.",
            (reported - expected).abs()
        );
    }

    // And the two are genuinely different numbers, so the check above is not
    // passing because the assertion is vacuous.
    let totals: Vec<f64> = plan.utilities.iter().map(|(_, total)| *total).collect();
    assert!(
        (totals[0] - totals[1]).abs() > 0.0,
        "the two candidates should not score identically, or nothing above \
         was actually tested"
    );
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
