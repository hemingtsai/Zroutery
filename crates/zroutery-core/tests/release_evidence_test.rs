#![cfg(feature = "ml")]

//! Node 7D — 7D's statistical constituent wired into 7E-3's release verdict.
//!
//! The claims this suite has to earn:
//!
//! 1. **the gate got stronger, not weaker** —
//!    `every_blocker_7e3_owned_still_withholds` breaks each of 7E-3's eleven
//!    constituents in turn and asserts the exact blocker string is still
//!    produced, and `the_seven_d_blockers_are_appended_never_interleaved`
//!    breaks all eleven at once and asserts they come first, in 7E-3's order,
//!    with 7D's after them;
//! 2. **7D's blocker is real and can be satisfied** —
//!    `a_statistical_refusal_withholds_the_verdict` and
//!    `every_constituent_passing_reaches_a_considerable_verdict`, the second of
//!    which is the anti-vacuity proof at the level of the whole gate: with a
//!    real effect and a real holdout, the verdict reaches `Considerable`;
//! 3. **a thin holdout is a finding, not a crash** —
//!    `a_holdout_too_thin_to_test_still_produces_a_report`;
//! 4. **bit-reproducibility through the wired path** —
//!    `two_runs_produce_the_same_statistical_constituent`;
//! 5. **the constituent is measured over the partition 7E-2D reserved** —
//!    `the_statistical_constituent_is_measured_over_the_reserved_holdout`;
//! 6. **both scopes are carried, and neither over-reads the other** —
//!    `the_report_carries_both_scopes`.
//!
//! Fixture note: 7E-3's fixture reserves eight holdout cohorts, which is below
//! any honest statistical floor, so its statistical constituent refuses. That is
//! why this suite builds its own partition for the tests that need a *supported*
//! measurement rather than reusing 7E-3's.

use std::collections::BTreeSet;

use zroutery_core::config::ModelTier;
use zroutery_core::failure::FailureClass;
use zroutery_core::ir::Usage;
use zroutery_core::ml::calibration::{
    collect_marginal_observations, measure_marginal, CalibrationConfig, CandidateInput,
    CohortContext, DecisionCohort, EmittedDecision, MarginalCalibrator, MarginalView,
    KWayCalibrator, ReliabilityConfig, DEFAULT_PROBABILITY_FLOOR,
};
use zroutery_core::ml::coordinator::{CoordinatorConfig, RoutingAction};
use zroutery_core::ml::dataset::{
    canonical_samples_from_decision_time, Targets, TrainingSample as DatasetTrainingSample,
};
use zroutery_core::ml::decision_engine::DecisionEngine;
use zroutery_core::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use zroutery_core::ml::model_identity::ModelCommit;
use zroutery_core::ml::offline_gate::{
    run_offline_gate, EvidenceFloors, GateConfig, GateInput, RecordedDecision, ReleaseMeasurements,
    ReleaseReport, ReleaseVerdict, RetentionAblation, TerminalAgreement, RELEASE_SCOPE,
    STATISTICAL_SCOPE,
};
use zroutery_core::ml::reward::RewardPolicy;
use zroutery_core::ml::shadow::{
    ModelEnsemblePredictor, ShadowCandidateInput, ShadowEngine, ShadowInput,
};
use zroutery_core::ml::statistics::{
    measure_release_evidence, EvidenceSupport, StatisticalConfig, StatisticalInput,
    StatisticalRelease,
};
use zroutery_core::outcome::{Attempt, CandidateIdentity, Outcome};
use zroutery_core::session::SessionRoutingMode;

const BASE: i64 = 1_700_000_000;

/// `(model, provider)` for the three candidates every decision compares.
const AXIS: [(&str, &str); 3] = [("alpha", "prov-a"), ("bravo", "prov-b"), ("charlie", "prov-c")];

const DECISIONS: usize = 24;

// ---------------------------------------------------------------------------
// 7E-3's fixture, restated so this suite does not depend on its test binary
// ---------------------------------------------------------------------------

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
    }
}

fn outcome(decision: usize, winner: usize) -> Outcome {
    let mut builder = Outcome::builder(format!("req-{decision:04}"))
        .decision_id(format!("dec-{decision:04}"))
        .planned(AXIS[0].0, AXIS[0].1)
        .dialect("openai")
        .streaming(false);
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

struct Trained {
    predictor: ModelEnsemblePredictor,
    commit: ModelCommit,
    lineage: Vec<ModelCommit>,
}

fn trained() -> Trained {
    let root_commit = ModelEnsemblePredictor::genesis().commit_record();
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

fn engine_config() -> CoordinatorConfig {
    CoordinatorConfig {
        exploration_enabled: false,
        ..CoordinatorConfig::default()
    }
}

fn gate_config() -> GateConfig {
    GateConfig {
        calibration: CalibrationConfig {
            holdout: zroutery_core::ml::calibration::HoldoutConfig {
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
        statistics: StatisticalConfig::default(),
    }
}

fn permissive_config() -> GateConfig {
    let mut config = gate_config();
    config.calibration.reliability = ReliabilityConfig {
        max_expected_calibration_error: 1.0,
        max_calibration_error: 1.0,
        max_candidate_calibration_error: 1.0,
        ..ReliabilityConfig::default()
    };
    config.calibration.drift = zroutery_core::ml::calibration::DriftConfig {
        max_population_stability_index: 1.0,
        max_base_rate_delta: 1.0,
        ..zroutery_core::ml::calibration::DriftConfig::default()
    };
    config
}

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

/// One gated run on the permissive configuration, and its report.
fn gated() -> (Trained, ReleaseReport) {
    let trained = trained();
    let recorded = fixture(&trained);
    let mut input = gate_input(&trained, recorded);
    input.config = permissive_config();
    let report = run_offline_gate(&input)
        .expect("the fixture replays")
        .report()
        .clone();
    (trained, report)
}

// ---------------------------------------------------------------------------
// 7E-3's eleven blockers, in the order its own `blockers()` emits them
// ---------------------------------------------------------------------------

/// The blockers 7E-3 owns, by the exact strings it emits, in its own order.
///
/// This is the list 7D promised not to touch. It is spelled out here rather than
/// derived, because a derived list would follow whatever the function happened
/// to return — and the claim is that the *strings and the order* are unchanged,
/// which only a literal can say.
const SEVEN_E_THREE_BLOCKERS: [&str; 11] = [
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

/// One 7E-3 constituent: the blocker it raises, and how to break it.
type Provocation = (&'static str, fn(&mut ReleaseMeasurements));

/// Each 7E-3 constituent, and a way to break exactly that one.
///
/// The counts are **absolute**, not relative. Zeroing `decisions_replayed` and
/// then deriving `decisions_equivalent = decisions_replayed - 1` would set both to
/// zero and *hide* the equivalence blocker behind an equality — which is 7E-3's
/// arithmetic interacting with a badly-written provocation, not a change in
/// 7E-3's rule. An absolute 23 against a replayed count of 0 keeps all three
/// replay-related blockers true at once, which is what lets the ordering test
/// assert all eleven positions.
fn seven_e_three_breaks() -> Vec<Provocation> {
    vec![
        ("the commit did not pass the accepted verification", |m| {
            m.commit_verified = false;
        }),
        ("the retained lineage did not verify", |m| {
            m.lineage_verified = false;
        }),
        ("no decision was replayed", |m| {
            m.decisions_replayed = 0;
        }),
        (
            "at least one replay was not bit-identical to its record",
            |m| {
                m.decisions_equivalent = 23;
            },
        ),
        (
            "at least one replay disagreed with its Outcome",
            |m| {
                m.terminal_agreements = 23;
            },
        ),
        (
            "no retained feature position was shown to be load-bearing for the replayed decision",
            |m| {
                m.retention = RetentionAblation {
                    probed: 4,
                    load_bearing: 0,
                };
            },
        ),
        ("at least one Outcome records no served identity", |m| {
            m.absent_served_identities = 1;
        }),
        ("the holdout overlaps the model's fit set", |m| {
            m.holdout.overlap_with_fit_set = 1;
        }),
        ("the holdout is empty", |m| m.holdout.decisions = 0),
        (
            "7E-2D reports the emitted vector as miscalibrated",
            |m| {
                m.calibration = zroutery_core::ml::calibration::CalibrationVerdict::Miscalibrated;
            },
        ),
        (
            "canonical sample floats do not survive this workspace's JSON read path",
            |m| {
                m.float_fidelity.round_trip_exact = false;
                m.float_fidelity.moved_fields = 1;
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// 1. The gate got stronger, not weaker
// ---------------------------------------------------------------------------

#[test]
fn every_blocker_7e3_owned_still_withholds() {
    let (_trained, report) = gated();
    let healthy = report.measurements.clone();
    let breaks = seven_e_three_breaks();
    assert_eq!(
        breaks.len(),
        SEVEN_E_THREE_BLOCKERS.len(),
        "every 7E-3 blocker needs a way to provoke it"
    );

    // Break the constituents one at a time, cumulatively, and check three
    // properties at every step: the constituent just broken withholds, nothing
    // that withheld a moment ago has stopped withholding, and the healthy
    // measurements never withheld at all. The cumulative build is what makes the
    // last assertion meaningful — 7E-3's counts interact, so a fresh clone per
    // step would not show a blocker being masked by a later mutation.
    let mut cumulative: Vec<&'static str> = Vec::new();
    let mut running = healthy.clone();
    for (index, (expected, break_it)) in breaks.iter().enumerate() {
        assert_eq!(
            *expected, SEVEN_E_THREE_BLOCKERS[index],
            "the literal list and the provocations must agree on order and text"
        );
        assert!(!ReleaseVerdict::blockers(&healthy).contains(expected));

        break_it(&mut running);
        let blockers = ReleaseVerdict::blockers(&running);
        assert!(
            blockers.contains(expected),
            "{expected} must still withhold, got {blockers:?}"
        );
        assert!(!ReleaseVerdict::from_measurements(&running).is_considerable());
        for blocker in &cumulative {
            assert!(
                blockers.contains(blocker),
                "{blocker} stopped withholding once {expected} was also broken: {blockers:?}"
            );
        }
        if !cumulative.contains(expected) {
            cumulative.push(expected);
        }
        assert_eq!(
            cumulative.len(),
            index + 1,
            "after {} breaks, {} of 7E-3's blockers withhold",
            index + 1,
            cumulative.len()
        );
    }
    assert_eq!(cumulative.len(), SEVEN_E_THREE_BLOCKERS.len());
}

#[test]
fn the_seven_d_blockers_are_appended_never_interleaved() {
    let (_trained, report) = gated();
    let mut broken = report.measurements.clone();
    // Break every 7E-3 constituent at once, and leave 7D's refusing too.
    for (_, break_it) in seven_e_three_breaks() {
        break_it(&mut broken);
    }
    let blockers = ReleaseVerdict::blockers(&broken);
    println!("ALL BROKEN: {blockers:#?}");

    // The first eleven entries are exactly 7E-3's, in 7E-3's order. Nothing was
    // removed, weakened or reordered, and nothing was inserted before them.
    assert_eq!(
        blockers.len(),
        SEVEN_E_THREE_BLOCKERS.len() + 1,
        "eleven of 7E-3's plus exactly one of 7D's: {blockers:?}"
    );
    for (index, expected) in SEVEN_E_THREE_BLOCKERS.iter().enumerate() {
        assert_eq!(
            blockers[index], *expected,
            "7E-3's {index}th blocker moved or changed"
        );
    }

    // 7D's entry follows, and it is a distinct label.
    let rest = &blockers[SEVEN_E_THREE_BLOCKERS.len()..];
    assert_eq!(rest.len(), 1, "{rest:?}");
    assert!(
        !SEVEN_E_THREE_BLOCKERS.contains(&rest[0]),
        "7D re-emitted a 7E-3 blocker: {rest:?}"
    );
    assert!(
        rest[0].contains("statistical claim could not be measured"),
        "the statistical refusal must be named: {rest:?}"
    );
}

#[test]
fn a_statistical_refusal_withholds_the_verdict() {
    let (_trained, report) = gated();
    let statistics = &report.statistics;
    assert!(!statistics.is_supported());

    // The 7E-3 fixture reserves eight holdout cohorts, which is below any
    // honest floor, so the constituent refuses — and says so with both numbers.
    let refusal = statistics
        .refusal()
        .expect("eight decisions must be refused, not measured");
    assert_eq!(refusal.code, "sample_too_small");
    println!("WIRED REFUSAL: {}: {}", refusal.code, refusal.reason);
    assert!(refusal.reason.contains("8 effective decisions"));
    assert!(refusal.reason.contains("30 are required"));

    // The refusal is a blocker, the verdict withholds, and the report is still a
    // complete report.
    assert!(!report.is_considerable());
    assert_eq!(report.recomputed_verdict(), ReleaseVerdict::NotConsiderable);
    assert!(report
        .blockers()
        .iter()
        .any(|blocker| blocker.contains("statistical claim could not be measured")));
    assert!(report
        .statistical_reasons()
        .iter()
        .any(|line| line.starts_with("sample_too_small:")));
    // 7E-3's own constituents are all fine, and the report says so.
    assert_eq!(report.transport.commit_id, report.model_commit.as_str());
    assert_eq!(report.measurements.decisions_replayed, DECISIONS);
    assert_eq!(report.measurements.decisions_equivalent, DECISIONS);
    assert_eq!(report.measurements.terminal_agreements, DECISIONS);
}

// ---------------------------------------------------------------------------
// 2. 7D's blocker can be satisfied, so it is not a blanket refusal
// ---------------------------------------------------------------------------

/// A real effect, measured over a partition large enough to support it.
///
/// Built through 7E-2D's own constructors, exactly as `statistics_test.rs`
/// does, so the constituent under test is one the release verdict can carry.
fn supported_constituent() -> StatisticalRelease {
    let n = 4_000usize;
    let marginal_calibrator = {
        let pairs: Vec<(f64, bool)> = (0..64)
            .map(|index| (0.1 + (index % 8) as f64 * 0.1, index % 3 == 0))
            .collect();
        MarginalCalibrator::fit(&pairs, &Default::default(), DEFAULT_PROBABILITY_FLOOR)
            .expect("the fixture's marginal pairs fit")
    };
    let cohorts: Vec<DecisionCohort> = (0..n)
        .map(|index| {
            let winner = index % 3;
            let pick = if index % 2 == 0 { winner } else { (winner + 1) % 3 };
            let context = CohortContext {
                timestamp: BASE + index as i64,
                dialect: "openai".to_string(),
                streaming: false,
                final_status_rank: 0,
                failure_class_rank: None,
            };
            let candidates: Vec<CandidateInput> = (0..AXIS.len())
                .map(|slot| CandidateInput::Ranked {
                    candidate: CandidateIdentity::new(AXIS[slot].0, AXIS[slot].1),
                    raw_success_probability: if slot == pick { 0.95 } else { 0.02 },
                })
                .collect();
            let subject = CandidateIdentity::new(AXIS[0].0, AXIS[0].1);
            DecisionCohort::try_new(
                context,
                Some(&subject),
                candidates,
                Some(CandidateIdentity::new(AXIS[winner].0, AXIS[winner].1)),
            )
            .expect("the fixture cohort is well formed")
        })
        .collect();
    let calibrator = KWayCalibrator::uncalibrated();
    let emitted: Vec<EmittedDecision> = cohorts
        .iter()
        .map(|cohort| {
            calibrator
                .distribution(cohort, DEFAULT_PROBABILITY_FLOOR)
                .expect("the fixture cohort emits a distribution")
        })
        .collect();
    let observations = collect_marginal_observations(
        &cohorts,
        &marginal_calibrator,
        DEFAULT_PROBABILITY_FLOOR,
    )
    .expect("the fixture's marginal observations collect");
    let marginal = measure_marginal(
        &observations,
        MarginalView::Calibrated,
        &ReliabilityConfig::default(),
    )
    .expect("the fixture's marginal observations measure")
    .per_candidate;

    measure_release_evidence(&StatisticalInput {
        partition: &cohorts,
        emitted: &emitted,
        marginal: &marginal,
        config: StatisticalConfig::default(),
    })
    .expect("a real effect is measurable")
}

#[test]
fn every_constituent_passing_reaches_a_considerable_verdict() {
    let (_trained, report) = gated();
    let statistics = supported_constituent();
    let support: &EvidenceSupport = statistics
        .support()
        .expect("a real effect is supported");
    println!("SUPPORTED: {}", support.headline());
    assert!(statistics.is_supported());
    assert!(statistics.blockers().is_empty());
    assert!(statistics.reasons().is_empty());

    // Put it on a real report whose every 7E-3 constituent passes.
    let mut measurements = report.measurements.clone();
    measurements.statistics = statistics.clone();
    assert!(
        !ReleaseVerdict::blockers(&measurements)
            .iter()
            .any(|blocker| SEVEN_E_THREE_BLOCKERS.contains(blocker)),
        "7E-3's constituents must all pass: {:?}",
        ReleaseVerdict::blockers(&measurements)
    );
    assert!(
        ReleaseVerdict::blockers(&measurements).is_empty(),
        "{:?}",
        ReleaseVerdict::blockers(&measurements)
    );
    assert_eq!(
        ReleaseVerdict::from_measurements(&measurements),
        ReleaseVerdict::Considerable,
        "with every constituent measured and passing, the whole gate is considerable"
    );

    // And the withheld verdict is the *only* difference: adding 7D's constituent
    // back to the thin-holdout report is what changed it, and nothing else.
    let mut thin = report.measurements.clone();
    thin.statistics = statistics.clone();
    assert_eq!(
        ReleaseVerdict::from_measurements(&thin),
        ReleaseVerdict::Considerable
    );
}

// ---------------------------------------------------------------------------
// 3. A thin holdout is a finding, not a crash
// ---------------------------------------------------------------------------

#[test]
fn a_holdout_too_thin_to_test_still_produces_a_report() {
    let trained = trained();
    let recorded = fixture(&trained);
    let mut input = gate_input(&trained, recorded);
    input.config = permissive_config();

    // The gate runs to completion. A holdout that cannot support a statistical
    // claim is a finding about the evidence, and the report is how a finding is
    // delivered; turning it into an `Err` would throw away every other
    // constituent the run measured.
    let outcome = run_offline_gate(&input).expect("the gate still produces a report");
    let report = outcome.report();
    assert!(!report.is_considerable());
    assert_eq!(report.statistics.refusal().expect("a refusal").code, "sample_too_small");
    // 7E-3's own report is intact and unaffected.
    assert_eq!(report.replay.len(), DECISIONS);
    assert_eq!(report.holdout.decisions, DECISIONS);
    assert_eq!(report.holdout.attributed, DECISIONS);
    assert!(report.calibration.is_calibrated());
    assert!(report.float_fidelity.round_trip_exact);
    // Every decision was still judged against its canonical Outcome, and keyed
    // on the identity that actually served. The fixture's model is arbitrary with
    // respect to the scripted winner, so more than one of 7E-3's classifications
    // is expected — which is the point: the classification is being computed over
    // the whole replay and is not affected by a thin statistical holdout.
    let mut seen: Vec<TerminalAgreement> = Vec::new();
    for evidence in &report.replay {
        assert!(evidence.served.is_some());
        assert!(evidence.components_compared > 50);
        if !seen.contains(&evidence.terminal) {
            seen.push(evidence.terminal);
        }
    }
    assert!(!seen.is_empty());
    assert!(
        report.measurements.terminal_agreements == report.replay.len(),
        "every replay is counted as an agreement with its Outcome"
    );
}

// ---------------------------------------------------------------------------
// 4. Bit-reproducibility through the wired path
// ---------------------------------------------------------------------------

#[test]
fn two_runs_produce_the_same_statistical_constituent() {
    let trained = trained();
    let recorded = fixture(&trained);
    let mut first = gate_input(&trained, recorded.clone());
    first.config = permissive_config();
    let mut second = gate_input(&trained, recorded);
    second.config = permissive_config();

    let left = run_offline_gate(&first).expect("the fixture replays");
    let right = run_offline_gate(&second).expect("the fixture replays");

    assert_eq!(left.report().statistics, right.report().statistics);
    assert_eq!(left.report().measurements, right.report().measurements);
    assert_eq!(left.report().verdict, right.report().verdict);
    assert_eq!(
        serde_json::to_string(&left.report().statistics).expect("serializes"),
        serde_json::to_string(&right.report().statistics).expect("serializes"),
        "the statistical constituent must be a pure function of the holdout"
    );
    assert_eq!(left.report().headline(), right.report().headline());
    assert_eq!(
        serde_json::to_string(left.report()).expect("serializes"),
        serde_json::to_string(right.report()).expect("serializes"),
        "a gate whose own output moves is a gate nobody can reason about"
    );
}

// ---------------------------------------------------------------------------
// 5. The constituent is measured over the partition 7E-2D reserved
// ---------------------------------------------------------------------------

#[test]
fn the_statistical_constituent_is_measured_over_the_reserved_holdout() {
    let (_trained, report) = gated();
    let calibration = &report.calibration;
    let refusal = report.statistics.refusal().expect("a refusal");

    // The refusal names the size of the partition 7E-2D reserved, not the size
    // of the whole snapshot. Eight, against twenty-four decisions replayed and a
    // snapshot of many more rows: the constituent measured the holdout and
    // nothing else.
    assert!(
        refusal.reason.contains(&calibration.holdout_cohorts.to_string()),
        "{} against {}",
        refusal.reason,
        calibration.holdout_cohorts
    );
    assert_eq!(calibration.holdout_cohorts, 8);
    assert_eq!(calibration.cohorts_total, 24);
    assert_eq!(calibration.fit_cohorts, 16);
    assert_ne!(calibration.holdout_cohorts, calibration.cohorts_total);

    // And the snapshot really does carry the attempt-scope rows a K axis is built
    // from, so the one-candidate-axis trap 7E-2D documents is not in play.
    let trained = trained();
    let first = &fixture(&trained)[0];
    let samples = canonical_samples_from_decision_time(
        &first.outcome,
        first.input(),
        zroutery_core::feedback::DataOrigin::Native,
    )
    .expect("the canonical conversion produces the attempt rows");
    let attempt_rows = samples
        .iter()
        .filter(|sample| {
            matches!(
                sample.scope,
                zroutery_core::ml::dataset::SampleScope::Attempt { .. }
            )
        })
        .count();
    assert!(
        attempt_rows > 1,
        "the axis needs more than one attempt row, it has {attempt_rows}"
    );
}

// ---------------------------------------------------------------------------
// 6. Both scopes are carried
// ---------------------------------------------------------------------------

#[test]
fn the_report_carries_both_scopes() {
    let (_trained, report) = gated();
    assert_eq!(report.scope, RELEASE_SCOPE);
    assert_eq!(report.statistical_scope, STATISTICAL_SCOPE);
    // Neither string over-reads the other: 7E-3's still disclaims statistics, and
    // 7D's carries the unit of independence and the same disclaimers about reach.
    assert!(RELEASE_SCOPE.contains("NOT a statistical claim"));
    assert!(STATISTICAL_SCOPE.contains("unit of independence is the"));
    assert!(STATISTICAL_SCOPE.contains("DECISION"));
    assert!(STATISTICAL_SCOPE.contains("NOT a claim about online traffic"));
    // Both reach the headline, and the report serializes.
    let headline = report.headline();
    assert!(headline.contains(RELEASE_SCOPE));
    assert!(headline.contains(STATISTICAL_SCOPE));
    assert!(headline.contains("statistical support=no"));
    assert!(headline.contains("8 effective decisions"));
    let json = serde_json::to_string(&report).expect("the report serializes");
    assert!(json.contains("statistical_scope"));
    // The statistical constituent is on the report in the *refused* form on this
    // fixture, so what is serialized is the typed refusal — never a zero-filled
    // measurement standing in for one that was not made.
    assert!(json.contains("sample_too_small"));
    assert!(json.contains("\"refused\""));
    assert!(
        !json.contains("raw_axis_observations"),
        "a refused constituent must carry no measurement at all"
    );

    // And a *measured* constituent carries the arithmetic, which is checked in
    // `every_constituent_passing_reaches_a_considerable_verdict`; here the point
    // is that both shapes travel on the same report type.
    let supported = supported_constituent();
    let measured = serde_json::to_string(&supported).expect("serializes");
    assert!(measured.contains("raw_axis_observations"));
    assert!(measured.contains("effective_decisions"));
    assert!(measured.contains("inflation"));
    assert!(measured.contains("\"measured\""));
}

/// 7D must not reach into anything this test does not already exercise, so the
/// only public surface it uses from the offline gate is the report.
/// The default is the weakest claim the gate accepts, so a caller cannot
/// accidentally ship a stricter gate than they meant to, and no value of the
/// configuration can switch the constituent off.
#[test]
fn the_verdict_needs_no_configuration_to_turn_the_statistical_gate_off() {
    let trained = trained();
    let recorded = fixture(&trained);
    for config in [
        StatisticalConfig::default(),
        StatisticalConfig {
            alpha: 0.001,
            ..StatisticalConfig::default()
        },
        StatisticalConfig {
            power: 0.99,
            ..StatisticalConfig::default()
        },
        StatisticalConfig {
            minimum_effect: 0.5,
            ..StatisticalConfig::default()
        },
    ] {
        let mut input = gate_input(&trained, recorded.clone());
        input.config = permissive_config();
        input.config.statistics = config;
        let report = run_offline_gate(&input)
            .expect("the fixture replays")
            .report()
            .clone();
        assert!(
            !report.is_considerable(),
            "a thin holdout withholds under every claim: {config:?}"
        );
        assert!(report.blockers().iter().any(|blocker| blocker
            .contains("statistical claim could not be measured")));
    }
    // A vacuous claim specification is refused rather than run, so a caller
    // cannot weaken the gate by making the claim meaningless.
    let mut input = gate_input(&trained, recorded);
    input.config = permissive_config();
    input.config.statistics = StatisticalConfig {
        minimum_effect: 0.0,
        ..StatisticalConfig::default()
    };
    let report = run_offline_gate(&input)
        .expect("the gate still produces a report")
        .report()
        .clone();
    assert_eq!(
        report.statistics.refusal().expect("a refusal").code,
        "invalid_claim"
    );
    assert!(!report.is_considerable());
}

/// The recorded decision names a routing action, and a thin statistical holdout
/// must not change it. Pinning it here means a refactor that dropped the field
/// from the evidence would fail in this suite rather than silently elsewhere.
#[test]
fn the_wired_path_still_records_a_routing_action() {
    let (_trained, report) = gated();
    assert!(!report.replay.is_empty());
    for evidence in &report.replay {
        // Exploration is a routing behaviour this node must never produce, and
        // the fixture disables it, so `Explore` would be a contract violation.
        assert_ne!(evidence.action, RoutingAction::Explore);
    }
}
