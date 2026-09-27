#![cfg(feature = "ml")]

//! Node 7D — the statistical release methodology and attempt-level attribution.
//!
//! The claims this suite has to earn, and the test that earns each:
//!
//! 1. **the sampling unit is the decision, and the raw row count is reported
//!    beside it** — `the_raw_row_count_and_the_effective_decision_count_are_both_in_the_output`,
//!    which asserts the inflation factor equals the arity and that a row-level
//!    interval would have been narrower by `sqrt(K)`;
//! 2. **the machinery ACCEPTS a real effect and REFUSES a null one** —
//!    `the_machinery_accepts_a_real_effect` and `the_machinery_refuses_a_null_effect`,
//!    which print every number rather than summarising it;
//! 3. **the baseline is not a strawman** —
//!    `a_fixed_uninformed_policy_cannot_produce_a_difference_from_itself` and
//!    `a_thirty_eight_percent_accurate_model_is_refused_for_being_too_small`,
//!    the two comparators a no-skill model would be measured against;
//! 4. **the family is stated and corrected** —
//!    `the_family_is_stated_and_the_correction_actually_binds`;
//! 5. **sample-size adequacy refuses with the required n in the message** —
//!    `an_underpowered_sample_is_refused_with_the_required_n`;
//! 6. **attempt-level attribution respects mutual exclusivity** —
//!    `a_misaligned_partition_is_refused`, `an_axis_of_one_candidate_is_refused`,
//!    `a_partition_whose_two_arms_never_disagree_cannot_produce_an_interval`;
//! 7. **bit-reproducibility** —
//!    `a_permuted_partition_produces_the_same_measurement` and
//!    `two_runs_produce_identical_serialized_measurements`;
//! 8. **the verdict is recomputed, never stored** —
//!    `every_criterion_carries_numbers_and_no_boolean`.
//!
//! # The fixtures, and why each is the shape it is
//!
//! Every cohort carries the **full three-candidate axis** and exactly one
//! served candidate, and the winner rotates, so every candidate wins a third of
//! the decisions. That makes the strongest uninformed policy — "always serve
//! whichever candidate wins most" — score exactly `1/3`, which is the honest
//! comparator: a policy with no information about the decision cannot beat it,
//! and a model with no skill ties it exactly.
//!
//! The three models differ only in which slot the emitted distribution is made to
//! rank first, and each is constructed so its discordant cells can be counted on
//! paper:
//!
//! * `skill_pick` is right on even decisions: `n10 = n/3`, `n01 = 0`, effect `1/6`.
//! * `null_pick` alternates between two *fixed* candidates: `n10 = n01 = n/6`,
//!   effect exactly `0`. It is a coin with no reference to the decision, which is
//!   what a router with no information would do.
//! * `tiny_pick` is right two times in five: effect `1/15`, which is a real
//!   effect and is *refused* because it is smaller than the stated minimum
//!   effect. That is the test that the gate is calibrated rather than lenient.
//!
//! Nothing here hand-builds a distribution, a base rate or a K axis: they all
//! come from 7E-2D's own constructors, because 7D does not own them and a
//! fixture that invented them would be testing 7D's fixture rather than 7D's
//! claim.

use std::collections::BTreeSet;

use zroutery_core::ml::attribution::{attribute, AttributionError, CandidateCredit, Independence};
use zroutery_core::ml::calibration::{
    collect_marginal_observations, measure_marginal, CandidateCalibration, CandidateInput,
    CohortContext, DecisionCohort, EmittedDecision, MarginalCalibrator, MarginalView,
    KWayCalibrator, ReliabilityConfig, DEFAULT_PROBABILITY_FLOOR,
};
use zroutery_core::ml::statistics::{
    measure_release_evidence, normal_quantile, EvidenceSupport, FamilyMemberKind, Interval,
    StatisticalConfig, StatisticalInput, StatisticalRelease, StatisticsError, UNMEASURABLE_LABEL,
};
use zroutery_core::outcome::CandidateIdentity;

/// The three-candidate axis every fixture compares.
const AXIS: [(&str, &str); 3] = [("alpha", "prov-a"), ("bravo", "prov-b"), ("charlie", "prov-c")];

/// Probability given to the slot the model should rank first, and to the rest.
///
/// Far enough apart that the argmax is never in doubt, which matters: a fixture
/// whose selection depended on a near-tie would be testing the tie-break.
const BOOSTED: f64 = 0.95;
const BACKGROUND: f64 = 0.02;

fn identity(slot: usize) -> CandidateIdentity {
    CandidateIdentity::new(AXIS[slot].0, AXIS[slot].1)
}

// ---------------------------------------------------------------------------
// The four models
// ---------------------------------------------------------------------------

/// The slot that actually served: uniform, so every candidate wins a third.
fn winner(index: usize) -> usize {
    index % 3
}

/// A model with real skill: it ranks the winner first on every other decision.
///
/// Right half the time against a one-in-three base rate, so its paired risk
/// difference against the best uninformed policy is `1/2 - 1/3 = 1/6`.
fn skill_pick(index: usize) -> usize {
    if index % 2 == 0 {
        winner(index)
    } else {
        (winner(index) + 1) % 3
    }
}

/// A model with no skill: a coin between two fixed candidates.
///
/// It never consults the decision, so over each block of six decisions it is
/// right once where the baseline is wrong and wrong once where the baseline is
/// right, and the paired difference is **exactly zero** — not merely small. This
/// is the honest null: a real effect estimate of 0, not an under-powered one.
fn null_pick(index: usize) -> usize {
    index % 2
}

/// A model with a real but small effect: right thirty-seven times in a hundred.
///
/// Its paired difference is `0.37 * 2/3 - 0.63 * 1/3 = 11/300`, which is above
/// zero and overwhelmingly significant at any sane `n` and still **below the
/// stated minimum effect**. That is the case the gate must refuse for the right
/// reason: the effect is real, the sample is ample, and the effect is too small
/// to be worth releasing for.
fn tiny_pick(index: usize) -> usize {
    if index % 100 < 37 {
        winner(index)
    } else {
        (winner(index) + 1) % 3
    }
}

/// A model that is exactly the best uninformed policy: always slot 0.
///
/// With a uniform winner every fixed policy scores a third, and the content
/// tie-break picks slot 0, so the comparator is the model. The two arms are then
/// identical and there is no difference to measure.
fn fixed_pick(_index: usize) -> usize {
    0
}

// ---------------------------------------------------------------------------
// Fixture assembly, entirely through 7E-2D's constructors
// ---------------------------------------------------------------------------

fn context(index: usize, served: bool) -> CohortContext {
    CohortContext {
        timestamp: 1_700_000_000 + index as i64,
        dialect: "openai".to_string(),
        streaming: false,
        final_status_rank: u8::from(!served),
        failure_class_rank: (!served).then_some(0),
    }
}

/// One cohort over the full axis, with `boosted` ranked first.
fn cohort(index: usize, boosted: usize, served: Option<usize>) -> DecisionCohort {
    let candidates: Vec<CandidateInput> = (0..AXIS.len())
        .map(|slot| CandidateInput::Ranked {
            candidate: identity(slot),
            raw_success_probability: if slot == boosted { BOOSTED } else { BACKGROUND },
        })
        .collect();
    let subject = identity(0);
    DecisionCohort::try_new(context(index, served.is_some()), Some(&subject), candidates, served.map(identity))
        .expect("the fixture cohort is well formed")
}

/// A partition built by a model function, plus 7E-2D's emitted distributions and
/// 7E-2D's unconditional base rate over exactly the same partition.
fn partition(n: usize, pick: fn(usize) -> usize) -> (Vec<DecisionCohort>, Emitted, Marginal) {
    let cohorts: Vec<DecisionCohort> = (0..n)
        .map(|index| cohort(index, pick(index), Some(winner(index))))
        .collect();
    emit(&cohorts)
}

/// The two 7E-2D products the measurement consumes, bundled so a fixture cannot
/// accidentally pair a partition with the wrong one.
struct Emitted {
    distributions: Vec<EmittedDecision>,
}

struct Marginal {
    rows: Vec<CandidateCalibration>,
}

fn emit(cohorts: &[DecisionCohort]) -> (Vec<DecisionCohort>, Emitted, Marginal) {
    // 7E-2D's own uncalibrated joint route, used exactly as 7E-2D uses it for
    // its own uncalibrated comparison, so the distribution under test is one
    // 7E-2D produced.
    let calibrator = KWayCalibrator::uncalibrated();
    let mut distributions = Vec::with_capacity(cohorts.len());
    for cohort in cohorts {
        distributions.push(
            calibrator
                .distribution(cohort, DEFAULT_PROBABILITY_FLOOR)
                .expect("the fixture cohort emits a distribution"),
        );
    }
    let marginal = marginal_calibrator();
    let observations =
        collect_marginal_observations(cohorts, &marginal, DEFAULT_PROBABILITY_FLOOR)
            .expect("the fixture's marginal observations collect");
    let rows = measure_marginal(
        &observations,
        MarginalView::Calibrated,
        &ReliabilityConfig::default(),
    )
    .expect("the fixture's marginal observations measure")
    .per_candidate;
    (
        cohorts.to_vec(),
        Emitted { distributions },
        Marginal { rows },
    )
}

/// A marginal calibrator fitted on a small synthetic set, so
/// `collect_marginal_observations` has the 1-D map it requires. 7E-2D's own
/// `MarginalCalibrator`; nothing here re-implements it.
fn marginal_calibrator() -> MarginalCalibrator {
    let pairs: Vec<(f64, bool)> = (0..64)
        .map(|index| (0.1 + (index % 8) as f64 * 0.1, index % 3 == 0))
        .collect();
    MarginalCalibrator::fit(&pairs, &Default::default(), DEFAULT_PROBABILITY_FLOOR)
        .expect("the fixture's marginal pairs fit")
}

fn measure(
    cohorts: &[DecisionCohort],
    emitted: &Emitted,
    marginal: &Marginal,
    config: StatisticalConfig,
) -> StatisticalRelease {
    measure_release_evidence(&StatisticalInput {
        partition: cohorts,
        emitted: &emitted.distributions,
        marginal: &marginal.rows,
        config,
    })
    .expect("the fixture is measurable")
}

fn try_measure(
    cohorts: &[DecisionCohort],
    emitted: &Emitted,
    marginal: &Marginal,
    config: StatisticalConfig,
) -> Result<StatisticalRelease, StatisticsError> {
    measure_release_evidence(&StatisticalInput {
        partition: cohorts,
        emitted: &emitted.distributions,
        marginal: &marginal.rows,
        config,
    })
}

/// The measured support, owned, so a test can hold it across statements
/// without borrowing a temporary.
fn support_of(release: StatisticalRelease) -> EvidenceSupport {
    release.support().expect("a measured release").clone()
}
/// The three-candidate axis and the unit of independence, in one fixture.
fn sample_of(n: usize) -> (Vec<DecisionCohort>, Emitted, Marginal) {
    partition(n, skill_pick)
}

// ---------------------------------------------------------------------------
// 1. The sampling unit
// ---------------------------------------------------------------------------

#[test]
fn the_raw_row_count_and_the_effective_decision_count_are_both_in_the_output() {
    let n = 4_000usize;
    let (cohorts, emitted, marginal) = sample_of(n);
    let support = measure(&cohorts, &emitted, &marginal, StatisticalConfig::default())
        .support()
        .expect("a measured release")
        .clone();

    // The arithmetic, stated: three candidates in every one of 4000 decisions.
    assert_eq!(support.independence.raw_axis_observations, 3 * n);
    assert_eq!(support.independence.effective_decisions, n);
    assert_eq!(support.independence.unserved_decisions, 0);
    assert_eq!(support.independence.inflation, Some(3.0));
    assert_eq!(
        support.credit.independence,
        Independence {
            raw_axis_observations: 3 * n,
            effective_decisions: n,
            unserved_decisions: 0,
            inflation: Some(3.0),
        },
        "the ledger and the headline report the same arithmetic"
    );
    assert_eq!(support.credit.outcomes.len(), n);
    assert_eq!(support.aggregate.decisions, n);
    assert_eq!(
        support.aggregate.model_only
            + support.aggregate.baseline_only
            + support.aggregate.concordant,
        n,
        "the four cells must reconcile against the decision count"
    );

    // And the consequence, which is the whole point. A row-level analysis would
    // have treated 12000 rows as 12000 independent observations. The standard
    // error it would have reported is smaller by exactly `sqrt(K)`.
    let row_level = (support.aggregate.discordance_rate / (3.0 * n as f64)).sqrt();
    let decision_level = support.aggregate.standard_error;
    assert!(
        decision_level > row_level,
        "a row-level interval would have been narrower: {row_level} against {decision_level}"
    );
    assert!(
        (decision_level / row_level - 3.0_f64.sqrt()).abs() < 1e-9,
        "and narrower by exactly sqrt(K): {} against {}",
        decision_level / row_level,
        3.0_f64.sqrt()
    );

    // The three numbers are all reachable from the serialized measurement, so a
    // reader of the report can do that arithmetic themselves.
    let json = serde_json::to_string(&support).expect("the measurement serializes");
    for key in [
        "raw_axis_observations",
        "effective_decisions",
        "unserved_decisions",
        "inflation",
    ] {
        assert!(json.contains(key), "{key} must be in the output: {json}");
    }
}

// ---------------------------------------------------------------------------
// 2. Anti-vacuity: the numbers, not a summary of them
// ---------------------------------------------------------------------------

/// Print every constituent of a measurement. Printed rather than asserted: this
/// node's product is evidence, and a summary of the evidence is not evidence.
fn report(tag: &str, support: &zroutery_core::ml::statistics::EvidenceSupport) {
    let aggregate = &support.aggregate;
    let magnitude = if aggregate.p_value > 0.0 {
        format!("{:.10}", aggregate.p_value)
    } else {
        format!("exp({:.4})", aggregate.log_p_value)
    };
    println!(
        "{tag}: effect={:.6} se={:.6} interval=[{:.6}, {upper:.6}] level={level} \
         raw_n={raw} effective_n={eff} inflation={inflation} model_only={n10} \
         baseline_only={n01} concordant={con} discordance_rate={psi:.6} p={magnitude} \
         family_size={size} adjusted_p={adjusted:.10} required_n={required}",
        aggregate.effect,
        aggregate.standard_error,
        aggregate.interval.lower,
        upper = aggregate.interval.upper,
        level = aggregate.interval.level,
        raw = support.independence.raw_axis_observations,
        eff = support.independence.effective_decisions,
        inflation = support
            .independence
            .inflation
            .map_or_else(|| "unknown".to_string(), |v| format!("{v:.3}")),
        n10 = aggregate.model_only,
        n01 = aggregate.baseline_only,
        con = aggregate.concordant,
        psi = aggregate.discordance_rate,
        magnitude = magnitude,
        size = support.family.size,
        adjusted = aggregate.adjusted_p_value,
        required = support.required_decisions,
    );
    for member in &support.family.members {
        println!(
            "{tag}: family member {:?} effect={:.6} interval=[{:.6}, {upper:.6}] p={p:.10} \
             adjusted_p={adjusted:.10}",
            member.member,
            member.comparison.effect,
            member.comparison.interval.lower,
            upper = member.comparison.interval.upper,
            p = member.comparison.p_value,
            adjusted = member.comparison.adjusted_p_value,
        );
    }
    for reason in support.reasons() {
        println!("{tag}: withheld because {reason}");
    }
    println!("{tag}: {}", support.headline());
}

#[test]
fn the_machinery_accepts_a_real_effect() {
    let n = 4_000usize;
    let (cohorts, emitted, marginal) = sample_of(n);
    let support = measure(&cohorts, &emitted, &marginal, StatisticalConfig::default())
        .support()
        .cloned()
        .expect("a measured release");
    report("ACCEPT", &support);

    // The claim is supported, and supported for the right reasons.
    assert!(support.is_supported(), "{:?}", support.reasons());
    assert!(support.reasons().is_empty());
    assert!(support.blockers().is_empty());
    let aggregate = &support.aggregate;
    assert!(aggregate.model_only > aggregate.baseline_only);
    assert!(aggregate.p_value <= support.config.alpha);
    assert!(aggregate.adjusted_p_value <= support.config.alpha);

    // The interval excludes the null and the stated minimum, with the bounds in
    // the output.
    assert!(aggregate.interval.excludes(0.0), "{:?}", aggregate.interval);
    assert!(!aggregate.interval.straddles(0.0));
    assert!(aggregate.interval.lower > support.config.minimum_effect);
    assert!(aggregate.interval.level > 0.97);

    // Adequacy, with both numbers in the output.
    assert!(support.independence.effective_decisions >= support.required_decisions);
    assert!(support.required_decisions > 0);

    // The effect is the one the fixture put in, to within the fixture's own
    // arithmetic. `skill_pick` is right on half the decisions, so the paired
    // difference against a one-in-three baseline is `1/6` up to the block
    // boundary, which `n` being a multiple of six makes exact.
    let expected = 1.0 / 6.0;
    assert!(
        (aggregate.effect - expected).abs() < 0.01,
        "the effect should be about {expected}, it is {}",
        aggregate.effect
    );
}

#[test]
fn the_machinery_refuses_a_null_effect() {
    let n = 4_000usize;
    let (cohorts, emitted, marginal) = partition(n, null_pick);
    let release = measure(&cohorts, &emitted, &marginal, StatisticalConfig::default());
    let support = release.support().expect("a measured release");
    report("REFUSE", support);

    // The same partition size, the same family, the same procedure. Only the
    // effect differs, which is the test that the gate reads the effect and not
    // the sample size.
    assert_eq!(support.independence.effective_decisions, n);
    assert!(!support.is_supported(), "{:?}", support.reasons());
    assert!(!release.is_supported());
    assert!(!release.blockers().is_empty());

    // The estimate is *exactly* zero, not merely small: the fixture is balanced
    // by construction, so this is a true null and not an under-powered one.
    assert_eq!(support.aggregate.effect, 0.0);
    assert_eq!(
        support.aggregate.model_only, support.aggregate.baseline_only,
        "the two arms disagree equally often in each direction"
    );
    assert_eq!(support.aggregate.p_value, 1.0);
    assert_eq!(support.aggregate.adjusted_p_value, 1.0);

    // The interval straddles the null, and the refusal says so with the bounds.
    let interval = support.aggregate.interval;
    assert!(interval.straddles(0.0), "{interval:?}");
    assert!(!interval.excludes(0.0));
    // The whole interval sits *below* the stated minimum, which is a stronger
    // refusal than straddling it: the effect is not merely uncertain, it is
    // confidently too small to release for.
    assert!(interval.upper < support.config.minimum_effect, "{interval:?}");
    assert!(interval.lower <= support.config.minimum_effect);
    assert!(
        support
            .reasons()
            .iter()
            .any(|line| line.contains("interval_excludes_the_minimum_effect")),
        "{:?}",
        support.reasons()
    );
    assert!(
        release
            .blockers()
            .iter()
            .any(|blocker| blocker.contains("does not exclude the null")),
        "{:?}",
        release.blockers()
    );

    // Adequacy *is* met. The refusal is about the effect, not about the sample,
    // and a reader can tell the two apart from the reasons.
    assert!(support.independence.effective_decisions >= support.required_decisions);
    assert!(
        !support
            .reasons()
            .iter()
            .any(|line| line.starts_with("sample_size_adequacy")),
        "the refusal must not be a sample-size one: {:?}",
        support.reasons()
    );
}

// ---------------------------------------------------------------------------
// 3. The baseline is not a strawman
// ---------------------------------------------------------------------------

#[test]
fn a_fixed_uninformed_policy_cannot_produce_a_difference_from_itself() {
    // The strongest uninformed policy, measured against itself. The model always
    // ranks slot 0, and with a uniform winner the best fixed policy is also slot 0
    // — so the gate's comparator *is* the model, and the two arms are identical
    // in every decision. There is no difference, so no interval can be computed,
    // and the honest answer is a refusal rather than a zero-width interval that
    // would claim the difference is known to be exactly nothing.
    let n = 200usize;
    let (cohorts, emitted, marginal) = partition(n, fixed_pick);
    let error = try_measure(
        &cohorts,
        &emitted,
        &marginal,
        StatisticalConfig {
            min_decisions: 1,
            ..StatisticalConfig::default()
        },
    )
    .expect_err("a model that is the baseline has no difference to measure");
    assert_eq!(error.code(), "interval_not_computable");
    println!("FIXED POLICY: {error}");
    assert!(
        error.to_string().contains("no sampling variability"),
        "{error}"
    );

    // And the ledger, which is measurable on its own, shows why: the model is
    // credited with exactly as many wins as the comparator scores.
    let ledger = attribute(&cohorts, &emitted.distributions, &marginal.rows)
        .expect("the ledger is buildable");
    let first: &CandidateCredit = ledger.credit_of(&identity(0)).expect("slot 0 is in the axis");
    assert_eq!(first.ranked_first, n);
    assert_eq!(first.ranked_first_and_won, first.served);
    assert!(ledger.outcomes.iter().all(|outcome| outcome.model_scored
        == (outcome.selection == outcome.served)));
}

#[test]
fn a_thirty_eight_percent_accurate_model_is_refused_for_being_too_small() {
    // The gate is calibrated rather than lenient. A model right two times in five
    // has a real, significant effect against the one-in-three baseline, and is
    // refused because the effect is *smaller than the stated minimum*. The
    // sample is ample — 20 000 decisions against a requirement of about 1500 — so
    // the refusal cannot be mistaken for a sample-size one, and the reasons say
    // which requirement failed.
    let n = 20_000usize;
    let (cohorts, emitted, marginal) = partition(n, tiny_pick);
    let release = measure(&cohorts, &emitted, &marginal, StatisticalConfig::default());
    let support = release.support().expect("a measured release");
    report("TOO SMALL", support);

    let interval = support.aggregate.interval;
    assert!(interval.lower > 0.0, "the effect is real: {interval:?}");
    assert!(interval.excludes(0.0));
    assert!(
        !interval.excludes(support.config.minimum_effect),
        "and still too small: {interval:?}"
    );
    assert!(interval.upper < support.config.minimum_effect, "{interval:?}");
    assert!(support.aggregate.p_value < 1e-8, "and significant");
    assert!(support.independence.effective_decisions >= support.required_decisions);
    assert!(
        !support
            .reasons()
            .iter()
            .any(|line| line.starts_with("sample_size_adequacy")),
        "{:?}",
        support.reasons()
    );
    assert!(!support.is_supported());
    assert!(
        support
            .reasons()
            .iter()
            .any(|line| line.contains("interval_excludes_the_minimum_effect")),
        "{:?}",
        support.reasons()
    );

    // The same effect, demanded as small as one point, is not cheap: the minimum
    // effect and the required `n` are one field precisely because a smaller
    // minimum is a *larger* sample, not a weaker claim. So at twenty thousand
    // decisions the one-point claim is now refused for thin evidence instead.
    let relaxed = try_measure(
        &cohorts,
        &emitted,
        &marginal,
        StatisticalConfig {
            minimum_effect: 0.01,
            ..StatisticalConfig::default()
        },
    )
    .expect("a measured release");
    let relaxed = support_of(relaxed);
    assert!(
        relaxed.required_decisions > support.required_decisions,
        "a smaller minimum effect must demand more decisions: {} against {}",
        relaxed.required_decisions,
        support.required_decisions
    );
    assert!(
        !relaxed.is_supported(),
        "and 20000 decisions cannot certify a one-point effect: {:?}",
        relaxed.reasons()
    );

    // With enough decisions for the smaller claim, the same model qualifies. So
    // the gate is not refusing the model; it is refusing the claim at the sample
    // size actually measured, which is the right object to refuse.
    let (many, many_emitted, many_marginal) = partition(60_000, tiny_pick);
    let certified = support_of(measure(
        &many,
        &many_emitted,
        &many_marginal,
        StatisticalConfig {
            minimum_effect: 0.01,
            ..StatisticalConfig::default()
        },
    ));
    assert!(certified.is_supported(), "{:?}", certified.reasons());
    assert!(certified.independence.effective_decisions >= certified.required_decisions);
    assert!(certified.aggregate.interval.lower > 0.01);
}

#[test]
fn the_baseline_is_the_strongest_fixed_policy_and_not_a_rate_maximiser() {
    // A skewed winner: slot 1 wins three times in four. The gate must name the
    // comparator it chose and report its score, so a reader can check the choice
    // rather than trust it. The model ranks the winner first only half the time,
    // which also gives every per-candidate test some discordance — a model that
    // is always right leaves a candidate's own test with no variability at all,
    // and that is a separate refusal this fixture is not about.
    let n = 600usize;
    let cohorts: Vec<DecisionCohort> = (0..n)
        .map(|index| {
            let served = if index % 4 == 3 { 0 } else { 1 };
            let pick = if index % 2 == 0 { served } else { 2 };
            cohort(index, pick, Some(served))
        })
        .collect();
    let (cohorts, emitted, marginal) = emit(&cohorts);
    let support = support_of(measure(&cohorts, &emitted, &marginal, StatisticalConfig {
            min_decisions: 1,
            ..StatisticalConfig::default()
        }));
    let leader: &CandidateCredit = support
        .credit
        .credit_of(&identity(1))
        .expect("slot 1 is in the axis");
    assert_eq!(leader.served, 3 * n / 4);
    assert_eq!(support.baseline.wins, leader.served);
    assert_eq!(support.baseline.candidate, identity(1));
    assert_eq!(support.baseline.decisions, n);
    assert_eq!(support.baseline.candidates_considered, AXIS.len());
    assert_eq!(support.baseline.rate, 0.75);

    // 7E-2D's base rate is carried beside the hit count, and cross-checked
    // against the counts measured here.
    assert_eq!(support.baseline.base_rate_observations, leader.ranked_decisions);
    assert_eq!(support.baseline.unconditional_base_rate, Some(0.75));
    assert!(support.baseline.interval.lower < 0.75);
    assert!(support.baseline.interval.upper > 0.75);
    assert_eq!(support.baseline.interval.level, support.config.level());
}

// ---------------------------------------------------------------------------
// 4. The family is stated and corrected
// ---------------------------------------------------------------------------

#[test]
fn the_family_is_stated_and_the_correction_actually_binds() {
    let n = 4_000usize;
    let (cohorts, emitted, marginal) = sample_of(n);
    let support = support_of(measure(&cohorts, &emitted, &marginal, StatisticalConfig::default()));

    // The family is the aggregate plus one test per candidate ranked first.
    assert_eq!(support.family.size, 1 + AXIS.len());
    assert_eq!(support.family.members.len(), support.family.size);
    assert!(support.family.rule.contains("aggregate"));
    assert!(support.family.rule.contains("ranked first"));
    assert!(support.family.correction.contains("holm"));
    assert!(matches!(
        support.family.members[0].member,
        FamilyMemberKind::Aggregate
    ));
    assert_eq!(
        support.family.members[0].comparison.p_value,
        support.aggregate.p_value,
        "the aggregate is the family's first member, not a copy of it"
    );
    assert_eq!(
        support.family.members[0].comparison.adjusted_p_value,
        support.aggregate.adjusted_p_value
    );

    // The correction is doing something observable rather than being decorative:
    // the aggregate is the smallest p, so Holm is Bonferroni-exact at it.
    let smallest = support
        .family
        .members
        .iter()
        .map(|member| member.comparison.p_value)
        .fold(f64::INFINITY, f64::min);
    assert_eq!(support.aggregate.p_value, smallest);
    assert!(
        (support.aggregate.adjusted_p_value - support.family.size as f64 * support.aggregate.p_value)
            .abs()
            < 1e-15,
        "the smallest p is Bonferroni-exact: {} against {}",
        support.aggregate.adjusted_p_value,
        support.family.size as f64 * support.aggregate.p_value
    );

    // Every member's adjusted value is at least its raw value, which is the only
    // direction a correction may move in.
    for member in &support.family.members {
        assert!(
            member.comparison.adjusted_p_value >= member.comparison.p_value - 1e-15,
            "{:?}",
            member.member
        );
        assert!(member.comparison.p_value.is_finite());
        assert!(member.comparison.effect.is_finite());
    }

    // A family above the ceiling is refused rather than silently truncated, which
    // would under-correct.
    let error = try_measure(
        &cohorts,
        &emitted,
        &marginal,
        StatisticalConfig {
            max_family: 2,
            ..StatisticalConfig::default()
        },
    )
    .expect_err("a family above the ceiling is refused");
    assert_eq!(error.code(), "family_not_enumerable");
    assert!(
        error.to_string().contains("above the ceiling of 2"),
        "{error}"
    );
    let refused = StatisticalRelease::Refused(zroutery_core::ml::statistics::StatisticalRefusal::from(
        &error,
    ));
    assert!(!refused.is_supported());
    assert_eq!(refused.blockers(), vec![UNMEASURABLE_LABEL]);
}

// ---------------------------------------------------------------------------
// 5. Sample-size adequacy
// ---------------------------------------------------------------------------

#[test]
fn an_underpowered_sample_is_refused_with_the_required_n() {
    // The same real effect as the accept case, on a partition a fraction of the
    // size. The floor of thirty is cleared, so the measurement *happens* and the
    // adequacy criterion is what withholds — the distinction that matters, since
    // a reader is told the evidence is too thin rather than that it could not be
    // looked at.
    let n = 200usize;
    let (cohorts, emitted, marginal) = sample_of(n);
    let release = measure(&cohorts, &emitted, &marginal, StatisticalConfig::default());
    assert!(matches!(release, StatisticalRelease::Measured(_)));
    let support = release.support().expect("a measured release");
    report("UNDERPOWERED", support);
    assert!(!support.is_supported(), "{:?}", support.reasons());

    // Both numbers are in the output, and the reason names the requirement.
    assert!(support.required_decisions > n);
    assert_eq!(support.independence.effective_decisions, n);
    let reason = support
        .reasons()
        .into_iter()
        .find(|line| line.starts_with("sample_size_adequacy"))
        .expect("the adequacy criterion must be the reason");
    println!("UNDERPOWERED reason: {reason}");
    assert!(reason.contains(&n.to_string()), "{reason}");
    assert!(
        reason.contains(&support.required_decisions.to_string()),
        "{reason}"
    );
    assert!(reason.contains("0.8 power"), "{reason}");
    assert!(
        support
            .blockers()
            .iter()
            .any(|blocker| blocker.contains("too few decisions")),
        "{:?}",
        support.blockers()
    );

    // The required n is not a guess: it is the power formula's own answer, and a
    // function of the discordance rate the partition actually shows.
    let z_two_sided = normal_quantile(support.config.level());
    let z_power = normal_quantile(support.config.power);
    let expected = (((z_two_sided + z_power).powi(2) * support.aggregate.discordance_rate)
        / support.config.minimum_effect.powi(2))
    .ceil() as usize;
    assert_eq!(support.required_decisions, expected.max(1));

    // The p-value is in the measurement; the criterion that withholds on it is in
    // the reasons. Both are on the report, which is the point.
    let json = serde_json::to_string(support).expect("the measurement serializes");
    assert!(json.contains("required_decisions"), "{json}");
    assert!(json.contains("\"p_value\""), "{json}");
    let reasons = serde_json::to_string(&support.reasons()).expect("reasons serialize");
    assert!(reasons.contains("sample_size_adequacy"), "{reasons}");

    // A partition below the hard floor is refused outright, with the same two
    // numbers, before the measurement is even attempted.
    let (few, few_emitted, few_marginal) = sample_of(12);
    let error = try_measure(
        &few,
        &few_emitted,
        &few_marginal,
        StatisticalConfig::default(),
    )
    .expect_err("twelve decisions is below the hard floor");
    assert_eq!(error.code(), "sample_too_small");
    let message = error.to_string();
    println!("UNDERPOWERED (floor): {message}");
    assert!(message.contains("12 effective decisions"), "{message}");
    assert!(message.contains("30 are required"), "{message}");
}

// ---------------------------------------------------------------------------
// 6. Attempt-level attribution
// ---------------------------------------------------------------------------

#[test]
fn a_misaligned_partition_is_refused() {
    // `DecisionCohort::try_new` validates the served identity against the axis,
    // so a served identity cannot be made absent from its own axis by
    // construction — which is the first half of "refuse rather than guess", and
    // is the accepted 7E-2A type doing it. The second half is a distribution
    // that is not this decision's, which is produced by handing the measurement a
    // permuted list. Both are refusals, and neither substitutes anything.
    let (cohorts, emitted, marginal) = sample_of(60);

    let mut swapped = emitted.distributions.clone();
    swapped.swap(0, 1);
    let error = try_measure(
        &cohorts,
        &Emitted {
            distributions: swapped,
        },
        &marginal,
        StatisticalConfig {
            min_decisions: 1,
            ..StatisticalConfig::default()
        },
    )
    .expect_err("a misaligned partition is refused");
    assert_eq!(error.code(), "partition_misaligned");
    let message = error.to_string();
    println!("MISALIGNED: {message}");
    assert!(
        message.contains("is not the distribution for partition row"),
        "{message}"
    );

    // A distribution list of the wrong length is refused as misaligned too,
    // rather than being zipped against a differently sized axis.
    let error = try_measure(
        &cohorts,
        &Emitted {
            distributions: emitted.distributions[..59].to_vec(),
        },
        &marginal,
        StatisticalConfig {
            min_decisions: 1,
            ..StatisticalConfig::default()
        },
    )
    .expect_err("a short distribution list is refused");
    assert_eq!(error.code(), "partition_misaligned");

    // And an empty partition is a typed refusal, not an empty measurement.
    let error = try_measure(
        &[],
        &Emitted {
            distributions: Vec::new(),
        },
        &Marginal {
            rows: Vec::new(),
        },
        StatisticalConfig::default(),
    )
    .expect_err("an empty partition is refused");
    assert_eq!(error.code(), "empty_partition");
}

#[test]
fn an_axis_of_one_candidate_is_refused() {
    // A one-candidate axis cannot be ranked: "the distribution ranked the winner
    // first" is true in every decision by construction and would otherwise be
    // reported as perfect skill.
    let singleton = DecisionCohort::try_new(
        context(0, true),
        Some(&identity(0)),
        vec![CandidateInput::Ranked {
            candidate: identity(0),
            raw_success_probability: 0.5,
        }],
        Some(identity(0)),
    )
    .expect("a one-candidate cohort is well formed for 7E-2D");
    let (cohorts, emitted, marginal) = emit(std::slice::from_ref(&singleton));

    let error = try_measure(
        &cohorts,
        &emitted,
        &marginal,
        StatisticalConfig::default(),
    )
    .expect_err("an axis of one cannot be ranked");
    assert_eq!(error.code(), "degenerate_axis");
    assert!(error.to_string().contains("arity 1"), "{error}");

    // The attribution layer refuses it on its own too, with the same code and
    // the decision it is about.
    let direct = attribute(&cohorts, &emitted.distributions, &marginal.rows)
        .expect_err("the ledger refuses a degenerate axis");
    assert_eq!(direct.code(), "degenerate_axis");
    assert!(matches!(direct, AttributionError::DegenerateAxis { arity: 1, .. }));
    assert!(direct.to_string().contains("axis of arity 1"), "{direct}");
}

#[test]
fn a_partition_whose_two_arms_never_disagree_cannot_produce_an_interval() {
    // Every decision is served by the candidate the model ranked first, so the
    // two arms agree everywhere. A zero-width interval would be a claim that the
    // effect is known to be exactly nothing, which is not what zero discordant
    // pairs says: it says the two arms are the same function.
    let n = 60usize;
    let cohorts: Vec<DecisionCohort> = (0..n)
        .map(|index| {
            let winner = index % 3;
            cohort(index, winner, Some(winner))
        })
        .collect();
    let (cohorts, emitted, marginal) = emit(&cohorts);
    let error = try_measure(
        &cohorts,
        &emitted,
        &marginal,
        StatisticalConfig {
            min_decisions: 1,
            ..StatisticalConfig::default()
        },
    )
    .expect_err("no discordance means no interval");
    assert_eq!(error.code(), "interval_not_computable");
    assert!(
        error.to_string().contains("no sampling variability"),
        "{error}"
    );
}

#[test]
fn a_null_or_absent_baseline_is_refused() {
    // Nobody won anything, so every fixed policy scores zero and there is no
    // comparator to beat. This is the null-baseline case.
    let n = 60usize;
    let cohorts: Vec<DecisionCohort> = (0..n)
        .map(|index| cohort(index, 0, None))
        .collect();
    let (cohorts, emitted, marginal) = emit(&cohorts);
    let error = try_measure(
        &cohorts,
        &emitted,
        &marginal,
        StatisticalConfig::default(),
    )
    .expect_err("an unattributed partition is refused");
    assert_eq!(error.code(), "sample_too_small");
    assert!(
        error
            .to_string()
            .contains("no decision in the partition had anybody served"),
        "{error}"
    );

    // The ledger reports the unattributed decisions rather than dropping them
    // silently, and the effective n is zero rather than the raw row count.
    let ledger = attribute(&cohorts, &emitted.distributions, &marginal.rows)
        .expect("the ledger is buildable");
    assert_eq!(ledger.independence.effective_decisions, 0);
    assert_eq!(ledger.independence.unserved_decisions, n);
    assert_eq!(ledger.independence.raw_axis_observations, 3 * n);
    assert_eq!(ledger.outcomes.len(), 0);
    assert_eq!(ledger.independence.inflation, None);
    for row in &ledger.candidates {
        assert_eq!(row.served, 0);
        assert_eq!(row.ranked_first_and_won, 0);
    }

    // The absent-baseline case: 7E-2D's row is dropped for a candidate that
    // carried a prediction, and refused rather than substituted with a
    // self-computed rate.
    let (cohorts, emitted, marginal) = sample_of(60);
    let trimmed: Vec<CandidateCalibration> = marginal
        .rows
        .iter()
        .filter(|row| row.candidate != identity(1))
        .cloned()
        .collect();
    let error = try_measure(
        &cohorts,
        &emitted,
        &Marginal { rows: trimmed },
        StatisticalConfig {
            min_decisions: 1,
            ..StatisticalConfig::default()
        },
    )
    .expect_err("a missing marginal row is refused");
    assert_eq!(error.code(), "baseline_unavailable");
    assert!(error.to_string().contains("prov-b/bravo"), "{error}");
}

#[test]
fn a_non_finite_measurement_is_refused() {
    // A non-finite *measurement* is a typed refusal in its own right, with the
    // offending value and the context it appeared in, and it is never a default.
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let error = StatisticsError::NonFiniteMeasurement {
            context: "the paired risk difference",
            value,
        };
        assert_eq!(error.code(), "non_finite_measurement");
        assert!(error.to_string().contains("the paired risk difference"));
        let refused = StatisticalRelease::from(&error);
        assert!(!refused.is_supported());
        assert_eq!(refused.blockers(), vec![UNMEASURABLE_LABEL]);
        assert_eq!(refused.reasons(), vec![format!("non_finite_measurement: {error}")]);
    }

    // Where a non-finite would actually come from is 7E-2D's own guard, and that
    // guard is consumed rather than duplicated: a probability outside `[0, 1]`
    // is refused by the distribution constructor.
    let broken = DecisionCohort::try_new(
        context(0, true),
        Some(&identity(0)),
        (0..AXIS.len())
            .map(|slot| CandidateInput::Ranked {
                candidate: identity(slot),
                raw_success_probability: if slot == 0 { 1.5 } else { 0.2 },
            })
            .collect(),
        Some(identity(0)),
    )
    .expect("the cohort itself is well formed");
    let refusal = KWayCalibrator::uncalibrated().distribution(&broken, DEFAULT_PROBABILITY_FLOOR);
    let carried = refusal.expect_err("7E-2D refuses the out-of-range probability");
    println!("NON-FINITE: carried as {carried}");
    assert!(carried.to_string().contains("outside [0, 1]"), "{carried}");
    assert!(carried.to_string().contains("1.5"), "{carried}");
}

// ---------------------------------------------------------------------------
// 7. Bit-reproducibility
// ---------------------------------------------------------------------------

#[test]
fn a_permuted_partition_produces_the_same_measurement() {
    // The test that actually bites. The same *set* of decisions, handed to the
    // measurement in a different order, must produce an identical measurement
    // down to every float, because the ledger's rows are emitted in content
    // order and every accumulated quantity is an integer count. If anything in
    // the walk let a partition's order or a container's iteration order reach a
    // sum, this fails.
    let (cohorts, emitted, marginal) = sample_of(300);

    let order: Vec<usize> = (0..cohorts.len()).rev().collect();
    let permuted: Vec<DecisionCohort> = order.iter().map(|i| cohorts[*i].clone()).collect();
    let permuted_emitted: Vec<EmittedDecision> =
        order.iter().map(|i| emitted.distributions[*i].clone()).collect();

    let straight = measure(&cohorts, &emitted, &marginal, StatisticalConfig::default());
    let shuffled = measure(
        &permuted,
        &Emitted {
            distributions: permuted_emitted,
        },
        &marginal,
        StatisticalConfig::default(),
    );
    assert_eq!(
        serde_json::to_string(&straight).expect("serializes"),
        serde_json::to_string(&shuffled).expect("serializes"),
        "a measurement that depends on the order decisions arrive in is not a measurement"
    );
    assert_eq!(straight, shuffled);

    // And 7E-2D's marginal rows may arrive in any order: this node keys on
    // identity, not on index, so 7E-2D's first-observation row order cannot
    // reach the measurement.
    let reversed = Marginal {
        rows: marginal.rows.iter().rev().cloned().collect(),
    };
    let reordered = measure(&cohorts, &emitted, &reversed, StatisticalConfig::default());
    assert_eq!(
        serde_json::to_string(&straight).expect("serializes"),
        serde_json::to_string(&reordered).expect("serializes"),
        "7E-2D's marginal row order must not reach the measurement"
    );
}

#[test]
fn two_runs_produce_identical_serialized_measurements() {
    let (cohorts, emitted, marginal) = sample_of(300);
    let config = StatisticalConfig::default();
    let left = measure(&cohorts, &emitted, &marginal, config);
    let right = measure(&cohorts, &emitted, &marginal, config);
    assert_eq!(left, right);
    assert_eq!(
        serde_json::to_string(&left).expect("serializes"),
        serde_json::to_string(&right).expect("serializes"),
        "a gate whose own output moves is a gate nobody can reason about"
    );
    assert_eq!(left.headline(), right.headline());
    assert_eq!(left.reasons(), right.reasons());
    assert_eq!(left.blockers(), right.blockers());
}

#[test]
fn the_ledger_is_emitted_in_content_order_and_reconciles_with_the_decisions() {
    let (cohorts, emitted, marginal) = sample_of(120);
    let ledger = attribute(&cohorts, &emitted.distributions, &marginal.rows)
        .expect("the ledger is buildable");
    let labels: Vec<String> = ledger.candidates.iter().map(CandidateCredit::label).collect();
    assert_eq!(
        labels,
        vec!["prov-a/alpha", "prov-b/bravo", "prov-c/charlie"],
        "rows are sorted by (provider, model), not by first appearance"
    );
    // The argmax identity: one selection per attributed decision.
    assert!(ledger.selections_total_is_the_decision_count());
    assert_eq!(ledger.selections_total(), ledger.decisions_with_a_selection);
    assert_eq!(ledger.selections_total(), ledger.independence.effective_decisions);
    assert_eq!(ledger.observed_arities, vec![3]);
    assert_eq!(ledger.axis().len(), AXIS.len());
    // The credit sums to the number of decisions the model got right, so the
    // per-candidate view and the decision-level view cannot disagree.
    assert_eq!(
        ledger
            .candidates
            .iter()
            .map(|row| row.ranked_first_and_won)
            .sum::<usize>(),
        ledger
            .outcomes
            .iter()
            .filter(|outcome| outcome.model_scored)
            .count()
    );
    // Every candidate is in every decision, and every one of them was ranked.
    for row in &ledger.candidates {
        assert_eq!(row.axis_decisions, 120);
        assert_eq!(row.ranked_decisions, 120);
        assert_eq!(row.base_rate_observations, 120);
        assert_eq!(row.served, 40, "a uniform winner gives each a third");
        assert!((row.unconditional_base_rate.expect("a rate") - 1.0 / 3.0).abs() < 1e-12);
    }
    assert_eq!(
        ledger
            .candidates
            .iter()
            .map(|row| row.served)
            .sum::<usize>(),
        120,
        "the wins across the axis are a composition of unity, not independent"
    );
}

#[test]
fn the_tie_in_a_flat_distribution_is_broken_by_content_and_not_by_position() {
    // Every candidate carries the same probability, so the argmax is a tie across
    // the whole axis. It must resolve to the content-first candidate in every
    // decision, whatever order the axis was built in, or a decision would be
    // credited to whichever slot happened to come first.
    let n = 120usize;
    let cohorts: Vec<DecisionCohort> = (0..n)
        .map(|index| {
            DecisionCohort::try_new(
                context(index, true),
                Some(&identity(0)),
                (0..AXIS.len())
                    .map(|slot| CandidateInput::Ranked {
                        candidate: identity(slot),
                        raw_success_probability: 0.5,
                    })
                    .collect(),
                Some(identity(2)),
            )
            .expect("a flat cohort is well formed")
        })
        .collect();
    let (cohorts, emitted, marginal) = emit(&cohorts);
    let ledger = attribute(&cohorts, &emitted.distributions, &marginal.rows)
        .expect("the ledger is buildable");
    assert!(ledger.outcomes.iter().all(|outcome| !outcome.model_scored));
    for outcome in &ledger.outcomes {
        assert_eq!(
            outcome.selection.as_ref().map(CandidateIdentity::model),
            Some(AXIS[0].0),
            "a total tie resolves to the content-first candidate"
        );
    }
    let first: &CandidateCredit = ledger.credit_of(&identity(0)).expect("slot 0 is in the axis");
    assert_eq!(first.ranked_first, n);
    assert_eq!(first.ranked_first_and_won, 0);
    assert_eq!(first.precision(), Some(0.0));
    assert_eq!(first.selection_rate(), 1.0);
    let never = ledger.credit_of(&identity(1)).expect("slot 1 is in the axis");
    assert_eq!(never.ranked_first, 0);
    assert_eq!(never.precision(), None, "never ranked first, so no precision");
}

// ---------------------------------------------------------------------------
// 8. The verdict is recomputed, never stored
// ---------------------------------------------------------------------------

#[test]
fn every_criterion_carries_numbers_and_no_boolean() {
    let (cohorts, emitted, marginal) = sample_of(600);
    let support = support_of(measure(
        &cohorts,
        &emitted,
        &marginal,
        StatisticalConfig::default(),
    ));
    let config = support.config;

    // No criterion carries a boolean that could disagree with the arithmetic. A
    // criterion is a measurement; whether it is met is a function of the
    // measurement and the claim specification, recomputed on every call.
    for criterion in support.criteria() {
        let json = serde_json::to_string(&criterion).expect("a criterion serializes");
        for forbidden in ["satisfied", "passes", "ok", "holds", "met"] {
            assert!(
                !json.contains(forbidden),
                "a criterion must not serialise `{forbidden}`: {json}"
            );
        }
        assert!(criterion.name().len() > 5);
        assert!(!criterion.detail(&config).is_empty());
    }
    // The one property that matters: the verdict is exactly "every criterion is
    // met", recomputed.
    let all = support
        .criteria()
        .iter()
        .all(|criterion| criterion.satisfied(&config));
    assert_eq!(all, support.is_supported());
    assert_eq!(support.reasons().is_empty(), support.is_supported());
    assert_eq!(support.blockers().is_empty(), support.is_supported());

    // The levels and the family are stated, not implied, and re-derivable.
    assert!(support.z_two_sided > 1.9 && support.z_two_sided < 2.0);
    assert!(support.z_power > 0.84 && support.z_power < 0.85);
    assert_eq!(support.family.size, support.family.members.len());
    assert_eq!(support.aggregate.interval.level, config.level());
    assert_eq!(support.baseline.interval.level, config.level());
    assert!((config.level() - 0.975).abs() < 1e-15, "the level is two-sided");

    // Demanding a stronger claim withholds, and demanding it of a withheld
    // verdict cannot resurrect it. Both a measured refusal and an outright
    // measurement refusal count, because both are the gate withholding.
    let (cohorts, emitted, marginal) = sample_of(4_000);
    for stricter in [
        StatisticalConfig {
            minimum_effect: 0.90,
            ..StatisticalConfig::default()
        },
        StatisticalConfig {
            min_decisions: 1_000_000,
            ..StatisticalConfig::default()
        },
    ] {
        let release = try_measure(&cohorts, &emitted, &marginal, stricter)
            .unwrap_or_else(|error| StatisticalRelease::from(&error));
        assert!(
            !release.is_supported(),
            "a stronger claim must withhold: {stricter:?}"
        );
        assert!(!release.blockers().is_empty());
        let reasons = release.reasons();
        assert_eq!(reasons.len(), release.blockers().len());
        for line in &reasons {
            assert!(!line.is_empty());
        }
    }

    // A level so demanding that the two-sided confidence rounds to one is refused
    // at the configuration, not downstream as a non-finite measurement: an
    // interval with no finite upper bound is not an interval.
    let absurd = try_measure(
        &cohorts,
        &emitted,
        &marginal,
        StatisticalConfig {
            alpha: 1e-30,
            ..StatisticalConfig::default()
        },
    )
    .expect_err("a level that rounds to one is refused");
    assert_eq!(absurd.code(), "invalid_claim");
    assert!(
        absurd.to_string().contains("no upper bound"),
        "{absurd}"
    );
}

#[test]
fn a_refusal_is_never_reported_as_support() {
    // Every refusal path, checked for the same property: `is_supported` is false,
    // the refusal is visible, and it is reported with a stable code.
    for (code, error) in [
        (
            "invalid_claim",
            StatisticsError::InvalidClaim {
                field: "alpha",
                value: 0.0,
                reason: "a test",
            },
        ),
        (
            "sample_too_small",
            StatisticsError::SampleTooSmall {
                observed: 1,
                required: 2,
                reason: "a test",
            },
        ),
        (
            "family_not_enumerable",
            StatisticsError::FamilyNotEnumerable {
                reason: "a test".to_string(),
            },
        ),
        (
            "interval_not_computable",
            StatisticsError::IntervalNotComputable {
                reason: "a test".to_string(),
            },
        ),
        (
            "non_finite_measurement",
            StatisticsError::NonFiniteMeasurement {
                context: "a test",
                value: f64::INFINITY,
            },
        ),
        (
            "baseline_unavailable",
            StatisticsError::BaselineUnavailable {
                reason: "a test".to_string(),
            },
        ),
        (
            "ledger_inconsistent",
            StatisticsError::LedgerInconsistent {
                detail: "a test".to_string(),
            },
        ),
        (
            "empty_partition",
            StatisticsError::Attribution(Box::new(AttributionError::EmptyPartition)),
        ),
        (
            "degenerate_axis",
            StatisticsError::Attribution(Box::new(AttributionError::DegenerateAxis {
                cohort: "a test".to_string(),
                arity: 1,
            })),
        ),
        (
            "served_absent_from_axis",
            StatisticsError::Attribution(Box::new(AttributionError::ServedAbsentFromAxis {
                cohort: "a test".to_string(),
                served: "prov-a/alpha".to_string(),
            })),
        ),
        (
            "multiple_served_in_one_decision",
            StatisticsError::Attribution(Box::new(
                AttributionError::MultipleServedInOneDecision {
                    cohort: "a test".to_string(),
                    winners: 2,
                },
            )),
        ),
        (
            "partition_misaligned",
            StatisticsError::Attribution(Box::new(AttributionError::PartitionMisaligned {
                index: 0,
                expected: "a test".to_string(),
                emitted: "b test".to_string(),
            })),
        ),
        (
            "baseline_disagrees",
            StatisticsError::Attribution(Box::new(AttributionError::BaselineDisagrees {
                candidate: "a test".to_string(),
                theirs: 1,
                their_served: 1,
                ours: 2,
                our_served: 1,
            })),
        ),
    ] {
        assert_eq!(error.code(), code);
        let release = StatisticalRelease::from(&error);
        assert!(!release.is_supported(), "{code} must never be support");
        assert!(release.support().is_none());
        let refusal = release.refusal().expect("a refusal");
        assert_eq!(refusal.code, code);
        assert_eq!(release.blockers(), vec![UNMEASURABLE_LABEL]);
        assert_eq!(release.reasons(), vec![format!("{code}: {error}")]);
        assert!(release.headline().contains("refused"), "{}", release.headline());
    }
}

#[test]
fn the_baseline_disagreement_cross_check_actually_fires() {
    // The cross-check against 7E-2D's base rate is a real check, not decoration:
    // perturbing 7E-2D's own counts is refused rather than reconciled.
    let (cohorts, emitted, marginal) = sample_of(80);
    let mut perturbed = marginal.rows.clone();
    perturbed[0].served += 1;
    let error = try_measure(
        &cohorts,
        &emitted,
        &Marginal {
            rows: perturbed.clone(),
        },
        StatisticalConfig {
            min_decisions: 1,
            ..StatisticalConfig::default()
        },
    )
    .expect_err("a base rate that disagrees with the counts is refused");
    assert_eq!(error.code(), "baseline_disagrees");
    assert!(error.to_string().contains("7E-2D observations"), "{error}");

    perturbed[0].served -= 1;
    perturbed[0].observations += 3;
    let error = try_measure(
        &cohorts,
        &emitted,
        &Marginal { rows: perturbed },
        StatisticalConfig {
            min_decisions: 1,
            ..StatisticalConfig::default()
        },
    )
    .expect_err("a base rate over the wrong observations is refused");
    assert_eq!(error.code(), "baseline_disagrees");
}

#[test]
fn an_interval_that_cannot_be_computed_is_refused_not_defaulted() {
    // `Interval` has no other constructor: it is two bounds and a level, and the
    // only way to fill it is the measurement. So there is no path that smuggles
    // in a zero-width or all-zero interval. What this pins is the reader-visible
    // consequence: an interval always states its own level, and `straddles` and
    // `excludes` can never both be true about the same null.
    for (lower, upper) in [(0.0_f64, 1.0_f64), (-0.5, 0.5), (0.2, 0.2), (0.3, 0.1)] {
        let interval = Interval {
            lower,
            upper,
            level: 0.95,
        };
        assert_eq!(interval.excludes(0.0), !interval.straddles(0.0));
    }
    let (cohorts, emitted, marginal) = sample_of(200);
    let support = support_of(measure(&cohorts, &emitted, &marginal, StatisticalConfig {
            min_decisions: 1,
            ..StatisticalConfig::default()
        }));
    for member in &support.family.members {
        let interval = member.comparison.interval;
        assert!(interval.lower.is_finite() && interval.upper.is_finite());
        assert!(interval.lower <= interval.upper);
        assert!(interval.level > 0.0 && interval.level < 1.0);
    }
    assert!(support.baseline.interval.lower.is_finite());
}

#[test]
fn the_set_of_credited_candidates_matches_the_set_the_model_ranked_first() {
    // The credit ledger and the family are two views of the same fact and must not
    // disagree: a candidate credited with a ranking appears in the family, and one
    // absent from the family was never ranked first.
    let (cohorts, emitted, marginal) = sample_of(200);
    let support = support_of(measure(&cohorts, &emitted, &marginal, StatisticalConfig {
            min_decisions: 1,
            ..StatisticalConfig::default()
        }));
    let ranked: BTreeSet<String> = support
        .credit
        .candidates
        .iter()
        .filter(|row| row.ranked_first > 0)
        .map(CandidateCredit::label)
        .collect();
    let members: BTreeSet<String> = support
        .family
        .members
        .iter()
        .filter_map(|member| match &member.member {
            FamilyMemberKind::Candidate(identity) => Some(format!(
                "{}/{}",
                identity.provider(),
                identity.model()
            )),
            FamilyMemberKind::Aggregate => None,
        })
        .collect();
    assert_eq!(ranked, members);
    assert_eq!(members.len() + 1, support.family.size);
}
