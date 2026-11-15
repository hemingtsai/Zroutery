use zroutery_core::ml::calibration::{
    measure_emitted, project_cohorts, run_calibration, AcceptanceTolerances, CalibrationMeasure,
    CalibrationVerdict, CandidateCalibration, EmittedDecision, KWayCalibrator, ReliabilityBin,
    ReliabilityConfig, DEFAULT_PROBABILITY_FLOOR,
};
use zroutery_core::ml::dataset::OutcomeTrainingSample;
use zroutery_core::ml::model::RoutingModel;

use super::support::*;

fn uncalibrated_measure(snapshot: &[OutcomeTrainingSample], table: &[u16]) -> CalibrationMeasure {
    uncalibrated_measure_with(snapshot, &ScriptedModel::new(table))
}

fn uncalibrated_measure_with(
    snapshot: &[OutcomeTrainingSample],
    model: &dyn RoutingModel,
) -> CalibrationMeasure {
    let cohorts =
        project_cohorts(snapshot, model, DEFAULT_PROBABILITY_FLOOR).expect("projection succeeds");
    let identity = KWayCalibrator::uncalibrated();
    let emitted: Vec<EmittedDecision> = cohorts
        .iter()
        .map(|cohort| identity.distribution(cohort, DEFAULT_PROBABILITY_FLOOR))
        .collect::<Result<Vec<_>, _>>()
        .expect("emission succeeds");
    measure_emitted(&emitted, &ReliabilityConfig::default()).expect("measurement succeeds")
}

fn candidate_row<'a>(measure: &'a CalibrationMeasure, provider: &str) -> &'a CandidateCalibration {
    measure
        .per_candidate
        .iter()
        .find(|row| row.candidate.provider() == provider)
        .unwrap_or_else(|| panic!("no per-candidate row for {provider}"))
}

// ---------------------------------------------------------------------------
// GATE 2 — calibration metrics
// ---------------------------------------------------------------------------

/// GATE 2: the headline measurement is over the FINAL emitted vector, and it is
/// close to a hand-computed truth.
///
/// The fixture is built so the truth can be written down. The winner cycle makes
/// the holdout's 40 decisions carry exactly 8 alpha wins, 16 bravo wins and 8
/// charlie wins, so the empirical per-candidate truth is `0.20 / 0.40 / 0.20`.
/// The emitted vector is asserted against the fitted numbers, and the verdict
/// against the ceilings, with no threshold chosen after seeing the result.
#[test]
fn the_final_vector_is_measured_and_is_close_to_the_hand_computed_truth() {
    let snapshot = render(&main_fixture(200), 11);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let report = outcome.report();

    assert_eq!(report.cohorts_total, 200);
    assert_eq!(report.fit_cohorts, 160);
    assert_eq!(report.holdout_cohorts, 40);
    assert_eq!(report.holdout_attributed_cohorts, 32);
    assert_eq!(report.ranked_observations, 120);

    let rows = &report.final_vector.per_candidate;
    assert_eq!(rows.len(), 3, "one row per candidate, bin-free");
    for (provider, expected_served) in [(ALPHA.0, 8_usize), (BRAVO.0, 16), (CHARLIE.0, 8)] {
        let row = candidate_row(&report.final_vector, provider);
        assert_eq!(row.observations, 40, "{provider} is in every decision");
        assert_eq!(row.served, expected_served, "{provider} served count");
    }
    assert!((candidate_row(&report.final_vector, BRAVO.0).observed_frequency - 0.40).abs() < 1e-12);
    assert!((candidate_row(&report.final_vector, ALPHA.0).observed_frequency - 0.20).abs() < 1e-12);
    assert!(
        (candidate_row(&report.final_vector, CHARLIE.0).observed_frequency - 0.20).abs() < 1e-12
    );

    // The emitted vector itself, axis for axis.
    let emitted = outcome.holdout_distributions()[0]
        .distribution()
        .probabilities()
        .to_vec();
    assert_eq!(emitted.len(), 3);
    assert!(
        (emitted[0] - 0.2487).abs() < 0.01,
        "alpha mass drifted from the fitted 0.2487: {emitted:?}"
    );
    assert!(
        (emitted[1] - 0.5032).abs() < 0.01,
        "bravo mass drifted from the fitted 0.5032: {emitted:?}"
    );
    assert!(
        (emitted[2] - 0.2481).abs() < 0.01,
        "charlie mass drifted from the fitted 0.2481: {emitted:?}"
    );

    // The summary errors over the final vector.
    let ece = report.final_vector.expected_calibration_error();
    assert!(
        ece < 0.07,
        "final vector ece {ece} is worse than the fit achieves"
    );
    assert!(report.final_vector.maximum_calibration_error().is_some());
    assert!(report.final_vector.brier_score().is_some());
    assert!(report.final_vector.log_loss().is_some());
    assert!(report.final_vector.multiclass_log_loss.is_some());
    assert!((report.final_vector.observed_positive_rate() - 32.0 / 120.0).abs() < 1e-12);
    assert_eq!(report.verdict, CalibrationVerdict::Calibrated);
    assert!(report.is_calibrated());

    // The binned curve really is binned, and its counts add up.
    let curve = &report.final_vector.curve;
    assert_eq!(curve.bin_count, 10);
    assert_eq!(curve.bins.len(), 10);
    assert_eq!(
        curve.bins.iter().map(|bin| bin.count).sum::<usize>(),
        curve.observations
    );
    assert!(
        curve.populated_bins() >= 2,
        "a curve with one populated row is not a curve"
    );
    assert!((curve.bins[0].lower - 0.0).abs() < 1e-12);
    assert!((curve.bins[9].upper - 1.0).abs() < 1e-12);
    for bin in curve.bins.iter().filter(|bin| bin.count > 0) {
        assert!(bin.gap.is_finite());
        assert!((bin.gap - (bin.mean_predicted - bin.observed_frequency)).abs() < 1e-12);
        assert!(bin.observed_frequency >= 0.0 && bin.observed_frequency <= 1.0);
    }

    // Subordination, structurally: the marginal numbers live in a different
    // type, and the headline is no worse than the normalized independent route
    // on the binned measure.
    //
    // It is worth being precise about what separates them, because it is not
    // the binned number. On this fixture the two routes *tie* on expected
    // calibration error (0.0667 each): the normalized route errs by about the
    // same amount on every candidate, and averaging that over three candidates
    // in three bins gives the same figure the joint achieves. The bin-free
    // table is what separates them — the joint's worst named candidate is
    // 0.1032 out while the normalized route's is 0.1673 out. That is the whole
    // argument for reporting a per-candidate table beside the curve, and it is
    // asserted as such rather than assumed.
    assert!(
        ece <= report.marginal_normalized.expected_calibration_error() + 1e-9,
        "the fitted joint must be no worse on the binned measure: {ece} vs {}",
        report.marginal_normalized.expected_calibration_error()
    );
    assert!(
        report
            .final_vector
            .maximum_candidate_calibration_error()
            .unwrap_or(1.0)
            < report
                .marginal_normalized
                .maximum_candidate_calibration_error()
                .unwrap_or(0.0),
        "and decisively better on the worst named candidate: {} vs {}",
        report
            .final_vector
            .maximum_candidate_calibration_error()
            .unwrap_or(f64::NAN),
        report
            .marginal_normalized
            .maximum_candidate_calibration_error()
            .unwrap_or(f64::NAN)
    );
    assert_eq!(
        report.final_vector.observations, report.marginal_normalized.observations,
        "both routes must be measured over the same observations"
    );
}

/// GATE 2, second part: the measurement has teeth. Two deliberately
/// confidently-wrong heads, both rejected, through the same validated-type path
/// as the headline.
#[test]
fn a_confidently_wrong_head_is_detected_by_the_measurement() {
    let snapshot = render(&main_fixture(200), 13);
    let ceilings = AcceptanceTolerances::from(&ReliabilityConfig::default());

    // A head certain that alpha serves, when alpha serves a fifth of the time.
    let confident = uncalibrated_measure(&snapshot, &CONFIDENT_WRONG_MILLI);
    assert!(
        !confident.passes(&ceilings),
        "a confident head must be refused"
    );
    assert_eq!(
        CalibrationVerdict::from_measurements(&confident, &ceilings),
        CalibrationVerdict::Miscalibrated
    );
    assert!(
        confident.maximum_calibration_error().unwrap_or(0.0) > 0.5,
        "a head 74 points wrong on its confident candidate must be caught: {:?}",
        confident.per_candidate
    );
    let alpha = candidate_row(&confident, ALPHA.0);
    assert!(alpha.mean_predicted > 0.9, "the head really was confident");
    assert!(
        (alpha.observed_frequency - 0.20).abs() < 1e-12,
        "and really was wrong about it"
    );

    // A head whose ranking is the exact reverse of the truth, and confident.
    let reversed = uncalibrated_measure(&snapshot, &REVERSED_MILLI);
    assert!(
        !reversed.passes(&ceilings),
        "a reversed ranking must be refused"
    );
    assert!(
        reversed.maximum_calibration_error().unwrap_or(0.0) > 0.3,
        "a reversed ranking must be caught: {:?}",
        reversed.per_candidate
    );
    // The two broken heads fail in different ways, and both are caught. The
    // confident head is wrong about one candidate and right about the other
    // two; the reversed head is wrong about all three at once. Which of the two
    // costs more depends on the axis, so no ordering between them is claimed
    // here — only that both are rejected and that both cost more than the
    // merely-miscalibrated honest head.
    //
    // The honest head is itself **miscalibrated**, and that is the premise of
    // the whole node rather than a flaw in the fixture: at the identity
    // parameterization it is out by 0.29 on its most over-claimed candidate,
    // which is exactly the error the fitted joint exists to remove. So the gate
    // is not "reject everything" — the same rule accepts the fitted joint over
    // this very snapshot.
    let honest = uncalibrated_measure(&snapshot, &HONEST_MILLI);
    assert_eq!(honest.per_candidate.len(), 3);
    assert!(
        !honest.passes(&ceilings),
        "the unfitted honest head is not calibrated either: {:?}",
        honest.per_candidate
    );
    for (label, measure) in [("confident", &confident), ("reversed", &reversed)] {
        assert!(
            measure.multiclass_log_loss.unwrap_or(0.0)
                > honest.multiclass_log_loss.unwrap_or(f64::INFINITY),
            "the {label} head must cost more multiclass log loss than the merely-miscalibrated honest one"
        );
        assert!(
            measure.brier_score().unwrap_or(1.0) > honest.brier_score().unwrap_or(0.0),
            "the {label} head must have a worse Brier score than the honest one"
        );
    }
    let fitted = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    assert!(fitted.report().final_vector.passes(&ceilings));
    assert!(
        fitted
            .report()
            .final_vector
            .maximum_candidate_calibration_error()
            .unwrap_or(1.0)
            < honest.maximum_candidate_calibration_error().unwrap_or(0.0),
        "and the fit is what closed the honest head's per-candidate gap"
    );

    // A head that is maximally *uninformative* is the known blind spot of a
    // binned gap measure, and it is pinned here rather than left unexamined: a
    // constant head produces a uniform vector, and against a non-uniform truth
    // its per-candidate errors partly cancel. This is a real limitation of
    // expected calibration error, it is why the per-candidate table exists, and
    // it is why this node does not rest the claim on the binned number alone.
    let constant = uncalibrated_measure_with(&snapshot, &ConstantModel(0.98));
    let masses: Vec<f64> = constant
        .per_candidate
        .iter()
        .map(|row| row.mean_predicted)
        .collect();
    assert!(
        masses.iter().all(|mass| (mass - 1.0 / 3.0).abs() < 1e-9),
        "a constant head must produce a uniform vector on a symmetric axis: {masses:?}"
    );
    assert!(
        constant
            .maximum_candidate_calibration_error()
            .unwrap_or(1.0)
            > 0.05,
        "the per-candidate table still sees the uniform head's error: {:?}",
        constant.per_candidate
    );
    assert!(
        constant.expected_calibration_error() < 0.07,
        "and the binned measure largely does not, which is the documented blind spot"
    );
}

/// GATE 2, third part: a binned curve can hide a per-candidate error, and this
/// node does not rest the claim on the bins alone.
///
/// On the main fixture the independent route lands all three of its calibrated
/// masses inside the `[0.2, 0.3)` bin, where their errors very nearly cancel.
/// Its binned expected calibration error is therefore ~0, while its worst
/// *named candidate* is more than twelve points out. The bin-free table is what
/// makes that visible, and the acceptance rule checks it, so the cancellation
/// cannot buy a calibration claim.
#[test]
fn the_bin_free_candidate_table_catches_what_the_bins_average_away() {
    let snapshot = render(&main_fixture(200), 17);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let marginal = &outcome.report().marginal_calibrated;

    let populated: Vec<&ReliabilityBin> = marginal
        .curve
        .bins
        .iter()
        .filter(|bin| bin.count > 0)
        .collect();
    assert_eq!(
        populated.len(),
        1,
        "the fixture is chosen so the independent route's three masses collide in one bin"
    );
    assert_eq!(populated[0].count, 120);
    assert!(
        marginal.expected_calibration_error() < 1e-9,
        "the binned error cancels: {}",
        marginal.expected_calibration_error()
    );

    // The bin-free table does not cancel.
    let worst = marginal
        .maximum_candidate_calibration_error()
        .expect("the table is populated");
    assert!(
        worst > 0.12,
        "the worst named candidate is more than twelve points out and the bins hid it: {worst}"
    );
    let bravo = marginal
        .per_candidate
        .iter()
        .find(|row| row.candidate.provider() == BRAVO.0)
        .expect("bravo is on the axis");
    assert!(
        bravo.gap < -0.10,
        "bravo is under-claimed by the shared map"
    );

    // And a ceiling on the bins alone would have accepted it. The
    // per-candidate ceiling is what refuses, under one shared rule.
    let bins_only = AcceptanceTolerances {
        max_expected_calibration_error: 0.10,
        max_calibration_error: 0.25,
        max_candidate_calibration_error: 1.0,
    };
    let strict = AcceptanceTolerances {
        max_candidate_calibration_error: 0.05,
        ..bins_only
    };
    assert!(
        marginal.passes(&bins_only),
        "with no per-candidate ceiling the cancellation would buy a claim"
    );
    assert!(
        !marginal.passes(&strict),
        "the per-candidate ceiling is what refuses it"
    );

    // The normalized route is refused under the same strict ceiling, so the
    // per-candidate ceiling is not merely punishing a route that has no
    // candidate structure at all.
    let normalized = &outcome.report().marginal_normalized;
    assert!(!normalized.passes(&strict));

    // The fitted joint accepts the *default* ceilings, which is the actual
    // claim. It does not clear the 0.05 ceiling used above, and this test says
    // so rather than quietly picking a ceiling that flatters it.
    let defaults = AcceptanceTolerances::from(&ReliabilityConfig::default());
    assert!(
        outcome.report().final_vector.passes(&defaults),
        "the fitted joint clears the default ceilings"
    );
    assert!(
        !outcome.report().final_vector.passes(&strict),
        "the fitted joint is not inside five points on every candidate, and that is reported"
    );
    assert!(
        outcome
            .report()
            .final_vector
            .maximum_candidate_calibration_error()
            .unwrap_or(1.0)
            < normalized
                .maximum_candidate_calibration_error()
                .unwrap_or(0.0),
        "but it is still the closest of the three"
    );
}

/// GATE 2, fourth part: the verdict is recomputed, never read.
///
/// The stored verdict and the recomputed verdict must agree, and tightening a
/// ceiling below the measured value flips the recomputed one — which is only
/// possible if the rule really reads the stored measurement.
#[test]
fn the_verdict_is_recomputed_from_the_stored_measurement() {
    let snapshot = render(&main_fixture(200), 19);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let report = outcome.report();

    assert_eq!(
        report.verdict,
        report.recomputed_verdict(),
        "the recorded verdict and the recomputed verdict must agree"
    );
    assert_eq!(
        report.is_calibrated(),
        report.recomputed_verdict().is_calibrated()
    );
    assert_eq!(
        CalibrationVerdict::from_measurements(&report.final_vector, &report.tolerances),
        report.verdict,
        "the verdict is a pure function of the final measurement and the ceilings"
    );

    let strict = AcceptanceTolerances {
        max_candidate_calibration_error: 0.001,
        ..report.tolerances
    };
    assert_eq!(
        CalibrationVerdict::from_measurements(&report.final_vector, &strict),
        CalibrationVerdict::Miscalibrated,
        "tightening a ceiling below the measured value must flip the verdict"
    );
    assert_eq!(report.verdict, CalibrationVerdict::Calibrated);
    assert!(report.headline().contains("final vector"));
}

/// GATE 2, fifth part: the honest answer to the question this node exists to
/// answer, measured.
///
/// The joint route and the independent route are both fitted on the same
/// partition and both measured on the same holdout with the same rule. The
/// damage normalization does to the independent route is a number, and so is the
/// joint route's distance from the same starting point.
#[test]
fn the_joint_route_beats_the_normalized_independent_route_and_the_damage_is_a_number() {
    let snapshot = render(&main_fixture(200), 67);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let report = outcome.report();

    // The head's three raw numbers sum to 1.85, so it is not a distribution and
    // has to be either calibrated or normalized before it can be compared to one.
    assert!(
        (HONEST_MILLI.iter().map(|m| f64::from(*m)).sum::<f64>() / 1000.0 - 1.85).abs() < 1e-12
    );

    // Normalizing the independently-calibrated vector degraded it. This is the
    // measured answer to "does normalizing make it worse", and the sign is the
    // claim.
    assert!(
        report.normalization_damage.degraded_calibration(),
        "normalization must be reported as having degraded calibration: {:?}",
        report.normalization_damage
    );
    assert!(
        report.normalization_damage.delta > 0.05,
        "{:?}",
        report.normalization_damage
    );
    assert!(
        report
            .normalization_damage
            .expected_calibration_error_before
            < 0.01,
        "the independent route is well calibrated as three separate probabilities"
    );
    // And the bin-free view of the same question agrees.
    assert!(
        report.normalization_damage.candidate_delta().unwrap_or(0.0) > 0.0,
        "the worst named candidate got worse too: {:?}",
        report.normalization_damage
    );
    assert_eq!(
        report.normalization_damage.delta,
        report.normalization_damage.expected_calibration_error_after
            - report
                .normalization_damage
                .expected_calibration_error_before
    );

    // The joint route, fitted on the same partition and measured on the same
    // holdout under the same rule, beats the normalized independent route on
    // the bin-free view and ties it on the binned one.
    assert!(
        report.final_vector.expected_calibration_error()
            <= report.marginal_normalized.expected_calibration_error() + 1e-9,
        "the fitted joint must be no worse on the binned measure"
    );
    let joint_worst = report
        .final_vector
        .maximum_candidate_calibration_error()
        .expect("the final vector has candidates");
    let normalized_worst = report
        .marginal_normalized
        .maximum_candidate_calibration_error()
        .expect("the normalized route has candidates");
    let calibrated_worst = report
        .marginal_calibrated
        .maximum_candidate_calibration_error()
        .expect("the calibrated route has candidates");
    assert!(
        joint_worst < normalized_worst,
        "the joint must be closer on the worst named candidate: {joint_worst} vs {normalized_worst}"
    );
    assert!(
        joint_worst < calibrated_worst,
        "and closer than the un-normalized independent route: {joint_worst} vs {calibrated_worst}"
    );
    // Every named candidate the joint emits is within a tenth of the truth,
    // while the normalized route is out by more than a sixth on one of them.
    assert!(joint_worst < 0.11, "joint worst {joint_worst}");
    assert!(
        normalized_worst > 0.16,
        "normalized worst {normalized_worst}"
    );

    // And it beats the same parameterization left unfitted, so the fit earned
    // its place.
    let (uncalibrated_ece, fitted_ece) = report
        .improvement_over_uncalibrated()
        .expect("the report carries the comparison");
    assert!(
        fitted_ece < uncalibrated_ece,
        "the fit must improve on the identity parameterization: {uncalibrated_ece} -> {fitted_ece}"
    );
    let (uncalibrated_worst, fitted_worst) = (
        report
            .uncalibrated_joint
            .maximum_candidate_calibration_error()
            .expect("measured"),
        joint_worst,
    );
    assert!(
        fitted_worst < uncalibrated_worst,
        "on the worst named candidate too: {uncalibrated_worst} -> {fitted_worst}"
    );

    // The mild fixture is the control: a head whose ranking already agrees with
    // the outcome, so the independent route is not handed an impossible task.
    // The joint route must still be calibrated there, which is what shows the
    // result above is about the *joint* parameterization and not about a
    // fixture built to be unwinnable.
    let mild = run_calibration(
        &render(&mild_fixture(240), 71),
        &mild_model(),
        &mild_config(),
    )
    .expect("the mild fixture calibrates");
    let mild_report = mild.report();
    assert_eq!(mild_report.verdict, CalibrationVerdict::Calibrated);
    assert!(
        mild_report.final_vector.expected_calibration_error() < 0.07,
        "mild final ece {}",
        mild_report.final_vector.expected_calibration_error()
    );
    assert!(
        mild_report
            .marginal_calibrated
            .maximum_candidate_calibration_error()
            .unwrap_or(1.0)
            < 0.05,
        "with a correctly ranked head the independent route really is close, which is the point"
    );
    // The independent route's intercepts moved the *right* way here, unlike on
    // the reversed fixture: the slope is positive, so the map preserved the
    // head's own ranking.
    assert!(
        mild.marginal_calibrator().slope() > 0.0,
        "a correctly ranked head needs a positively sloped map"
    );
    assert!(
        outcome.marginal_calibrator().slope() < 0.0,
        "a reversed head needs a negatively sloped map, which is a sign the map is fighting its input"
    );
}
