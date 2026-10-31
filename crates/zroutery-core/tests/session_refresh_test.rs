//! Bounded Zroutery session renewal and client handoff against local mocks.

use std::sync::atomic::{AtomicBool, Ordering};
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
use zroutery_core::protocol::SseDecoder;
use zroutery_core::server::{AppState, ServerHandle};

#[derive(Clone)]
struct Mock {
    status: u16,
    sse: bool,
    recover: bool,
    calls: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
    barrier: Arc<Mutex<Option<Arc<tokio::sync::Barrier>>>>,
    late_error: Arc<AtomicBool>,
    crypto_error: Arc<AtomicBool>,
}

fn frame(event: &str, body: Value) -> String {
    format!("event: {event}\ndata: {body}\n\n")
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
    let error = json!({"error":{"code":30013,"type":"new_api_error","message":
        "session binding invalidated: the model this session is bound to can no longer serve it; restart the task with a new session id"}});
    if mock.late_error.load(Ordering::SeqCst) {
        let tool = json!({"id":"partial", "model":body["model"], "choices":[{"index":0,
            "delta":{"tool_calls":[{"index":0,"id":"already-delivered","type":"function",
                "function":{"name":"exec","arguments":"{}"}}]}, "finish_reason":null}]});
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(format!(
                "data: {tool}\n\ndata: {error}\n\n"
            )))
            .unwrap();
    }
    let renewed = headers
        .get("session_id")
        .and_then(|id| id.to_str().ok())
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
    if !renewed {
        let barrier = mock.barrier.lock().unwrap().clone();
        if let Some(barrier) = barrier {
            barrier.wait().await;
        }
    }
    if renewed
        && mock.crypto_error.load(Ordering::SeqCst)
        && body["input"]
            .as_array()
            .is_some_and(|input| input.iter().any(|item| item["type"] == "reasoning"))
    {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":{
            "type":"invalid_request_error", "code":"invalid_encrypted_content", "message":"encrypted history could not be verified"
        }}))).into_response();
    }
    if mock.recover && renewed {
        let native = body.get("input").is_some();
        let response = if native {
            json!({"id":"recovered", "model":body["model"], "status":"completed", "output":[
                {"type":"message", "role":"assistant", "content":[{"type":"output_text","text":"recovered"}]}
            ], "usage":{"input_tokens":1,"output_tokens":1}})
        } else {
            json!({"id":"recovered", "model":body["model"], "choices":[{
                "message":{"role":"assistant","content":"recovered"}, "finish_reason":"stop"
            }], "usage":{"prompt_tokens":1,"completion_tokens":1}})
        };
        if body["stream"] != true {
            return Json(response).into_response();
        }
        let stream = if native {
            frame(
                "response.created",
                json!({"type":"response.created","response":{"id":"recovered","output":[]}}),
            ) + &frame(
                "response.output_text.delta",
                json!({"type":"response.output_text.delta","delta":"recovered"}),
            ) + &frame(
                "response.completed",
                json!({"type":"response.completed","response":response}),
            )
        } else {
            format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"id":"recovered","model":body["model"],
                "choices":[{"index":0,"delta":{"content":"recovered"},"finish_reason":"stop"}]})
            )
        };
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(stream))
            .unwrap();
    }
    if mock.sse {
        Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(frame("error", error)))
            .unwrap()
    } else {
        (StatusCode::from_u16(mock.status).unwrap(), Json(error)).into_response()
    }
}

struct Harness {
    mock: Mock,
    state: Arc<AppState>,
    gateway: ServerHandle,
    upstream: tokio::task::JoinHandle<()>,
    client: reqwest::Client,
    dialect: Dialect,
    auto: bool,
}

impl Harness {
    async fn new(dialect: Dialect, status: u16, sse: bool, recover: bool, auto: bool) -> Self {
        let mock = Mock {
            status,
            sse,
            recover,
            calls: Default::default(),
            barrier: Default::default(),
            late_error: Default::default(),
            crypto_error: Default::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let routes = Router::new()
            .route("/v1/chat/completions", post(upstream))
            .route("/v1/responses", post(upstream))
            .with_state(mock.clone());
        let upstream = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        let mut config = AppConfig::default();
        config.server.port = 0;
        config.server.require_auth = false;
        config.server.bypass_proxy = true;
        config.routing.rectifier.codex_auto = auto;
        let mut provider = ProviderConfig::new("p", "Local NewAPI", ProviderKind::OpenAICompatible);
        provider.base_url = format!("http://{address}");
        provider.key_ref.clear();
        provider.api_surface =
            serde_json::from_value(json!(if dialect == Dialect::OpenAIResponses {
                "responses"
            } else {
                "chat"
            }))
            .unwrap();
        config.providers.push(provider);
        for name in ["bound", "sibling"] {
            let mut model = ModelEntry::for_upstream("p", name, None);
            model.capabilities.thinking = true;
            model.capabilities.tools = true;
            config.models.push(model);
        }
        let secrets = MemorySecretStore::new().with("second-key", "offline-key");
        let state = Arc::new(AppState::new(config, Arc::new(secrets)));
        let gateway = ServerHandle::start(state.clone()).await.unwrap();
        Self {
            mock,
            state,
            gateway,
            upstream,
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            dialect,
            auto,
        }
    }

    fn request(&self, model: &str, stream: bool) -> Value {
        if self.dialect == Dialect::OpenAIResponses {
            json!({"model":format!("p-{model}"),"stream":stream,"user":"client-session","prompt_cache_key":"client-session","input":[
                {"type":"reasoning","encrypted_content":"prior-cipher","summary":[]},
                {"role":"user","content":"preserve this conversation"}
            ]})
        } else {
            json!({"model":format!("p-{model}"),"stream":stream,"user":"client-session","messages":[
                {"role":"user","content":"preserve this conversation"}
            ]})
        }
    }

    async fn send(&self, body: &Value, acknowledged: Option<&str>) -> reqwest::Response {
        let path = if self.dialect == Dialect::OpenAIResponses {
            "responses"
        } else {
            "chat/completions"
        };
        let mut request = self
            .client
            .post(format!("http://{}/v1/{path}", self.gateway.addr))
            .header("user-agent", "opencode/2.0.32")
            .header("x-opencode-session", "client-session")
            .timeout(Duration::from_secs(5))
            .json(body);
        if let Some(id) = acknowledged {
            request = request.header("x-zroutery-session-id", id);
        }
        request.send().await.unwrap()
    }

    async fn recovered_id(&self, body: &Value, acknowledged: Option<&str>) -> String {
        let response = self.send(body, acknowledged).await;
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers().clone();
        let text = response.text().await.unwrap();
        assert!(text.contains("recovered"), "{text}");
        assert!(!text.contains("session_binding_invalidated"), "{text}");
        let id = if self.dialect == Dialect::OpenAIResponses && body["stream"] == true && self.auto
        {
            let mut decoder = SseDecoder::new();
            let frames = decoder.push(text.as_bytes());
            let completed = frames
                .iter()
                .find(|frame| frame.event.as_deref() == Some("response.completed"))
                .unwrap()
                .json()
                .unwrap();
            completed["response"]["metadata"]["zroutery_session_id"]
                .as_str()
                .unwrap()
                .to_string()
        } else {
            let id = headers
                .get("x-zroutery-session-id")
                .unwrap()
                .to_str()
                .unwrap();
            assert_eq!(headers["x-session-id"], id);
            id.to_string()
        };
        assert!(uuid::Uuid::parse_str(&id).is_ok());
        id
    }

    async fn stop(self) {
        self.gateway.stop().await;
        self.upstream.abort();
    }
}

#[tokio::test]
async fn refresh_returns_an_id_and_old_or_acknowledged_client_ids_reuse_it() {
    for dialect in [Dialect::OpenAI, Dialect::OpenAIResponses] {
        for auto in [false, true] {
            for (status, sse) in [(400, false), (503, false), (200, false), (200, true)] {
                for stream in [false, true] {
                    if sse && !stream {
                        continue;
                    }
                    let h = Harness::new(dialect, status, sse, true, auto).await;
                    let body = h.request("bound", stream);
                    let id = h.recovered_id(&body, None).await;
                    assert_eq!(h.mock.calls.lock().unwrap().len(), 2);
                    assert_eq!(h.recovered_id(&body, None).await, id);
                    assert_eq!(h.recovered_id(&body, Some(&id)).await, id);
                    let calls = h.mock.calls.lock().unwrap().clone();
                    assert_eq!(calls.len(), 4);
                    for (headers, request) in &calls[1..] {
                        for header in ["session_id", "x-session-id", "x-opencode-session"] {
                            assert_eq!(headers[header], id);
                        }
                        let history = if dialect == Dialect::OpenAIResponses {
                            "input"
                        } else {
                            "messages"
                        };
                        assert_eq!(request[history], body[history]);
                        assert_eq!(request["user"], id);
                        if dialect == Dialect::OpenAIResponses {
                            assert_eq!(request["prompt_cache_key"], id);
                        }
                    }
                    let outcomes = h.state.outcomes().recent(3);
                    assert_eq!(
                        outcomes
                            .iter()
                            .map(|outcome| outcome.attempts.len())
                            .sum::<usize>(),
                        4
                    );
                    assert!(outcomes
                        .iter()
                        .all(|outcome| outcome.final_status.is_success()));
                    let repaired = outcomes
                        .iter()
                        .find(|outcome| outcome.attempts.len() == 2)
                        .unwrap();
                    assert_ne!(
                        repaired.attempts[0].attempt_id,
                        repaired.attempts[1].attempt_id
                    );
                    assert_eq!(h.state.codex_filter.snapshot().cleanup_requests, 0);
                    h.stop().await;
                }
            }
        }
    }
}

#[tokio::test]
async fn persistent_binding_failure_is_bounded_to_one_renewal() {
    for dialect in [Dialect::OpenAI, Dialect::OpenAIResponses] {
        let h = Harness::new(dialect, 503, false, false, true).await;
        let response = h.send(&h.request("bound", true), None).await;
        let text = response.text().await.unwrap();
        assert!(text.contains("session_binding_invalidated"), "{text}");
        assert_eq!(h.mock.calls.lock().unwrap().len(), 2);
        assert_eq!(h.state.outcomes().recent(1)[0].attempts.len(), 2);
        h.stop().await;
    }
}

#[tokio::test]
async fn renewed_ids_are_isolated_by_model_and_credential() {
    let h = Harness::new(Dialect::OpenAIResponses, 400, false, true, true).await;
    let bound = h.request("bound", false);
    let first = h.recovered_id(&bound, None).await;
    let sibling = h.recovered_id(&h.request("sibling", false), None).await;
    assert_ne!(first, sibling);
    assert_eq!(h.recovered_id(&bound, None).await, first);
    let mut config = (*h.state.config()).clone();
    config.providers[0].key_ref = "second-key".into();
    h.state.set_config(config);
    let second_key = h.recovered_id(&bound, None).await;
    assert_ne!(first, second_key);
    h.stop().await;
}

#[tokio::test]
async fn concurrent_requests_share_the_same_replacement_binding() {
    let h = Harness::new(Dialect::OpenAI, 400, false, true, true).await;
    *h.mock.barrier.lock().unwrap() = Some(Arc::new(tokio::sync::Barrier::new(2)));
    let body = h.request("bound", false);
    let (first, second) = tokio::join!(h.recovered_id(&body, None), h.recovered_id(&body, None));
    assert_eq!(first, second);
    assert_eq!(h.mock.calls.lock().unwrap().len(), 4);
    h.stop().await;
}

#[tokio::test]
async fn a_binding_failure_after_delivered_tools_is_not_replayed() {
    let h = Harness::new(Dialect::OpenAI, 200, true, true, true).await;
    h.mock.late_error.store(true, Ordering::SeqCst);
    let mut body = h.request("bound", true);
    body["tools"] =
        json!([{"type":"function","function":{"name":"exec","parameters":{"type":"object"}}}]);
    let response = h.send(&body, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let text = response.text().await.unwrap();
    assert!(text.contains("already-delivered"), "{text}");
    assert!(text.contains("session_binding_invalidated"), "{text}");
    assert_eq!(h.mock.calls.lock().unwrap().len(), 1);
    assert!(!h.state.outcomes().recent(1)[0].final_status.is_success());
    h.stop().await;
}

#[tokio::test]
async fn renewed_sessions_can_then_repair_ciphertext_without_changing_ids_again() {
    let h = Harness::new(Dialect::OpenAIResponses, 400, false, true, true).await;
    h.mock.crypto_error.store(true, Ordering::SeqCst);
    let body = h.request("bound", true);
    let id = h.recovered_id(&body, None).await;
    let calls = h.mock.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[1].1["input"], body["input"]);
    assert!(!calls[2].1["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["type"] == "reasoning"));
    assert_eq!(calls[1].0["session_id"], id);
    assert_eq!(calls[2].0["session_id"], id);
    assert_eq!(h.state.codex_filter.snapshot().cleanup_requests, 1);
    assert_eq!(h.state.outcomes().recent(1)[0].attempts.len(), 3);
    h.stop().await;
}

#[tokio::test]
async fn renewal_does_not_rewrite_an_ordinary_user_identifier() {
    let h = Harness::new(Dialect::OpenAI, 400, false, true, true).await;
    let mut body = h.request("bound", false);
    body["user"] = json!("ses_account-user");
    let id = h.recovered_id(&body, None).await;
    assert_eq!(h.recovered_id(&body, Some(&id)).await, id);
    for (_, request) in h.mock.calls.lock().unwrap().iter() {
        assert_eq!(request["user"], "ses_account-user");
    }
    h.stop().await;
}
