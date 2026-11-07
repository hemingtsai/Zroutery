//! Rejected raw counters cannot be resurrected by lenient protocol decoders.
use std::sync::Arc;

use axum::{extract::State, routing::post, Json};
use serde_json::{json, Value};
use zroutery_core::billing::Pricing;
use zroutery_core::budget::Ledger;
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ProviderConfig, ProviderKind,
};
use zroutery_core::ir::UsageEvidence;
use zroutery_core::runtime_store::RuntimeStore;
use zroutery_core::server::{AppState, ServerHandle};

async fn malformed_reply(State(responses): State<bool>) -> Json<Value> {
    Json(if responses {
        json!({"id":"reply", "object":"response", "status":"completed", "model":"observed-revision",
            "output":[{"type":"message", "role":"assistant", "content":[{"type":"output_text","text":"fixture"}]}],
            "usage":{"input_tokens":100,"output_tokens":5,"input_tokens_details":{"cached_tokens":-3}}})
    } else {
        json!({"id":"reply", "model":"observed-revision",
            "choices":[{"index":0,"message":{"role":"assistant","content":"fixture"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":100,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":-3}}})
    })
}

async fn invalid_wire_stays_unknown(responses: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let path = if responses {
        "/v1/responses"
    } else {
        "/v1/chat/completions"
    };
    let routes = axum::Router::new()
        .route(path, post(malformed_reply))
        .with_state(responses);
    let upstream = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut config = AppConfig::default();
    config.server.port = 0;
    config.server.require_auth = false;
    config.server.bypass_proxy = true;
    config.routing.rectifier.codex_auto = responses;
    let mut provider = ProviderConfig::new("p", "Fixture", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{address}");
    provider.key_ref.clear();
    if responses {
        provider.api_surface = serde_json::from_value(json!("responses")).unwrap();
    }
    let mut model = ModelEntry::for_upstream("p", "fixture", None);
    model.pricing = Some(Pricing::new("USD", 2.0, 4.0));
    config.providers.push(provider);
    config.models.push(model);
    let app = Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())));
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(RuntimeStore::open(&root.path().join("runtime.db")).unwrap());
    app.attach_accounting(store.clone(), &Ledger::new(), "invalid usage fixture")
        .unwrap();
    let gateway = ServerHandle::start(app.clone()).await.unwrap();
    let body = if responses {
        json!({"model":"p-fixture", "input":"fixture"})
    } else {
        json!({"model":"p-fixture", "messages":[{"role":"user", "content":"fixture"}]})
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
    assert_eq!(
        response.status(),
        200,
        "answer delivery is independent of unknown accounting"
    );
    assert!(response.headers().get("x-zroutery-cost").is_none());
    let _ = response.bytes().await.unwrap();
    let record = store.usage().unwrap().pop().unwrap();
    let attempts = store.usage_attempts(&record.id).unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].usage_evidence, Some(UsageEvidence::Invalid));
    assert!(attempts[0].usage.is_none());
    assert!(attempts[0].cost.is_none());
    assert_eq!(
        attempts[0].declared.as_ref().unwrap().observed_model(),
        Some("observed-revision")
    );
    let accounting = record.accounting.unwrap();
    assert_eq!(accounting.unknown_usage_attempts, 1);
    assert_eq!(accounting.unknown_cost_attempts, 1);
    assert_eq!(
        accounting.observed_usage.input_tokens, 0,
        "no invalid measurement enters totals"
    );
    assert_eq!(app.ledger(), Ledger::new());
    gateway.stop().await;
    upstream.abort();
}

#[tokio::test]
async fn malformed_chat_cache_counters_remain_unknown_after_decoding() {
    invalid_wire_stays_unknown(false).await;
}

#[tokio::test]
async fn malformed_responses_cache_counters_remain_unknown_after_staging() {
    invalid_wire_stays_unknown(true).await;
}

#[tokio::test]
async fn a_json_refusal_to_a_stream_keeps_its_structured_metadata() {
    async fn refusal() -> Json<Value> {
        Json(
            json!({"model":"refused-revision", "usage":{"prompt_tokens":12,"completion_tokens":3},
            "error":{"code":"session_binding_invalidated", "message":"fixture refusal"}}),
        )
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let routes = axum::Router::new().route("/v1/chat/completions", post(refusal));
    let upstream = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut provider = ProviderConfig::new("p", "Fixture", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{address}");
    let client = zroutery_core::config::resolve_client_identity(
        zroutery_core::config::ProviderClientProfile::Native,
        &axum::http::HeaderMap::new(),
    );
    let transport = zroutery_core::upstream::Upstream::new(true, 5);
    let result = transport
        .stream_with_identity_policy(
            &provider,
            None,
            &json!({"model":"m", "stream":true}),
            "m",
            &client,
            true,
        )
        .await;
    if let Ok(mut events) = result {
        use futures_util::StreamExt;
        while events.next().await.is_some() {}
    }
    let observed = client.observation.snapshot();
    assert_eq!(
        observed.declaration.observed_model(),
        Some("refused-revision")
    );
    assert_eq!(observed.accounting_usage().unwrap().input_tokens, 12);
    assert_eq!(observed.usage_evidence, UsageEvidence::Partial);
    upstream.abort();
}

#[tokio::test]
async fn a_parsed_start_cannot_rescue_invalid_raw_usage() {
    async fn malformed_stream() -> axum::response::Response {
        let chunk = json!({"id":"reply", "model":"observed-revision", "choices":[
            {"index":0, "delta":{"content":"fixture"}, "finish_reason":"stop"}],
            "usage":{"prompt_tokens":100,"completion_tokens":5,
                "prompt_tokens_details":{"cached_tokens":-3}}});
        axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(format!(
                "data: {chunk}\n\ndata: [DONE]\n\n"
            )))
            .unwrap()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let routes = axum::Router::new().route("/v1/chat/completions", post(malformed_stream));
    let upstream = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut provider = ProviderConfig::new("p", "Fixture", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{address}");
    let client = zroutery_core::config::resolve_client_identity(
        zroutery_core::config::ProviderClientProfile::Native,
        &axum::http::HeaderMap::new(),
    );
    let transport = zroutery_core::upstream::Upstream::new(true, 5);
    let mut events = transport
        .stream_with_identity_policy(
            &provider,
            None,
            &json!({"model":"m", "stream":true}),
            "m",
            &client,
            true,
        )
        .await
        .unwrap();
    use futures_util::StreamExt;
    while let Some(event) = events.next().await {
        event.unwrap();
    }
    let observed = client.observation.snapshot();
    assert_eq!(
        observed.declaration.observed_model(),
        Some("observed-revision")
    );
    assert_eq!(observed.usage_evidence, UsageEvidence::Invalid);
    assert!(observed.accounting_usage().is_none());
    upstream.abort();
}
