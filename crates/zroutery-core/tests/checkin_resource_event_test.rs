//! A finished check-in, turned into the resource event it actually was.
//!
//! # Why this mapping is its own file
//!
//! `ObservedResourceEvent` and `AccountEventLog` were written, fixture-tested and
//! then held by nothing: no check-in ever constructed an event, so the log was
//! always empty and nothing downstream could be exercised. This is the link that
//! makes them reachable, and it is tested on its own because the rules are where
//! the lies would come from.
//!
//! # The one rule everything else serves
//!
//! **Only a confirmed reward may claim that the balance moved.** Every other
//! outcome has to say something weaker, and each one has a distinct weaker thing to
//! say:
//!
//! | confirmation | effect | what that claims |
//! |---|---|---|
//! | `Confirmed` | `Credited` | a structured surface established this amount |
//! | `AlreadyCompleted` | `NoChange` | the provider established that nothing settled |
//! | `ObservationPending` | `Unobserved` | it may have been granted; nothing says so |
//! | failure, cancellation | *no event* | nothing about the balance is established at all |
//! | not yet ended | *no event* | nothing has happened yet |
//!
//! The last two are different from each other and from `NoChange`, and collapsing
//! any of the three is the defect this file exists to prevent.

#![cfg(feature = "newapi")]

use zroutery_core::account::checkin::as_resource_event;
use zroutery_core::account::resource::{
    ResourceAmount, ResourceEffect, ResourceEventKind, ResourceEventSource,
};
use zroutery_core::account::{
    AccountId, CheckinConfirmation, CheckinPhase, CheckinReport, CheckinReward, RewardSource,
};

/// A report carrying the given confirmation, so the mapping can be exercised
/// without going through a provider.
fn report_with(confirmation: Option<CheckinConfirmation>) -> CheckinReport {
    let mut report = CheckinReport::requested("relay", AccountId("main".into()), 1_000);
    report.phase = CheckinPhase::Confirmed;
    report.confirmation = confirmation;
    report
}

fn confirmed(amount: f64, source: RewardSource) -> CheckinConfirmation {
    CheckinConfirmation::Confirmed {
        reward: CheckinReward {
            amount: ResourceAmount::normalised(amount, "USD"),
            source,
        },
    }
}

#[test]
fn a_confirmed_reward_becomes_a_credited_grant_that_carries_its_source() {
    let event = as_resource_event(&report_with(Some(confirmed(
        25.0,
        RewardSource::ProviderRecord,
    ))))
    .expect("a confirmed check-in establishes something");

    assert_eq!(event.event_kind, ResourceEventKind::SystemGrant);
    assert_eq!(event.provider_id, "relay");
    assert_eq!(event.account_id, AccountId("main".into()));
    assert!(
        event.moved_resource(),
        "only a confirmed reward may claim that the balance moved"
    );
    match &event.resource_effect {
        ResourceEffect::Credited { amount } => {
            assert_eq!(amount.value, 25.0);
            assert_eq!(amount.unit, "USD");
        }
        other => panic!("expected a credit, got {other:?}"),
    }
    // The source travels with the figure. A consumer that cannot tell a structured
    // field from a number recovered out of log text will eventually add them up.
    assert_eq!(event.source, ResourceEventSource::AccountState);
    assert_eq!(
        event.metadata.get("checkin_source").map(String::as_str),
        Some("provider_record")
    );
}

#[test]
fn an_already_completed_check_in_records_that_nothing_moved() {
    // The provider may still report the figure the *earlier* check-in was granted.
    // Recording it here would show one credit twice and would attribute the earlier
    // grant to this attempt — which is the exact fabrication the whole confirmation
    // ladder exists to prevent, arriving through the ledger instead of the panel.
    let event = as_resource_event(&report_with(Some(CheckinConfirmation::AlreadyCompleted {
        reward: Some(CheckinReward {
            amount: ResourceAmount::normalised(25.0, "USD"),
            source: RewardSource::ProviderRecord,
        }),
    })))
    .expect("an already-completed check-in is still worth recording");

    assert_eq!(
        event.resource_effect,
        ResourceEffect::NoChange,
        "the provider established that nothing settled, which is a finding"
    );
    assert!(
        !event.moved_resource(),
        "a second check-in must never claim to have moved the balance"
    );
    assert_eq!(
        event.balance_delta(),
        None,
        "and must contribute nothing to any reconstruction of one"
    );
    // Recorded rather than dropped: "we checked and it was already done" is worth
    // knowing, and is not the same as "we did not look".
    assert!(!event.description.is_empty());
}

#[test]
fn an_unobserved_effect_is_carried_rather_than_guessed() {
    let event = as_resource_event(&report_with(Some(CheckinConfirmation::ObservationPending)))
        .expect("an accepted check-in with nothing observed is still a fact");

    assert_eq!(
        event.resource_effect,
        ResourceEffect::Unobserved,
        "the operation may have been granted; nothing observed says so"
    );
    assert!(!event.moved_resource());
    assert_eq!(event.balance_delta(), None);
}

#[test]
fn an_attempt_that_never_ended_produces_no_event() {
    // Every non-terminal phase, including the two that mean a human is needed. An
    // event for a paused attempt would put a grant in the log for something that has
    // not happened yet, and it would do so at exactly the moment somebody is least
    // likely to question it.
    for phase in [
        CheckinPhase::Requested,
        CheckinPhase::Executing,
        CheckinPhase::ProviderAccepted,
        CheckinPhase::ObservationPending,
        CheckinPhase::BlockedByWaf,
        CheckinPhase::NeedsUserAction,
    ] {
        let mut report = report_with(Some(confirmed(25.0, RewardSource::ProviderRecord)));
        report.phase = phase;
        assert_eq!(
            as_resource_event(&report),
            None,
            "{phase:?} has not established anything yet"
        );
    }
}

#[test]
fn a_failed_attempt_records_nothing_at_all() {
    // Distinct from the already-completed case, and the distinction is the point: a
    // failure says nothing whatever about the balance, while an already-completed
    // check-in says the balance did not move. One is silence and one is a finding.
    for phase in [
        CheckinPhase::Failed,
        CheckinPhase::Cancelled,
        CheckinPhase::NotSupported,
    ] {
        let mut report = report_with(None);
        report.phase = phase;
        assert_eq!(
            as_resource_event(&report),
            None,
            "{phase:?} establishes nothing about the balance"
        );
    }
}

#[test]
fn each_reward_source_maps_to_the_channel_a_reader_can_reason_about() {
    // The two weak sources must not be dressed up as the structured ones.
    for (source, expected, name) in [
        (
            RewardSource::ProviderResponse,
            ResourceEventSource::OperationResponse,
            "provider_response",
        ),
        (
            RewardSource::ProviderRecord,
            ResourceEventSource::AccountState,
            "provider_record",
        ),
        (
            RewardSource::EventContent,
            ResourceEventSource::AccountLog,
            "event_content",
        ),
        (
            RewardSource::BalanceDelta,
            ResourceEventSource::AccountState,
            "balance_delta",
        ),
    ] {
        let event = as_resource_event(&report_with(Some(confirmed(1.0, source))))
            .expect("a confirmed check-in produces an event");
        assert_eq!(event.source, expected, "{name} maps to the wrong channel");
        assert_eq!(
            event.metadata.get("checkin_source").map(String::as_str),
            Some(name),
            "the weaker sources must stay nameable"
        );
    }
}

#[test]
fn the_amount_comes_from_the_structured_surface_and_not_from_the_description() {
    // The description is text the event layer displays. Nothing in this mapping
    // parses it, so a reworded or localised upstream string cannot become a number
    // here — which is the whole reason the amount travels as a field.
    let event = as_resource_event(&report_with(Some(confirmed(
        25.0,
        RewardSource::EventContent,
    ))))
    .expect("even the weakest source carries a figure");
    assert!(
        !event.metadata.contains_key("description"),
        "the description is never smuggled into metadata where a parser might find it"
    );
    assert_eq!(
        event.resource_effect.amount().map(|a| a.value),
        Some(25.0),
        "the adapter parsed it; this layer did not"
    );
    // And a figure recovered from log text is still labelled as such, so a consumer
    // can decide it is not worth as much as a structured one.
    assert_eq!(event.source, ResourceEventSource::AccountLog);
}

#[test]
fn each_outcome_says_a_different_thing() {
    use zroutery_core::account::checkin::as_resource_event;

    // Found by `scripts/mutation_gate.py`, which reported MISSED when the wording of
    // the confirmed description was changed and nothing objected. The distinction
    // between "granted a reward", "nothing moved" and "nothing established it" is
    // the entire content of this mapping, and it was unpinned: a panel could be
    // handed the same sentence for all three and no test would notice.
    //
    // Pinned by asserting that they differ rather than by asserting exact strings,
    // so a rewording does not fail — collapsing three into one does.
    let confirmed = as_resource_event(&report_with(Some(confirmed(
        25.0,
        RewardSource::ProviderRecord,
    ))))
    .expect("a confirmed check-in produces an event");
    let already = as_resource_event(&report_with(Some(CheckinConfirmation::AlreadyCompleted {
        reward: None,
    })))
    .expect("an already-completed check-in produces an event");
    let pending = as_resource_event(&report_with(Some(CheckinConfirmation::ObservationPending)))
        .expect("an accepted check-in produces an event");

    let three = [
        confirmed.description.as_str(),
        already.description.as_str(),
        pending.description.as_str(),
    ];
    for (i, a) in three.iter().enumerate() {
        assert!(!a.is_empty(), "every outcome must say something");
        for (j, b) in three.iter().enumerate() {
            if i == j {
                continue;
            }
            assert!(
                a != b,
                "two outcomes render as the same sentence ({a:?}); the difference \
                 between them is the whole content of the mapping"
            );
        }
    }
}
