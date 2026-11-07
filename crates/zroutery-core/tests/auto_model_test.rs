//! Provider-local auto model metadata and Responses-to-Chat tool traffic.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelCapabilities, ModelEntry, ModelTier, ProviderConfig,
    ProviderKind,
};
use zroutery_core::ir::Capability;
use zroutery_core::models_dev::{enrich_from, parse};
use zroutery_core::protocol::SseDecoder;
use zroutery_core::registry::{Registry, Resolution};
use zroutery_core::server::{AppState, ServerHandle};
use zroutery_core::upstream::DiscoveredModel;

#[test]
fn importing_auto_without_provider_metadata_keeps_the_tool_default() {
    let catalog = parse(&json!({
        "tool-relay": {"models": {"auto": {"tool_call": true}}},
        "text-relay": {"models": {"auto": {"tool_call": false}}}
    }));
    let mut discovered = vec![DiscoveredModel {
        id: "auto".into(),
        ..Default::default()
    }];
    enrich_from(&catalog, &mut discovered, "lightvela", None);

    let mut entry = ModelEntry::for_upstream("lightvela", "auto", Some(ModelTier::Standard));
    // The catalogue import copies capability metadata only when discovery
    // published it. A foreign auto pool must not erase this constructor default.
    if let Some(capabilities) = &discovered[0].capabilities {
        entry.capabilities = capabilities.clone();
    }
    let config = AppConfig {
        providers: vec![ProviderConfig::new(
            "lightvela",
            "LightVela",
            ProviderKind::OpenAICompatible,
        )],
        models: vec![entry],
        ..Default::default()
    };
    let registry = Registry::new(Arc::new(config));
    let resolution = registry.resolve("lightvela-auto").unwrap();
    assert_eq!(resolution, Resolution::Direct("lightvela-auto".into()));
    let plan = zroutery_core::router::Router::new()
        .plan(&registry, &resolution, &[Capability::Tools])
        .unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].entry.upstream_model, "auto");
    assert!(!plan[0].degraded);
}

type Calls = Arc<Mutex<Vec<Value>>>;

async fn auto_chat(State(calls): State<Calls>, Json(body): Json<Value>) -> Response {
    calls.lock().unwrap().push(body.clone());
    if body["stream"] != true {
        return Json(json!({
            "id": "chatcmpl-auto-final", "object": "chat.completion", "model": "auto",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "done"},
                "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3}
        }))
        .into_response();
    }
    let chunk = |delta: Value, finish: Value| {
        format!(
            "data: {}\n\n",
            json!({"id": "chatcmpl-auto", "object": "chat.completion.chunk", "model": "auto",
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
        )
    };
    let sse = chunk(
        json!({"role": "assistant", "tool_calls": [{"index": 0, "id": "call-noop",
            "type": "function", "function": {"name": "diagnostic_noop", "arguments": "{\"ok\":true}"}}]}),
        Value::Null,
    ) + &chunk(json!({}), json!("tool_calls"))
        + &format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"id": "chatcmpl-auto", "model": "auto", "choices": [],
                "usage": {"prompt_tokens": 10, "completion_tokens": 4}})
        );
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from(sse))
        .unwrap()
}

#[tokio::test]
async fn lightvela_auto_tool_capability_fix_applies_without_restarting_the_gateway() {
    let calls: Calls = Default::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = Router::new()
        .route("/v1/chat/completions", post(auto_chat))
        .with_state(calls.clone());
    let task = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let mut provider =
        ProviderConfig::new("lightvela", "LightVela", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{address}/v1");
    provider.key_ref.clear();
    let mut entry = ModelEntry::for_upstream("lightvela", "auto", Some(ModelTier::Standard));
    entry.capabilities = ModelCapabilities::default();
    entry.effort_levels = vec!["high".into()];
    let mut config = AppConfig {
        providers: vec![provider],
        models: vec![entry],
        ..Default::default()
    };
    config.server.port = 0;
    config.server.require_auth = false;
    config.server.bypass_proxy = true;
    let state = Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())));
    let gateway = ServerHandle::start(state.clone()).await.unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("http://{}/v1/responses", gateway.addr);
    let mut body = json!({
        "model": "lightvela-auto", "input": "run the synthetic diagnostic",
        "stream": true, "store": false, "max_output_tokens": 256,
        "reasoning": {"effort": "high"},
        "tools": [{"type": "function", "name": "diagnostic_noop", "parameters": {"type": "object"}}],
        "tool_choice": {"type": "function", "name": "diagnostic_noop"}
    });

    let rejected = client.post(&url).json(&body).send().await.unwrap();
    assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        calls.lock().unwrap().is_empty(),
        "the old config rejects locally"
    );

    let mut fixed = (*state.config()).clone();
    fixed.models[0].capabilities.tools = true;
    fixed.models[0].capabilities.thinking = true;
    state.set_config(fixed);

    let response = client.post(&url).json(&body).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = response.text().await.unwrap();
    let frames = SseDecoder::new().push(text.as_bytes());
    assert!(!frames
        .iter()
        .any(|frame| matches!(frame.event.as_deref(), Some("error" | "response.failed"))));
    let completed = frames
        .iter()
        .find(|frame| frame.event.as_deref() == Some("response.completed"))
        .unwrap()
        .json()
        .unwrap();
    let function = completed["response"]["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call")
        .unwrap();
    assert_eq!(function["name"], "diagnostic_noop");
    assert_eq!(function["arguments"], "{\"ok\":true}");
    assert_eq!(completed["response"]["usage"]["input_tokens"], 10);
    assert_eq!(completed["response"]["usage"]["output_tokens"], 4);

    // Replay a synthetic result, without executing the requested tool, to
    // exercise the same tool-history capability gate on the next client turn.
    body["stream"] = json!(false);
    body["input"] = json!([
        {"role": "user", "content": "run the synthetic diagnostic"},
        function,
        {"type": "function_call_output", "call_id": function["call_id"], "output": "{\"ok\":true}"},
        {"role": "user", "content": "continue"}
    ]);
    let followup = client.post(&url).json(&body).send().await.unwrap();
    assert_eq!(followup.status(), StatusCode::OK);
    let followup: Value = followup.json().await.unwrap();
    assert_eq!(followup["output"][0]["content"][0]["text"], "done");
    let received = calls.lock().unwrap().clone();
    assert_eq!(received.len(), 2);
    for request in &received {
        assert_eq!(request["model"], "auto");
        assert_eq!(request["reasoning_effort"], "high");
        assert_eq!(request["tools"][0]["function"]["name"], "diagnostic_noop");
    }
    assert!(
        received[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool"
                && message["tool_call_id"] == function["call_id"])
    );
    gateway.stop().await;
    task.abort();
}
