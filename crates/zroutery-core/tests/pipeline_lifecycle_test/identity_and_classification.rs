use std::sync::Arc;

use zroutery_core::budget::{Budget, BudgetPeriod, BudgetScope};
use zroutery_core::config::{MemorySecretStore, ModelTier};
use zroutery_core::failure::FailureClass;
use zroutery_core::outcome::FinalStatus;

use super::support::*;

// ------------------------------------------------------------------ gates 3, 5, 6

/// Exactly one terminal transition per request, across every kind of ending.
///
/// A served buffer, a served stream, a stream that fails at the handshake and a
/// stream the client abandons in one server: four requests, four activity
/// records, four outcomes correlated by request id, and the spend booked once
/// each.
#[tokio::test]
async fn every_request_reaches_exactly_one_terminal_transition() {
    let h = Harness::new().await;

    // 1. a served buffered request
    assert_eq!(h.ask("beta-good-model").await.status(), 200);
    // 2. a served stream, drained to its end
    let served = h.ask_streaming("beta-good-model").await;
    let _ = served.text().await.unwrap();
    // 3. a stream whose handshake fails
    assert_eq!(h.ask_streaming("beta-broken-model").await.status(), 500);
    // 4. a stream the client abandons
    let mut dropped = h.ask_streaming("beta-hold-model").await;
    let _ = dropped.chunk().await;
    drop(dropped);

    wait_for_outcomes(&h, 4).await;

    let records = h.records();
    let outcomes = h.outcomes();
    assert_eq!(records.len(), 4, "one activity record per request");
    assert_eq!(outcomes.len(), 4, "one outcome per request");

    // Records are newest first and so are outcomes, so the two line up index
    // for index: every recorded request has exactly one outcome.
    let record_ids: Vec<&str> = records.iter().map(|record| record.id.as_str()).collect();
    let outcome_ids: Vec<&str> = outcomes
        .iter()
        .map(|outcome| outcome.request_id.as_str())
        .collect();
    assert_eq!(record_ids, outcome_ids, "one outcome per recorded request");
    let mut unique = outcome_ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 4, "no request was recorded twice");

    for outcome in &outcomes {
        assert!(
            outcome.validate().is_ok(),
            "every constructed outcome satisfies the schema: {:?}",
            outcome.validate()
        );
    }

    // Two succeeded, one failed at the handshake, one was abandoned.
    let mut statuses: Vec<FinalStatus> = outcomes.iter().map(|o| o.final_status).collect();
    statuses.sort_by_key(|status| format!("{status:?}"));
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == FinalStatus::Success)
            .count(),
        2
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == FinalStatus::Failed)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == FinalStatus::Interrupted)
            .count(),
        1
    );
    assert!(outcomes
        .iter()
        .all(|outcome| !outcome.request_id.is_empty()));

    // The ledger and the records agree: spend was booked once per request.
    let expected: f64 = records
        .iter()
        .filter_map(|record| record.cost.as_ref())
        .map(|cost| cost.amount)
        .sum();
    assert!(
        (h.spent_today() - expected).abs() < 1e-12,
        "the ledger and the records agree: {} vs {expected}",
        h.spent_today()
    );

    h.shutdown().await;
}

/// The served identity is the identity that actually delivered the answer, and
/// the planned identity is the router's own choice — never the same field.
#[tokio::test]
async fn planned_last_attempted_and_served_are_correlated() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // The preferred standard candidate always fails, so the planned identity
    // and the identity that served the answer differ.
    cfg.models[0].upstream_model = "broken-model".into();
    cfg.models[0].priority = 0;
    let h = Harness::start(cfg, mock).await;

    assert_eq!(h.ask("standard-class").await.status(), 200);
    assert_eq!(h.mock.count(), 2, "the failing candidate was tried first");

    let outcome = h.outcome();
    let planned = outcome
        .planned_identity()
        .expect("a policy-routed request has a planned identity");
    let last = outcome
        .last_attempted_identity()
        .expect("the request made an attempt");
    let served = outcome.served_identity().expect("the request was served");

    // Planned comes from the router's decision, not from what happened later.
    assert_eq!(planned.model(), "alpha-broken-model");
    assert_eq!(planned.provider(), "alpha");
    assert_eq!(last.model(), "beta-good-model");
    assert_eq!(served, last, "the last attempt is the one that served");
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert_eq!(outcome.attempts.len(), 2);
    assert_eq!(outcome.fallback_count, 1);
    assert_eq!(outcome.initial_model, planned.model());
    assert_eq!(outcome.final_model, served.model());
    assert!(outcome.is_terminal_success());
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    // The failed attempt carries the canonical class, not a re-derived one.
    let first = &outcome.attempts[0];
    assert!(!first.success);
    assert_eq!(first.failure_class, Some(FailureClass::Unknown));
    assert_eq!(first.http_status, Some(500));
    assert!(!first.rectified);
    assert!(outcome.attempts[1].is_terminal_success());

    // The outcome and the record correlate on the decision the router made.
    let record = h.record();
    let decision = record
        .routing_decision
        .as_ref()
        .expect("a policy-routed record carries its decision");
    assert_eq!(
        outcome.decision_id.as_deref(),
        Some(decision.decision_id.as_str())
    );
    assert_eq!(record.attempts, 2);

    h.shutdown().await;
}

/// A request that never reached a candidate is still a request: one record, one
/// outcome, and no provider health touched at all.
#[tokio::test]
async fn a_budget_denial_is_a_terminal_outcome_with_no_candidate() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.budgets = vec![Budget::new(BudgetScope::Global, BudgetPeriod::Day, "USD", 0.0).rejecting()];
    let h = Harness::start(cfg, mock).await;

    let response = h.ask("alpha-good-model").await;
    assert_eq!(response.status(), 402, "the budget stopped it");

    let record = h.record();
    assert!(!record.ok);
    assert_eq!(record.status, 402);
    assert_eq!(record.attempts, 0);

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(outcome.failure_class(), Some(FailureClass::OverBudget));
    assert!(outcome.attempts.is_empty());
    assert!(outcome.planned_identity().is_none(), "nothing was planned");
    assert!(outcome.last_attempted_identity().is_none());
    assert!(outcome.served_identity().is_none());
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());
    assert_eq!(h.mock.count(), 0, "no provider was contacted");
    assert!(
        h.state.router().health_snapshot().is_empty(),
        "a local refusal cannot poison provider health"
    );

    h.shutdown().await;
}

// ------------------------------------------------------------------ gate 1 (last attempted)

/// A candidate the loop could not even prepare is still the final attempt it
/// made: last-attempted means the last thing the request tried, not the last
/// thing that reached a provider. A missing key is a local problem, so it is
/// recorded as a classified fact without becoming a health failure.
#[tokio::test]
async fn an_unusable_candidate_is_the_last_attempt_without_poisoning_health() {
    let (addr, mock) = start_mock().await;
    let cfg = config_for(addr);
    // `provider:alpha` is deliberately absent from the secret store.
    let h = Harness::start_with_secrets(
        cfg,
        mock,
        Arc::new(MemorySecretStore::new().with("provider:beta", "sk-beta")),
    )
    .await;

    // A direct resolution: the only candidate cannot be prepared, so the
    // request ends on that candidate.
    assert_eq!(h.ask("alpha-good-model").await.status(), 412);
    assert_eq!(h.mock.count(), 0, "nothing was sent");

    let record = h.record();
    assert!(!record.ok);
    assert_eq!(record.status, 412);
    assert_eq!(record.attempts, 1);

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(outcome.attempts.len(), 1);
    let last = outcome
        .last_attempted_identity()
        .expect("the loop tried a candidate");
    assert_eq!(last.model(), "alpha-good-model");
    assert_eq!(last.provider(), "alpha");
    assert_eq!(outcome.planned_identity(), Some(last.clone()));
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::MissingApiKey),
        "the unusable candidate is recorded with the canonical class"
    );
    assert_eq!(outcome.failure_class(), Some(FailureClass::MissingApiKey));
    assert!(outcome.served_identity().is_none());
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    assert!(
        h.state.router().health_snapshot().is_empty(),
        "a missing key is not provider health"
    );
    let breakdown = h
        .state
        .router()
        .stats_store
        .get("alpha-good-model", "alpha");
    assert_eq!(
        breakdown.failures.count(FailureClass::MissingApiKey),
        1,
        "the class is recorded exactly once"
    );

    h.shutdown().await;
}

// ------------------------------------------------------------------ gate 2

/// One classified failure path: the pipeline asks the canonical classifier and
/// the accepted router adapters, and never classifies a status or a message of
/// its own.
#[test]
fn the_pipeline_has_no_second_failure_classifier() {
    // `pipeline/mod.rs` is now a directory: the orchestration and its children. A
    // fence reading only the barrel would stop seeing a write the day it moved,
    // which is indistinguishable from the write not existing.
    const PIPELINE_SOURCES: &[(&str, &str)] = &[
        ("mod.rs", include_str!("../../src/server/pipeline/mod.rs")),
        (
            "codex_auto.rs",
            include_str!("../../src/server/pipeline/codex_auto.rs"),
        ),
        ("sse.rs", include_str!("../../src/server/pipeline/sse.rs")),
        (
            "budget.rs",
            include_str!("../../src/server/pipeline/budget.rs"),
        ),
        (
            "client.rs",
            include_str!("../../src/server/pipeline/client.rs"),
        ),
        (
            "lifecycle.rs",
            include_str!("../../src/server/pipeline/lifecycle.rs"),
        ),
        (
            "ml_rank.rs",
            include_str!("../../src/server/pipeline/ml_rank.rs"),
        ),
        (
            "repair.rs",
            include_str!("../../src/server/pipeline/repair.rs"),
        ),
        (
            "response.rs",
            include_str!("../../src/server/pipeline/response.rs"),
        ),
        (
            "credentials.rs",
            include_str!("../../src/server/pipeline/credentials.rs"),
        ),
        (
            "auxiliary.rs",
            include_str!("../../src/server/pipeline/auxiliary.rs"),
        ),
    ];
    let source = &PIPELINE_SOURCES
        .iter()
        .map(|(_name, src)| *src)
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in [
        "from_status_with_body",
        "FailureClass::from_status",
        "ClassifiedFailure::from_status",
        "from_error_message",
        ".is_retryable()",
        "report_failure(",
    ] {
        assert!(
            !source.contains(forbidden),
            "the pipeline must not classify failures itself: {forbidden}"
        );
    }
    // The canonical entry point and the router adapters are what it does use.
    assert!(source.contains(".classified()"));
    assert!(source.contains("record_candidate_failure("));
    assert!(source.contains("should_fallback_candidate("));
    assert!(
        !source.contains("Some(class)"),
        "the pipeline never assembles a class of its own to hand the router"
    );

    // Gate 6, structurally: the activity record and the charge are written in
    // exactly one place, so no terminal path can write either of them twice.
    assert_eq!(
        source.matches(".settle_request(&mut fact)").count(),
        1,
        "the activity record and budget are committed once by the terminal settlement owner"
    );
    assert_eq!(
        source.matches(".charge(").count(),
        0,
        "the pipeline cannot add a second direct charge beside durable attempt settlement"
    );
}

/// A rate limit is classified once, degrades observations, and must not open
/// the circuit: the impact table's answer, reached through one path.
#[tokio::test]
async fn a_rate_limit_is_classified_once_without_opening_the_circuit() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models = vec![model("alpha", "limited-model", 0, ModelTier::Standard)];
    let h = Harness::start(cfg, mock).await;

    assert_eq!(h.ask("alpha-limited-model").await.status(), 429);

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(outcome.failure_class(), Some(FailureClass::RateLimit));
    assert_eq!(outcome.attempts.len(), 1);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::RateLimit)
    );
    assert!(outcome.served_identity().is_none());
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    let breakdown = h
        .state
        .router()
        .stats_store
        .get("alpha-limited-model", "alpha");
    assert_eq!(breakdown.failures.count(FailureClass::RateLimit), 1);
    assert_eq!(
        breakdown.failures.count(FailureClass::ProviderUnavailable),
        0,
        "the class is not re-derived from the status"
    );
    let health = h.state.router().health_snapshot();
    assert_eq!(health[0].total_failure, 1, "counted once");
    assert!(
        !h.state.router().is_cooling("alpha-limited-model"),
        "a quota is not a dead provider"
    );

    h.shutdown().await;
}
