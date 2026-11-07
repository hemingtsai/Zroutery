//! Pre-send revalidation: executable options and availability must describe the
//! configuration as it is now, not the snapshot a plan was ranked under.

use std::sync::Arc;
use std::time::Duration;

use axum::{extract::State, http::HeaderMap, routing::post, Json};
use zroutery_core::billing::{EffectiveRate, Pricing, RateSource, RATE_SCHEMA_VERSION};
use zroutery_core::config::{
    AccountBinding, AccountConfig, AppConfig, InferenceKeyConfig, MemorySecretStore, ModelEntry,
    ProviderConfig, ProviderKind,
};
use zroutery_core::ir::Dialect;
use zroutery_core::router::{Candidate, InferenceCredential, Router};
use zroutery_core::server::{AppState, ServerHandle};

const BINDING_VERSION: &str = "gen:0:0";

fn priced_account(key_enabled: bool, input_per_mtok: f64) -> AccountConfig {
    AccountConfig {
        account_id: "a".into(),
        key_ref: "panel-token".into(),
        enabled: true,
        binding: Some(AccountBinding {
            generation: "gen".into(),
            ..AccountBinding::default()
        }),
        inference_keys: vec![InferenceKeyConfig {
            id: "k".into(),
            key_ref: "inference-k".into(),
            enabled: key_enabled,
            ..InferenceKeyConfig::default()
        }],
        effective_rates: vec![EffectiveRate {
            schema_version: RATE_SCHEMA_VERSION,
            id: "rate".into(),
            upstream_model: "model".into(),
            dialect: Dialect::OpenAI,
            binding_version: BINDING_VERSION.into(),
            inference_key_id: Some("k".into()),
            group: Some("vip".into()),
            source: RateSource::VerifiedEffective,
            observed_at: 1,
            expires_at: None,
            evidence: "panel /api/pricing for the account's group".into(),
            pricing: Pricing::new("CNY", input_per_mtok, 4.0),
        }],
        ..Default::default()
    }
}

fn provider_of(account: AccountConfig) -> ProviderConfig {
    let mut provider = ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible);
    provider.accounts.push(account);
    provider
}

fn config_with(provider: &ProviderConfig) -> AppConfig {
    let mut config = AppConfig::default();
    config.providers.push(provider.clone());
    config.models.push(ModelEntry::for_upstream(
        "relay",
        "model",
        Some(zroutery_core::config::ModelTier::Standard),
    ));
    config
}

/// A candidate carrying the provider snapshot a plan was ranked under.
fn planned_candidate(provider: &ProviderConfig) -> Candidate {
    let entry = ModelEntry::for_upstream("relay", "model", None);
    Candidate {
        exposed_id: entry.exposed_id(),
        entry,
        provider: provider.clone(),
        degraded: false,
        credential: None,
        rate_quote: None,
    }
}

#[test]
fn execution_options_describe_the_current_configuration() {
    let router = Router::new();
    let planned = provider_of(priced_account(true, 1.0));
    let candidate = planned_candidate(&planned);

    // The account was repriced after planning: options quote the current
    // tariff, while the option keeps the planned provider identity for the
    // pre-send validation to compare.
    let mut current = provider_of(priced_account(true, 9.0));
    current.base_url = "https://moved.example/v1".into();
    let config = config_with(&current);
    let options = router.execution_options(&config, &candidate);
    assert_eq!(options.len(), 1);
    assert_eq!(
        options[0]
            .rate_quote
            .as_ref()
            .expect("the current rate resolves")
            .pricing
            .input_per_mtok,
        9.0
    );
    assert_eq!(
        options[0].provider.base_url, planned.base_url,
        "the planned identity survives for validate_execution"
    );

    // The key was disabled after planning: no option may offer it.
    let disabled = provider_of(priced_account(false, 1.0));
    let config = config_with(&disabled);
    assert!(router.execution_options(&config, &candidate).is_empty());

    // The provider was removed: nothing is executable.
    let config = AppConfig::default();
    assert!(router.execution_options(&config, &candidate).is_empty());

    // Unchanged: the option is still there with the same credential.
    let unchanged = provider_of(priced_account(true, 1.0));
    let config = config_with(&unchanged);
    let options = router.execution_options(&config, &candidate);
    assert_eq!(options.len(), 1);
    assert_eq!(options[0].credential.as_ref().unwrap().key_id, "k");
}

#[test]
fn closed_wallets_and_cooled_keys_are_not_execution_options() {
    let router = Router::new();
    let mut a = priced_account(true, 1.0);
    a.inference_keys.push(InferenceKeyConfig {
        id: "cheap".into(),
        key_ref: "inference-cheap".into(),
        priority: 50,
        ..InferenceKeyConfig::default()
    });
    let provider = provider_of(a);
    let config = config_with(&provider);
    let candidate = planned_candidate(&provider);

    // An account-level wallet latch closes everything under it.
    router.hold_empty_wallet("relay", Some("a"), None);
    assert!(router.execution_options(&config, &candidate).is_empty());
    router
        .release_wallet("relay", Some("a"), None)
        .expect("operator releases the latch after topping up");

    // A key-level latch closes only that key; the priority sibling remains.
    router.hold_empty_wallet("relay", Some("a"), Some("cheap"));
    let options = router.execution_options(&config, &candidate);
    let offered: Vec<_> = options
        .iter()
        .map(|option| option.credential.as_ref().unwrap().key_id.as_str())
        .collect();
    assert_eq!(offered, ["k"]);
    router
        .release_wallet("relay", Some("a"), Some("cheap"))
        .unwrap();

    // A rate-limit window on one key also removes only that key.
    let mut cheap_candidate = planned_candidate(&provider);
    cheap_candidate.credential = Some(InferenceCredential {
        account_id: "a".into(),
        key_id: "cheap".into(),
    });
    router.note_candidate_rate_limit_wait(&cheap_candidate, Duration::from_secs(30));
    let options = router.execution_options(&config, &candidate);
    assert_eq!(options.len(), 1);
    assert_eq!(options[0].credential.as_ref().unwrap().key_id, "k");
}

#[test]
fn a_gated_provider_or_model_has_no_default_option() {
    let router = Router::new();
    // No explicit pool: the provider default credential is the single option.
    let mut provider = ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible);
    provider.accounts.push(AccountConfig {
        account_id: "a".into(),
        key_ref: "panel-token".into(),
        ..AccountConfig::default()
    });
    let config = config_with(&provider);
    let candidate = planned_candidate(&provider);
    assert_eq!(router.execution_options(&config, &candidate).len(), 1);

    // A provider wallet latch closes the default credential too.
    router.hold_empty_wallet("relay", None, None);
    assert!(router.execution_options(&config, &candidate).is_empty());
    router.release_wallet("relay", None, None).unwrap();

    // So does a rate-limit window on the model itself.
    router.note_candidate_rate_limit_wait(&candidate, Duration::from_secs(30));
    assert!(router.execution_options(&config, &candidate).is_empty());
}

#[tokio::test]
async fn priority_policy_sends_the_expanded_key_under_the_strict_validation() {
    type Calls = Arc<std::sync::Mutex<Vec<String>>>;
    async fn answer(State(calls): State<Calls>, headers: HeaderMap) -> Json<serde_json::Value> {
        calls
            .lock()
            .unwrap()
            .push(headers["authorization"].to_str().unwrap().into());
        Json(serde_json::json!({"id":"reply","model":"resolved-model",
            "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},
            "finish_reason":"stop"}],
            "usage":{"prompt_tokens":10,"completion_tokens":2}}))
    }
    let calls = Calls::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let routes = axum::Router::new()
        .route("/v1/chat/completions", post(answer))
        .with_state(calls.clone());
    let upstream = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });

    let provider = provider_of(priced_account(true, 1.0));
    let mut config = config_with(&provider);
    config.server.port = 0;
    config.server.require_auth = false;
    config.server.bypass_proxy = true;
    config.routing.strategy = zroutery_core::config::RoutingStrategy::Priority;
    config.routing.max_attempts = 1;
    config.providers[0].base_url = format!("http://{address}");
    let secrets = MemorySecretStore::new().with("inference-k", "k-secret");
    let app = Arc::new(AppState::new(config, Arc::new(secrets)));
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        zroutery_core::runtime_store::RuntimeStore::open(&dir.path().join("runtime.db")).unwrap(),
    );
    app.attach_accounting(
        store.clone(),
        &zroutery_core::budget::Ledger::new(),
        "revalidation fixture",
    )
    .unwrap();
    let gateway = ServerHandle::start(app).await.unwrap();
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{}/v1/chat/completions", gateway.addr))
        .json(&serde_json::json!({"model":"standard-class",
            "messages":[{"role":"user","content":"fixture"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert_eq!(*calls.lock().unwrap(), ["Bearer k-secret"]);
    let request = store.usage().unwrap().pop().unwrap();
    let attempts = store.usage_attempts(&request.id).unwrap();
    assert_eq!(
        attempts[0].execution.inference_key_id.as_deref(),
        Some("k"),
        "the expanded key is the one that was sent and recorded"
    );
    gateway.stop().await;
    upstream.abort();
}
