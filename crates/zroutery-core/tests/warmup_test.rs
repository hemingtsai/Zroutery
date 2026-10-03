#![cfg(feature = "ml")]

//! Node 7E-2B gate tests: offline supervised warmup.
//!
//! The claims under test are the ones this node owns —
//!
//! 1. purity: warmup is a pure library call that reaches no installation
//!    surface (structural tripwire plus a live-predictor behavioural check);
//! 2. determinism: one snapshot and one configuration produce byte-identical
//!    model state and one identical commit id;
//! 3. lineage: the produced commit verifies, round-trips through the accepted
//!    lineage loader, and names the parent it really has;
//! 4. holdout honesty: the partitions are disjoint, the comparison is against
//!    the accepted cold baseline, a deliberately bad model is reported as worse,
//!    and a warmup that cannot improve refuses to claim that it did;
//! 5. label discipline: a failed, cancelled, or interrupted request cannot
//!    become a positive success label through the projection warmup uses, and
//!    the per-dimension targets keep their `None` discipline;
//! 6. fail-closed: every malformed input is a typed refusal with a reason.

use std::collections::HashSet;

use zroutery_core::failure::FailureClass;
use zroutery_core::feedback::DataOrigin;
use zroutery_core::ml::coordinator::CoordinatorConfig;
use zroutery_core::ml::dataset::{
    outcome_to_dataset_sample, try_samples_from_outcome, OutcomeTrainingSample, SampleScope,
    TrainingSample as DatasetTrainingSample,
};
use zroutery_core::ml::decision_contract::{DecisionDimension, DecisionModel, DecisionModelStates};
use zroutery_core::ml::decision_engine::DecisionEngine;
use zroutery_core::ml::features::{
    RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION, F_CONTEXT_TOKENS,
};
use zroutery_core::ml::model::RoutingModel;
use zroutery_core::ml::model_identity::{ModelEnsemble, ReplayError};
use zroutery_core::ml::reward::RewardPolicy;
use zroutery_core::ml::shadow::{ModelEnsemblePredictor, ShadowDecision, ShadowEngine};
use zroutery_core::ml::warmup::{
    head_scope, partition_snapshot, project_for_training, run_warmup, HeadScope, WarmupConfig,
    WarmupError, WarmupOutcome, WarmupVerdict, BASELINE_DESCRIPTION,
};
use zroutery_core::outcome::{Attempt, FinalStatus, Outcome};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The terminal state a fixture outcome is built with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Terminal {
    Success,
    Failed,
    Cancelled,
    Interrupted,
}

const MODEL: &str = "model-a";
const PROVIDER: &str = "provider-a";

/// A real Outcome built through the accepted builder, so every fixture sample
/// travels the same collection path production does.
fn fixture_outcome(quality: f32, terminal: Terminal, timestamp: i64) -> Outcome {
    let success = terminal == Terminal::Success;
    // An unsuccessful attempt must carry a class, and a cancelled or
    // interrupted request must carry the class that explains it.
    let failure_class = match terminal {
        Terminal::Success => None,
        Terminal::Failed => Some(FailureClass::RateLimit),
        Terminal::Cancelled => Some(FailureClass::ClientCancelled),
        Terminal::Interrupted => Some(FailureClass::Interrupted),
    };
    let attempt = Attempt {
        attempt_id: format!("att_{timestamp}"),
        candidate_model: MODEL.to_string(),
        candidate_provider: PROVIDER.to_string(),
        started_at: timestamp,
        completed_at: timestamp + 1,
        latency_ms: f64::from(100.0 + quality * 400.0),
        ttft_ms: success.then(|| f64::from(50.0 + quality * 100.0)),
        success,
        failure_class,
        failure_message: failure_class.map(|class| format!("fixture {class:?}")),
        http_status: match terminal {
            Terminal::Success => Some(200),
            Terminal::Failed => Some(429),
            Terminal::Cancelled | Terminal::Interrupted => None,
        },
        rectified: false,
    };
    let builder = Outcome::builder(format!("req_{timestamp}"))
        .single_candidate(MODEL, PROVIDER)
        .dialect("openai")
        .streaming(true)
        .attempt(attempt)
        .total_latency_ms(f64::from(100.0 + quality * 400.0))
        .cost(Some(0.01), Some(0.009))
        .timestamp(timestamp);
    let builder = match terminal {
        Terminal::Success => builder.ttft_ms(f64::from(50.0 + quality * 100.0)),
        Terminal::Failed => builder,
        Terminal::Cancelled => builder.cancelled(),
        Terminal::Interrupted => builder.interrupted(),
    };
    builder.build()
}

/// The decision-time vector for a fixture row. One dimension carries the signal,
/// so a warmup that learns anything learns it from that dimension alone.
fn fixture_features(quality: f32) -> RoutingFeatures {
    let mut features = RoutingFeatures::default();
    features.values[F_CONTEXT_TOKENS] = quality;
    features
}

/// One canonical sample, collected from a real Outcome.
fn sample(quality: f32, terminal: Terminal, timestamp: i64) -> OutcomeTrainingSample {
    let outcome = fixture_outcome(quality, terminal, timestamp);
    outcome_to_dataset_sample(&outcome, fixture_features(quality), DataOrigin::Native)
        .expect("a fixture outcome must canonicalize")
}

/// A dataset whose success label is learnable from one feature dimension.
fn learnable_snapshot(rows: usize) -> Vec<OutcomeTrainingSample> {
    (0..rows)
        .map(|index| {
            let quality = if index % 2 == 0 { 0.15 } else { 0.85 };
            let terminal = if index % 2 == 0 {
                Terminal::Failed
            } else {
                Terminal::Success
            };
            sample(quality, terminal, 1_700_000_000 + index as i64)
        })
        .collect()
}

/// A dataset whose success label is *inverted across the split*: the training
/// partition is perfectly learnable, and the holdout carries exactly the
/// opposite correlation. A model trained on it is confidently wrong on the
/// holdout, which is the honest way to be a bad model — a balanced dataset with
/// random labels would be a coin flip rather than a test.
fn inverted_across_split_snapshot(rows: usize, split: usize) -> Vec<OutcomeTrainingSample> {
    (0..rows)
        .map(|index| {
            let quality = if index % 2 == 0 { 0.15 } else { 0.85 };
            let succeeds_when_even = index % 2 == 0;
            let terminal = match (index < split, succeeds_when_even) {
                (true, true) | (false, false) => Terminal::Success,
                (true, false) | (false, true) => Terminal::Failed,
            };
            sample(quality, terminal, 1_700_000_000 + index as i64)
        })
        .collect()
}

/// A dataset whose training partition and holdout disagree: every training row
/// failed and every holdout row succeeded. The model learns "nothing succeeds"
/// from the data it was given and is then confidently wrong.
fn shifted_across_split_snapshot(rows: usize, split: usize) -> Vec<OutcomeTrainingSample> {
    (0..rows)
        .map(|index| {
            let quality = if index % 2 == 0 { 0.15 } else { 0.85 };
            let terminal = if index < split {
                Terminal::Failed
            } else {
                Terminal::Success
            };
            sample(quality, terminal, 1_700_000_000 + index as i64)
        })
        .collect()
}

/// One canonical request in the multi-row shape production collects: an
/// attempt-scope row and a request-scope row that share a timestamp and a
/// request id. `outcome_to_dataset_sample` emits a single request row, so the
/// shared canonical generator is the only honest source of this shape.
fn multi_row_request(index: usize) -> Vec<OutcomeTrainingSample> {
    let quality = if index % 2 == 0 { 0.15 } else { 0.85 };
    let terminal = if index % 2 == 0 {
        Terminal::Failed
    } else {
        Terminal::Success
    };
    let outcome = fixture_outcome(quality, terminal, 1_700_000_000 + index as i64);
    let features = fixture_features(quality);
    try_samples_from_outcome(&outcome, &[features.clone(), features], DataOrigin::Native)
        .expect("the canonical generator emits one attempt row and one request row")
}

/// A dataset of `requests` requests, each contributing an attempt row and a
/// request row under one request id.
fn multi_row_snapshot(requests: usize) -> Vec<OutcomeTrainingSample> {
    (0..requests).flat_map(multi_row_request).collect()
}

/// One real request that fell back: candidate A failed after 1000 ms, candidate
/// B served in 100 ms, and the request as a whole took 1100 ms. This is the
/// shape whose attempt row and request row share B's feature vector while
/// carrying different durations.
fn mixed_scope_outcome(timestamp: i64) -> Outcome {
    let failed = Attempt {
        attempt_id: "attempt-a".to_string(),
        candidate_model: "model-a".to_string(),
        candidate_provider: "provider-a".to_string(),
        started_at: timestamp,
        completed_at: timestamp + 1,
        latency_ms: 1000.0,
        ttft_ms: None,
        success: false,
        failure_class: Some(FailureClass::RateLimit),
        failure_message: Some("fixture rate limit".to_string()),
        http_status: Some(429),
        rectified: false,
    };
    let served = Attempt {
        attempt_id: "attempt-b".to_string(),
        candidate_model: "model-b".to_string(),
        candidate_provider: "provider-b".to_string(),
        started_at: timestamp + 1,
        completed_at: timestamp + 3,
        latency_ms: 100.0,
        ttft_ms: Some(30.0),
        success: true,
        failure_class: None,
        failure_message: None,
        http_status: Some(200),
        rectified: false,
    };
    Outcome::builder(format!("req-mix-{timestamp}"))
        .initial("model-a", "provider-a")
        .final_candidate("model-b", "provider-b")
        .dialect("openai")
        .streaming(true)
        .attempt(failed)
        .attempt(served)
        .total_latency_ms(1100.0)
        .ttft_ms(30.0)
        .cost(Some(0.02), Some(0.009))
        .timestamp(timestamp)
        .build()
}

/// The canonical rows of [`mixed_scope_outcome`]: A's attempt, B's attempt, and
/// the request as a whole, with B's vector retained for the request scope the
/// way the decision-time collection path does.
fn mixed_scope_rows(timestamp: i64) -> Vec<OutcomeTrainingSample> {
    let outcome = mixed_scope_outcome(timestamp);
    try_samples_from_outcome(
        &outcome,
        &[
            fixture_features(0.15),
            fixture_features(0.85),
            fixture_features(0.85),
        ],
        DataOrigin::Native,
    )
    .expect("the canonical generator emits the attempt and request rows")
}

/// Whether two optional targets disagree: both present and different.
fn optional_targets_disagree(left: Option<f64>, right: Option<f64>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left != right)
}

/// The serialized per-dimension states of a trained ensemble, for a byte-level
/// comparison between two runs.
fn state_bytes(ensemble: &ModelEnsemble) -> Vec<String> {
    vec![
        serde_json::to_string(&ensemble.success.save()).expect("state serializes"),
        serde_json::to_string(&ensemble.latency.save()).expect("state serializes"),
        serde_json::to_string(&ensemble.ttft.save()).expect("state serializes"),
        serde_json::to_string(&ensemble.cost.save()).expect("state serializes"),
    ]
}

fn live_engine() -> ShadowEngine {
    ShadowEngine::new(
        DecisionEngine::new(CoordinatorConfig::default(), RewardPolicy::default()),
        true,
    )
}

fn error_of(result: Result<WarmupOutcome, WarmupError>) -> WarmupError {
    match result {
        Ok(_) => panic!("the run must be refused"),
        Err(error) => error,
    }
}

/// Everything about a stored shadow verdict that is a function of the pinned
/// commit and the input, and nothing about when it was recorded.
fn probe_fingerprint(decision: ShadowDecision) -> String {
    serde_json::to_string(&(
        &decision.observation.model_commit,
        &decision.shadow.model_commit,
        decision.decision_input_checksum,
        decision.decision_checksum,
        &decision.candidates,
    ))
    .expect("verdict serializes")
}

// ---------------------------------------------------------------------------
// Gate 1 + 5: purity, and no activation
// ---------------------------------------------------------------------------

/// GATE 1/5 (structural): this module's own source may not reach the
/// installation surface.
///
/// The tokens are deliberately code-shaped rather than prose-shaped, so a
/// documentation sentence about what the module does not do cannot satisfy or
/// trip the wire.
#[test]
fn warmup_source_reaches_no_activation_or_background_surface() {
    let source = include_str!("../src/ml/warmup.rs").to_lowercase();

    for forbidden in [
        // The only type that carries an installation call, and the calls
        // themselves under any spelling.
        "shadowengine",
        "swap",
        "try_train_and_swap",
        "from_trained_parts",
        // The infallible training wrapper, which panics on a bad sample.
        ".train(",
        // Live-state mutation.
        "rwlock",
        "crate::sync::write",
        "crate::sync::read",
        // A commit into a live store.
        "modelstore",
        "try_commit",
        // An implicit dependency on a store, and therefore on the wall clock.
        "datasetstore",
        // Server wiring.
        "appstate",
        "build_app",
        "serverhandle",
        "server::",
        // Background work, scheduling, and timers.
        "std::thread",
        "thread::spawn",
        "tokio::spawn",
        "spawn(",
        "sleep(",
        "interval(",
        "tokio::time",
        "tokio::task",
    ] {
        assert!(
            !source.contains(forbidden),
            "warmup.rs must not reference {forbidden}"
        );
    }
}

/// GATE 5 (structural, second half): warmup fails closed, so it holds no panic
/// path. An `expect`, an `unwrap`, or an `unreachable` here would be exactly the
/// never-panic surface the refusal contract forbids.
#[test]
fn warmup_source_holds_no_panic_path() {
    let source = include_str!("../src/ml/warmup.rs");
    for forbidden in [
        "unwrap(",
        ".expect(",
        "panic!",
        "unreachable!",
        "todo!",
        "unimplemented!",
    ] {
        assert!(
            !source.contains(forbidden),
            "warmup.rs must not contain {forbidden}"
        );
    }
}

/// GATE 1/5 (behavioural): a warmup run leaves the live predictor exactly as it
/// found it — same commit, same checkpoint content, same lineage, and the same
/// answers to a real counterfactual evaluation.
#[test]
fn a_warmup_run_leaves_the_live_predictor_unchanged() {
    let engine = live_engine();
    let before_commit = engine.predictor_commit();
    let before_checkpoint = engine.predictor_checkpoint();
    let before_lineage = engine.predictor_lineage();
    let before_verdict = engine
        .evaluate("req-probe", &probe_input())
        .map(probe_fingerprint);
    assert!(before_verdict.is_some(), "the probe must be evaluated");

    let outcome = run_warmup(&learnable_snapshot(40), &WarmupConfig::default())
        .expect("a well-formed snapshot warms up");
    assert!(outcome.commit().verify(), "the produced commit must verify");

    let after_commit = engine.predictor_commit();
    let after_checkpoint = engine.predictor_checkpoint();
    let after_lineage = engine.predictor_lineage();
    let after_verdict = engine
        .evaluate("req-probe", &probe_input())
        .map(probe_fingerprint);

    assert_eq!(before_commit.commit_id, after_commit.commit_id);
    assert_eq!(
        before_checkpoint.content_hash(),
        after_checkpoint.content_hash()
    );
    assert_eq!(before_lineage.len(), after_lineage.len());
    assert_eq!(
        before_commit.checkpoint.content_hash(),
        after_commit.checkpoint.content_hash()
    );
    assert_eq!(
        before_verdict, after_verdict,
        "the live counterfactual answer must be untouched by a warmup"
    );
    assert_eq!(
        engine.fault_count(),
        0,
        "a warmup must not fault the engine"
    );
    // The warmed commit is a different artifact from the installed one; the
    // point is that it is installed nowhere.
    assert_ne!(after_commit.commit_id, outcome.commit_id());
}

// ---------------------------------------------------------------------------
// Gate 2: determinism
// ---------------------------------------------------------------------------

/// GATE 2: one snapshot and one configuration produce byte-identical model state
/// and one identical commit id.
#[test]
fn two_runs_of_one_snapshot_agree_byte_for_byte() {
    let snapshot = learnable_snapshot(40);
    let config = WarmupConfig::default();

    let first = run_warmup(&snapshot, &config).expect("first run warms up");
    let second = run_warmup(&snapshot, &config).expect("second run warms up");

    assert_eq!(
        state_bytes(first.ensemble()),
        state_bytes(second.ensemble()),
        "model state must be byte-identical across runs"
    );
    assert_eq!(
        first.commit_id(),
        second.commit_id(),
        "the commit id must be identical across runs"
    );
    assert_eq!(
        first.commit().checkpoint.content_hash(),
        second.commit().checkpoint.content_hash()
    );
    assert_eq!(
        serde_json::to_string(first.report()).expect("report serializes"),
        serde_json::to_string(second.report()).expect("report serializes"),
        "the report must be identical across runs"
    );
    assert_eq!(first.report().verdict, second.report().verdict);
    assert_eq!(
        first.holdout().samples().len(),
        second.holdout().samples().len()
    );
    assert_eq!(first.lineage().len(), second.lineage().len());
}

/// GATE 2 (negative): a different dataset is a different artifact, so
/// determinism is not a claim that every snapshot trains the same model.
#[test]
fn a_different_dataset_produces_a_different_commit() {
    let first = run_warmup(&learnable_snapshot(40), &WarmupConfig::default())
        .expect("first snapshot warms up");
    let smaller = learnable_snapshot(40);
    let second =
        run_warmup(&smaller[..20], &WarmupConfig::default()).expect("smaller snapshot warms up");
    assert_ne!(first.commit_id(), second.commit_id());
}

// ---------------------------------------------------------------------------
// Gate 3: lineage and verifiability
// ---------------------------------------------------------------------------

/// GATE 3: the produced commit verifies, round-trips through the accepted
/// lineage loader, and records the parent it really has.
#[test]
fn the_produced_commit_verifies_and_round_trips_with_its_lineage() {
    let snapshot = learnable_snapshot(40);
    let outcome = run_warmup(&snapshot, &WarmupConfig::default()).expect("the snapshot warms up");
    let commit = outcome.commit();

    assert!(commit.verify(), "the produced commit must verify");
    assert!(commit.checkpoint.verify(), "the checkpoint must verify");

    // One record per trained sample, plus the cold genesis commit.
    let trained = outcome.report().train_samples;
    assert_eq!(commit.learning_event_count, trained as u64);
    assert_eq!(outcome.lineage().len(), trained + 1);
    assert!(
        outcome.lineage()[0].parent.is_none(),
        "the chain must start at a root commit"
    );
    assert!(
        commit.parent.is_some(),
        "a trained commit must not claim a genesis lineage it does not have"
    );
    assert_eq!(
        commit.parent,
        Some(outcome.lineage()[trained - 1].commit_id.clone())
    );
    assert_ne!(commit.commit_id, outcome.lineage()[0].commit_id);

    // The accepted loader accepts the commit together with the lineage warmup
    // returned, and rebuilds a predictor pinned to that exact commit.
    let rebuilt = ModelEnsemblePredictor::from_model_commit_with_lineage(commit, outcome.lineage())
        .expect("the returned lineage must load");
    assert_eq!(rebuilt.commit(), outcome.commit_id());
    assert!(rebuilt.verify());

    // A commit without its lineage is refused, which is what makes the returned
    // lineage load-bearing rather than decorative.
    assert!(matches!(
        ModelEnsemblePredictor::from_model_commit(commit),
        Err(ReplayError::LineageCorrupt { .. })
    ));
}

/// GATE 3 (cross-check): the chain warmup assembles one row at a time is the
/// same chain the accepted seam builds from the whole batch at once.
///
/// The accepted seam keeps its ordered history to itself, so warmup cannot read
/// the chain back; it has to rebuild it. This test is what makes that rebuild
/// trustworthy rather than merely plausible.
#[test]
fn the_assembled_chain_equals_the_accepted_single_shot_rebuild() {
    let snapshot = learnable_snapshot(12);
    let config = WarmupConfig::new(0.01, 1);
    let outcome = run_warmup(&snapshot, &config).expect("the snapshot warms up");
    assert_eq!(outcome.report().holdout_samples, 1);

    let mut legacy: Vec<DatasetTrainingSample> = snapshot
        .iter()
        .cloned()
        .map(OutcomeTrainingSample::into_legacy)
        .collect();
    legacy.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.sample_id.cmp(&right.sample_id))
    });
    let training = &legacy[..legacy.len() - 1];

    let (ensemble, single_shot) = ModelEnsemblePredictor::genesis()
        .try_train(training)
        .expect("the accepted seam trains the batch");
    assert_eq!(
        outcome.commit_id(),
        single_shot.commit_id,
        "the row-at-a-time chain must equal the accepted batch rebuild"
    );
    assert_eq!(state_bytes(outcome.ensemble()), state_bytes(&ensemble));
}

// ---------------------------------------------------------------------------
// Gate 4: holdout honesty
// ---------------------------------------------------------------------------

/// GATE 4: the two partitions are disjoint by construction, and the holdout is
/// the later evidence.
#[test]
fn the_holdout_is_disjoint_from_training_and_holds_the_later_rows() {
    let snapshot = learnable_snapshot(40);
    let config = WarmupConfig::default();
    let outcome = run_warmup(&snapshot, &config).expect("the snapshot warms up");
    let report = outcome.report();

    assert_eq!(
        report.train_samples + report.holdout_samples,
        report.samples_total
    );
    assert_eq!(report.samples_total, snapshot.len());
    assert!(report.holdout_disjoint_from_train);

    let holdout_ids: HashSet<&str> = outcome
        .holdout()
        .samples()
        .iter()
        .map(|sample| sample.sample_id.as_str())
        .collect();
    let mut latest = 0;
    for (index, sample) in snapshot.iter().enumerate() {
        if holdout_ids.contains(sample.sample_id.as_str()) {
            latest = latest.max(index);
        }
    }
    // The holdout is the trailing slice, so no training row is later than the
    // earliest holdout row.
    let earliest_holdout = snapshot
        .iter()
        .position(|sample| holdout_ids.contains(sample.sample_id.as_str()))
        .expect("the holdout holds at least one snapshot row");
    for (index, sample) in snapshot.iter().enumerate() {
        if index < earliest_holdout {
            assert!(
                !holdout_ids.contains(sample.sample_id.as_str()),
                "row {index} is in both partitions"
            );
        }
    }
    assert_eq!(latest, snapshot.len() - 1);
    assert!(outcome.holdout().len() >= config.min_holdout);
}

/// GATE 4: the split cuts between whole requests, never between the rows of one
/// request.
///
/// A canonical request contributes an attempt row and a request row under one
/// request id. The fixture below has 21 requests and 42 rows, and its alternating
/// success labels put a row boundary exactly between the two rows of one
/// request, so a row-boundary split would train on that request's attempt and
/// hold out its request row while still calling the holdout disjoint.
#[test]
fn the_partition_keeps_every_request_on_one_side() {
    let snapshot = multi_row_snapshot(21);
    assert_eq!(snapshot.len(), 42);
    let all_requests: HashSet<&str> = snapshot
        .iter()
        .map(|sample| sample.request_id.as_str())
        .collect();
    assert_eq!(all_requests.len(), 21);

    let config = WarmupConfig::default();
    let partition = partition_snapshot(&snapshot, &config).expect("the snapshot partitions");

    let train_requests: HashSet<&str> = partition
        .training
        .iter()
        .map(|sample| sample.request_id.as_str())
        .collect();
    let holdout_requests: HashSet<&str> = partition
        .holdout
        .iter()
        .map(|sample| sample.request_id.as_str())
        .collect();
    assert!(
        train_requests.is_disjoint(&holdout_requests),
        "no request may be on both sides: {train_requests:?} / {holdout_requests:?}"
    );
    assert_eq!(
        train_requests.len() + holdout_requests.len(),
        all_requests.len(),
        "every request must land on exactly one side"
    );
    for sample in &snapshot {
        let in_train = train_requests.contains(sample.request_id.as_str());
        let in_holdout = holdout_requests.contains(sample.request_id.as_str());
        assert!(
            in_train ^ in_holdout,
            "request '{}' row '{}' is split across the partition",
            sample.request_id,
            sample.sample_id
        );
    }

    // Whole requests are the unit, so both sides are made of complete group
    // shapes and the independent request floor really bounds the holdout.
    assert_eq!(partition.holdout.len(), 2 * partition.holdout_requests());
    assert_eq!(
        partition.training.len(),
        snapshot.len() - partition.holdout.len()
    );
    assert!(partition.holdout_requests() > 1);
    assert!(partition.holdout_requests() >= config.min_holdout_requests);
    assert!(partition.holdout.len() >= config.min_holdout);
    assert!(partition.is_disjoint());
}

/// GATE 4 (the exact boundary case): the request straddling the row boundary
/// the old split would have drawn is entirely inside the holdout.
#[test]
fn a_request_is_never_cut_by_the_holdout_boundary() {
    let snapshot = multi_row_snapshot(21);
    let config = WarmupConfig::new(0.5, 1);
    let partition = partition_snapshot(&snapshot, &config).expect("the snapshot partitions");

    // The row boundary at 50% of 42 rows falls inside request 10, whose two rows
    // are indexes 20 and 21 of the ordered snapshot.
    let requests: Vec<&str> = {
        let mut ids: Vec<&str> = snapshot
            .iter()
            .map(|sample| sample.request_id.as_str())
            .collect();
        ids.dedup();
        ids
    };
    assert_eq!(requests.len(), 21);
    let boundary_request = requests[10];

    let holdout_ids: HashSet<&str> = partition
        .holdout
        .iter()
        .map(|sample| sample.sample_id.as_str())
        .collect();
    let boundary_rows: Vec<&OutcomeTrainingSample> = snapshot
        .iter()
        .filter(|sample| sample.request_id == boundary_request)
        .collect();
    assert_eq!(boundary_rows.len(), 2, "the fixture request has two rows");
    let in_holdout = boundary_rows
        .iter()
        .filter(|sample| holdout_ids.contains(sample.sample_id.as_str()))
        .count();
    assert!(
        in_holdout == 0 || in_holdout == boundary_rows.len(),
        "request '{boundary_request}' must be wholly on one side, found {in_holdout} of {} rows in the holdout",
        boundary_rows.len()
    );
    assert!(partition.is_disjoint());
}

/// GATE 4: the report counts the independent requests on both sides, and the
/// disjointness it declares is the group-level property, not a row-id claim.
#[test]
fn the_report_counts_whole_requests_on_both_sides() {
    let snapshot = multi_row_snapshot(21);
    let config = WarmupConfig::default();
    let outcome = run_warmup(&snapshot, &config).expect("the snapshot warms up");
    let report = outcome.report();

    assert!(report.holdout_disjoint_from_train);
    assert_eq!(report.train_requests + report.holdout_requests, 21);
    assert!(report.holdout_requests >= config.min_holdout_requests);
    assert!(report.holdout_samples >= config.min_holdout);
    assert_eq!(report.train_samples + report.holdout_samples, 42);
    assert_eq!(report.holdout_samples, 2 * report.holdout_requests);
    assert_eq!(report.train_samples, 2 * report.train_requests);
    assert!(outcome.commit().verify());
}

/// GATE 6: a holdout that clears the row floor with too few independent
/// requests is refused, because whole requests are the unit of evidence.
#[test]
fn a_holdout_short_of_independent_requests_is_refused() {
    let snapshot = multi_row_snapshot(4);
    assert_eq!(snapshot.len(), 8);
    let mut config = WarmupConfig::new(0.5, 1);
    config.min_holdout_requests = 5;

    // The row floor alone would have been satisfied, so the refusal is the
    // independent request floor doing the work.
    assert!(snapshot.len().saturating_sub(1) >= config.min_holdout);
    let error = error_of(run_warmup(&snapshot, &config));
    assert!(
        matches!(
            error,
            WarmupError::TooFewRequestsToHoldOut {
                requests: 4,
                min_requests: 5
            }
        ),
        "got {error:?}"
    );
}

/// GATE 4: a learnable dataset beats the accepted cold baseline, and the report
/// says so with the numbers behind it.
#[test]
fn a_warmup_beats_the_cold_baseline_on_a_learnable_dataset() {
    let outcome = run_warmup(&learnable_snapshot(40), &WarmupConfig::default())
        .expect("the snapshot warms up");
    let report = outcome.report();

    assert_eq!(report.baseline_description, BASELINE_DESCRIPTION);
    // The cold baseline answers every row with 0.5, so its log loss is the
    // entropy of the holdout's own success rate.
    let expected = ln2_of(report.holdout_positive_rate);
    assert!(
        (report.baseline.log_loss.expect("baseline log loss") - expected).abs() < 1e-9,
        "the cold baseline must score the holdout's own entropy"
    );
    assert!((report.baseline.brier_score.expect("baseline brier") - 0.25).abs() < 1e-9);

    assert!(report.log_loss_delta < 0.0, "log loss must improve");
    assert!(
        report.brier_delta <= 0.0,
        "the Brier score must not regress"
    );
    assert_eq!(report.verdict, WarmupVerdict::Better);
    assert!(report.is_improvement());
    assert_eq!(report.coverage.success, report.train_samples);
    assert!(
        report.coverage.latency_ms < report.train_samples,
        "only the successes carry a latency target"
    );
    assert_eq!(
        report.coverage.latency_ms, report.coverage.ttft_ms,
        "every successful streaming fixture carries both timing targets"
    );
    assert_eq!(
        report.coverage.cost, report.train_samples,
        "cost is a captured fact for non-success terminal results too"
    );
    assert_eq!(
        report.coverage.feedback, 0,
        "no fixture row carries feedback, so none may be counted"
    );
}

/// GATE 4: a deliberately bad model is reported as worse, and the report does
/// not dress it up.
#[test]
fn a_deliberately_bad_model_is_reported_as_worse() {
    let outcome = run_warmup(
        &inverted_across_split_snapshot(40, 30),
        &WarmupConfig::default(),
    )
    .expect("the snapshot warms up");
    let report = outcome.report();

    // The training partition is learnable and the holdout is the mirror image of
    // it, so the model is confidently wrong on every holdout row.
    assert_eq!(report.train_samples, 30);
    assert_eq!(report.holdout_samples, 10);
    assert!(
        !report.holdout_is_degenerate,
        "the holdout holds both classes"
    );
    assert!(
        report.log_loss_delta > 0.0,
        "the model must lose on log loss"
    );
    assert!(
        report.brier_delta > 0.0,
        "the model must lose on the Brier score"
    );
    assert_eq!(report.verdict, WarmupVerdict::Worse);
    assert!(!report.is_improvement());
    assert!(!report.verdict.is_improvement());
}

/// GATE 4: a warmup that cannot show improvement says so, and never presents
/// its commit as an improvement.
#[test]
fn a_warmup_that_cannot_improve_does_not_claim_that_it_did() {
    // Every training row failed and every holdout row succeeded: the model
    // learns the truth of the data it was given and is then confidently wrong.
    let outcome = run_warmup(
        &shifted_across_split_snapshot(40, 30),
        &WarmupConfig::default(),
    )
    .expect("the snapshot warms up");
    let report = outcome.report();

    assert_eq!(report.train_success, 0);
    assert_eq!(report.train_non_success, report.train_samples);
    assert_eq!(report.coverage.latency_ms, 0);
    assert_eq!(report.coverage.ttft_ms, 0);
    assert!(report.holdout_positive_rate > 0.9);
    assert!(report.log_loss_delta > 0.0);
    assert_eq!(report.verdict, WarmupVerdict::Worse);
    assert!(
        !report.is_improvement(),
        "an artifact that lost to the baseline must not claim an improvement"
    );
    // The artifact is still verified and still honest about what it is.
    assert!(outcome.commit().verify());
}

/// GATE 4 (a single-class holdout): a holdout holding one class only cannot
/// demonstrate discrimination, so it can never carry an improvement claim — not
/// even for a model that has beaten the constant baseline on the metrics.
#[test]
fn a_single_class_holdout_cannot_carry_an_improvement_claim() {
    let snapshot: Vec<OutcomeTrainingSample> = (0..40)
        .map(|index| {
            let quality = if index % 2 == 0 { 0.15 } else { 0.85 };
            sample(quality, Terminal::Failed, 1_700_000_000 + index as i64)
        })
        .collect();
    let outcome = run_warmup(&snapshot, &WarmupConfig::default()).expect("the snapshot warms up");
    let report = outcome.report();

    assert!(report.holdout_is_degenerate);
    assert_eq!(report.holdout_positive_rate, 0.0);
    // The metrics alone would have read as an improvement: the constant
    // baseline scores ln(2) on a one-class holdout, so any model that learned
    // the base rate beats it. That is the trap this rule closes.
    assert!(
        report.log_loss_delta < 0.0,
        "the raw metric must favour the model"
    );
    assert_eq!(
        report.verdict,
        WarmupVerdict::NotBetter,
        "a degenerate holdout must not produce an improvement claim"
    );
    assert!(!report.is_improvement());
    assert_eq!(
        WarmupVerdict::Better.downgraded_to_not_better(),
        WarmupVerdict::NotBetter
    );
    assert_eq!(
        WarmupVerdict::Worse.downgraded_to_not_better(),
        WarmupVerdict::Worse
    );
}

/// GATE 4 (the rule itself): the verdict is a pure function of the two deltas,
/// and it cannot report a win the numbers do not support.
#[test]
fn the_verdict_rule_is_two_sided_and_strict() {
    assert_eq!(
        WarmupVerdict::from_deltas(-0.1, -0.01),
        WarmupVerdict::Better
    );
    assert_eq!(WarmupVerdict::from_deltas(-0.1, 0.0), WarmupVerdict::Better);
    // Better on average, worse per row: not an improvement.
    assert_eq!(WarmupVerdict::from_deltas(-0.1, 0.01), WarmupVerdict::Worse);
    // Worse on average, better per row: also not an improvement.
    assert_eq!(WarmupVerdict::from_deltas(0.1, -0.01), WarmupVerdict::Worse);
    assert_eq!(
        WarmupVerdict::from_deltas(0.0, 0.0),
        WarmupVerdict::NotBetter
    );
    for verdict in [WarmupVerdict::Worse, WarmupVerdict::NotBetter] {
        assert!(!verdict.is_improvement());
    }
}

// ---------------------------------------------------------------------------
// Gate 5b + the typed contract's participation
// ---------------------------------------------------------------------------

/// GATE (b): the typed decision contract participates as a load boundary. The
/// warmed states load into it, pinned to the warmup commit, and score a typed
/// candidate finitely in all four dimensions.
#[test]
fn the_warmed_artifact_loads_into_the_typed_decision_contract() {
    let outcome = run_warmup(&learnable_snapshot(40), &WarmupConfig::default())
        .expect("the snapshot warms up");
    let report = outcome.report();

    let contract = outcome
        .decision_model()
        .expect("the warmed states must load into the decision contract");
    assert_eq!(contract.commit(), &outcome.commit_id());
    assert_eq!(contract.dimension(), FEATURE_DIMENSION);
    assert_eq!(contract.feature_schema(), FEATURE_SCHEMA_VERSION);
    assert_eq!(
        contract.sample_counts()[0] as usize,
        report.train_samples,
        "the success head saw every training row"
    );
    assert_eq!(
        contract.sample_counts()[1] as usize,
        report.coverage.latency_ms,
        "the latency head saw only the rows that carried a latency target"
    );

    let states = outcome.decision_model_states();
    let score = contract
        .try_score_candidate(&probe_candidate())
        .expect("the contract scores the warmed artifact");
    for dimension in DecisionDimension::ALL {
        let prediction = score.predictions.get(dimension);
        assert!(
            prediction.value.is_finite(),
            "{dimension:?} value is not finite"
        );
        assert!(
            prediction.confidence.is_finite(),
            "{dimension:?} confidence is not finite"
        );
    }
    // The states handed over are exactly the states the commit holds.
    assert_eq!(
        serde_json::to_string(&states.success).expect("state serializes"),
        serde_json::to_string(&outcome.commit().checkpoint.success).expect("state serializes")
    );
}

/// GATE (b): the attestation is real, not decorative. A state the commit does
/// not hold is refused by the contract, so a warmup that reported an artifact
/// the decision surface would reject could not have passed here.
#[test]
fn the_decision_contract_refuses_states_the_commit_does_not_hold() {
    let outcome = run_warmup(&learnable_snapshot(40), &WarmupConfig::default())
        .expect("the snapshot warms up");
    let mut states: DecisionModelStates = outcome.decision_model_states();
    states.success.parameters[3] = f64::from_bits(states.success.parameters[3].to_bits() ^ 1);

    assert!(DecisionModel::try_from_states(
        FEATURE_DIMENSION,
        FEATURE_SCHEMA_VERSION,
        outcome.commit_id(),
        &states,
    )
    .is_err());
}

// ---------------------------------------------------------------------------
// Gate 5 of the brief: label discipline through the projection
// ---------------------------------------------------------------------------

/// GATE 5 (labels): the raw canonical-to-legacy conversion copies the targets
/// verbatim. Whatever the canonical sample says about its own dimensions is
/// exactly what the conversion carries; warmup's training projection then
/// applies the per-head scope on top of it.
#[test]
fn the_raw_legacy_conversion_carries_the_targets_verbatim() {
    for terminal in [
        Terminal::Success,
        Terminal::Failed,
        Terminal::Cancelled,
        Terminal::Interrupted,
    ] {
        let canonical = sample(0.5, terminal, 1_700_000_000);
        let expected = serde_json::to_string(&canonical.targets).expect("targets serialize");
        let legacy = canonical.clone().into_legacy();
        assert_eq!(
            serde_json::to_string(&legacy.targets).expect("targets serialize"),
            expected,
            "{terminal:?}: the conversion must not alter a single target"
        );
        assert_eq!(legacy.sample_id, canonical.sample_id);
        assert_eq!(legacy.schema_version, canonical.schema_version);
        assert_eq!(legacy.features, canonical.features);
        assert_eq!(legacy.feedback.len(), canonical.feedback_signals().len());
    }
}

/// GATE 5 (labels): the scope each head consumes is explicit, and the whole
/// request heads never take a target from an attempt row.
#[test]
fn each_head_consumes_an_explicit_scope() {
    assert_eq!(
        head_scope(DecisionDimension::Latency),
        HeadScope::Request,
        "latency is the whole request's duration"
    );
    assert_eq!(head_scope(DecisionDimension::Ttft), HeadScope::Request);
    assert_eq!(head_scope(DecisionDimension::Cost), HeadScope::Request);
    assert_eq!(
        head_scope(DecisionDimension::Success),
        HeadScope::EveryRow,
        "attempt rows carry the candidate's own result and request rows the terminal one"
    );

    let canonical = mixed_scope_rows(1_700_000_000);
    assert_eq!(canonical.len(), 3);
    for row in &canonical {
        let projected = project_for_training(row);
        match row.scope {
            SampleScope::Request => {
                assert_eq!(projected.targets, row.targets, "a request row is copied");
                assert!(projected.targets.latency_ms.is_some());
                assert!(projected.targets.ttft_ms.is_some());
                assert!(projected.targets.cost.is_some());
            }
            SampleScope::Attempt { .. } => {
                assert!(
                    projected.targets.latency_ms.is_none(),
                    "an attempt row must not carry a whole-request latency target"
                );
                assert!(projected.targets.ttft_ms.is_none());
                assert!(projected.targets.cost.is_none());
            }
        }
    }
}

/// GATE 5 (labels): one request no longer supervises one head with two
/// durations. B's attempt row and B's request row share a feature vector, and
/// after projection they no longer disagree on latency, TTFT, cost, or success.
#[test]
fn the_two_rows_of_one_request_no_longer_disagree_on_a_head() {
    let canonical = mixed_scope_rows(1_700_000_000);
    let projected: Vec<DatasetTrainingSample> =
        canonical.iter().map(project_for_training).collect();

    // The fixture really does repeat B's feature vector across two scopes.
    let b_attempt = &projected[1];
    let b_request = &projected[2];
    assert_eq!(b_attempt.features, b_request.features);
    assert_ne!(b_attempt.targets.latency_ms, b_request.targets.latency_ms);

    // The whole-request total is the only latency label B's features carry.
    assert_eq!(b_request.targets.latency_ms, Some(1100.0));
    assert_eq!(b_request.targets.ttft_ms, Some(30.0));
    assert_eq!(b_request.targets.cost, Some(0.009));
    assert_eq!(b_attempt.targets.latency_ms, None);
    assert_eq!(b_attempt.targets.ttft_ms, None);
    assert_eq!(b_attempt.targets.cost, None);
    assert!(b_attempt.targets.success);
    assert!(b_request.targets.success);

    // A's failed attempt keeps its own failure evidence and no timing.
    assert!(!projected[0].targets.success);
    assert_eq!(projected[0].targets.latency_ms, None);

    // No two rows with an identical feature vector disagree on any head.
    for (index, left) in projected.iter().enumerate() {
        for right in projected.iter().skip(index + 1) {
            if left.features != right.features {
                continue;
            }
            assert_eq!(
                left.targets.success, right.targets.success,
                "identical features disagree on success"
            );
            assert!(
                !optional_targets_disagree(left.targets.latency_ms, right.targets.latency_ms),
                "identical features disagree on latency: {:?} vs {:?}",
                left.targets.latency_ms,
                right.targets.latency_ms
            );
            assert!(!optional_targets_disagree(
                left.targets.ttft_ms,
                right.targets.ttft_ms
            ));
            assert!(!optional_targets_disagree(
                left.targets.cost,
                right.targets.cost
            ));
        }
    }

    // The cost head keeps its supervision: request rows still carry the captured
    // total cost, which attempt rows never have.
    assert!(projected.iter().any(|row| row.targets.cost.is_some()));
}

/// GATE 5 (labels): a failed, cancelled, or interrupted request cannot become a
/// positive success label through the projection warmup uses, and the
/// per-dimension targets keep their `None` discipline.
#[test]
fn a_non_success_terminal_state_cannot_become_a_positive_label() {
    for terminal in [Terminal::Failed, Terminal::Cancelled, Terminal::Interrupted] {
        let canonical = sample(0.9, terminal, 1_700_000_000);
        assert_ne!(canonical.final_status, FinalStatus::Success);
        assert!(!canonical.success);
        assert!(!canonical.targets.success);
        assert!(canonical.identity.served.is_none(), "{terminal:?}: served");

        let legacy = canonical.clone().into_legacy();
        assert!(
            !legacy.targets.success,
            "{terminal:?}: a non-success request must not project a success label"
        );
        assert!(
            legacy.targets.latency_ms.is_none(),
            "{terminal:?}: a non-success request must keep its latency target absent"
        );
        assert!(
            legacy.targets.ttft_ms.is_none(),
            "{terminal:?}: a non-success request must keep its ttft target absent"
        );
        assert!(
            legacy.targets.failure_class.is_some(),
            "{terminal:?}: the failure class must survive the projection"
        );
        // Cost is deliberately not required to be absent: it is a captured
        // billing fact for a non-success terminal result too.
        assert_eq!(legacy.targets.cost, Some(0.009));

        // The models agree: the row trains "this candidate did not succeed".
        let mut ensemble = ModelEnsemble::new();
        ensemble.update_all(&legacy);
        assert_eq!(ensemble.success.sample_count(), 1);
        assert_eq!(
            ensemble.latency.sample_count(),
            0,
            "{terminal:?}: nothing observed a duration"
        );
    }
}

/// GATE 5 (labels): a dataset of nothing but non-success rows cannot produce a
/// success-positive training partition, however many of them there are.
#[test]
fn a_dataset_of_non_successes_trains_no_success_head() {
    let snapshot: Vec<OutcomeTrainingSample> =
        [Terminal::Failed, Terminal::Cancelled, Terminal::Interrupted]
            .into_iter()
            .flat_map(|terminal| {
                (0..15).map(move |index| {
                    sample(
                        if index % 2 == 0 { 0.15 } else { 0.85 },
                        terminal,
                        1_700_000_000 + index as i64,
                    )
                })
            })
            .collect();
    let outcome = run_warmup(&snapshot, &WarmupConfig::default()).expect("the snapshot warms up");

    assert_eq!(outcome.report().train_success, 0);
    assert_eq!(outcome.report().holdout_positive_rate, 0.0);
    assert_eq!(outcome.report().coverage.latency_ms, 0);
    assert_eq!(outcome.report().coverage.ttft_ms, 0);
    assert!(!outcome.report().is_improvement());
}

/// GATE 5 (labels): a hand-edited row cannot promote a cancelled request into a
/// positive label. Warmup re-validates the canonical sample, so the promotion is
/// refused rather than trained on.
#[test]
fn a_promoted_non_success_row_is_refused() {
    let mut promoted = sample(0.9, Terminal::Cancelled, 1_700_000_000);
    promoted.success = true;
    promoted.targets.success = true;

    let error = error_of(run_warmup(&[promoted], &WarmupConfig::new(0.5, 1)));
    assert!(
        matches!(error, WarmupError::InvalidSample { .. }),
        "a promoted row must be refused, got {error:?}"
    );

    // And the reason names the disagreement rather than being generic.
    let snapshot = learnable_snapshot(40);
    let mut tampered = snapshot;
    tampered[0].success = true;
    tampered[0].targets.success = true;
    let error = error_of(run_warmup(&tampered, &WarmupConfig::default()));
    assert!(error.to_string().contains("warmup sample 0 is invalid"));
}

/// GATE 5 (feedback): a row with no feedback contributes no rating, and warmup
/// never supplies one.
#[test]
fn warmup_never_invents_a_rating() {
    let snapshot = learnable_snapshot(40);
    assert!(snapshot.iter().all(|sample| sample.feedback.is_none()));
    let outcome = run_warmup(&snapshot, &WarmupConfig::default()).expect("the snapshot warms up");
    assert_eq!(outcome.report().coverage.feedback, 0);
    assert!(outcome
        .holdout()
        .samples()
        .iter()
        .all(|sample| sample.feedback.is_empty()));
    assert!(outcome
        .ensemble()
        .success
        .predict(&fixture_features(0.85))
        .value
        .is_finite());
}

// ---------------------------------------------------------------------------
// Gate 6: fail-closed refusals
// ---------------------------------------------------------------------------

/// GATE 6: an empty dataset is a typed refusal, not an empty model.
#[test]
fn an_empty_dataset_is_refused() {
    assert!(matches!(
        error_of(run_warmup(&[], &WarmupConfig::default())),
        WarmupError::EmptyDataset
    ));
}

/// GATE 6: a dataset too small to hold out is a typed refusal, and the run
/// returns nothing at all rather than training on the whole snapshot.
#[test]
fn a_dataset_too_small_to_hold_out_is_refused() {
    let snapshot = learnable_snapshot(4);
    let error = error_of(run_warmup(&snapshot, &WarmupConfig::default()));
    assert!(
        matches!(
            error,
            WarmupError::DatasetTooSmallToHoldOut {
                samples: 4,
                min_holdout: 8
            }
        ),
        "got {error:?}"
    );
    // One row short of the floor is still too small.
    let snapshot = learnable_snapshot(8);
    assert!(matches!(
        error_of(run_warmup(&snapshot, &WarmupConfig::default())),
        WarmupError::DatasetTooSmallToHoldOut { .. }
    ));
}

/// GATE 6: a schema-version mismatch is a typed refusal naming the component.
#[test]
fn a_schema_version_mismatch_is_refused() {
    let mut snapshot = learnable_snapshot(40);
    snapshot[7].schema_version = FEATURE_SCHEMA_VERSION + 1;
    let error = error_of(run_warmup(&snapshot, &WarmupConfig::default()));
    assert!(
        matches!(
            error,
            WarmupError::SchemaMismatch {
                index: 7,
                component: "sample",
                ..
            }
        ),
        "got {error:?}"
    );
    assert!(error.to_string().contains("sample schema version"));

    let mut snapshot = learnable_snapshot(40);
    snapshot[3].features.schema_version = FEATURE_SCHEMA_VERSION + 9;
    let error = error_of(run_warmup(&snapshot, &WarmupConfig::default()));
    assert!(
        matches!(
            error,
            WarmupError::SchemaMismatch {
                index: 3,
                component: "feature",
                ..
            }
        ),
        "got {error:?}"
    );
}

/// GATE 6: a non-finite feature value is a typed refusal naming its position.
#[test]
fn a_non_finite_feature_is_refused() {
    for (position, value) in [
        (0usize, f32::NAN),
        (11, f32::INFINITY),
        (31, f32::NEG_INFINITY),
    ] {
        let mut snapshot = learnable_snapshot(40);
        snapshot[5].features.values[position] = value;
        let error = error_of(run_warmup(&snapshot, &WarmupConfig::default()));
        assert!(
            matches!(
                error,
                WarmupError::NonFiniteFeature {
                    index: 5,
                    position: found,
                    ..
                } if found == position
            ),
            "position {position}: got {error:?}"
        );
        assert!(error.to_string().contains("non-finite feature"));
    }
}

/// GATE 6: a feature vector of the wrong width. The width is a compile-time
/// constant, so the type refuses to hold one — and the schema marker, which is
/// the one thing that can disagree with the trained dimension, is refused with a
/// reason.
#[test]
fn a_wrong_width_feature_vector_is_refused() {
    // The width is not expressible: a vector of any other length fails to
    // deserialize into the accepted feature type, so it can never reach warmup.
    let short = r#"{"values":[0.1,0.2,0.3],"schema_version":1}"#;
    assert!(
        serde_json::from_str::<RoutingFeatures>(short).is_err(),
        "a vector of the wrong width must not deserialize at all"
    );
    let long_values: Vec<String> = (0..FEATURE_DIMENSION + 1)
        .map(|_| "0.5".to_string())
        .collect();
    let long = format!(
        r#"{{"values":[{}],"schema_version":1}}"#,
        long_values.join(",")
    );
    assert!(serde_json::from_str::<RoutingFeatures>(&long).is_err());

    // The dimension marker that can disagree is refused by name.
    let mut snapshot = learnable_snapshot(40);
    snapshot[2].features.schema_version = FEATURE_SCHEMA_VERSION + 1;
    let error = error_of(run_warmup(&snapshot, &WarmupConfig::default()));
    assert!(
        matches!(
            error,
            WarmupError::SchemaMismatch {
                index: 2,
                component: "feature",
                ..
            }
        ),
        "got {error:?}"
    );
    // And the width guard itself is present and satisfied by the accepted type.
    let mut snapshot = learnable_snapshot(40);
    snapshot[0].features.values[0] = 0.0;
    assert_eq!(snapshot[0].features.values.len(), FEATURE_DIMENSION);
}

/// GATE 6: a non-success row carrying timing is refused, because a regression
/// head trained on it would learn a duration for a request that never
/// completed.
#[test]
fn a_non_success_row_carrying_timing_is_refused() {
    let mut snapshot = learnable_snapshot(40);
    // Even rows are failures, so their timing targets must stay absent.
    assert!(!snapshot[0].targets.success);
    snapshot[0].targets.latency_ms = Some(123.0);
    let error = error_of(run_warmup(&snapshot, &WarmupConfig::default()));
    assert!(
        matches!(
            error,
            WarmupError::NonSuccessTiming {
                index: 0,
                dimension: "latency_ms",
                value
            } if value == 123.0
        ),
        "got {error:?}"
    );

    let mut snapshot = learnable_snapshot(40);
    assert!(!snapshot[2].targets.success);
    snapshot[2].targets.ttft_ms = Some(45.0);
    let error = error_of(run_warmup(&snapshot, &WarmupConfig::default()));
    assert!(
        matches!(
            error,
            WarmupError::NonSuccessTiming {
                index: 2,
                dimension: "ttft_ms",
                ..
            }
        ),
        "got {error:?}"
    );

    // A success row carrying timing is ordinary, not a contradiction.
    let mut snapshot = learnable_snapshot(40);
    assert!(snapshot[1].targets.success);
    snapshot[1].targets.ttft_ms = Some(45.0);
    assert!(run_warmup(&snapshot, &WarmupConfig::default()).is_ok());
}

/// GATE 6: a repeated sample id is refused, because two rows sharing an id could
/// put one sample in both partitions.
#[test]
fn a_repeated_sample_id_is_refused() {
    let mut snapshot = learnable_snapshot(40);
    snapshot[9].sample_id = snapshot[4].sample_id.clone();
    let error = error_of(run_warmup(&snapshot, &WarmupConfig::default()));
    assert!(
        matches!(
            error,
            WarmupError::DuplicateSampleId {
                first: 4,
                second: 9,
                ..
            }
        ),
        "got {error:?}"
    );
}

/// GATE 6: a configuration that cannot support a holdout is refused before the
/// snapshot is even looked at.
#[test]
fn an_unusable_configuration_is_refused() {
    for ratio in [0.0, 1.0, -0.5, f64::NAN, f64::INFINITY] {
        let error = error_of(run_warmup(
            &learnable_snapshot(40),
            &WarmupConfig::new(ratio, 8),
        ));
        assert!(
            matches!(error, WarmupError::InvalidHoldoutRatio { .. }),
            "ratio {ratio}: got {error:?}"
        );
    }
    let error = error_of(run_warmup(
        &learnable_snapshot(40),
        &WarmupConfig::new(0.25, 0),
    ));
    assert!(matches!(error, WarmupError::EmptyHoldoutFloor));

    let mut empty_request_floor = WarmupConfig::new(0.25, 8);
    empty_request_floor.min_holdout_requests = 0;
    assert!(matches!(
        error_of(run_warmup(&learnable_snapshot(40), &empty_request_floor)),
        WarmupError::EmptyHoldoutRequestFloor
    ));

    // A bad configuration outranks an empty snapshot, so a caller fixing one
    // defect at a time is never told about the other.
    assert!(matches!(
        error_of(run_warmup(&[], &WarmupConfig::new(2.0, 8))),
        WarmupError::InvalidHoldoutRatio { .. }
    ));
}

/// GATE 6 (closure): a run either returns a verified commit or a typed reason.
/// There is no third outcome, and no malformed input reaches a panic.
#[test]
fn every_refusal_is_typed_and_no_malformed_input_panics() {
    let mut malformed: Vec<Vec<OutcomeTrainingSample>> = vec![
        Vec::new(),
        learnable_snapshot(1),
        learnable_snapshot(4),
        learnable_snapshot(40),
        inverted_across_split_snapshot(40, 30),
        shifted_across_split_snapshot(40, 30),
    ];
    // A schema mismatch, a non-finite feature, a forged timing target, a
    // repeated id, and a promoted non-success row.
    let mut a = learnable_snapshot(40);
    a[0].schema_version = 99;
    malformed.push(a);
    let mut b = learnable_snapshot(40);
    b[1].features.values[0] = f32::NAN;
    malformed.push(b);
    let mut c = learnable_snapshot(40);
    c[2].targets.latency_ms = Some(1.0);
    malformed.push(c);
    let mut d = learnable_snapshot(40);
    d[3].sample_id = d[0].sample_id.clone();
    malformed.push(d);
    let mut e = learnable_snapshot(40);
    e[4].success = true;
    e[4].targets.success = true;
    malformed.push(e);

    for snapshot in malformed {
        match run_warmup(&snapshot, &WarmupConfig::default()) {
            Ok(outcome) => {
                assert!(outcome.commit().verify());
                assert!(outcome.report().train_samples > 0);
            }
            Err(error) => {
                assert!(!error.to_string().is_empty(), "a refusal needs a reason");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers used by the purity tests
// ---------------------------------------------------------------------------

fn ln2_of(rate: f64) -> f64 {
    let p = rate.clamp(f64::MIN_POSITIVE, 1.0 - f64::EPSILON);
    -(p * p.ln() + (1.0 - p) * (1.0 - p).ln())
}

fn probe_candidate() -> zroutery_core::ml::DecisionCandidate {
    zroutery_core::ml::DecisionCandidate::new(
        zroutery_core::outcome::CandidateIdentity::new(MODEL, PROVIDER),
        zroutery_core::ml::CandidateEligibility::Eligible,
        fixture_features(0.85),
    )
}

fn probe_input() -> zroutery_core::ml::ShadowInput {
    zroutery_core::ml::ShadowInput {
        decision_id: "dec-probe".to_string(),
        policy_id: "policy-probe".to_string(),
        client_id: None,
        policy_revision: Default::default(),
        task: Default::default(),
        production_selected: MODEL.to_string(),
        feature_schema: FEATURE_SCHEMA_VERSION,
        candidates: vec![zroutery_core::ml::ShadowCandidateInput {
            candidate_id: MODEL.to_string(),
            provider_id: PROVIDER.to_string(),
            tier: None,
            eligible: true,
            features: fixture_features(0.85),
            rejection_reason: None,
        }],
        session_mode: Default::default(),
        session_switch_count: 0,
        is_fallback: false,
    }
}
