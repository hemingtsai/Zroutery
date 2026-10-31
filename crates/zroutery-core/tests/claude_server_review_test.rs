//! Native server-review fields are opaque; no verdict or billing eligibility is
//! invented. Loopback fixtures only, and no command/tool is actually executed.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::server::{AppState, ServerHandle};

#[derive(Clone, Default)]
struct Mock(Arc<Mutex<Vec<(Value, HeaderMap)>>>);

fn results() -> Value {
    json!({"opaque_fixture":{"tool_use_id":"tool-original","result":"deny","extension":{"keep":true}}})
}

async fn upstream(
    State(mock): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    mock.0.lock().unwrap().push((body.clone(), headers));
    let response = json!({"id":"upstream-original","type":"message","role":"assistant","model":"mock",
        "content":[{"type":"tool_use","id":"tool-original","name":"Bash","input":{"command":"echo local fixture"},"future_field":{"keep":true}}],
        "stop_reason":"tool_use","usage":{"input_tokens":10,"output_tokens":1},"safeguard_results":results(),"future_top_level":{"keep":true}});
    if body["stream"] != true {
        let mut response = Json(response).into_response();
        response
            .headers_mut()
            .insert("anthropic-ratelimit-test", "fixture".parse().unwrap());
        response
            .headers_mut()
            .insert("x-should-retry", "false".parse().unwrap());
        return response;
    }
    let frame = |event: &str, data: Value| format!("event: {event}\ndata: {data}\n\n");
    let sse = frame("message_start",json!({"type":"message_start","message":{"id":"upstream-original","model":"mock",
        "role":"assistant","type":"message","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}))
        + &frame("content_block_start",json!({"type":"content_block_start","index":0,
            "content_block":{"type":"tool_use","id":"tool-original","name":"Bash","input":{},"future_field":{"keep":true}}}))
        + &frame("content_block_delta",json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}))
        + &frame("content_block_stop",json!({"type":"content_block_stop","index":0}))
        + &frame("message_delta",json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}))
        + &frame("future_review_progress",json!({"type":"future_review_progress","opaque":{"keep":true}}))
        // Results arriving at the final message_stop must not be cut off just
        // because the earlier delta already named a stop_reason.
        + &frame("message_stop",json!({"type":"message_stop","safeguard_results":results()}));
    Response::builder()
        .header("content-type", "text/event-stream")
        .header("anthropic-ratelimit-test", "fixture")
        .header("x-should-retry", "false")
        .body(axum::body::Body::from(sse))
        .unwrap()
}

#[tokio::test]
async fn native_review_preserves_request_headers_result_fields_tool_ids_and_final_stream_events() {
    let mock = Mock::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let routes = axum::Router::new()
        .route("/v1/messages", post(upstream))
        .with_state(mock.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    let mut config = AppConfig::default();
    config.server.port = 0;
    config.server.auth_token = "local-gateway-key".into();
    let mut provider = ProviderConfig::new("p", "Mock", ProviderKind::Anthropic);
    provider.base_url = format!("http://{address}");
    let secrets = MemorySecretStore::new().with(provider.key_ref.clone(), "own-upstream-key");
    config.providers.push(provider);
    config.models.push(ModelEntry::for_upstream(
        "p",
        "mock",
        Some(ModelTier::Standard),
    ));
    let state = Arc::new(AppState::new(config, Arc::new(secrets)));
    let gateway = ServerHandle::start(state.clone()).await.unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let safeguards = json!({"opaque_fixture":{"checks":["keep"]}});
    for stream in [false, true] {
        let response=client.post(format!("http://{}/v1/messages",gateway.addr)).header("x-api-key","local-gateway-key")
            .header("user-agent","claude-cli/2.1.281").header("anthropic-beta","auto-mode-classifier-future,opaque-future")
            .header("anthropic-future-metadata","unchanged").header("x-claude-code-session-id","fixture-session")
            .json(&json!({"model":"standard-class","stream":stream,"max_tokens":100,"safeguards":safeguards,
                "system":[{"type":"text","text":"original system","cache_control":{"type":"ephemeral"}}],
                "messages":[{"role":"user","content":"local fixture"}],"tools":[{"name":"Bash","input_schema":{"type":"object"}}]}))
            .send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["anthropic-ratelimit-test"], "fixture");
        assert_eq!(response.headers()["x-should-retry"], "false");
        let text = response.text().await.unwrap();
        if stream {
            assert!(text.contains("future_review_progress"));
            assert!(text.contains("tool-original"));
            let last = text
                .split("event: message_stop\ndata: ")
                .nth(1)
                .unwrap()
                .trim();
            let terminal: Value = serde_json::from_str(last).unwrap();
            assert_eq!(terminal["safeguard_results"], results());
        } else {
            let body: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(body["safeguard_results"], results());
            assert_eq!(body["content"][0]["id"], "tool-original");
            assert_eq!(body["content"][0]["future_field"]["keep"], true);
        }
    }
    {
        let received = mock.0.lock().unwrap();
        assert_eq!(
            received.len(),
            2,
            "no auxiliary classifier or replay was submitted"
        );
        for (body, headers) in received.iter() {
            assert_eq!(body["safeguards"], safeguards);
            assert_eq!(
                body["system"][0]["text"], "original system",
                "do not prepend a fabricated prompt block"
            );
            assert_eq!(
                headers["x-api-key"], "own-upstream-key",
                "gateway credentials are never forwarded as upstream credentials"
            );
            assert_eq!(headers["anthropic-future-metadata"], "unchanged");
            assert_eq!(headers["x-claude-code-session-id"], "fixture-session");
        }
    }
    assert!(state
        .stats()
        .recent(2)
        .iter()
        .all(|record| record.ok && record.kind.is_main()));
    gateway.stop().await;
    task.abort();
}
