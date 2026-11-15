use zroutery_core::failure::FailureClass;
use zroutery_core::feedback::DataOrigin;
use zroutery_core::ml::calibration::{
    project_cohorts, run_calibration, CalibrationConfig, CalibrationError, CandidateInput,
    DecisionCohort, DegeneracyReason, DriftVerdict, EmittedDecision, HoldoutConfig, KWayCalibrator,
    PartitionKind, DEFAULT_PROBABILITY_FLOOR,
};
use zroutery_core::ml::dataset::{try_samples_from_outcome, OutcomeTrainingSample};
use zroutery_core::outcome::{Attempt, Outcome};

use super::support::*;

/// Emit every decision's vector at the calibrator's identity parameterization
/// and measure it.
///
/// The point of going through the identity rather than through a fitted
/// calibrator is that what is under test is the *measurement*. A fitted
/// calibrator would repair a broken head, and the test would then be about the
/// fit rather than about whether the gate can see a confidently-wrong model.
/// The projection is in arrival order, because that is the only order in which
/// a prefix and a suffix mean "earlier" and "later".
///
/// This was the third defect in one family. `project_cohorts` used to sort by
/// `CohortOrderKey`, which is a *content* key, and `run_calibration` then took
/// `cohorts.split_at(len - holdout)` as a frozen holdout. The reasoning behind the
/// sort was sound — cohorts that tie on the key are indistinguishable to every sum
/// the module computes — and it was applied in the wrong place, because a
/// partition is not a sum. Which cohorts land in the holdout is decided by
/// position, and position in a content-sorted vector is a function of tie-breaking
/// rather than of history.
///
/// `sort_by` is stable, so tied cohorts kept the order `group_attempt_rows`
/// produced, and the degenerate case therefore appeared only when the ties fell
/// differently. Measured on a window whose recorded decisions alternated two
/// served identities perfectly, the holdout held one identity in five runs out of
/// six and six in the other, and the gate correctly refused the degenerate minority.
///
/// Asserted on the fixture that has three candidates cycling, so a content-sorted
/// projection and an arrival-ordered one are trivially distinguishable.
#[test]
fn projection_is_in_arrival_order_not_content_order() {
    let specs = main_fixture(24);
    let snapshot = render(&specs, 7);
    let cohorts =
        project_cohorts(&snapshot, &honest_model(), DEFAULT_PROBABILITY_FLOOR).expect("projection");

    let served: Vec<String> = cohorts
        .iter()
        .map(|cohort| {
            cohort
                .served()
                .map(|identity| format!("{}/{}", identity.provider(), identity.model()))
                .unwrap_or_else(|| "<none>".to_string())
        })
        .collect();

    // Every fifth decision served nobody, so arrival order has a recognisable
    // rhythm: three served, one not, repeating.
    assert_eq!(served.len(), specs.len(), "every decision becomes a cohort");
    for (index, identity) in served.iter().enumerate() {
        let expected_served = index % 5 != 4;
        assert_eq!(
            identity != "<none>",
            expected_served,
            "decision {index} served {identity}, but the fixture says {}",
            if expected_served { "served" } else { "nobody" }
        );
    }

    // And the served identities cycle, which a content sort would flatten into
    // runs of one candidate.
    let distinct = served
        .iter()
        .filter(|identity| *identity != "<none>")
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        distinct.len() >= 3,
        "the fixture cycles three candidates; an arrival-ordered projection keeps \
         all three interleaved, a content-ordered one would not. Saw {distinct:?}"
    );
}

/// The frozen holdout is a later slice of history, so it carries the later
/// decisions' identities rather than whichever content cluster sorted last.
///
/// The complement of `projection_is_in_arrival_order_not_content_order`: that one
/// pins the projection, this one pins the partition taken from it. The split is
/// performed here rather than through `run_calibration` so the assertion is about
/// the rule — "the trailing decisions", by the same arithmetic the gate uses — and
/// not about whatever the outcome happens to expose.
#[test]
fn the_holdout_is_the_later_slice_rather_than_a_content_cluster() {
    let specs = main_fixture(400);
    let snapshot = render(&specs, 11);
    let holdout_size = config().holdout.holdout_cohorts;
    let cohorts =
        project_cohorts(&snapshot, &honest_model(), DEFAULT_PROBABILITY_FLOOR).expect("projection");
    assert!(
        cohorts.len() > holdout_size,
        "the fixture produced {} cohorts, which is not longer than the {holdout_size}-cohort \
         holdout, so the split proves nothing",
        cohorts.len()
    );

    let distinct_in_tail = |slice: &[zroutery_core::ml::DecisionCohort]| {
        slice
            .iter()
            .filter_map(|cohort| cohort.served())
            .map(|identity| {
                (
                    identity.provider().to_string(),
                    identity.model().to_string(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>()
    };

    // The trailing decisions cycle through three candidates, so the tail must too.
    // Under a content-ordered projection the tail was one candidate and the gate
    // refused with `SingleServedCandidate`.
    let tail = distinct_in_tail(&cohorts[cohorts.len() - holdout_size..]);
    assert!(
        tail.len() >= 2,
        "the trailing {holdout_size} decisions hold {} distinct served \
         identities; a later slice of a cycling history holds several, and one \
         means the tail was a content cluster rather than a slice of time. Saw {tail:?}",
        tail.len()
    );

    // And the head, for symmetry: a prefix of a cycling history is not one
    // candidate either. Both halves are checked because a projection that put the
    // whole body in one cluster would satisfy neither.
    let head = distinct_in_tail(&cohorts[..holdout_size]);
    assert!(
        head.len() >= 2,
        "the leading {holdout_size} decisions hold {} distinct served identities. \
         Saw {head:?}",
        head.len()
    );
}

// ---------------------------------------------------------------------------
// GATE 4 -- holdout and drift
// ---------------------------------------------------------------------------

/// GATE 4, first part: the calibrator is fitted on one partition and measured
/// on a disjoint one, and it is actually fitted.
#[test]
fn the_holdout_is_disjoint_from_the_partition_the_calibrator_was_fitted_on() {
    let snapshot = render(&main_fixture(200), 31);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let report = outcome.report();

    assert_eq!(
        report.fit_cohorts + report.holdout_cohorts,
        report.cohorts_total
    );
    assert_eq!(
        outcome.calibrator().fit_cohorts(),
        report.fit_cohorts,
        "the calibrator and the report must agree on the fit size"
    );
    assert_eq!(
        outcome.calibrator().fit_attributed_cohorts(),
        report.fit_attributed_cohorts
    );
    assert!(report.fit_cohorts > report.holdout_cohorts);
    assert!(outcome.calibrator().is_fitted());
    assert!(
        outcome.calibrator().final_log_loss() < outcome.calibrator().initial_log_loss(),
        "the fit must improve on the identity parameterization in-sample too"
    );
    assert!(outcome.calibrator().final_log_loss().is_finite());
    assert!(
        outcome.marginal_calibrator().final_log_loss()
            < outcome.marginal_calibrator().initial_log_loss()
    );

    // The intercepts are sum-to-zero, which is what makes the temperature and
    // the intercepts separately identifiable.
    let intercept_sum: f64 = outcome
        .calibrator()
        .intercepts()
        .iter()
        .map(|record| record.intercept)
        .sum();
    assert!(
        intercept_sum.abs() < 1e-9,
        "the intercepts must be projected to sum to zero, got {intercept_sum}"
    );
    for record in outcome.calibrator().intercepts() {
        assert_eq!(
            record.fit_cohorts, 160,
            "every candidate is in every decision"
        );
        assert!(
            record.fit_serving_cohorts > 0,
            "every candidate serves sometimes"
        );
    }
    assert!(outcome.calibrator().temperature() > 0.0);

    // The holdout's fingerprints are content-derived and appear once per
    // decision.
    let fingerprints: Vec<&str> = outcome
        .holdout_distributions()
        .iter()
        .map(EmittedDecision::cohort_fingerprint)
        .collect();
    assert_eq!(fingerprints.len(), report.holdout_cohorts);
    assert!(fingerprints
        .iter()
        .all(|fingerprint| fingerprint.len() == 16));
}

/// Two candidates the head reports identically, decided 60/40.
///
/// The only signal is the empirical winner frequency, so a fit that solves the
/// reported mean log loss must reproduce it and a fit whose step size scales
/// with the row count will not.
fn contested_cohorts(count: usize) -> Vec<DecisionCohort> {
    (0..count)
        .map(|index| {
            let alpha_serves = index % 10 < 6;
            DecisionCohort::try_new(
                probe_context(BASE_TIMESTAMP + index as i64),
                None,
                vec![
                    CandidateInput::Ranked {
                        candidate: identity(ALPHA),
                        raw_success_probability: 0.5,
                    },
                    CandidateInput::Ranked {
                        candidate: identity(BRAVO),
                        raw_success_probability: 0.5,
                    },
                ],
                Some(identity(if alpha_serves { ALPHA } else { BRAVO })),
            )
            .expect("a two-candidate contested cohort")
        })
        .collect()
}

/// GATE 4, third part: the joint fit solves the loss the report publishes.
///
/// The reported objective is the *mean* log loss over the attributed cohorts.
/// Accumulating an unnormalised gradient made the fitted map a function of the
/// snapshot's row count: the same 60/40 distribution fitted on ten decisions
/// learned roughly the truth, while the same distribution replicated to ten
/// thousand decisions learned its reverse and reported a worse mean loss. A
/// fit of a mean loss must be invariant under replication, because replicating
/// a distribution does not add information.
#[test]
fn replicating_the_same_distribution_does_not_move_the_joint_fit() {
    let base = contested_cohorts(10);
    let mut replicated = Vec::with_capacity(base.len() * 1_000);
    for _ in 0..1_000 {
        replicated.extend(base.iter().cloned());
    }

    let config = CalibrationConfig::default();
    let small = KWayCalibrator::fit(&base, &config, DEFAULT_PROBABILITY_FLOOR).expect("fit");
    let large = KWayCalibrator::fit(&replicated, &config, DEFAULT_PROBABILITY_FLOOR)
        .expect("the replicated fit");

    assert_eq!(small.intercepts().len(), large.intercepts().len());
    for (small_record, large_record) in small.intercepts().iter().zip(large.intercepts()) {
        assert_eq!(small_record.candidate, large_record.candidate);
        assert!(
            (small_record.intercept - large_record.intercept).abs() < 1e-9,
            "{} moved from {} to {} when the same distribution was replicated",
            small_record.candidate.provider(),
            small_record.intercept,
            large_record.intercept
        );
    }
    assert!(
        (small.temperature() - large.temperature()).abs() < 1e-9,
        "temperature moved from {} to {}",
        small.temperature(),
        large.temperature()
    );

    // And the fit still improves on the identity parameterization at both
    // sizes: solving the reported loss must not trade one degeneracy for
    // another.
    for calibrator in [&small, &large] {
        assert!(
            calibrator.final_log_loss() < calibrator.initial_log_loss(),
            "the fit must beat identity: {} vs {}",
            calibrator.final_log_loss(),
            calibrator.initial_log_loss()
        );
        assert!(calibrator.final_log_loss().is_finite());
    }

    // The fitted mass tracks the 60/40 the data actually shows rather than
    // reversing it. `alpha` is the always-present first candidate.
    let alpha = small
        .intercepts()
        .iter()
        .find(|record| record.candidate == identity(ALPHA))
        .expect("alpha is in the vocabulary");
    let bravo = small
        .intercepts()
        .iter()
        .find(|record| record.candidate == identity(BRAVO))
        .expect("bravo is in the vocabulary");
    assert!(
        alpha.intercept > bravo.intercept,
        "the more frequent winner must not be pushed below the less frequent one: \
         {} vs {}",
        alpha.intercept,
        bravo.intercept
    );
}

/// A legitimate same-candidate retry stays one candidate on the axis.
///
/// The product retries the same provider/model after a rectifier repairs a
/// request and records each attempt separately. Projection used to push one
/// candidate per attempt and then reject the resulting axis as a duplicate, so
/// a single repaired request failed the whole batch. The candidate axis is now
/// the set of unique identities and a retried candidate's evidence is its
/// terminal attempt.
#[test]
fn a_same_candidate_rectifier_retry_is_one_candidate() {
    let model = "model-alpha";
    let provider = "p-alpha";

    let outcome = Outcome::builder("req-rectified")
        .decision_id("dec-rectified")
        .dialect("anthropic")
        .timestamp(BASE_TIMESTAMP)
        .single_candidate(model, provider)
        .attempt(Attempt {
            attempt_id: "att-rectified-0".to_string(),
            candidate_model: model.to_string(),
            candidate_provider: provider.to_string(),
            started_at: BASE_TIMESTAMP,
            completed_at: BASE_TIMESTAMP + 1,
            latency_ms: 30.0,
            ttft_ms: None,
            success: false,
            failure_class: Some(FailureClass::Transport),
            failure_message: Some("first attempt failed".to_string()),
            http_status: Some(503),
            rectified: false,
            cost: None,
        })
        .attempt(Attempt {
            attempt_id: "att-rectified-1".to_string(),
            candidate_model: model.to_string(),
            candidate_provider: provider.to_string(),
            started_at: BASE_TIMESTAMP + 1,
            completed_at: BASE_TIMESTAMP + 2,
            latency_ms: 140.0,
            ttft_ms: Some(50.0),
            success: true,
            failure_class: None,
            failure_message: None,
            http_status: Some(200),
            rectified: true,
            cost: None,
        })
        .total_latency_ms(170.0)
        .cost(Some(0.01), Some(0.01))
        .build();

    outcome
        .validate()
        .expect("a failed-then-rectified outcome is valid");
    assert!(outcome.success, "the terminal attempt succeeded");
    assert_eq!(
        outcome.served_identity(),
        Some(identity((provider, model))),
        "the retried candidate is the served identity"
    );

    // Two attempts on one candidate, plus the request-scope row.
    let samples = try_samples_from_outcome(
        &outcome,
        &[scripted_features(0, 300), scripted_features(0, 700)],
        DataOrigin::Native,
    )
    .expect("the canonical projection accepts a retried candidate");
    assert_eq!(samples.len(), 3, "two attempts and the request row");
    assert_eq!(
        samples[0].attempts.len(),
        2,
        "both attempts stay in the retained evidence"
    );
    assert!(!samples[0].rectified, "the first attempt is not the retry");
    assert!(
        samples[1].rectified,
        "the second attempt is the rectifier retry"
    );

    let cohorts = project_cohorts(&samples, &echo_model(), DEFAULT_PROBABILITY_FLOOR)
        .expect("a repaired request must not fail the whole batch");
    assert_eq!(cohorts.len(), 1, "both attempts belong to one decision");
    let cohort = &cohorts[0];
    assert_eq!(cohort.arity(), 1, "the identity appears on the axis once");
    assert_eq!(
        cohort.candidates()[0].candidate(),
        &identity((provider, model))
    );
    assert_eq!(cohort.served(), Some(&identity((provider, model))));
    let raw = cohort.candidates()[0]
        .raw_success_probability()
        .expect("the retried candidate is ranked");
    assert!(
        (raw - 0.7).abs() < 1e-12,
        "the terminal attempt is the candidate's evidence, got {raw}"
    );
    assert!(
        cohort.is_attributed(),
        "the retried candidate is attributed"
    );
}

/// GATE 4, second part: a regime shift between the two partitions is REFUSED,
/// and the refusal carries the measurement.
///
/// The holdout's head output is moved to the other end of the unit interval,
/// which is what a real regime change looks like to a probability head.
#[test]
fn a_shifted_holdout_is_refused_with_the_drift_measurement_attached() {
    let specs = shifted_fixture(&main_fixture(200), 160);
    let snapshot = render(&specs, 37);

    let error = run_calibration(&snapshot, &echo_model(), &config())
        .expect_err("a shifted holdout must be refused, not reported on");
    let CalibrationError::DistributionShift { measurement } = &error else {
        panic!("expected a drift refusal, got {error:?}");
    };
    assert!(
        measurement.population_stability_index
            > measurement.tolerances.max_population_stability_index,
        "the refusal must be caused by the measured shift: {measurement}"
    );
    assert!(
        measurement.population_stability_index > 0.5,
        "the two regimes share no probability bin at all, so the shift must be large: {measurement}"
    );
    assert_eq!(measurement.verdict(), DriftVerdict::Shifted);
    assert!(!measurement.is_acceptable());
    assert_eq!(measurement.fit_cohorts, 160);
    assert_eq!(measurement.holdout_cohorts, 40);
    assert!(measurement.summary().contains("psi="));
    // The refusal text carries the numbers, not just a verdict.
    assert!(error.to_string().contains("psi="));
    // The two partitions really do occupy disjoint bins, which is why the index
    // is what it is.
    let fit_support: Vec<usize> = measurement
        .fit_bin_proportions
        .iter()
        .enumerate()
        .filter(|(_, share)| **share > 0.0)
        .map(|(slot, _)| slot)
        .collect();
    let holdout_support: Vec<usize> = measurement
        .holdout_bin_proportions
        .iter()
        .enumerate()
        .filter(|(_, share)| **share > 0.0)
        .map(|(slot, _)| slot)
        .collect();
    assert_eq!(fit_support, vec![4, 5, 9], "the honest regime's bins");
    assert_eq!(holdout_support, vec![1, 6, 8], "the shifted regime's bins");
}

/// GATE 4, third part: the unshifted fixture is *not* refused, so the drift gate
/// is a gate and not a blanket rejection.
#[test]
fn an_unshifted_holdout_passes_the_drift_gate() {
    let snapshot = render(&main_fixture(200), 41);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let drift = &outcome.report().drift;
    assert!(drift.is_acceptable());
    assert_eq!(drift.verdict(), DriftVerdict::WithinTolerance);
    assert_eq!(drift.bin_count, 10);
    assert_eq!(drift.fit_bin_proportions.len(), 10);
    assert_eq!(drift.holdout_bin_proportions.len(), 10);
    assert!(
        (drift.fit_bin_proportions.iter().sum::<f64>() - 1.0).abs() < 1e-9,
        "the drift histogram must be a distribution"
    );
    assert!((drift.holdout_bin_proportions.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    // The head reports the same three numbers in both partitions, so the shift
    // is exactly zero. This asserts the measurement is sensitive to the input
    // rather than to the partition boundary.
    assert!(drift.population_stability_index < 1e-9, "{drift}");
    assert!(drift.base_rate_delta < 1e-9);
    assert!(drift.arity_delta < 1e-9);
    assert_eq!(drift.tolerances.bin_count, 10);
    assert_eq!(drift.tolerances.max_population_stability_index, 0.25);
    assert_eq!(drift.tolerances.max_base_rate_delta, 0.20);
}

/// GATE 4, fourth part: a degenerate partition is refused rather than producing
/// a calibrated-looking vector.
///
/// Each case is built to look as plausible as possible: requests that all
/// failed, and requests where one candidate always won.
#[test]
fn a_degenerate_partition_is_refused_with_a_typed_reason() {
    // Every request fails, so no cohort is attributed and every observed
    // frequency would be zero — any non-zero mass would be confidently wrong.
    let mut all_failed = main_fixture(200);
    for spec in &mut all_failed {
        spec.served = None;
    }
    assert_degenerate(
        &render(&all_failed, 43),
        DegeneracyReason::NoAttributedOutcome,
        "every request failed",
    );

    // One candidate always wins, so the axis cannot be told apart and a uniform
    // 1/K answer would score the base rate. This is the K-way analogue of the
    // single-class holdout 7E-2B refused on.
    let mut one_winner = main_fixture(200);
    for spec in &mut one_winner {
        if spec.served.is_some() {
            spec.served = Some(BRAVO);
        }
    }
    assert_degenerate(
        &render(&one_winner, 47),
        DegeneracyReason::SingleServedCandidate,
        "only one candidate ever served",
    );
}

/// GATE 4, fifth part: too few attributed decisions for the configured floor is
/// a refusal naming the holdout partition.
#[test]
fn a_sparsely_attributed_holdout_is_refused_by_name() {
    let mut sparse = main_fixture(200);
    // The holdout is the trailing forty decisions. Leave exactly two of them
    // served, by *different* candidates, so the attributed floor is what fires
    // rather than the single-winner check.
    for (index, spec) in sparse.iter_mut().enumerate() {
        if index < 160 {
            continue;
        }
        spec.served = match index {
            160 => Some(BRAVO),
            161 => Some(CHARLIE),
            _ => None,
        };
    }
    let error = run_calibration(&render(&sparse, 53), &honest_model(), &config())
        .expect_err("a holdout with two served decisions must be refused");
    match &error {
        CalibrationError::DegeneratePartition {
            partition,
            reason,
            detail,
        } => {
            assert_eq!(*partition, PartitionKind::Holdout);
            assert_eq!(
                *reason,
                DegeneracyReason::TooFewAttributedOutcomes,
                "the floor is what must fire here"
            );
            assert!(
                detail.contains('2'),
                "the refusal must carry its count: {detail}"
            );
        }
        other => panic!("expected a holdout degeneracy, got {other:?}"),
    }
    assert!(error.to_string().contains("holdout"));
}

/// GATE 4, sixth part: a snapshot too small to split at all is refused before
/// anything is fitted, and a configuration that could never split is refused
/// even earlier.
#[test]
fn a_snapshot_too_small_to_hold_out_is_refused() {
    let error = run_calibration(&render(&main_fixture(12), 59), &honest_model(), &config())
        .expect_err("twelve decisions cannot fill a forty-decision holdout");
    assert!(
        matches!(error, CalibrationError::SnapshotTooSmall { .. }),
        "expected a size refusal, got {error:?}"
    );

    let impossible = CalibrationConfig {
        holdout: HoldoutConfig {
            holdout_cohorts: 40,
            min_fit_cohorts: usize::MAX,
            min_attributed_outcomes: 4,
        },
        ..CalibrationConfig::default()
    };
    let error = run_calibration(
        &render(&main_fixture(200), 61),
        &honest_model(),
        &impossible,
    )
    .expect_err("a holdout that consumes the whole snapshot is meaningless");
    assert!(
        matches!(error, CalibrationError::MeaninglessHoldoutSplit { .. }),
        "expected a configuration refusal, got {error:?}"
    );
}

fn assert_degenerate(snapshot: &[OutcomeTrainingSample], expected: DegeneracyReason, why: &str) {
    match run_calibration(snapshot, &honest_model(), &config()) {
        Err(error) => {
            match &error {
                CalibrationError::DegeneratePartition {
                    partition,
                    reason,
                    detail,
                } => {
                    assert_eq!(*reason, expected, "{why}: got {error}");
                    assert!(
                        matches!(partition, PartitionKind::Fit | PartitionKind::Holdout),
                        "the refusal must name its partition"
                    );
                    assert!(!detail.is_empty(), "a refusal must carry its numbers");
                }
                other => panic!("{why}: expected a degeneracy, got {other:?}"),
            }
            assert!(
                !error.to_string().is_empty(),
                "a refusal must carry a reason"
            );
        }
        Ok(outcome) => panic!(
            "{why}: expected a {expected:?} refusal, got a verdict of {:?}",
            outcome.report().verdict
        ),
    }
}
