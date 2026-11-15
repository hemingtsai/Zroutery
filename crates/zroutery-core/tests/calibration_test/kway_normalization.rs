use zroutery_core::ml::calibration::{
    run_calibration, CalibrationError, CandidateInput, DecisionCohort, KWayCalibrator,
    UnrankedReason, DEFAULT_PROBABILITY_FLOOR,
};
use zroutery_core::ml::decision_contract::{
    DecisionContractError, DecisionDistribution, DISTRIBUTION_NORMALIZATION_TOLERANCE,
};
use zroutery_core::outcome::CandidateIdentity;

use super::support::*;

// ---------------------------------------------------------------------------
// GATE 1 — K-way normalization
// ---------------------------------------------------------------------------

/// GATE 1: every emitted vector sums to one inside the 7E-2A tolerance, has
/// arity equal to its axis, and is accepted by the 7E-2A type.
///
/// The tolerance is asserted against the accepted constant rather than a copy of
/// it, so a change to 7E-2A cannot silently leave this test asserting the wrong
/// thing.
#[test]
fn every_emitted_vector_is_normalized_and_accepted_by_the_contract_type() {
    let snapshot = render(&main_fixture(200), 7);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");

    assert_eq!(outcome.holdout_distributions().len(), 40);
    for decision in outcome.holdout_distributions() {
        let distribution = decision.distribution();
        let mass = distribution.total_mass();
        assert!(
            (mass - 1.0).abs() <= DISTRIBUTION_NORMALIZATION_TOLERANCE,
            "emitted mass {mass} is outside the accepted tolerance {DISTRIBUTION_NORMALIZATION_TOLERANCE}"
        );
        assert_eq!(distribution.k(), distribution.outcomes().len());
        assert_eq!(distribution.k(), distribution.probabilities().len());
        assert_eq!(distribution.k(), 3, "the fixture's axis is three wide");
        for probability in distribution.probabilities() {
            assert!(probability.is_finite(), "a probability was not finite");
            assert!(*probability >= 0.0, "a probability was negative");
            assert!(*probability <= 1.0, "a probability exceeded one");
        }
        // The subject is one of the outcomes, which the type already enforces;
        // restated so the test says what it relies on.
        assert!(distribution.outcomes().contains(distribution.subject()));
        for outcome_axis in distribution.outcomes() {
            assert!(distribution.probability_of(outcome_axis).is_some());
        }
    }
}

/// GATE 1, second half: the 7E-2A type is the gate, and it really does refuse.
///
/// Every way this node could emit an invalid vector is fed to the accepted
/// constructor and must come back as the matching typed refusal. If the type
/// stopped refusing any of these, this node's "the type validates my output"
/// claim would be vacuous.
#[test]
fn the_contract_type_refuses_every_vector_this_node_could_have_emitted() {
    let alpha = identity(ALPHA);
    let bravo = identity(BRAVO);
    let outcomes = vec![alpha.clone(), bravo.clone()];

    let unnormalized =
        DecisionDistribution::try_new(alpha.clone(), outcomes.clone(), vec![0.4, 0.4]);
    assert!(matches!(
        unnormalized,
        Err(DecisionContractError::DistributionNotNormalized { .. })
    ));

    let arity = DecisionDistribution::try_new(alpha.clone(), outcomes.clone(), vec![0.5, 0.4, 0.1]);
    assert!(matches!(
        arity,
        Err(DecisionContractError::DistributionArity { .. })
    ));

    let negative = DecisionDistribution::try_new(alpha.clone(), outcomes.clone(), vec![1.2, -0.2]);
    assert!(matches!(
        negative,
        Err(DecisionContractError::NegativeProbability { .. })
    ));

    let infinite =
        DecisionDistribution::try_new(alpha.clone(), outcomes.clone(), vec![f64::INFINITY, 0.0]);
    assert!(matches!(
        infinite,
        Err(DecisionContractError::NonFiniteProbability { .. })
    ));

    let duplicate = DecisionDistribution::try_new(
        alpha.clone(),
        vec![alpha.clone(), alpha.clone()],
        vec![0.5, 0.5],
    );
    assert!(matches!(
        duplicate,
        Err(DecisionContractError::DuplicateDistributionOutcome(_))
    ));

    let blank = DecisionDistribution::try_new(alpha.clone(), outcomes.clone(), vec![0.5, 0.5]);
    assert!(blank.is_ok(), "a well-formed vector is accepted");

    let foreign_subject =
        DecisionDistribution::try_new(identity(CHARLIE), outcomes.clone(), vec![0.5, 0.5]);
    assert!(matches!(
        foreign_subject,
        Err(DecisionContractError::DistributionSubjectNotAnOutcome(_))
    ));

    let empty = DecisionDistribution::try_new(alpha.clone(), Vec::new(), Vec::new());
    assert!(matches!(
        empty,
        Err(DecisionContractError::EmptyDistribution)
    ));

    // Zero mass is legal, which is what makes an unranked candidate
    // representable at all rather than a special case.
    let zero_mass = DecisionDistribution::try_new(alpha.clone(), outcomes.clone(), vec![1.0, 0.0]);
    assert!(
        zero_mass.is_ok(),
        "an exact zero must be a legal probability, not a refusal"
    );
}

/// GATE 1, third part: an unranked candidate takes exactly `0.0` and does not
/// move the ranked ones.
///
/// The assertion that matters is the second group. "The unranked candidate has
/// no mass" is easy; "the ranked candidates kept precisely the vector they would
/// have had without it" is the property that makes an unranked candidate
/// representable *without distorting the rest*, so the same decision is emitted
/// twice, with and without the unranked entry.
#[test]
fn an_unranked_candidate_takes_exactly_zero_without_moving_the_ranked_ones() {
    let ranked_only = DecisionCohort::try_new(
        probe_context(10),
        Some(&identity(ALPHA)),
        vec![
            CandidateInput::Ranked {
                candidate: identity(ALPHA),
                raw_success_probability: 0.9,
            },
            CandidateInput::Ranked {
                candidate: identity(BRAVO),
                raw_success_probability: 0.55,
            },
        ],
        Some(identity(BRAVO)),
    )
    .expect("a two-candidate cohort is valid");

    let with_unranked = DecisionCohort::try_new(
        probe_context(10),
        Some(&identity(ALPHA)),
        vec![
            CandidateInput::Ranked {
                candidate: identity(ALPHA),
                raw_success_probability: 0.9,
            },
            CandidateInput::Unranked {
                candidate: identity(CHARLIE),
                reason: UnrankedReason::NotEligible,
            },
            CandidateInput::Ranked {
                candidate: identity(BRAVO),
                raw_success_probability: 0.55,
            },
        ],
        Some(identity(BRAVO)),
    )
    .expect("an axis may carry an unranked candidate");

    assert_eq!(ranked_only.arity(), 2);
    assert_eq!(with_unranked.arity(), 3);
    assert_eq!(with_unranked.ranked_count(), 2);
    assert_eq!(
        with_unranked
            .candidates()
            .iter()
            .filter(|input| !input.is_ranked())
            .count(),
        1
    );

    let identity_calibrator = KWayCalibrator::uncalibrated();
    let bare = identity_calibrator
        .distribution(&ranked_only, DEFAULT_PROBABILITY_FLOOR)
        .expect("emission succeeds");
    let padded = identity_calibrator
        .distribution(&with_unranked, DEFAULT_PROBABILITY_FLOOR)
        .expect("emission succeeds");

    let bare_vector = bare.distribution().probabilities();
    let padded_vector = padded.distribution().probabilities();
    assert_eq!(padded_vector.len(), 3);
    assert_eq!(
        padded_vector[1], 0.0,
        "the unranked candidate must hold exactly 0.0, not a small mass"
    );
    assert!(
        padded.unranked().contains(&identity(CHARLIE)),
        "the unranked candidate must be reported as unranked, not as a low-probability candidate"
    );
    assert_eq!(
        padded_vector[0], bare_vector[0],
        "the first ranked candidate must keep its mass exactly"
    );
    assert_eq!(
        padded_vector[2], bare_vector[1],
        "the second ranked candidate must keep its mass exactly"
    );
    assert!(
        (padded.distribution().total_mass() - 1.0).abs() <= DISTRIBUTION_NORMALIZATION_TOLERANCE
    );

    // Every unranked reason is representable and typed.
    for reason in [
        UnrankedReason::NotEligible,
        UnrankedReason::InsufficientEvidence,
        UnrankedReason::ModelCold,
    ] {
        assert_eq!(reason.label(), reason.to_string());
        let cohort = DecisionCohort::try_new(
            probe_context(11),
            Some(&identity(ALPHA)),
            vec![
                CandidateInput::Ranked {
                    candidate: identity(ALPHA),
                    raw_success_probability: 0.8,
                },
                CandidateInput::Unranked {
                    candidate: identity(BRAVO),
                    reason,
                },
            ],
            Some(identity(ALPHA)),
        )
        .expect("an axis may carry an unranked candidate");
        let emitted = identity_calibrator
            .distribution(&cohort, DEFAULT_PROBABILITY_FLOOR)
            .expect("emission succeeds");
        assert_eq!(emitted.distribution().probabilities()[1], 0.0);
    }
}

// ---------------------------------------------------------------------------
// The validating wire form
// ---------------------------------------------------------------------------

/// The 7E-2A type deliberately derives neither `Serialize` nor `Deserialize`,
/// on the stated terms that the node which produces one owns giving it a wire
/// form. This is that wire form, and it is *validating*: a stored record cannot
/// re-enter the measurement path having skipped the arity and normalization
/// checks.
#[test]
fn the_wire_form_revalidates_on_the_way_back_in() {
    let snapshot = render(&main_fixture(200), 109);
    let outcome = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let records = outcome.holdout_records();
    assert_eq!(records.len(), 40);

    for (record, decision) in records.iter().zip(outcome.holdout_distributions()) {
        assert_eq!(record.cohort_fingerprint, decision.cohort_fingerprint());
        assert_eq!(record.outcomes, decision.distribution().outcomes());
        assert_eq!(
            record.probabilities,
            decision.distribution().probabilities()
        );
        assert!((record.total_mass - 1.0).abs() <= DISTRIBUTION_NORMALIZATION_TOLERANCE);
        assert_eq!(record.unranked, decision.unranked());

        let rebuilt = record
            .try_into_distribution()
            .expect("a well-formed record must rebuild");
        assert_eq!(&rebuilt, decision.distribution());
    }

    // A tampered record is refused on the way back in, not accepted.
    let mut tampered = records[0].clone();
    tampered.probabilities[0] += 0.5;
    let error = tampered
        .try_into_distribution()
        .expect_err("a tampered record must be refused");
    assert!(
        matches!(error, CalibrationError::DistributionRejected { .. }),
        "got {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains(DISTRIBUTION_NORMALIZATION_TOLERANCE.to_string().as_str())
            || error.to_string().contains("not normalized")
    );

    let mut wrong_arity = records[0].clone();
    wrong_arity.probabilities.pop();
    assert!(wrong_arity.try_into_distribution().is_err());

    // A subject that is not on its own axis. `CHARLIE` would be accepted — it
    // *is* on this record's axis — so the stand-in is a candidate from
    // elsewhere.
    let mut wrong_subject = records[0].clone();
    wrong_subject.subject = CandidateIdentity::new("model-elsewhere", "p-elsewhere");
    let error = wrong_subject
        .try_into_distribution()
        .expect_err("a foreign subject must be refused");
    assert!(matches!(
        error,
        CalibrationError::DistributionRejected { .. }
    ));

    let mut negative = records[0].clone();
    negative.probabilities[0] = -0.1;
    negative.probabilities[1] = 1.1;
    assert!(negative.try_into_distribution().is_err());

    let mut non_finite = records[0].clone();
    non_finite.probabilities[0] = f64::NAN;
    assert!(non_finite.try_into_distribution().is_err());
}
