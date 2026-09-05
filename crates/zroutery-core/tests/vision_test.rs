//! Integration tests for vision fallback.
//!
//! One mock provider plays both roles: the blind text model that cannot
//! accept images, and the vision model that describes them. The scenarios pin
//! the two trigger paths and the honest failure:
//!
//! * preflight — the target model is known not to see, so the image is
//!   described before the first attempt;
//! * reactive — the capability is unknown, the upstream rejects the image,
//!   and the same candidate is retried with a description;
//! * no vision model — every image becomes the placeholder, never a drop.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelClass, ModelEntry, ProviderConfig, ProviderKind,
    VisionConfig,
};
use zroutery_core::server::{AppState, ServerHandle};

// ------------------------------------------------------------------ mock upstream

#[derive(Default)]
struct MockInner {
    received: Vec<Value>,
}

#[derive(Clone, Default)]
struct Mock {
    inner: Arc<Mutex<MockInner>>,
}

impl Mock {
    fn bodies(&self) -> Vec<Value> {
        self.inner.lock().unwrap().received.clone()
    }
}

/// The mock reacts to the upstream model name:
/// * `blind*`   -> rejects any request containing an image (400, "does not
///                 support image input"), answers text-only requests
/// * `eyes*`    -> the vision model: describes whatever image it was sent
/// * anything else -> plain text answer
async fn mock_openai_chat(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    mock.inner.lock().unwrap().received.push(body.clone());

    let model = body["model"].as_str().unwrap_or("").to_string();
    let has_image = body["messages"]
        .as_array()
        .map(|messages| {
            messages.iter().any(|m| {
                let content = m.get("content");
                content
                    .and_then(Value::as_array)
                    .map(|blocks| {
                        blocks
                            .iter()
                            .any(|b| b.get("type").and_then(Value::as_str) == Some("image_url"))
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

    if model.starts_with("fail-eyes") {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "vision model exploded"}})),
        )
            .into_response();
    }

    let content = if model.starts_with("eyes") {
        "A chart with a rising line, titled \"Revenue\"."
    } else {
        "text answer"
    };

    Json(json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "created": 1,
        "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": content},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7}
    }))
    .into_response()
}

async fn start_mock() -> (SocketAddr, Mock) {
    let mock = Mock::default();
    let app = axum::Router::new()
        .route("/chat/completions", post(mock_openai_chat))
        .route("/v1/chat/completions", post(mock_openai_chat))
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

/// An Anthropic-style request carrying one image, so the ingress decode and
/// the cross-dialect encode are both exercised.
fn image_request() -> Value {
    json!({
        "model": "sonnet-class",
        "max_tokens": 128,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "image", "source": {
                    "type": "url", "url": "https://example.com/chart.png"}},
                {"type": "text", "text": "what does this chart show"}
            ]
        }]
    })
}

fn config_for(mock: SocketAddr, vision: VisionConfig, blind_supports_vision: bool) -> AppConfig {
    let mut provider = ProviderConfig::new("p", "P", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{mock}");
    provider.key_ref = "provider:p".into();
    provider.timeout_secs = 10;

    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();
    cfg.providers = vec![provider];
    cfg.models = vec![
        ModelEntry::for_upstream("p", "blind-model", Some(ModelClass::Sonnet)),
        ModelEntry::for_upstream("p", "eyes-model", Some(ModelClass::Haiku)),
    ];
    cfg.models[0].supports_vision = blind_supports_vision;
    cfg.models[1].supports_vision = true;
    cfg.vision = vision;
    cfg
}

fn secrets() -> Arc<MemorySecretStore> {
    Arc::new(MemorySecretStore::new().with("provider:p", "sk-p"))
}

struct Harness {
    base: String,
    server: Option<ServerHandle>,
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
            client: reqwest::Client::new(),
            mock,
        }
    }

    async fn post(&self, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{}/v1/messages", self.base))
            .header("x-api-key", TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn shutdown(mut self) {
        if let Some(s) = self.server.take() {
            s.stop().await;
        }
    }
}

// ------------------------------------------------------------------ the tests

/// Preflight: the model is declared blind, so the image is described before
/// the first send — the blind model never sees an image, and its answer
/// arrives with the description in the prompt.
#[tokio::test]
async fn a_blind_target_gets_its_image_described_before_sending() {
    let (addr, mock) = start_mock().await;
    let vision = VisionConfig {
        enabled: true,
        model: Some("p-eyes-model".into()),
        ..VisionConfig::default()
    };
    let h = Harness::start(config_for(addr, vision, false), mock).await;

    let resp = h.post(image_request()).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "text answer");

    // Two upstream calls: the description, then the blind model. The blind
    // call must carry text where the image was, not the image itself.
    let bodies = h.mock.bodies();
    assert_eq!(bodies.len(), 2, "describe + blind answer");
    assert_eq!(bodies[0]["model"], "eyes-model");
    assert_eq!(bodies[1]["model"], "blind-model");
    let blind_prompt = bodies[1]["messages"][0]["content"].as_str().unwrap();
    assert!(
        blind_prompt.contains("[Image description: A chart with a rising line"),
        "the blind model received: {blind_prompt}"
    );

    h.shutdown().await;
}

/// Reactive: the capability is unknown (not declared), so the original image
/// goes out first; the 400 triggers the vision repair and the same candidate
/// is retried — with a description, not a placeholder.
#[tokio::test]
async fn an_unknown_capability_rejection_is_repaired_with_a_description() {
    let (addr, mock) = start_mock().await;
    let vision = VisionConfig {
        enabled: true,
        model: Some("p-eyes-model".into()),
        ..VisionConfig::default()
    };
    // The model can see as far as the registry knows — the upstream is the
    // one that disagrees.
    let h = Harness::start(config_for(addr, vision, true), mock).await;

    let resp = h.post(image_request()).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "text answer");

    // Three calls: image attempt (rejected), description, text retry.
    let bodies = h.mock.bodies();
    assert_eq!(bodies.len(), 3, "image -> describe -> retry");
    assert_eq!(bodies[0]["model"], "blind-model");
    // First try: the image went out — content is the OpenAI block array, not a
    // joined string.
    assert!(bodies[0]["messages"][0]["content"].as_array().is_some(), "first try sends the image");
    assert_eq!(bodies[1]["model"], "eyes-model");
    let retry_prompt = bodies[2]["messages"][0]["content"].as_str().unwrap();
    assert!(retry_prompt.contains("[Image description: A chart"), "retry: {retry_prompt}");

    h.shutdown().await;
}

/// No vision model configured: the placeholder is honest about the loss, and
/// the request still succeeds — one upstream call, no vision traffic.
#[tokio::test]
async fn without_a_vision_model_the_placeholder_is_used() {
    let (addr, mock) = start_mock().await;
    let vision = VisionConfig {
        enabled: true,
        model: None,
        ..VisionConfig::default()
    };
    let h = Harness::start(config_for(addr, vision, false), mock).await;

    let resp = h.post(image_request()).await;
    assert_eq!(resp.status(), 200);

    let bodies = h.mock.bodies();
    assert_eq!(bodies.len(), 1, "no vision call happens");
    // The placeholder replaced the image block in place; the neighbouring
    // text block is untouched, and the encoder may keep both as blocks.
    let message = serde_json::to_string(&bodies[0]["messages"][0]).unwrap();
    assert!(message.contains("[Unsupported Image]"), "message: {message}");
    assert!(
        message.contains("what does this chart show"),
        "the user's question survived: {message}"
    );

    h.shutdown().await;
}

/// Vision off entirely: the request goes as it came, the blind model rejects
/// the image, and the plain placeholder rectifier repairs it — the old
/// behaviour, unchanged, because nothing was promised.
#[tokio::test]
async fn vision_off_sends_the_image_as_is() {
    let (addr, mock) = start_mock().await;
    let mut off = VisionConfig::default();
    off.enabled = false;
    off.model = Some("p-eyes-model".into());
    let h = Harness::start(config_for(addr, off, false), mock).await;

    let resp = h.post(image_request()).await;
    // The mock rejects image requests for blind models; with vision off the
    // placeholder rectifier still repairs the body, so the retry succeeds.
    assert_eq!(resp.status(), 200);

    let bodies = h.mock.bodies();
    // The first call is the blind model with the image still in place — no
    // preflight happened. The repair (placeholder) only shows up on the retry.
    let first = &bodies[0];
    assert_eq!(first["model"], "blind-model");
    assert!(
        first["messages"][0]["content"].as_array().is_some(),
        "the image went out untouched: {}",
        serde_json::to_string(&first["messages"][0]).unwrap()
    );

    h.shutdown().await;
}

fn base64_image_request() -> Value {
    json!({
        "model": "sonnet-class",
        "max_tokens": 128,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "image", "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": "iVBORw0KGgo="}},
                {"type": "text", "text": "what is this"}
            ]
        }]
    })
}

#[tokio::test]
async fn vision_model_failure_uses_placeholder() {
    let (addr, mock) = start_mock().await;
    let vision = VisionConfig {
        enabled: true,
        model: Some("p-fail-eyes-model".into()),
        ..VisionConfig::default()
    };
    // blind target -> preflight vision -> vision model fails -> placeholder used
    let h = Harness::start(config_for(addr, vision, false), mock).await;
    let resp = h.post(image_request()).await;
    assert_eq!(resp.status(), 200);
    let bodies = h.mock.bodies();
    // Should have: vision attempt (500), then blind-model with placeholder
    let message = serde_json::to_string(&bodies.last().unwrap()).unwrap();
    assert!(message.contains("[Unsupported Image]"), "placeholder: {message}");
    h.shutdown().await;
}

#[tokio::test]
async fn base64_image_gets_described_in_preflight() {
    let (addr, mock) = start_mock().await;
    let vision = VisionConfig {
        enabled: true,
        model: Some("p-eyes-model".into()),
        ..VisionConfig::default()
    };
    let h = Harness::start(config_for(addr, vision, false), mock).await;
    let resp = h.post(base64_image_request()).await;
    assert_eq!(resp.status(), 200);
    let bodies = h.mock.bodies();
    // eyes-model got the description request, blind-model got text
    assert_eq!(bodies.len(), 2, "describe + answer");
    assert_eq!(bodies[0]["model"], "eyes-model");
    assert_eq!(bodies[1]["model"], "blind-model");
    let blind_prompt = bodies[1]["messages"][0]["content"].as_str().unwrap();
    assert!(blind_prompt.contains("[Image description:"), "got: {blind_prompt}");
    h.shutdown().await;
}
