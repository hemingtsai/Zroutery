//! Check-in reconciliation: execution, then observation, then a verdict.
//!
//! # What is being pinned
//!
//! §23 and §24 of the design require that a check-in distinguish four outcomes
//! that all look like "it worked" if you only watch the HTTP status:
//!
//! * `Confirmed` — a reward was observed and attributed;
//! * `ProviderAccepted` → `ObservationPending` — the provider accepted it and
//!   nothing observed contradicts that, but no figure was established;
//! * `AlreadyCompleted` — the period's check-in was already done, so no new
//!   reward is expected and its absence is not a missing observation;
//! * `BlockedByWaf` — a challenge intercepted it and a live browser is held open.
//!
//! The failure mode these tests exist to prevent is a report that says
//! `Confirmed` with a `+25.00` because an operation returned HTTP 200. That
//! number would be fabricated, and fabricated in the direction an operator most
//! wants to believe.

#![cfg(feature = "newapi")]

use zroutery_core::account::adapters::newapi::checkin::{
    reconcile, CheckinAttempt, CheckinContext, CheckinObservation, NewApiCheckinRecord,
    NewApiCheckinSnapshot, NewApiCheckinStatus,
};
use zroutery_core::account::{
    AccountId, CheckinConfirmation, CheckinFailure, CheckinPhase, CheckinReport, RewardSource,
};

const QUOTA_PER_UNIT: f64 = 500_000.0;
const DAY: &str = "2026-10-06";
const AT: i64 = 1_760_000_000;

fn account() -> AccountId {
    AccountId("acct-1".into())
}

fn reconcile_with(
    attempt: CheckinAttempt,
    observation: CheckinObservation,
    id: &AccountId,
) -> CheckinReport {
    let ctx = CheckinContext::usd("newapi", id, QUOTA_PER_UNIT);
    reconcile(attempt, observation, &ctx, AT)
}

fn snapshot(
    checked_in_today: Option<bool>,
    records: Vec<NewApiCheckinRecord>,
) -> NewApiCheckinSnapshot {
    NewApiCheckinSnapshot {
        enabled: Some(true),
        checked_in_today,
        total_checkins: Some(3),
        total_quota: Some(1_500_000),
        records,
        read_at: AT,
    }
}

fn record(date: &str, awarded: Option<i64>) -> NewApiCheckinRecord {
    NewApiCheckinRecord {
        checkin_date: Some(date.to_string()),
        quota_awarded: awarded,
    }
}

fn system_event(content: &str, at: i64) -> zroutery_core::account::ObservedResourceEvent {
    let json = serde_json::json!({
        "id": 1,
        "created_at": at,
        "type": 4,
        "content": content,
        "quota": 0,
    })
    .to_string();
    let id = AccountId("acct-1".into());
    let item: zroutery_core::account::adapters::newapi::NewApiLogItem =
        serde_json::from_str(&json).expect("system row decodes");
    let ctx = zroutery_core::account::adapters::newapi::LogContext::usd(&id, QUOTA_PER_UNIT);
    zroutery_core::account::adapters::newapi::interpret_log(&item, &ctx)
        .expect("a type=4 row is an event")
}

// ── §23: the successful reconciliation ─────────────────────────────────────

#[test]
fn an_accepted_checkin_with_a_reported_award_is_confirmed() {
    // The operation's own `quota_awarded`. The highest-trust source there is:
    // the grant itself rather than a report about it.
    let id = account();
    let observation = CheckinObservation {
        after: Some(snapshot(Some(true), vec![record(DAY, Some(12_500_000))])),
        before: Some(snapshot(Some(false), vec![])),
        quota_after: Some(12_500_000),
        quota_before: Some(0),
        system_events: vec![system_event("用户签到，获得额度 ＄25.000000 额度", AT + 1)],
        observed_at: AT + 2,
    };
    let report = reconcile_with(
        CheckinAttempt::Accepted {
            quota_awarded: Some(12_500_000),
            checkin_date: Some(DAY.to_string()),
        },
        observation,
        &id,
    );

    assert_eq!(report.phase, CheckinPhase::Confirmed);
    assert_eq!(report.completed_at, Some(AT + 2));
    let reward = report
        .reward()
        .expect("a confirmed report carries a reward");
    assert_eq!(reward.source, RewardSource::ProviderResponse);
    assert_eq!(reward.amount.provider_value, Some(12_500_000.0));
    assert!((reward.amount.value - 25.0).abs() < 1e-9);
    assert_eq!(reward.amount.unit, "USD");

    // The observation layer ran too: state refreshed and the system event found.
    assert_eq!(report.observed_events.len(), 1);
    assert!(report.reward_event.is_some());
    assert_eq!(report.failure, None);
}

#[test]
fn a_browser_checkin_with_no_response_figure_is_confirmed_from_the_record() {
    // The product path. A browser performed the check-in, so this adapter never
    // saw a response figure — but the panel wrote a `checkins` row, and that
    // structured record is enough.
    let id = account();
    let observation = CheckinObservation {
        after: Some(snapshot(Some(true), vec![record(DAY, Some(4_250_000))])),
        before: Some(snapshot(Some(false), vec![])),
        quota_after: Some(4_250_000),
        quota_before: Some(0),
        system_events: Vec::new(),
        observed_at: AT + 5,
    };
    let report = reconcile_with(
        CheckinAttempt::Accepted {
            quota_awarded: None,
            checkin_date: None,
        },
        observation,
        &id,
    );

    assert_eq!(report.phase, CheckinPhase::Confirmed);
    let reward = report.reward().expect("confirmed");
    assert_eq!(
        reward.source,
        RewardSource::ProviderRecord,
        "confirmed from the panel's own check-in record, not from the operation"
    );
    assert!((reward.amount.value - 8.5).abs() < 1e-9);
}

#[test]
fn an_accepted_checkin_with_no_observation_at_all_is_pending_not_confirmed() {
    // §23's explicit requirement: a provider that says success but offers no
    // observable surface gets `ObservationPending`, never a fabricated
    // `Confirmed`.
    let id = account();
    let observation = CheckinObservation {
        after: None,
        before: None,
        quota_after: None,
        quota_before: None,
        system_events: Vec::new(),
        observed_at: AT + 3,
    };
    let report = reconcile_with(
        CheckinAttempt::Accepted {
            quota_awarded: None,
            checkin_date: None,
        },
        observation,
        &id,
    );

    assert_eq!(report.phase, CheckinPhase::ObservationPending);
    assert!(!report.phase.is_confirmed());
    assert_eq!(report.reward(), None);
    assert_eq!(
        report.confirmation,
        Some(CheckinConfirmation::ObservationPending)
    );
    assert!(
        !report.phase.is_terminal(),
        "an observation may still arrive"
    );
    assert!(report.failure.is_none(), "pending is not a failure");
}

#[test]
fn a_system_event_alone_does_not_confirm_the_reward() {
    // The log row is the weakest of three sources. Its own `quota` is zero, its
    // text is localised, and confirming a dollar figure from it would be inventing
    // one. It is kept as corroboration and says so.
    let id = account();
    let observation = CheckinObservation {
        after: None,
        before: None,
        quota_after: None,
        quota_before: None,
        system_events: vec![system_event(
            "每日签到成功，增加额度 ＄25.000000 额度",
            AT + 1,
        )],
        observed_at: AT + 2,
    };
    let report = reconcile_with(
        CheckinAttempt::Accepted {
            quota_awarded: None,
            checkin_date: None,
        },
        observation,
        &id,
    );

    assert_eq!(report.phase, CheckinPhase::ObservationPending);
    assert_eq!(report.reward(), None, "no amount was established");
    assert!(
        report.reward_event.is_some(),
        "but the event is kept as evidence"
    );
    let event = report.reward_event.expect("event");
    // The parsed number, not the rendered text: `＄25.000000` is recovered as 25.
    let display: f64 = event
        .metadata_value("newapi.system.display_amount")
        .expect("a rendered amount was recorded")
        .parse()
        .unwrap();
    assert!((display - 25.0).abs() < 1e-9);
    assert_eq!(
        event.metadata_value("newapi.system.display_symbol"),
        Some("＄")
    );
}

#[test]
fn a_growing_balance_alone_does_not_confirm_the_reward() {
    // Concurrent spend lands in the same difference. Confirming here would credit
    // a check-in with whatever else happened during the window.
    let id = account();
    let observation = CheckinObservation {
        after: None,
        before: None,
        quota_after: Some(12_500_000),
        quota_before: Some(9_000_000),
        system_events: Vec::new(),
        observed_at: AT + 2,
    };
    assert_eq!(observation.balance_delta_credits(), Some(3_500_000));
    let report = reconcile_with(
        CheckinAttempt::Accepted {
            quota_awarded: None,
            checkin_date: None,
        },
        observation,
        &id,
    );
    assert_eq!(report.phase, CheckinPhase::ObservationPending);
    assert_eq!(report.reward(), None);
}

#[test]
fn a_checkin_todays_status_flipping_is_a_structured_observation() {
    // No amount, but the transition itself is real: before says not today, after
    // says today. That is more than nothing and less than a figure.
    let id = account();
    let observation = CheckinObservation {
        after: Some(snapshot(Some(true), vec![record(DAY, None)])),
        before: Some(snapshot(Some(false), vec![])),
        quota_after: None,
        quota_before: None,
        system_events: Vec::new(),
        observed_at: AT + 2,
    };
    let report = reconcile_with(
        CheckinAttempt::Accepted {
            quota_awarded: None,
            checkin_date: None,
        },
        observation,
        &id,
    );
    assert_eq!(report.phase, CheckinPhase::ObservationPending);
    assert_eq!(
        report.confirmation,
        Some(CheckinConfirmation::ObservationPending)
    );
}

// ── §24: already checked in ────────────────────────────────────────────────

#[test]
fn an_already_completed_checkin_is_a_success_not_a_failure() {
    let id = account();
    let observation = CheckinObservation {
        after: Some(snapshot(Some(true), vec![record(DAY, Some(12_500_000))])),
        before: Some(snapshot(Some(true), vec![record(DAY, Some(12_500_000))])),
        quota_after: Some(12_500_000),
        quota_before: Some(12_500_000),
        // No new system event, and none is expected: nothing was granted.
        system_events: Vec::new(),
        observed_at: AT + 2,
    };
    let report = reconcile_with(
        CheckinAttempt::AlreadyCompleted {
            reward_quota: Some(12_500_000),
        },
        observation,
        &id,
    );

    assert_eq!(report.phase, CheckinPhase::AlreadyCompleted);
    assert!(report.phase.is_terminal());
    assert_eq!(report.failure, None, "a repeat check-in is not an error");
    assert!(
        !report.phase.is_confirmed(),
        "this operation caused no credit, and must not render as one"
    );
    assert!(report.observed_events.is_empty());
}

#[test]
fn an_already_completed_checkin_with_no_record_reports_no_reward() {
    // The absence of a reward here is expected and says nothing about whether the
    // earlier one landed.
    let id = account();
    let report = reconcile_with(
        CheckinAttempt::AlreadyCompleted { reward_quota: None },
        CheckinObservation {
            observed_at: AT,
            ..Default::default()
        },
        &id,
    );
    assert_eq!(report.phase, CheckinPhase::AlreadyCompleted);
    assert_eq!(report.reward(), None);
    assert_eq!(
        report.confirmation,
        Some(CheckinConfirmation::AlreadyCompleted { reward: None })
    );
}

// ── §25: failure classification ────────────────────────────────────────────

#[test]
fn a_waf_block_is_a_paused_operation_not_a_failure() {
    let id = account();
    let report = reconcile_with(
        CheckinAttempt::WafBlocked,
        CheckinObservation {
            observed_at: AT,
            ..Default::default()
        },
        &id,
    );
    assert_eq!(report.phase, CheckinPhase::BlockedByWaf);
    assert!(report.phase.is_waiting_for_user());
    assert!(!report.phase.is_terminal());
    assert_eq!(report.failure, Some(CheckinFailure::WafBlocked));
    assert!(!report.failure.expect("reason").is_retryable_without_user());
}

#[test]
fn an_expired_credential_is_told_apart_from_a_transport_failure() {
    // §25: these need different actions, so they cannot both be "failed".
    let id = account();
    let expired = reconcile_with(
        CheckinAttempt::AuthenticationExpired,
        CheckinObservation {
            observed_at: AT,
            ..Default::default()
        },
        &id,
    );
    assert_eq!(expired.phase, CheckinPhase::Failed);
    assert_eq!(expired.failure, Some(CheckinFailure::AuthenticationExpired));
    assert!(expired.failure.expect("reason").needs_user());

    let network = reconcile_with(
        CheckinAttempt::NetworkFailure {
            detail: "connection reset".into(),
        },
        CheckinObservation {
            observed_at: AT,
            ..Default::default()
        },
        &id,
    );
    assert_eq!(network.phase, CheckinPhase::Failed);
    assert_eq!(network.failure, Some(CheckinFailure::NetworkFailure));
    assert!(
        network.failure.expect("reason").is_retryable_without_user(),
        "a transport blip is the one failure a scheduler may retry"
    );
}

#[test]
fn every_failure_class_is_distinguishable() {
    // §25 asks for these categories; a report that collapses two of them into
    // one string is what the classification exists to prevent.
    let attempts = [
        CheckinAttempt::NotSupported,
        CheckinAttempt::Disabled,
        CheckinAttempt::AuthenticationExpired,
        CheckinAttempt::WafBlocked,
        CheckinAttempt::NetworkFailure { detail: "x".into() },
        CheckinAttempt::Rejected {
            code: Some("c".into()),
            message: "m".into(),
        },
        CheckinAttempt::Cancelled,
    ];
    let id = account();
    let mut reasons = Vec::new();
    for attempt in attempts {
        let report = reconcile_with(
            attempt,
            CheckinObservation {
                observed_at: AT,
                ..Default::default()
            },
            &id,
        );
        assert!(matches!(
            report.phase,
            CheckinPhase::Failed
                | CheckinPhase::NotSupported
                | CheckinPhase::Cancelled
                | CheckinPhase::BlockedByWaf
        ));
        if let Some(failure) = report.failure {
            reasons.push(format!("{failure:?}"));
        }
    }
    let unique: std::collections::BTreeSet<_> = reasons.iter().collect();
    assert_eq!(
        unique.len(),
        reasons.len(),
        "each failure class must be reported distinctly: {reasons:?}"
    );
}

#[test]
fn a_not_supported_instance_is_not_reported_as_a_failure() {
    let id = account();
    let report = reconcile_with(
        CheckinAttempt::NotSupported,
        CheckinObservation {
            observed_at: AT,
            ..Default::default()
        },
        &id,
    );
    assert_eq!(report.phase, CheckinPhase::NotSupported);
    assert_eq!(report.failure, None);
}

#[test]
fn a_disabled_instance_says_why_it_did_nothing() {
    // `NotSupported` with no reason and `Disabled` with one are different answers,
    // and a scheduler needs to tell them apart to know whether to stop trying.
    let id = account();
    let report = reconcile_with(
        CheckinAttempt::Disabled,
        CheckinObservation {
            observed_at: AT,
            ..Default::default()
        },
        &id,
    );
    assert_eq!(report.phase, CheckinPhase::NotSupported);
    assert_eq!(report.failure, Some(CheckinFailure::Disabled));
}

// ── Snapshot decoding ──────────────────────────────────────────────────────

#[test]
fn a_status_payload_decodes_into_a_snapshot() {
    // Transcribed from `GetCheckinStatus` + `model.GetUserCheckinStats`.
    let status: NewApiCheckinStatus = serde_json::from_str(
        r#"{
            "enabled": true,
            "min_quota": 100000,
            "max_quota": 20000000,
            "stats": {
                "total_quota": 45000000,
                "total_checkins": 12,
                "checkin_count": 3,
                "checked_in_today": true,
                "records": [
                    { "checkin_date": "2026-10-06", "quota_awarded": 12500000 }
                ]
            }
        }"#,
    )
    .expect("the status payload decodes");

    let snapshot = NewApiCheckinSnapshot::from_status(&status, AT).expect("stats were present");
    assert_eq!(snapshot.checked_in_today, Some(true));
    assert_eq!(snapshot.total_checkins, Some(12));
    assert_eq!(snapshot.records.len(), 1);
    assert_eq!(
        snapshot
            .record_for("2026-10-06")
            .and_then(|r| r.quota_awarded),
        Some(12_500_000)
    );
    assert_eq!(snapshot.record_for("2026-10-05"), None);
}

#[test]
fn a_status_without_stats_is_not_a_snapshot_of_zero_checkins() {
    // "The instance offers no statistics" and "this account has never checked in"
    // must not decode to the same thing.
    let status: NewApiCheckinStatus =
        serde_json::from_str(r#"{ "enabled": false }"#).expect("decodes");
    assert!(
        NewApiCheckinSnapshot::from_status(&status, AT).is_none(),
        "an absent `stats` is absent data, not a measurement of zero"
    );
}
