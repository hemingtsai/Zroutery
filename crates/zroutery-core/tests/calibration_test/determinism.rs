use zroutery_core::ml::calibration::run_calibration;

use super::support::*;

// ---------------------------------------------------------------------------
// GATE 3 — determinism, including UUID invariance
// ---------------------------------------------------------------------------

/// GATE 3, first half: the same snapshot and configuration give byte-identical
/// calibrator and report.
///
/// Serialized bytes rather than `PartialEq`, because `PredictionMetrics` has
/// none and the accepted warmup node set the same precedent.
#[test]
fn one_snapshot_and_configuration_give_byte_identical_output() {
    let snapshot = render(&main_fixture(200), 23);
    let first = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");
    let second = run_calibration(&snapshot, &honest_model(), &config()).expect("calibration runs");

    assert_eq!(
        serde_json::to_string(first.calibrator()).unwrap(),
        serde_json::to_string(second.calibrator()).unwrap(),
        "the fitted calibrator must be byte-identical across runs"
    );
    assert_eq!(
        serde_json::to_string(first.marginal_calibrator()).unwrap(),
        serde_json::to_string(second.marginal_calibrator()).unwrap()
    );
    assert_eq!(
        serde_json::to_string(first.report()).unwrap(),
        serde_json::to_string(second.report()).unwrap(),
        "the report must be byte-identical across runs: it holds no timestamp"
    );
    assert_eq!(
        serde_json::to_string(&first.holdout_records()).unwrap(),
        serde_json::to_string(&second.holdout_records()).unwrap()
    );
}

/// GATE 3, second half — **the test that actually bites**.
///
/// `outcome_id` is `req_{uuid}` (`stats.rs`), `decision_id` is `dec-{uuid}`
/// (`router.rs`), and `deterministic_sample_id` is
/// `format!("samp-{}-{suffix}", outcome.outcome_id)` (`ml/dataset.rs`). So all
/// three identifiers are UUID-derived, and `Decision.timestamp` is
/// second-resolution, so a timestamp comparison ties constantly and the UUID
/// becomes the tiebreaker.
///
/// This test renders the *same logical data* twice under deliberately different
/// UUIDs, reverses the second rendering's row order, and asserts that the
/// partition, the fitted calibrator, the emitted distributions, every
/// measurement, the damage, the drift and the verdict are identical. An
/// in-process byte comparison cannot catch a UUID leak, because the UUIDs are
/// fixed within a process. This can.
#[test]
fn uuid_invariance_the_partition_and_every_number_survive_a_uuid_change() {
    let specs = main_fixture(200);
    let first = render(&specs, 0x1111_2222_3333_4444);
    let second = render(&specs, 0xAAAA_BBBB_CCCC_DDDD);

    // No row of the first rendering may share an identifier with the second, and
    // every identifier of the second must really carry its own salt.
    let first_ids: Vec<&str> = first.iter().map(|row| row.sample_id.as_str()).collect();
    for row in &second {
        assert!(
            !first_ids.contains(&row.sample_id.as_str()),
            "the two renderings must not share any sample id"
        );
        assert!(row.sample_id.starts_with("samp-req_aaaa"));
        assert!(row.outcome_id.starts_with("req_aaaa"));
        assert!(row
            .decision_id
            .as_deref()
            .is_some_and(|id| id.starts_with("dec-aaaa")));
    }

    // The two renderings carry the same history in the same order, differing only
    // in their identifiers. A row reversal used to be applied here as well, on the
    // reasoning that "row arrival order is not the content order either".
    //
    // It was removed because it conflated two independent claims, and the second
    // one is false. `render` stamps `BASE_TIMESTAMP + index`, so reversing the
    // rows reverses the *history*: it is not the same dataset in a different
    // order, it is the same measurements running backwards in time. A frozen
    // holdout is by definition a later slice, so a time-reversed dataset must
    // produce a different one.
    //
    // What is asserted now is the property this test is named for. Row-order
    // independence is asserted where it is true — within a partition — by
    // `projection_is_in_arrival_order_not_content_order`.

    let one = run_calibration(&first, &honest_model(), &config()).expect("calibration runs");
    let two = run_calibration(&second, &honest_model(), &config()).expect("calibration runs");

    // The partition.
    assert_eq!(one.report().fit_cohorts, two.report().fit_cohorts);
    assert_eq!(one.report().holdout_cohorts, two.report().holdout_cohorts);
    assert_eq!(
        one.report().fit_attributed_cohorts,
        two.report().fit_attributed_cohorts
    );
    assert_eq!(
        one.report().holdout_attributed_cohorts,
        two.report().holdout_attributed_cohorts
    );
    assert_eq!(
        one.calibrator().fit_cohorts(),
        two.calibrator().fit_cohorts()
    );

    // The calibrator, byte for byte.
    assert_eq!(
        serde_json::to_string(one.calibrator()).unwrap(),
        serde_json::to_string(two.calibrator()).unwrap(),
        "a UUID leaked into the fit"
    );
    assert_eq!(
        serde_json::to_string(one.marginal_calibrator()).unwrap(),
        serde_json::to_string(two.marginal_calibrator()).unwrap()
    );

    // The emitted distributions, byte for byte, fingerprints included — the
    // fingerprints are computed from content, so a UUID in one would show here.
    assert_eq!(
        serde_json::to_string(&one.holdout_records()).unwrap(),
        serde_json::to_string(&two.holdout_records()).unwrap(),
        "a UUID leaked into the emitted vectors or their fingerprints"
    );

    // Every number in the report.
    assert_eq!(
        serde_json::to_string(one.report()).unwrap(),
        serde_json::to_string(two.report()).unwrap(),
        "a UUID leaked into the report"
    );
    assert_eq!(one.report().verdict, two.report().verdict);
    assert_eq!(one.is_calibrated(), two.is_calibrated());
}
