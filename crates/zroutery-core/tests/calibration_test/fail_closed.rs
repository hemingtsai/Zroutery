use zroutery_core::ml::calibration::{
    measure_emitted, measure_marginal, project_cohorts, run_calibration, CalibrationConfig,
    CalibrationError, CandidateInput, DecisionCohort, DriftConfig, FitConfig, HoldoutConfig,
    MarginalView, ReliabilityConfig, UnrankedReason, DEFAULT_PROBABILITY_FLOOR,
};
use zroutery_core::ml::dataset::SampleScope;
use zroutery_core::ml::features::FEATURE_SCHEMA_VERSION;
use zroutery_core::outcome::CandidateIdentity;

use super::support::*;

// ---------------------------------------------------------------------------
// GATE 5 — fail-closed, typed refusals
// ---------------------------------------------------------------------------

/// GATE 5, first part: the cohort-scope refusals.
///
/// The mixed-`decision_id` case is the one worth stating: a request whose
/// attempt rows disagree about carrying a decision id is **refused**, not
/// coalesced. Coalescing would merge two real planning events into one invented
/// candidate set and would count a candidate twice if the same identity appeared
/// in both, so losing that fact is treated as the fail-closed condition it is.
#[test]
fn an_incoherent_decision_scope_is_refused_not_coalesced() {
    let mut snapshot = render(&main_fixture(200), 73);
    // Strip the decision id from one attempt row of one request, so that request
    // now disagrees with itself.
    let victim = snapshot
        .iter()
        .position(|row| matches!(row.scope, SampleScope::Attempt { index: 1, .. }))
        .expect("the fixture has a second attempt");
    let request = snapshot[victim].outcome_id.clone();
    snapshot[victim].decision_id = None;
    assert!(snapshot
        .iter()
        .any(|row| row.outcome_id == request && row.decision_id.is_some()));
    assert!(snapshot
        .iter()
        .any(|row| row.outcome_id == request && row.decision_id.is_none()));

    let error = run_calibration(&snapshot, &honest_model(), &config())
        .expect_err("a request that disagrees with itself must be refused");
    assert!(
        matches!(error, CalibrationError::MixedDecisionScope { .. }),
        "expected a mixed decision scope refusal, got {error:?}"
    );
    assert!(error.to_string().contains("decision id"));
}

/// GATE 5, second part: a decision id that names two requests is refused, and a
/// candidate set that cannot form a distribution is refused.
#[test]
fn a_decision_id_spanning_two_requests_is_refused() {
    let mut snapshot = render(&main_fixture(200), 79);
    let first_request = snapshot[0].outcome_id.clone();
    let first_decision = snapshot[0]
        .decision_id
        .clone()
        .expect("the fixture carries decision ids");

    // Point every attempt row of the *second* request at the first request's
    // decision id, so one decision id ends up naming two requests.
    let mut second_request = None;
    for row in &mut snapshot {
        if row.outcome_id == first_request {
            continue;
        }
        if second_request.is_none() {
            second_request = Some(row.outcome_id.clone());
        }
        if second_request.as_deref() == Some(row.outcome_id.as_str())
            && matches!(row.scope, SampleScope::Attempt { .. })
        {
            row.decision_id = Some(first_decision.clone());
        }
    }
    let second_request = second_request.expect("the fixture has a second request");
    assert!(snapshot.iter().any(|row| {
        row.outcome_id == second_request
            && row.decision_id.as_deref() == Some(first_decision.as_str())
    }));

    let error = run_calibration(&snapshot, &honest_model(), &config())
        .expect_err("one decision id naming two requests must be refused");
    assert!(
        matches!(error, CalibrationError::DecisionScopeSpansRequests { .. }),
        "expected a spanning refusal, got {error:?}"
    );
}

/// GATE 5, third part: a candidate set that cannot form a distribution.
///
/// Three shapes, all refused with a reason rather than coerced into a
/// normalized vector: no candidates, no ranked candidate, and a repeated
/// candidate.
#[test]
fn a_candidate_set_that_cannot_form_a_distribution_is_refused() {
    let empty = DecisionCohort::try_new(probe_context(1), None, Vec::new(), None);
    assert!(
        matches!(empty, Err(CalibrationError::CohortWithoutCandidates { .. })),
        "got {empty:?}"
    );

    let all_unranked = DecisionCohort::try_new(
        probe_context(1),
        None,
        vec![
            CandidateInput::Unranked {
                candidate: identity(ALPHA),
                reason: UnrankedReason::NotEligible,
            },
            CandidateInput::Unranked {
                candidate: identity(BRAVO),
                reason: UnrankedReason::ModelCold,
            },
        ],
        None,
    );
    assert!(
        matches!(
            all_unranked,
            Err(CalibrationError::CohortWithoutRankedCandidate { .. })
        ),
        "got {all_unranked:?}"
    );

    let repeated = DecisionCohort::try_new(
        probe_context(1),
        None,
        vec![
            CandidateInput::Ranked {
                candidate: identity(ALPHA),
                raw_success_probability: 0.9,
            },
            CandidateInput::Ranked {
                candidate: identity(ALPHA),
                raw_success_probability: 0.4,
            },
        ],
        Some(identity(ALPHA)),
    );
    assert!(
        matches!(
            repeated,
            Err(CalibrationError::DuplicateCohortCandidate { .. })
        ),
        "got {repeated:?}"
    );

    // The same model offered by two providers is two candidates, not a
    // duplicate: the axis is a set of (provider, model) pairs.
    let two_providers = DecisionCohort::try_new(
        probe_context(1),
        None,
        vec![
            CandidateInput::Ranked {
                candidate: CandidateIdentity::new(ALPHA.1, ALPHA.0),
                raw_success_probability: 0.9,
            },
            CandidateInput::Ranked {
                candidate: CandidateIdentity::new(ALPHA.1, "p-other"),
                raw_success_probability: 0.4,
            },
        ],
        Some(CandidateIdentity::new(ALPHA.1, ALPHA.0)),
    );
    assert!(
        two_providers.is_ok(),
        "one model on two providers is a set of two"
    );

    // A served candidate that is not on its own axis.
    let foreign = DecisionCohort::try_new(
        probe_context(1),
        None,
        vec![CandidateInput::Ranked {
            candidate: identity(ALPHA),
            raw_success_probability: 0.9,
        }],
        Some(identity(CHARLIE)),
    );
    assert!(
        matches!(
            foreign,
            Err(CalibrationError::ServedCandidateNotInCohort { .. })
        ),
        "got {foreign:?}"
    );
}

/// GATE 5, fourth part: a malformed row is a typed refusal, never a silent
/// skip and never a substituted value.
#[test]
fn a_malformed_row_is_a_typed_refusal() {
    // A repeated sample id would put one candidate into a cohort twice. The
    // clone is an *attempt* row, because request-scope rows are not candidate
    // evidence and are ignored by design.
    let mut duplicated = render(&main_fixture(200), 83);
    let victim = duplicated
        .iter()
        .position(|row| matches!(row.scope, SampleScope::Attempt { .. }))
        .expect("the fixture has attempt rows");
    let clone = duplicated[victim].clone();
    duplicated.push(clone);
    let error = run_calibration(&duplicated, &honest_model(), &config())
        .expect_err("a repeated row must be refused");
    assert!(
        matches!(error, CalibrationError::DuplicateSampleId { .. }),
        "got {error:?}"
    );

    // A schema version this build does not accept.
    let mut wrong_schema = render(&main_fixture(200), 89);
    wrong_schema[1].schema_version = FEATURE_SCHEMA_VERSION + 1;
    let error = run_calibration(&wrong_schema, &honest_model(), &config())
        .expect_err("a foreign schema must be refused");
    assert!(
        matches!(
            error,
            CalibrationError::SchemaMismatch {
                component: "sample",
                ..
            }
        ),
        "got {error:?}"
    );

    // A feature vector of the wrong width.
    let mut wrong_width = render(&main_fixture(200), 97);
    wrong_width[1].features.values[0] = f32::NAN;
    let error = run_calibration(&wrong_width, &honest_model(), &config())
        .expect_err("a non-finite feature must be refused");
    assert!(
        matches!(error, CalibrationError::NonFiniteFeature { .. }),
        "got {error:?}"
    );

    // An empty snapshot.
    let error = run_calibration(&[], &honest_model(), &config())
        .expect_err("an empty snapshot must be refused");
    assert!(
        matches!(error, CalibrationError::EmptySnapshot),
        "got {error:?}"
    );
}

/// GATE 5, fifth part: a non-finite or out-of-range prediction is refused, never
/// clamped into range.
///
/// A `NaN` probability is a broken head, not a very confident one, and silently
/// repairing it would be exactly the "calibrated-looking vector" this node is
/// required not to produce.
#[test]
fn a_non_finite_or_out_of_range_prediction_is_refused() {
    let snapshot = render(&main_fixture(200), 101);

    for broken in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1.4, -0.2] {
        let error = run_calibration(&snapshot, &BrokenModel(broken), &config()).unwrap_err();
        assert!(
            matches!(
                error,
                CalibrationError::NonFinitePrediction { .. }
                    | CalibrationError::PredictionOutOfRange { .. }
            ),
            "a head returning {broken} must be refused, got {error:?}"
        );
    }

    // And the same refusal surfaces from the bare projection entry point, not
    // only through the full run.
    let error =
        project_cohorts(&snapshot, &BrokenModel(f64::NAN), DEFAULT_PROBABILITY_FLOOR).unwrap_err();
    assert!(matches!(
        error,
        CalibrationError::NonFinitePrediction { .. }
    ));
}

/// GATE 5, sixth part: every configuration that would make a gate meaningless is
/// refused before a single row is read.
#[test]
fn a_meaningless_configuration_is_refused_before_anything_is_read() {
    let snapshot = render(&main_fixture(200), 103);
    let base = config();

    let cases: Vec<(CalibrationConfig, &str)> = vec![
        (
            CalibrationConfig {
                reliability: ReliabilityConfig {
                    bin_count: 0,
                    ..base.reliability
                },
                ..base
            },
            "zero bins is a curve with no bins",
        ),
        (
            CalibrationConfig {
                drift: DriftConfig {
                    bin_count: 0,
                    ..base.drift
                },
                ..base
            },
            "zero drift bins is an index with no bins",
        ),
        (
            CalibrationConfig {
                reliability: ReliabilityConfig {
                    max_expected_calibration_error: -0.1,
                    ..base.reliability
                },
                ..base
            },
            "a negative ece ceiling could never be met",
        ),
        (
            CalibrationConfig {
                reliability: ReliabilityConfig {
                    max_expected_calibration_error: f64::INFINITY,
                    ..base.reliability
                },
                ..base
            },
            "an infinite ece ceiling could always be met",
        ),
        (
            CalibrationConfig {
                reliability: ReliabilityConfig {
                    max_candidate_calibration_error: -1.0,
                    ..base.reliability
                },
                ..base
            },
            "a negative per-candidate ceiling could never be met",
        ),
        (
            CalibrationConfig {
                reliability: ReliabilityConfig {
                    max_calibration_error: f64::NAN,
                    ..base.reliability
                },
                ..base
            },
            "a NaN ceiling is not a comparison",
        ),
        (
            CalibrationConfig {
                drift: DriftConfig {
                    max_population_stability_index: -0.5,
                    ..base.drift
                },
                ..base
            },
            "a negative psi ceiling could never be met",
        ),
        (
            CalibrationConfig {
                drift: DriftConfig {
                    max_base_rate_delta: 1.5,
                    ..base.drift
                },
                ..base
            },
            "a base-rate ceiling above one is not a difference of two rates",
        ),
        (
            CalibrationConfig {
                holdout: HoldoutConfig {
                    holdout_cohorts: 0,
                    ..base.holdout
                },
                ..base
            },
            "a holdout of zero decisions decides nothing",
        ),
        (
            CalibrationConfig {
                holdout: HoldoutConfig {
                    min_fit_cohorts: 0,
                    ..base.holdout
                },
                ..base
            },
            "a fit floor of zero would permit fitting on nothing",
        ),
        (
            CalibrationConfig {
                holdout: HoldoutConfig {
                    min_attributed_outcomes: 0,
                    ..base.holdout
                },
                ..base
            },
            "an attributed floor of zero would pass what it exists to fail",
        ),
        (
            CalibrationConfig {
                fit: FitConfig {
                    iterations: 0,
                    ..base.fit
                },
                ..base
            },
            "zero gradient steps would report an unfitted calibrator as fitted",
        ),
        (
            CalibrationConfig {
                fit: FitConfig {
                    learning_rate: 0.0,
                    ..base.fit
                },
                ..base
            },
            "a zero step size never moves the parameters",
        ),
        (
            CalibrationConfig {
                fit: FitConfig {
                    min_temperature: 0.0,
                    ..base.fit
                },
                ..base
            },
            "a zero temperature collapses the vector onto one candidate",
        ),
        (
            CalibrationConfig {
                fit: FitConfig {
                    max_temperature: 0.01,
                    ..base.fit
                },
                ..base
            },
            "a maximum temperature below the floor pins the fit",
        ),
        (
            CalibrationConfig {
                fit: FitConfig {
                    intercept_l2: -1.0,
                    ..base.fit
                },
                ..base
            },
            "negative regularization is an anti-prior",
        ),
        (
            CalibrationConfig {
                probability_floor: 0.0,
                ..base
            },
            "a zero probability floor makes ln(0) reachable",
        ),
        (
            CalibrationConfig {
                probability_floor: 0.5,
                ..base
            },
            "a floor of one half collapses the unit interval",
        ),
    ];

    for (candidate, why) in cases {
        let error = match run_calibration(&snapshot, &honest_model(), &candidate) {
            Ok(outcome) => panic!(
                "{why}: expected a configuration refusal, got a verdict of {:?}",
                outcome.report().verdict
            ),
            Err(error) => error,
        };
        assert!(
            !error.to_string().is_empty(),
            "{why}: the refusal must carry a reason"
        );
        assert!(
            !matches!(error, CalibrationError::DistributionShift { .. }),
            "{why}: a configuration must be refused before any partition is examined"
        );
    }
}

/// GATE 5, seventh part: a measurement asked for over nothing is a refusal, not
/// an empty-but-plausible report.
#[test]
fn measuring_nothing_is_a_refusal() {
    let error = measure_emitted(&[], &ReliabilityConfig::default())
        .expect_err("no observations is not a measurement");
    assert!(
        matches!(error, CalibrationError::NoObservations { .. }),
        "got {error:?}"
    );

    let error = measure_emitted(
        &[],
        &ReliabilityConfig {
            bin_count: 0,
            ..ReliabilityConfig::default()
        },
    )
    .expect_err("zero bins is checked first");
    assert!(
        matches!(error, CalibrationError::ZeroReliabilityBins { .. }),
        "got {error:?}"
    );

    let error = measure_marginal(&[], MarginalView::Calibrated, &ReliabilityConfig::default())
        .expect_err("no observations is not a measurement");
    assert!(
        matches!(error, CalibrationError::NoObservations { .. }),
        "got {error:?}"
    );
}

/// GATE 5, eighth part: the module holds no panic path.
///
/// The whole contract is "fail closed, never panic", so a bare `unwrap`, an
/// `expect`, or a `panic!` in this module would be exactly the surface the
/// contract forbids. The tokens are checked against the module's own source.
#[test]
fn the_calibration_module_holds_no_panic_path() {
    // `calibration.rs` became `calibration/`, so the fence reads every file of
    // the module, the way the router and policy fences do.
    let source = CALIBRATION_SOURCES
        .iter()
        .map(|(_name, src)| *src)
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in [
        "unwrap(",
        ".expect(",
        "panic!",
        "unreachable!",
        "todo!",
        "unimplemented!",
        "assert!",
        "assert_eq!",
    ] {
        assert!(
            !source.contains(forbidden),
            "calibration.rs must not contain {forbidden}"
        );
    }
}
