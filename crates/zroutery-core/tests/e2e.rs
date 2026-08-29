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
    AppConfig, MemorySecretStore, ModelClass, ModelEntry, ProviderConfig, ProviderKind,
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
        ModelEntry::for_upstream("deepseek", "deepseek-v4-flash", Some(ModelClass::Haiku)),
        ModelEntry::for_upstream("deepseek", "deepseek-v4-pro", Some(ModelClass::Sonnet)),
        ModelEntry::for_upstream("openai", "gpt-5.3-sol", Some(ModelClass::Opus)),
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
            client: reqwest::Client::new(),
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
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelClass::Sonnet))
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
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelClass::Sonnet))
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
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelClass::Sonnet))
            .with_priority(10),
    );
    cfg.routing.break_after_failures = 1;
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
        BudgetScope::Class {
            class: ModelClass::Opus,
        },
        BudgetPeriod::Day,
        "USD",
        0.01,
    )
    .degrading_to(ModelClass::Haiku)];
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
        BudgetScope::Class {
            class: ModelClass::Opus,
        },
        BudgetPeriod::Day,
        "USD",
        0.01,
    )
    .degrading_to(ModelClass::Haiku)];
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
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelClass::Sonnet))
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
    let sonnet = election.classes.get(&ModelClass::Sonnet).unwrap();
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
    cfg.models[3].class = Some(ModelClass::Opus);
    let h = Harness::start(cfg, mock).await;

    let election = h.state.hold_election().await;
    let opus = election.classes.get(&ModelClass::Opus).unwrap();
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
        election.classes.get(&ModelClass::Sonnet).unwrap().winner(),
        Some("deepseek-deepseek-v4-pro")
    );

    // Add a member whose priority would otherwise put it first.
    let mut next = (*h.state.config()).clone();
    next.models.push(
        ModelEntry::for_upstream("openai", "gpt-sonnet", Some(ModelClass::Sonnet))
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
        .class_members(ModelClass::Sonnet)
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
        .unwrap();
    assert_eq!(resp.status(), 413);
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
    assert!(ids.contains(&"opus-class"));
    assert!(ids.contains(&"sonnet-class"));
    assert!(ids.contains(&"haiku-class"));

    // Both dialects find what they expect on each entry.
    let entry = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "sonnet-class")
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
    assert_eq!(entry["zroutery"]["class"], Value::Null);
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
    assert_eq!(single["zroutery"]["class"], "opus");

    assert_eq!(h.get("/v1/models/nope").send().await.unwrap().status(), 404);

    h.shutdown().await;
}

#[tokio::test]
async fn unknown_and_unclassified_routing_errors() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // Remove every opus member so the class is empty.
    cfg.models.retain(|m| m.class != Some(ModelClass::Opus));
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
        Some(ModelClass::Sonnet),
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
        Some(ModelClass::Sonnet),
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
