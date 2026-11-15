use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use chrono::Local;
use futures_util::StreamExt;
use serde_json::{json, Value};
use zroutery_core::budget::{BudgetPeriod, BudgetScope, Ledger};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
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
pub(super) struct Mock {
    inner: Arc<Mutex<MockInner>>,
}

impl Mock {
    fn record(&self, path: &str, key: Option<String>, body: &Value) {
        let mut inner = self.inner.lock().unwrap();
        inner.paths.push(path.to_string());
        inner.keys.push(key.unwrap_or_default());
        inner.received.push(body.clone());
    }

    pub(super) fn bodies(&self) -> Vec<Value> {
        self.inner.lock().unwrap().received.clone()
    }

    pub(super) fn keys(&self) -> Vec<String> {
        self.inner.lock().unwrap().keys.clone()
    }

    pub(super) fn count(&self) -> usize {
        self.inner.lock().unwrap().received.len()
    }
}

/// The mock reacts to the requested model name:
/// * `broken*`  -> HTTP 500
/// * `refuse*`  -> HTTP 400 naming the request invalid (non retryable)
/// * `broke*`   -> HTTP 402 with a billing marker (empty wallet)
/// * `limited*` -> HTTP 429
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

    if headers
        .get("authorization")
        .is_some_and(|key| key == "Bearer sk-empty-wallet")
    {
        return (
            axum::http::StatusCode::PAYMENT_REQUIRED,
            Json(json!({"error":{"code":"insufficient_balance","message":"insufficient balance"}})),
        )
            .into_response();
    }
    if headers
        .get("authorization")
        .is_some_and(|key| key == "Bearer sk-invalid-inference")
    {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"type":"invalid_api_key","message":"invalid API key"}})),
        )
            .into_response();
    }

    let model = body["model"].as_str().unwrap_or("").to_string();
    if model.starts_with("broken") {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "upstream exploded", "type": "server_error"}})),
        )
            .into_response();
    }
    if model.starts_with("broke") {
        return (
            axum::http::StatusCode::PAYMENT_REQUIRED,
            Json(json!({"error": {"message": "insufficient balance",
                                  "type": "insufficient_quota"}})),
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
        } else if model == "guardian-risk" {
            json!({"role":"assistant", "content":"low"})
        } else if model == "guardian-review" {
            json!({"role":"assistant", "content":"{\"outcome\":\"allow\"}"})
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
    } else if matches!(model.as_str(), "guardian-risk" | "guardian-review") {
        let text = if model == "guardian-risk" {
            "low"
        } else {
            "{\"outcome\":\"allow\"}"
        };
        sse.push_str(&chunk(
            json!({"role":"assistant", "content":text}),
            Value::Null,
        ));
        sse.push_str(&chunk(json!({}), json!("stop")));
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
    if body["model"]
        .as_str()
        .unwrap_or("")
        .starts_with("signature")
    {
        let stale = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|message| {
                message["content"]
                    .as_array()
                    .is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "thinking"))
            });
        if stale {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error":{"type":"invalid_request_error",
                "message":"invalid signature thinking block"}})),
            )
                .into_response();
        }
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

pub(super) async fn start_mock() -> (SocketAddr, Mock) {
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

// ------------------------------------------------------------------ timing mock

/// How long a buffered request waits for a peer before answering alone.
///
/// It exists so the same mock can prove both directions: two requests that are
/// allowed to overlap show `max_in_flight == 2` within milliseconds, while a
/// serialised request still finishes after this grace instead of hanging.
const HOLD_GRACE: Duration = Duration::from_millis(250);

/// A mock upstream that holds each buffered request until a second one arrives,
/// so a test can see how many requests the proxy really allows in flight.
#[derive(Clone, Default)]
pub(super) struct HoldMock {
    inner: Arc<Mutex<HoldInner>>,
    notify: Arc<tokio::sync::Notify>,
}

#[derive(Default)]
struct HoldInner {
    /// Every request the mock was asked to answer.
    requests: usize,
    in_flight: usize,
    max_in_flight: usize,
}

impl HoldMock {
    /// Count a request as in flight; the returned guard leaves on drop.
    fn enter(&self) -> InFlight {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.in_flight += 1;
            inner.max_in_flight = inner.max_in_flight.max(inner.in_flight);
        }
        InFlight(self.clone())
    }

    fn leave(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.in_flight = inner.in_flight.saturating_sub(1);
    }

    fn record(&self) {
        self.inner.lock().unwrap().requests += 1;
    }

    pub(super) fn requests(&self) -> usize {
        self.inner.lock().unwrap().requests
    }

    pub(super) fn max_in_flight(&self) -> usize {
        self.inner.lock().unwrap().max_in_flight
    }

    /// Wait until a peer is also in flight, or until `grace` runs out.
    ///
    /// The timeout is what lets a correctly serialised request finish: no peer
    /// is coming, and the request under test still needs its answer.
    async fn await_peer(&self) {
        let deadline = tokio::time::Instant::now() + HOLD_GRACE;
        loop {
            {
                let inner = self.inner.lock().unwrap();
                if inner.in_flight >= 2 {
                    self.notify.notify_waiters();
                    return;
                }
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if tokio::time::timeout(left, self.notify.notified())
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

/// Leaves the in-flight count on drop, so an early return cannot skew it.
struct InFlight(HoldMock);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.leave();
    }
}

async fn hold_openai_chat(State(mock): State<HoldMock>, Json(body): Json<Value>) -> Response {
    mock.record();
    let model = body["model"].as_str().unwrap_or("").to_string();

    if body["stream"].as_bool().unwrap_or(false) {
        // A stream the proxy has to end: it emits one frame, then never
        // finishes, so only a cancellation or a disconnect takes the request
        // off the books.
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from_stream(stalled_openai_sse()))
            .unwrap();
    }

    let _in_flight = mock.enter();
    mock.await_peer().await;
    Json(json!({
        "id": "chatcmpl-hold",
        "object": "chat.completion",
        "created": 1,
        "model": model,
        "choices": [{"index": 0,
                     "message": {"role": "assistant", "content": "held"},
                     "finish_reason": "stop"}],
        // 1000 fresh input tokens at the pricing the tests install is exactly
        // one dime per request.
        "usage": {"prompt_tokens": 1000, "completion_tokens": 0}
    }))
    .into_response()
}

/// One OpenAI SSE frame, then a stream that never ends.
fn stalled_openai_sse(
) -> impl futures_util::Stream<Item = std::result::Result<bytes::Bytes, std::io::Error>> {
    let first = format!(
        "data: {}\n\n",
        json!({"id": "chatcmpl-hold", "object": "chat.completion.chunk", "created": 1,
               "model": "mock",
               "choices": [{"index": 0,
                            "delta": {"role": "assistant", "content": "partial"},
                            "finish_reason": null}]})
    );
    futures_util::stream::once(async move { Ok(bytes::Bytes::from(first)) }).chain(
        futures_util::stream::once(async {
            std::future::pending::<()>().await;
            Ok(bytes::Bytes::new())
        }),
    )
}

pub(super) async fn start_hold_mock() -> (SocketAddr, HoldMock) {
    let mock = HoldMock::default();
    let app = axum::Router::new()
        .route("/chat/completions", post(hold_openai_chat))
        .route("/v1/chat/completions", post(hold_openai_chat))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, mock)
}

/// The global day spend the ledger currently holds.
pub(super) fn global_day_spend(state: &AppState) -> f64 {
    state
        .ledger()
        .totals_for(&BudgetScope::Global, Local::now())
        .into_iter()
        .find(|(period, _)| *period == BudgetPeriod::Day)
        .map(|(_, cost)| cost.amount)
        .unwrap_or(0.0)
}

/// Wait until the lifecycle has recorded terminal transitions.
///
/// A client disconnect is noticed by the server when the connection goes away,
/// which is asynchronous by nature, so this polls instead of guessing a delay.
pub(super) async fn wait_for_outcomes(harness: &Harness, expected: usize) {
    for _ in 0..200 {
        if harness.state.outcomes().len() == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "expected {expected} terminal outcomes, saw {}",
        harness.state.outcomes().len()
    );
}

// ------------------------------------------------------------------ fixtures

pub(super) const TOKEN: &str = "zr-test-token";

/// The scenario from the brief, pointed at the mock instead of the real APIs.
pub(super) fn config_for(mock: SocketAddr) -> AppConfig {
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

pub(super) fn secrets() -> Arc<MemorySecretStore> {
    Arc::new(
        MemorySecretStore::new()
            .with("provider:deepseek", "sk-deepseek")
            .with("provider:openai", "sk-openai")
            .with("provider:anthropic", "sk-ant"),
    )
}

pub(super) struct Harness {
    pub(super) base: String,
    pub(super) server: Option<ServerHandle>,
    pub(super) state: Arc<AppState>,
    pub(super) client: reqwest::Client,
    pub(super) mock: Mock,
}

impl Harness {
    pub(super) async fn start(cfg: AppConfig, mock: Mock) -> Harness {
        let state = Arc::new(AppState::new(cfg, secrets()));
        Harness::from_state(state, mock).await
    }

    /// Start with a ledger that is already on the books.
    ///
    /// This is how a test states "$0.99 has already been spent" without
    /// spending it: the admission fix is about what the check reads, and the
    /// only way to make it deterministic is to hand it a known total. The
    /// harness's own recording mock is left empty because the admission tests
    /// assert against the timing mock they started themselves.
    pub(super) async fn start_with_ledger(cfg: AppConfig, ledger: Ledger) -> Harness {
        let state = Arc::new(AppState::new(cfg, secrets()));
        state.set_ledger(ledger);
        Harness::from_state(state, Mock::default()).await
    }

    pub(super) async fn from_state(state: Arc<AppState>, mock: Mock) -> Harness {
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

    pub(super) async fn new() -> Harness {
        let (addr, mock) = start_mock().await;
        Harness::start(config_for(addr), mock).await
    }

    pub(super) fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}{path}", self.base))
            .header("x-api-key", TOKEN)
    }

    pub(super) fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .get(format!("{}{path}", self.base))
            .header("x-api-key", TOKEN)
    }

    pub(super) async fn shutdown(mut self) {
        if let Some(s) = self.server.take() {
            s.stop().await;
        }
    }
}

// ---------------------------------------------------------------- shadow (ml)

/// A policy-routed request (`sonnet-class` resolves through the tier and policy
/// engine); a direct-resolution request (`anthropic-claude-native` names an
/// exact model) stays out of scope.
#[cfg(feature = "ml")]
pub(super) async fn post_policy_routed(
    h: &Harness,
    stream: bool,
    round: usize,
) -> reqwest::Response {
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
