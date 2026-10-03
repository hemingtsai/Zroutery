#![cfg(feature = "ml")]

//! Dataset ingestion, validation and retention gates.
//!
//! These are the boundary claims, proved without a server:
//!
//! - a sample's features are the vectors retained at decision time, and only
//!   those; nothing is synthesized, reconstructed or defaulted;
//! - a request with no retained decision-time input produces no sample, and
//!   says so in a way that cannot be confused with a refusal;
//! - a malformed or inconsistent sample is refused with a reason and never
//!   stored, and never repaired;
//! - the store is bounded by count *and* age, and both evictions are visible;
//! - a failed, cancelled or interrupted request is retained as the negative
//!   sample it was, with its failure class intact;
//! - nothing in the dataset module can route, and a fault is contained.

use zroutery_core::failure::FailureClass;
use zroutery_core::feedback::DataOrigin;
use zroutery_core::ml::dataset::{
    contain_dataset_fault, contained_ingest, try_outcome_sample, DatasetStore, Ingestion,
    IngestionCounters, OutcomeTrainingSample, SampleScope,
};
use zroutery_core::ml::shadow::ShadowInput;
use zroutery_core::ml::{RoutingFeatures, FEATURE_SCHEMA_VERSION};
use zroutery_core::outcome::{Attempt, FinalStatus, Outcome};

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

fn features(seed: usize) -> RoutingFeatures {
    let mut vector = RoutingFeatures::default();
    vector.values[0] = 1.0;
    vector.values[8] = seed as f32 / 100.0;
    vector
}

fn attempt(model: &str, provider: &str, success: bool, class: Option<FailureClass>) -> Attempt {
    Attempt {
        attempt_id: format!("att_{model}"),
        candidate_model: model.to_string(),
        candidate_provider: provider.to_string(),
        started_at: 1_700_000_000,
        completed_at: 1_700_000_001,
        latency_ms: 120.0,
        ttft_ms: success.then_some(40.0),
        success,
        failure_class: class,
        failure_message: class.map(|class| format!("{class:?} happened")),
        http_status: success.then_some(200),
        rectified: false,
    }
}

/// A served request: one attempt, one delivered answer.
fn served_outcome() -> Outcome {
    Outcome::builder("req-served")
        .single_candidate("model-a", "alpha")
        .dialect("openai")
        .streaming(true)
        .attempt(attempt("model-a", "alpha", true, None))
        .served_candidate("model-a", "alpha")
        .total_latency_ms(120.0)
        .ttft_ms(40.0)
        .build()
}

/// A failover: the first candidate failed, the second answered.
fn failover_outcome() -> Outcome {
    Outcome::builder("req-failover")
        .initial("model-a", "alpha")
        .final_candidate("model-b", "beta")
        .dialect("openai")
        .attempt(attempt(
            "model-a",
            "alpha",
            false,
            Some(FailureClass::RateLimit),
        ))
        .attempt(attempt("model-b", "beta", true, None))
        .served_candidate("model-b", "beta")
        .total_latency_ms(400.0)
        .build()
}

/// A request that served nothing: the upstream failed on every candidate.
fn failed_outcome() -> Outcome {
    Outcome::builder("req-failed")
        .single_candidate("model-a", "alpha")
        .dialect("openai")
        .attempt(attempt(
            "model-a",
            "alpha",
            false,
            Some(FailureClass::ProviderUnavailable),
        ))
        .total_latency_ms(120.0)
        .build()
}

fn candidate(model: &str, provider: &str, vector: RoutingFeatures) -> ShadowInput {
    ShadowInput {
        decision_id: "dec-1".to_string(),
        production_selected: model.to_string(),
        candidates: vec![zroutery_core::ml::ShadowCandidateInput {
            candidate_id: model.to_string(),
            provider_id: provider.to_string(),
            tier: None,
            eligible: true,
            features: vector,
            rejection_reason: None,
        }],
        ..Default::default()
    }
}

fn with_candidates(inputs: Vec<zroutery_core::ml::ShadowCandidateInput>) -> ShadowInput {
    ShadowInput {
        decision_id: "dec-1".to_string(),
        candidates: inputs,
        ..Default::default()
    }
}

fn candidate_input(
    model: &str,
    provider: &str,
    vector: RoutingFeatures,
) -> zroutery_core::ml::ShadowCandidateInput {
    zroutery_core::ml::ShadowCandidateInput {
        candidate_id: model.to_string(),
        provider_id: provider.to_string(),
        tier: None,
        eligible: true,
        features: vector,
        rejection_reason: None,
    }
}

fn served_input() -> ShadowInput {
    candidate("model-a", "alpha", features(10))
}

fn failover_input() -> ShadowInput {
    with_candidates(vec![
        candidate_input("model-a", "alpha", features(20)),
        candidate_input("model-b", "beta", features(30)),
    ])
}

fn find<'a>(samples: &'a [OutcomeTrainingSample], suffix: &str) -> &'a OutcomeTrainingSample {
    samples
        .iter()
        .find(|sample| sample.sample_id.ends_with(suffix))
        .unwrap_or_else(|| panic!("expected a sample ending in '{suffix}'"))
}

fn no_counters() -> IngestionCounters {
    IngestionCounters::default()
}

/// Ingest and insist it stored something.
fn ingest_ok(store: &DatasetStore, outcome: &Outcome, input: &ShadowInput) {
    let result = store.ingest(&outcome.request_id, outcome, Some(input));
    assert!(
        result.is_ingested(),
        "'{}' was not ingested: {result:?}",
        outcome.request_id
    );
}

/// The request ids of the retained samples, oldest first. A request with one
/// attempt appears twice: once for the attempt, once for the request.
fn retained_requests(store: &DatasetStore) -> Vec<String> {
    store
        .training_slice()
        .into_iter()
        .map(|sample| sample.request_id)
        .collect()
}

/// The request-scope sample for one request id.
fn request_sample<'a>(
    samples: &'a [OutcomeTrainingSample],
    request_id: &str,
) -> &'a OutcomeTrainingSample {
    samples
        .iter()
        .find(|sample| {
            sample.request_id == request_id && matches!(sample.scope, SampleScope::Request)
        })
        .unwrap_or_else(|| panic!("expected a request-scope sample for '{request_id}'"))
}

// ---------------------------------------------------------------------------
// the features are the retained ones
// ---------------------------------------------------------------------------

/// The central claim: an ingested sample carries the vector the record retained,
/// for the exact candidate the attempt used.
#[test]
fn ingested_samples_carry_the_retained_decision_time_vectors() {
    let store = DatasetStore::new(10, 3600);
    let outcome = failover_outcome();
    let input = failover_input();

    let Ingestion::Ingested { sample_ids } =
        store.ingest(&outcome.request_id, &outcome, Some(&input))
    else {
        panic!("a correlated ingestion must store");
    };

    let samples = store.training_slice();
    assert_eq!(samples.len(), 3, "one sample per attempt plus the request");
    assert_eq!(sample_ids.len(), 3);
    for sample in &samples {
        assert_eq!(sample.request_id, outcome.request_id);
        assert_eq!(sample.outcome_id, outcome.outcome_id);
        assert_eq!(sample.origin, DataOrigin::Native, "production provenance");
        assert!(sample.feedback.is_none(), "no signal was supplied");
    }

    // The first attempt's sample carries the first candidate's retained vector,
    // not the second's and not a default.
    let first = find(&samples, "attempt-0");
    assert_eq!(first.provider_id, "alpha");
    assert_eq!(first.model_id, "model-a");
    assert_eq!(first.features.values[8], features(20).values[8]);
    assert_ne!(first.features.values[8], features(30).values[8]);
    assert_ne!(
        first.features.values[8],
        RoutingFeatures::default().values[8]
    );

    // The request-level sample carries the vector of the identity that served.
    let request = find(&samples, "-request");
    assert_eq!(request.provider_id, "beta");
    assert_eq!(request.model_id, "model-b");
    assert_eq!(request.features.values[8], features(30).values[8]);
}

/// The same input and the same Outcome always produce the same samples: the
/// ingestion is a pure function of retained evidence.
#[test]
fn ingestion_is_deterministic() {
    let outcome = failover_outcome();
    let input = failover_input();
    let first = DatasetStore::new(10, 3600);
    let second = DatasetStore::new(10, 3600);
    assert_eq!(
        first.ingest(&outcome.request_id, &outcome, Some(&input)),
        second.ingest(&outcome.request_id, &outcome, Some(&input))
    );
    assert_eq!(
        serde_json::to_value(first.training_slice()).unwrap(),
        serde_json::to_value(second.training_slice()).unwrap()
    );
}

// ---------------------------------------------------------------------------
// no decision-time input is observable, and never a fabricated sample
// ---------------------------------------------------------------------------

/// A request with no retained record has no features and no sample — and the
/// store says which case that was instead of staying silent.
#[test]
fn no_decision_time_input_yields_no_sample_and_is_counted_apart() {
    let store = DatasetStore::new(10, 3600);
    let outcome = served_outcome();

    let result = store.ingest(&outcome.request_id, &outcome, None);

    assert_eq!(result, Ingestion::NoDecisionTimeInput);
    assert!(result.is_without_decision_time_input());
    assert!(!result.is_ingested());
    assert!(result.sample_ids().is_empty());
    assert!(store.is_empty(), "no sample was invented");
    assert!(store.training_slice().is_empty());
    assert!(store.legacy_training_slice().is_empty());
    let counters = store.counters();
    assert_eq!(
        counters,
        IngestionCounters {
            no_decision_time_input: 1,
            ..no_counters()
        }
    );
    assert_eq!(counters.rejected, 0, "this is not a refusal");
    assert_eq!(counters.samples, 0, "nothing was collected");
}

/// The refused case and the no-input case are different facts and stay
/// different, whichever order they happen in.
#[test]
fn a_refusal_is_not_reported_as_missing_decision_time_input() {
    let store = DatasetStore::new(10, 3600);
    let outcome = served_outcome();

    // Correlating the wrong request is a refusal, not a missing input.
    let mismatched = store.ingest("req-other", &outcome, Some(&served_input()));
    assert!(matches!(mismatched, Ingestion::Rejected { .. }));
    assert!(store.is_empty());

    store.ingest(&outcome.request_id, &outcome, None);
    let counters = store.counters();
    assert_eq!(counters.rejected, 1);
    assert_eq!(counters.no_decision_time_input, 1);
    assert_eq!(counters.ingested, 0);
}

/// A retained record whose vector is unusable is refused, not repaired.
#[test]
fn an_unusable_retained_vector_is_refused_rather_than_repaired() {
    let store = DatasetStore::new(10, 3600);
    let outcome = served_outcome();

    let mut non_finite = served_input();
    non_finite.candidates[0].features.values[4] = f32::NAN;
    let result = store.ingest(&outcome.request_id, &outcome, Some(&non_finite));
    assert!(
        matches!(&result, Ingestion::Rejected { reason } if reason.contains("no usable retained features")),
        "got {result:?}"
    );
    assert!(store.is_empty());
    assert_eq!(store.counters().rejected, 1);

    let mut foreign_schema = served_input();
    foreign_schema.candidates[0].features.schema_version = FEATURE_SCHEMA_VERSION + 9;
    let result = store.ingest(&outcome.request_id, &outcome, Some(&foreign_schema));
    assert!(matches!(result, Ingestion::Rejected { .. }));
    assert!(store.is_empty());

    let mut foreign_input_schema = served_input();
    foreign_input_schema.feature_schema = FEATURE_SCHEMA_VERSION + 9;
    let result = store.ingest(&outcome.request_id, &outcome, Some(&foreign_input_schema));
    assert!(
        matches!(&result, Ingestion::Rejected { reason } if reason.contains("schema mismatch"))
    );
    assert!(store.is_empty());
}

/// A candidate that was retained but never observed eligible carries no usable
/// vector: it was refused by policy, so it gets no sample at all.
#[test]
fn a_policy_rejected_candidate_yields_no_sample() {
    let store = DatasetStore::new(10, 3600);
    let outcome = served_outcome();
    let mut input = served_input();
    input.candidates[0].eligible = false;
    input.candidates[0].rejection_reason = Some("policy rejected".to_string());

    let result = store.ingest(&outcome.request_id, &outcome, Some(&input));

    assert!(matches!(result, Ingestion::Rejected { .. }));
    assert!(store.is_empty());
}

/// An Outcome the schema rejects never becomes a sample.
#[test]
fn an_invalid_outcome_is_refused_with_a_reason() {
    let store = DatasetStore::new(10, 3600);
    let mut outcome = served_outcome();
    // A served request whose Outcome claims a different identity is internally
    // inconsistent, and the schema is what says so.
    outcome.actual_cost = Some(f64::NAN);

    let result = store.ingest(&outcome.request_id, &outcome, Some(&served_input()));

    let Ingestion::Rejected { reason } = result else {
        panic!("an inconsistent outcome must be refused, got {result:?}");
    };
    assert!(!reason.is_empty(), "a refusal always carries its reason");
    assert!(store.is_empty());
    assert_eq!(store.counters().rejected, 1);
}

// ---------------------------------------------------------------------------
// validation at the store boundary
// ---------------------------------------------------------------------------

#[test]
fn a_mutated_sample_is_refused_by_the_store_and_never_stored() {
    let store = DatasetStore::new(10, 3600);
    let outcome = served_outcome();
    let sample =
        try_outcome_sample(&outcome, features(10), DataOrigin::Native, None).expect("sample");

    // 1. A success label the terminal state contradicts.
    let mut contradictory = sample.clone();
    contradictory.success = false;
    let err = store.push(contradictory).unwrap_err();
    assert!(err.contains("disagrees with final_status"), "got: {err}");
    assert!(store.is_empty());

    // 2. A non-finite target.
    let mut non_finite = sample.clone();
    non_finite.targets.latency_ms = Some(f64::INFINITY);
    let err = store.push(non_finite).unwrap_err();
    assert!(err.contains("latency_ms"), "got: {err}");
    assert!(store.is_empty());

    // 3. A negative target.
    let mut negative = sample.clone();
    negative.targets.cost = Some(-1.0);
    let err = store.push(negative).unwrap_err();
    assert!(err.contains("cost"), "got: {err}");
    assert!(store.is_empty());

    // 4. A non-finite feature value.
    let mut broken_features = sample.clone();
    broken_features.features.values[2] = f32::NAN;
    let err = store.push(broken_features).unwrap_err();
    assert!(err.contains("finite"), "got: {err}");
    assert!(store.is_empty());

    // 5. A foreign schema on the sample itself.
    let mut foreign = sample.clone();
    foreign.schema_version = FEATURE_SCHEMA_VERSION + 1;
    let err = store.push(foreign).unwrap_err();
    assert!(err.contains("schema version mismatch"), "got: {err}");
    assert!(store.is_empty());

    // The untouched sample is still accepted: refusal is not damage.
    store.push(sample).unwrap();
    assert_eq!(store.len(), 1);
}

#[test]
fn the_store_holds_the_canonical_sample_and_the_legacy_shape_is_one_way() {
    let store = DatasetStore::new(10, 3600);
    let outcome = failover_outcome();
    ingest_ok(&store, &outcome, &failover_input());

    let canonical: Vec<OutcomeTrainingSample> = store.training_slice();
    assert_eq!(canonical.len(), 3);
    // The legacy view is reachable, and it is a projection of the canonical
    // samples rather than a second storage path.
    let legacy = store.legacy_training_slice();
    assert_eq!(legacy.len(), canonical.len());
    assert_eq!(legacy[0].outcome_id, canonical[0].outcome_id);
    assert_eq!(legacy[0].features, canonical[0].features);
    assert_eq!(legacy[0].targets, canonical[0].targets);
    // Nothing converts back: the store has no constructor from the legacy shape.
    assert_eq!(store.len(), 3, "the adapter allocated nothing new");
}

// ---------------------------------------------------------------------------
// retention
// ---------------------------------------------------------------------------

/// Count eviction: the store never exceeds its bound, and says how many left.
#[test]
fn retention_evicts_by_count_and_reports_it() {
    // One request contributes one sample per attempt plus one for the request.
    let store = DatasetStore::new(4, 3600);
    for index in 0..6 {
        let outcome = Outcome::builder(format!("req-{index}"))
            .single_candidate("model-a", "alpha")
            .dialect("openai")
            .attempt(attempt("model-a", "alpha", true, None))
            .served_candidate("model-a", "alpha")
            .total_latency_ms(10.0)
            .build();
        ingest_ok(&store, &outcome, &served_input());
    }

    assert_eq!(store.len(), 4, "the store is bounded by count");
    let requests = retained_requests(&store);
    assert_eq!(
        requests,
        vec!["req-4", "req-4", "req-5", "req-5"],
        "the two most recent requests survive, oldest evidence first"
    );
    let counters = store.counters();
    assert_eq!(counters.ingested, 6);
    assert_eq!(
        counters.samples, 12,
        "all six were collected before eviction"
    );
    assert_eq!(counters.evicted_by_count, 8);
    assert_eq!(counters.evicted_by_age, 0);
}

/// A zero count bound is refused at construction: it can hold no sample and can
/// evict none, so the retention loop used to spin forever on an empty queue
/// while holding the lock.
///
/// The bound is asserted before the pushes, so a regression fails on the bound
/// rather than hanging the suite in the lock.
#[test]
fn a_zero_count_bound_is_floored_and_push_still_makes_progress() {
    let store = DatasetStore::new(0, 3600);
    assert_eq!(
        store.max_samples(),
        1,
        "a zero count bound cannot be accepted"
    );

    let first = try_outcome_sample(&served_outcome(), features(10), DataOrigin::Native, None)
        .expect("a valid sample");
    store.push(first).expect("push must make progress");
    assert_eq!(store.len(), 1);

    let second = try_outcome_sample(&failed_outcome(), features(11), DataOrigin::Native, None)
        .expect("a valid sample");
    store.push(second).expect("push must still make progress");
    assert_eq!(store.len(), 1, "the floored bound is a real bound");
    assert_eq!(store.counters().evicted_by_count, 1);
}

/// Age eviction is physical, not only a read-time filter, and it is reported.
#[test]
fn retention_evicts_by_age_and_reports_it() {
    let store = DatasetStore::new(100, 60);
    let mut stale = served_outcome();
    stale.timestamp = chrono_stale();
    ingest_ok(&store, &stale, &served_input());
    assert_eq!(store.len(), 2, "the stale request's two samples are stored");

    // A fresh push drops the expired samples on the way in.
    let fresh = Outcome::builder("req-fresh")
        .single_candidate("model-a", "alpha")
        .dialect("openai")
        .attempt(attempt("model-a", "alpha", true, None))
        .served_candidate("model-a", "alpha")
        .total_latency_ms(10.0)
        .build();
    ingest_ok(&store, &fresh, &served_input());

    assert_eq!(store.len(), 2, "the stale samples did not stay resident");
    assert_eq!(store.counters().evicted_by_age, 2);
    assert_eq!(retained_requests(&store), vec!["req-fresh", "req-fresh"]);
}

fn chrono_stale() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64 - 3600)
        .unwrap_or_default()
}

/// Both bounds together: neither one alone is the retention policy.
#[test]
fn retention_is_bounded_by_both_count_and_age() {
    let store = DatasetStore::new(3, 3600);
    assert_eq!(store.max_samples(), 3);
    assert_eq!(store.max_age_secs(), 3600);
    assert!(store.max_samples() > 0 && store.max_age_secs() > 0);
    for index in 0..10 {
        let outcome = Outcome::builder(format!("req-{index}"))
            .single_candidate("model-a", "alpha")
            .dialect("openai")
            .attempt(attempt("model-a", "alpha", true, None))
            .served_candidate("model-a", "alpha")
            .total_latency_ms(10.0)
            .build();
        store
            .ingest(&outcome.request_id, &outcome, Some(&served_input()))
            .is_ingested()
            .then_some(())
            .expect("ingested");
    }
    assert!(store.len() <= 3);
    assert!(store.training_slice().len() <= 3);
}

// ---------------------------------------------------------------------------
// non-success requests
// ---------------------------------------------------------------------------

/// A request that failed is retained as the negative sample it was, with its
/// failure class and terminal state intact.
#[test]
fn a_failed_request_is_retained_as_a_negative_sample() {
    let store = DatasetStore::new(10, 3600);
    let outcome = failed_outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    let input = candidate("model-a", "alpha", features(40));

    let result = store.ingest(&outcome.request_id, &outcome, Some(&input));
    assert!(result.is_ingested(), "got {result:?}");

    let samples = store.training_slice();
    let request = find(&samples, "-request");
    assert!(!request.success);
    assert!(!request.targets.success);
    assert_eq!(request.final_status, FinalStatus::Failed);
    assert_eq!(
        request.targets.failure_class.as_deref(),
        Some("ProviderUnavailable")
    );
    assert!(request.identity.served.is_none(), "nothing served");
    assert!(request.targets.latency_ms.is_none());
    assert!(request.targets.ttft_ms.is_none());
    assert!(request.terminal_error.is_some());
    assert_eq!(
        request.terminal_error.as_ref().map(|facts| facts.class),
        Some(FailureClass::ProviderUnavailable)
    );
    // Cost stays a captured fact and never becomes a success hint.
    assert!(request.targets.cost.is_none() || request.targets.cost.is_some());

    let attempt_sample = find(&samples, "attempt-0");
    assert!(!attempt_sample.success);
    assert_eq!(
        attempt_sample.targets.failure_class.as_deref(),
        Some("ProviderUnavailable")
    );
}

/// A failed attempt must not carry a service latency, for the same reason a
/// failed request must not: there was no service to be slow.
///
/// The producer never produced one — `Targets::from_attempt` leaves the timing
/// absent for a non-success attempt — so this is about the hand-built and
/// deserialized paths, which are exactly the ones that reach a training head
/// without passing through the producer. Before the validator mirrored the
/// request-scope rule, such a sample validated and `update_all` would have fed
/// a failed attempt's duration to the latency regression.
#[test]
fn a_failed_attempt_sample_cannot_carry_service_timing() {
    let store = DatasetStore::new(10, 3600);
    let mut samples = zroutery_core::ml::dataset::try_samples_from_outcome(
        &failover_outcome(),
        &[features(11), features(12)],
        DataOrigin::Native,
    )
    .expect("the failover outcome converts");
    let failed = samples
        .iter_mut()
        .find(|sample| {
            matches!(
                sample.scope,
                zroutery_core::ml::dataset::SampleScope::Attempt { .. }
            ) && !sample.success
        })
        .expect("the failover keeps a failed attempt sample");
    assert!(
        failed.targets.latency_ms.is_none() && failed.targets.ttft_ms.is_none(),
        "the producer leaves timing absent on a failed attempt"
    );

    // Hand-build the forbidden shape: a negative attempt that claims a latency.
    failed.targets.latency_ms = Some(1_500.0);
    let reason = zroutery_core::ml::dataset::validate_outcome_sample(failed)
        .expect_err("a failed attempt carrying a latency must be refused");
    assert!(
        reason.contains("non-success attempt sample cannot carry success timing"),
        "the refusal names the discipline: {reason}"
    );

    // The same claim on the time-to-first-token field is refused too.
    failed.targets.latency_ms = None;
    failed.targets.ttft_ms = Some(42.0);
    assert!(
        zroutery_core::ml::dataset::validate_outcome_sample(failed).is_err(),
        "ttft is the same claim as latency"
    );

    // Cost stays exempt on both sides: it is money spent, not a latency.
    failed.targets.ttft_ms = None;
    failed.targets.cost = Some(0.004);
    zroutery_core::ml::dataset::validate_outcome_sample(failed)
        .expect("a failed attempt may still carry the cost it incurred");
    store
        .push(failed.clone())
        .expect("and the store accepts it");
    let stored = store
        .training_slice()
        .into_iter()
        .find(|sample| sample.sample_id == failed.sample_id)
        .expect("the exempt sample is stored");
    assert_eq!(stored.targets.cost, Some(0.004));
}

/// The positive case is unaffected: a successful attempt keeps its timing.
#[test]
fn a_successful_attempt_still_carries_its_own_timing() {
    let samples = zroutery_core::ml::dataset::try_samples_from_outcome(
        &failover_outcome(),
        &[features(11), features(12)],
        DataOrigin::Native,
    )
    .expect("the failover outcome converts");
    let served = samples
        .iter()
        .find(|sample| sample.success)
        .expect("the failover keeps a served attempt sample");
    assert!(
        served.targets.latency_ms.is_some() || served.targets.ttft_ms.is_some(),
        "a served attempt reports the timing that was actually measured"
    );
    zroutery_core::ml::dataset::validate_outcome_sample(served)
        .expect("a successful attempt may carry its own timing");
}

/// A failed attempt inside an otherwise successful request keeps its own label:
/// the failover's first candidate stays a negative sample.
#[test]
fn a_failover_keeps_the_failed_attempt_as_its_own_negative_sample() {
    let store = DatasetStore::new(10, 3600);
    let outcome = failover_outcome();
    ingest_ok(&store, &outcome, &failover_input());

    let samples = store.training_slice();
    let first = find(&samples, "attempt-0");
    let second = find(&samples, "attempt-1");
    assert!(
        !first.success,
        "the rate-limited candidate is a negative sample"
    );
    assert_eq!(first.targets.failure_class.as_deref(), Some("RateLimit"));
    assert!(
        second.success,
        "the answering candidate is a positive sample"
    );
    assert!(second.targets.failure_class.is_none());
    assert_eq!(first.final_status, FinalStatus::Success);
    assert_eq!(second.final_status, FinalStatus::Success);
}

/// Every non-success terminal state a request can reach is stored as itself.
#[test]
fn every_non_success_terminal_state_is_retained_as_itself() {
    let store = DatasetStore::new(10, 3600);
    let input = candidate("model-a", "alpha", features(50));

    // Production settles the open attempt of a stream that broke or was
    // abandoned with the same classification the request ends on, so the
    // attempt evidence and the terminal state agree.
    let interrupted = Outcome::builder("req-interrupted")
        .single_candidate("model-a", "alpha")
        .dialect("openai")
        .attempt(attempt(
            "model-a",
            "alpha",
            false,
            Some(FailureClass::Interrupted),
        ))
        .interrupted()
        .usage(zroutery_core::ir::Usage {
            input_tokens: 12,
            output_tokens: 5,
            ..Default::default()
        })
        .build();
    let cancelled = Outcome::builder("req-cancelled")
        .single_candidate("model-a", "alpha")
        .dialect("openai")
        .attempt(attempt(
            "model-a",
            "alpha",
            false,
            Some(FailureClass::ClientCancelled),
        ))
        .cancelled()
        .build();

    for outcome in [interrupted, cancelled] {
        assert!(!outcome.is_terminal_success());
        ingest_ok(&store, &outcome, &input);
    }

    let samples = store.training_slice();
    let interrupted_sample = request_sample(&samples, "req-interrupted");
    let cancelled_sample = request_sample(&samples, "req-cancelled");

    assert_eq!(interrupted_sample.final_status, FinalStatus::Interrupted);
    assert!(!interrupted_sample.success);
    assert!(!interrupted_sample.targets.success);
    assert_eq!(
        interrupted_sample.targets.failure_class.as_deref(),
        Some("Interrupted")
    );
    assert_eq!(cancelled_sample.final_status, FinalStatus::Cancelled);
    assert!(!cancelled_sample.success);
    assert_eq!(
        cancelled_sample.targets.failure_class.as_deref(),
        Some("ClientCancelled")
    );
    for sample in [interrupted_sample, cancelled_sample] {
        assert!(sample.identity.served.is_none(), "nothing was delivered");
        assert!(sample.targets.latency_ms.is_none());
        assert!(sample.targets.ttft_ms.is_none());
    }
    // The usage that arrived with the abandoned stream is still retained — it
    // is a captured fact, never a success signal.
    assert!(interrupted_sample.usage.is_some());
    assert!(!interrupted_sample.targets.success);
}

/// One request, one ingestion: the sample count is bounded by the evidence, and
/// a second ingestion of the same request is refused rather than duplicated.
#[test]
fn one_request_produces_one_bounded_ingestion() {
    let store = DatasetStore::new(10, 3600);
    let outcome = failover_outcome();
    let result = store.ingest(&outcome.request_id, &outcome, Some(&failover_input()));
    let Ingestion::Ingested { sample_ids } = result else {
        panic!("expected an ingestion, got {result:?}");
    };
    assert_eq!(sample_ids.len(), outcome.attempts.len() + 1);
    let scopes: Vec<SampleScope> = store
        .training_slice()
        .into_iter()
        .map(|sample| sample.scope)
        .collect();
    assert!(scopes.contains(&SampleScope::Request));
    assert_eq!(
        scopes
            .iter()
            .filter(|scope| matches!(scope, SampleScope::Attempt { .. }))
            .count(),
        outcome.attempts.len()
    );

    // The store, not just the caller, holds the one-ingestion rule.
    let again = store.ingest(&outcome.request_id, &outcome, Some(&failover_input()));
    let Ingestion::Rejected { reason } = again else {
        panic!("a second ingestion must be refused, got {again:?}");
    };
    assert!(reason.contains("already represented"), "got: {reason}");
    assert_eq!(store.len(), 3, "nothing was duplicated");
    assert_eq!(store.counters().ingested, 1);
    assert_eq!(store.counters().rejected, 1);
}

/// The count bound holds even when one request's evidence exceeds it.
#[test]
fn the_count_bound_holds_even_for_one_oversized_request() {
    let store = DatasetStore::new(1, 3600);
    let outcome = failover_outcome();
    // Two attempts plus the request is three samples against a bound of one.
    let result = store.ingest(&outcome.request_id, &outcome, Some(&failover_input()));
    let Ingestion::Ingested { sample_ids } = result else {
        panic!("expected an ingestion, got {result:?}");
    };
    assert_eq!(sample_ids.len(), 1, "only the bound is kept");
    assert_eq!(store.len(), 1, "the store never exceeds its bound");
    assert_eq!(store.counters().evicted_by_count, 2);
    assert_eq!(
        store.counters().samples,
        1,
        "only what is stored is counted"
    );
}

// ---------------------------------------------------------------------------
// purity and containment
// ---------------------------------------------------------------------------

/// A fault inside the dataset boundary is counted, not propagated.
#[test]
fn a_dataset_fault_is_contained_and_counted() {
    let store = DatasetStore::new(10, 3600);

    let result = contain_dataset_fault(&store, || panic!("injected dataset fault"));

    let err = result.unwrap_err();
    assert!(err.contains("dataset fault"), "got: {err}");
    assert_eq!(store.counters().faults, 1);
    assert!(store.is_empty(), "a fault stored nothing");
}

#[test]
fn a_contained_ingestion_never_raises_and_never_throws_away_its_reason() {
    let store = DatasetStore::new(10, 3600);
    let outcome = served_outcome();
    assert_eq!(store.counters(), no_counters());

    let ok = contained_ingest(&store, &outcome.request_id, &outcome, Some(&served_input()));
    assert!(ok.is_ingested());
    let refused = contained_ingest(&store, "req-other", &outcome, Some(&served_input()));
    assert!(matches!(refused, Ingestion::Rejected { .. }));
    let missing = contained_ingest(&store, &outcome.request_id, &outcome, None);
    assert!(missing.is_without_decision_time_input());
    assert_eq!(
        store.counters().faults,
        0,
        "an ordinary refusal is not a fault"
    );
}

/// The dataset module cannot route: it holds no routing, egress, health or
/// spend surface, and it can only be fed, never read for a decision.
#[test]
fn the_dataset_module_references_no_mutation_surface() {
    let source = include_str!("../src/ml/dataset.rs");
    for forbidden in [
        "plan_with_policy",
        "plan_classifier",
        "allow_request",
        "Router",
        "AppState",
        "Upstream",
        "upstream",
        "record_success",
        "record_failure",
        "clear_affinity",
        "charge",
        "extract_features",
        "ModelEnsemble",
        "try_train",
        "update_all",
    ] {
        assert!(
            !source.contains(forbidden),
            "dataset.rs must not reference {forbidden}"
        );
    }
}

/// Ingestion is the only thing the pipeline does with the dataset, it happens
/// after the outcome exists, and it returns nothing.
#[test]
fn the_pipeline_dataset_surface_is_ingestion_only() {
    let source = include_str!("../src/server/pipeline.rs");
    const BEGIN: &str = "// dataset-block-begin";
    const END: &str = "// dataset-block-end";
    let mut blocks = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find(BEGIN) {
        let after = &rest[start + BEGIN.len()..];
        let stop = after.find(END).expect("every dataset block is closed");
        blocks.push(&after[..stop]);
        rest = &after[stop + END.len()..];
    }
    assert_eq!(blocks.len(), 1, "one marked dataset block");
    let block = blocks[0];
    for forbidden in [
        // Nothing in the dataset path may fail or panic a request.
        ".unwrap(",
        ".expect(",
        "panic!",
        "unreachable!",
        // It is not a routing, health or spend input.
        "plan_with_policy",
        "plan_classifier",
        "allow_request",
        "state.upstream",
        "Upstream::",
        "record_classified_attempt",
        "report_success",
        "charge",
        "Router",
        // It learns nothing and activates nothing.
        "train(",
        "try_train(",
        ".swap(",
        "to_feedback",
    ] {
        assert!(
            !block.contains(forbidden),
            "pipeline dataset block must not reference {forbidden}"
        );
    }
    // One definition, one call site, and the call site is inside the one
    // terminal transition, after the outcome has been built and recorded.
    assert_eq!(source.matches("self.dataset_ingested(").count(), 1);
    let (before_finalize, after) = source
        .split_once("fn finalize(")
        .expect("the lifecycle still has its one terminal transition");
    assert!(
        before_finalize.contains("fn dataset_ingested("),
        "the ingestion is a lifecycle method, defined ahead of the transition"
    );
    let record = after
        .find("outcomes().record(outcome")
        .expect("record call");
    let ingest = after
        .find("self.dataset_ingested(")
        .expect("ingestion call");
    assert!(record < ingest, "the outcome exists before it is ingested");
    // The store is reached only through AppState, never by a second path.
    assert_eq!(source.matches(".dataset()").count(), 1);
}
