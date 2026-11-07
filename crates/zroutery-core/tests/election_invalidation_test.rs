//! Election cache invalidation and the late-publication race, on loopback fakes.
//!
//! # What is pinned
//!
//! * A standing election is invalidated by the changes its ranking actually
//!   measured: an existing model's price or priority, a provider's credential
//!   or account binding, an account's effective rate, and the scoring rules.
//! * Display edits (names, aliases, context windows), membership flips, and a
//!   pure model addition keep the measurement — and the newcomer does not take
//!   the measured primary slot.
//! * An election suspended mid-probe by a configuration swap must not publish
//!   the ranking it measured against the replaced snapshot, while the swap
//!   itself still drops the standing cache at the moment of the change.
//!
//! Every "upstream" here is an axum app on `127.0.0.1:0`; secrets live in a
//! `MemorySecretStore`; synchronization uses `Notify` and an atomic arm flag,
//! never sleep-based waits. Nothing here reaches a real gateway, reads a real
//! credential, or sends a paid probe.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use chrono::Utc;
use serde_json::{json, Value};

use zroutery_core::billing::{EffectiveRate, Pricing, RateSource, RATE_SCHEMA_VERSION};
use zroutery_core::config::{
    AccountBinding, AccountConfig, AppConfig, MemorySecretStore, ModelEntry, ModelTier,
    ProviderConfig, ProviderKind, RoutingStrategy,
};
use zroutery_core::election::Election;
use zroutery_core::ir::Dialect;
use zroutery_core::server::AppState;

// ------------------------------------------------------------------ fake upstream

/// Park-and-release control for one probe, so a test can catch an election
/// suspended between measuring and publishing.
#[derive(Default)]
struct Stall {
    /// Set by the test: the next probe parks instead of answering.
    armed: AtomicBool,
    /// Signalled once a parked probe has arrived at the fake.
    arrived: tokio::sync::Notify,
    /// Signalled by the test to let the parked probe answer.
    release: tokio::sync::Notify,
}

#[derive(Clone, Default)]
struct Fake {
    stall: Arc<Stall>,
}

async fn answer_chat(State(fake): State<Fake>, Json(body): Json<Value>) -> Response {
    if fake.stall.armed.swap(false, Ordering::SeqCst) {
        fake.stall.arrived.notify_one();
        fake.stall.release.notified().await;
    }
    let model = body["model"].as_str().unwrap_or("unknown");
    Json(json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion",
        "created": 1,
        "model": model,
        "choices": [{"index": 0,
                     "message": {"role": "assistant", "content": "pong"},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1}
    }))
    .into_response()
}

/// Bind a loopback port and serve the fake; the listener dies with the test's
/// runtime.
async fn start_fake() -> (SocketAddr, Arc<Stall>) {
    let fake = Fake::default();
    let stall = Arc::clone(&fake.stall);
    let app = axum::Router::new()
        .route("/chat/completions", post(answer_chat))
        // Bare OpenAI-compatible hosts get `/v1` added by chat_url().
        .route("/v1/chat/completions", post(answer_chat))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, stall)
}

// ------------------------------------------------------------------ fixtures

/// Two standard-tier members on one fake relay, priced a hundred to one and
/// scored on price alone, so a price change visibly re-ranks them. Weighting
/// latency out is deliberate: the first probe of an election always pays the
/// cold TCP connect (~4 ms) while the second reuses the pooled connection
/// (0 ms), and a zero best-latency sends the other member's ratio to the cap —
/// a jitter coin-flip the price axis must not share.
fn config_for(addr: SocketAddr) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.routing.strategy = RoutingStrategy::Balanced;
    cfg.routing.scoring.price_weight = 1.0;
    cfg.routing.scoring.latency_weight = 0.0;

    let mut provider = ProviderConfig::new("fake", "Fake Relay", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{addr}");
    provider.key_ref = "provider:fake".into();
    provider.timeout_secs = 10;
    cfg.providers = vec![provider];
    cfg.models = vec![
        ModelEntry::for_upstream("fake", "model-a", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("fake", "model-b", Some(ModelTier::Standard)),
    ];
    cfg.models[0].pricing = Some(Pricing::new("USD", 0.01, 0.04));
    cfg.models[1].pricing = Some(Pricing::new("USD", 1.0, 4.0));
    cfg
}

fn state_for(cfg: AppConfig) -> Arc<AppState> {
    // In-memory secrets only; no real credential is read or stored.
    let secrets = Arc::new(MemorySecretStore::new().with("provider:fake", "sk-fake"));
    Arc::new(AppState::new(cfg, secrets))
}

/// Every wait is bounded so a broken signal fails the test instead of hanging.
const WAIT: Duration = Duration::from_secs(10);

/// Hold an election and assert it actually took the cache.
async fn hold_installed(state: &AppState) -> Election {
    let election = state.hold_election().await;
    assert!(
        state.router().election().is_some(),
        "an election over the live snapshot is installed"
    );
    election
}

/// Hold an election, apply one configuration edit, and assert the pinned
/// ranking did not survive it.
async fn assert_edit_invalidates(state: &AppState, why: &str, edit: impl FnOnce(&mut AppConfig)) {
    hold_installed(state).await;
    let mut next = (*state.config()).clone();
    edit(&mut next);
    state.set_config(next);
    assert!(
        state.router().election().is_none(),
        "{why}: the pinned ranking must not survive"
    );
}

/// Hold an election, apply one configuration edit, and assert the pinned
/// ranking survived it untouched.
async fn assert_edit_preserves(state: &AppState, why: &str, edit: impl FnOnce(&mut AppConfig)) {
    let pinned = hold_installed(state).await;
    let mut next = (*state.config()).clone();
    edit(&mut next);
    state.set_config(next);
    let standing = state
        .router()
        .election()
        .unwrap_or_else(|| panic!("{why}: the measured ranking must survive"));
    assert_eq!(
        standing, pinned,
        "{why}: the pinned ranking must be untouched"
    );
}

// ------------------------------------------------------------------ invalidating edits

#[tokio::test]
async fn an_existing_model_price_change_invalidates_the_standing_election() {
    let (addr, _stall) = start_fake().await;
    let state = state_for(config_for(addr));
    assert_edit_invalidates(
        &state,
        "the ranked members' prices no longer exist",
        |next| next.models[0].pricing = Some(Pricing::new("USD", 100.0, 400.0)),
    )
    .await;
}

#[tokio::test]
async fn a_priority_change_invalidates_the_standing_election() {
    let (addr, _stall) = start_fake().await;
    let state = state_for(config_for(addr));
    assert_edit_invalidates(
        &state,
        "the tie-break the election measured changed",
        |next| {
            next.models[0].priority = -3;
        },
    )
    .await;
}

#[tokio::test]
async fn a_scoring_change_invalidates_the_standing_election() {
    let (addr, _stall) = start_fake().await;
    let state = state_for(config_for(addr));
    assert_edit_invalidates(
        &state,
        "every price was scored through the old weights",
        |next| {
            next.routing.scoring.price_weight = 0.0;
            next.routing.scoring.latency_weight = 1.0;
        },
    )
    .await;
}

#[tokio::test]
async fn a_credential_change_invalidates_the_standing_election() {
    let (addr, _stall) = start_fake().await;
    let state = state_for(config_for(addr));
    assert_edit_invalidates(&state, "the probe ran against the old credential", |next| {
        next.providers[0].key_ref = "provider:fake-rotated".into();
    })
    .await;
}

#[tokio::test]
async fn an_account_binding_change_invalidates_the_standing_election() {
    let (addr, _stall) = start_fake().await;
    let state = state_for(config_for(addr));
    assert_edit_invalidates(
        &state,
        "quotes bind to a binding the ranking never saw",
        |next| {
            next.providers[0].accounts.push(AccountConfig {
                account_id: "acct".into(),
                key_ref: "account:acct".into(),
                binding: Some(AccountBinding::fresh()),
                ..AccountConfig::default()
            });
        },
    )
    .await;
}

#[tokio::test]
async fn an_account_effective_rate_change_invalidates_the_standing_election() {
    let (addr, _stall) = start_fake().await;
    let state = state_for(config_for(addr));
    assert_edit_invalidates(
        &state,
        "a verified effective rate is a quote the ranking never saw",
        |next| {
            let binding = AccountBinding::fresh();
            next.providers[0].accounts.push(AccountConfig {
                account_id: "acct".into(),
                binding: Some(binding.clone()),
                effective_rates: vec![EffectiveRate {
                    schema_version: RATE_SCHEMA_VERSION,
                    id: "rate-1".into(),
                    upstream_model: "model-a".into(),
                    dialect: Dialect::OpenAI,
                    binding_version: binding.version(),
                    inference_key_id: None,
                    group: Some("default".into()),
                    source: RateSource::VerifiedEffective,
                    observed_at: Utc::now().timestamp(),
                    expires_at: None,
                    evidence: "loopback fixture".into(),
                    pricing: Pricing::new("USD", 0.05, 0.2),
                }],
                ..AccountConfig::default()
            });
        },
    )
    .await;
}

// ------------------------------------------------------------------ preserving edits

#[tokio::test]
async fn display_edits_keep_the_measured_ranking() {
    let (addr, _stall) = start_fake().await;
    let state = state_for(config_for(addr));
    assert_edit_preserves(&state, "display metadata measured nothing", |next| {
        next.models[0].display_name = Some("Fancier A".into());
        next.models[0].aliases.push("cheap-a".into());
        next.models[0].context_window = Some(128_000);
    })
    .await;
}

#[tokio::test]
async fn a_provider_rename_keeps_the_measured_ranking() {
    let (addr, _stall) = start_fake().await;
    let state = state_for(config_for(addr));
    assert_edit_preserves(&state, "the GUI label measured nothing", |next| {
        next.providers[0].name = "Renamed Relay".into();
    })
    .await;
}

#[tokio::test]
async fn membership_and_capability_edits_keep_the_measured_ranking() {
    let (addr, _stall) = start_fake().await;
    let state = state_for(config_for(addr));
    // A tier move and a capability flip change who is eligible for what, not
    // the relative order the members measured; the pinned order is filtered
    // against live membership when it is used.
    assert_edit_preserves(&state, "the members' measured order is unchanged", |next| {
        next.models[1].tier = Some(ModelTier::Fast);
        next.models[0].capabilities.tools = false;
    })
    .await;
}

#[tokio::test]
async fn a_model_added_after_an_election_keeps_the_measurement_and_its_slot() {
    let (addr, _stall) = start_fake().await;
    let state = state_for(config_for(addr));
    let pinned = hold_installed(&state).await;
    assert_eq!(
        pinned.tiers.get(&ModelTier::Standard).unwrap().winner(),
        Some("fake-model-a")
    );

    // A newcomer whose priority would put it first by hand.
    let mut next = (*state.config()).clone();
    next.models.push(
        ModelEntry::for_upstream("fake", "model-c", Some(ModelTier::Standard)).with_priority(-100),
    );
    state.set_config(next);

    let standing = state
        .router()
        .election()
        .expect("a pure addition must keep the measured ranking");
    assert_eq!(standing, pinned);
    assert_eq!(
        standing.tiers.get(&ModelTier::Standard).unwrap().winner(),
        Some("fake-model-a"),
        "the unmeasured newcomer did not take the measured primary slot"
    );
}

// ------------------------------------------------------------------ the late-publication race

/// The race the cache contract lives or dies by: an election suspended
/// mid-measurement while a configuration swap lands must not publish the
/// ranking it measured against the replaced snapshot.
#[tokio::test]
async fn an_election_overtaken_by_a_config_swap_cannot_install_its_stale_ranking() {
    let (addr, stall) = start_fake().await;
    let state = state_for(config_for(addr));

    // A first election installs the ranking the swap is about to invalidate.
    let first = hold_installed(&state).await;
    assert_eq!(
        first.tiers.get(&ModelTier::Standard).unwrap().winner(),
        Some("fake-model-a")
    );

    // Park the next election's first probe mid-measurement.
    stall.armed.store(true, Ordering::SeqCst);
    let suspended = {
        let state = Arc::clone(&state);
        tokio::spawn(async move { state.hold_election().await })
    };
    tokio::time::timeout(WAIT, stall.arrived.notified())
        .await
        .expect("the probe reached the fake and parked");

    // Swap the configuration while the election is suspended. The swap itself
    // drops the standing ranking — invalidation at the moment of the change.
    let mut next = (*state.config()).clone();
    next.models[0].pricing = Some(Pricing::new("USD", 100.0, 400.0));
    state.set_config(next);
    assert!(
        state.router().election().is_none(),
        "the swap invalidated the standing ranking"
    );

    // Let the suspended election finish. Its result was measured against the
    // replaced snapshot; it must not be installed, no matter how it ranked.
    stall.armed.store(false, Ordering::SeqCst);
    stall.release.notify_one();
    let late = tokio::time::timeout(WAIT, suspended)
        .await
        .expect("the suspended election finished")
        .expect("the election task did not panic");
    assert_eq!(
        late.tiers.get(&ModelTier::Standard).unwrap().winner(),
        Some("fake-model-a"),
        "the late result is the ranking measured under the old price"
    );
    assert!(
        state.router().election().is_none(),
        "a ranking measured against the replaced configuration must not be installed"
    );

    // The cache is refilled only by an election under the live configuration,
    // which under the new price ranks the other member first.
    let fresh = state.hold_election().await;
    assert_eq!(
        fresh.tiers.get(&ModelTier::Standard).unwrap().winner(),
        Some("fake-model-b")
    );
    assert!(
        state.router().election().is_some(),
        "the fresh ranking is installed"
    );
}
