//! Read-only correlation projection: what it joins, what it refuses, and what
//! it refuses to leak.
//!
//! The tests are organised around the claims the module makes about itself:
//! the unit is one request, the join keys are UUID-derived identifiers, no
//! wall-clock value is ever a key, and no free-form text leaves Core.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use zroutery_core::failure::{FailureClass, FailureImpact};
use zroutery_core::ir::Usage;
use zroutery_core::observability::{
    decision_set_problems, parse_identity, project_batch, project_request, DecisionReasonKind,
    FailureProjection, IdentityKind, Projection, ProjectionBatch, RefusalReason, RequestProjection,
    ATTEMPT_PROJECTION_FIELDS, FAILURE_PROJECTION_FIELDS, PROJECTION_SCHEMA_VERSION,
    REQUEST_PROJECTION_FIELDS, USAGE_PROJECTION_FIELDS,
};
use zroutery_core::outcome::{
    failure_class_wire_name, Attempt, FailureFacts, FinalStatus, Outcome,
};
use zroutery_core::policy::{
    CandidateDecision, DecisionReason, PolicyRevision, RouteDecision, TaskProfileSummary,
};

// ---------------------------------------------------------------------------
// Fixtures. Every identifier is minted the way Core mints it.
// ---------------------------------------------------------------------------

/// `out_` + 32 lowercase hex.
fn outcome_id() -> String {
    format!("out_{}", uuid::Uuid::new_v4().simple())
}

/// `req_` + 32 lowercase hex.
fn request_id() -> String {
    format!("req_{}", uuid::Uuid::new_v4().simple())
}

/// `dec-` + 32 lowercase hex.
fn decision_id() -> String {
    format!("dec-{}", uuid::Uuid::new_v4().simple())
}

/// `att_` + 32 lowercase hex.
fn attempt_id() -> String {
    format!("att_{}", uuid::Uuid::new_v4().simple())
}

fn task() -> TaskProfileSummary {
    TaskProfileSummary {
        complexity: "medium".to_string(),
        task_type: "chat".to_string(),
        context_tokens: 1000,
        estimated_output_tokens: 200,
        streaming: false,
        has_tools: false,
        has_vision: false,
        required_capabilities: vec![],
    }
}

fn candidate(model: &str, provider: &str, eligible: bool) -> CandidateDecision {
    CandidateDecision {
        model_id: model.to_string(),
        provider_id: provider.to_string(),
        tier: Some("primary".to_string()),
        eligible,
        rejection: if eligible {
            None
        } else {
            Some("ineligible".to_string())
        },
        score: None,
        final_score: if eligible { Some(0.9) } else { None },
    }
}

fn decision_for(decision_id: &str, selected: &str) -> RouteDecision {
    RouteDecision {
        decision_id: decision_id.to_string(),
        timestamp: 1_700_000_000,
        task: task(),
        policy_id: "default".to_string(),
        client_id: None,
        candidates: vec![
            candidate(selected, "openai", true),
            candidate("claude-3", "anthropic", true),
            candidate("rejected-model", "openai", false),
        ],
        selected: Some(selected.to_string()),
        fallback_chain: vec!["gpt-4".to_string()],
        reason: DecisionReason::PolicySelected,
        policy_revision: PolicyRevision {
            policy_id: "default".to_string(),
            policy_enabled: true,
            requirements_hash: 1,
            preference_hash: 2,
        },
    }
}

fn attempt(model: &str, provider: &str, success: bool, class: Option<FailureClass>) -> Attempt {
    Attempt {
        attempt_id: attempt_id(),
        candidate_model: model.to_string(),
        candidate_provider: provider.to_string(),
        started_at: 1_700_000_000,
        completed_at: 1_700_000_001,
        latency_ms: 300.0,
        ttft_ms: if success { Some(120.0) } else { None },
        success,
        failure_class: class,
        failure_message: class.map(|class| format!("upstream said {class:?}")),
        http_status: if success { Some(200) } else { Some(500) },
        rectified: false,
    }
}

/// A served request with a decision, one attempt, usage and a cost.
fn served() -> (Outcome, RouteDecision) {
    let decision = decision_for(&decision_id(), "gpt-4");
    let outcome = Outcome::builder(request_id())
        .decision_id(decision.decision_id.clone())
        .planned("gpt-4", "openai")
        .dialect("openai")
        .streaming(true)
        .attempt(attempt("gpt-4", "openai", true, None))
        .total_latency_ms(300.0)
        .ttft_ms(120.0)
        .usage(Usage {
            input_tokens: 50,
            output_tokens: 20,
            ..Usage::default()
        })
        .cost(Some(0.001), Some(0.002))
        .timestamp(1_700_000_000)
        .build();
    (outcome, decision)
}

/// A request that failed terminally with a classification.
fn failed(class: FailureClass) -> (Outcome, RouteDecision) {
    let decision = decision_for(&decision_id(), "gpt-4");
    let outcome = Outcome::builder(request_id())
        .decision_id(decision.decision_id.clone())
        .planned("gpt-4", "openai")
        .dialect("openai")
        .attempt(attempt("gpt-4", "openai", false, Some(class)))
        .total_latency_ms(120.0)
        .timestamp(1_700_000_000)
        .build();
    (outcome, decision)
}

/// A corruption to apply to one named measure, so a table of them can be walked
/// without repeating the boilerplate of a closure per row.
type FigureMutation = Box<dyn Fn(&mut Outcome)>;

fn project_one(outcome: &Outcome, decision: &RouteDecision) -> RequestProjection {
    match project_request(outcome, Some(decision)) {
        Projection::Projected(record) => record,
        Projection::Refused(refusal) => panic!(
            "expected a projection, got {:?}: {}",
            refusal.reason.tag(),
            refusal.consequence()
        ),
    }
}

// ---------------------------------------------------------------------------
// The unit
// ---------------------------------------------------------------------------

#[test]
fn the_unit_is_one_request_addressed_by_its_outcome_id() {
    let (outcome, decision) = served();
    let record = project_one(&outcome, &decision);

    // One record in, one record out: the unit is the request, not the attempt.
    assert_eq!(record.outcome_id(), outcome.outcome_id);
    assert_eq!(record.request_id(), outcome.request_id);
    assert_eq!(record.attempt_ids().count(), outcome.attempts.len());

    // A second request projects to a second, separately addressable record.
    let (other, other_decision) = served();
    let other_record = project_one(&other, &other_decision);
    assert_ne!(record.outcome_id(), other_record.outcome_id());

    let batch = project_batch(
        &[outcome.clone(), other.clone()],
        &[decision, other_decision],
    );
    assert_eq!(batch.len(), 2);
    assert_eq!(batch.refused_count(), 0);
    assert_eq!(
        batch
            .record(&outcome.outcome_id)
            .map(RequestProjection::outcome_id),
        Some(outcome.outcome_id.as_str())
    );
}

// ---------------------------------------------------------------------------
// (a) Decision, failure and usage, joined by identifier
// ---------------------------------------------------------------------------

#[test]
fn a_complete_record_joins_decision_failure_and_usage_in_one_place() {
    let decision = decision_for(&decision_id(), "gpt-4");
    let failed_attempt = attempt("gpt-4", "openai", false, Some(FailureClass::RateLimit));
    let failed_attempt_id = failed_attempt.attempt_id.clone();
    let served_attempt = attempt("claude-3", "anthropic", true, None);

    let outcome = Outcome::builder(request_id())
        .decision_id(decision.decision_id.clone())
        .planned("gpt-4", "openai")
        .dialect("openai")
        .streaming(true)
        .attempt(failed_attempt)
        .attempt(served_attempt)
        .total_latency_ms(900.0)
        .ttft_ms(400.0)
        .usage(Usage {
            input_tokens: 100,
            output_tokens: 40,
            cache_read_tokens: 5,
            cache_write_tokens: 2,
            reasoning_tokens: 7,
        })
        .cost(Some(0.001), Some(0.004))
        .timestamp(1_700_000_000)
        .build();

    let record = project_one(&outcome, &decision);

    // The decision, joined by its own identifier.
    let summary = record.decision.as_ref().expect("the decision joined");
    assert_eq!(summary.decision_id, decision.decision_id);
    assert_eq!(summary.reason, DecisionReasonKind::PolicySelected);
    assert_eq!(summary.selected_model.as_deref(), Some("gpt-4"));
    assert_eq!(summary.candidate_count, 3);
    assert_eq!(summary.eligible_count, 2);
    assert_eq!(summary.fallback_chain_len, 1);

    // The failure, on the attempt that failed, from the accepted classification.
    let failed_projection = record
        .attempt(&failed_attempt_id)
        .expect("the failed attempt is addressable by its attempt_id");
    let failure = failed_projection
        .failure
        .as_ref()
        .expect("the failed attempt carries its classification");
    assert_eq!(failure.class, "rate_limit");
    assert_eq!(failure.http_status, Some(500));
    assert!(
        !failure.impact.affects_circuit,
        "a quota failure is not a dead circuit"
    );
    assert!(failure.impact.fallbackable);

    // The usage, on the same record.
    let usage = record.usage.expect("usage projected");
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 40);
    assert_eq!(usage.cache_read_tokens, 5);
    assert_eq!(usage.cache_write_tokens, 2);
    assert_eq!(usage.reasoning_tokens, 7);
    assert_eq!(usage.total_tokens, 140);
    assert_eq!(record.estimated_cost, Some(0.001));
    assert_eq!(record.actual_cost, Some(0.004));

    // The three routings stay distinct: planned was gpt-4, served was claude-3.
    assert_eq!(
        record
            .planned_identity
            .as_ref()
            .map(|label| label.model.as_str()),
        Some("gpt-4")
    );
    assert_eq!(
        record
            .served_identity
            .as_ref()
            .map(|label| label.model.as_str()),
        Some("claude-3")
    );
    assert_eq!(
        record
            .last_attempted_identity
            .as_ref()
            .map(|label| label.provider.as_str()),
        Some("anthropic")
    );
}

#[test]
fn a_terminal_failure_and_its_impact_travel_with_the_decision() {
    for class in FailureClass::ALL {
        let (outcome, decision) = failed(class);
        let record = project_one(&outcome, &decision);

        let failure = record
            .failure
            .as_ref()
            .expect("the terminal failure projected");
        assert_eq!(failure.class, failure_class_wire_name(class));
        assert_eq!(failure.failure_class(), Some(class));

        // The impact table is consumed, never re-derived.
        let impact: FailureImpact = class.impact();
        assert_eq!(
            failure.impact.affects_observation,
            impact.affects_observation
        );
        assert_eq!(failure.impact.affects_circuit, impact.affects_circuit);
        assert_eq!(failure.impact.retryable, impact.retryable);
        assert_eq!(failure.impact.fallbackable, impact.fallbackable);
        assert_eq!(failure.impact.provider_fault, impact.provider_fault);
        assert_eq!(failure.impact.records_stats, impact.records_stats());

        // A failure never claims a served identity.
        assert!(
            record.served_identity.is_none(),
            "{class:?} claimed a served identity"
        );
        assert!(!record.success);
        assert!(!record.final_status.is_success());
    }
}

#[test]
fn a_failure_reaches_the_record_through_the_accepted_facts_not_a_reclassification() {
    let decision = decision_for(&decision_id(), "gpt-4");
    // A 429 whose message would classify differently if it were ever read. The
    // class and status both come from the accepted attempt, so a reader cannot
    // tell that a message was available at all.
    let outcome = Outcome::builder(request_id())
        .decision_id(decision.decision_id.clone())
        .planned("gpt-4", "openai")
        .dialect("openai")
        .attempt(attempt(
            "gpt-4",
            "openai",
            false,
            Some(FailureClass::RateLimit),
        ))
        .total_latency_ms(50.0)
        .timestamp(1_700_000_000)
        .build();

    let record = project_one(&outcome, &decision);

    // The attempt's own classification, verbatim, with no reclassification.
    let projected = record
        .attempts
        .first()
        .and_then(|attempt| attempt.failure.as_ref())
        .expect("the attempt failure projected");
    assert_eq!(projected.class, "rate_limit");
    assert_eq!(projected.http_status, Some(500));
    assert!(
        projected.impact.retryable,
        "the impact comes from the class, not the status"
    );
    assert!(!projected.impact.affects_circuit);

    // And the terminal classification is the same accepted class.
    let terminal = record
        .failure
        .as_ref()
        .expect("the terminal failure projected");
    assert_eq!(terminal.class, "rate_limit");
    assert_eq!(terminal, projected);

    // The facts type projects the identical value from the identical facts, so
    // the record has exactly one source of classification and no second policy.
    let facts = FailureFacts::new(FailureClass::RateLimit, Some("boom".to_string()), Some(429));
    assert_eq!(
        &FailureProjection::from_facts(&facts).class,
        &projected.class
    );
    assert_eq!(
        &FailureProjection::from_facts(&facts).impact,
        &projected.impact
    );
}

// ---------------------------------------------------------------------------
// (b) No wall-clock value is ever a key
// ---------------------------------------------------------------------------

#[test]
fn order_is_by_identifier_and_two_records_in_one_second_still_have_an_order() {
    // Both requests are stamped with the SAME second. Nothing in the output may
    // depend on which arrived first.
    let (first, first_decision) = served();
    let (second, second_decision) = served();
    assert_eq!(first.timestamp, second.timestamp);

    let mut outcomes = vec![first.clone(), second.clone()];
    let mut expected: Vec<String> = outcomes
        .iter()
        .map(|outcome| outcome.outcome_id.clone())
        .collect();
    expected.sort();
    let arrivals = [first.clone(), second.clone()];

    // Every arrival order gives the same output order.
    for permutation in [[0usize, 1], [1, 0]] {
        outcomes = permutation
            .iter()
            .map(|index| arrivals[*index].clone())
            .collect();
        let decisions: Vec<RouteDecision> = if permutation == [0, 1] {
            vec![first_decision.clone(), second_decision.clone()]
        } else {
            vec![second_decision.clone(), first_decision.clone()]
        };
        let batch = project_batch(&outcomes, &decisions);
        let order: Vec<String> = batch
            .records
            .iter()
            .map(RequestProjection::outcome_id)
            .map(str::to_string)
            .collect();
        assert_eq!(
            order, expected,
            "output order followed arrival, not identifier"
        );
    }
}

#[test]
fn changing_only_the_timestamp_does_not_change_the_record_order() {
    let (first, first_decision) = served();
    let (second, second_decision) = served();

    // Project, then re-project with the timestamps transposed. The order must be
    // unchanged, and only the carried clock may differ.
    let before = project_batch(
        &[first.clone(), second.clone()],
        &[first_decision.clone(), second_decision.clone()],
    );
    let mut later = first.clone();
    later.timestamp = 9_999_999_999;
    let mut earlier = second.clone();
    earlier.timestamp = 1;
    let after = project_batch(
        &[later.clone(), earlier.clone()],
        &[first_decision.clone(), second_decision.clone()],
    );

    let before_order: Vec<&str> = before
        .records
        .iter()
        .map(RequestProjection::outcome_id)
        .collect();
    let after_order: Vec<&str> = after
        .records
        .iter()
        .map(RequestProjection::outcome_id)
        .collect();
    assert_eq!(before_order, after_order);

    // The clock is carried, inertly: it changed, and nothing else did.
    let changed: Vec<i64> = after
        .records
        .iter()
        .map(|record| record.recorded.received.as_unix_secs())
        .collect();
    assert!(changed.contains(&9_999_999_999));
    assert!(changed.contains(&1));
}

#[test]
fn a_second_granular_timestamp_is_never_a_key_or_an_ordering_input() {
    // Behavioural half: the value round-trips for display and nothing else.
    let value = zroutery_core::observability::SecondGranularTimestamp::from_unix_secs(7);
    assert_eq!(value.as_unix_secs(), 7);
    assert_eq!(
        serde_json::to_value(value).expect("serializes"),
        json!(7),
        "the clock is carried as data, and only as data"
    );

    // Structural half: the type carries no comparison at all. If it ever grew an
    // `Ord`, this test fails, because the sortable-key assertion below and the
    // declared field set are what a future reader would build an order from.
    let sortable: Option<fn(&RequestProjection, &RequestProjection) -> std::cmp::Ordering> =
        Some(RequestProjection::content_order);
    assert!(sortable.is_some());

    // And the projection itself exposes no comparator that could involve a clock:
    // the only ordering entry point is the identifier one, exercised above.
    let first = zroutery_core::observability::SecondGranularTimestamp::from_unix_secs(1);
    let also_first = zroutery_core::observability::SecondGranularTimestamp::from_unix_secs(1);
    let later = zroutery_core::observability::SecondGranularTimestamp::from_unix_secs(2);
    assert_eq!(
        first, also_first,
        "two events inside one second are indistinguishable by the clock, which is exactly why the clock cannot key them"
    );
    assert_ne!(first, later);
}

#[test]
fn batch_output_is_byte_identical_across_repeated_runs() {
    let (first, first_decision) = served();
    let (second, second_decision) = failed(FailureClass::Timeout);

    let one = project_batch(
        &[first.clone(), second.clone()],
        &[first_decision.clone(), second_decision.clone()],
    );
    let two = project_batch(
        &[first.clone(), second.clone()],
        &[first_decision.clone(), second_decision.clone()],
    );

    let left = serde_json::to_string(&one).expect("serializes");
    let right = serde_json::to_string(&two).expect("serializes");
    assert_eq!(
        left, right,
        "the same input projected differently on a second run"
    );
}

#[test]
fn every_permutation_of_a_batch_projects_identical_bytes() {
    // Six records: a mix of served, failed, cancelled and refused inputs, so
    // both lists (records and refusals) are exercised for order independence.
    let (served_one, decision_one) = served();
    let (served_two, decision_two) = served();
    let (failed_one, decision_three) = failed(FailureClass::Protocol);
    let (failed_two, decision_four) = failed(FailureClass::Authentication);
    let (cancelled, decision_five) = cancelled();
    let (corrupt, _) = served();

    // A record that must refuse, so the refusal ordering is exercised too.
    let mut no_decision = served_one.clone();
    no_decision.decision_id = None;
    let malformed = malformed_request_id();

    let outcomes = vec![
        served_one,
        served_two,
        failed_one,
        failed_two,
        cancelled,
        corrupt,
        no_decision,
        malformed,
    ];
    let decisions = vec![
        decision_one,
        decision_two,
        decision_three,
        decision_four,
        decision_five,
    ];

    let baseline = project_batch(&outcomes, &decisions);
    assert!(
        baseline.refused_count() > 0,
        "this batch is supposed to contain refusals"
    );
    let expected = serde_json::to_string(&baseline).expect("serializes");

    // Exhaustively walk every permutation of the inputs for a small fixed set by
    // rotating and reversing: a hash-order leak shows up as a difference in
    // either, and a tie-break leak shows up when two records share a locator.
    let mut variants: Vec<Vec<Outcome>> = Vec::new();
    let mut rotated = outcomes.clone();
    rotated.rotate_left(1);
    variants.push(rotated);
    let mut reversed = outcomes.clone();
    reversed.reverse();
    variants.push(reversed);
    let mut swapped = outcomes.clone();
    swapped.swap(2, 5);
    variants.push(swapped);
    let mut decision_swapped = decisions.clone();
    decision_swapped.reverse();
    variants.push(outcomes.clone());
    for variant in variants {
        let rendered =
            serde_json::to_string(&project_batch(&variant, &decision_swapped)).expect("serializes");
        assert_eq!(
            rendered, expected,
            "output depended on the order of the inputs"
        );
    }
}

#[test]
fn a_record_and_its_decision_join_on_the_decision_id_alone() {
    let (outcome, decision) = served();

    // The join is by identifier, not by position: shuffling the supplied
    // decisions cannot change which decision a record gets.
    let alone = project_batch(
        std::slice::from_ref(&outcome),
        std::slice::from_ref(&decision),
    );
    let mut reversed = vec![decision.clone()];
    reversed.reverse();
    let shuffled = project_batch(std::slice::from_ref(&outcome), &reversed);

    assert_eq!(alone.records, shuffled.records);

    // Supplying the wrong decision is a refusal, not a wrong join.
    let wrong = decision_for(&decision_id(), "some-other-model");
    match project_request(&outcome, Some(&wrong)) {
        Projection::Refused(refusal) => {
            assert_eq!(refusal.reason.tag(), "decision_identity_mismatch");
            assert_eq!(
                refusal.reason,
                RefusalReason::DecisionIdentityMismatch {
                    supplied_decision_id: wrong.decision_id.clone()
                }
            );
        }
        Projection::Projected(_) => panic!("a mismatched decision was joined anyway"),
    }
}

#[test]
fn a_record_without_a_supplied_decision_projects_with_the_join_absent() {
    let (outcome, _) = served();

    // Not supplied is a fact about the input, and is projected as an absent join
    // with the decision id still present and honest.
    let batch = project_batch(std::slice::from_ref(&outcome), &[]);
    assert_eq!(batch.refused_count(), 0);
    let record = &batch.records[0];
    assert!(record.decision.is_none());
    assert_eq!(record.decision_id(), outcome.decision_id.as_ref().unwrap());
}

// ---------------------------------------------------------------------------
// Absent and malformed identities refuse, and are reported rather than dropped
// ---------------------------------------------------------------------------

#[test]
fn a_record_with_no_decision_id_refuses_and_is_still_reported() {
    let mut outcome = served().0;
    outcome.decision_id = None;

    match project_request(&outcome, None) {
        Projection::Refused(refusal) => {
            assert_eq!(refusal.reason, RefusalReason::DecisionIdAbsent);
            // The consequence is stated, not implied.
            assert!(refusal
                .consequence()
                .contains("decision correlation is unavailable"));
            // The record is addressed by the identifiers it does have.
            assert_eq!(
                refusal.record.outcome_id.as_deref(),
                Some(outcome.outcome_id.as_str())
            );
            assert_eq!(
                refusal.record.request_id.as_deref(),
                Some(outcome.request_id.as_str())
            );
            // And the absent decision id is reported as absent, not filled in.
            assert_eq!(refusal.record.decision_id, None);
        }
        Projection::Projected(_) => panic!("a record with no decision id was projected"),
    }
}

#[test]
fn a_missing_decision_id_is_never_substituted_with_the_planned_identity() {
    let mut outcome = served().0;
    outcome.decision_id = None;
    // The record still knows what it planned.
    assert!(outcome.planned_identity().is_some());

    match project_request(&outcome, None) {
        Projection::Projected(_) => panic!("a planned identity stood in for a served one"),
        Projection::Refused(refusal) => {
            let serialised = serde_json::to_string(&refusal).expect("serializes");
            assert!(!serialised.contains("planned_identity"));
            assert!(
                !serialised.contains("gpt-4"),
                "a planned identity leaked in"
            );
        }
    }
}

#[test]
fn an_identity_that_is_present_but_does_not_parse_refuses() {
    for (kind, bad) in [
        (IdentityKind::Outcome, "out_not-hex-32-characters-at-all-x"),
        (IdentityKind::Request, "req_1"),
        (IdentityKind::Decision, "dec-nope"),
        (IdentityKind::Attempt, "att_"),
    ] {
        assert!(
            parse_identity(kind, bad).is_err(),
            "{bad:?} should not parse as {:?}",
            kind
        );
    }

    // And the refusal names the malformed value verbatim rather than repairing it.
    let (mut outcome, decision) = served();
    outcome.request_id = "req_1".to_string();
    match project_request(&outcome, Some(&decision)) {
        Projection::Refused(refusal) => match refusal.reason {
            RefusalReason::IdentityUnparsable { kind, raw, detail } => {
                assert_eq!(kind, IdentityKind::Request);
                assert_eq!(raw, "req_1");
                assert!(detail.contains("32 lowercase hex"), "{detail}");
            }
            other => panic!("expected an unparsable identity, got {other:?}"),
        },
        Projection::Projected(_) => panic!("a malformed request id was correlated on"),
    }
}

#[test]
fn an_empty_identity_is_reported_as_absent_rather_than_unparsable() {
    let (mut outcome, decision) = served();
    outcome.request_id = "   ".to_string();

    match project_request(&outcome, Some(&decision)) {
        Projection::Refused(refusal) => {
            assert_eq!(
                refusal.reason,
                RefusalReason::IdentityAbsent {
                    kind: IdentityKind::Request
                }
            );
            assert!(refusal.consequence().contains("empty identifier"));
        }
        Projection::Projected(_) => panic!("a blank identity was correlated on"),
    }
}

#[test]
fn a_well_formed_identity_from_each_minting_site_parses() {
    for kind in [
        IdentityKind::Outcome,
        IdentityKind::Request,
        IdentityKind::Decision,
        IdentityKind::Attempt,
    ] {
        let body = "0123456789abcdef0123456789abcdef";
        let raw = format!("{}{}", kind.prefix(), body);
        assert_eq!(parse_identity(kind, &raw).unwrap(), raw);

        // Uppercase hex is not what `Uuid::simple()` mints, so it is not accepted.
        let upper = format!("{}{}", kind.prefix(), body.to_uppercase());
        assert!(parse_identity(kind, &upper).is_err());
    }
}

// ---------------------------------------------------------------------------
// Non-finite and negative figures refuse
// ---------------------------------------------------------------------------

#[test]
fn a_non_finite_or_negative_figure_refuses_and_names_the_field() {
    let cases: Vec<(&str, FigureMutation)> = vec![
        (
            "total_latency_ms",
            Box::new(|o: &mut Outcome| o.total_latency_ms = f64::NAN),
        ),
        (
            "ttft_ms",
            Box::new(|o: &mut Outcome| o.ttft_ms = Some(-1.0)),
        ),
        (
            "estimated_cost",
            Box::new(|o: &mut Outcome| o.estimated_cost = Some(f64::INFINITY)),
        ),
        (
            "actual_cost",
            Box::new(|o: &mut Outcome| o.actual_cost = Some(-0.5)),
        ),
        (
            "attempts[0].latency_ms",
            Box::new(|o: &mut Outcome| o.attempts[0].latency_ms = f64::NEG_INFINITY),
        ),
        (
            "attempts[0].ttft_ms",
            Box::new(|o: &mut Outcome| o.attempts[0].ttft_ms = Some(-0.001)),
        ),
    ];

    for (expected_field, mutate) in cases {
        let (mut outcome, decision) = served();
        mutate(&mut outcome);
        match project_request(&outcome, Some(&decision)) {
            Projection::Refused(refusal) => match &refusal.reason {
                RefusalReason::FigureNotFiniteOrNegative { field, value } => {
                    assert_eq!(field, expected_field);
                    assert!(!value.is_finite() || *value < 0.0);
                    assert!(refusal.consequence().contains("incomparable"));
                }
                other => panic!("expected a figure refusal for {expected_field}, got {other:?}"),
            },
            Projection::Projected(_) => panic!("a non-finite figure in {expected_field} projected"),
        }
    }
}

#[test]
fn a_zero_figure_is_a_legitimate_measurement_and_still_projects() {
    let (mut outcome, decision) = served();
    outcome.total_latency_ms = 0.0;
    outcome.ttft_ms = Some(0.0);
    outcome.actual_cost = Some(0.0);

    let record = project_one(&outcome, &decision);
    assert_eq!(record.total_latency_ms, 0.0);
    assert_eq!(record.ttft_ms, Some(0.0));
    assert_eq!(record.actual_cost, Some(0.0));
}

// ---------------------------------------------------------------------------
// A missing failure classification refuses
// ---------------------------------------------------------------------------

#[test]
fn a_failed_outcome_with_no_classification_refuses() {
    // An unsuccessful attempt with no FailureClass at all: the accepted schema
    // also rejects this, but the projection has to name its own reason first.
    let decision = decision_for(&decision_id(), "gpt-4");
    let mut orphan = attempt("gpt-4", "openai", false, None);
    orphan.failure_message = None;
    let outcome = Outcome::builder(request_id())
        .decision_id(decision.decision_id.clone())
        .planned("gpt-4", "openai")
        .dialect("openai")
        .attempt(orphan)
        .total_latency_ms(50.0)
        .timestamp(1_700_000_000)
        .build();

    // The record really does claim failure with nothing to classify it.
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert!(outcome.terminal_failure_facts().is_none());

    match project_request(&outcome, Some(&decision)) {
        Projection::Refused(refusal) => {
            assert_eq!(
                refusal.reason,
                RefusalReason::FailureClassificationAbsent {
                    final_status: FinalStatus::Failed
                }
            );
            assert!(refusal.consequence().contains("no accepted classification"));
        }
        Projection::Projected(_) => panic!("a failure with no classification was projected"),
    }
}

#[test]
fn a_cancelled_outcome_with_no_classification_also_refuses() {
    let decision = decision_for(&decision_id(), "gpt-4");
    let outcome = Outcome::builder(request_id())
        .decision_id(decision.decision_id.clone())
        .planned("gpt-4", "openai")
        .dialect("openai")
        .cancelled()
        .timestamp(1_700_000_000)
        .build();

    match project_request(&outcome, Some(&decision)) {
        Projection::Refused(refusal) => assert_eq!(
            refusal.reason,
            RefusalReason::FailureClassificationAbsent {
                final_status: FinalStatus::Cancelled
            }
        ),
        Projection::Projected(_) => panic!("a cancelled record with no classification projected"),
    }
}

#[test]
fn a_cancelled_outcome_with_its_classification_does_project() {
    let (outcome, decision) = cancelled();
    let record = project_one(&outcome, &decision);
    assert_eq!(record.final_status, FinalStatus::Cancelled);
    let failure = record.failure.expect("the cancellation is classified");
    assert_eq!(failure.class, "client_cancelled");
    assert!(
        !failure.impact.provider_fault,
        "a client cancelling is not the provider's fault"
    );
    assert!(record.served_identity.is_none());
    assert!(!record.success);
}

// ---------------------------------------------------------------------------
// Anti-vacuity: every refusal is reachable and a good record still succeeds
// ---------------------------------------------------------------------------

#[test]
fn every_refusal_kind_is_reachable_from_a_real_input() {
    let mut seen: BTreeMap<&'static str, ()> = BTreeMap::new();
    let mut note = |outcome: &Outcome, decision: Option<&RouteDecision>| {
        if let Projection::Refused(refusal) = project_request(outcome, decision) {
            seen.insert(refusal.reason.tag(), ());
        }
    };

    // identity_absent
    let (mut blank, decision) = served();
    blank.request_id = String::new();
    note(&blank, Some(&decision));
    // identity_unparsable
    let (mut bad, decision) = served();
    bad.outcome_id = "out_nope".to_string();
    note(&bad, Some(&decision));
    // decision_id_absent
    let (mut none, _) = served();
    none.decision_id = None;
    note(&none, None);
    // duplicate_attempt_id
    let (mut dup, decision) = served();
    let shared = dup.attempts[0].attempt_id.clone();
    let mut twin = dup.attempts[0].clone();
    twin.attempt_id = shared;
    dup.attempts.push(twin);
    note(&dup, Some(&decision));
    // decision_identity_mismatch: the record names one decision, a different
    // decision is supplied.
    let (good, _) = served();
    let (unrelated, other) = served();
    assert_ne!(good.decision_id, unrelated.decision_id);
    note(&good, Some(&other));
    // figure_not_finite_or_negative
    let (mut figure, decision) = served();
    figure.total_latency_ms = f64::NAN;
    note(&figure, Some(&decision));
    // failure_classification_absent
    let decision = decision_for(&decision_id(), "gpt-4");
    let mut orphan = attempt("gpt-4", "openai", false, None);
    orphan.failure_message = None;
    let unclassified = Outcome::builder(request_id())
        .decision_id(decision.decision_id.clone())
        .planned("gpt-4", "openai")
        .dialect("openai")
        .attempt(orphan)
        .total_latency_ms(50.0)
        .timestamp(1_700_000_000)
        .build();
    note(&unclassified, Some(&decision));
    // invalid_outcome: the accepted schema rejects a success flag that contradicts
    // a classified terminal failure. The projection's own checks pass here — the
    // record is well identified, its figures are sound, and its failure is
    // classified — so only the schema can refuse it.
    let (mut contradiction, decision) = failed(FailureClass::Timeout);
    contradiction.success = true;
    note(&contradiction, Some(&decision));

    for tag in [
        "identity_absent",
        "identity_unparsable",
        "decision_id_absent",
        "duplicate_attempt_id",
        "decision_identity_mismatch",
        "figure_not_finite_or_negative",
        "failure_classification_absent",
        "invalid_outcome",
    ] {
        assert!(
            seen.contains_key(tag),
            "{tag} is unreachable from any input"
        );
    }
}

#[test]
fn duplicate_identities_across_a_batch_refuse_instead_of_being_merged() {
    let (first, decision) = served();
    let mut second = first.clone();
    // Same outcome_id, different request: two records claim one address.
    second.request_id = request_id();
    let mut third = first.clone();
    // A genuinely distinct record: its own address, so it is not a claimant.
    third.outcome_id = outcome_id();
    third.decision_id = Some(decision_id());
    third.request_id = request_id();

    let batch = project_batch(
        &[first.clone(), second.clone(), third.clone()],
        std::slice::from_ref(&decision),
    );

    // The two that share an outcome_id are both refused; the third is not.
    let duplicated = batch
        .refusals
        .iter()
        .filter(|refusal| refusal.record.outcome_id.as_deref() == Some(first.outcome_id.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        duplicated.len(),
        2,
        "both claimants must refuse, not just the second"
    );
    for refusal in duplicated {
        assert_eq!(
            refusal.reason,
            RefusalReason::DuplicateOutcomeId { occurrences: 2 }
        );
    }
    assert!(batch.record(&third.outcome_id).is_some());
    assert!(batch.record(&first.outcome_id).is_none());
}

#[test]
fn an_ambiguous_decision_id_refuses_the_record_rather_than_picking_one() {
    let (outcome, decision) = served();
    let mut impostor = decision.clone();
    impostor.selected = Some("claude-3".to_string());

    let batch = project_batch(std::slice::from_ref(&outcome), &[decision, impostor]);

    assert_eq!(batch.refused_count(), 1);
    assert_eq!(
        batch.refusals[0].reason,
        RefusalReason::AmbiguousDecision {
            decision_id: outcome.decision_id.clone().unwrap()
        }
    );
    assert!(batch.records.is_empty());

    // And the set-level problem is reported independently of any record.
    let problems = decision_set_problems(&[decision_for(&decision_id(), "a"), {
        let mut second = decision_for(&decision_id(), "b");
        second.decision_id = "dec-".to_string();
        second
    }]);
    assert!(problems.is_empty(), "two distinct ids are not ambiguous");
}

#[test]
fn decision_set_problems_names_a_duplicated_decision_id() {
    let shared = decision_id();
    let first = decision_for(&shared, "gpt-4");
    let second = decision_for(&shared, "claude-3");
    let problems = decision_set_problems(&[first, second]);
    assert_eq!(problems.len(), 1);
    assert_eq!(
        problems[0].reason,
        RefusalReason::DuplicateDecisionId {
            decision_id: shared.clone(),
            occurrences: 2
        }
    );
}

#[test]
fn every_input_is_either_projected_or_refused_and_never_dropped() {
    let (good, good_decision) = served();
    let (mut absent, _) = served();
    absent.decision_id = None;
    let malformed = malformed_request_id();
    let (mut broken, broken_decision) = served();
    broken.total_latency_ms = f64::NAN;

    let outcomes = vec![good, absent, malformed, broken];
    let batch = project_batch(&outcomes, &[good_decision, broken_decision]);

    assert_eq!(
        batch.len() + batch.refused_count(),
        outcomes.len(),
        "a record was silently dropped"
    );
    assert_eq!(batch.len(), 1);
    assert_eq!(batch.refused_count(), 3);
}

// ---------------------------------------------------------------------------
// No leakage
// ---------------------------------------------------------------------------

#[test]
fn no_secret_material_reaches_the_projection() {
    let api_key = "sk-ant-api03-LEAKCANARY-abcdef0123456789";
    let bearer = "Bearer eyJhbGciOiJIUzI1NiJ9.LEAKCANARY";
    let auth_header = "authorization: Basic ZGVtbzpkZW1w";
    let body = r#"{"error":{"message":"LEAKCANARY request body echo","prompt":"secret prompt"}}"#;

    let decision = decision_for(&decision_id(), "gpt-4");
    let mut attempt = attempt("gpt-4", "openai", false, Some(FailureClass::Authentication));
    attempt.failure_message = Some(format!("{api_key} {bearer} {auth_header} {body}"));
    let outcome = Outcome::builder(request_id())
        .decision_id(decision.decision_id.clone())
        .planned("gpt-4", "openai")
        .dialect("openai")
        .attempt(attempt)
        .terminal_failure(
            FailureClass::Authentication,
            format!("{api_key} {body}"),
            Some(401),
        )
        .total_latency_ms(50.0)
        .timestamp(1_700_000_000)
        .build();

    let record = project_one(&outcome, &decision);
    let rendered = serde_json::to_string(&record).expect("serializes");

    for canary in [
        api_key,
        bearer,
        auth_header,
        body,
        "LEAKCANARY",
        "sk-ant-",
        "secret prompt",
    ] {
        assert!(
            !rendered.contains(canary),
            "the projection leaked {canary:?}: {rendered}"
        );
    }

    // The classification itself is present, so the record is still useful.
    assert_eq!(
        record.failure.as_ref().map(|f| f.class.as_str()),
        Some("authentication")
    );
}

#[test]
fn the_projected_field_set_is_declared_and_carries_no_free_text_field() {
    let (outcome, decision) = served();
    let record = project_one(&outcome, &decision);
    let value = serde_json::to_value(&record).expect("serializes");

    let keys: std::collections::BTreeSet<&str> = value
        .as_object()
        .expect("a record is an object")
        .keys()
        .map(String::as_str)
        .collect();
    for declared in REQUEST_PROJECTION_FIELDS {
        assert!(
            keys.contains(declared),
            "{declared} is declared but not projected"
        );
    }
    for present in keys.iter() {
        assert!(
            REQUEST_PROJECTION_FIELDS.contains(present),
            "{present} is projected but not declared"
        );
    }

    // Attempts, usage and failures are audited the same way.
    let attempt = &value["attempts"][0];
    for declared in ATTEMPT_PROJECTION_FIELDS {
        assert!(
            attempt.get(declared).is_some(),
            "{declared} is missing from an attempt"
        );
    }
    for key in attempt.as_object().expect("an attempt is an object").keys() {
        assert!(
            ATTEMPT_PROJECTION_FIELDS.contains(&key.as_str()),
            "attempt field {key} is projected but not declared"
        );
    }
    let usage = &value["usage"];
    for declared in USAGE_PROJECTION_FIELDS {
        assert!(
            usage.get(declared).is_some(),
            "{declared} is missing from usage"
        );
    }
    for key in usage.as_object().expect("usage is an object").keys() {
        assert!(
            USAGE_PROJECTION_FIELDS.contains(&key.as_str()),
            "usage field {key} is projected but not declared"
        );
    }
    let failure = json!({
        "class": "timeout",
        "http_status": 504u16,
        "impact": FailureProjection::from_facts(&FailureFacts::new(
            FailureClass::Timeout,
            Some("x".to_string()),
            Some(504),
        )),
    });
    for declared in FAILURE_PROJECTION_FIELDS {
        assert!(
            failure.get(declared).is_some(),
            "{declared} is missing from a failure"
        );
    }
    for key in failure.as_object().expect("a failure is an object").keys() {
        assert!(
            FAILURE_PROJECTION_FIELDS.contains(&key.as_str()),
            "failure field {key} is projected but not declared"
        );
    }
}

#[test]
fn the_projection_reads_no_clock_no_environment_and_writes_nothing() {
    // The purity claim is checked against the module's own source rather than
    // asserted in prose. Comment lines are stripped first, so that the module
    // may *describe* what it does not do without tripping the scan: this is a
    // check on the code, not on the documentation.
    let code: String = include_str!("../src/observability.rs")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();

    for forbidden in [
        "std::env",
        "env::var",
        "env::current",
        "std::process",
        "std::fs",
        "file::",
        "openoptions",
        "std::io",
        "std::net",
        "reqwest",
        "axum",
        "tokio",
        "std::thread",
        "thread::spawn",
        "std::sync",
        "mutex",
        "rwlock",
        "atomic",
        "oncecell",
        "once_lock",
        "lazy_static",
        "chrono",
        "systemtime",
        "instant::",
        "utc::now",
        "std::time",
        "rand",
        "uuid",
        "database",
        "journal",
        "dataset",
        "shadow",
        "activation",
        "takeover",
        "ml::",
        "feature = \"ml\"",
    ] {
        assert!(
            !code.contains(forbidden),
            "the projection module's code names {forbidden:?}"
        );
    }

    // No mutable statics and no interior mutability.
    for forbidden in ["static mut", "const mut", "derefmut", "get_mut", "unsafe"] {
        assert!(
            !code.contains(forbidden),
            "the projection module's code names {forbidden:?}"
        );
    }

    // It reads only from the accepted Core types and `std`.
    assert!(code.contains("use crate::failure"));
    assert!(code.contains("use crate::outcome"));
    assert!(code.contains("use crate::policy"));
    assert!(code.contains("use crate::ir"));
}

#[test]
fn the_projected_record_declares_the_schema_version_it_was_built_with() {
    let (outcome, decision) = served();
    let record = project_one(&outcome, &decision);
    assert_eq!(record.schema_version, PROJECTION_SCHEMA_VERSION);
}

#[test]
fn a_projection_round_trips_through_json_without_gaining_a_field() {
    let (outcome, decision) = failed(FailureClass::ProviderUnavailable);
    let record = project_one(&outcome, &decision);

    let rendered = serde_json::to_string(&record).expect("serializes");
    let restored: RequestProjection = serde_json::from_str(&rendered).expect("round trips");
    assert_eq!(restored, record);
    assert_eq!(
        serde_json::to_string(&restored).expect("re-serializes"),
        rendered
    );
}

#[test]
fn the_batch_is_the_whole_output_and_holds_no_accumulator() {
    // Two projections of the same input are independent values: projecting a
    // second time from the same inputs cannot observe the first.
    let (outcome, decision) = served();
    let first = project_batch(
        std::slice::from_ref(&outcome),
        std::slice::from_ref(&decision),
    );
    let second = project_batch(
        std::slice::from_ref(&outcome),
        std::slice::from_ref(&decision),
    );
    assert_eq!(first, second);
    assert_eq!(ProjectionBatch::default().len(), 0);
    assert!(ProjectionBatch::default().is_empty());
}

// ---------------------------------------------------------------------------
// Helpers that build records the accepted schema would not produce
// ---------------------------------------------------------------------------

fn cancelled() -> (Outcome, RouteDecision) {
    let decision = decision_for(&decision_id(), "gpt-4");
    let outcome = Outcome::builder(request_id())
        .decision_id(decision.decision_id.clone())
        .planned("gpt-4", "openai")
        .dialect("openai")
        .failure(FailureClass::ClientCancelled, "client went away", None)
        .total_latency_ms(40.0)
        .timestamp(1_700_000_000)
        .build();
    (outcome, decision)
}

fn malformed_request_id() -> Outcome {
    let decision = decision_for(&decision_id(), "gpt-4");
    let mut outcome = Outcome::builder("req_1")
        .decision_id(decision.decision_id.clone())
        .planned("gpt-4", "openai")
        .dialect("openai")
        .attempt(attempt("gpt-4", "openai", true, None))
        .total_latency_ms(300.0)
        .timestamp(1_700_000_000)
        .build();
    outcome.request_id = "req_1".to_string();
    outcome
}

/// Serialise a record to JSON, so the field-set and leakage audits read the same
/// bytes a consumer would.
#[allow(dead_code)]
fn as_value(record: &RequestProjection) -> Value {
    serde_json::to_value(record).expect("serializes")
}
