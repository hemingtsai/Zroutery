//! Gate fixtures for account execution: reconciling declared accounts into the
//! store something already owns.
//!
//! # What these exist to prevent
//!
//! `AppState` has owned an `AccountStore` and `ProviderConfig` has declared
//! accounts since the ownership determination, with nothing ever running them. The
//! risk in adding the execution is not that it fails loudly — it is that it fails
//! *quietly in the direction that looks right*:
//!
//! * a reconciler that only publishes on success leaves the last good reading in
//!   place, so a panel shows a healthy account while it is failing;
//! * a provider that declines to report quota is indistinguishable from one
//!   reporting a quota of zero, and `AccountQuota` is an `Option` precisely because
//!   those are different claims;
//! * a transport failure is not evidence that an account is suspended, and
//!   recording it as `Suspended` tells the user something untrue.
//!
//! Every fixture here uses a stub provider, so nothing depends on a network and
//! nothing depends on a panel being reachable.

#![cfg(feature = "account")]

use zroutery_core::account::reconcile::{
    AccountProbe, AccountReconciler, AccountSyncOutcome, DynAccountProvider,
};
use zroutery_core::account::store::AccountStore;
use zroutery_core::account::types::{
    AccountCapabilities, AccountId, AccountQuota, AccountRuntime, AccountStatus, AccountUsage,
};
use zroutery_core::account::AccountProvider;
use zroutery_core::error::Result;

/// A provider whose every answer is scripted, so a test states its expectations
/// rather than discovering them.
struct ScriptedProvider {
    capabilities: AccountCapabilities,
    refresh: RefreshBehaviour,
    /// Whether `fetch_quota` answers. Declaring the capability and then not
    /// answering is the case `PartiallyReported` exists for.
    quota_answers: bool,
    usage_answers: bool,
}

/// How a refresh answers.
///
/// A descriptor rather than a constructed `Error`, because `Error` is not `Clone`
/// and `Error::Transport` carries a `reqwest::Error` a test cannot build. Built
/// fresh on each call instead.
#[derive(Debug, Clone, Copy)]
enum RefreshBehaviour {
    Ok,
    Unauthorized,
    Upstream(u16),
}

impl RefreshBehaviour {
    fn to_error(self) -> Option<zroutery_core::Error> {
        match self {
            Self::Ok => None,
            Self::Unauthorized => Some(zroutery_core::Error::Unauthorized),
            Self::Upstream(status) => Some(zroutery_core::Error::Upstream {
                provider: "relay".to_string(),
                status,
                body: String::new(),
            }),
        }
    }
}

impl ScriptedProvider {
    /// Declares quota, usage and refresh support, and answers all three. A provider
    /// that declares a capability it will not answer is a different fixture, so
    /// "capable" means what it says.
    fn capable() -> Self {
        Self {
            capabilities: AccountCapabilities {
                supports_usage: true,
                supports_quota: true,
                supports_refresh: true,
                supports_checkin: false,
                supports_health_check: true,
            },
            refresh: RefreshBehaviour::Ok,
            quota_answers: true,
            usage_answers: true,
        }
    }

    /// Declares quota and usage support, refreshes successfully, and then declines
    /// to answer either. The case `PartiallyReported` exists for, and one a
    /// panel must not render as a quota of zero.
    fn declares_but_does_not_answer() -> Self {
        Self {
            quota_answers: false,
            usage_answers: false,
            ..Self::capable()
        }
    }

    /// Declares nothing, including refresh.
    fn declining() -> Self {
        Self {
            capabilities: AccountCapabilities::default(),
            refresh: RefreshBehaviour::Ok,
            quota_answers: false,
            usage_answers: false,
        }
    }
}

impl AccountProvider for ScriptedProvider {
    fn provider_id(&self) -> &str {
        "relay"
    }

    fn capabilities(&self) -> AccountCapabilities {
        self.capabilities.clone()
    }

    async fn refresh(&self, account_id: &AccountId) -> Result<AccountRuntime> {
        match self.refresh.to_error() {
            Some(error) => Err(error),
            None => Ok(AccountRuntime {
                account_id: account_id.clone(),
                provider_id: "relay".to_string(),
                status: AccountStatus::Active,
                ..Default::default()
            }),
        }
    }

    async fn fetch_quota(&self, _account_id: &AccountId) -> Result<AccountQuota> {
        if self.quota_answers {
            Ok(AccountQuota {
                total: 100.0,
                used: 25.0,
                remaining: 75.0,
                unit: "USD".to_string(),
                resets_at: None,
            })
        } else {
            Err(zroutery_core::Error::internal("no quota scripted"))
        }
    }

    async fn fetch_usage(&self, _account_id: &AccountId) -> Result<AccountUsage> {
        if self.usage_answers {
            Ok(AccountUsage {
                total_requests: 7,
                total_tokens: 1234,
                total_cost: 1.5,
                currency: "USD".to_string(),
                period_start: None,
                period_end: None,
            })
        } else {
            Err(zroutery_core::Error::internal("no usage scripted"))
        }
    }
}

fn probe(id: &str) -> AccountProbe {
    AccountProbe {
        provider_id: "relay".to_string(),
        account_id: AccountId(id.to_string()),
    }
}

/// A clock that does not move, so a timestamp assertion is about the contract
/// rather than about when the test happened to run.
fn reconciler(store: &AccountStore) -> AccountReconciler<'_> {
    AccountReconciler::with_clock(store, || 1_700_000_000)
}

#[tokio::test]
async fn a_successful_probe_publishes_a_runtime_with_a_timestamp() {
    let store = AccountStore::new();
    let provider = ScriptedProvider::capable();

    let report = reconciler(&store)
        .reconcile(&provider, &[probe("main")])
        .await;

    assert_eq!(report.outcomes.len(), 1);
    assert_eq!(report.outcomes[0].2, AccountSyncOutcome::Refreshed);
    assert_eq!(report.failed(), 0);

    let runtime = store
        .get("relay", &AccountId("main".to_string()))
        .expect("published");
    assert_eq!(runtime.status, AccountStatus::Active);
    assert_eq!(runtime.last_success, Some(1_700_000_000));
    assert_eq!(runtime.last_sync, Some(1_700_000_000));
    assert_eq!(runtime.last_failure, None);
    assert!(runtime.capabilities.supports_quota);
}

#[tokio::test]
async fn a_failed_probe_replaces_the_previous_good_reading() {
    // The one that matters most. If failure published nothing, the store would
    // still hold the last `Active` reading and a panel would show a healthy
    // account while it is failing — wrong, and looking right.
    let store = AccountStore::new();
    let account_id = AccountId("main".to_string());

    // First a success.
    reconciler(&store)
        .reconcile(&ScriptedProvider::capable(), &[probe("main")])
        .await;
    assert_eq!(
        store.get("relay", &account_id).unwrap().status,
        AccountStatus::Active
    );

    // Then the provider starts failing.
    let mut failing = ScriptedProvider::capable();
    failing.refresh = RefreshBehaviour::Upstream(503);
    let report = reconciler(&store)
        .reconcile(&failing, &[probe("main")])
        .await;

    assert_eq!(report.failed(), 1);
    let runtime = store.get("relay", &account_id).expect("still published");
    assert_ne!(
        runtime.status,
        AccountStatus::Active,
        "the stale healthy reading must not survive a failure"
    );
    assert_eq!(runtime.last_failure, Some(1_700_000_000));
    assert_eq!(
        runtime.last_success, None,
        "a failure must not claim a success timestamp for this attempt"
    );
}

#[tokio::test]
async fn an_authentication_failure_becomes_authentication_expired() {
    let store = AccountStore::new();
    let mut provider = ScriptedProvider::capable();
    provider.refresh = RefreshBehaviour::Unauthorized;

    reconciler(&store)
        .reconcile(&provider, &[probe("main")])
        .await;

    let runtime = store
        .get("relay", &AccountId("main".to_string()))
        .expect("published");
    assert_eq!(runtime.status, AccountStatus::AuthenticationExpired);
}

#[tokio::test]
async fn a_transport_failure_does_not_claim_the_account_is_suspended() {
    // A timeout establishes that the account did not answer. It does not
    // establish that it is suspended, and recording that would tell the user
    // something the evidence does not support.
    let store = AccountStore::new();
    let mut provider = ScriptedProvider::capable();
    provider.refresh = RefreshBehaviour::Upstream(503);

    reconciler(&store)
        .reconcile(&provider, &[probe("main")])
        .await;

    let runtime = store
        .get("relay", &AccountId("main".to_string()))
        .expect("published");
    assert_eq!(
        runtime.status,
        AccountStatus::Unknown,
        "nothing was established beyond 'it did not answer'"
    );
}

#[tokio::test]
async fn a_provider_that_declines_refresh_is_not_a_failure_and_not_a_reading() {
    let store = AccountStore::new();
    let report = reconciler(&store)
        .reconcile(&ScriptedProvider::declining(), &[probe("main")])
        .await;

    assert_eq!(report.outcomes[0].2, AccountSyncOutcome::Unsupported);
    assert!(report.outcomes[0].2.is_healthy());
    assert_eq!(report.failed(), 0, "declining is not failing");

    let runtime = store
        .get("relay", &AccountId("main".to_string()))
        .expect("published so a panel can show the account exists");
    assert_eq!(runtime.quota, None);
    assert_eq!(runtime.usage, None);
    assert_eq!(
        runtime.last_success, None,
        "no measurement was taken, so no success may be claimed"
    );
    assert_eq!(
        runtime.status,
        AccountStatus::Unknown,
        "not Active: nothing established, and rendering Unknown as healthy would \
         repeat the original sin elsewhere"
    );
}

#[tokio::test]
async fn a_declined_capability_is_named_rather_than_reported_as_an_empty_reading() {
    // The provider says it supports quota, then does not answer with one. That is
    // `PartiallyReported { missing: ["quota"] }`, not a quota of zero.
    let store = AccountStore::new();
    let provider = ScriptedProvider::declares_but_does_not_answer();
    let report = reconciler(&store)
        .reconcile(&provider, &[probe("main")])
        .await;

    assert_eq!(
        report.outcomes[0].2,
        AccountSyncOutcome::PartiallyReported {
            missing: vec!["quota", "usage"]
        },
        "both declared capabilities went unanswered and both must be named"
    );

    let runtime = store
        .get("relay", &AccountId("main".to_string()))
        .expect("published");
    assert_eq!(runtime.quota, None, "absent, not zero");
    assert_eq!(runtime.usage, None, "absent, not zero");
}

#[tokio::test]
async fn several_accounts_are_probed_independently() {
    // One provider serves several declared accounts, and one failing must not
    // stop the others or overwrite them.
    let store = AccountStore::new();
    let mut provider = ScriptedProvider::capable();
    provider.refresh = RefreshBehaviour::Ok;

    let report = reconciler(&store)
        .reconcile(&provider, &[probe("main"), probe("backup"), probe("spare")])
        .await;

    assert_eq!(report.outcomes.len(), 3);
    assert_eq!(report.failed(), 0);
    for id in ["main", "backup", "spare"] {
        assert!(
            store.get("relay", &AccountId(id.to_string())).is_some(),
            "{id} should have been published independently"
        );
    }
    assert_eq!(store.list_by_provider("relay").len(), 3);
}

#[tokio::test]
async fn the_report_snapshot_is_keyed_the_way_the_store_is() {
    // `snapshot` reads the store back by `(provider_id, account_id)`. If it lost
    // the provider it would silently return an empty map, which looks exactly
    // like "no accounts".
    let store = AccountStore::new();
    let report = reconciler(&store)
        .reconcile(
            &ScriptedProvider::capable(),
            &[probe("main"), probe("backup")],
        )
        .await;

    let snapshot = report.snapshot(&store);
    assert_eq!(snapshot.len(), 2);
    assert!(snapshot.contains_key(&("relay".to_string(), "main".to_string())));
    assert!(snapshot.contains_key(&("relay".to_string(), "backup".to_string())));
}

#[tokio::test]
async fn an_empty_probe_set_publishes_nothing_and_succeeds() {
    let store = AccountStore::new();
    let report = reconciler(&store)
        .reconcile(&ScriptedProvider::capable(), &[])
        .await;

    assert!(report.outcomes.is_empty());
    assert_eq!(report.failed(), 0);
    assert!(report.snapshot(&store).is_empty());
}

#[tokio::test]
async fn the_dyn_shim_forwards_to_the_trait_it_wraps() {
    // The shim is hand-written forwarding, so it can drift from the trait. This
    // is what proves an existing adapter is reconcilable through it unchanged.
    let store = AccountStore::new();
    let provider = ScriptedProvider::capable();
    let boxed: &dyn DynAccountProvider = &provider;

    assert_eq!(boxed.provider_id(), "relay");
    assert!(boxed.capabilities().supports_refresh);

    let report = reconciler(&store).reconcile(boxed, &[probe("main")]).await;
    assert_eq!(report.outcomes[0].2, AccountSyncOutcome::Refreshed);
    assert!(store.get("relay", &AccountId("main".to_string())).is_some());
}
