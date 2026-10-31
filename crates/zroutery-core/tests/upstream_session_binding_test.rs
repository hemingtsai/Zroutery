//! OpenCode -> Zroutery -> NewAPI binding errors, without live credentials.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ProviderConfig, ProviderKind,
};
use zroutery_core::error::Error;
use zroutery_core::failure::FailureClass;
use zroutery_core::ir::Dialect;
use zroutery_core::protocol::{self, SseFrame, StreamEncoder};
use zroutery_core::rectifier::codex_auto;
use zroutery_core::server::{AppState, ServerHandle};

const MESSAGE: &str = "session binding invalidated: the model this session is bound to can no longer serve it; restart the task with a new session id [trace_id=private-trace code=30013]";

fn payload() -> Value {
    json!({"error": {
        "type": "new_api_error", "code": null, "message": MESSAGE,
        "channel": "private-channel", "credential": "private-credential"
    }})
}

fn upstream_error(status: u16, body: &str) -> Error {
    Error::Upstream {
        provider: "NewAPI".into(),
        status,
        body: body.into(),
        retry_after: None,
    }
}

fn assert_binding_error(error: &Error) {
    let failure = error.classified();
    assert_eq!(failure.class, FailureClass::InvalidRequest, "{error:?}");
    assert_eq!(failure.status, Some(400));
    assert!(!failure.retryable() && !failure.fallbackable());
    assert!(!failure.affects_circuit() && !failure.affects_observation());
    assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    assert!(codex_auto::classify_error(error).is_none());
    let wire = error.to_wire(Dialect::OpenAI);
    assert_eq!(wire["error"]["type"], "invalid_request_error");
    assert_eq!(wire["error"]["code"], "session_binding_invalidated");
    assert!(wire["error"]["message"]
        .as_str()
        .unwrap()
        .contains("new session id"));
    let text = wire.to_string();
    assert!(text.contains("30013"));
    for private in ["private-trace", "private-channel", "private-credential"] {
        assert!(!text.contains(private), "upstream details leaked: {text}");
    }
}

#[test]
fn recognizes_numeric_string_wrapped_and_plain_binding_errors() {
    for body in [
        json!({"error":{"code":30013,"message":"private-credential"}}).to_string(),
        json!({"error":{"code":"30013","message":"private-credential"}}).to_string(),
        payload().to_string(),
        json!({"error":{"error":{"code":30013}}}).to_string(),
        json!({"type":"response.failed","response":{"error":{"code":30013}}}).to_string(),
        json!({"success":false,"message":MESSAGE}).to_string(),
        MESSAGE.to_string(),
    ] {
        for status in [200, 400, 500, 503] {
            assert_binding_error(&upstream_error(status, &body));
            assert_eq!(
                FailureClass::from_status_with_body(status, &body),
                FailureClass::InvalidRequest
            );
        }
    }
    assert_eq!(
        FailureClass::from_error_message(MESSAGE),
        FailureClass::InvalidRequest
    );
}

#[test]
fn parsers_retain_numeric_binding_codes_in_json_and_sse() {
    let body = json!({"error":{"code":30013,"message":"private-credential"}});
    for dialect in [Dialect::OpenAI, Dialect::OpenAIResponses] {
        let error = protocol::decode_response(dialect, body.clone()).unwrap_err();
        assert_binding_error(&error);
        let mut parser = protocol::stream_parser(dialect, "bound-model");
        let frame = SseFrame {
            event: Some("error".into()),
            data: body.to_string(),
        };
        let error = parser.push(&frame).unwrap_err();
        assert_binding_error(&error);
    }
}

#[test]
fn binding_errors_are_not_successful_or_generic_responses_sse_events() {
    let error = upstream_error(503, &payload().to_string());
    let mut encoder = protocol::responses::ResponsesStreamEncoder::new("bound-model");
    let frames = encoder.error(&error);
    let failed = frames.last().unwrap().json().unwrap();
    assert_eq!(failed["type"], "response.failed");
    assert_eq!(
        failed["response"]["error"]["code"],
        "session_binding_invalidated"
    );
    assert!(encoder.finish().is_empty());
}

#[test]
fn authentication_billing_and_unrelated_codes_are_not_relabelled() {
    for status in [401, 403] {
        assert_eq!(
            upstream_error(status, &payload().to_string())
                .classified()
                .class,
            FailureClass::Authentication
        );
    }
    let auth = json!({"error":{"type":"authentication_error","code":30013,"message":MESSAGE}});
    assert_eq!(
        upstream_error(200, &auth.to_string()).classified().class,
        FailureClass::Authentication
    );
    let billing = json!({"error":{"code":30013,"message":"insufficient balance"}});
    assert_eq!(
        upstream_error(503, &billing.to_string()).classified().class,
        FailureClass::BalanceExhausted
    );
    for body in [
        json!({"error":{"code":300130,"message":"unrelated"}}),
        json!({"error":{"code":"other","message":"trace_id=30013"}}),
    ] {
        let error = upstream_error(503, &body.to_string());
        assert_eq!(error.classified().class, FailureClass::ProviderUnavailable);
        assert_eq!(error.to_wire(Dialect::OpenAI)["error"]["code"], "api_error");
    }
    let normal = json!({"id":"reply", "model":"m", "choices":[{
        "message":{"role":"assistant","content":MESSAGE}, "finish_reason":"stop"
    }]});
    assert!(protocol::openai::decode_response(normal).is_ok());
    let quoted = Error::BadUpstreamPayload(format!("response was not JSON: {MESSAGE}"));
    assert_eq!(quoted.classified().class, FailureClass::Protocol);
    assert_eq!(
        quoted.to_wire(Dialect::OpenAI)["error"]["code"],
        "api_error"
    );
}

#[derive(Clone)]
struct Mock {
    status: u16,
    sse: bool,
    bodies: Arc<Mutex<Vec<Value>>>,
}

async fn newapi(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    mock.bodies.lock().unwrap().push(body);
    let status = StatusCode::from_u16(mock.status).unwrap();
    if mock.sse {
        Response::builder()
            .status(status)
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(format!(
                "event: error\ndata: {}\n\n",
                payload()
            )))
            .unwrap()
    } else {
        (status, Json(payload())).into_response()
    }
}

async fn gateway_case(dialect: Dialect, status: u16, sse: bool, stream: bool) {
    let mock = Mock {
        status,
        sse,
        bodies: Default::default(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let routes = Router::new()
        .route("/v1/chat/completions", post(newapi))
        .route("/v1/responses", post(newapi))
        .with_state(mock.clone());
    let upstream = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let mut config = AppConfig::default();
    config.server.port = 0;
    config.server.require_auth = false;
    config.server.bypass_proxy = true;
    config.routing.rectifier.enabled = false;
    let mut provider = ProviderConfig::new("newapi", "NewAPI", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{address}");
    provider.key_ref.clear();
    provider.api_surface = serde_json::from_value(json!(if dialect == Dialect::OpenAIResponses {
        "responses"
    } else {
        "chat"
    }))
    .unwrap();
    config.providers.push(provider);
    let mut model = ModelEntry::for_upstream("newapi", "bound", None);
    model.capabilities.thinking = true;
    config.models.push(model);
    let state = Arc::new(AppState::new(config, Arc::new(MemorySecretStore::new())));
    let gateway = ServerHandle::start(state.clone()).await.unwrap();
    let path = if dialect == Dialect::OpenAIResponses {
        "responses"
    } else {
        "chat/completions"
    };
    let request = if dialect == Dialect::OpenAIResponses {
        json!({"model":"newapi-bound", "stream":stream, "prompt_cache_key":"client-session", "input":[
            {"type":"reasoning","encrypted_content":"prior-cipher","summary":[]},
            {"role":"user","content":"fixture"}
        ]})
    } else {
        json!({"model":"newapi-bound", "stream":stream, "messages":[{"role":"user","content":"fixture"}]})
    };
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{}/v1/{path}", gateway.addr))
        .header("user-agent", "opencode/2.0.32")
        .timeout(Duration::from_secs(3))
        .json(&request)
        .send()
        .await
        .unwrap();
    let response_status = response.status();
    let text = response.text().await.unwrap();
    if dialect == Dialect::OpenAIResponses && stream && state.config().routing.rectifier.enabled {
        assert_eq!(response_status, StatusCode::OK);
        let mut decoder = protocol::SseDecoder::new();
        let frames = decoder.push(text.as_bytes());
        let failed = frames
            .iter()
            .find(|frame| frame.event.as_deref() == Some("response.failed"))
            .expect("the staged stream must report a failure")
            .json()
            .unwrap();
        assert_eq!(
            failed["response"]["error"]["code"],
            "session_binding_invalidated"
        );
        assert!(!text.contains("response.completed"));
    } else {
        assert_eq!(response_status, StatusCode::BAD_REQUEST, "{text}");
        let wire: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(wire["error"]["code"], "session_binding_invalidated");
    }
    assert!(text.contains("new session id"), "{text}");
    assert!(!text.contains("private-"), "{text}");
    assert_eq!(
        mock.bodies.lock().unwrap().len(),
        1,
        "the invalid binding must not be replayed"
    );
    if dialect == Dialect::OpenAIResponses {
        let bodies = mock.bodies.lock().unwrap();
        assert_eq!(bodies[0]["input"], request["input"]);
        assert_eq!(bodies[0]["prompt_cache_key"], request["prompt_cache_key"]);
    }
    let filter = state.codex_filter.snapshot();
    assert_eq!(filter.cleanup_requests, 0);
    assert_eq!(filter.retries, [0; 4]);
    assert_eq!(state.outcomes().recent(1)[0].attempts.len(), 1);
    gateway.stop().await;
    upstream.abort();
}

#[tokio::test]
async fn opencode_chat_requests_recognize_newapi_http_json_and_sse_binding_errors() {
    for (status, sse) in [(400, false), (503, false), (200, false), (200, true)] {
        gateway_case(Dialect::OpenAI, status, sse, true).await;
        if !sse {
            gateway_case(Dialect::OpenAI, status, sse, false).await;
        }
    }
}

#[tokio::test]
async fn opencode_responses_requests_preserve_history_instead_of_cleaning_a_bad_binding() {
    for (status, sse) in [(400, false), (503, false), (200, false), (200, true)] {
        gateway_case(Dialect::OpenAIResponses, status, sse, true).await;
        if !sse {
            gateway_case(Dialect::OpenAIResponses, status, sse, false).await;
        }
    }
}
