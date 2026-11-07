//! An executed failure keeps its quote; a refused retry is never a second send.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, OnceLock,
};

use axum::{extract::State, http::StatusCode, routing::post, Json};
use serde_json::{json, Value};
use zroutery_core::accounting::AttemptState;
use zroutery_core::billing::Pricing;
use zroutery_core::budget::Ledger;
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ProviderConfig, ProviderKind,
};
use zroutery_core::runtime_store::RuntimeStore;
use zroutery_core::server::{AppState, ServerHandle};

#[derive(Clone, Default)]
struct Mock {
    app: Arc<OnceLock<Arc<AppState>>>,
    calls: Arc<AtomicUsize>,
}

async fn refuse_after_price_change(
    State(mock): State<Mock>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    mock.calls.fetch_add(1, Ordering::SeqCst);
    let app = mock.app.get().unwrap();
    let mut config = (*app.config()).clone();
    config.models[0].pricing = Some(Pricing::new("USD", 200.0, 400.0));
    app.set_config(config);
    let usage = if body.get("input").is_some() {
        json!({"input_tokens": 10, "output_tokens": 2})
    } else {
        json!({"prompt_tokens": 10, "completion_tokens": 2})
    };
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "model": "executed-revision",
            "usage": usage,
            "error": {"type": "server_error", "message": "temporary service outage"}
        })),
    )
}

async fn changed_quote_stops_retry(stream: bool, responses: bool) {
    let mock = Mock::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let path = if responses {
        "/v1/responses"
    } else {
        "/v1/chat/completions"
    };
    let routes = axum::Router::new()
        .route(path, post(refuse_after_price_change))
        .with_state(mock.clone());
    let upstream = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });

    let mut config = AppConfig::default();
    config.server.port = 0;
    config.server.require_auth = false;
    config.server.bypass_proxy = true;
    config.routing.rectifier.enabled = true;
    config.routing.rectifier.codex_auto = responses;
    let mut provider = ProviderConfig::new("p", "Fixture", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{address}");
    provider.key_ref.clear();
    if responses {
        provider.api_surface = serde_json::from_value(json!("responses")).unwrap();
    }
    let mut model = ModelEntry::for_upstream("p", "fixture", None);
    model.pricing = Some(Pricing::new("USD", 2.0, 4.0));
    config.models.push(model);
    config.providers.push(provider);
    let app = Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())));
    assert!(mock.app.set(app.clone()).is_ok());
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(RuntimeStore::open(&root.path().join("runtime.db")).unwrap());
    app.attach_accounting(store.clone(), &Ledger::new(), "retry fixture")
        .unwrap();
    let gateway = ServerHandle::start(app).await.unwrap();

    let body = if responses {
        json!({"model": "p-fixture", "stream": stream, "input": "fixture"})
    } else {
        json!({"model": "p-fixture", "stream": stream,
            "messages": [{"role": "user", "content": "fixture"}]})
    };
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{}{path}", gateway.addr))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let _ = response.bytes().await.unwrap();
    if !responses || !stream {
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
    assert_eq!(
        mock.calls.load(Ordering::SeqCst),
        1,
        "a changed quote cannot replay the request"
    );
    let records = store.usage().unwrap();
    assert_eq!(records.len(), 1);
    assert!(!records[0].ok);
    let attempts = store.usage_attempts(&records[0].id).unwrap();
    assert_eq!(
        attempts.len(),
        2,
        "one executed failure and one refused local retry"
    );
    assert_eq!(attempts[0].state, AttemptState::Failed);
    assert!(attempts[0].sent);
    assert_eq!(
        attempts[0].declared.as_ref().unwrap().observed_model(),
        Some("executed-revision")
    );
    let expected = (10.0 * 2.0 + 2.0 * 4.0) / 1_000_000.0;
    assert!((attempts[0].cost.as_ref().unwrap().amount - expected).abs() < 1e-12);
    assert_eq!(attempts[1].state, AttemptState::Failed);
    assert!(!attempts[1].sent);
    assert!(attempts[1].usage.is_none());
    assert!(attempts[1].cost.is_none());
    let accounting = records[0].accounting.as_ref().unwrap();
    assert_eq!(
        accounting.unknown_cost_attempts, 1,
        "the failed send has only partial usage; local refusal adds no unknown remote charge"
    );
    assert_eq!(accounting.unknown_usage_attempts, 1);
    assert_eq!(accounting.observed_usage.input_tokens, 10);
    assert_eq!(accounting.observed_usage.output_tokens, 2);
    assert!(store.unresolved_attempts().unwrap().is_empty());

    gateway.stop().await;
    upstream.abort();
}

#[tokio::test]
async fn buffered_retry_validates_before_opening_a_sent_attempt() {
    changed_quote_stops_retry(false, false).await;
}

#[tokio::test]
async fn streaming_handshake_retry_validates_before_opening_a_sent_attempt() {
    changed_quote_stops_retry(true, false).await;
}

#[tokio::test]
async fn codex_staging_retry_keeps_the_original_failed_charge() {
    changed_quote_stops_retry(false, true).await;
}
