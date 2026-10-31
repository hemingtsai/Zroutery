//! Production routes against local mocks, never real credentials or paid models.
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::server::{AppState, ServerHandle};

#[derive(Clone, Default)]
struct Mock {
    bodies: Arc<Mutex<Vec<Value>>>,
}

async fn chat_upstream(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    mock.bodies.lock().unwrap().push(body.clone());
    let name = body["tools"][0]["function"]["name"]
        .as_str()
        .unwrap_or("functions__exec");
    let chunk = |delta: Value, finish: Value| {
        format!(
            "data: {}\n\n",
            json!({"id":"chat-mock","model":"chat-custom",
        "choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
        )
    };
    let sse = chunk(
        json!({"tool_calls":[{"index":0,"id":"call","type":"function","function":{"name":name,"arguments":""}}]}),
        Value::Null,
    ) + &chunk(
        json!({"tool_calls":[{"index":0,"function":{"arguments":"{\"input\":\"echo hello\"}"}}]}),
        Value::Null,
    ) + &chunk(json!({}), json!("tool_calls"))
        + &format!(
            "data: {}\n\n",
            json!({"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":3}})
        )
        + "data: [DONE]\n\n";
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from(sse))
        .unwrap()
}

fn frame(kind: &str, value: Value) -> String {
    format!("event: {kind}\ndata: {value}\n\n")
}

async fn upstream(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    let model = body["model"].as_str().unwrap_or_default();
    let count = {
        let mut bodies = mock.bodies.lock().unwrap();
        bodies.push(body.clone());
        bodies.iter().filter(|body| body["model"] == model).count()
    };
    let encrypted = json!({"type":"invalid_request_error","code":"invalid_encrypted_content",
        "message":"The encrypted content could not be verified."});
    if model == "cancel-backoff" {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error":encrypted})),
        )
            .into_response();
    }
    if count == 1 && matches!(model, "http-cleanup" | "unsafe") {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error":encrypted})),
        )
            .into_response();
    }
    if count == 1 && model == "sse-cleanup" {
        let sse = frame(
            "response.output_item.done",
            json!({"type":"response.output_item.done",
            "item":{"type":"custom_tool_call","call_id":"must_not_execute","name":"exec","input":"danger"}}),
        ) + &frame(
            "response.failed",
            json!({"type":"response.failed","response":{"error":encrypted}}),
        );
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(sse))
            .unwrap();
    }
    if count == 1 && model == "missing-error" {
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(frame(
                "response.failed",
                json!({"type":"response.failed","response":{"error":null}}),
            )))
            .unwrap();
    }
    if model == "quota" {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":{"code":"insufficient_quota","message":"Quota exhausted"}})),
        )
            .into_response();
    }
    if model == "cancel" {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(1);
        tokio::spawn(async move {
            let _ = tx
                .send(frame(
                    "response.output_text.delta",
                    json!({"type":"response.output_text.delta","delta":"must_wait"}),
                ))
                .await;
            std::future::pending::<()>().await;
        });
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv()
                .await
                .map(|part| (Ok::<_, std::io::Error>(part), rx))
        });
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from_stream(stream))
            .unwrap();
    }
    let mut response = json!({"id":format!("upstream-{count}"),"object":"response","status":"completed","model":model,
        "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"only_success"}]}],
        "usage":{"input_tokens":10,"output_tokens":2}});
    if model == "portable-history" {
        response["output"].as_array_mut().unwrap().push(json!({
            "type":"reasoning", "id":"rs-fresh", "summary":[],
            "encrypted_content":"fresh_fixture"
        }));
    }
    if body["stream"] == true {
        let sse = frame(
            "response.created",
            json!({"type":"response.created","response":{"id":response["id"],"model":model,"output":[]}}),
        ) + &frame(
            "response.output_text.delta",
            json!({"type":"response.output_text.delta","delta":"only_success"}),
        ) + &frame(
            "response.completed",
            json!({"type":"response.completed","response":response}),
        );
        Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(sse))
            .unwrap()
    } else {
        Json(response).into_response()
    }
}

struct Harness {
    mock: Mock,
    state: Arc<AppState>,
    gateway: ServerHandle,
    upstream: tokio::task::JoinHandle<()>,
    client: reqwest::Client,
}

impl Harness {
    async fn new() -> Self {
        let mock = Mock::default();
        let routes = Router::new()
            .route("/v1/responses", post(upstream))
            .route("/v1/responses/compact", post(upstream))
            .route("/v1/chat/completions", post(chat_upstream))
            .with_state(mock.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        let mut config = AppConfig::default();
        config.server.port = 0;
        config.server.require_auth = false;
        config.server.bypass_proxy = true;
        let mut provider = ProviderConfig::new("relay", "Mock", ProviderKind::OpenAICompatible);
        provider.api_surface = serde_json::from_value(json!("responses")).unwrap();
        provider.base_url = format!("http://{addr}");
        let secrets = MemorySecretStore::new().with(provider.key_ref.clone(), "offline-key");
        config.providers.push(provider);
        let mut chat_provider =
            ProviderConfig::new("chat", "Mock Chat", ProviderKind::OpenAICompatible);
        chat_provider.base_url = format!("http://{addr}");
        config.providers.push(chat_provider);
        for name in [
            "http-cleanup",
            "sse-cleanup",
            "missing-error",
            "unsafe",
            "quota",
            "healthy",
            "portable-history",
            "cancel",
            "cancel-backoff",
        ] {
            let mut model = ModelEntry::for_upstream("relay", name, None);
            model.capabilities.tools = true;
            model.capabilities.thinking = true;
            config.models.push(model);
        }
        let mut model = ModelEntry::for_upstream("chat", "custom", None);
        model.capabilities.tools = true;
        config.models.push(model);
        for (name, tier) in [
            ("tier-chat-fast", ModelTier::Fast),
            ("tier-chat", ModelTier::Standard),
            ("tier-chat-reasoning", ModelTier::Reasoning),
        ] {
            let mut chat_tier = ModelEntry::for_upstream("chat", name, Some(tier));
            chat_tier.capabilities.tools = true;
            config.models.push(chat_tier);
        }
        let mut native_tier =
            ModelEntry::for_upstream("relay", "tier-native", Some(ModelTier::Frontier));
        native_tier.capabilities.tools = true;
        config.models.push(native_tier);
        let secrets = secrets.with("provider:chat", "offline-key");
        let state = Arc::new(AppState::new(config, Arc::new(secrets)));
        let gateway = ServerHandle::start(Arc::clone(&state)).await.unwrap();
        Self {
            mock,
            state,
            gateway,
            upstream: task,
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
        }
    }
    async fn send(&self, name: &str, stream: bool) -> reqwest::Response {
        self.client
            .post(format!("http://{}/v1/responses", self.gateway.addr))
            .json(&history(name, stream))
            .send()
            .await
            .unwrap()
    }
    async fn stop(self) {
        self.gateway.stop().await;
        self.upstream.abort();
    }
}

fn history(name: &str, stream: bool) -> Value {
    json!({"model":format!("relay-{name}"),"stream":stream,"store":true,"reasoning":{"effort":"xhigh"},
    "include":["reasoning.encrypted_content"],"input":[
        {"type":"reasoning","id":"rs","encrypted_content":"private_fixture","summary":[]},
        {"type":"message","id":"msg","role":"user","content":"hello"},
        {"type":"additional_tools","role":"developer","tools":[{"type":"function","name":"lookup","parameters":{"type":"object"}}]}
    ]})
}

#[tokio::test]
async fn native_http_cleanup_is_once_and_resets_for_the_next_request() {
    let h = Harness::new().await;
    let response = h.send("http-cleanup", false).await;
    assert_eq!(response.status(), 200);
    let wire: Value = response.json().await.unwrap();
    assert_eq!(wire["output"][0]["content"][0]["text"], "only_success");
    assert_eq!(h.send("http-cleanup", false).await.status(), 200);
    let bodies = h.mock.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[0]["input"], history("http-cleanup", false)["input"]);
    assert_eq!(bodies[1]["input"].as_array().unwrap().len(), 2);
    assert_eq!(bodies[1]["store"], false);
    assert_eq!(bodies[1]["reasoning"]["effort"], "xhigh");
    assert_eq!(bodies[2]["input"], bodies[0]["input"]);
    assert_eq!(h.state.outcomes().recent(10).len(), 2);
    let stats = h.state.codex_filter.snapshot();
    assert_eq!(stats.cleanup_requests, 1);
    assert_eq!(stats.removed_reasoning_items, 1);
    assert_eq!(stats.retries[0], 1);
    assert_eq!(stats.active_requests, 0);
    h.stop().await;
}

#[tokio::test]
async fn sse_cleanup_and_missing_details_discard_failed_output_before_retry() {
    let h = Harness::new().await;
    for model in ["sse-cleanup", "missing-error"] {
        let response = h.send(model, true).await;
        assert_eq!(response.status(), 200);
        let text = tokio::time::timeout(Duration::from_secs(10), response.text())
            .await
            .unwrap()
            .unwrap();
        assert!(text.contains("only_success"), "{text}");
        assert!(!text.contains("must_not_execute"));
        assert!(!text.contains("response.failed"));
        let bodies = h.mock.bodies.lock().unwrap().clone();
        let bodies: Vec<_> = bodies.iter().filter(|b| b["model"] == model).collect();
        assert_eq!(bodies.len(), 2);
        if model == "missing-error" {
            assert_eq!(bodies[0], bodies[1]);
        }
    }
    h.stop().await;
}

#[tokio::test]
async fn unsafe_context_and_quota_do_not_replay_or_silently_clean() {
    let h = Harness::new().await;
    let mut body = history("unsafe", false);
    body["input"] = json!([{"type":"item_reference","id":"old"}]);
    let response = h
        .client
        .post(format!("http://{}/v1/responses", h.gateway.addr))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("encrypted_context_not_portable"));
    assert_eq!(h.send("quota", false).await.status(), 429);
    assert_eq!(h.mock.bodies.lock().unwrap().len(), 2);
    h.stop().await;
}

#[tokio::test]
async fn native_compact_forwards_once_without_repair() {
    let h = Harness::new().await;
    let response = h
        .client
        .post(format!("http://{}/v1/responses/compact", h.gateway.addr))
        .json(&history("http-cleanup", false))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(h.mock.bodies.lock().unwrap().len(), 1);
    h.stop().await;
}

#[tokio::test]
async fn chat_bridge_restores_custom_tools_and_keeps_the_final_usage_trailer() {
    let h = Harness::new().await;
    let body = json!({"model":"chat-custom","stream":true,"store":false,"input":[
        {"role":"user","content":"run"},
        {"type":"additional_tools","role":"developer","tools":[{"type":"namespace","name":"functions","tools":[
            {"type":"custom","name":"exec","description":"Execute","format":{"type":"text"}}]}]}
    ]});
    let response = h
        .client
        .post(format!("http://{}/v1/responses", h.gateway.addr))
        .json(&body)
        .send()
        .await
        .unwrap();
    let text = response.text().await.unwrap();
    assert!(
        text.contains("response.custom_tool_call_input.delta"),
        "{text}"
    );
    assert!(text.contains("echo hello"));
    assert!(!text.contains("functions__exec"));
    let mut decoder = zroutery_core::protocol::SseDecoder::new();
    let frames = decoder.push(text.as_bytes());
    let terminal = frames.last().unwrap().json().unwrap();
    assert_eq!(terminal["response"]["usage"]["input_tokens"], 20);
    assert_eq!(
        terminal["response"]["output"][0]["type"],
        "custom_tool_call"
    );
    assert_eq!(terminal["response"]["output"][0]["namespace"], "functions");
    h.stop().await;
}

#[tokio::test]
async fn disconnect_cancels_the_buffered_attempt_without_replay() {
    let h = Harness::new().await;
    let response = h.send("cancel", true).await;
    // Wait for the request to reach the mock rather than relying on a timer.
    for _ in 0..100 {
        if !h.mock.bodies.lock().unwrap().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    drop(response);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(h.mock.bodies.lock().unwrap().len(), 1);
    assert_eq!(h.state.outcomes().recent(10).len(), 1);
    assert!(!h.state.outcomes().recent(1)[0].final_status.is_success());
    h.stop().await;
}

#[tokio::test]
async fn cancellation_during_cleanup_backoff_prevents_dispatch() {
    let h = Harness::new().await;
    let response = h.send("cancel-backoff", true).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while h.state.codex_filter.snapshot().cleanup_requests == 0 {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    drop(response);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(h.mock.bodies.lock().unwrap().len(), 1);
    let stats = h.state.codex_filter.snapshot();
    assert_eq!(stats.retries[0], 0);
    assert_eq!(stats.active_requests, 0);
    assert_eq!(stats.cancelled, 1);
    h.stop().await;
}

#[tokio::test]
async fn compressed_request_decodes_before_gateway_validation() {
    let h = Harness::new().await;
    // Fixed gzip fixture for {"model":"relay-healthy","input":"hello"}.
    let bytes: Vec<u8> = vec![
        31, 139, 8, 0, 0, 0, 0, 0, 0, 10, 171, 86, 202, 205, 79, 73, 205, 81, 178, 82, 42, 74, 205,
        73, 172, 212, 205, 72, 77, 204, 41, 201, 168, 84, 210, 81, 202, 204, 43, 40, 45, 81, 178,
        82, 202, 72, 205, 201, 201, 87, 170, 5, 0, 6, 218, 194, 59, 41, 0, 0, 0,
    ];
    let response = h
        .client
        .post(format!("http://{}/v1/responses", h.gateway.addr))
        .header("content-type", "application/json")
        .header("content-encoding", "gzip")
        .body(bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    h.stop().await;
}

#[tokio::test]
async fn explicit_cancel_endpoint_stops_staging_without_executable_output() {
    let h = Harness::new().await;
    let response = h.send("cancel", true).await;
    let id = response.headers()["x-zroutery-response-id"]
        .to_str()
        .unwrap()
        .to_string();
    let cancel = h
        .client
        .post(format!(
            "http://{}/v1/responses/{id}/cancel",
            h.gateway.addr
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(cancel.status(), 200);
    let text = response.text().await.unwrap();
    assert!(!text.contains("must_wait"));
    assert!(!text.contains("response.completed"));
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(h.state.codex_filter.snapshot().active_requests, 0);
    assert_eq!(h.state.codex_filter.snapshot().cancelled, 1);
    assert_eq!(
        h.state.response_store.get(&id).unwrap().status,
        zroutery_core::ir::ResponseStatus::Cancelled
    );
    h.stop().await;
}

#[tokio::test]
async fn disabling_auto_keeps_custom_tool_translation_without_auto_retries() {
    let h = Harness::new().await;
    let mut config = (*h.state.config()).clone();
    config.routing.rectifier.codex_auto = false;
    h.state.set_config(config);
    let body = json!({"model":"chat-custom","stream":true,"input":[
        {"role":"user","content":"run"},
        {"type":"additional_tools","role":"developer","tools":[{"type":"namespace","name":"functions","tools":[
            {"type":"custom","name":"exec","format":{"type":"text"}}]}]}]});
    let response = h
        .client
        .post(format!("http://{}/v1/responses", h.gateway.addr))
        .json(&body)
        .send()
        .await
        .unwrap();
    let text = response.text().await.unwrap();
    assert!(text.contains("custom_tool_call"));
    assert!(!text.contains("functions__exec"));
    assert_eq!(h.mock.bodies.lock().unwrap().len(), 1);
    h.stop().await;
}

async fn assert_history_portability(
    h: &Harness,
    stream: bool,
    portable: bool,
    custom: bool,
) -> Value {
    let mut request = history("portable-history", stream);
    request["include"] = json!([
        "reasoning.encrypted_content",
        "message.output_text.logprobs"
    ]);
    request["input"].as_array_mut().unwrap().splice(
        1..1,
        [
            json!({"type":"message", "id":"msg-prior", "role":"assistant", "phase":"commentary",
                "content":[{"type":"output_text", "text":"prior answer"}]}),
            json!({"type":"function_call", "id":"fc-prior", "call_id":"pair", "name":"lookup",
                "arguments":"{\"id\":\"nested\"}"}),
            json!({"type":"function_call_output", "id":"out-prior", "call_id":"pair", "output":"done"}),
        ],
    );
    if custom {
        request["tools"] = json!([{"type":"namespace", "name":"functions", "tools":[
            {"type":"custom", "name":"exec", "format":{"type":"text"}}
        ]}]);
        request["input"].as_array_mut().unwrap().splice(
            4..4,
            [
                json!({"type":"custom_tool_call", "id":"custom-prior", "call_id":"custom-pair",
                    "namespace":"functions", "name":"exec", "input":"echo hello"}),
                json!({"type":"custom_tool_call_output", "id":"custom-out-prior",
                    "call_id":"custom-pair", "output":"hello"}),
            ],
        );
    }
    let mut expected = request.clone();
    expected["model"] = json!("portable-history");
    if portable {
        expected["store"] = json!(false);
        expected["include"] = json!(["message.output_text.logprobs"]);
        let input = expected["input"].as_array_mut().unwrap();
        input.retain(|item| item["type"] != "reasoning");
        for item in input {
            item.as_object_mut().unwrap().remove("id");
        }
    }
    let before = h.mock.bodies.lock().unwrap().len();
    let response = h
        .client
        .post(format!("http://{}/v1/responses", h.gateway.addr))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    let wire: Value = if stream {
        let mut decoder = zroutery_core::protocol::SseDecoder::new();
        decoder
            .push(text.as_bytes())
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.completed"))
            .unwrap()
            .json()
            .unwrap()["response"]
            .clone()
    } else {
        serde_json::from_str(&text).unwrap()
    };
    let bodies = h.mock.bodies.lock().unwrap().clone();
    assert_eq!(
        bodies.len(),
        before + 1,
        "portability must apply before the first send"
    );
    assert_eq!(
        bodies.last().unwrap(),
        &expected,
        "portable={portable}, stream={stream}, custom={custom}"
    );
    wire
}

#[tokio::test]
async fn portable_history_is_honoured_on_first_send_with_or_without_auto() {
    let h = Harness::new().await;
    for auto in [true, false] {
        for portable in [false, true] {
            let mut config = (*h.state.config()).clone();
            config.routing.rectifier.codex_auto = auto;
            config
                .providers
                .iter_mut()
                .find(|p| p.id == "relay")
                .unwrap()
                .quirks
                .portable_history = portable;
            h.state.set_config(config);
            for stream in [false, true] {
                let wire = assert_history_portability(&h, stream, portable, false).await;
                if auto {
                    assert_eq!(
                        wire["output"][1]["encrypted_content"], "fresh_fixture",
                        "staging must preserve new reasoning verbatim; only history is stripped"
                    );
                }
            }
        }
    }
    h.stop().await;
}

#[tokio::test]
async fn portable_history_is_honoured_when_only_custom_tool_staging_is_enabled() {
    let h = Harness::new().await;
    for portable in [false, true] {
        let mut config = (*h.state.config()).clone();
        config.routing.rectifier.enabled = false;
        config
            .providers
            .iter_mut()
            .find(|p| p.id == "relay")
            .unwrap()
            .quirks
            .portable_history = portable;
        h.state.set_config(config);
        let wire = assert_history_portability(&h, true, portable, true).await;
        assert_eq!(wire["output"][1]["encrypted_content"], "fresh_fixture");
    }
    h.stop().await;
}
#[tokio::test]
async fn native_only_fast_standard_and_reasoning_escalate_to_frontier_without_dropping_context() {
    let h = Harness::new().await;
    for requested in ["fast-class", "standard-class", "reasoning-class"] {
        let request = json!({
            "model": requested,
            "stream": false,
            "conversation": "conversation-fixture",
            "context_management": [{"type":"compaction","compact_threshold":120000}],
            "tools": [{"type":"web_search_preview","search_context_size":"medium"}],
            "input": "find the current fixture"
        });
        let before = h.mock.bodies.lock().unwrap().len();
        let response = h
            .client
            .post(format!("http://{}/v1/responses", h.gateway.addr))
            .json(&request)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.text().await.unwrap();
        assert_eq!(status, 200, "requested={requested}: {body}");
        assert_eq!(headers["x-zroutery-tier"], "frontier");
        assert_eq!(headers["x-zroutery-model"], "relay-tier-native");

        let bodies = h.mock.bodies.lock().unwrap().clone();
        assert_eq!(
            bodies.len(),
            before + 1,
            "requested={requested}: incompatible Chat candidates must not be attempted"
        );
        let sent = bodies.last().unwrap();
        assert_eq!(sent["model"], "tier-native");
        assert_eq!(sent["conversation"], request["conversation"]);
        assert_eq!(sent["context_management"], request["context_management"]);
        assert_eq!(sent["tools"], request["tools"]);

        let record = h.state.stats().recent(1).pop().unwrap();
        assert_eq!(record.escalated_from.as_deref(), Some(requested));
        assert_eq!(record.resolved_model.as_deref(), Some("relay-tier-native"));
        let decision = record.routing_decision.as_ref().unwrap();
        assert_eq!(decision.selected.as_deref(), Some("relay-tier-native"));
        let projected = decision.planned_identity().unwrap();
        assert_eq!(projected.model_id, "relay-tier-native");
        assert_eq!(projected.provider_id, "relay");
    }
    h.stop().await;
}

#[tokio::test]
async fn native_only_direct_chat_model_fails_before_any_upstream_send() {
    let h = Harness::new().await;
    let before = h.mock.bodies.lock().unwrap().len();
    let response = h
        .client
        .post(format!("http://{}/v1/responses", h.gateway.addr))
        .json(&json!({
            "model":"chat-custom",
            "stream":false,
            "conversation":"conversation-fixture",
            "tools":[{"type":"web_search_preview"}],
            "input":"plain request"
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, 503, "{body}");
    assert!(body.contains("Responses context or hosted tools require a Responses upstream"));
    assert_eq!(h.mock.bodies.lock().unwrap().len(), before);
    h.stop().await;
}
#[tokio::test]
async fn ordinary_standard_request_keeps_the_standard_chat_candidate() {
    let h = Harness::new().await;
    let before = h.mock.bodies.lock().unwrap().len();
    let response = h
        .client
        .post(format!("http://{}/v1/responses", h.gateway.addr))
        .json(&json!({"model":"standard-class","stream":false,"input":"plain request"}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.text().await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(headers["x-zroutery-tier"], "standard");
    assert_eq!(headers["x-zroutery-model"], "chat-tier-chat");

    let bodies = h.mock.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), before + 1);
    assert_eq!(bodies.last().unwrap()["model"], "tier-chat");
    assert!(h.state.stats().recent(1)[0].escalated_from.is_none());
    h.stop().await;
}
