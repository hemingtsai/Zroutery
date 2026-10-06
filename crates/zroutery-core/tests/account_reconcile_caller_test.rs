//! The production caller's own gates: `AppState::reconcile_accounts`.
//!
//! `account_reconcile_test.rs` proves the reconciler does the right thing given a
//! provider. This proves the thing that was missing is now *reachable*: that
//! reading a declaration, resolving a credential, choosing a backend and writing
//! the store happens in production code, not only in the reconciler's tests.
//!
//! # Why every "this fails" case points at a closed loopback port
//!
//! An earlier version of this file used `192.0.2.1` (TEST-NET-1) to mean
//! "unreachable". That is not a property of the machine running the test: it is
//! only unreachable if nothing answers, and a developer with an `HTTP_PROXY` set
//! gets a **502 from their proxy**, which is a response, not a refusal. The test
//! then passes or fails depending on the developer's shell, which is the same
//! class of defect as the [`SnapshotWriter`][^sw] decision elsewhere in this
//! repository: a gate whose outcome depends on the environment rather than on the
//! code.
//!
//! [`closed_endpoint`] binds an ephemeral loopback port and drops the listener, so
//! the kernel refuses the connection. `NO_PROXY` covers loopback, so no proxy is
//! consulted and the result is the same on a laptop, on CI, and behind a proxy.
//!
//! [^sw]: `crates/zroutery-core/src/migration.rs`, whose rollback gate is
//!        injectable for the same reason.

#![cfg(feature = "account")]

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use zroutery_core::account::types::{AccountCapabilities, AccountRuntime, AccountStatus};
use zroutery_core::account::{AccountId, AccountSyncOutcome, ReconcileReport};
use zroutery_core::config::{
    AccountConfig, AppConfig, MemorySecretStore, ProviderConfig, ProviderKind, SecretStore,
};
use zroutery_core::AppState;

/// A base URL on a loopback port nothing is listening on.
///
/// Binding and dropping is how the kernel is made to refuse: the port was real
/// and is now closed, so this is a refused connection rather than a DNS timeout
/// or a proxy's opinion.
async fn closed_endpoint() -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral loopback port");
    let addr: SocketAddr = listener.local_addr().expect("read the bound address back");
    drop(listener);
    format!("http://{addr}")
}

/// A provider with `count` enabled accounts, pointed at `base_url`.
fn provider(id: &str, base_url: &str, count: usize) -> ProviderConfig {
    let mut provider = ProviderConfig::new(
        id.to_string(),
        "Relay".to_string(),
        ProviderKind::OpenAICompatible,
    );
    provider.base_url = base_url.to_string();
    provider.accounts = (0..count)
        .map(|n| AccountConfig {
            account_id: format!("acct-{n}"),
            enabled: true,
            ..AccountConfig::default()
        })
        .collect();
    provider
}

fn state_with(config: AppConfig, secrets: MemorySecretStore) -> AppState {
    AppState::new(config, Arc::new(secrets) as Arc<dyn SecretStore>)
}

fn outcome_of(report: &ReconcileReport, account: &str) -> AccountSyncOutcome {
    report
        .outcomes
        .iter()
        .find(|(_, id, _)| id.0 == account)
        .map(|(_, _, outcome)| outcome.clone())
        .unwrap_or_else(|| panic!("no outcome for {account} in {:?}", report.outcomes))
}

fn stored(state: &AppState, provider: &str, account: &str) -> AccountRuntime {
    state
        .accounts()
        .get(provider, &AccountId(account.to_string()))
        .unwrap_or_else(|| panic!("nothing published for {provider}/{account}"))
}

/// A runtime as a *successful* probe would have left it.
fn healthy(provider: &str, account: &str) -> AccountRuntime {
    AccountRuntime {
        account_id: AccountId(account.to_string()),
        provider_id: provider.to_string(),
        status: AccountStatus::Active,
        capabilities: AccountCapabilities::default(),
        quota: None,
        usage: None,
        rate_limit: None,
        last_success: Some(1),
        last_failure: None,
        last_sync: Some(1),
        metadata: Default::default(),
    }
}

/// GATE 1: a declared, enabled account is probed and published into the store.
///
/// The gate this whole change exists to make possible. Before it, no code path
/// outside the reconciler's own tests ever put a runtime into `AppState`'s store.
/// It asserts the *store*, not the report, because the store is what a panel
/// reads.
#[tokio::test]
async fn a_declared_account_reaches_the_store() {
    let mut config = AppConfig::default();
    config
        .providers
        .push(provider("relay", &closed_endpoint().await, 1));

    let state = state_with(config, MemorySecretStore::new());
    assert!(
        state
            .accounts()
            .get("relay", &AccountId("acct-0".to_string()))
            .is_none(),
        "the store starts empty, so a later hit cannot be pre-existing state"
    );

    let report = state.reconcile_accounts().await;
    assert_eq!(report.outcomes.len(), 1, "one declared account, one entry");

    let runtime = stored(&state, "relay", "acct-0");
    assert_eq!(runtime.provider_id, "relay");
    assert_eq!(runtime.account_id, AccountId("acct-0".to_string()));
    assert!(
        runtime.last_sync.is_some(),
        "a probe that happened must be stamped, or a panel cannot say how stale a reading is"
    );
}

/// GATE 2: a failed probe REPLACES the previous good reading.
///
/// The defect the reconciler exists to prevent, asserted through the caller. If
/// this caller ever skipped the write on failure, the store would keep showing an
/// account as healthy while it is failing — which is worse than a wrong number,
/// because it is wrong *and* it looks right.
#[tokio::test]
async fn a_failed_probe_overwrites_a_previous_healthy_reading() {
    let mut config = AppConfig::default();
    config
        .providers
        .push(provider("relay", &closed_endpoint().await, 1));

    let state = state_with(config, MemorySecretStore::new());

    // Plant a healthy reading, as an earlier successful reconcile would have.
    state.accounts().upsert(healthy("relay", "acct-0"));
    let before = stored(&state, "relay", "acct-0");
    assert_eq!(before.status, AccountStatus::Active);
    assert!(before.last_success.is_some(), "precondition");

    let _ = state.reconcile_accounts().await;

    let after = stored(&state, "relay", "acct-0");
    assert_ne!(
        after.status,
        AccountStatus::Active,
        "the stale healthy reading survived a failed probe"
    );
    assert!(
        after.last_failure.is_some(),
        "a failure must be stamped, or the panel cannot say the account is failing"
    );
    assert_eq!(
        after.last_success, None,
        "last_success must not survive into a reading that was never established"
    );
}

/// GATE 3: a DISABLED account is not probed at all.
///
/// The other half of the absent/present distinction. A switched-off account must
/// be missing from the report, not present as `Unknown`: a panel that cannot tell
/// "off" from "we tried and learned nothing" renders a deliberately disabled
/// account as an unhealthy one and nags about it forever.
#[tokio::test]
async fn a_disabled_account_is_not_probed() {
    let base = closed_endpoint().await;
    let mut p = provider("relay", &base, 2);
    p.accounts[1].enabled = false;

    let mut config = AppConfig::default();
    config.providers.push(p);

    let state = state_with(config, MemorySecretStore::new());
    let report = state.reconcile_accounts().await;

    assert_eq!(
        report.outcomes.len(),
        1,
        "only the enabled account is probed"
    );
    assert!(
        report.outcomes.iter().all(|(_, id, _)| id.0 != "acct-1"),
        "a disabled account was probed"
    );
    assert!(
        state
            .accounts()
            .get("relay", &AccountId("acct-1".to_string()))
            .is_none(),
        "a disabled account must not reach the store at all"
    );
}

/// GATE 4: a DISABLED provider is not probed, whatever its accounts declare.
#[tokio::test]
async fn a_disabled_provider_is_not_probed() {
    let base = closed_endpoint().await;
    let mut p = provider("relay", &base, 2);
    p.enabled = false;

    let mut config = AppConfig::default();
    config.providers.push(p);

    let state = state_with(config, MemorySecretStore::new());
    let report = state.reconcile_accounts().await;

    assert!(
        report.outcomes.is_empty(),
        "a disabled provider was probed: {:?}",
        report.outcomes
    );
    assert!(
        state.accounts().list_by_provider("relay").is_empty(),
        "a disabled provider must publish nothing"
    );
}

/// GATE 5: no declarations means no probes, and no invented accounts.
///
/// The opposite failure: a caller that reports a healthy account nobody declared
/// is a store a panel renders as having accounts it does not have.
#[tokio::test]
async fn a_provider_with_no_accounts_declares_nothing() {
    let base = closed_endpoint().await;
    let mut p = provider("relay", &base, 0);
    p.accounts.clear();

    let mut config = AppConfig::default();
    config.providers.push(p);

    let state = state_with(config, MemorySecretStore::new());
    let report = state.reconcile_accounts().await;

    assert!(report.outcomes.is_empty());
    assert_eq!(report.failed(), 0);
}

/// GATE 6: each declared account is probed and published on its own.
///
/// # What this gate deliberately does NOT claim
///
/// An earlier version of this gate tried to prove that `acct-0` and `acct-1`
/// resolve *different* credentials, by arranging for one `key_ref` to be
/// registered and the other not, and asserting the two accounts landed in
/// different states. That gate could not fail: with a refused connection, an
/// account probed with a real credential and one probed with none both produce a
/// failed probe, and both publish `Unknown`. It was asserting something the store
/// cannot represent.
///
/// Credential resolution is therefore tested where it is decidable — in
/// `resolve_credential`'s own unit tests in `server/accounts.rs`, including the
/// direction that matters, that an unresolvable reference resolves to nothing
/// rather than borrowing the provider's key. This gate keeps only the claim the
/// store can actually carry: each account is its own entry, published
/// independently, under its own key.
#[tokio::test]
async fn each_declared_account_is_probed_and_published_independently() {
    let base = closed_endpoint().await;
    let mut p = provider("relay", &base, 2);
    p.key_ref = "provider-key".to_string();
    p.accounts[1].key_ref = "acct-1-key".to_string();

    let mut config = AppConfig::default();
    config.providers.push(p);

    let state = state_with(
        config,
        MemorySecretStore::new()
            .with("provider-key", "secret-for-provider")
            .with("acct-1-key", "secret-for-acct-1"),
    );
    let report = state.reconcile_accounts().await;

    assert_eq!(
        report.outcomes.len(),
        2,
        "each declared account is its own entry, not one per provider"
    );
    assert_eq!(report.failed(), 2);
    assert_eq!(
        stored(&state, "relay", "acct-0").account_id,
        AccountId("acct-0".to_string())
    );
    assert_eq!(
        stored(&state, "relay", "acct-1").account_id,
        AccountId("acct-1".to_string())
    );
    assert_eq!(
        state.accounts().list_by_provider("relay").len(),
        2,
        "both accounts must be independently addressable by a panel"
    );
}

/// GATE 7: two providers declaring the same account id do not collide.
///
/// `AccountStore` is keyed on `(provider_id, account_id)`. If the caller passed
/// only the account id, two providers declaring `acct-0` would overwrite each
/// other and one would be unreachable while the configuration claimed both.
#[tokio::test]
async fn the_same_account_id_under_two_providers_stays_separate() {
    let mut config = AppConfig::default();
    config
        .providers
        .push(provider("relay-a", &closed_endpoint().await, 1));
    config
        .providers
        .push(provider("relay-b", &closed_endpoint().await, 1));

    let state = state_with(config, MemorySecretStore::new());
    let report = state.reconcile_accounts().await;

    assert_eq!(report.outcomes.len(), 2);
    assert_eq!(stored(&state, "relay-a", "acct-0").provider_id, "relay-a");
    assert_eq!(stored(&state, "relay-b", "acct-0").provider_id, "relay-b");
}

/// GATE 8: a declaration naming no resolvable credential is probed, not skipped.
///
/// An account whose `key_ref` names nothing still gets a published runtime.
/// Skipping it would leave the previous reading in the store — the exact defect
/// GATE 2 covers — so the caller must probe and let the failure publish.
#[tokio::test]
async fn an_unresolvable_credential_is_probed_and_published() {
    let base = closed_endpoint().await;
    let mut p = provider("relay", &base, 1);
    p.accounts[0].key_ref = "nobody-registered-this".to_string();

    let mut config = AppConfig::default();
    config.providers.push(p);

    let state = state_with(config, MemorySecretStore::new());
    let report = state.reconcile_accounts().await;

    assert_eq!(report.outcomes.len(), 1, "the account must still be probed");
    let runtime = stored(&state, "relay", "acct-0");
    assert!(
        runtime.last_failure.is_some() || runtime.last_success.is_some(),
        "a published account must carry one of the two stamps, got {runtime:?}"
    );
    assert_ne!(
        runtime.status,
        AccountStatus::Active,
        "an account that could not be probed must not read as healthy"
    );
}

/// GATE 9: a provider kind no account backend speaks to is published as
/// UNKNOWN — not healthy, and not absent.
///
/// `ProviderKind::Anthropic` has no account backend. The caller must not infer one
/// from the base URL, and must not quietly leave the account out of the store: the
/// two would be rendered as "this account is offline" and "you have no such
/// account", which are different facts and only one of them is true.
#[tokio::test]
async fn a_provider_no_backend_speaks_to_is_published_as_unknown() {
    let base = closed_endpoint().await;
    let mut p = provider("relay", &base, 1);
    p.kind = ProviderKind::Anthropic;

    let mut config = AppConfig::default();
    config.providers.push(p);

    let state = state_with(config, MemorySecretStore::new());
    let report = state.reconcile_accounts().await;

    let outcome = outcome_of(&report, "acct-0");
    assert!(
        matches!(outcome, AccountSyncOutcome::Unsupported),
        "expected Unsupported for a provider no backend speaks to, got {outcome:?}"
    );
    let runtime = stored(&state, "relay", "acct-0");
    assert_eq!(
        runtime.status,
        AccountStatus::Unknown,
        "nothing was established, so Unknown is the only honest status"
    );
    assert_eq!(
        runtime.last_success, None,
        "an account that was never refreshed cannot claim a successful refresh"
    );
    assert_eq!(
        runtime.last_failure, None,
        "declining is not failing: these are different readings and are kept apart"
    );
}

/// GATE 10: a backend that will NOT BUILD is a failure carrying the reason.
///
/// The distinction from GATE 9. There, no backend exists; here one does and the
/// declaration is what is wrong. Collapsing them would make a typo'd base URL
/// indistinguishable from a feature-gated build, and the panel would show a red row
/// with no explanation for the second case and no red row for the first.
#[tokio::test]
async fn an_unbuildable_backend_fails_with_a_reason() {
    let mut config = AppConfig::default();
    config.providers.push(provider("relay", "not a url", 1));

    let state = state_with(config, MemorySecretStore::new());
    let report = state.reconcile_accounts().await;

    let outcome = outcome_of(&report, "acct-0");
    let AccountSyncOutcome::Failed { reason } = outcome else {
        panic!("expected Failed for an unbuildable backend, got {outcome:?}");
    };
    assert!(
        !reason.trim().is_empty(),
        "a failure must carry the reason, or the panel shows an unexplained red row"
    );
    // And it must be a failure, not a silent skip: the account is still published.
    assert!(stored(&state, "relay", "acct-0").last_failure.is_some());
}

/// GATE 11: the report is serialisable, because a panel reads it.
///
/// The report crosses the desktop command boundary as-is. If it stopped being
/// `Serialize`, the UI would have to rebuild it by hand — and a hand-built summary
/// is how an absent quota starts rendering as a zero. `Deserialize` is
/// deliberately *not* derived: nothing reads a report back, and a wire format the
/// product never parses is surface nobody asked for.
#[test]
fn the_report_serialises_with_its_outcomes_intact() {
    let report = ReconcileReport {
        outcomes: vec![
            (
                "relay".to_string(),
                AccountId("acct-0".to_string()),
                AccountSyncOutcome::Refreshed,
            ),
            (
                "relay".to_string(),
                AccountId("acct-1".to_string()),
                AccountSyncOutcome::Failed {
                    reason: "boom".to_string(),
                },
            ),
            (
                "relay".to_string(),
                AccountId("acct-2".to_string()),
                AccountSyncOutcome::PartiallyReported {
                    missing: vec!["quota"],
                },
            ),
        ],
    };

    let json = serde_json::to_value(&report).expect("the report must serialise");
    let outcomes = json["outcomes"]
        .as_array()
        .unwrap_or_else(|| panic!("outcomes must be an array, got {json}"));
    assert_eq!(outcomes.len(), 3);

    // The tagged shape is what keeps a failure from rendering as an account with
    // no reason, which is the mistake a panel is most likely to make.
    assert_eq!(outcomes[0][2], serde_json::json!("refreshed"));
    assert_eq!(
        outcomes[1][2]["failed"]["reason"],
        serde_json::json!("boom")
    );
    assert_eq!(
        outcomes[2][2]["partially_reported"]["missing"],
        serde_json::json!(["quota"]),
        "a partial report must name what is missing rather than leave it empty"
    );
}

/// GATE 12: the report's own `snapshot` finds what the caller published.
///
/// `ReconcileReport::snapshot` exists so a panel does not assemble the store view
/// by hand. Asserted because a caller that published under a key the report does
/// not name would make it return nothing — silently, which is the failure mode a
/// panel cannot detect on its own.
#[tokio::test]
async fn the_reports_snapshot_returns_what_the_caller_published() {
    let mut config = AppConfig::default();
    config
        .providers
        .push(provider("relay", &closed_endpoint().await, 1));

    let state = state_with(config, MemorySecretStore::new());
    let report = state.reconcile_accounts().await;

    let snapshot = report.snapshot(state.accounts());
    assert_eq!(
        snapshot.len(),
        1,
        "the report must be able to find its own entry in the store"
    );
    let runtime = snapshot
        .get(&("relay".to_string(), "acct-0".to_string()))
        .expect("keyed on (provider_id, account_id)");
    assert_eq!(runtime.provider_id, "relay");
}

/// GATE 13: a genuinely refused connection is published as a failure.
///
/// Every other gate here uses [`closed_endpoint`], so this is the one that pins
/// the whole file's central claim to an observed refusal rather than to an
/// assumption: a refused connection produces a `Failed` outcome, and the runtime
/// is still published.
#[tokio::test]
async fn a_refused_connection_is_a_published_failure() {
    let mut config = AppConfig::default();
    config
        .providers
        .push(provider("relay", &closed_endpoint().await, 1));

    let state = state_with(config, MemorySecretStore::new());
    let report = state.reconcile_accounts().await;

    assert_eq!(report.outcomes.len(), 1);
    assert_eq!(
        report.failed(),
        1,
        "a refused connection is a failure, got {:?}",
        report.outcomes
    );
    let runtime = stored(&state, "relay", "acct-0");
    assert!(
        runtime.last_failure.is_some(),
        "the refusal must be stamped"
    );
    assert_ne!(
        runtime.status,
        AccountStatus::Active,
        "a refused connection must not read as healthy"
    );
}
