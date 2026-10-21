//! The panel-backed balance, end to end.
//!
//! # Why this file exists
//!
//! Every other balance test drives one piece: the sum of two quotas, the
//! refusal to add two currencies, the question of whether a provider has a
//! balance to ask for at all. None of them asks a panel.
//!
//! What a user gets when they press *查询余额* on a NewAPI relay is a longer
//! chain than any of those: the provider's configuration, the account's own
//! credential resolved through the secret store, an HTTP call to a panel the
//! user cannot see from here, a quota parsed out of the answer, published into
//! a store, read back out, summed, and rendered as a figure in dollars. Every
//! link of that chain was proven on its own, and **the chain itself was not
//! proven at all** — which is the same gap the cutover had, and the same
//! shape of gap: each part right, the whole untested.
//!
//! So the whole is driven here, against a real panel on a real socket. The
//! panel answers the four endpoints the path reads and refuses everything
//! else, the way the real one does.
//!
//! # What the fixture has to be careful about
//!
//! Three things, and each of them is a way the test could pass while the
//! product is broken:
//!
//! 1. **The panel must be the one the account belongs to.** A NewAPI relay
//!    serves `/v1` and publishes no balance there; the panel that owns the
//!    wallet publishes one, and only to the user's own access token. The
//!    provider's relay key is stored too, so a path that resolved the
//!    provider's credential instead of the account's would meet a relay key
//!    where a dashboard token is required — and the adapter refuses to be
//!    built with one.
//! 2. **The panel must answer as the panel answers.** The endpoint answers
//!    carry the dashboard's envelope, not a bare payload, and the quota is in
//!    NewAPI's own credit units rather than in dollars. A fixture that
//!    answered dollars would make the arithmetic look right for the wrong
//!    reason.
//! 3. **The token must arrive.** The credential rides in the `Authorization`
//!    header with no `Bearer` prefix, because NewAPI compares the header
//!    value against the stored token verbatim. The panel here records what it
//!    was given, and the test says so.

#![cfg(feature = "newapi")]

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

use zroutery_core::billing::BalancePreset;
use zroutery_core::config::{AccountConfig, AppConfig, ProviderConfig, ProviderKind};

use super::tests::FakeSecrets;
use crate::secrets::KeychainSecrets;

/// The panel's own unit: NewAPI counts quota in credits, and
/// `common.QuotaPerUnit` says how many credits make a dollar.
const QUOTA_PER_UNIT: f64 = 500_000.0;

/// What the wallet holds, in credits: twelve dollars.
const WALLET_QUOTA: i64 = 6_000_000;
/// What has been spent, in credits: three dollars.
const USED_QUOTA: i64 = 1_500_000;

/// The account's personal access token, as the panel's settings page issues it.
const PANEL_TOKEN: &str = "panel-personal-access-token";

/// A relay key, stored as the provider's own credential.
///
/// Not decoration: the whole point of the account's credential is that it is
/// *not* this. A path that resolved the provider's key would meet a relay key
/// where a dashboard token is required, and the adapter refuses to be built
/// with one — so the figure below could only have come from the account.
const RELAY_KEY: &str = "sk-not-a-dashboard-token";

/// What the panel saw, so the test can say which credential arrived.
type Seen = Arc<Mutex<Vec<String>>>;

/// The panel's own state: the credential it accepts, and what it has seen.
#[derive(Clone)]
struct PanelState {
    accepted: String,
    seen: Seen,
}

/// `GET /api/status`, the public endpoint that says how the panel counts.
async fn panel_status(State(state): State<PanelState>) -> axum::Json<serde_json::Value> {
    state.seen.lock().unwrap().push("GET /api/status".into());
    axum::Json(json!({
        "success": true,
        "message": "",
        "data": {
            "version": "0.9.0",
            "system_name": "fixture panel",
            "quota_per_unit": QUOTA_PER_UNIT,
            "checkin_enabled": false,
            "turnstile_check": false,
            "self_use_mode_enabled": false,
        }
    }))
}

/// `GET /api/user/self`, the endpoint the wallet is read from.
///
/// It refuses any credential it was not told to accept, with the panel's own
/// answer: a 401 and the code that tells a wrong credential from an expired
/// one. A fixture that accepted anything would let a path that presented the
/// wrong token look like it worked.
async fn panel_self(
    State(state): State<PanelState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    state.seen.lock().unwrap().push(presented.clone());

    if presented != state.accepted {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(json!({
                "success": false,
                "message": "无权进行此操作，未提供 access token",
                "code": "AUTH_UNAUTHORIZED",
                "data": null,
            })),
        )
            .into_response();
    }

    axum::Json(json!({
        "success": true,
        "message": "",
        "data": {
            "id": 7,
            "username": "operator",
            "status": 1,
            "quota": WALLET_QUOTA,
            "used_quota": USED_QUOTA,
            "request_count": 3,
            "group": "default",
        }
    }))
    .into_response()
}

/// `GET /api/subscription/self`, which a funded wallet never needs to ask for.
async fn panel_subscription() -> axum::Json<serde_json::Value> {
    axum::Json(json!({"success": true, "message": "", "data": {"subscriptions": []}}))
}

/// `GET /api/log/self/stat`, the rate probe.
async fn panel_stat() -> axum::Json<serde_json::Value> {
    axum::Json(json!({"success": true, "message": "", "data": {"quota": 0, "rpm": 0, "tpm": 0}}))
}

/// A panel that answers the four endpoints the balance path reads.
///
/// It refuses any other endpoint and any credential it was not told to
/// accept, so a path that asked the wrong question or presented the wrong
/// token fails rather than being quietly satisfied.
async fn panel(accept: &str, seen: Seen) -> String {
    let app = Router::new()
        .route("/api/status", get(panel_status))
        .route("/api/user/self", get(panel_self))
        .route("/api/subscription/self", get(panel_subscription))
        .route("/api/log/self/stat", get(panel_stat))
        .fallback(|| async {
            (
                axum::http::StatusCode::NOT_FOUND,
                axum::Json(json!({
                    "success": false,
                    "message": "not found",
                    "code": "NOT_FOUND",
                    "data": null,
                })),
            )
                .into_response()
        })
        .with_state(PanelState {
            accepted: accept.to_string(),
            seen,
        });

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the fixture panel");
    let address = listener.local_addr().expect("the panel's address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

/// A relay whose panel is at `panel`, with one account declared.
///
/// The relay publishes no balance of its own — that is what makes this the
/// case the path exists for — and the account carries its own credential
/// reference, so the panel's token is what gets presented.
fn relay(panel: &str) -> ProviderConfig {
    let mut provider = ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible);
    provider.base_url = panel.to_string();
    provider.balance.preset = BalancePreset::None;
    provider.key_ref = "relay-key".into();
    provider.accounts = vec![AccountConfig {
        account_id: "acct-0".into(),
        key_ref: "acct-token".into(),
        enabled: true,
        ..AccountConfig::default()
    }];
    provider
}

/// A desktop whose secrets live in memory, with both credentials stored.
fn desktop_with(config: AppConfig) -> (Arc<super::Desktop>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("zroutery-panel-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = Arc::new(KeychainSecrets::with_backend(Box::new(
        FakeSecrets::default(),
    )));
    secrets.set("acct-token", PANEL_TOKEN).unwrap();
    secrets.set("relay-key", RELAY_KEY).unwrap();
    (
        Arc::new(super::Desktop::new(dir.clone(), config, secrets)),
        dir,
    )
}

/// The panel's figure, reached through the whole path.
///
/// The chain is the product's: the provider's configuration, the account's
/// credential resolved through the secret store, an HTTP call to a panel the
/// test can see, a quota parsed out of the answer and published into the
/// store, and the sum the panel renders.
#[tokio::test]
async fn a_relays_balance_is_the_panel_wallet_the_account_is_a_user_of() {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let panel_url = panel(PANEL_TOKEN, Arc::clone(&seen)).await;

    let mut config = AppConfig::default();
    config.providers.push(relay(&panel_url));
    let (desktop, dir) = desktop_with(config);

    desktop
        .refresh_balance("relay")
        .await
        .expect("the panel answered the balance question");

    let snapshot = desktop.snapshot().await;
    let status = snapshot
        .balances
        .get("relay")
        .expect("the provider has a balance reading");
    assert!(
        status.error.is_none(),
        "the panel refused rather than answering: {:?}",
        status.error
    );
    let balance = status
        .balance
        .as_ref()
        .expect("the panel's figure is a balance");
    assert_eq!(balance.currency, "USD");
    assert_eq!(balance.remaining, Some(12.0));
    assert_eq!(balance.used, Some(3.0));
    assert_eq!(balance.total, Some(15.0));

    // The account's own token arrived, and the relay key did not.
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.iter().any(|token| token == PANEL_TOKEN),
        "the account's personal access token never reached the panel; it saw {seen:?}"
    );
    assert!(
        !seen.iter().any(|token| token == RELAY_KEY),
        "the relay key was presented to a dashboard endpoint; it saw {seen:?}"
    );

    std::fs::remove_dir_all(dir).ok();
}

/// A panel that will not have this token produces a reason, not a zero.
///
/// The empty case and the refused case are different facts, and a refusal that
/// renders as "0 dollars left" is the defect this path exists to avoid. The
/// message has to name the cause, because the cause is something the user can
/// fix: the wrong kind of credential.
#[tokio::test]
async fn a_panel_that_refuses_the_credential_says_so_instead_of_reporting_zero() {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let panel_url = panel("a-different-token", Arc::clone(&seen)).await;

    let mut config = AppConfig::default();
    config.providers.push(relay(&panel_url));
    let (desktop, dir) = desktop_with(config);

    let error = desktop
        .refresh_balance("relay")
        .await
        .expect_err("a panel that refuses the credential does not produce a balance");

    assert!(
        !error.contains("no account of"),
        "the refusal was reported as a panel that said nothing rather than as the \
         refusal it was: {error}"
    );
    assert!(
        error.contains("acct-0"),
        "the refusal does not name the account it came from: {error}"
    );

    let snapshot = desktop.snapshot().await;
    let status = snapshot
        .balances
        .get("relay")
        .expect("the provider has a balance reading");
    assert!(
        status.balance.is_none(),
        "a refused panel produced a balance anyway: {:?}",
        status.balance
    );
    assert!(
        status.error.is_some(),
        "the refusal was thrown away rather than recorded"
    );

    std::fs::remove_dir_all(dir).ok();
}
