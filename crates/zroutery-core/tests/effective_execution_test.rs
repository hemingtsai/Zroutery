//! Policy cost ranking binds the actual account/key before the attempt cap.
use std::sync::Arc;

use axum::{extract::State, http::HeaderMap, routing::post, Json};
use serde_json::{json, Value};
use zroutery_core::billing::{EffectiveRate, Pricing, RateSource, RATE_SCHEMA_VERSION};
use zroutery_core::config::{
    AccountBinding, AccountConfig, AppConfig, InferenceKeyConfig, MemorySecretStore, ModelEntry,
    ModelTier, ProviderConfig, ProviderKind, RoutingStrategy,
};
use zroutery_core::policy::{PolicyFallback, PolicyPreference, PolicyRequirements};
use zroutery_core::registry::{Registry, Resolution};
use zroutery_core::router::Router;
use zroutery_core::server::{AppState, ServerHandle};

fn config() -> AppConfig {
    let mut provider = ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible);
    let binding = AccountBinding::fresh();
    let rates = [("first", 0.8), ("cheap", 0.02)]
        .into_iter()
        .map(|(key, amount)| EffectiveRate {
            schema_version: RATE_SCHEMA_VERSION,
            id: key.into(),
            upstream_model: "same-model".into(),
            dialect: zroutery_core::ir::Dialect::OpenAI,
            binding_version: binding.version(),
            inference_key_id: Some(key.into()),
            group: Some(key.into()),
            source: RateSource::ManualOverride,
            observed_at: 1,
            expires_at: None,
            evidence: "explicit fixture tariff".into(),
            pricing: Pricing {
                per_request: Some(amount),
                ..Pricing::new("USD", 0.0, 0.0)
            },
        })
        .collect();
    provider.accounts.push(AccountConfig {
        account_id: "a".into(),
        binding: Some(binding),
        key_ref: "panel-only".into(),
        enabled: true,
        inference_keys: vec![
            InferenceKeyConfig {
                id: "first".into(),
                key_ref: "inference-first".into(),
                priority: 0,
                ..Default::default()
            },
            InferenceKeyConfig {
                id: "cheap".into(),
                key_ref: "inference-cheap".into(),
                priority: 50,
                ..Default::default()
            },
        ],
        effective_rates: rates,
        ..Default::default()
    });
    let mut config = AppConfig::default();
    config.server.port = 0;
    config.server.require_auth = false;
    config.server.bypass_proxy = true;
    config.routing.strategy = RoutingStrategy::Priority;
    config.routing.max_attempts = 1;
    config.providers.push(provider);
    config.models.push(ModelEntry::for_upstream(
        "relay",
        "same-model",
        Some(ModelTier::Standard),
    ));
    config
}

fn cost_preference() -> PolicyPreference {
    PolicyPreference {
        preferred_tier: None,
        health_weight: 0.0,
        latency_weight: 0.0,
        cost_weight: 1.0,
        priority_weight: 0.0,
        tier_weight: 0.0,
    }
}

#[test]
fn priority_and_cost_policies_choose_different_keys_without_duplicating_models() {
    let registry = Registry::new(Arc::new(config()));
    let router = Router::new();
    let (cost, decision) = router
        .plan_with_policy(
            &registry,
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &PolicyRequirements::default(),
            &cost_preference(),
            &PolicyFallback::Reject,
            None,
        )
        .unwrap();
    assert_eq!(cost.len(), 1);
    assert_eq!(decision.candidates.len(), 1);
    assert_eq!(cost[0].credential.as_ref().unwrap().key_id, "cheap");
    assert_eq!(cost[0].settled_pricing().unwrap().per_request, Some(0.02));
    let preference = PolicyPreference {
        cost_weight: 0.0,
        priority_weight: 1.0,
        ..cost_preference()
    };
    let (priority, _) = router
        .plan_with_policy(
            &registry,
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &PolicyRequirements::default(),
            &preference,
            &PolicyFallback::Reject,
            None,
        )
        .unwrap();
    assert_eq!(priority[0].credential.as_ref().unwrap().key_id, "first");
}

#[test]
fn unknown_or_cross_currency_costs_do_not_become_comparable_scores() {
    let mut config = config();
    config.providers[0].accounts[0].effective_rates[1]
        .pricing
        .currency = "CNY".into();
    let router = Router::new();
    let plan = |config: AppConfig| {
        router.plan_with_policy(
            &Registry::new(Arc::new(config)),
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &PolicyRequirements::default(),
            &cost_preference(),
            &PolicyFallback::Reject,
            None,
        )
    };
    assert!(plan(config.clone())
        .unwrap_err()
        .to_string()
        .contains("currencies"));
    config.providers[0].accounts[0].effective_rates.clear();
    assert!(plan(config).unwrap_err().to_string().contains("rate"));
}

#[test]
fn a_cost_preference_is_not_reordered_by_global_priority_or_attempt_cap() {
    let mut config = config();
    let mut other =
        ModelEntry::for_upstream("other", "low-priority-number", Some(ModelTier::Standard));
    other.priority = 0;
    other.pricing = Some(Pricing {
        per_request: Some(0.1),
        ..Pricing::new("USD", 0.0, 0.0)
    });
    config.providers.push(ProviderConfig::new(
        "other",
        "Other",
        ProviderKind::OpenAICompatible,
    ));
    config.models[0].priority = 99;
    config.models.push(other);
    let registry = Registry::new(Arc::new(config));
    let (plan, decision) = Router::new()
        .plan_with_policy(
            &registry,
            &Resolution::Tier(ModelTier::Standard),
            &[],
            &PolicyRequirements::default(),
            &cost_preference(),
            &PolicyFallback::Reject,
            None,
        )
        .unwrap();
    assert_eq!(plan.len(), 1, "the attempt cap applies after cost ordering");
    assert_eq!(plan[0].provider.id, "relay");
    assert_eq!(plan[0].credential.as_ref().unwrap().key_id, "cheap");
    assert_eq!(decision.selected.as_deref(), Some("relay-same-model"));
}

#[tokio::test]
async fn cost_policy_sends_and_settles_the_quoted_key() {
    type Calls = Arc<std::sync::Mutex<Vec<String>>>;
    async fn answer(State(calls): State<Calls>, headers: HeaderMap) -> Json<Value> {
        calls
            .lock()
            .unwrap()
            .push(headers["authorization"].to_str().unwrap().into());
        Json(
            json!({"id":"reply","model":"resolved-model","choices":[{"index":0,
            "message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":10,"completion_tokens":2}}),
        )
    }
    let calls = Calls::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let routes = axum::Router::new()
        .route("/v1/chat/completions", post(answer))
        .with_state(calls.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    let mut config = config();
    config.providers[0].base_url = format!("http://{address}");
    let mut policy = zroutery_core::policy::default_policy();
    policy.id = "cost".into();
    policy.preference = cost_preference();
    config.routing.policies.default_policy = Some(policy.id.clone());
    config.routing.policies.policies.push(policy);
    let secrets = MemorySecretStore::new()
        .with("inference-first", "first-key")
        .with("inference-cheap", "cheap-key");
    let app = Arc::new(AppState::new(config, Arc::new(secrets)));
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        zroutery_core::runtime_store::RuntimeStore::open(&dir.path().join("runtime.db")).unwrap(),
    );
    app.attach_accounting(
        store.clone(),
        &zroutery_core::budget::Ledger::new(),
        "price fixture",
    )
    .unwrap();
    let gateway = ServerHandle::start(app).await.unwrap();
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{}/v1/chat/completions", gateway.addr))
        .json(&json!({"model":"standard-class","messages":[{"role":"user","content":"fixture"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert_eq!(*calls.lock().unwrap(), ["Bearer cheap-key"]);
    let request = store.usage().unwrap().pop().unwrap();
    let attempts = store.usage_attempts(&request.id).unwrap();
    assert_eq!(
        attempts[0].execution.inference_key_id.as_deref(),
        Some("cheap")
    );
    assert_eq!(attempts[0].cost.as_ref().unwrap().amount, 0.02);
    gateway.stop().await;
    task.abort();
}
