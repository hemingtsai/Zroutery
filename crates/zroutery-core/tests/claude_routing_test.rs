//! Native Claude request controls through the real gateway to local fake relays.
//! No live credentials, external inference, or executable tool actions.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::server::{AppState, ServerHandle};

#[derive(Clone, Default)]
struct Mock(Arc<Mutex<Vec<Value>>>);

async fn chat(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    mock.0.lock().unwrap().push(body.clone());
    if body["stream"] != true {
        return Json(json!({"id":"mock-chat", "model":body["model"],
            "choices":[{"message":{"role":"assistant", "content":"local accepted"}, "finish_reason":"stop"}],
            "usage":{"prompt_tokens":20,"completion_tokens":3}})).into_response();
    }
    let chunk = |delta: Value, finish: Value| {
        format!(
            "data: {}\n\n",
            json!({"id":"mock-chat", "model":body["model"],
        "choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
        )
    };
    let sse = chunk(
        json!({"role":"assistant","content":"local accepted"}),
        Value::Null,
    ) + &chunk(json!({}), json!("stop"))
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

async fn responses(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    mock.0.lock().unwrap().push(body.clone());
    let response = json!({"id":"mock-responses","model":body["model"],"status":"completed","object":"response",
        "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"local accepted"}]}],
        "usage":{"input_tokens":20,"output_tokens":3}});
    if body["stream"] != true {
        return Json(response).into_response();
    }
    let frame = |kind: &str, value: Value| format!("event: {kind}\ndata: {value}\n\n");
    let sse = frame(
        "response.created",
        json!({"type":"response.created","response":{"id":"mock-responses","model":body["model"],"output":[]}}),
    ) + &frame(
        "response.output_text.delta",
        json!({"type":"response.output_text.delta","delta":"local accepted"}),
    ) + &frame(
        "response.completed",
        json!({"type":"response.completed","response":response}),
    );
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from(sse))
        .unwrap()
}

async fn harness() -> (
    Arc<AppState>,
    ServerHandle,
    Mock,
    tokio::task::JoinHandle<()>,
) {
    let mock = Mock::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let routes = axum::Router::new()
        .route("/v1/chat/completions", post(chat))
        .route("/v1/responses", post(responses))
        .with_state(mock.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    let mut config = AppConfig::default();
    config.server.port = 0;
    config.server.require_auth = false;
    config.server.bypass_proxy = true;
    config.routing.max_attempts = 1;
    for (id, surface) in [("chat", "chat"), ("responses", "responses")] {
        let mut provider = ProviderConfig::new(id, id, ProviderKind::OpenAICompatible);
        provider.base_url = format!("http://{addr}");
        provider.key_ref.clear();
        provider.api_surface = serde_json::from_value(json!(surface)).unwrap();
        // Do not rely on a quirk to retain an explicit native effort.
        provider.quirks.send_reasoning_effort = false;
        config.providers.push(provider);
    }
    for tier in ModelTier::ALL {
        let id = if matches!(tier, ModelTier::Reasoning | ModelTier::Frontier) {
            "responses"
        } else {
            "chat"
        };
        let mut unsupported =
            ModelEntry::for_upstream(id, format!("{}-limited", tier.as_str()), Some(tier));
        unsupported.capabilities.thinking = true;
        unsupported.effort_levels = vec!["low".into()];
        unsupported.priority = 0;
        let mut supported =
            ModelEntry::for_upstream(id, format!("{}-max", tier.as_str()), Some(tier));
        supported.capabilities.thinking = true;
        supported.effort_levels = vec!["low".into(), "high".into(), "max".into()];
        supported.priority = 10;
        config.models.extend([unsupported, supported]);
    }
    let state = Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())));
    let gateway = ServerHandle::start(state.clone()).await.unwrap();
    (state, gateway, mock, server)
}

#[tokio::test]
async fn all_four_classes_keep_claude_max_before_the_attempt_cap_on_buffered_and_live_streams() {
    let (state, gateway, mock, server) = harness().await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for tier in ModelTier::ALL {
        for stream in [false, true] {
            let response = client.post(format!("http://{}/v1/messages",gateway.addr)).json(&json!({
                "model":tier.virtual_id(), "stream":stream,"max_tokens":8000,
                "thinking":{"type":"adaptive"},"output_config":{"effort":"max"},
                "system":"fixture instruction", "messages":[
                    {"role":"user","content":"prior fixture"},
                    {"role":"assistant","content":"prior answer"},
                    {"role":"system","content":[{"type":"text","text":"mid-turn reminder","cache_control":{"type":"ephemeral"}}]},
                    {"role":"user","content":"local test"}
                ]
            })).send().await.unwrap();
            assert_eq!(
                response.status(),
                200,
                "{} stream={stream}",
                tier.virtual_id()
            );
            assert!(response.text().await.unwrap().contains("local accepted"));
            let sent = mock.0.lock().unwrap().last().cloned().unwrap();
            assert_eq!(sent["model"], format!("{}-max", tier.as_str()));
            let effort = sent
                .get("reasoning_effort")
                .or_else(|| sent.pointer("/reasoning/effort"))
                .unwrap();
            assert_eq!(effort, "max");
            assert!(
                sent.get("thinking").is_none(),
                "no fabricated numeric budget on OpenAI wires"
            );
            if let Some(input) = sent["input"].as_array() {
                assert_eq!(input[2]["role"], "system");
            } else {
                assert_eq!(sent["messages"][3]["role"], "system");
            }
        }
    }
    assert_eq!(mock.0.lock().unwrap().len(), 8);
    assert!(state
        .stats()
        .recent(8)
        .iter()
        .all(|record| record.attempts == 1 && record.ok));
    gateway.stop().await;
    server.abort();
}

#[tokio::test]
async fn a_real_effort_gap_returns_non_retryable_422_without_sending_or_lowering() {
    let (_, gateway, mock, server) = harness().await;
    let response = reqwest::Client::builder().no_proxy().build().unwrap().post(format!("http://{}/v1/messages",gateway.addr))
        .json(&json!({"model":"frontier-class","max_tokens":8000,"messages":[{"role":"user","content":"local test"}],
            "thinking":{"type":"adaptive"},"output_config":{"effort":"medium"}})).send().await.unwrap();
    assert_eq!(response.status(), 422);
    assert_eq!(response.headers()["x-should-retry"], "false");
    let body: Value = response.json().await.unwrap();
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("not lowered"));
    assert!(mock.0.lock().unwrap().is_empty());
    gateway.stop().await;
    server.abort();
}
