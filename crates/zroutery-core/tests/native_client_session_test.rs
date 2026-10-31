//! Native Claude Code and Codex session renewal against local relay mocks.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ProviderConfig, ProviderKind,
};
use zroutery_core::ir::Dialect;
use zroutery_core::protocol::{self, SseFrame};
use zroutery_core::server::{AppState, ServerHandle};

const OLD: &str = "00000000-0000-4000-8000-000000000001";
const LEGACY_PREFIX: &str = "user_device-kept_account_account-kept";

#[derive(Clone, Copy, Debug)]
enum Cli {
    Codex,
    ClaudeJson,
    ClaudeLegacy,
    ClaudeHeader,
}

impl Cli {
    fn source(self) -> Dialect {
        if matches!(self, Self::Codex) {
            Dialect::OpenAIResponses
        } else {
            Dialect::Anthropic
        }
    }

    fn user(self, session: &str) -> String {
        match self {
            Self::ClaudeJson => json!({"device_id":"device-kept","account_uuid":"account-kept",
                "session_id":session,"extension":{"keep":true}})
            .to_string(),
            Self::ClaudeLegacy => format!("{LEGACY_PREFIX}_session_{session}"),
            _ => "ordinary-account".to_string(),
        }
    }

    fn request(self, stream: bool) -> Value {
        if matches!(self, Self::Codex) {
            json!({"model":"p-bound","stream":stream,"store":false,"prompt_cache_key":OLD,
            "include":["reasoning.encrypted_content"], "tools":[{"type":"function","name":"inspect","parameters":{"type":"object"}}],
            "input":[
                {"type":"reasoning","id":"rs-old","encrypted_content":"opaque-kept","summary":[]},
                {"type":"message","id":"msg-old","role":"assistant","phase":"commentary",
                    "content":[{"type":"output_text","text":"prior answer"}]},
                {"type":"function_call","id":"fc-old","call_id":"call-kept","name":"inspect","arguments":"{\"id\":\"nested-kept\"}"},
                {"type":"function_call_output","id":"out-old","call_id":"call-kept","output":"result-kept"},
                {"role":"user","content":"continue"}
            ]})
        } else {
            json!({"model":"p-bound","stream":stream,"max_tokens":2048,
            "system":[{"type":"text","text":"instructions-kept"}],
            "metadata":{"user_id":self.user(OLD),"application":"kept"},
            "tools":[{"name":"inspect","input_schema":{"type":"object"}}],
            "messages":[
                {"role":"assistant","content":[
                    {"type":"thinking","thinking":"prior reasoning","signature":"signature-kept"},
                    {"type":"tool_use","id":"call-kept","name":"inspect","input":{"id":"nested-kept"}}
                ]},
                {"role":"user","content":[
                    {"type":"tool_result","tool_use_id":"call-kept","content":"result-kept"},
                    {"type":"text","text":"continue"}
                ]}
            ]})
        }
    }
}

fn frame(event: &str, body: Value) -> String {
    format!("event: {event}\ndata: {body}\n\n")
}

#[derive(Clone)]
struct Mock {
    target: Dialect,
    status: u16,
    sse: bool,
    calls: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
}

fn metadata_session(body: &Value) -> Option<String> {
    let user = body
        .pointer("/metadata/user_id")
        .or_else(|| body.get("user"))?
        .as_str()?;
    if let Ok(value) = serde_json::from_str::<Value>(user) {
        return value["session_id"].as_str().map(str::to_string);
    }
    user.rsplit_once("_session_").map(|(_, id)| id.to_string())
}

async fn upstream(
    State(mock): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    mock.calls
        .lock()
        .unwrap()
        .push((headers.clone(), body.clone()));
    let session = headers
        .get("x-claude-code-session-id")
        .or_else(|| headers.get("session_id"))
        .and_then(|id| id.to_str().ok())
        .map(str::to_string)
        .or_else(|| metadata_session(&body));
    if session.as_deref().is_none_or(|id| id == OLD) {
        let error = json!({"type":"error","error":{"type":"invalid_request_error","code":30013,"message":"opaque error details"}});
        return if mock.sse {
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(axum::body::Body::from(frame("error", error)))
                .unwrap()
        } else {
            (StatusCode::from_u16(mock.status).unwrap(), Json(error)).into_response()
        };
    }
    let wire = match mock.target {
        Dialect::Anthropic => {
            json!({"id":"msg-usable","type":"message","role":"assistant","model":"bound",
            "content":[{"type":"text","text":"usable"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}})
        }
        Dialect::OpenAIResponses => {
            json!({"id":"resp-usable","object":"response","status":"completed","model":"bound",
            "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"usable"}]}],
            "usage":{"input_tokens":1,"output_tokens":1}})
        }
        _ => {
            json!({"id":"chat-usable","model":"bound","choices":[{"index":0,"message":{"role":"assistant","content":"usable"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":1,"completion_tokens":1}})
        }
    };
    if body["stream"] != true {
        return Json(wire).into_response();
    }
    let stream = match mock.target {
        Dialect::Anthropic => {
            frame(
                "message_start",
                json!({"type":"message_start","message":{"id":"msg-usable","type":"message","role":"assistant","model":"bound","content":[],"usage":{"input_tokens":1}}}),
            ) + &frame(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ) + &frame(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"usable"}}),
            ) + &frame(
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ) + &frame(
                "message_delta",
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}),
            ) + &frame("message_stop", json!({"type":"message_stop"}))
        }
        Dialect::OpenAIResponses => {
            frame(
                "response.created",
                json!({"type":"response.created","response":{"id":"resp-usable","output":[]}}),
            ) + &frame(
                "response.output_text.delta",
                json!({"type":"response.output_text.delta","delta":"usable"}),
            ) + &frame(
                "response.completed",
                json!({"type":"response.completed","response":wire}),
            )
        }
        _ => format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"id":"chat-usable","model":"bound",
            "choices":[{"index":0,"delta":{"content":"usable"},"finish_reason":"stop"}]})
        ),
    };
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from(stream))
        .unwrap()
}

struct Harness {
    cli: Cli,
    mock: Mock,
    state: Arc<AppState>,
    gateway: ServerHandle,
    upstream: tokio::task::JoinHandle<()>,
    client: reqwest::Client,
}

impl Harness {
    async fn new(cli: Cli, target: Dialect, status: u16, sse: bool) -> Self {
        let mock = Mock {
            target,
            status,
            sse,
            calls: Default::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let routes = Router::new()
            .route("/v1/messages", post(upstream))
            .route("/v1/responses", post(upstream))
            .route("/v1/chat/completions", post(upstream))
            .with_state(mock.clone());
        let upstream = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        let mut config = AppConfig::default();
        config.server.port = 0;
        config.server.require_auth = false;
        config.server.bypass_proxy = true;
        let kind = if target == Dialect::Anthropic {
            ProviderKind::Anthropic
        } else {
            ProviderKind::OpenAICompatible
        };
        let mut provider = ProviderConfig::new("p", "Local relay", kind);
        provider.base_url = format!("http://{address}");
        provider.key_ref.clear();
        if target == Dialect::OpenAIResponses {
            provider.api_surface = serde_json::from_value(json!("responses")).unwrap();
        }
        config.providers.push(provider);
        let mut model = ModelEntry::for_upstream("p", "bound", None);
        model.capabilities.thinking = true;
        model.capabilities.tools = true;
        config.models.push(model);
        let state = Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())));
        let gateway = ServerHandle::start(state.clone()).await.unwrap();
        Self {
            cli,
            mock,
            state,
            gateway,
            upstream,
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
        }
    }

    async fn send(&self, body: &Value, acknowledged: Option<&str>) -> reqwest::Response {
        let path = if self.cli.source() == Dialect::Anthropic {
            "messages"
        } else {
            "responses"
        };
        let mut request = self
            .client
            .post(format!("http://{}/v1/{path}", self.gateway.addr))
            .json(body)
            .timeout(Duration::from_secs(5));
        request = if matches!(self.cli, Cli::Codex) {
            request
                .header("user-agent", "codex_cli_rs/0.1.0")
                .header("originator", "codex_cli_rs")
                .header("session_id", OLD)
                .header("x-session-id", OLD)
        } else {
            let request = request
                .header("user-agent", "claude-cli/2.1.217")
                .header("x-app", "cli")
                .header("anthropic-beta", "claude-code-20250219");
            if matches!(self.cli, Cli::ClaudeHeader) {
                request.header("x-claude-code-session-id", OLD)
            } else {
                request
            }
        };
        if let Some(id) = acknowledged {
            request = request.header("x-zroutery-session-id", id);
        }
        request.send().await.unwrap()
    }

    async fn id(&self, body: &Value, acknowledged: Option<&str>) -> String {
        let response = self.send(body, acknowledged).await;
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers().clone();
        let text = response.text().await.unwrap();
        assert!(text.contains("usable"), "{text}");
        assert!(!text.contains("session_binding_invalidated"), "{text}");
        let id = if matches!(self.cli, Cli::Codex) && body["stream"] == true {
            let mut decoder = protocol::SseDecoder::new();
            let frames = decoder.push(text.as_bytes());
            let done = frames
                .iter()
                .find(|frame| frame.event.as_deref() == Some("response.completed"))
                .unwrap()
                .json()
                .unwrap();
            done["response"]["metadata"]["zroutery_session_id"]
                .as_str()
                .unwrap()
                .to_string()
        } else {
            let id = headers["x-zroutery-session-id"].to_str().unwrap();
            assert_eq!(headers["session_id"], id);
            if !matches!(self.cli, Cli::Codex) {
                assert_eq!(headers["x-claude-code-session-id"], id);
            }
            id.to_string()
        };
        assert_ne!(id, OLD);
        assert!(uuid::Uuid::parse_str(&id).is_ok());
        id
    }

    async fn stop(self) {
        self.gateway.stop().await;
        self.upstream.abort();
    }
}

#[tokio::test]
async fn claude_json_legacy_and_native_headers_refresh_without_changing_identity_or_history() {
    for cli in [Cli::ClaudeJson, Cli::ClaudeLegacy, Cli::ClaudeHeader] {
        for (status, sse) in [(400, false), (503, false), (200, false), (200, true)] {
            for stream in [false, true] {
                if sse && !stream {
                    continue;
                }
                let h = Harness::new(cli, Dialect::Anthropic, status, sse).await;
                let body = cli.request(stream);
                let id = h.id(&body, None).await;
                assert_eq!(h.mock.calls.lock().unwrap().len(), 2);
                assert_eq!(h.id(&body, None).await, id);
                assert_eq!(h.id(&body, Some(&id)).await, id);
                let calls = h.mock.calls.lock().unwrap().clone();
                assert_eq!(calls.len(), 4);
                for (headers, request) in &calls[1..] {
                    assert_eq!(headers["x-claude-code-session-id"], id);
                    assert_eq!(request["metadata"]["user_id"], cli.user(&id));
                    assert_eq!(request["metadata"]["application"], "kept");
                    assert_eq!(request["messages"], body["messages"]);
                    assert_eq!(request["system"], body["system"]);
                    assert_eq!(request["tools"], body["tools"]);
                }
                assert_eq!(
                    h.state
                        .outcomes()
                        .recent(3)
                        .iter()
                        .map(|o| o.attempts.len())
                        .sum::<usize>(),
                    4
                );
                h.stop().await;
            }
        }
    }
}

#[tokio::test]
async fn codex_native_session_and_cache_ids_refresh_without_discarding_reasoning_or_tool_pairs() {
    for (status, sse) in [(400, false), (503, false), (200, false), (200, true)] {
        for stream in [false, true] {
            if sse && !stream {
                continue;
            }
            let h = Harness::new(Cli::Codex, Dialect::OpenAIResponses, status, sse).await;
            let body = Cli::Codex.request(stream);
            let id = h.id(&body, None).await;
            assert_eq!(h.id(&body, None).await, id);
            assert_eq!(h.id(&body, Some(&id)).await, id);
            let calls = h.mock.calls.lock().unwrap().clone();
            assert_eq!(calls.len(), 4);
            for (headers, request) in &calls[1..] {
                assert_eq!(headers["session_id"], id);
                assert_eq!(headers["x-session-id"], id);
                assert_eq!(request["prompt_cache_key"], id);
                assert_eq!(request["input"], body["input"]);
                assert_eq!(request["include"], body["include"]);
            }
            assert_eq!(h.state.codex_filter.snapshot().cleanup_requests, 0);
            h.stop().await;
        }
    }
}

#[tokio::test]
async fn claude_metadata_session_also_refreshes_when_bridged_to_chat() {
    for cli in [Cli::ClaudeJson, Cli::ClaudeLegacy, Cli::ClaudeHeader] {
        for stream in [false, true] {
            let h = Harness::new(cli, Dialect::OpenAI, 400, false).await;
            let body = cli.request(stream);
            let id = h.id(&body, None).await;
            assert_eq!(h.id(&body, None).await, id);
            assert_eq!(h.id(&body, Some(&id)).await, id);
            for (headers, request) in h.mock.calls.lock().unwrap().iter().skip(1) {
                assert_eq!(headers["x-claude-code-session-id"], id);
                assert_eq!(request["user"], cli.user(&id));
            }
            h.stop().await;
        }
    }
}

#[test]
fn anthropic_json_and_sse_keep_numeric_binding_codes() {
    let body = json!({"type":"error","error":{"type":"invalid_request_error","code":30013,"message":"opaque details"}});
    let error = protocol::anthropic::decode_response(body.clone()).unwrap_err();
    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    let mut parser = protocol::stream_parser(Dialect::Anthropic, "bound");
    let error = parser
        .push(&SseFrame {
            event: Some("error".into()),
            data: body.to_string(),
        })
        .unwrap_err();
    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        error.to_wire(Dialect::OpenAI)["error"]["code"],
        "session_binding_invalidated"
    );
}
