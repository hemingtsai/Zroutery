//! Reconciling declared accounts into the store: the execution half of `ACCOUNT`.
//!
//! # Why this exists
//!
//! `AppState` owns an `AccountStore` and `ProviderConfig` declares accounts, so
//! the types and their ownership were in place while nothing ever *ran*. A store
//! nobody fills is a store a GUI cannot render, and `ACCOUNT`'s own record says
//! the missing half plainly: "persistence and production ownership/execution do
//! not exist".
//!
//! What this adds is the loop, and the part that is easy to get wrong: what the
//! store holds **after a failure**. A reconciler that only upserts on success
//! leaves the last good reading in place, so a panel shows a healthy account
//! while it is failing — the worst possible reading, because it is wrong and it
//! looks right. Every probe therefore publishes an outcome, success or not.
//!
//! # The distinction this preserves
//!
//! `AccountQuota`, `AccountUsage` and `RateLimitState` are all `Option`, and so
//! is [`AccountSyncOutcome::Unsupported`]. An account whose provider cannot
//! report usage is not an account with usage of zero, and the two render
//! identically if they are collapsed into one field. So a provider that declines
//! an operation is recorded as declining it, and never as reporting an empty
//! measurement.
//!
//! # Dispatch
//!
//! [`AccountProvider`] uses native `async fn` in a trait, which is not
//! `dyn`-compatible — and the trait's own documentation says as much, naming this
//! as the case where boxing would be needed. [`DynAccountProvider`] is that
//! boxing, written against `std` rather than a dependency, with a blanket impl so
//! an existing adapter needs no changes to be reconcilable.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use crate::account::provider::AccountProvider;
use crate::account::store::AccountStore;
use crate::account::types::{AccountCapabilities, AccountId, AccountRuntime, AccountStatus};
use crate::Error;

/// A boxed future, so an `async fn` in a trait can be dispatched dynamically.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Object-safe view of [`AccountProvider`].
///
/// Blanket-implemented for every `AccountProvider`, so an adapter gains
/// reconcilability without being modified. Deliberately narrower than the trait
/// it wraps: only the operations a reconcile actually performs, because every
/// method added here is a hand-written forward that can drift from the trait.
pub trait DynAccountProvider: Send + Sync {
    /// Provider identifier.
    fn provider_id(&self) -> &str;

    /// What operations this provider supports.
    fn capabilities(&self) -> AccountCapabilities;

    /// Refresh account state from the provider.
    fn refresh_dyn<'a>(
        &'a self,
        account_id: &'a AccountId,
    ) -> BoxFuture<'a, Result<AccountRuntime, Error>>;

    /// Fetch current usage.
    fn fetch_usage_dyn<'a>(
        &'a self,
        account_id: &'a AccountId,
    ) -> BoxFuture<'a, Result<crate::account::types::AccountUsage, Error>>;

    /// Fetch current quota.
    fn fetch_quota_dyn<'a>(
        &'a self,
        account_id: &'a AccountId,
    ) -> BoxFuture<'a, Result<crate::account::types::AccountQuota, Error>>;

    /// Check account health.
    fn health_check_dyn<'a>(
        &'a self,
        account_id: &'a AccountId,
    ) -> BoxFuture<'a, Result<AccountStatus, Error>>;
}

impl<T: AccountProvider> DynAccountProvider for T {
    fn provider_id(&self) -> &str {
        AccountProvider::provider_id(self)
    }

    fn capabilities(&self) -> AccountCapabilities {
        AccountProvider::capabilities(self)
    }

    fn refresh_dyn<'a>(
        &'a self,
        account_id: &'a AccountId,
    ) -> BoxFuture<'a, Result<AccountRuntime, Error>> {
        Box::pin(AccountProvider::refresh(self, account_id))
    }

    fn fetch_usage_dyn<'a>(
        &'a self,
        account_id: &'a AccountId,
    ) -> BoxFuture<'a, Result<crate::account::types::AccountUsage, Error>> {
        Box::pin(AccountProvider::fetch_usage(self, account_id))
    }

    fn fetch_quota_dyn<'a>(
        &'a self,
        account_id: &'a AccountId,
    ) -> BoxFuture<'a, Result<crate::account::types::AccountQuota, Error>> {
        Box::pin(AccountProvider::fetch_quota(self, account_id))
    }

    fn health_check_dyn<'a>(
        &'a self,
        account_id: &'a AccountId,
    ) -> BoxFuture<'a, Result<AccountStatus, Error>> {
        Box::pin(AccountProvider::health_check(self, account_id))
    }
}

/// One declared account to probe.
///
/// The credential is supplied by the caller rather than read here: core has no
/// credential store, and the desktop layer owns the keyring. Turning
/// configuration plus a keyring reference into this shape is the caller's job, and
/// keeping it out of here is what stops a secret reaching a struct that derives
/// `Serialize`.
#[derive(Debug, Clone)]
pub struct AccountProbe {
    /// Provider that owns this account.
    pub provider_id: String,
    /// Which account within that provider.
    pub account_id: AccountId,
}

/// What a reconcile did with one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountSyncOutcome {
    /// The provider answered and the runtime was published.
    Refreshed,
    /// The provider answered, but declines to report usage or quota.
    ///
    /// Distinct from a provider that reported nothing: nothing here claims a
    /// measurement was taken.
    PartiallyReported { missing: Vec<&'static str> },
    /// The provider does not support refreshing at all.
    Unsupported,
    /// The probe failed. The store still holds a runtime, carrying the failure.
    Failed { reason: String },
}

impl AccountSyncOutcome {
    /// Whether this outcome represents a usable reading.
    pub fn is_healthy(&self) -> bool {
        matches!(
            self,
            Self::Refreshed | Self::PartiallyReported { .. } | Self::Unsupported
        )
    }
}

/// The result of reconciling a set of accounts.
#[derive(Debug, Clone, Default)]
pub struct ReconcileReport {
    /// One entry per probed account, in probe order, as
    /// `(provider_id, account_id, outcome)`.
    ///
    /// The provider id is carried rather than looked up, because `AccountStore`
    /// is keyed on the pair and a report that dropped it could not find its own
    /// entries again.
    pub outcomes: Vec<(String, AccountId, AccountSyncOutcome)>,
}

impl ReconcileReport {
    /// How many accounts failed to produce a reading.
    pub fn failed(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|(_, _, outcome)| matches!(outcome, AccountSyncOutcome::Failed { .. }))
            .count()
    }

    /// What the store now holds, keyed by `(provider_id, account_id)`.
    ///
    /// Returned rather than left for each caller to assemble, because a GUI that
    /// builds this by hand will eventually render an absent quota as a zero.
    pub fn snapshot(&self, store: &AccountStore) -> HashMap<(String, String), AccountRuntime> {
        let mut out = HashMap::new();
        for (provider_id, account_id, _) in &self.outcomes {
            if let Some(runtime) = store.get(provider_id, account_id) {
                out.insert((provider_id.clone(), account_id.0.clone()), runtime);
            }
        }
        out
    }
}

/// Probes declared accounts and publishes the result into a store.
pub struct AccountReconciler<'a> {
    store: &'a AccountStore,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl<'a> AccountReconciler<'a> {
    /// Build a reconciler writing into `store`, using the wall clock.
    pub fn new(store: &'a AccountStore) -> Self {
        Self {
            store,
            now: Box::new(|| chrono::Utc::now().timestamp()),
        }
    }

    /// Build a reconciler with an injected clock.
    ///
    /// Injected because every timestamp this writes is part of the contract a
    /// panel renders — "last seen 3 minutes ago" is meaningless if the value is
    /// whatever the test happened to run at.
    pub fn with_clock(
        store: &'a AccountStore,
        now: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            store,
            now: Box::new(now),
        }
    }

    /// Probe every account in `probes` through `provider` and publish each result.
    ///
    /// Publishes on failure as well as on success. A failed probe leaves a
    /// runtime carrying `last_failure` and a non-`Active` status, so a panel
    /// cannot show the previous healthy reading as though it were current.
    pub async fn reconcile(
        &self,
        provider: &dyn DynAccountProvider,
        probes: &[AccountProbe],
    ) -> ReconcileReport {
        let capabilities = provider.capabilities();
        let mut outcomes = Vec::with_capacity(probes.len());

        for probe in probes {
            let outcome = if !capabilities.supports_refresh {
                // Recorded as declining, not as failing, and not as reporting an
                // empty reading: there is no measurement here at all.
                self.publish_unsupported(probe, capabilities.clone());
                AccountSyncOutcome::Unsupported
            } else {
                match provider.refresh_dyn(&probe.account_id).await {
                    Ok(mut runtime) => {
                        let stamp = (self.now)();
                        runtime.provider_id = probe.provider_id.clone();
                        runtime.account_id = probe.account_id.clone();
                        runtime.capabilities = capabilities.clone();
                        if runtime.quota.is_none() && capabilities.supports_quota {
                            if let Ok(quota) = provider.fetch_quota_dyn(&probe.account_id).await {
                                runtime.quota = Some(quota);
                            }
                        }
                        if runtime.usage.is_none() && capabilities.supports_usage {
                            if let Ok(usage) = provider.fetch_usage_dyn(&probe.account_id).await {
                                runtime.usage = Some(usage);
                            }
                        }
                        runtime.last_success = Some(stamp);
                        runtime.last_sync = Some(stamp);
                        self.store.upsert(runtime);
                        match missing_capabilities(&capabilities, self.store, probe) {
                            missing if missing.is_empty() => AccountSyncOutcome::Refreshed,
                            missing => AccountSyncOutcome::PartiallyReported { missing },
                        }
                    }
                    Err(error) => {
                        self.publish_failure(probe, capabilities.clone(), &error);
                        AccountSyncOutcome::Failed {
                            reason: error.to_string(),
                        }
                    }
                }
            };
            outcomes.push((probe.provider_id.clone(), probe.account_id.clone(), outcome));
        }

        ReconcileReport { outcomes }
    }

    /// Publish a runtime for a provider that declines to refresh.
    fn publish_unsupported(&self, probe: &AccountProbe, capabilities: AccountCapabilities) {
        self.store.upsert(AccountRuntime {
            account_id: probe.account_id.clone(),
            provider_id: probe.provider_id.clone(),
            // `Unknown`, not `Active`: nothing has been established about this
            // account, and a panel that renders `Unknown` as healthy would be
            // repeating the original sin in a different place.
            status: AccountStatus::Unknown,
            capabilities,
            quota: None,
            usage: None,
            rate_limit: None,
            last_success: None,
            last_failure: None,
            last_sync: Some((self.now)()),
            metadata: HashMap::new(),
        });
    }

    /// Publish a runtime for a failed probe.
    ///
    /// Publishes rather than skipping, which is the whole point: a reconciler
    /// that only writes on success leaves the last good reading in the store, so
    /// a panel shows a healthy account while it is failing. That is worse than a
    /// wrong number, because it is wrong and it looks right.
    fn publish_failure(
        &self,
        probe: &AccountProbe,
        capabilities: AccountCapabilities,
        error: &Error,
    ) {
        let stamp = (self.now)();
        self.store.upsert(AccountRuntime {
            account_id: probe.account_id.clone(),
            provider_id: probe.provider_id.clone(),
            status: status_for(error),
            capabilities,
            quota: None,
            usage: None,
            rate_limit: None,
            last_success: None,
            last_failure: Some(stamp),
            last_sync: Some(stamp),
            metadata: HashMap::new(),
        });
    }
}

/// Which capabilities went unanswered, so the report can name them.
///
/// Read back out of the store rather than tracked alongside it, so the report
/// cannot disagree with what a panel would actually render.
fn missing_capabilities(
    capabilities: &AccountCapabilities,
    store: &AccountStore,
    probe: &AccountProbe,
) -> Vec<&'static str> {
    let mut missing = Vec::new();
    let Some(runtime) = store.get(&probe.provider_id, &probe.account_id) else {
        return missing;
    };
    if capabilities.supports_quota && runtime.quota.is_none() {
        missing.push("quota");
    }
    if capabilities.supports_usage && runtime.usage.is_none() {
        missing.push("usage");
    }
    missing
}

/// Map a transport or provider error onto an account status.
///
/// Only the distinctions a caller can act on. Everything else becomes `Unknown`
/// rather than being guessed into `Suspended`: telling a user their account is
/// suspended because a request timed out is worse than saying nothing was
/// established.
fn status_for(error: &Error) -> AccountStatus {
    match error {
        Error::Unauthorized | Error::MissingApiKey(_) => AccountStatus::AuthenticationExpired,
        // Everything else stays `Unknown`. A transport failure or an upstream
        // error establishes that the account did not answer, which is not the same
        // as establishing that it is suspended or out of quota, and guessing one of
        // those would tell the user something the evidence does not support.
        _ => AccountStatus::Unknown,
    }
}
