use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::billing::Pricing;
use zroutery_core::budget::{BudgetPeriod, BudgetScope};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::outcome::Outcome;
use zroutery_core::server::{AppState, ServerHandle};
use zroutery_core::stats::RequestRecord;

// ------------------------------------------------------------------ mock upstream

#[derive(Clone, Default)]
pub(super) struct Mock {
    inner: Arc<Mutex<Vec<String>>>,
    /// Every upstream chat body the mock received, in order, so a test can see
    /// what the proxy actually asked the model — in particular the expanded
    /// history of a `previous_response_id` continuation.
    bodies: Arc<Mutex<Vec<Value>>>,
    /// Hands out a distinct id per buffered answer, the way a real provider
    /// does. A constant id would make every stored turn point at itself.
    ids: Arc<std::sync::atomic::AtomicUsize>,
}

impl Mock {
    fn record(&self, model: &str) {
        self.inner.lock().unwrap().push(model.to_string());
    }

    fn record_body(&self, body: Value) {
        self.bodies.lock().unwrap().push(body);
    }

    fn next_id(&self, prefix: &str) -> String {
        let n = self.ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("{prefix}-{n}")
    }

    pub(super) fn count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// How many sends named this upstream model, in order.
    pub(super) fn count_of(&self, model: &str) -> usize {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| seen.as_str() == model)
            .count()
    }

    pub(super) fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().unwrap().clone()
    }
}

fn openai_chunk(id: &str, delta: Value, finish: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"id": id, "object": "chat.completion.chunk", "created": 1,
               "model": "mock", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    )
}

/// An upstream stream that delivers part of an answer and then stays open.
///
/// This is what a slow answer looks like to the proxy, and it is the only way
/// to make a client disconnect land *mid-stream*: bytes have gone to the client
/// and the client is gone before the answer is finished.
///
/// `id` is the chunk id the upstream reports. An empty one matters for the
/// Responses API: the proxy then substitutes the response id it registered as
/// in-flight, which is the id a cancellation has to name.
fn held_open_stream(id: &str) -> axum::body::Body {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let first = id.to_string();
    let second = id.to_string();
    let third = id.to_string();
    tokio::spawn(async move {
        let _ = tx.send(openai_chunk(
            &first,
            json!({"role": "assistant", "content": ""}),
            Value::Null,
        ));
        let _ = tx.send(openai_chunk(
            &second,
            json!({"content": "hel"}),
            Value::Null,
        ));
        let _ = tx.send(openai_chunk(&third, json!({"content": "lo"}), Value::Null));
        // The upstream never ends: the answer is still being written when the
        // client decides it has had enough.
        std::future::pending::<()>().await;
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|chunk| (Ok::<_, std::io::Error>(chunk), rx))
    });
    axum::body::Body::from_stream(stream)
}

async fn mock_chat(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    let model = body["model"].as_str().unwrap_or_default().to_string();
    mock.record(&model);
    mock.record_body(body.clone());

    // A model that cannot see rejects any request that still carries an image;
    // the media rectifier then retries it with a text placeholder.
    let has_image = body["messages"]
        .as_array()
        .map(|messages| {
            messages.iter().any(|message| {
                message
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|blocks| {
                        blocks.iter().any(|block| {
                            matches!(
                                block.get("type").and_then(Value::as_str),
                                Some("image_url") | Some("image")
                            )
                        })
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if model.starts_with("blind") && has_image {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message":
                "this model does not support image input; images are unsupported",
                "type": "invalid_request_error"}})),
        )
            .into_response();
    }

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

    if body["stream"].as_bool().unwrap_or(false) {
        if model.starts_with("chat-refusal") || model.starts_with("chat-partial-refusal") {
            let code = if model.ends_with("wallet") {
                "insufficient_user_quota"
            } else {
                "rate_limit_exceeded"
            };
            let mut sse = String::new();
            if model.starts_with("chat-partial-refusal") {
                sse.push_str(&openai_chunk(
                    "chatcmpl-refusal",
                    json!({"role":"assistant", "content":"partial answer"}),
                    Value::Null,
                ));
            }
            // Same network chunk as any preceding output; a generic message
            // must not erase the code or make that output safe to replay.
            sse.push_str(&format!(
                "data: {}\n\n",
                json!({
                    "error":{"type":"new_api_error", "code":code, "message":"openai_error"},
                    "usage":{"prompt_tokens":11, "completion_tokens":7}
                })
            ));
            return Response::builder()
                .header("content-type", "text/event-stream")
                .header("retry-after", "120")
                .body(axum::body::Body::from(sse))
                .unwrap();
        }
        if model.starts_with("preoutput-quota") {
            let sse = format!(
                "event: response.failed\ndata: {}\n\n",
                json!({
                    "type":"response.failed", "response":{
                        "status":"failed", "error":{"code":"rate_limit_exceeded","message":"model token rate exceeded"}
                    }
                })
            );
            return Response::builder()
                .header("content-type", "text/event-stream")
                .body(axum::body::Body::from(sse))
                .unwrap();
        }
        if model.starts_with("terminal-tail") {
            // The protocol has ended, but the relay keeps TCP open. A client
            // may stop at response.completed without waiting for EOF.
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            tx.send(openai_chunk(
                "tail-stream",
                json!({"role": "assistant", "content": "answer"}),
                Value::Null,
            ))
            .unwrap();
            tx.send(openai_chunk("tail-stream", json!({}), json!("stop")))
                .unwrap();
            tx.send(format!(
                "data: {}\n\n",
                json!({
                    "id": "tail-stream", "choices": [],
                    "usage": {"prompt_tokens": 11, "completion_tokens": 7}
                })
            ))
            .unwrap();
            let stream = futures_util::stream::unfold((rx, tx), |(mut rx, tx)| async move {
                rx.recv()
                    .await
                    .map(|chunk| (Ok::<_, std::io::Error>(chunk), (rx, tx)))
            });
            return Response::builder()
                .header("content-type", "text/event-stream")
                .body(axum::body::Body::from_stream(stream))
                .unwrap();
        }
        if model.starts_with("hold") {
            return Response::builder()
                .header("content-type", "text/event-stream")
                .body(held_open_stream("chatcmpl-mock"))
                .unwrap();
        }
        if model.starts_with("cancel") {
            // An empty upstream id makes the proxy publish the response id it
            // registered as in-flight, which is what a cancel request names.
            return Response::builder()
                .header("content-type", "text/event-stream")
                .body(held_open_stream(""))
                .unwrap();
        }
        if model.starts_with("truncate") {
            // A 200 that starts an answer and then just ends: no
            // `finish_reason`, no `[DONE]`, no terminal of any kind. This is
            // what a relay cutting the connection looks like.
            let mut sse = String::new();
            sse.push_str(&openai_chunk(
                "chatcmpl-mock",
                json!({"role": "assistant", "content": ""}),
                Value::Null,
            ));
            sse.push_str(&openai_chunk(
                "chatcmpl-mock",
                json!({"content": "partial answer"}),
                Value::Null,
            ));
            if model.starts_with("truncate-usage") {
                // The relay reported usage before it vanished, so the spend
                // really happened even though the answer did not finish.
                sse.push_str(&format!(
                    "data: {}\n\n",
                    json!({"id": "chatcmpl-mock", "object": "chat.completion.chunk",
                           "model": "mock", "choices": [],
                           "usage": {"prompt_tokens": 11, "completion_tokens": 7}})
                ));
            }
            return Response::builder()
                .header("content-type", "text/event-stream")
                .body(axum::body::Body::from(sse))
                .unwrap();
        }
        if model.starts_with("empty-stream") {
            // HTTP 200 with no SSE frames at all.
            return Response::builder()
                .header("content-type", "text/event-stream")
                .body(axum::body::Body::from(""))
                .unwrap();
        }
        if model.starts_with("streamerr") {
            // The handshake succeeds and part of the answer streams, then the
            // upstream reports an error frame: the answer is a failed one even
            // though the HTTP status was 200 and an earlier candidate had
            // already failed.
            let mut sse = String::new();
            sse.push_str(&openai_chunk(
                "chatcmpl-mock",
                json!({"role": "assistant", "content": ""}),
                Value::Null,
            ));
            sse.push_str(&openai_chunk(
                "chatcmpl-mock",
                json!({"content": "partial"}),
                Value::Null,
            ));
            sse.push_str(&format!(
                "data: {}\n\n",
                json!({"error": {"message": "malformed output", "type": "server_error"}})
            ));
            return Response::builder()
                .header("content-type", "text/event-stream")
                .body(axum::body::Body::from(sse))
                .unwrap();
        }
        let mut sse = String::new();
        sse.push_str(&openai_chunk(
            "chatcmpl-mock",
            json!({"role": "assistant", "content": ""}),
            Value::Null,
        ));
        sse.push_str(&openai_chunk(
            "chatcmpl-mock",
            json!({"content": "hi"}),
            Value::Null,
        ));
        sse.push_str(&openai_chunk("chatcmpl-mock", json!({}), json!("stop")));
        sse.push_str(&format!(
            "data: {}\n\n",
            json!({"id": "chatcmpl-mock", "object": "chat.completion.chunk", "model": "mock",
                   "choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 2}})
        ));
        sse.push_str("data: [DONE]\n\n");
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(sse))
            .unwrap();
    }

    Json(json!({
        "id": mock.next_id("chatcmpl-mock"),
        "object": "chat.completion",
        "created": 1,
        "model": model,
        "choices": [{"index": 0,
                     "message": {"role": "assistant", "content": "hello from mock"},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7}
    }))
    .into_response()
}

pub(super) async fn start_mock() -> (SocketAddr, Mock) {
    let mock = Mock::default();
    let app = axum::Router::new()
        .route("/v1/chat/completions", post(mock_chat))
        .route("/v1/responses", post(mock_chat))
        .route("/chat/completions", post(mock_chat))
        .route("/v1/messages", post(mock_anthropic))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, mock)
}

/// One Anthropic SSE frame.
fn anthropic_frame(kind: &str, data: Value) -> String {
    format!("event: {kind}\ndata: {data}\n\n")
}

/// An Anthropic stream that names its own non-empty message id and then stays
/// open, the way a real provider holds a long answer.
fn held_open_anthropic(id: &str) -> axum::body::Body {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let id = id.to_string();
    tokio::spawn(async move {
        let _ = tx.send(anthropic_frame(
            "message_start",
            json!({"type": "message_start", "message": {
                "id": id, "model": "claude-mock",
                "usage": {"input_tokens": 11, "output_tokens": 0}}}),
        ));
        let _ = tx.send(anthropic_frame(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0,
                   "content_block": {"type": "text", "text": ""}}),
        ));
        let _ = tx.send(anthropic_frame(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "text_delta", "text": "hel"}}),
        ));
        let _ = tx.send(anthropic_frame(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "text_delta", "text": "lo"}}),
        ));
        // The answer is never finished.
        std::future::pending::<()>().await;
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|chunk| (Ok::<_, std::io::Error>(chunk), rx))
    });
    axum::body::Body::from_stream(stream)
}

/// An Anthropic stream that reaches a real normal terminal.
fn completed_anthropic_stream(id: &str) -> axum::body::Body {
    let mut sse = String::new();
    sse.push_str(&anthropic_frame(
        "message_start",
        json!({"type": "message_start", "message": {
            "id": id, "model": "claude-mock",
            "usage": {"input_tokens": 11, "output_tokens": 0}}}),
    ));
    sse.push_str(&anthropic_frame(
        "content_block_start",
        json!({"type": "content_block_start", "index": 0,
               "content_block": {"type": "text", "text": ""}}),
    ));
    sse.push_str(&anthropic_frame(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": 0,
               "delta": {"type": "text_delta", "text": "repaired"}}),
    ));
    sse.push_str(&anthropic_frame(
        "content_block_stop",
        json!({"type": "content_block_stop", "index": 0}),
    ));
    sse.push_str(&anthropic_frame(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
               "usage": {"output_tokens": 3}}),
    ));
    sse.push_str(&anthropic_frame(
        "message_stop",
        json!({"type": "message_stop"}),
    ));
    axum::body::Body::from(sse)
}

/// Every content-block type present in an encoded Anthropic request body.
fn anthropic_block_types(body: &Value) -> Vec<String> {
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter_map(|block| block["type"].as_str())
        .map(str::to_string)
        .collect()
}

/// The Anthropic-shaped upstream, so an upstream id with the other provider
/// dialect's shape goes through the same identity rule.
async fn mock_anthropic(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    let model = body["model"].as_str().unwrap_or_default().to_string();
    mock.record(&model);
    mock.record_body(body.clone());

    if body["stream"].as_bool().unwrap_or(false) && model.starts_with("hold") {
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(held_open_anthropic("msg_upstream"))
            .unwrap();
    }

    // The first send is a repairable request shape; the repair itself is
    // refused with a provider failure. That failure is classified as
    // fallbackable, which is what has to move the request to the next
    // candidate instead of ending it here.
    if model == "repair-fail-model" {
        if mock.count_of(&model) == 1 {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": {"type": "invalid_request_error",
                                      "message": "invalid signature thinking block"}})),
            )
                .into_response();
        }
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": {"type": "server_error", "message": "upstream overloaded"}})),
        )
            .into_response();
    }

    // A model family that rejects the request shape and accepts it after the
    // pipeline's rectifiers repair it. `repair-once`/`repair-twice`/`repair-
    // stream` reject a stale thinking signature; `repair-twice` and
    // `repair-multi` then reject an image. These test complete attempt identity
    // without relying on silent reduction of an explicitly requested budget.
    if model.starts_with("repair") {
        let budget = body["thinking"]
            .get("budget_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let block_types = anthropic_block_types(&body);
        let has_thinking_block = block_types.iter().any(|kind| kind == "thinking");
        let has_image = block_types.iter().any(|kind| kind == "image");
        let reject = |message: &str| {
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": {"type": "invalid_request_error", "message": message}})),
            )
                .into_response()
        };
        if model.starts_with("repair-never") {
            return reject(if budget > 0 {
                "thinking budget too large"
            } else {
                "invalid signature thinking block"
            });
        }
        if model.starts_with("repair-multi") || model.starts_with("repair-twice") {
            if has_thinking_block {
                return reject("invalid signature thinking block");
            }
            if has_image {
                return reject("image unsupported");
            }
        } else if has_thinking_block {
            return reject("invalid signature thinking block");
        }
        if model.starts_with("repair-stream") && body["stream"].as_bool().unwrap_or(false) {
            return Response::builder()
                .header("content-type", "text/event-stream")
                .body(completed_anthropic_stream("msg_repaired"))
                .unwrap();
        }
    }

    Json(json!({
        "id": "msg_mock",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{"type": "text", "text": "hello from mock"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 11, "output_tokens": 7}
    }))
    .into_response()
}

// ------------------------------------------------------------------ fixtures

pub(super) const TOKEN: &str = "zr-lifecycle-token";

pub(super) fn provider(id: &str, name: &str, mock: SocketAddr) -> ProviderConfig {
    let mut provider = ProviderConfig::new(id, name, ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{mock}");
    provider.key_ref = format!("provider:{id}");
    provider.timeout_secs = 10;
    provider
}

/// A provider that speaks the other upstream dialect, so the same identity rule
/// is exercised on an `msg_…` id as well as a `chatcmpl-…` one.
pub(super) fn anthropic_provider(id: &str, name: &str, mock: SocketAddr) -> ProviderConfig {
    let mut provider = ProviderConfig::new(id, name, ProviderKind::Anthropic);
    provider.base_url = format!("http://{mock}");
    provider.key_ref = format!("provider:{id}");
    provider.timeout_secs = 10;
    provider
}

pub(super) fn model(provider: &str, upstream: &str, priority: i32, tier: ModelTier) -> ModelEntry {
    let mut entry =
        ModelEntry::for_upstream(provider, upstream, Some(tier)).with_priority(priority);
    entry.pricing = Some(Pricing::new("USD", 3.0, 15.0));
    entry
}

/// Two healthy candidates in the standard tier, plus the awkward models the
/// lifecycle tests need: one that never finishes streaming and one that always
/// fails. Those live in the fast tier so they never join a standard-tier plan.
pub(super) fn config_for(mock: SocketAddr) -> AppConfig {
    let mut cfg = AppConfig::default();
    // These tests exercise live-stream lifecycle semantics. The staged Codex
    // mode has separate offline gates in codex_auto_e2e_test.rs.
    cfg.routing.rectifier.codex_auto = false;
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();
    cfg.providers = vec![
        provider("alpha", "Alpha", mock),
        provider("beta", "Beta", mock),
    ];
    cfg.models = vec![
        model("alpha", "good-model", 10, ModelTier::Standard),
        model("beta", "good-model", 10, ModelTier::Standard),
        model("alpha", "hold-model", 10, ModelTier::Fast),
        model("beta", "hold-model", 10, ModelTier::Fast),
        model("beta", "broken-model", 10, ModelTier::Fast),
        model("alpha", "cancel-model", 10, ModelTier::Fast),
        model("alpha", "truncate-model", 10, ModelTier::Fast),
        model("alpha", "truncate-usage-model", 10, ModelTier::Fast),
        model("alpha", "empty-stream-model", 10, ModelTier::Fast),
        model("alpha", "blind-model", 10, ModelTier::Fast),
    ];
    cfg
}

pub(super) struct Harness {
    pub(super) base: String,
    pub(super) server: Option<ServerHandle>,
    pub(super) state: Arc<AppState>,
    pub(super) client: reqwest::Client,
    pub(super) mock: Mock,
}

impl Harness {
    pub(super) async fn start_with_secrets(
        cfg: AppConfig,
        mock: Mock,
        secrets: Arc<MemorySecretStore>,
    ) -> Harness {
        let state = Arc::new(AppState::new(cfg, secrets));
        let server = ServerHandle::start(Arc::clone(&state)).await.unwrap();
        Harness {
            base: format!("http://{}", server.addr),
            server: Some(server),
            state,
            client: reqwest::Client::builder()
                .pool_max_idle_per_host(0)
                .build()
                .unwrap(),
            mock,
        }
    }

    pub(super) async fn start(cfg: AppConfig, mock: Mock) -> Harness {
        Harness::start_with_secrets(
            cfg,
            mock,
            Arc::new(
                MemorySecretStore::new()
                    .with("provider:alpha", "sk-alpha")
                    .with("provider:beta", "sk-beta"),
            ),
        )
        .await
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

    /// A buffered main-traffic request.
    pub(super) async fn ask(&self, model: &str) -> reqwest::Response {
        self.post("/v1/messages")
            .json(&json!({
                "model": model,
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .send()
            .await
            .unwrap()
    }

    /// A streaming main-traffic request.
    pub(super) async fn ask_streaming(&self, model: &str) -> reqwest::Response {
        self.post("/v1/messages")
            .json(&json!({
                "model": model,
                "max_tokens": 16,
                "stream": true,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .send()
            .await
            .unwrap()
    }

    /// A Responses API request body for a buffered or streaming answer.
    pub(super) fn responses_request(
        &self,
        model: &str,
        store: Option<bool>,
        stream: bool,
    ) -> reqwest::RequestBuilder {
        let mut body = json!({
            "model": model,
            "input": [{"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "DO_NOT_STORE_MARKER"}
            ]}],
        });
        body["stream"] = json!(stream);
        if let Some(store) = store {
            body["store"] = json!(store);
        }
        self.post("/v1/responses").json(&body)
    }

    /// `GET /v1/responses/{id}`.
    pub(super) fn get_response(&self, id: &str) -> reqwest::RequestBuilder {
        self.client
            .get(format!("{}/v1/responses/{id}", self.base))
            .header("x-api-key", TOKEN)
    }

    pub(super) fn records(&self) -> Vec<RequestRecord> {
        self.state.stats().recent(100)
    }

    pub(super) fn outcomes(&self) -> Vec<Outcome> {
        self.state.outcomes().recent(100)
    }

    /// The one outcome a single request produced.
    pub(super) fn outcome(&self) -> Outcome {
        let outcomes = self.outcomes();
        assert_eq!(
            outcomes.len(),
            1,
            "expected exactly one terminal outcome, got {:?}",
            outcomes
                .iter()
                .map(|outcome| (outcome.request_id.clone(), outcome.final_status))
                .collect::<Vec<_>>()
        );
        outcomes.into_iter().next().unwrap()
    }

    pub(super) fn record(&self) -> RequestRecord {
        let records = self.records();
        assert_eq!(records.len(), 1, "expected exactly one activity record");
        records.into_iter().next().unwrap()
    }

    /// Total spend booked against the global budget scope today.
    pub(super) fn spent_today(&self) -> f64 {
        self.state
            .ledger()
            .totals_for(&BudgetScope::Global, chrono::Local::now())
            .into_iter()
            .filter(|(period, _)| *period == BudgetPeriod::Day)
            .map(|(_, cost)| cost.amount)
            .sum()
    }

    pub(super) async fn shutdown(mut self) {
        if let Some(server) = self.server.take() {
            server.stop().await;
        }
    }
}

/// Wait until the lifecycle has recorded a request's terminal transition.
///
/// A client disconnect is noticed by the server when the connection goes away,
/// which is asynchronous by nature, so the assertions poll instead of guessing
/// how long that takes.
pub(super) async fn wait_for_outcomes(harness: &Harness, expected: usize) {
    for _ in 0..200 {
        if harness.state.outcomes().len() == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "expected {expected} terminal outcomes, saw {} (records: {})",
        harness.state.outcomes().len(),
        harness.state.stats().recent(10).len()
    );
}

/// Two candidates in the fast tier, tried in the order given, so a failover
/// scenario can name exactly what failed first and what the request fell
/// through to.
pub(super) fn failover_config(mock: SocketAddr, first: &str, second: &str) -> AppConfig {
    let mut cfg = config_for(mock);
    cfg.models = vec![
        model("alpha", first, 0, ModelTier::Fast),
        model("beta", second, 10, ModelTier::Fast),
    ];
    cfg
}
