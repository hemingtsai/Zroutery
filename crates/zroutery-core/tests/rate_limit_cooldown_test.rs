//! Scoped 429 cooldowns through production paths, against loopback mocks only.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::config::{
    AccountConfig, AppConfig, InferenceKeyConfig, MemorySecretStore, ModelEntry, ModelTier,
    ProviderConfig, ProviderKind,
};
use zroutery_core::server::{AppState, ServerHandle};

#[derive(Clone, Default)]
struct Mock(Arc<Mutex<Vec<(String, String)>>>);

async fn upstream(
    State(mock): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let model = body["model"].as_str().unwrap_or("").to_string();
    let key = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    mock.0.lock().unwrap().push((model.clone(), key.clone()));
    if model.starts_with("limited") || key == "Bearer fixture-limited" {
        return Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header("retry-after", "120")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({"error":{"type":"rate_limit_error","message":"temporary token window"}})
                    .to_string(),
            ))
            .unwrap();
    }
    Json(json!({"id":"mock","model":model,"choices":[{"message":{"role":"assistant","content":"healthy"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":20,"completion_tokens":3}})).into_response()
}

async fn fixture(
    surface: &str,
    keys: bool,
) -> (
    Arc<AppState>,
    ServerHandle,
    Mock,
    tokio::task::JoinHandle<()>,
) {
    let mock = Mock::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let routes = axum::Router::new()
        .route("/v1/chat/completions", post(upstream))
        .route("/v1/responses", post(upstream))
        .with_state(mock.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    let mut config = AppConfig::default();
    config.server.port = 0;
    config.server.require_auth = false;
    config.server.bypass_proxy = true;
    config.routing.strategy = zroutery_core::config::RoutingStrategy::Priority;
    let mut provider = ProviderConfig::new("p", "Mock", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{addr}");
    provider.api_surface = serde_json::from_value(json!(surface)).unwrap();
    provider.key_ref.clear();
    let mut secrets = MemorySecretStore::new();
    if keys {
        provider.accounts.push(AccountConfig {
            account_id: "account".into(),
            enabled: true,
            inference_keys: vec![
                InferenceKeyConfig {
                    id: "first".into(),
                    key_ref: "first-ref".into(),
                    priority: 0,
                    ..Default::default()
                },
                InferenceKeyConfig {
                    id: "second".into(),
                    key_ref: "second-ref".into(),
                    priority: 10,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        secrets.insert("first-ref", "fixture-limited");
        secrets.insert("second-ref", "fixture-healthy");
        config.models.push(ModelEntry::for_upstream(
            "p",
            "healthy",
            Some(ModelTier::Standard),
        ));
    } else {
        config.models.push(
            ModelEntry::for_upstream("p", "limited", Some(ModelTier::Standard)).with_priority(0),
        );
        config.models.push(
            ModelEntry::for_upstream("p", "healthy", Some(ModelTier::Standard)).with_priority(10),
        );
    }
    config.providers.push(provider);
    let state = Arc::new(AppState::new(config, Arc::new(secrets)));
    let gateway = ServerHandle::start(state.clone()).await.unwrap();
    (state, gateway, mock, task)
}

async fn ask(gateway: &ServerHandle, model: &str, stream: bool) -> reqwest::Response {
    reqwest::Client::builder().no_proxy().build().unwrap().post(format!("http://{}/v1/chat/completions",gateway.addr))
        .json(&json!({"model":model,"stream":stream,"messages":[{"role":"user","content":"local fixture"}]}))
        .send().await.unwrap()
}

#[tokio::test]
async fn first_429_closes_default_model_window_and_next_direct_request_never_reaches_the_upstream()
{
    for stream in [false, true] {
        let (state, gateway, mock, task) = fixture("chat", false).await;
        let first = ask(&gateway, "p-limited", stream).await;
        assert_eq!(first.status(), 429);
        assert_eq!(first.headers()["retry-after"], "120");
        let _ = first.text().await.unwrap();
        let second = ask(&gateway, "p-limited", stream).await;
        assert_eq!(
            second.status(),
            429,
            "local cooldown should not become a transient 503"
        );
        assert!(
            second.headers()["retry-after"]
                .to_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                >= 119
        );
        assert_eq!(
            mock.0.lock().unwrap().len(),
            1,
            "no same-candidate replay or repeated send in cooling"
        );
        assert!(!state.router().is_circuit_open("p-limited"));
        assert!(
            !state.router().is_provider_gated("p"),
            "ordinary 429 is model scoped"
        );
        assert!(
            state
                .router()
                .model_rate_limit_remaining("p", "p-limited")
                .unwrap()
                > Duration::from_secs(118)
        );
        assert_eq!(
            state
                .router()
                .stats_store
                .get("p-limited", "p")
                .failures
                .count(zroutery_core::failure::FailureClass::RateLimit),
            1
        );
        state.set_config((*state.config()).clone());
        assert!(
            state
                .router()
                .model_rate_limit_remaining("p", "p-limited")
                .is_some(),
            "config refresh is not recovery evidence"
        );
        gateway.stop().await;
        task.abort();
    }
}

#[tokio::test]
async fn a_limited_key_leaves_its_sibling_available_for_the_same_model_and_future_requests() {
    let (state, gateway, mock, task) = fixture("chat", true).await;
    for _ in 0..2 {
        let response = ask(&gateway, "standard-class", false).await;
        assert_eq!(response.status(), 200);
        let _ = response.text().await.unwrap();
    }
    let keys: Vec<_> = mock
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|(_, key)| key.clone())
        .collect();
    assert_eq!(
        keys,
        [
            "Bearer fixture-limited",
            "Bearer fixture-healthy",
            "Bearer fixture-healthy"
        ]
    );
    assert!(state.router().is_key_gated("p", "account", "first"));
    assert!(!state.router().is_key_gated("p", "account", "second"));
    assert!(!state.router().is_circuit_open("p-healthy"));
    assert!(state
        .router()
        .model_rate_limit_remaining("p", "p-healthy")
        .is_none());
    gateway.stop().await;
    task.abort();
}

#[tokio::test]
async fn staged_codex_429_uses_the_same_scope_and_does_not_spend_twenty_replays() {
    let (state, gateway, mock, task) = fixture("responses", false).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for _ in 0..2 {
        let response = client
            .post(format!("http://{}/v1/responses", gateway.addr))
            .json(&json!({"model":"p-limited","input":"local fixture"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 429);
        let _ = response.text().await.unwrap();
    }
    assert_eq!(mock.0.lock().unwrap().len(), 1);
    assert!(!state.router().is_circuit_open("p-limited"));
    gateway.stop().await;
    task.abort();
}

#[tokio::test]
async fn an_explicit_key_cooldown_is_429_not_an_empty_model_503() {
    for surface in ["chat", "responses"] {
        for stream in [false, true] {
            let (state, gateway, mock, task) = fixture(surface, true).await;
            let mut config = (*state.config()).clone();
            config.providers[0].accounts[0].inference_keys.truncate(1);
            state.set_config(config);
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let request = if surface == "responses" {
                json!({"model":"p-healthy","stream":stream,"input":"local fixture"})
            } else {
                json!({"model":"p-healthy","stream":stream,"messages":[{"role":"user","content":"local fixture"}]})
            };
            let path = if surface == "responses" {
                "responses"
            } else {
                "chat/completions"
            };
            for index in 0..2 {
                let response = client
                    .post(format!("http://{}/v1/{path}", gateway.addr))
                    .json(&request)
                    .send()
                    .await
                    .unwrap();
                if surface == "responses" && stream && index == 0 {
                    let _ = response.text().await.unwrap();
                } else {
                    assert_eq!(
                        response.status(),
                        429,
                        "all keys cooling is not no model available"
                    );
                    assert!(
                        response.headers()["retry-after"]
                            .to_str()
                            .unwrap()
                            .parse::<u64>()
                            .unwrap()
                            >= 119
                    );
                    let body: Value = response.json().await.unwrap();
                    assert_eq!(body["error"]["code"], "rate_limit_error");
                }
            }
            assert_eq!(mock.0.lock().unwrap().len(), 1);
            assert!(!state.router().is_circuit_open("p-healthy"));
            gateway.stop().await;
            task.abort();
        }
    }
}
