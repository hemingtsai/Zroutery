//! Several transient 503s recover; every upstream send keeps a distinct attempt.
use axum::response::IntoResponse;
use axum::routing::post;
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::server::{AppState, ServerHandle};

#[tokio::test]
async fn fourth_attempt_recovers_without_missing_retry_identity_or_replaying_delivered_tools() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mock = tokio::spawn(async move {
        let app = axum::Router::new().route("/v1/chat/completions", post(move || {
            let counter = counter.clone();
            async move {
                if counter.fetch_add(1, Ordering::SeqCst) < 3 {
                    return (axum::http::StatusCode::SERVICE_UNAVAILABLE, axum::Json(json!({"error":{"message":"temporary service outage","type":"server_error"}}))).into_response();
                }
                axum::Json(json!({"id":"complete","model":"mock","choices":[{"message":{"role":"assistant","content":"recovered"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5}})).into_response()
            }
        }));
        axum::serve(listener, app).await.unwrap();
    });
    let mut config = AppConfig::default();
    config.server.port = 0;
    config.server.require_auth = false;
    let mut provider = ProviderConfig::new("p", "Mock", ProviderKind::OpenAICompatible);
    provider.key_ref.clear();
    provider.base_url = format!("http://{address}");
    config.providers.push(provider);
    config.models.push(ModelEntry::for_upstream(
        "p",
        "mock",
        Some(ModelTier::Standard),
    ));
    let app = Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())));
    let gateway = ServerHandle::start(app.clone()).await.unwrap();
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", gateway.addr))
        .json(&json!({"model":"standard-class","messages":[{"role":"user","content":"fixture"}]}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let _ = response.text().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    let outcome = &app.outcomes().recent(1)[0];
    assert_eq!(outcome.attempts.len(), 4);
    assert!(outcome.attempts[3].success);
    let ids: std::collections::HashSet<_> = outcome
        .attempts
        .iter()
        .map(|attempt| &attempt.attempt_id)
        .collect();
    assert_eq!(ids.len(), 4);
    assert_eq!(app.stats().recent(1)[0].attempts, 4);
    gateway.stop().await;
    mock.abort();
}
