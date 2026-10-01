#![cfg(feature = "ml")]

//! Node 7E-3 — the float round-trip evidence.
//!
//! This suite exists to turn a documented hazard into a measurement.
//!
//! `serde_json` is used in this workspace **without** its `float_roundtrip`
//! feature. Its `f64` parser is therefore a fast path that multiplies or
//! divides the significand by a power of ten rather than correctly rounding
//! the decimal, so a value can come back **one ULP different** after a round
//! trip. The parent recorded that against the journal node as E-089 (5 of 11
//! arbitrary finite `f64` came back one ULP different; safe decimals such as
//! 0.1, 1.3 and one third did not).
//!
//! This node consumes canonical samples whose `f64` targets *are* carried
//! through that path, so the drift is real measurement error here rather than a
//! theoretical one. The suite pins down three things:
//!
//! 1. the drift is real, it is counted, and the worst offender is named —
//!    [`JournalFloatFidelity`] reports it rather than absorbing it;
//! 2. the `f32` feature vector the replay is driven by survives the same path
//!    bit-exactly, which is *why* replay equivalence stays decidable at all;
//! 3. the model artifact does **not** survive plain JSON, and 7E-2F's bit-exact
//!    wire form is what makes it transportable — which is why the gate measures
//!    the commit through that wire form.
//!
//! Nothing here asserts that the workspace's float handling is good. It
//! asserts that this gate measures it, reports it, and refuses on it.

use zroutery_core::failure::FailureClass;
use zroutery_core::feedback::DataOrigin;
use zroutery_core::ml::activation::{
    snapshot_id_for, CommitFile, SnapshotFile, ACTIVATION_SNAPSHOT_SCHEMA_VERSION,
};
use zroutery_core::ml::dataset::{Targets, TrainingSample as DatasetTrainingSample};
use zroutery_core::ml::evaluation::{f32_identical, f64_identical, ulp_distance};
use zroutery_core::ml::features::{RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use zroutery_core::ml::shadow::ModelEnsemblePredictor;
use zroutery_core::ml::offline_gate::JournalFloatFidelity;
use zroutery_core::outcome::Attempt;

// ---------------------------------------------------------------------------
// A canonical sample with controllable f64 targets
// ---------------------------------------------------------------------------

/// A canonical sample whose `f64` targets are whatever the caller puts there.
///
/// Hand-built rather than derived from an `Outcome`, because this suite is
/// about the *transport* of the values, not about where they came from.
fn sample_with(id: &str, latency: f64, cost: f64) -> Outcome2 {
    let mut values = [0.0f32; FEATURE_DIMENSION];
    for (index, value) in values.iter_mut().enumerate() {
        *value = (index as f32 + 1.0) / 64.0;
    }
    Outcome2 {
        sample_id: id.to_string(),
        schema_version: FEATURE_SCHEMA_VERSION,
        timestamp: 1_700_000_000,
        streaming: false,
        dialect: "openai".to_string(),
        features: RoutingFeatures {
            values,
            schema_version: FEATURE_SCHEMA_VERSION,
        },
        targets: Targets {
            success: true,
            latency_ms: Some(latency),
            ttft_ms: Some(latency / 3.0),
            cost: Some(cost),
            failure_class: None,
            fallback_count: 0,
        },
        provider_id: "prov-a".to_string(),
        model_id: "alpha".to_string(),
        origin: DataOrigin::Native,
        outcome_id: format!("out-{id}"),
        request_id: format!("req-{id}"),
        decision_id: Some(format!("dec-{id}")),
        response_id: None,
        final_status: zroutery_core::outcome::FinalStatus::Success,
        success: true,
        identity: Default::default(),
        scope: zroutery_core::ml::dataset::SampleScope::Request,
        attempt_id: None,
        rectified: false,
        attempts: vec![Attempt {
            attempt_id: format!("att-{id}"),
            candidate_model: "alpha".to_string(),
            candidate_provider: "prov-a".to_string(),
            started_at: 1_700_000_000,
            completed_at: 1_700_000_001,
            latency_ms: latency,
            ttft_ms: Some(latency / 3.0),
            success: true,
            failure_class: None,
            failure_message: None,
            http_status: Some(200),
            rectified: false,
        }],
        usage: None,
        estimated_cost: Some(cost),
        actual_cost: Some(cost),
        terminal_error: None,
        feedback: None,
    }
}

/// The accepted canonical sample type, named locally so the fixture above reads
/// as a sample rather than as a type alias.
type Outcome2 = zroutery_core::ml::dataset::OutcomeTrainingSample;

/// A deterministic spread of arbitrary finite `f64` bit patterns.
///
/// A tiny LCG over the bit space, skipping the ranges that are not finite.
/// Nothing here is a "realistic" number: that is the point — E-089's finding
/// was that *arbitrary* values drift while hand-written safe decimals do not,
/// and a fixture made only of tidy decimals could never detect the hazard.
fn arbitrary_f64(count: usize) -> Vec<f64> {
    let mut state: u64 = 0x243F_6A88_85A3_08D3;
    let mut values = Vec::with_capacity(count);
    while values.len() < count {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let exponent = ((state >> 52) & 0x7FF) as i64 - 1023;
        // Keep ordinary magnitudes: no subnormals, no overflow, and a positive
        // exponent range wide enough to include the awkward cases.
        if !(-40..=40).contains(&exponent) {
            continue;
        }
        let bits = (state & 0x800F_FFFF_FFFF_FFFF) | (((exponent + 1023) as u64) << 52);
        let value = f64::from_bits(bits);
        if value.is_finite() && value > 0.0 {
            values.push(value);
        }
    }
    values
}

// ---------------------------------------------------------------------------
// 1. The drift is real, counted, and named
// ---------------------------------------------------------------------------

#[test]
fn every_arbitrary_f64_target_survives_this_workspaces_read_path() {
    let values = arbitrary_f64(256);
    let samples: Vec<Outcome2> = values
        .iter()
        .enumerate()
        .map(|(index, value)| sample_with(&format!("s{index:04}"), *value, *value))
        .collect();

    let fidelity = JournalFloatFidelity::measure(&samples);

    assert_eq!(fidelity.samples, samples.len());
    assert!(fidelity.f64_fields > 0, "there must be fields to measure");
    // This used to assert `moved_fields > 0`, on the reasoning that a
    // measurement reporting zero would mean the measurement was wrong rather
    // than that the transport was safe. That reasoning was right while
    // `serde_json` parsed floats without `float_roundtrip`, and this test
    // measured 367 of 1792 fields moving at one ULP.
    //
    // The workspace now enables `float_roundtrip`, so the hazard class is empty
    // and the assertion has flipped with it. That makes this the regression test
    // for the fix rather than for the bug: removing the feature makes
    // `moved_fields` climb again and fails here, naming the cause.
    assert_eq!(
        fidelity.moved_fields, 0,
        "arbitrary f64 targets must now survive bit for bit. If this fails, serde_json's \
         `float_roundtrip` was removed or stopped applying to this workspace, and every \
         byte-exactness claim in the ml tree is void until it is restored. Before it was \
         enabled this same assertion read 367 of {} fields moving.",
        fidelity.f64_fields
    );
    assert!(
        fidelity.round_trip_exact,
        "a zero moved count is what makes the exact claim available"
    );
    assert_eq!(fidelity.max_ulp, 0);
    assert!(
        fidelity.worst.is_none(),
        "there is no offending field to name, because nothing moved"
    );
    assert!(
        fidelity.moved_fields <= fidelity.f64_fields,
        "a field cannot move twice"
    );

    println!(
        "arbitrary f64 round trip: {} of {} fields moved, max {} ulp",
        fidelity.moved_fields, fidelity.f64_fields, fidelity.max_ulp
    );
}

#[test]
fn safe_decimals_survive_this_workspaces_read_path_bit_exactly() {
    // The values a real latency/cost field tends to hold, and the ones E-089
    // recorded as surviving. This is the honest other half of the finding: the
    // transport is exact for these, which is why the gate measures rather than
    // assuming a universal failure.
    let values = [0.1_f64, 1.3, 1.0 / 3.0, 120.0, 40.0, 0.009, 0.005, 0.000_1, 1.0, 0.5];
    let samples: Vec<Outcome2> = values
        .iter()
        .enumerate()
        .map(|(index, value)| sample_with(&format!("safe{index:02}"), *value, *value))
        .collect();

    let fidelity = JournalFloatFidelity::measure(&samples);

    assert_eq!(fidelity.samples, samples.len());
    assert!(
        fidelity.round_trip_exact,
        "safe decimals must survive: {} of {} fields moved",
        fidelity.moved_fields,
        fidelity.f64_fields
    );
    assert_eq!(fidelity.moved_fields, 0);
    assert_eq!(fidelity.max_ulp, 0);
    assert!(fidelity.worst.is_none());
}

#[test]
fn the_measurement_is_about_the_f64_surface_and_not_the_features() {
    // The 32 f32 features are deliberately excluded: they take a different
    // path (see the next test), and folding them in would make one number
    // stand for two different round trips.
    let sample = sample_with("surface", 123.456_789, 0.012_345_678);
    let fidelity = JournalFloatFidelity::measure(std::slice::from_ref(&sample));

    // latency_ms, ttft_ms, cost, estimated_cost, actual_cost, and the attempt's
    // latency_ms and ttft_ms: seven f64 fields on this fixture.
    assert_eq!(
        fidelity.f64_fields, 7,
        "the measured field set must be exactly the sample's f64 surface"
    );
    assert_eq!(fidelity.samples, 1);
}

#[test]
fn an_empty_measurement_is_exact_and_says_so() {
    let fidelity = JournalFloatFidelity::measure(&[]);
    assert_eq!(fidelity.samples, 0);
    assert_eq!(fidelity.f64_fields, 0);
    assert_eq!(fidelity.moved_fields, 0);
    assert!(fidelity.round_trip_exact);
    assert!(fidelity.worst.is_none());
}

// ---------------------------------------------------------------------------
// 2. The f32 feature vector survives — which is why replay stays decidable
// ---------------------------------------------------------------------------

#[test]
fn the_f32_feature_vector_the_replay_is_driven_by_survives_the_same_path() {
    // Every retained feature position takes the same route as the f64 targets
    // and comes back bit-identical. That is the load-bearing fact behind this
    // node's design: replay equivalence compares model *outputs* recomputed
    // from these vectors, so the vectors have to be exact, and they are.
    let mut values = [0.0f32; FEATURE_DIMENSION];
    for (index, value) in values.iter_mut().enumerate() {
        // Deliberately awkward decimals, not tidy tenths.
        *value = 0.1 + index as f32 * 0.037;
    }
    let original = RoutingFeatures {
        values,
        schema_version: FEATURE_SCHEMA_VERSION,
    };

    let mut sample = sample_with("features", 10.0, 0.5);
    sample.features = original.clone();

    let bytes = serde_json::to_vec(&sample).expect("the sample serializes");
    let reparsed: Outcome2 = serde_json::from_slice(&bytes).expect("the sample re-parses");

    assert_eq!(reparsed.features.values.len(), FEATURE_DIMENSION);
    assert_eq!(reparsed.features.schema_version, FEATURE_SCHEMA_VERSION);
    for (index, (before, after)) in original
        .values
        .iter()
        .zip(reparsed.features.values.iter())
        .enumerate()
    {
        assert!(
            f32_identical(*before, *after),
            "retained feature {index} moved: {before:?} -> {after:?}"
        );
    }
}

#[test]
fn a_float_that_moved_is_not_treated_as_equal_by_the_fidelity_measurement() {
    // The reason the comparison is on bits and not on `==`: one ULP is a real
    // difference, and `==` would hide it.
    let moved = f64::from_bits(0.1_f64.to_bits() + 1);
    assert_eq!(ulp_distance(0.1, moved), 1);
    assert_ne!(0.1, moved);
    assert!(!f64_identical(0.1, moved));
}

// ---------------------------------------------------------------------------
// 3. The model artifact needs the bit-exact wire form
// ---------------------------------------------------------------------------

#[test]
fn a_trained_checkpoint_survives_the_bit_exact_wire_form_and_still_verifies() {
    // This is 7E-2F's answer to the same hazard, and the gate measures the
    // commit through it. Enabling `float_roundtrip` in the workspace manifest
    // would fix the general problem too, but that is a global change outside
    // any one node's ownership — both 7E-2E and 7E-2F recorded that — so this
    // node consumes the wire form instead of reaching for the manifest.
    let root = ModelEnsemblePredictor::genesis().commit_record();
    let from_root = ModelEnsemblePredictor::from_model_commit_with_lineage(
        &root,
        std::slice::from_ref(&root),
    )
    .expect("the genesis root lineage verifies");

    let mut samples: Vec<DatasetTrainingSample> = (0..48)
        .map(|index| {
            let mut values = [0.0f32; FEATURE_DIMENSION];
            for (position, value) in values.iter_mut().enumerate() {
                *value = ((index * 7 + position * 13) % 100) as f32 / 100.0;
            }
            DatasetTrainingSample {
                sample_id: format!("fit-{index:04}"),
                schema_version: FEATURE_SCHEMA_VERSION,
                timestamp: 1_700_000_000 + index as i64,
                features: RoutingFeatures {
                    values,
                    schema_version: FEATURE_SCHEMA_VERSION,
                },
                targets: Targets {
                    success: index % 2 == 0,
                    latency_ms: Some(100.0 + index as f64 * 1.7),
                    ttft_ms: Some(30.0 + index as f64 * 0.9),
                    cost: Some(0.004 + index as f64 * 0.000_137),
                    failure_class: if index % 2 == 0 {
                        None
                    } else {
                        Some("Timeout".to_string())
                    },
                    fallback_count: 0,
                },
                provider_id: "prov-a".to_string(),
                model_id: "alpha".to_string(),
                origin: DataOrigin::Native,
                outcome_id: format!("out-fit-{index:04}"),
                feedback: Vec::new(),
            }
        })
        .collect();
    samples.shrink_to_fit();

    let (_, trained) = from_root
        .try_train(&samples)
        .expect("the fixture trains");
    assert!(trained.verify(), "the trained commit must verify before transport");

    // The bit-exact wire form: each f64 parameter as 16 hex digits of its bits.
    let snapshot_id = snapshot_id_for(&trained);
    let file = SnapshotFile {
        schema_version: ACTIVATION_SNAPSHOT_SCHEMA_VERSION,
        snapshot_id: snapshot_id.as_str().to_string(),
        commit: CommitFile::from_commit(&trained),
        created_at: trained.created_at,
    };
    let bytes = serde_json::to_vec(&file).expect("the wire form serializes");
    let reparsed: SnapshotFile = serde_json::from_slice(&bytes).expect("the wire form re-parses");
    let rebuilt = reparsed
        .try_into_snapshot(&snapshot_id)
        .expect("the wire form rebuilds a verifying snapshot");

    assert!(
        rebuilt.commit().verify(),
        "a commit that came back through the bit-exact wire form must still verify"
    );
    assert_eq!(rebuilt.commit().commit_id, trained.commit_id);
    assert_eq!(rebuilt.commit().checkpoint.content_hash(), trained.checkpoint.content_hash());
    assert_eq!(
        rebuilt.commit().checkpoint.success.parameters,
        trained.checkpoint.success.parameters,
        "every f64 parameter must be bit-identical after the wire form"
    );
    for (before, after) in trained
        .checkpoint
        .success
        .parameters
        .iter()
        .zip(rebuilt.commit().checkpoint.success.parameters.iter())
    {
        assert!(f64_identical(*before, *after), "a parameter moved: {before:?} -> {after:?}");
    }
}

#[test]
fn a_checkpoint_carrying_arbitrary_parameters_is_what_the_wire_form_exists_for() {
    // Show the hazard the wire form removes, on the artifact path, without
    // asserting a claim about the manifest.
    //
    // The trained parameters are arbitrary trained floats. If they happen to
    // survive plain JSON on this toolchain, the wire form is still the correct
    // transport — it is *guaranteed* lossless rather than incidentally
    // lossless — so this test asserts the guarantee, and records what plain
    // JSON happened to do rather than failing on it.
    let root = ModelEnsemblePredictor::genesis().commit_record();
    let from_root =
        ModelEnsemblePredictor::from_model_commit_with_lineage(&root, std::slice::from_ref(&root))
            .expect("the genesis root lineage verifies");
    let mut samples: Vec<DatasetTrainingSample> = (0..48)
        .map(|index| {
            let mut values = [0.0f32; FEATURE_DIMENSION];
            for (position, value) in values.iter_mut().enumerate() {
                *value = ((index * 11 + position * 17) % 89) as f32 / 89.0;
            }
            DatasetTrainingSample {
                sample_id: format!("fit-{index:04}"),
                schema_version: FEATURE_SCHEMA_VERSION,
                timestamp: 1_700_000_000 + index as i64,
                features: RoutingFeatures {
                    values,
                    schema_version: FEATURE_SCHEMA_VERSION,
                },
                targets: Targets {
                    success: index % 3 != 0,
                    latency_ms: Some(80.0 + index as f64 * 2.3),
                    ttft_ms: Some(20.0 + index as f64 * 1.1),
                    cost: Some(0.003 + index as f64 * 0.000_271),
                    failure_class: None,
                    fallback_count: 0,
                },
                provider_id: "prov-b".to_string(),
                model_id: "bravo".to_string(),
                origin: DataOrigin::Native,
                outcome_id: format!("out-fit-{index:04}"),
                feedback: Vec::new(),
            }
        })
        .collect();
    samples.shrink_to_fit();

    let (_, trained) = from_root.try_train(&samples).expect("the fixture trains");

    let plain = serde_json::to_vec(&trained).expect("plain JSON serializes");
    let plain_reparsed: zroutery_core::ml::model_identity::ModelCommit =
        serde_json::from_slice(&plain).expect("plain JSON re-parses");
    let moved = trained
        .checkpoint
        .success
        .parameters
        .iter()
        .zip(plain_reparsed.checkpoint.success.parameters.iter())
        .filter(|(before, after)| !f64_identical(**before, **after))
        .count();
    let plain_verifies = plain_reparsed.verify();
    println!(
        "plain JSON checkpoint round trip: {moved} of {} success parameters moved, \
         verification after round trip: {plain_verifies}",
        trained.checkpoint.success.parameters.len()
    );

    // Whatever plain JSON did here, the wire form is lossless, and that is the
    // property the gate actually relies on.
    let snapshot_id = snapshot_id_for(&trained);
    let file = SnapshotFile {
        schema_version: ACTIVATION_SNAPSHOT_SCHEMA_VERSION,
        snapshot_id: snapshot_id.as_str().to_string(),
        commit: CommitFile::from_commit(&trained),
        created_at: trained.created_at,
    };
    let bytes = serde_json::to_vec(&file).expect("the wire form serializes");
    let reparsed: SnapshotFile = serde_json::from_slice(&bytes).expect("the wire form re-parses");
    let rebuilt = reparsed
        .try_into_snapshot(&snapshot_id)
        .expect("the wire form always rebuilds");
    assert!(
        rebuilt.commit().verify(),
        "the wire form is guaranteed lossless by construction. It used to be that plain \
         JSON was NOT, which is why this module has its own form; `float_roundtrip` is now \
         enabled workspace-wide, so plain JSON is lossless here too and this form is \
         redundancy rather than necessity"
    );
    assert_eq!(
        rebuilt.commit().checkpoint.success.parameters,
        trained.checkpoint.success.parameters
    );
}

#[test]
fn the_outcome_attempt_latency_is_part_of_the_measured_surface() {
    // The attempt chain is retained on the sample, so its f64 latencies travel
    // through the same path and are measured with the same rigour.
    let mut sample = sample_with("chain", 250.0, 0.02);
    sample.attempts.push(Attempt {
        attempt_id: "att-chain-1".to_string(),
        candidate_model: "bravo".to_string(),
        candidate_provider: "prov-b".to_string(),
        started_at: 1_700_000_000,
        completed_at: 1_700_000_002,
        latency_ms: 31.7,
        ttft_ms: None,
        success: false,
        failure_class: Some(FailureClass::Timeout),
        failure_message: Some("timed out".to_string()),
        http_status: Some(504),
        rectified: false,
    });

    let fidelity = JournalFloatFidelity::measure(std::slice::from_ref(&sample));
    // Seven from the first attempt's sample plus the second attempt's latency;
    // its `ttft_ms` is `None` on both sides and is not compared.
    assert_eq!(fidelity.f64_fields, 8);
    assert!(fidelity.round_trip_exact);
}

#[test]
fn the_reexported_comparison_helpers_are_the_same_functions() {
    // The exact-comparison primitives are re-exported from `ml` so a consumer
    // comparing a report against a stored value uses one implementation rather
    // than a second comparison that could disagree with the gate's.
    assert_eq!(
        zroutery_core::ml::ulp_distance(1.0, 1.0 + f64::EPSILON),
        ulp_distance(1.0, 1.0 + f64::EPSILON)
    );
    assert!(zroutery_core::ml::f64_identical(0.1, 0.1));
    assert!(zroutery_core::ml::f32_identical(0.25f32, 0.25f32));
}
