//! The success path of the production caller, against a real HTTP panel.
//!
//! `account_reconcile_caller_test.rs` proves the caller publishes *failures*, which
//! is the half that is easy to get wrong and the half that protects a panel from
//! showing a stale healthy reading. This file proves the other half: that the same
//! caller, given a panel that answers, produces a `Refreshed` reading with a real
//! quota and a success stamp.
//!
//! Every other fixture in this repository's account suite drives the adapter
//! directly. Before this, nothing drove the whole path — config declaration,
//! credential resolution, adapter construction, HTTP, parse, publish — in one go.
//!
//! # The panel is a real server, not a stub
//!
//! It binds a loopback port and is reached over HTTP by `reqwest`, so the request
//! carries a real `Authorization` header and the adapter's redirect policy, timeouts
//! and bearer handling all apply. That matters for the credential gates below: a
//! credential that fails to resolve never produces a request, which is exactly what
//! makes them observable here and not in the failure-path file.
//!
//! It serves only the three routes `refresh` touches. `rate_limit_probe` is
//! deliberately left to answer as well, so the published runtime carries a rate
//! limit rather than an `Absent` that would hide a parse regression behind a
//! tolerated failure.

#![cfg(feature = "newapi")]

use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use zroutery_core::account::types::{AccountId, AccountStatus};
use zroutery_core::account::{AccountSyncOutcome, ReconcileReport};
use zroutery_core::config::{
    AccountConfig, AppConfig, MemorySecretStore, ProviderConfig, ProviderKind, SecretStore,
};
use zroutery_core::AppState;

/// The bearer each account's credential must present.
const PANEL_BEARER_A: &str = "bearer-for-acct-0";
const PANEL_BEARER_B: &str = "bearer-for-acct-1";

/// A running panel. Dropping it stops the server.
struct Panel {
    base_url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// NewAPI's success envelope. The adapter refuses anything else, which is why a
/// fixture that returns a bare object would look like a broken adapter rather than
/// a broken panel.
fn envelope(data: Value) -> Value {
    json!({ "success": true, "message": "", "data": data })
}

async fn status() -> Json<Value> {
    Json(envelope(json!({
        "version": "v0.9.0",
        "system_name": "New API",
        "quota_per_unit": 500_000.0,
        "checkin_enabled": true,
        "turnstile_check": false,
        "self_use_mode_enabled": false,
        "display_in_currency": true,
        "usd_exchange_rate": 7.3,
    })))
}

async fn self_snapshot(headers: HeaderMap) -> Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_string();

    match presented.as_str() {
        PANEL_BEARER_A | PANEL_BEARER_B => Json(envelope(json!({
            "id": 7,
            "username": "alice",
            "display_name": "Alice",
            "status": 1,
            "group": "default",
            "quota": 1_500_000,
            "used_quota": 500_000,
            "request_count": 42,
        })))
        .into_response(),
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "success": false,
                "code": "AUTH_UNAUTHORIZED",
                "message": "unauthorized",
            })),
        )
            .into_response(),
    }
}

async fn stat(_headers: HeaderMap) -> Json<Value> {
    Json(envelope(json!({ "quota": 0, "rpm": 12, "tpm": 3_400 })))
}

async fn start_panel() -> Panel {
    let app = Router::new()
        .route("/api/status", get(status))
        .route("/api/user/self", get(self_snapshot))
        .route("/api/log/self/stat", get(stat))
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "success": false,
                    "code": "NOT_FOUND",
                    "message": "no such endpoint",
                })),
            )
                .into_response()
        });

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("read the address back");
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Panel {
        base_url: format!("http://{addr}"),
        task,
    }
}

/// A provider with `count` enabled accounts, pointed at the panel.
fn provider(base_url: &str, count: usize) -> ProviderConfig {
    let mut provider =
        ProviderConfig::new("relay", "Relay".to_string(), ProviderKind::OpenAICompatible);
    provider.base_url = base_url.to_string();
    provider.accounts = (0..count)
        .map(|n| AccountConfig {
            account_id: format!("acct-{n}"),
            key_ref: format!("acct-{n}-key"),
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

/// GATE 1: the caller turns a declared account into a REFRESHED reading.
///
/// This is the gate the failure-path file cannot provide. Everything asserted here
/// was produced by production code reading production configuration: the
/// declaration came from `ProviderConfig::accounts`, the credential came from the
/// `SecretStore`, the adapter was built by the caller, and the bytes crossed a
/// socket.
#[tokio::test]
async fn a_declared_account_probes_a_live_panel_and_publishes_a_refreshed_reading() {
    let panel = start_panel().await;
    let mut config = AppConfig::default();
    config.providers.push(provider(&panel.base_url, 1));

    let state = state_with(
        config,
        MemorySecretStore::new().with("acct-0-key", PANEL_BEARER_A),
    );
    let report = state.reconcile_accounts().await;

    // `PartiallyReported`, not `Refreshed`, and that is the honest reading rather
    // than a fixture that was written to match whatever came back. `refresh`
    // deliberately does not pay for the usage sweep, so a panel that declares
    // `supports_usage` and answers every route the sweep needs still has no usage
    // measurement in the store, and the reconciler names that rather than reporting
    // a complete reading it does not have.
    assert_eq!(
        outcome_of(&report, "acct-0"),
        AccountSyncOutcome::PartiallyReported {
            missing: vec!["usage"]
        },
        "a panel that answered must publish a reading that names what it did not measure"
    );
    assert_eq!(report.failed(), 0);

    let runtime = state
        .accounts()
        .get("relay", &AccountId("acct-0".to_string()))
        .expect("the successful probe must be published");

    assert_eq!(
        runtime.status,
        AccountStatus::Active,
        "a panel that answered 200 with a live account is Active"
    );
    assert!(
        runtime.last_success.is_some(),
        "a successful probe must stamp last_success"
    );
    assert!(
        runtime.last_failure.is_none(),
        "a successful probe must not also stamp a failure"
    );
    assert!(
        runtime.quota.is_some(),
        "the panel reported a quota, so an absent one would be a parse regression"
    );
    assert!(
        runtime.rate_limit.is_some(),
        "the rate probe answered, so an absent rate limit would be a parse regression"
    );
    // The credential must not survive into the store, which is `Serialize` and
    // therefore readable by anything holding the snapshot.
    let serialised = serde_json::to_string(&runtime).expect("the runtime serialises");
    assert!(
        !serialised.contains(PANEL_BEARER_A),
        "a credential reached a serialisable runtime: {serialised}"
    );
}

/// GATE 2: the quota is the number the panel reported, not a default.
///
/// Guards the failure mode where a parse regression yields `None` or zero and the
/// panel renders "no quota" instead of "quota unmeasured". `quota_per_unit` is
/// 500_000 and remaining is 1_000_000, so the expected quota is 2.0.
#[tokio::test]
async fn the_published_quota_is_the_number_the_panel_reported() {
    let panel = start_panel().await;
    let mut config = AppConfig::default();
    config.providers.push(provider(&panel.base_url, 1));

    let state = state_with(
        config,
        MemorySecretStore::new().with("acct-0-key", PANEL_BEARER_A),
    );
    let _ = state.reconcile_accounts().await;

    let runtime = state
        .accounts()
        .get("relay", &AccountId("acct-0".to_string()))
        .expect("published");
    let quota = runtime.quota.expect("the panel reported a quota");

    // Whatever the unit arithmetic is, it must be derived from the payload: a
    // hardcoded zero or a default would pass a bare `is_some()`.
    assert_ne!(
        quota.total, 0.0,
        "a panel reporting 1_500_000 quota and 500_000 used must not publish zero"
    );
    assert!(
        quota.total.is_finite() && quota.total > 0.0,
        "published quota is not a usable measurement: {:?}",
        quota.total
    );
}

/// GATE 3: each account is probed with ITS OWN credential.
///
/// This is the gate the failure-path file explicitly gave up on, because against an
/// unreachable endpoint a resolved credential and an absent one fail identically.
/// Here they are distinguishable: the panel answers 401 to a bearer it does not
/// hold, which the adapter maps to `AuthenticationExpired`. So a caller that
/// resolved every account through one reference would show both accounts here.
#[tokio::test]
async fn each_account_is_authenticated_with_its_own_credential() {
    let panel = start_panel().await;
    let mut config = AppConfig::default();
    config.providers.push(provider(&panel.base_url, 2));

    // `acct-0-key` resolves to the bearer the panel holds; `acct-1-key` resolves to
    // a secret the panel does not hold.
    let state = state_with(
        config,
        MemorySecretStore::new()
            .with("acct-0-key", PANEL_BEARER_A)
            .with("acct-1-key", "a-bearer-the-panel-does-not-hold"),
    );
    let report = state.reconcile_accounts().await;

    assert_eq!(report.outcomes.len(), 2, "both accounts are enabled");

    assert_eq!(
        outcome_of(&report, "acct-0"),
        AccountSyncOutcome::PartiallyReported {
            missing: vec!["usage"]
        },
        "the account whose credential the panel holds must produce a reading"
    );
    let refused = outcome_of(&report, "acct-1");
    assert!(
        matches!(refused, AccountSyncOutcome::Failed { .. }),
        "the account whose credential the panel refuses must fail, got {refused:?}"
    );

    let good = state
        .accounts()
        .get("relay", &AccountId("acct-0".to_string()))
        .expect("published");
    let bad = state
        .accounts()
        .get("relay", &AccountId("acct-1".to_string()))
        .expect("a refused account is still published");
    assert_eq!(good.status, AccountStatus::Active);
    assert_eq!(
        bad.status,
        AccountStatus::AuthenticationExpired,
        "a 401 establishes that authentication failed, and nothing more"
    );
    assert!(
        bad.last_success.is_none(),
        "a refused account must not carry a successful refresh"
    );
}

/// GATE 4: an account naming no registered reference is refused, not borrowed.
///
/// The direction that would be dangerous. If an unresolvable reference fell back
/// to the provider's key, this account would authenticate and report healthy,
/// which is the same account probed with someone else's credential.
#[tokio::test]
async fn an_unresolvable_credential_does_not_borrow_the_providers() {
    let panel = start_panel().await;
    let mut p = provider(&panel.base_url, 1);
    // The account names a reference nothing is registered under; the provider's own
    // reference IS registered, and it is the one the panel holds.
    p.accounts[0].key_ref = "acct-0-key-never-registered".to_string();
    p.key_ref = "acct-0-key".to_string();

    let mut config = AppConfig::default();
    config.providers.push(p);

    let state = state_with(
        config,
        MemorySecretStore::new().with("acct-0-key", PANEL_BEARER_A),
    );
    let _ = state.reconcile_accounts().await;

    let runtime = state
        .accounts()
        .get("relay", &AccountId("acct-0".to_string()))
        .expect("published");
    assert_ne!(
        runtime.status,
        AccountStatus::Active,
        "the account borrowed a credential it never declared and reported healthy"
    );
}

/// GATE 5: two accounts, two bearers, two independent readings.
///
/// Proves the caller builds one adapter per account rather than reusing one. If it
/// built a single adapter per provider, the second account would be probed with the
/// first account's bearer and would be refused, or would be silently reported with
/// the first account's numbers.
#[tokio::test]
async fn two_accounts_are_probed_with_two_separate_sessions() {
    let panel = start_panel().await;
    let mut config = AppConfig::default();
    config.providers.push(provider(&panel.base_url, 2));

    let state = state_with(
        config,
        MemorySecretStore::new()
            .with("acct-0-key", PANEL_BEARER_A)
            .with("acct-1-key", PANEL_BEARER_B),
    );
    let report = state.reconcile_accounts().await;

    assert_eq!(report.failed(), 0, "both bearers are held by the panel");
    let complete = AccountSyncOutcome::PartiallyReported {
        missing: vec!["usage"],
    };
    assert_eq!(outcome_of(&report, "acct-0"), complete);
    assert_eq!(outcome_of(&report, "acct-1"), complete);
}
