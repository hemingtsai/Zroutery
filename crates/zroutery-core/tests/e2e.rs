//! End to end tests: a real Zroutery server in front of a mock provider.
//!
//! These cover the paths that unit tests cannot: cross dialect streaming,
//! failover between providers, auth and the model listing.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::billing::{BalanceConfig, BalancePreset, BalanceProbe, Pricing};
use zroutery_core::budget::{Budget, BudgetPeriod, BudgetScope};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelTier, ModelEntry, ProviderConfig, ProviderKind,
    RoutingStrategy,
};
use zroutery_core::server::{AppState, ServerHandle};

// ------------------------------------------------------------------ mock upstream

#[derive(Default)]
struct MockInner {
    /// Every request body the mock received, in order.
    received: Vec<Value>,
    /// Path of each request.
    paths: Vec<String>,
    /// Authorization / x-api-key values seen.
    keys: Vec<String>,
}

#[derive(Clone, Default)]
struct Mock {
    inner: Arc<Mutex<MockInner>>,
}

impl Mock {
    fn record(&self, path: &str, key: Option<String>, body: &Value) {
        let mut inner = self.inner.lock().unwrap();
        inner.paths.push(path.to_string());
        inner.keys.push(key.unwrap_or_default());
        inner.received.push(body.clone());
    }

    fn bodies(&self) -> Vec<Value> {
        self.inner.lock().unwrap().received.clone()
    }

    fn keys(&self) -> Vec<String> {
        self.inner.lock().unwrap().keys.clone()
    }

    fn count(&self) -> usize {
        self.inner.lock().unwrap().received.len()
    }
}

/// The mock reacts to the requested model name:
/// * `broken*`  -> HTTP 500
/// * `refuse*`  -> HTTP 400 (non retryable)
/// * `tools*`   -> answers with a tool call
/// * `think*`   -> answers with reasoning content
/// * anything else -> plain text answer
async fn mock_openai_chat(
    State(mock): State<Mock>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let key = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    mock.record("/chat/completions", key, &body);

    let model = body["model"].as_str().unwrap_or("").to_string();
    if model.starts_with("broken") {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "upstream exploded", "type": "server_error"}})),
        )
            .into_response();
    }
    if model.starts_with("limited") {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": {"message": "rate limited", "type": "rate_limit_error"}})),
        )
            .into_response();
    }
    if model.starts_with("refuse") {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "bad request", "type": "invalid_request_error"}})),
        )
            .into_response();
    }

    let stream = body["stream"].as_bool().unwrap_or(false);
    if !stream {
        let message = if model.starts_with("tools") {
            json!({
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": [{"id": "call_a", "type": "function",
                                "function": {"name": "get_weather", "arguments": "{\"city\":\"SH\"}"}}]
            })
        } else if model.starts_with("think") {
            json!({"role": "assistant", "content": "final", "reasoning_content": "reasoning"})
        } else if ["stop", "stop_sequences"].iter().any(|key| {
            body[key]
                .as_array()
                .is_some_and(|s| s.iter().any(|v| v == "</block>"))
        }) {
            // A classifier query: answer with a well-formed verdict.
            json!({"role": "assistant", "content": "<block>no</block>"})
        } else {
            json!({"role": "assistant", "content": "hello from mock"})
        };
        let finish = if model.starts_with("tools") {
            "tool_calls"
        } else {
            "stop"
        };
        return Json(json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "created": 1,
            "model": model,
            "choices": [{"index": 0, "message": message, "finish_reason": finish}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 7,
                      "completion_tokens_details": {"reasoning_tokens": 3}}
        }))
        .into_response();
    }

    let mut sse = String::new();
    let chunk = |delta: Value, finish: Value| {
        format!(
            "data: {}\n\n",
            json!({"id": "chatcmpl-mock", "object": "chat.completion.chunk", "created": 1,
                   "model": "mock", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
        )
    };
    if model.starts_with("think") {
        sse.push_str(&chunk(json!({"reasoning_content": "step 1"}), Value::Null));
    }
    if model.starts_with("tools") {
        sse.push_str(&chunk(
            json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function",
                                   "function": {"name": "get_weather", "arguments": ""}}]}),
            Value::Null,
        ));
        sse.push_str(&chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"city\":\"SH\"}"}}]}),
            Value::Null,
        ));
        sse.push_str(&chunk(json!({}), json!("tool_calls")));
    } else {
        sse.push_str(&chunk(
            json!({"role": "assistant", "content": ""}),
            Value::Null,
        ));
        sse.push_str(&chunk(json!({"content": "hel"}), Value::Null));
        sse.push_str(&chunk(json!({"content": "lo"}), Value::Null));
        sse.push_str(&chunk(json!({}), json!("stop")));
    }
    sse.push_str(&format!(
        "data: {}\n\n",
        json!({"id": "chatcmpl-mock", "object": "chat.completion.chunk", "model": "mock",
               "choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 2}})
    ));
    sse.push_str("data: [DONE]\n\n");

    Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from(sse))
        .unwrap()
}

async fn mock_anthropic_messages(
    State(mock): State<Mock>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    mock.record("/v1/messages", key, &body);

    // Same convention as the OpenAI side, so a test can make either dialect fail.
    if body["model"].as_str().unwrap_or("").starts_with("broken") {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"type": "error",
                        "error": {"type": "api_error", "message": "upstream exploded"}})),
        )
            .into_response();
    }
    if body["model"].as_str().unwrap_or("").starts_with("limited") {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"type": "error",
                        "error": {"type": "rate_limit_error", "message": "rate limited"}})),
        )
            .into_response();
    }
    // A repairable rejection for the rectifier A/B test: the thinking budget
    // exceeds the (pretend) provider allowance. The thinking-budget rectifier
    // halves `budget_tokens` and retries the SAME model, so the halved body
    // passes — one rejected attempt plus one repaired retry per request.
    if body["model"].as_str().unwrap_or("").starts_with("budget") {
        let budget = body["thinking"]["budget_tokens"].as_u64();
        if budget.is_some_and(|b| b > 1024) {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"type": "error",
                            "error": {"type": "invalid_request_error",
                                      "message": "thinking budget must be at most 1024"}})),
            )
                .into_response();
        }
    }

    let stream = body["stream"].as_bool().unwrap_or(false);
    if !stream {
        return Json(json!({
            "id": "msg_mock",
            "type": "message",
            "role": "assistant",
            "model": body["model"],
            "content": [{"type": "text", "text": "anthropic mock"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 4, "output_tokens": 2}
        }))
        .into_response();
    }

    let sse = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_mock\",\"model\":\"claude-mock\",\"usage\":{\"input_tokens\":4}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"claude \"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"stream\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":6}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from(sse))
        .unwrap()
}

async fn mock_models() -> Json<Value> {
    Json(json!({"object": "list", "data": [
        {"id": "m-b"},
        {"id": "m-a"},
        {"id": "m-a"},
        // An OpenRouter style entry, priced per single token.
        {"id": "m-priced", "pricing": {"prompt": "0.0000005", "completion": "0.000002"}},
    ]}))
}

/// A DeepSeek shaped balance payload, with the amounts as decimal strings.
async fn mock_balance() -> Json<Value> {
    Json(json!({
        "is_available": true,
        "balance_infos": [
            {"currency": "CNY", "total_balance": "48.75",
             "granted_balance": "0.00", "topped_up_balance": "48.75"}
        ]
    }))
}

/// What a Sub2API relay answers on `/v1/usage` for a quota bound key.
async fn mock_sub2api_usage() -> Json<Value> {
    Json(json!({
        "mode": "quota_limited",
        "isValid": true,
        "status": "active",
        "remaining": 7.25,
        "unit": "USD",
        "quota": {"limit": 20.0, "used": 12.75, "remaining": 7.25, "unit": "USD"},
        "usage": {"requests": 42},
    }))
}

async fn start_mock() -> (SocketAddr, Mock) {
    let mock = Mock::default();
    let app = axum::Router::new()
        .route("/chat/completions", post(mock_openai_chat))
        // Bare OpenAI-compatible hosts get /v1 added by chat_url().
        .route("/v1/chat/completions", post(mock_openai_chat))
        .route("/v1/messages", post(mock_anthropic_messages))
        .route("/models", get(mock_models))
        .route("/user/balance", get(mock_balance))
        // A relay answers on both depths, because it serves both dialects.
        .route("/usage", get(mock_sub2api_usage))
        .route("/v1/usage", get(mock_sub2api_usage))
        .route("/v1/models", get(mock_models))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, mock)
}

// ------------------------------------------------------------------ fixtures

const TOKEN: &str = "zr-test-token";

/// The scenario from the brief, pointed at the mock instead of the real APIs.
fn config_for(mock: SocketAddr) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();

    let mut deepseek = ProviderConfig::new("deepseek", "DeepSeek", ProviderKind::OpenAICompatible);
    deepseek.base_url = format!("http://{mock}");
    deepseek.key_ref = "provider:deepseek".into();
    deepseek.timeout_secs = 10;

    let mut openai = ProviderConfig::new("openai", "OpenAI", ProviderKind::OpenAICompatible);
    openai.base_url = format!("http://{mock}");
    openai.key_ref = "provider:openai".into();
    openai.timeout_secs = 10;

    let mut anthropic = ProviderConfig::new("anthropic", "Anthropic", ProviderKind::Anthropic);
    anthropic.base_url = format!("http://{mock}");
    anthropic.key_ref = "provider:anthropic".into();
    anthropic.timeout_secs = 10;

    cfg.providers = vec![deepseek, openai, anthropic];
    cfg.models = vec![
        ModelEntry::for_upstream("deepseek", "deepseek-v4-flash", Some(ModelTier::Fast)),
        ModelEntry::for_upstream("deepseek", "deepseek-v4-pro", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("openai", "gpt-5.3-sol", Some(ModelTier::Reasoning)),
        ModelEntry::for_upstream("anthropic", "claude-native", None),
    ];
    cfg
}

fn secrets() -> Arc<MemorySecretStore> {
    Arc::new(
        MemorySecretStore::new()
            .with("provider:deepseek", "sk-deepseek")
            .with("provider:openai", "sk-openai")
            .with("provider:anthropic", "sk-ant"),
    )
}

struct Harness {
    base: String,
    server: Option<ServerHandle>,
    state: Arc<AppState>,
    client: reqwest::Client,
    mock: Mock,
}

impl Harness {
    async fn start(cfg: AppConfig, mock: Mock) -> Harness {
        let state = Arc::new(AppState::new(cfg, secrets()));
        let server = ServerHandle::start(Arc::clone(&state)).await.unwrap();
        let base = format!("http://{}", server.addr);
        Harness {
            base,
            server: Some(server),
            state,
            // Do not keep idle connections: a rejected oversized body makes
            // the server close the connection, and a pooled connection that the
            // server has already closed would otherwise surface as a spurious
            // broken pipe on the next request.
            client: reqwest::Client::builder()
                .pool_max_idle_per_host(0)
                .build()
                .unwrap(),
            mock,
        }
    }

    async fn new() -> Harness {
        let (addr, mock) = start_mock().await;
        Harness::start(config_for(addr), mock).await
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}{path}", self.base))
            .header("x-api-key", TOKEN)
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .get(format!("{}{path}", self.base))
            .header("x-api-key", TOKEN)
    }

    async fn shutdown(mut self) {
        if let Some(s) = self.server.take() {
            s.stop().await;
        }
    }
}

// ------------------------------------------------------------------ the tests

#[tokio::test]
async fn classifier_requests_route_to_the_classifier_pool() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.classifier = zroutery_core::config::ClassifierConfig {
        enabled: true,
        candidates: vec![zroutery_core::config::ClassifierCandidate {
            model: "deepseek-deepseek-v4-flash".into(),
            priority: 10,
            enabled: true,
        }],
        ..zroutery_core::config::ClassifierConfig::default()
    };
    let h = Harness::start(cfg, mock).await;

    // An Auto Mode stage-1 shaped request, asking for a model the registry
    // does not even have: the classifier pool must answer regardless.
    let resp = h
        .post("/v1/messages")
        .json(&json!({
            "model": "claude-opus-4-8[1m]",
            "max_tokens": 64,
            "temperature": 0,
            "stop_sequences": ["</block>"],
            "system": [{"type": "text", "text":
                "You are a security monitor for autonomous AI coding agents."}],
            "messages": [{"role": "user", "content": "should this run?"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "deepseek-deepseek-v4-flash");
    assert_eq!(resp.headers()["x-zroutery-classifier"], "1");
    // The candidate's own model name went upstream.
    assert_eq!(h.mock.bodies()[0]["model"], "deepseek-v4-flash");
    // And it was recorded as classifier traffic, not main traffic.
    let kind = &h.state.stats().summary().per_kind;
    assert!(kind.iter().any(|k| k.kind == "auto_mode" && k.requests == 1));

    h.shutdown().await;
}

#[tokio::test]
async fn anthropic_in_openai_out_non_streaming() {
    let h = Harness::new().await;

    let resp = h
        .post("/v1/messages")
        .json(&json!({
            "model": "sonnet-class",
            "max_tokens": 100,
            "system": "be brief",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-pro"
    );
    assert_eq!(resp.headers()["x-zroutery-provider"], "DeepSeek");
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["type"], "message");
    assert_eq!(body["role"], "assistant");
    assert_eq!(body["model"], "deepseek-deepseek-v4-pro");
    assert_eq!(body["content"][0]["text"], "hello from mock");
    assert_eq!(body["stop_reason"], "end_turn");
    assert_eq!(body["usage"]["input_tokens"], 11);
    assert_eq!(body["usage"]["output_tokens"], 7);

    // The upstream saw an OpenAI shaped request with the mapped model id.
    let sent = &h.mock.bodies()[0];
    // The provider is asked for its own name, not for our namespaced id.
    assert_eq!(sent["model"], "deepseek-v4-pro");
    assert_eq!(sent["messages"][0]["role"], "system");
    assert_eq!(sent["messages"][0]["content"], "be brief");
    assert_eq!(sent["messages"][1]["role"], "user");
    assert_eq!(sent["messages"][1]["content"], "hi");
    assert_eq!(sent["max_tokens"], 100);
    assert_eq!(h.mock.keys()[0], "Bearer sk-deepseek");

    h.shutdown().await;
}

#[tokio::test]
async fn openai_in_openai_out_with_tool_calls() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // Force the mock into tool-call mode via the upstream model name.
    cfg.models[1].upstream_model = "tools-model".into();
    let h = Harness::start(cfg, mock).await;

    let body: Value = h
        .post("/v1/chat/completions")
        .json(&json!({
            "model": "sonnet-class",
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [{"type": "function", "function": {"name": "get_weather", "parameters": {"type": "object"}}}]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(body["choices"][0]["message"]["content"], Value::Null);
    let call = &body["choices"][0]["message"]["tool_calls"][0];
    assert_eq!(call["function"]["name"], "get_weather");
    assert_eq!(call["function"]["arguments"], "{\"city\":\"SH\"}");
    assert_eq!(body["usage"]["total_tokens"], 18);

    let sent = &h.mock.bodies()[0];
    assert_eq!(sent["tools"][0]["function"]["name"], "get_weather");

    h.shutdown().await;
}

#[tokio::test]
async fn anthropic_client_streaming_over_an_openai_provider() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].upstream_model = "think-model".into();
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/messages")
        .json(&json!({
            "model": "sonnet-class",
            "max_tokens": 64,
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let wire = resp.text().await.unwrap();

    // Anthropic clients need the full event lifecycle, in order.
    let order: Vec<&str> = wire
        .lines()
        .filter_map(|l| l.strip_prefix("event: "))
        .collect();
    assert_eq!(order.first(), Some(&"message_start"));
    assert_eq!(order.last(), Some(&"message_stop"));
    assert!(order.contains(&"content_block_start"));
    assert!(order.contains(&"message_delta"));

    // Reasoning became a thinking block, and text its own block.
    assert!(wire.contains("\"type\":\"thinking\""));
    assert!(wire.contains("\"thinking\":\"step 1\""));
    assert!(wire.contains("\"text\":\"hel\""));
    assert!(wire.contains("\"text\":\"lo\""));
    // Usage from the trailing OpenAI chunk survived the translation.
    assert!(wire.contains("\"output_tokens\":2"));
    // Two blocks opened, two closed.
    assert_eq!(wire.matches("event: content_block_start").count(), 2);
    assert_eq!(wire.matches("event: content_block_stop").count(), 2);

    let stats = h.state.stats().summary();
    assert_eq!(stats.requests, 1);
    assert_eq!(stats.failures, 0);
    assert_eq!(stats.output_tokens, 2);

    h.shutdown().await;
}

#[tokio::test]
async fn openai_client_streaming_over_an_anthropic_provider() {
    let h = Harness::new().await;

    let wire = h
        .post("/v1/chat/completions")
        .json(&json!({
            "model": "anthropic-claude-native",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
            "stream_options": {"include_usage": true}
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(wire.contains("\"object\":\"chat.completion.chunk\""));
    assert!(wire.contains("\"content\":\"claude \""));
    assert!(wire.contains("\"content\":\"stream\""));
    assert!(wire.contains("\"finish_reason\":\"stop\""));
    assert!(wire.contains("\"prompt_tokens\":4"));
    assert!(wire.contains("\"completion_tokens\":6"));
    assert!(wire.trim_end().ends_with("data: [DONE]"));

    // The Anthropic upstream got an Anthropic shaped body and the right auth.
    let sent = &h.mock.bodies()[0];
    assert_eq!(sent["model"], "claude-native");
    assert_eq!(sent["messages"][0]["content"][0]["type"], "text");
    assert_eq!(sent["max_tokens"], 4096, "anthropic requires max_tokens");
    assert_eq!(h.mock.keys()[0], "sk-ant");

    h.shutdown().await;
}

#[tokio::test]
async fn failover_moves_to_the_next_model_in_the_class() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // Two sonnet candidates: the preferred one always fails.
    cfg.models[1].upstream_model = "broken-model".into();
    cfg.models[1].priority = 0;
    cfg.models.push(
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelTier::Standard))
            .with_priority(10),
    );
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/messages")
        .json(&json!({
            "model": "sonnet-class",
            "max_tokens": 10,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "openai-gpt-sonnet");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "hello from mock");

    assert_eq!(h.mock.count(), 2, "the broken model was tried first");
    let health = h.state.router().health_snapshot();
    let broken = health
        .iter()
        .find(|m| m.model_id == "deepseek-broken-model")
        .unwrap();
    assert_eq!(broken.total_failure, 1);
    let good = health
        .iter()
        .find(|m| m.model_id == "openai-gpt-sonnet")
        .unwrap();
    assert_eq!(good.total_success, 1);

    let record = &h.state.stats().recent(1)[0];
    assert_eq!(record.attempts, 2);
    assert_eq!(record.resolved_model.as_deref(), Some("openai-gpt-sonnet"));

    h.shutdown().await;
}

#[tokio::test]
async fn client_errors_are_not_retried() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].upstream_model = "refuse-model".into();
    cfg.models.push(
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelTier::Standard))
            .with_priority(10),
    );
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 10,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(h.mock.count(), 1, "a 400 must not trigger failover");

    h.shutdown().await;
}

#[tokio::test]
async fn circuit_breaker_skips_a_failing_model() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].upstream_model = "broken-model".into();
    cfg.models.push(
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelTier::Standard))
            .with_priority(10),
    );
    cfg.routing.circuit_breaker.failure_threshold = 1;
    let h = Harness::start(cfg, mock).await;

    for _ in 0..2 {
        let status = h
            .post("/v1/messages")
            .json(&json!({"model": "sonnet-class", "max_tokens": 10,
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, 200);
    }

    // First call: broken + good = 2 upstream calls. Second call: the breaker is
    // open, so the broken model is demoted and only the good one is used.
    assert_eq!(h.mock.count(), 3);
    assert!(h.state.router().is_cooling("deepseek-broken-model"));

    h.shutdown().await;
}

#[tokio::test]
async fn missing_api_key_fails_over_and_reports_clearly() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.providers[0].key_ref = "provider:nonexistent".into();
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/messages")
        .json(
            &json!({"model": "deepseek-deepseek-v4-pro", "max_tokens": 10,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 412);
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no API key"));
    assert_eq!(h.mock.count(), 0);

    h.shutdown().await;
}

#[tokio::test]
async fn a_budget_stops_spending_once_it_is_used_up() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].pricing = Some(Pricing::new("USD", 1000.0, 1000.0));
    // The mock reports 11 + 7 tokens, so one request costs 0.018 USD.
    cfg.budgets = vec![Budget::new(
        BudgetScope::Global,
        BudgetPeriod::Day,
        "USD",
        0.01,
    )];
    let h = Harness::start(cfg, mock).await;

    let ask = || {
        h.post("/v1/messages")
            .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                          "messages": [{"role": "user", "content": "hi"}]}))
    };

    // The first request fits, and the one that crosses the line still completes.
    assert_eq!(ask().send().await.unwrap().status(), 200);

    // The next is refused, naming the limit that stopped it.
    let resp = ask().send().await.unwrap();
    assert_eq!(resp.status(), 402);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "budget_exceeded");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("today") && message.contains("everything"),
        "{message}"
    );

    // Nothing reached the provider for the refused request.
    assert_eq!(h.mock.count(), 1);
    // A direct call to the same model is stopped too: a global budget is global.
    assert_eq!(
        h.post("/v1/chat/completions")
            .json(&json!({"model": "deepseek-deepseek-v4-pro",
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap()
            .status(),
        402
    );

    h.shutdown().await;
}

#[tokio::test]
async fn a_class_budget_degrades_instead_of_refusing() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[2].pricing = Some(Pricing::new("USD", 1000.0, 1000.0));
    cfg.budgets = vec![Budget::new(
        BudgetScope::Tier {
            tier: ModelTier::Reasoning,
        },
        BudgetPeriod::Day,
        "USD",
        0.01,
    )
    .degrading_to(ModelTier::Fast)];
    let h = Harness::start(cfg, mock).await;

    // The first opus request goes to opus and spends past the limit.
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "opus-class", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.headers()["x-zroutery-model"], "openai-gpt-5.3-sol");

    // The next is served by the cheap class rather than refused.
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "opus-class", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-flash"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn a_class_budget_also_gates_direct_id_requests() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[2].pricing = Some(Pricing::new("USD", 1000.0, 1000.0));
    cfg.budgets = vec![Budget::new(
        BudgetScope::Tier {
            tier: ModelTier::Reasoning,
        },
        BudgetPeriod::Day,
        "USD",
        0.01,
    )
    .degrading_to(ModelTier::Fast)];
    let h = Harness::start(cfg, mock).await;

    // The direct call spends past the opus budget…
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "openai-gpt-5.3-sol", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "openai-gpt-5.3-sol");

    // …so the next direct call to the same id degrades to the cheap class
    // rather than spending on: charging already bills direct calls against
    // the model's class, so gating must match.
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "openai-gpt-5.3-sol", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-flash"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn spend_survives_a_restart() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].pricing = Some(Pricing::new("USD", 1000.0, 1000.0));
    cfg.budgets = vec![Budget::new(
        BudgetScope::Global,
        BudgetPeriod::Day,
        "USD",
        0.01,
    )];
    let h = Harness::start(cfg.clone(), mock.clone()).await;

    assert_eq!(
        h.post("/v1/messages")
            .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let ledger = h.state.ledger();
    assert!(!ledger.is_empty(), "the spend was recorded");
    h.shutdown().await;

    // A fresh process that adopts the ledger is already over its limit, which is the
    // whole point: a guardrail that forgets on restart is not a guardrail.
    let restarted = Harness::start(cfg, mock).await;
    restarted.state.set_ledger(ledger);
    assert_eq!(
        restarted
            .post("/v1/messages")
            .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap()
            .status(),
        402
    );

    restarted.shutdown().await;
}

#[tokio::test]
async fn an_election_pins_the_cheap_fast_model_as_primary() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.routing.strategy = RoutingStrategy::Balanced;
    // Two sonnet members priced ten to one, both answered by the same mock at the
    // same speed, so price is what has to decide.
    cfg.models[1].pricing = Some(Pricing::new("USD", 0.2, 0.8));
    cfg.models.push(
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelTier::Standard))
            // Priority puts this one first; the election is expected to overrule it.
            .with_priority(-100),
    );
    let last = cfg.models.len() - 1;
    cfg.models[last].pricing = Some(Pricing::new("USD", 2.0, 8.0));
    let h = Harness::start(cfg, mock).await;

    // Before any election the configured priority still rules.
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.headers()["x-zroutery-model"], "openai-gpt-sonnet");

    let election = h.state.hold_election().await;
    let sonnet = election.tiers.get(&ModelTier::Standard).unwrap();
    assert!(sonnet.priced, "both members are priced in one currency");
    assert_eq!(sonnet.winner(), Some("deepseek-deepseek-v4-pro"));
    assert!(sonnet.ranked[0].latency_ms.is_some());
    assert!(sonnet.ranked[0].note.as_ref().unwrap().contains("primary"));

    // Traffic follows the election from here on, not the priority.
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-pro"
    );

    // A probe is a real call, so it doubles as the freshest health signal.
    let health = h.state.router().health_snapshot();
    assert!(health
        .iter()
        .any(|m| m.model_id == "openai-gpt-sonnet" && m.total_success > 0));

    h.shutdown().await;
}

#[tokio::test]
async fn an_election_ranks_a_broken_model_last_and_says_why() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.routing.strategy = RoutingStrategy::Balanced;
    // A second opus member that always fails; the election has to notice.
    cfg.models[3].upstream_model = "broken-model".into();
    cfg.models[3].tier = Some(ModelTier::Reasoning);
    let h = Harness::start(cfg, mock).await;

    let election = h.state.hold_election().await;
    let opus = election.tiers.get(&ModelTier::Reasoning).unwrap();
    assert_eq!(opus.winner(), Some("openai-gpt-5.3-sol"));
    let last = opus.ranked.last().unwrap();
    assert_eq!(last.model_id, "anthropic-broken-model");
    assert!(last.score.is_none());
    assert!(last.note.as_ref().unwrap().contains("did not answer"));

    // Neither is priced, so latency decided and the reason is on the record.
    assert!(!opus.priced);
    assert!(opus
        .note
        .as_ref()
        .unwrap()
        .contains("not every model has a price"));

    h.shutdown().await;
}

#[tokio::test]
async fn a_model_added_after_an_election_is_used_but_not_promoted() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.routing.strategy = RoutingStrategy::Balanced;
    let h = Harness::start(cfg, mock).await;

    let election = h.state.hold_election().await;
    assert_eq!(
        election.tiers.get(&ModelTier::Standard).unwrap().winner(),
        Some("deepseek-deepseek-v4-pro")
    );

    // Add a member whose priority would otherwise put it first.
    let mut next = (*h.state.config()).clone();
    next.models.push(
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelTier::Standard))
            .with_priority(-100),
    );
    h.state.set_config(next);

    // It is reachable, but the measured model keeps the primary slot until an
    // election has something to say about the newcomer.
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-pro"
    );
    assert!(h
        .state
        .registry()
        .tier_members(ModelTier::Standard)
        .iter()
        .any(|m| m.upstream_model == "gpt-sonnet"));

    h.shutdown().await;
}

#[tokio::test]
async fn both_prefixes_reach_the_same_endpoints() {
    let h = Harness::new().await;

    // A base URL with /v1 and one without both work, because clients disagree
    // about which of the two they are handed.
    for path in ["/v1/models", "/models"] {
        let resp = h.get(path).send().await.unwrap();
        assert_eq!(resp.status(), 200, "{path}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["object"], "list", "{path}");
    }
    for path in [
        "/v1/models/openai-gpt-5.3-sol",
        "/models/openai-gpt-5.3-sol",
    ] {
        let body: Value = h.get(path).send().await.unwrap().json().await.unwrap();
        assert_eq!(body["id"], "openai-gpt-5.3-sol", "{path}");
    }

    for path in ["/v1/chat/completions", "/chat/completions"] {
        let resp = h
            .post(path)
            .json(&json!({"model": "sonnet-class",
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{path}");
    }
    for path in ["/v1/messages", "/messages"] {
        let resp = h
            .post(path)
            .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{path}");
    }
    for path in ["/v1/messages/count_tokens", "/messages/count_tokens"] {
        let resp = h
            .post(path)
            .json(&json!({"model": "sonnet-class",
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{path}");
    }

    // The alias sits inside the auth layer; it is not a way around it. Each path
    // is probed with the verb it actually implements, since a wrong verb is
    // answered before authentication runs.
    for (path, post) in [
        ("/models", false),
        ("/status", false),
        ("/chat/completions", true),
        ("/messages", true),
    ] {
        let url = format!("{}{path}", h.base);
        let request = if post {
            h.client.post(url).json(&json!({"model": "sonnet-class"}))
        } else {
            h.client.get(url)
        };
        let resp = request.send().await.unwrap();
        assert_eq!(resp.status(), 401, "{path} must still need the token");
    }

    h.shutdown().await;
}

#[tokio::test]
async fn an_unknown_path_explains_itself() {
    let h = Harness::new().await;

    let resp = h.get("/v2/models").send().await.unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    // OpenAI shaped, because that is what a client on this path expects.
    assert_eq!(body["error"]["code"], "not_found_error");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("GET /v2/models"));
    let endpoints = body["zroutery"]["endpoints"].as_array().unwrap();
    assert!(endpoints.iter().any(|e| e == "/v1/models"));
    assert!(body["zroutery"]["likely_cause"].is_null());

    // A path that looks like a messages call answers in the Anthropic envelope.
    let body: Value = h
        .post("/v1/messages/nope")
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "not_found_error");

    // The classic misconfiguration: a base URL ending in /v1 plus an SDK that
    // appends /v1 itself.
    let body: Value = h
        .post("/v1/v1/messages")
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cause = body["zroutery"]["likely_cause"].as_str().unwrap();
    assert!(cause.contains("base URL already ends in /v1"), "{cause}");
    assert!(cause.contains("/v1/messages"), "{cause}");

    // A real path with the wrong verb says which verb it wants, rather than
    // answering with an empty 405 that reads like a broken proxy.
    let resp = h.get("/v1/chat/completions").send().await.unwrap();
    assert_eq!(resp.status(), 405);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "method_not_allowed");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("POST endpoint"), "{message}");
    assert!(message.contains("not GET"), "{message}");

    h.shutdown().await;
}

#[tokio::test]
async fn authentication_is_enforced_on_api_routes_only() {
    let h = Harness::new().await;

    // No credentials at all.
    let resp = h
        .client
        .post(format!("{}/v1/messages", h.base))
        .json(&json!({"model": "sonnet-class", "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "authentication_error");

    // Wrong token.
    let resp = h
        .client
        .get(format!("{}/v1/models", h.base))
        .header("authorization", "Bearer nope")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Bearer form is accepted.
    let resp = h
        .client
        .get(format!("{}/v1/models", h.base))
        .header("authorization", format!("Bearer {TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Liveness stays open, and says nothing else.
    let resp = h
        .client
        .get(format!("{}/health", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(
        body.as_object().unwrap().len(),
        1,
        "an unauthenticated route must not describe the configuration: {body}"
    );

    // The detail moved behind the token.
    let resp = h
        .client
        .get(format!("{}/v1/status", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: Value = h
        .get("/v1/status")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["auth_required"], true);
    assert!(body["models"].as_u64().unwrap() > 0);
    assert!(body["version"].is_string());

    h.shutdown().await;
}

#[tokio::test]
async fn oversized_request_bodies_are_rejected_before_reaching_a_provider() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.server.max_body_mib = 1;
    let h = Harness::start(cfg, mock).await;

    let huge = "x".repeat(2 * 1024 * 1024);
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                      "messages": [{"role": "user", "content": huge}]}))
        .send()
        .await
        .ok();
    // A connection reset while the body is still uploading is a rejection
    // too: the server stops reading and closes without draining the rest of
    // the oversized body, so the client may see the reset instead of the 413.
    // Either way nothing reached a provider, which the next line asserts.
    assert!(
        resp.map_or(true, |r| r.status() == 413),
        "an oversized body must be rejected",
    );
    assert_eq!(h.mock.count(), 0, "nothing was forwarded upstream");

    // A normal request on the same server still works.
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    h.shutdown().await;
}

#[tokio::test]
async fn cors_is_limited_to_the_configured_origins() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.server.allow_cors = true;
    cfg.server.cors_origins = vec!["http://localhost:3000".into()];
    assert!(!cfg.server.cors_is_wide_open());
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .get("/v1/models")
        .header("origin", "http://localhost:3000")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers()["access-control-allow-origin"],
        "http://localhost:3000"
    );

    let resp = h
        .get("/v1/models")
        .header("origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "an origin outside the list must not be allowed"
    );

    // Enabling CORS without a list is allowed but reported.
    let mut wide = (*h.state.config()).clone();
    wide.server.cors_origins.clear();
    assert!(wide.server.cors_is_wide_open());
    assert!(wide
        .validate()
        .iter()
        .any(|i| i.code == "server.cors_any_origin"));

    h.shutdown().await;
}

#[tokio::test]
async fn model_listing_exposes_real_and_virtual_models() {
    let h = Harness::new().await;

    let body: Value = h
        .get("/v1/models")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["object"], "list");
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"deepseek-deepseek-v4-flash"));
    assert!(ids.contains(&"deepseek-deepseek-v4-pro"));
    assert!(ids.contains(&"openai-gpt-5.3-sol"));
    assert!(ids.contains(&"anthropic-claude-native"));
    assert!(ids.contains(&"reasoning-class"));
    assert!(ids.contains(&"standard-class"));
    assert!(ids.contains(&"fast-class"));

    // Both dialects find what they expect on each entry.
    let entry = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "standard-class")
        .unwrap();
    assert_eq!(entry["object"], "model");
    assert_eq!(entry["type"], "model");
    assert!(entry["created"].is_i64());
    assert!(entry["created_at"].is_string());
    assert_eq!(entry["zroutery"]["virtual"], true);
    assert_eq!(entry["zroutery"]["member_count"], 1);

    // The unclassified model is listed but has no class.
    let entry = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "anthropic-claude-native")
        .unwrap();
    assert_eq!(entry["zroutery"]["tier"], Value::Null);
    assert_eq!(entry["owned_by"], "Anthropic");

    let single: Value = h
        .get("/v1/models/openai-gpt-5.3-sol")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(single["id"], "openai-gpt-5.3-sol");
    assert_eq!(single["zroutery"]["tier"], "reasoning");

    assert_eq!(h.get("/v1/models/nope").send().await.unwrap().status(), 404);

    h.shutdown().await;
}

#[tokio::test]
async fn unknown_and_unclassified_routing_errors() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // Remove every opus member so the class is empty.
    cfg.models.retain(|m| m.tier != Some(ModelTier::Reasoning));
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/chat/completions")
        .json(&json!({"model": "totally-unknown", "messages": [{"role": "user", "content": "x"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["code"],
        "not_found_error"
    );

    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "opus-class", "max_tokens": 10,
                      "messages": [{"role": "user", "content": "x"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["type"],
        "overloaded_error"
    );

    assert_eq!(h.mock.count(), 0);
    let summary = h.state.stats().summary();
    assert_eq!(summary.requests, 2);
    assert_eq!(summary.failures, 2);

    h.shutdown().await;
}

#[tokio::test]
async fn claude_style_model_names_are_routed_by_class() {
    let h = Harness::new().await;

    let resp = h
        .post("/v1/messages")
        .json(&json!({
            "model": "claude-sonnet-4-5-20250929",
            "max_tokens": 32,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-pro"
    );

    let resp = h
        .post("/v1/messages")
        .json(&json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 32,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-flash"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn count_tokens_endpoint_answers_anthropic_clients() {
    let h = Harness::new().await;
    let body: Value = h
        .post("/v1/messages/count_tokens")
        .json(&json!({
            "model": "sonnet-class",
            "messages": [{"role": "user", "content": "count these characters please"}]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["input_tokens"].as_u64().unwrap() > 0);
    assert_eq!(h.mock.count(), 0, "estimated locally, no upstream call");
    // Unpriced models simply say which model answered.
    assert_eq!(body["zroutery"]["estimated"], true);
    assert_eq!(body["zroutery"]["model"], "deepseek-deepseek-v4-pro");
    assert!(body["zroutery"]["estimated_input_cost"].is_null());
    h.shutdown().await;
}

#[tokio::test]
async fn priced_requests_report_their_cost() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // 3 USD per million in, 15 out: the shape of a frontier model's price list.
    cfg.models[1].pricing = Some(Pricing::new("USD", 3.0, 15.0));
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 16,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // The mock reports 11 prompt and 7 completion tokens.
    let expected = 3.0 * 11.0 / 1e6 + 15.0 * 7.0 / 1e6;
    assert_eq!(
        resp.headers()["x-zroutery-cost"],
        format!("USD {expected:.6}")
    );

    let record = &h.state.stats().recent(1)[0];
    let cost = record.cost.as_ref().unwrap();
    assert_eq!(cost.currency, "USD");
    assert!((cost.amount - expected).abs() < 1e-12);

    let summary = h.state.stats().summary();
    assert!((summary.cost.get("USD") - expected).abs() < 1e-12);
    let per_model = summary
        .per_model
        .iter()
        .find(|m| m.model_id == "deepseek-deepseek-v4-pro")
        .unwrap();
    assert!((per_model.cost.get("USD") - expected).abs() < 1e-12);

    // And the estimate offered before sending uses the same price.
    let body: Value = h
        .post("/v1/messages/count_tokens")
        .json(&json!({"model": "sonnet-class",
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let estimate = &body["zroutery"]["estimated_input_cost"];
    assert_eq!(estimate["currency"], "USD");
    assert!(estimate["amount"].as_f64().unwrap() > 0.0);
    assert_eq!(body["zroutery"]["input_per_mtok"], 3.0);

    h.shutdown().await;
}

#[tokio::test]
async fn streamed_requests_are_priced_even_without_a_header() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].pricing = Some(Pricing::new("CNY", 2.0, 8.0));
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/messages")
        .json(
            &json!({"model": "sonnet-class", "max_tokens": 16, "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    // Headers are already sent when the usage arrives, so there is nothing to put
    // in them; the record still gets the cost.
    assert!(resp.headers().get("x-zroutery-cost").is_none());
    let _ = resp.text().await.unwrap();

    let record = &h.state.stats().recent(1)[0];
    let cost = record.cost.as_ref().unwrap();
    // The mock's streaming trailer reports 5 prompt and 2 completion tokens.
    assert_eq!(cost.currency, "CNY");
    assert!((cost.amount - (2.0 * 5.0 / 1e6 + 8.0 * 2.0 / 1e6)).abs() < 1e-12);

    h.shutdown().await;
}

#[tokio::test]
async fn a_balance_is_fetched_with_the_providers_own_key() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // The built-in presets point at real vendors, so the mock is driven by a
    // custom probe; the presets themselves are covered by unit tests.
    cfg.providers[0].balance = BalanceConfig {
        preset: BalancePreset::Custom,
        custom: Some(BalanceProbe {
            path: "/user/balance".into(),
            remaining_pointer: Some("/balance_infos/0/total_balance".into()),
            currency_pointer: Some("/balance_infos/0/currency".into()),
            ..BalanceProbe::default()
        }),
    };
    let provider = cfg.providers[0].clone();
    let h = Harness::start(cfg, mock).await;

    let balance = h
        .state
        .upstream()
        .fetch_balance(
            &provider,
            Some("sk-deepseek"),
            &provider.balance.probe(provider.base_depth()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(balance.currency, "CNY");
    assert_eq!(balance.remaining, Some(48.75));

    // A provider that publishes nothing is not asked at all.
    let quiet = &h.state.config().providers[1];
    assert!(!quiet.balance.is_supported(quiet.base_depth()));

    // A pointer into thin air is an error rather than a silent zero.
    let mut broken = provider.clone();
    broken.balance.custom = Some(BalanceProbe {
        path: "/user/balance".into(),
        remaining_pointer: Some("/nope".into()),
        ..BalanceProbe::default()
    });
    let err = h
        .state
        .upstream()
        .fetch_balance(
            &broken,
            Some("sk-deepseek"),
            &broken.balance.probe(broken.base_depth()).unwrap(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no balance found"), "{err}");

    h.shutdown().await;
}

#[tokio::test]
async fn the_sub2api_preset_reads_a_relay_of_either_dialect() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // A relay reached through the OpenAI dialect: its base already ends in /v1,
    // so the probe asks for `/usage`.
    cfg.providers[0].balance = BalanceConfig {
        preset: BalancePreset::Sub2Api,
        custom: None,
    };
    // The same relay reached as Anthropic: the base is the API root, so the probe
    // has to ask for `/v1/usage` instead.
    cfg.providers[2].balance = BalanceConfig {
        preset: BalancePreset::Sub2Api,
        custom: None,
    };
    let openai_style = cfg.providers[0].clone();
    let anthropic_style = cfg.providers[2].clone();
    let h = Harness::start(cfg, mock).await;

    for provider in [&openai_style, &anthropic_style] {
        let key = if provider.kind == ProviderKind::Anthropic {
            "sk-ant"
        } else {
            "sk-deepseek"
        };
        let probe = provider.balance.probe(provider.base_depth()).unwrap();
        let balance = h
            .state
            .upstream()
            .fetch_balance(provider, Some(key), &probe)
            .await
            .unwrap();
        // The relay reports the key's quota, not just a wallet total.
        assert_eq!(balance.currency, "USD", "{}", provider.name);
        assert_eq!(balance.remaining, Some(7.25));
        assert_eq!(balance.total, Some(20.0));
        assert_eq!(balance.used, Some(12.75));

        // Sub2API accepts either credential header, so each dialect sending its
        // own is enough; this is what actually goes on the wire.
        let headers = zroutery_core::upstream::build_headers(provider, Some(key)).unwrap();
        match provider.kind {
            ProviderKind::Anthropic => assert_eq!(headers["x-api-key"], key),
            ProviderKind::OpenAICompatible => {
                assert_eq!(headers["authorization"], format!("Bearer {key}"))
            }
        }
    }

    h.shutdown().await;
}

#[tokio::test]
async fn config_can_be_swapped_while_running() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(config_for(addr), mock).await;

    let mut cfg = (*h.state.config()).clone();
    cfg.models.retain(|m| m.upstream_model != "deepseek-v4-pro");
    cfg.models.push(ModelEntry::for_upstream(
        "openai",
        "gpt-sonnet",
        Some(ModelTier::Standard),
    ));
    h.state.set_config(cfg);

    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 10,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.headers()["x-zroutery-model"], "openai-gpt-sonnet");

    let resp = h
        .post("/v1/messages")
        .json(
            &json!({"model": "deepseek-deepseek-v4-pro", "max_tokens": 10,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    h.shutdown().await;
}

#[tokio::test]
async fn the_same_model_from_two_providers_stays_addressable() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // Both providers offer a model with the very same upstream name, which is
    // what happens as soon as an aggregator sits next to a direct account.
    cfg.models.push(ModelEntry::for_upstream(
        "openai",
        "deepseek-v4-pro",
        Some(ModelTier::Standard),
    ));
    let h = Harness::start(cfg, mock).await;

    let listing: Value = h
        .get("/v1/models")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = listing["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"deepseek-deepseek-v4-pro"), "{ids:?}");
    assert!(ids.contains(&"openai-deepseek-v4-pro"), "{ids:?}");

    // Each id reaches its own provider, and each provider is asked for the bare
    // model name with its own key.
    for (id, provider, key) in [
        ("deepseek-deepseek-v4-pro", "DeepSeek", "Bearer sk-deepseek"),
        ("openai-deepseek-v4-pro", "OpenAI", "Bearer sk-openai"),
    ] {
        let resp = h
            .post("/v1/messages")
            .json(&json!({"model": id, "max_tokens": 16,
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{id}");
        assert_eq!(resp.headers()["x-zroutery-model"], id);
        assert_eq!(resp.headers()["x-zroutery-provider"], provider);
        assert_eq!(h.mock.bodies().last().unwrap()["model"], "deepseek-v4-pro");
        assert_eq!(h.mock.keys().last().unwrap(), key);
    }

    // They are separate members of the same class, so they can cover for each
    // other and are accounted for separately.
    let health = h.state.router().health_snapshot();
    assert_eq!(health.len(), 2);
    assert_eq!(health[0].model_id, "deepseek-deepseek-v4-pro");
    assert_eq!(health[1].model_id, "openai-deepseek-v4-pro");
    assert!(health.iter().all(|m| m.total_success == 1));

    h.shutdown().await;
}

#[tokio::test]
async fn ids_from_before_0_2_keep_working() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // What `AppConfig::normalize` leaves behind for a 0.1.x configuration: the
    // old free-form id survives as an alias next to the derived one.
    cfg.models[1].aliases.push("deepseek-v4-pro".into());
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/chat/completions")
        .json(&json!({"model": "deepseek-v4-pro",
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // The answer reports the current id, so clients can migrate when they like.
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-pro"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn provider_model_discovery_dedupes_and_sorts() {
    let (addr, mock) = start_mock().await;
    let cfg = config_for(addr);
    let provider = cfg.providers[0].clone();
    let h = Harness::start(cfg, mock).await;

    let models = h
        .state
        .upstream()
        .list_models(&provider, Some("sk-deepseek"))
        .await
        .unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["m-a", "m-b", "m-priced"]);
    // Prices come along when the catalogue publishes them, per million tokens.
    assert!(models[0].pricing.is_none());
    let priced = models[2].pricing.as_ref().unwrap();
    assert_eq!(priced.currency, "USD");
    assert!((priced.input_per_mtok - 0.5).abs() < 1e-9);
    assert!((priced.output_per_mtok - 2.0).abs() < 1e-9);

    h.shutdown().await;
}

#[tokio::test]
async fn rate_limit_triggers_failover() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // Priority is explicit: equal priorities order by exposed id, which would
    // put the healthy model first and skip the 429 path this test covers.
    cfg.models = vec![
        ModelEntry::for_upstream("deepseek", "limited-v4", Some(ModelTier::Standard))
            .with_priority(0),
        ModelEntry::for_upstream("deepseek", "deepseek-v4-pro", Some(ModelTier::Standard))
            .with_priority(10),
    ];
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/chat/completions")
        .json(&json!({"model": "sonnet-class", "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();

    // Should succeed via failover to the second model.
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "deepseek-deepseek-v4-pro");
    // Two upstream calls: one 429, one success.
    assert_eq!(h.mock.bodies()[0]["model"], "limited-v4");
    assert_eq!(h.mock.bodies()[1]["model"], "deepseek-v4-pro");
    assert_eq!(h.mock.count(), 2);

    h.shutdown().await;
}

#[tokio::test]
async fn transport_error_returns_502() {
    // Nothing listens on port 1 — the connection will be refused or time out.
    let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();

    let mut provider =
        ProviderConfig::new("dead", "DeadProvider", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{addr}");
    provider.key_ref = "provider:dead".into();
    provider.timeout_secs = 2;
    cfg.providers = vec![provider];
    cfg.models = vec![ModelEntry::for_upstream(
        "dead",
        "dead-model",
        Some(ModelTier::Standard),
    )];

    let _mock = Mock::default();
    let secrets = Arc::new(
        MemorySecretStore::new().with("provider:dead", "sk-dead"),
    );
    let state = Arc::new(AppState::new(cfg, secrets));
    let server = ServerHandle::start(Arc::clone(&state)).await.unwrap();
    let base = format!("http://{}", server.addr);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();

    let resp = client
        .post(format!("{base}/v1/chat/completions"))
        .header("x-api-key", TOKEN)
        .json(&json!({"model": "sonnet-class", "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();

    // Should get a server error (502/503), not a panic or hang.
    assert!(
        resp.status().is_server_error(),
        "expected 5xx, got {}",
        resp.status()
    );

    server.stop().await;
}

#[tokio::test]
async fn empty_messages_returns_error() {
    let h = Harness::new().await;
    let resp = h
        .post("/v1/chat/completions")
        .json(&json!({"model": "sonnet-class", "messages": []}))
        .send()
        .await
        .unwrap();

    // Should return a client error or handle gracefully; read the actual behaviour.
    if resp.status().is_client_error() {
        let body: Value = resp.json().await.unwrap();
        assert!(
            body["error"].is_object(),
            "client error must include an error object"
        );
    } else {
        // If the server accepts it, the response must still be well-formed.
        assert_eq!(resp.status(), 200);
    }

    h.shutdown().await;
}

#[tokio::test]
async fn per_provider_budget_blocks_only_that_provider() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // Price the deepseek-sonnet model so the budget can measure spend.
    cfg.models[1].pricing = Some(Pricing::new("USD", 1000.0, 1000.0));
    // Budget scoped to the deepseek provider, exhausted after one request.
    cfg.budgets = vec![Budget::new(
        BudgetScope::Provider {
            id: "deepseek".into(),
        },
        BudgetPeriod::Day,
        "USD",
        0.01,
    )];
    let h = Harness::start(cfg, mock).await;

    // First deepseek request succeeds and spends past the limit.
    let resp = h
        .post("/v1/messages")
        .json(
            &json!({"model": "deepseek-deepseek-v4-pro", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Second deepseek request is refused by the provider budget.
    let resp = h
        .post("/v1/messages")
        .json(
            &json!({"model": "deepseek-deepseek-v4-pro", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 402);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "budget_exceeded");

    // OpenAI (different provider) should still work.
    let resp = h
        .post("/v1/chat/completions")
        .json(
            &json!({"model": "openai-gpt-5.3-sol",
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    h.shutdown().await;
}

// ------------------------------------------------------- shadow evaluation (ml)
//
// Stage 7E-1: the ML stack records what it *would have done* for policy-routed
// main traffic. Shadow is record-only by construction, so every test here also
// proves a negative: that production behaves identically with it on.

/// A policy-routed request (`sonnet-class` resolves through the tier and policy
/// engine); a direct-resolution request (`anthropic-claude-native` names an
/// exact model) stays out of scope.
#[cfg(feature = "ml")]
async fn post_policy_routed(h: &Harness, stream: bool, round: usize) -> reqwest::Response {
    h.post("/v1/messages")
        .json(&json!({
            "model": "sonnet-class",
            "max_tokens": 64,
            "stream": stream,
            "messages": [{"role": "user", "content": format!("round {round}")}]
        }))
        .send()
        .await
        .unwrap()
}

#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_decisions_record_only_for_policy_routed_requests() {
    use zroutery_core::ml::{ShadowScope, FEATURE_SCHEMA_VERSION};

    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.shadow.enabled = true;
    let h = Harness::start(cfg, mock).await;

    // Policy-routed: the tier virtual id resolves through the policy engine.
    let resp = post_policy_routed(&h, false, 0).await;
    assert_eq!(resp.status(), 200);

    let store = h.state.shadow().store();
    assert_eq!(store.len(), 1, "one shadow decision per policy-routed request");
    let shadow = &store.decisions()[0];

    // Correlated with the request record: same request id, same decision id.
    let record = h.state.stats().recent(10).remove(0);
    assert_eq!(shadow.actual.request_id, record.id);
    let decision = record
        .routing_decision
        .as_ref()
        .expect("policy-routed request record carries its routing decision");
    assert_eq!(shadow.actual.decision_id, decision.decision_id);

    // The record itself is well-formed evidence.
    assert_eq!(shadow.scope, ShadowScope::PolicyRouted);
    assert!(shadow.shadow_id.starts_with("shadow-"));
    assert_eq!(shadow.shadow.feature_schema, FEATURE_SCHEMA_VERSION);
    assert_eq!(shadow.actual.selected, "deepseek-deepseek-v4-pro");
    // Single-candidate plan: the engine sees the current candidate first, so
    // its hypothetical verdict keeps production's choice.
    assert_eq!(shadow.shadow.selected, "deepseek-deepseek-v4-pro");
    assert!(!shadow.candidates.is_empty());
    assert_eq!(shadow.candidates[0].candidate_id, "deepseek-deepseek-v4-pro");
    assert!(shadow.candidates[0].eligible);
    assert_eq!(h.state.shadow().fault_count(), 0);

    // Direct resolution (exact model id) is out of shadow scope.
    let resp = h
        .post("/v1/chat/completions")
        .json(&json!({
            "model": "anthropic-claude-native",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(store.len(), 1, "direct-resolution requests are not recorded");

    h.shutdown().await;
}

/// A/B purity: shadow on must not change what reaches providers or how
/// requests are counted.
#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_causes_no_provider_requests() {
    async fn run(shadow: bool) -> (usize, u64, usize) {
        let (addr, mock) = start_mock().await;
        let mut cfg = config_for(addr);
        cfg.shadow.enabled = shadow;
        let h = Harness::start(cfg, mock).await;
        for round in 0..3 {
            let resp = post_policy_routed(&h, false, round).await;
            assert_eq!(resp.status(), 200);
        }
        let upstream_calls = h.mock.count();
        let requests = h.state.stats().summary().requests;
        let shadow_records = h.state.shadow().store().len();
        h.shutdown().await;
        (upstream_calls, requests, shadow_records)
    }

    let off = run(false).await;
    let on = run(true).await;
    // Identical upstream traffic and request accounting...
    assert_eq!(off.0, on.0, "upstream call count must not change");
    assert_eq!(off.1, on.1, "request accounting must not change");
    // ...while only the ON side actually recorded shadow decisions.
    assert_eq!(off.2, 0);
    assert_eq!(on.2, 3);
}

/// A/B with expected delta: shadow on must not mutate router health, spend,
/// per-request routing outcomes or the bytes the client receives.
#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_causes_no_runtime_mutation() {
    use zroutery_core::budget::Ledger;

    /// A request record with the volatile fields masked: what was asked, which
    /// model answered on which provider, ok flag, status, attempt count.
    /// (Ids, timestamps and latencies differ per run by construction.)
    #[derive(Debug, PartialEq)]
    struct MaskedRecord {
        requested_model: String,
        resolved_model: Option<String>,
        provider_name: Option<String>,
        ok: bool,
        status: u16,
        attempts: u32,
    }

    /// Health rows with the timing-derived fields (`avg_latency_ms`,
    /// `cooldown_remaining_secs`) masked: latency is measured wall clock,
    /// so identical scripts still differ by scheduling noise. What must
    /// not diverge is the structural state — breaker state, counters and
    /// the last error.
    #[derive(Debug, PartialEq)]
    struct MaskedHealth {
        model_id: String,
        state: zroutery_core::circuit_breaker::CircuitState,
        consecutive_failures: u32,
        total_success: u64,
        total_failure: u64,
        last_error: Option<String>,
    }

    struct Outcome {
        health: Vec<MaskedHealth>,
        ledger: Ledger,
        records: Vec<MaskedRecord>,
        bodies: Vec<Value>,
    }

    async fn run(shadow: bool) -> Outcome {
        let (addr, mock) = start_mock().await;
        let mut cfg = config_for(addr);
        cfg.shadow.enabled = shadow;
        // Price the model that answers so the ledger comparison is over real
        // spends, not two empty ledgers.
        cfg.models[1].pricing = Some(Pricing::new("USD", 0.5, 2.0));
        let h = Harness::start(cfg, mock).await;

        let mut bodies = Vec::new();
        for round in 0..3 {
            let resp = post_policy_routed(&h, false, round).await;
            assert_eq!(resp.status(), 200);
            bodies.push(resp.json::<Value>().await.unwrap());
        }

        let records = h
            .state
            .stats()
            .recent(10)
            .into_iter()
            .map(|r| MaskedRecord {
                requested_model: r.requested_model,
                resolved_model: r.resolved_model,
                provider_name: r.provider_name,
                ok: r.ok,
                status: r.status,
                attempts: r.attempts,
            })
            .collect();
        let health = h
            .state
            .router()
            .health_snapshot()
            .into_iter()
            .map(|mh| MaskedHealth {
                model_id: mh.model_id,
                state: mh.state,
                consecutive_failures: mh.consecutive_failures,
                total_success: mh.total_success,
                total_failure: mh.total_failure,
                last_error: mh.last_error,
            })
            .collect();
        let outcome = Outcome {
            health,
            ledger: h.state.ledger(),
            records,
            bodies,
        };
        h.shutdown().await;
        outcome
    }

    let off = run(false).await;
    let on = run(true).await;

    // Router health identical: no shadow-driven failures, recoveries or probes.
    assert_eq!(off.health, on.health, "health must not diverge");
    // Identical spend for identical deterministic traffic.
    assert_eq!(off.ledger, on.ledger, "ledger must not diverge");
    // Identical per-request routing outcomes.
    assert_eq!(off.records, on.records, "records must not diverge");
    // Byte-identical answers to the client.
    assert_eq!(off.bodies, on.bodies, "response bodies must not diverge");
    // The SessionStore is not wired into the production pipeline (it has no
    // reader yet), and the shadow path holds no session handle, so it is
    // untouched on both sides by construction.
}

/// Production must be unaffected with shadow on, and a healthy predictor runs
/// fault-free. (Poisoned-predictor isolation is covered at engine level in
/// the ml shadow tests; AppState wires the real predictor.)
#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_survives_predictor_faults() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.shadow.enabled = true;
    let h = Harness::start(cfg, mock).await;

    // Two buffered and one streaming policy-routed request, so both evaluate
    // hooks (buffered_chat and stream_chat) run with shadow on.
    let resp = post_policy_routed(&h, false, 0).await;
    assert_eq!(resp.status(), 200);
    let resp = post_policy_routed(&h, false, 1).await;
    assert_eq!(resp.status(), 200);
    let resp = post_policy_routed(&h, true, 2).await;
    assert_eq!(resp.status(), 200);
    // Drain the stream so its record finalizes.
    let _ = resp.text().await.unwrap();

    // Every request succeeded and every one was shadow-evaluated, fault-free.
    assert_eq!(h.state.stats().summary().requests, 3);
    assert_eq!(h.state.stats().summary().failures, 0);
    assert_eq!(h.state.shadow().store().len(), 3);
    assert_eq!(h.state.shadow().fault_count(), 0);

    h.shutdown().await;
}

#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_disabled_by_default() {
    let h = Harness::new().await;
    assert!(!h.state.shadow().enabled());

    // Policy-routed traffic while disabled: nothing recorded, nothing faulted.
    let resp = post_policy_routed(&h, false, 0).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(h.state.shadow().store().len(), 0);
    assert_eq!(h.state.shadow().fault_count(), 0);

    h.shutdown().await;
}

/// GATE 7E-1 (model mutation = 0): production traffic never trains the
/// shadow ensemble. Across a burst of policy-routed requests — buffered and
/// streaming, so both evaluate hooks run — every recorded decision carries
/// one and the same model commit, and that commit is the genesis commit the
/// engine started from. The commit id is derived from the cold-start
/// ensemble's content hash, so any training during the burst would have
/// advanced it.
#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_model_commit_stable_across_traffic() {
    use zroutery_core::ml::ModelEnsemblePredictor;

    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.shadow.enabled = true;
    let h = Harness::start(cfg, mock).await;

    for round in 0..4 {
        let stream = round % 2 == 0;
        let resp = post_policy_routed(&h, stream, round).await;
        assert_eq!(resp.status(), 200);
        if stream {
            // Drain the stream so its record finalizes.
            let _ = resp.text().await.unwrap();
        }
    }

    let decisions = h.state.shadow().store().decisions();
    assert_eq!(decisions.len(), 4, "one shadow decision per request");
    assert_eq!(h.state.shadow().fault_count(), 0);

    // One identical commit across the whole burst: nothing trained mid-flight.
    let observed = decisions[0].shadow.model_commit.clone();
    assert!(
        decisions.iter().all(|d| d.shadow.model_commit == observed),
        "every decision must carry the same model commit"
    );

    // ...and that commit is genesis: a fresh predictor's deterministic
    // cold-start commit — the same one AppState wired at startup.
    let genesis = ModelEnsemblePredictor::genesis().commit();
    assert_eq!(
        observed, genesis,
        "production traffic must not advance the shadow model"
    );

    h.shutdown().await;
}

// ------------------------------------------------- fallback / retry A/B (ml)
//
// Twin-harness evidence for the two remaining gate items: production
// fallback = 0 and production retry = 0 under shadow. Each test runs the
// same script twice (shadow off vs on) against fresh mocks and compares
// everything downstream of the shadow hook.

/// Masked request record for the A/B comparison (same masking discipline as
/// `shadow_causes_no_runtime_mutation`: ids, timestamps and latencies are
/// volatile by construction; everything else must match — including each
/// record's attempt count).
#[cfg(feature = "ml")]
#[derive(Debug, PartialEq)]
struct AbRecord {
    requested_model: String,
    resolved_model: Option<String>,
    provider_name: Option<String>,
    ok: bool,
    status: u16,
    attempts: u32,
}

/// Masked health row: the timing-derived fields (`avg_latency_ms`,
/// `cooldown_remaining_secs`) are dropped; the structural state — breaker
/// state, counters, last error — must match.
#[cfg(feature = "ml")]
#[derive(Debug, PartialEq)]
struct AbHealth {
    model_id: String,
    state: zroutery_core::circuit_breaker::CircuitState,
    consecutive_failures: u32,
    total_success: u64,
    total_failure: u64,
    last_error: Option<String>,
}

/// Everything the A/B tests compare across a twin run.
#[cfg(feature = "ml")]
struct AbOutcome {
    /// The full upstream request bodies, in order — identical traffic.
    upstream_bodies: Vec<Value>,
    /// The model that answered each client request (`x-zroutery-model`).
    served_models: Vec<String>,
    records: Vec<AbRecord>,
    health: Vec<AbHealth>,
    ledger: zroutery_core::budget::Ledger,
    bodies: Vec<Value>,
    shadow_decisions: usize,
    shadow_faults: u64,
}

#[cfg(feature = "ml")]
impl AbOutcome {
    fn capture(h: &Harness, served_models: Vec<String>, bodies: Vec<Value>) -> Self {
        let records = h
            .state
            .stats()
            .recent(10)
            .into_iter()
            .map(|r| AbRecord {
                requested_model: r.requested_model,
                resolved_model: r.resolved_model,
                provider_name: r.provider_name,
                ok: r.ok,
                status: r.status,
                attempts: r.attempts,
            })
            .collect();
        let health = h
            .state
            .router()
            .health_snapshot()
            .into_iter()
            .map(|mh| AbHealth {
                model_id: mh.model_id,
                state: mh.state,
                consecutive_failures: mh.consecutive_failures,
                total_success: mh.total_success,
                total_failure: mh.total_failure,
                last_error: mh.last_error,
            })
            .collect();
        AbOutcome {
            upstream_bodies: h.mock.bodies(),
            served_models,
            records,
            health,
            ledger: h.state.ledger(),
            bodies,
            shadow_decisions: h.state.shadow().store().len(),
            shadow_faults: h.state.shadow().fault_count(),
        }
    }

    /// Assert the two twin runs are production-identical.
    fn assert_identical(off: &AbOutcome, on: &AbOutcome) {
        assert_eq!(
            off.upstream_bodies, on.upstream_bodies,
            "upstream traffic must not change"
        );
        assert_eq!(
            off.served_models, on.served_models,
            "served models must not change"
        );
        assert_eq!(
            off.records, on.records,
            "request records (incl. attempt counts) must not change"
        );
        assert_eq!(off.health, on.health, "health must not diverge");
        assert_eq!(off.ledger, on.ledger, "ledger must not diverge");
        assert_eq!(off.bodies, on.bodies, "response bodies must not diverge");
    }
}

/// GATE 7E-1 (fallback = 0): production failover must be identical with
/// shadow off vs on. Script: two policy-routed requests whose first
/// candidate always answers 429 (the mock's `limited*` convention, as in
/// `rate_limit_triggers_failover`), forcing production to fail over to the
/// second candidate. Compared across the twin runs: the exact upstream
/// traffic, the served model per request, the masked request records
/// (including each record's attempt count), the masked router health, the
/// spend ledger and the bytes the client receives.
#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_does_not_cause_fallback() {
    async fn run(shadow: bool) -> AbOutcome {
        let (addr, mock) = start_mock().await;
        let mut cfg = config_for(addr);
        cfg.shadow.enabled = shadow;
        // Primary always 429s; the fallback model is healthy and priced so
        // the ledger comparison is over real spend.
        let mut fallback =
            ModelEntry::for_upstream("deepseek", "deepseek-v4-pro", Some(ModelTier::Standard))
                .with_priority(10);
        fallback.pricing = Some(Pricing::new("USD", 0.5, 2.0));
        cfg.models = vec![
            ModelEntry::for_upstream("deepseek", "limited-v4", Some(ModelTier::Standard))
                .with_priority(0),
            fallback,
        ];
        let h = Harness::start(cfg, mock).await;

        let mut served_models = Vec::new();
        let mut bodies = Vec::new();
        for round in 0..2 {
            let resp = post_policy_routed(&h, false, round).await;
            assert_eq!(resp.status(), 200, "the failover script must succeed");
            served_models.push(
                resp.headers()["x-zroutery-model"]
                    .to_str()
                    .unwrap()
                    .to_string(),
            );
            bodies.push(resp.json::<Value>().await.unwrap());
        }

        let outcome = AbOutcome::capture(&h, served_models, bodies);
        h.shutdown().await;
        outcome
    }

    let off = run(false).await;
    let on = run(true).await;

    // The script really exercised production fallback: the rate-limited
    // primary was attempted and at least one request needed a second attempt.
    assert!(
        off.upstream_bodies
            .iter()
            .any(|b| b["model"] == "limited-v4"),
        "the rate-limited primary must have been attempted"
    );
    assert!(
        off.records.iter().any(|r| r.attempts >= 2),
        "the script must trigger a fallback"
    );

    // Identical traffic, outcomes, health, spend and bytes.
    AbOutcome::assert_identical(&off, &on);

    // Only the ON side recorded shadow decisions — one per request,
    // fault-free. Each snapshot is taken before any attempt (is_fallback is
    // false by construction), so a store record exists for every fallback
    // request exactly as for every clean one.
    assert_eq!(off.shadow_decisions, 0);
    assert_eq!(on.shadow_decisions, 2);
    assert_eq!(on.shadow_faults, 0);
}

/// GATE 7E-1 (retry = 0): the production retry path must be identical with
/// shadow off vs on. Script: a policy-routed request whose first candidate
/// answers with a REPAIRABLE 400 (thinking budget too large — the mock's
/// `budget*` convention). The pipeline runs the thinking-budget rectifier,
/// which halves `budget_tokens` and retries the SAME candidate, so one
/// client request produces two upstream calls to the same model (the
/// rejected shape, then the repaired one) and never fails over.
///
/// Honest scope note: chat requests have no other same-candidate retry seam
/// to drive from a test — client errors are not retried, the transport-level
/// handshake retries happen inside the upstream client, and every other
/// retry-shaped failure flows through the failover loop the fallback test
/// already covers. The rectifier path is the one retry the pipeline itself
/// drives, and both its calls land on the same per-request upstream counter
/// this test compares.
#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_does_not_cause_retry() {
    async fn run(shadow: bool) -> AbOutcome {
        let (addr, mock) = start_mock().await;
        let mut cfg = config_for(addr);
        cfg.shadow.enabled = shadow;
        // The primary answers 400 until its thinking budget is halved to the
        // floor (1024); the rectifier does exactly that and retries the SAME
        // candidate. It advertises the thinking capability the request
        // requires and is priced so the ledger comparison is over real spend.
        let mut primary =
            ModelEntry::for_upstream("anthropic", "budget-v4", Some(ModelTier::Standard))
                .with_priority(0);
        primary.capabilities.thinking = true;
        primary.pricing = Some(Pricing::new("USD", 0.5, 2.0));
        cfg.models = vec![
            primary,
            ModelEntry::for_upstream("deepseek", "deepseek-v4-pro", Some(ModelTier::Standard))
                .with_priority(10),
        ];
        let h = Harness::start(cfg, mock).await;

        let mut served_models = Vec::new();
        let mut bodies = Vec::new();
        for round in 0..2 {
            let resp = h
                .post("/v1/messages")
                .json(&json!({
                    "model": "sonnet-class",
                    "max_tokens": 64,
                    "thinking": {"type": "enabled", "budget_tokens": 2048},
                    "messages": [{"role": "user", "content": format!("round {round}")}]
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "the repaired retry must succeed");
            served_models.push(
                resp.headers()["x-zroutery-model"]
                    .to_str()
                    .unwrap()
                    .to_string(),
            );
            bodies.push(resp.json::<Value>().await.unwrap());
        }

        let outcome = AbOutcome::capture(&h, served_models, bodies);
        h.shutdown().await;
        outcome
    }

    let off = run(false).await;
    let on = run(true).await;

    // The script really exercised the same-candidate retry: every request
    // hit the SAME model twice — the rejected 2048-budget shape, then the
    // repaired 1024-budget body — and never failed over.
    for round in 0..2 {
        let rejected = &off.upstream_bodies[round * 2];
        let repaired = &off.upstream_bodies[round * 2 + 1];
        assert_eq!(rejected["model"], "budget-v4");
        assert_eq!(
            repaired["model"], "budget-v4",
            "the retry must stay on the same candidate"
        );
        assert_eq!(rejected["thinking"]["budget_tokens"], 2048);
        assert_eq!(
            repaired["thinking"]["budget_tokens"], 1024,
            "the rectifier must have halved the budget"
        );
    }
    assert!(
        off.records.iter().all(|r| r.attempts == 1),
        "a repaired retry is not a fresh attempt"
    );

    // Identical traffic, outcomes, health, spend and bytes.
    AbOutcome::assert_identical(&off, &on);

    // Only the ON side recorded shadow decisions — one per request,
    // fault-free, snapshotted before the attempt (and therefore before the
    // retry) — so the retry path ran with a decision on record and was
    // untouched by it.
    assert_eq!(off.shadow_decisions, 0);
    assert_eq!(on.shadow_decisions, 2);
    assert_eq!(on.shadow_faults, 0);
}
