//! A bounded, in-memory log of observed resource events.
//!
//! # Why the core keeps an event log at all
//!
//! The provider is the authority on an account's balance, and this log is not a
//! second copy of it. It exists for the question a balance cannot answer: *why
//! is this the number it is*. A panel showing `12.40` next to "check-in granted
//! 25.00, consumed 12.60" is answering a different and more useful question from
//! one showing `12.40` alone.
//!
//! # Bounded, in memory, and not durable
//!
//! Three properties follow from the same decision. It is a ring buffer, because
//! an event log that grows with account lifetime is a memory leak with a
//! plausible-looking name. It is in memory, because these are observations and
//! the provider holds the durable record — persisting a copy would create a
//! second source of truth that can disagree with the first. And it is capped on
//! **total** events rather than per account, so one chatty account cannot evict
//! every other account's history to make room for its own.
//!
//! # Ordering and duplicates
//!
//! Events are stored newest-first on insertion, and [`AccountEventLog::record`]
//! rejects an event whose id was already recorded, so re-reading the same log
//! page after a check-in cannot turn one provider event into several. Dedup is
//! by provider event id and deliberately does not fall back to time-and-content:
//! two genuinely separate rewards of the same amount are two events, and
//! collapsing them would understate the account's funding.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::sync;

use super::resource::ObservedResourceEvent;
use super::types::AccountId;

/// How many events the log holds across every account.
///
/// Small on purpose. This backs a "what happened recently" panel, not an audit
/// trail; the provider's own log endpoint is the archive, and this is the
/// working set.
pub const DEFAULT_EVENT_CAPACITY: usize = 512;

/// A bounded ring of the most recently observed resource events.
///
/// The key is `(provider_id, account_id)` because that is the pair the account
/// store is keyed on, and a log keyed any other way could not be joined to a
/// balance.
#[derive(Debug)]
pub struct AccountEventLog {
    capacity: usize,
    /// Newest first. A single ring for all accounts, so the bound is global.
    events: Mutex<VecDeque<ObservedResourceEvent>>,
}

impl AccountEventLog {
    /// A log holding at most `capacity` events in total.
    ///
    /// A capacity of zero is raised to one rather than accepted: a log that can
    /// hold nothing would accept an event and then silently drop it, which is
    /// the same behaviour as a broken log while looking configured.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            events: Mutex::new(VecDeque::new()),
        }
    }

    /// Record one event, unless the same provider event id was already recorded
    /// **for the same account**.
    ///
    /// Returns `true` when the event was added. `false` means either the log is
    /// full or this provider event id was already recorded; the two are not
    /// distinguished, because neither leaves a caller with anything to do.
    ///
    /// The account is part of the dedup key and that is load-bearing. Ids are
    /// per-user upstream, so the same id can legitimately appear under two
    /// accounts — and a dedup that ignored the account would silently drop the
    /// second account's event, which is the same class of cross-account bleed as
    /// a shared browser profile and would show up as a missing balance change on
    /// one account and a duplicate on the other.
    pub fn record(&self, event: ObservedResourceEvent) -> bool {
        let mut guard = sync::lock(&self.events);
        if let Some(id) = event.event_id.as_deref() {
            let key = (event.provider_id.as_str(), event.account_id.0.as_str(), id);
            if guard.iter().any(|existing| {
                existing.event_id.as_deref() == Some(id)
                    && existing.provider_id == key.0
                    && existing.account_id.0 == key.1
            }) {
                return false;
            }
        }
        guard.push_front(event);
        while guard.len() > self.capacity {
            guard.pop_back();
        }
        true
    }

    /// Record several events, returning how many were new.
    pub fn record_all(&self, events: impl IntoIterator<Item = ObservedResourceEvent>) -> usize {
        events
            .into_iter()
            .filter(|event| self.record(event.clone()))
            .count()
    }

    /// The events for one account, newest first.
    pub fn for_account(
        &self,
        provider_id: &str,
        account_id: &AccountId,
    ) -> Vec<ObservedResourceEvent> {
        self.guard()
            .iter()
            .filter(|event| event.provider_id == provider_id && event.account_id == *account_id)
            .cloned()
            .collect()
    }

    /// The newest events for one account that match a predicate.
    ///
    /// Takes the predicate rather than a kind so a caller can also filter on
    /// time proximity or provider metadata, which a check-in confirmation needs
    /// to do and a kind-only filter could not express.
    pub fn for_account_matching(
        &self,
        provider_id: &str,
        account_id: &AccountId,
        predicate: impl Fn(&ObservedResourceEvent) -> bool,
    ) -> Vec<ObservedResourceEvent> {
        self.guard()
            .iter()
            .filter(|event| {
                event.provider_id == provider_id
                    && event.account_id == *account_id
                    && predicate(event)
            })
            .cloned()
            .collect()
    }

    /// Events newer than `since` (unix seconds), newest first.
    pub fn since(
        &self,
        provider_id: &str,
        account_id: &AccountId,
        since: i64,
    ) -> Vec<ObservedResourceEvent> {
        self.for_account_matching(provider_id, account_id, |event| event.observed_at >= since)
    }

    /// How many events are held, across all accounts.
    pub fn len(&self) -> usize {
        self.guard().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop everything.
    ///
    /// Called when the configuration changes underneath the log: the events
    /// described accounts that may no longer be declared, and keeping them would
    /// leave a panel able to render history for an account the user deleted.
    pub fn clear(&self) {
        self.guard().clear();
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, VecDeque<ObservedResourceEvent>> {
        sync::lock(&self.events)
    }
}

impl Default for AccountEventLog {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::resource::{
        ResourceAmount, ResourceEffect, ResourceEventKind, ResourceEventSource,
    };
    use crate::account::types::AccountId;
    use std::collections::BTreeMap;

    fn event(provider: &str, account: &str, id: Option<&str>) -> ObservedResourceEvent {
        ObservedResourceEvent {
            event_id: id.map(str::to_string),
            provider_id: provider.into(),
            account_id: AccountId(account.into()),
            observed_at: 1_700_000_000,
            event_kind: ResourceEventKind::Consumption,
            raw_type: Some(2),
            description: String::new(),
            resource_effect: ResourceEffect::Debited {
                amount: ResourceAmount::normalised(1.0, "USD"),
            },
            source: ResourceEventSource::AccountLog,
            request_id: None,
            channel_id: Some(54),
            model: None,
            accounting: None,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn events_come_back_newest_first() {
        let log = AccountEventLog::new(8);
        log.record(event("p", "a", Some("1")));
        log.record(event("p", "a", Some("2")));
        let got = log.for_account("p", &AccountId("a".into()));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].event_id.as_deref(), Some("2"));
        assert_eq!(got[1].event_id.as_deref(), Some("1"));
    }

    #[test]
    fn a_repeated_provider_event_id_is_not_recorded_twice() {
        let log = AccountEventLog::new(8);
        assert!(log.record(event("p", "a", Some("1"))));
        assert!(
            !log.record(event("p", "a", Some("1"))),
            "re-reading the same log page must not multiply one provider event"
        );
        assert_eq!(log.for_account("p", &AccountId("a".into())).len(), 1);
    }

    #[test]
    fn two_identical_rewards_stay_two_events() {
        // Dedup is by id, never by content: two separate rewards of the same
        // amount are two events, and collapsing them understates the funding.
        let log = AccountEventLog::new(8);
        assert!(log.record(event("p", "a", Some("1"))));
        assert!(log.record(event("p", "a", Some("2"))));
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn an_event_without_an_id_is_always_accepted() {
        // A provider that reports no ids cannot be deduped against. Rejecting
        // such events would drop real history, so they are accepted and the
        // caller is left to avoid re-reading.
        let log = AccountEventLog::new(8);
        assert!(log.record(event("p", "a", None)));
        assert!(log.record(event("p", "a", None)));
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn the_bound_is_global_so_one_account_cannot_evict_another() {
        let log = AccountEventLog::new(3);
        for i in 0..5 {
            log.record(event("chatty", "a", Some(&i.to_string())));
        }
        log.record(event("quiet", "b", Some("x")));
        assert_eq!(log.len(), 3, "the ring is bounded, not per-account");
        assert_eq!(log.for_account("chatty", &AccountId("a".into())).len(), 2);
        assert_eq!(log.for_account("quiet", &AccountId("b".into())).len(), 1);
    }

    #[test]
    fn accounts_are_kept_apart_by_the_full_key() {
        let log = AccountEventLog::new(8);
        log.record(event("p", "a", Some("1")));
        log.record(event("p", "b", Some("1")));
        log.record(event("q", "a", Some("1")));
        assert_eq!(log.len(), 3, "the same id under three keys is three events");
        assert_eq!(log.for_account("p", &AccountId("a".into())).len(), 1);
        assert_eq!(log.for_account("q", &AccountId("a".into())).len(), 1);
    }

    #[test]
    fn since_filters_on_the_providers_own_timestamp() {
        let log = AccountEventLog::new(8);
        let mut old = event("p", "a", Some("old"));
        old.observed_at = 100;
        let mut new = event("p", "a", Some("new"));
        new.observed_at = 200;
        log.record(old);
        log.record(new);
        let got = log.since("p", &AccountId("a".into()), 150);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].event_id.as_deref(), Some("new"));
    }

    #[test]
    fn a_predicate_can_filter_beyond_kind() {
        let log = AccountEventLog::new(8);
        let mut grant = event("p", "a", Some("g"));
        grant.event_kind = ResourceEventKind::SystemGrant;
        grant.resource_effect = ResourceEffect::Unobserved;
        log.record(grant);
        log.record(event("p", "a", Some("c")));
        let got = log.for_account_matching("p", &AccountId("a".into()), |e| {
            e.event_kind == ResourceEventKind::SystemGrant
        });
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].event_id.as_deref(), Some("g"));
    }

    #[test]
    fn a_zero_capacity_log_still_holds_one_event() {
        // Otherwise it accepts an event and silently drops it: configured and
        // broken look identical.
        let log = AccountEventLog::new(0);
        assert!(log.record(event("p", "a", Some("1"))));
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn clear_drops_everything() {
        let log = AccountEventLog::new(8);
        log.record(event("p", "a", Some("1")));
        assert!(!log.is_empty());
        log.clear();
        assert!(log.is_empty());
    }

    #[test]
    fn record_all_counts_only_what_was_new() {
        let log = AccountEventLog::new(8);
        let added = log.record_all([
            event("p", "a", Some("1")),
            event("p", "a", Some("2")),
            event("p", "a", Some("1")),
        ]);
        assert_eq!(added, 2);
        assert_eq!(log.len(), 2);
    }
}
