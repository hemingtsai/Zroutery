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
//!
//! The last group splits the main model and the vision model across two
//! providers, so the auxiliary call's budget and its ledger entries can be
//! read on the provider the main request never touches.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::billing::Pricing;
use zroutery_core::budget::{Budget, BudgetPeriod, BudgetScope, Ledger};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind, VisionConfig,
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
///   support image input"), answers text-only requests
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

    // A candidate that always fails, so the router has to fail over to the
    // next one in the tier.
    if model.starts_with("flaky") {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "flaky model exploded"}})),
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
        ModelEntry::for_upstream("p", "blind-model", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("p", "eyes-model", Some(ModelTier::Fast)),
        ModelEntry::for_upstream("p", "fail-eyes-model", Some(ModelTier::Fast)),
    ];
    cfg.models[0].capabilities.vision = blind_supports_vision;
    cfg.models[1].capabilities.vision = true;
    cfg.models[2].capabilities.vision = true;
    cfg.vision = vision;
    cfg
}

/// The main model on provider `p` and the vision model on provider `v`, both
/// served by the same mock. Splitting them is what makes the auxiliary call's
/// own budget and ledger entries readable: provider `p` never answers a
/// description, provider `v` never answers the client.
fn split_config(mock: SocketAddr, vision_upstream: &str) -> AppConfig {
    let mut main = ProviderConfig::new("p", "P", ProviderKind::OpenAICompatible);
    main.base_url = format!("http://{mock}");
    main.key_ref = "provider:p".into();
    main.timeout_secs = 10;

    let mut vision = ProviderConfig::new("v", "V", ProviderKind::OpenAICompatible);
    vision.base_url = format!("http://{mock}");
    vision.key_ref = "provider:v".into();
    vision.timeout_secs = 10;

    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();
    cfg.providers = vec![main, vision];
    cfg.models = vec![
        ModelEntry::for_upstream("p", "blind-model", Some(ModelTier::Standard)),
        ModelEntry::for_upstream("v", vision_upstream, Some(ModelTier::Fast)),
    ];
    cfg.models[1].capabilities.vision = true;
    // 11 prompt + 7 completion tokens per description, at 100 USD/Mtok.
    cfg.models[1].pricing = Some(Pricing::new("USD", 100.0, 100.0));
    let vision_model = cfg.models[1].exposed_id();
    cfg.vision = VisionConfig {
        enabled: true,
        model: Some(vision_model),
        ..VisionConfig::default()
    };
    cfg
}

/// The exact cost of one description at the mock's usage and price.
const ONE_DESCRIPTION_USD: f64 = 18.0 * 100.0 / 1_000_000.0;

/// An Anthropic-style request carrying two images.
fn two_image_request() -> Value {
    json!({
        "model": "sonnet-class",
        "max_tokens": 128,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "image", "source": {
                    "type": "url", "url": "https://example.com/one.png"}},
                {"type": "image", "source": {
                    "type": "url", "url": "https://example.com/two.png"}},
                {"type": "text", "text": "compare these two charts"}
            ]
        }]
    })
}

/// How many calls the mock received for a model name prefix.
fn calls_to(bodies: &[Value], prefix: &str) -> usize {
    bodies
        .iter()
        .filter(|body| {
            body["model"]
                .as_str()
                .is_some_and(|model| model.starts_with(prefix))
        })
        .count()
}

/// Spend booked against one budget scope today.
fn spent_today(ledger: &Ledger, scope: BudgetScope) -> f64 {
    ledger
        .totals_for(&scope, chrono::Local::now())
        .into_iter()
        .filter(|(period, _)| *period == BudgetPeriod::Day)
        .map(|(_, cost)| cost.amount)
        .sum()
}

fn secrets() -> Arc<MemorySecretStore> {
    Arc::new(
        MemorySecretStore::new()
            .with("provider:p", "sk-p")
            .with("provider:v", "sk-v"),
    )
}

struct Harness {
    base: String,
    server: Option<ServerHandle>,
    client: reqwest::Client,
    mock: Mock,
    state: Arc<AppState>,
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
            state,
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
    assert!(
        bodies[0]["messages"][0]["content"].as_array().is_some(),
        "first try sends the image"
    );
    assert_eq!(bodies[1]["model"], "eyes-model");
    let retry_prompt = bodies[2]["messages"][0]["content"].as_str().unwrap();
    assert!(
        retry_prompt.contains("[Image description: A chart"),
        "retry: {retry_prompt}"
    );

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
    assert!(
        message.contains("[Unsupported Image]"),
        "message: {message}"
    );
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
    let off = VisionConfig {
        enabled: false,
        model: Some("p-eyes-model".into()),
        ..Default::default()
    };
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

/// The auxiliary call fails: the main request keeps its placeholder policy and
/// still succeeds, and the failure lands on the record for the model that was
/// asked rather than disappearing.
#[tokio::test]
async fn vision_model_failure_uses_placeholder_and_records_the_failure() {
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
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "text answer");

    let bodies = h.mock.bodies();
    assert_eq!(
        calls_to(&bodies, "fail-eyes"),
        1,
        "the failing vision model is asked exactly once"
    );
    assert_eq!(calls_to(&bodies, "eyes-model"), 0, "the healthy one is not");
    // Should have: vision attempt (500), then blind-model with placeholder
    let message = serde_json::to_string(&bodies.last().unwrap()).unwrap();
    assert!(
        message.contains("[Unsupported Image]"),
        "placeholder: {message}"
    );

    let health = h.state.router().health_snapshot();
    let failed = health
        .iter()
        .find(|row| row.model_id == "p-fail-eyes-model")
        .expect("the vision model has a health row after failing");
    assert!(
        failed.consecutive_failures >= 1 && failed.total_failure >= 1,
        "failure recorded: {failed:?}"
    );
    assert!(failed.last_error.is_some(), "with its error: {failed:?}");

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
    assert!(
        blind_prompt.contains("[Image description:"),
        "got: {blind_prompt}"
    );
    h.shutdown().await;
}

// ------------------------------------------------- auxiliary budget and spend

/// An exhausted provider budget for `v` stops the description before it is
/// sent: the main request is untouched by a budget that only covers the vision
/// provider, and the image becomes the placeholder instead.
#[tokio::test]
async fn an_over_budget_vision_provider_is_never_contacted() {
    let (addr, mock) = start_mock().await;
    let mut cfg = split_config(addr, "eyes-model");
    cfg.budgets = vec![Budget::new(
        BudgetScope::Provider { id: "v".into() },
        BudgetPeriod::Day,
        "USD",
        0.0,
    )];
    let h = Harness::start(cfg, mock).await;

    let resp = h.post(image_request()).await;
    assert_eq!(resp.status(), 200, "the main request is not gated by v");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "text answer");

    let bodies = h.mock.bodies();
    assert_eq!(
        calls_to(&bodies, "eyes-model"),
        0,
        "an over-budget vision provider receives nothing"
    );
    let message = serde_json::to_string(&bodies.last().unwrap()).unwrap();
    assert!(
        message.contains("[Unsupported Image]"),
        "the image is still accounted for: {message}"
    );
    assert_eq!(
        spent_today(&h.state.ledger(), BudgetScope::Provider { id: "v".into() }),
        0.0
    );

    h.shutdown().await;
}

/// A tier budget gates the auxiliary call by the tier it would actually reach:
/// the vision model is Fast while the main request is Standard, so exhausting
/// Fast stops the description and nothing else.
#[tokio::test]
async fn a_tier_budget_stops_the_vision_call() {
    let (addr, mock) = start_mock().await;
    let mut cfg = split_config(addr, "eyes-model");
    cfg.budgets = vec![Budget::new(
        BudgetScope::Tier {
            tier: ModelTier::Fast,
        },
        BudgetPeriod::Day,
        "USD",
        0.0,
    )];
    let h = Harness::start(cfg, mock).await;

    let resp = h.post(image_request()).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(calls_to(&h.mock.bodies(), "eyes-model"), 0);

    h.shutdown().await;
}

/// A description is charged once, to the provider and tier that answered it —
/// never to the main request's provider, and never folded into the main
/// request's own settlement.
#[tokio::test]
async fn a_description_is_charged_to_the_provider_that_answered() {
    let (addr, mock) = start_mock().await;
    let mut cfg = split_config(addr, "eyes-model");
    // Price the main model too, so the two attributions cannot be confused.
    cfg.models[0].pricing = Some(Pricing::new("USD", 1000.0, 1000.0));
    let h = Harness::start(cfg, mock).await;

    let resp = h.post(image_request()).await;
    assert_eq!(resp.status(), 200);

    let ledger = h.state.ledger();
    let vision = spent_today(&ledger, BudgetScope::Provider { id: "v".into() });
    assert!(
        (vision - ONE_DESCRIPTION_USD).abs() < 1e-12,
        "one description costs {ONE_DESCRIPTION_USD}, ledger says {vision}"
    );
    let main = spent_today(&ledger, BudgetScope::Provider { id: "p".into() });
    assert!(main > 0.0, "the main request settled its own spend");
    assert!(
        (main - vision).abs() > 1e-9,
        "the two attributions are separate"
    );
    let fast = spent_today(
        &ledger,
        BudgetScope::Tier {
            tier: ModelTier::Fast,
        },
    );
    assert!(
        (fast - ONE_DESCRIPTION_USD).abs() < 1e-12,
        "the vision model's tier carries the auxiliary spend: {fast}"
    );
    let global = spent_today(&ledger, BudgetScope::Global);
    assert!(
        (global - (main + vision)).abs() < 1e-9,
        "global sees both: {global} vs {main} + {vision}"
    );

    h.shutdown().await;
}

/// The global scope gates the auxiliary call too, and it is read again before
/// every description: the main request is admitted because nothing has been
/// spent yet, the first description crosses the line, and the second image is
/// already stopped rather than described and charged beyond the limit.
#[tokio::test]
async fn a_global_budget_stops_the_next_description() {
    let (addr, mock) = start_mock().await;
    let mut cfg = split_config(addr, "eyes-model");
    // One description costs 0.0018; the whole day allows less than that.
    cfg.budgets = vec![Budget::new(
        BudgetScope::Global,
        BudgetPeriod::Day,
        "USD",
        ONE_DESCRIPTION_USD / 2.0,
    )];
    let h = Harness::start(cfg, mock).await;

    let resp = h.post(two_image_request()).await;
    assert_eq!(resp.status(), 200, "the main request is admitted");

    let bodies = h.mock.bodies();
    assert_eq!(
        calls_to(&bodies, "eyes-model"),
        1,
        "the second image is not described: {bodies:?}"
    );
    let global = spent_today(&h.state.ledger(), BudgetScope::Global);
    assert!(
        (global - ONE_DESCRIPTION_USD).abs() < 1e-12,
        "and only the description that was sent is charged: {global}"
    );
    let message = serde_json::to_string(&bodies.last().unwrap()).unwrap();
    assert!(
        message.contains("[Image description:"),
        "the first image kept its description: {message}"
    );
    assert!(
        message.contains("[Unsupported Image]"),
        "the second became the placeholder: {message}"
    );

    h.shutdown().await;
}

/// Two images and a main-candidate failover: the ledger follows the
/// descriptions that were actually sent, exactly once each. Every candidate
/// preflights its own copy of the request, so the failing attempt and the one
/// that takes over each describe both images; what must never happen is a
/// phantom charge for a call that was not made, or a lost charge for one that
/// was.
#[tokio::test]
async fn every_description_is_charged_once_across_a_failover() {
    let (addr, mock) = start_mock().await;
    let mut cfg = split_config(addr, "eyes-model");
    // A failing candidate ahead of the blind one in the same tier.
    cfg.models.push(
        ModelEntry::for_upstream("p", "flaky-model", Some(ModelTier::Standard)).with_priority(0),
    );
    cfg.models[0].priority = 1;
    let h = Harness::start(cfg, mock).await;

    let resp = h.post(two_image_request()).await;
    assert_eq!(resp.status(), 200, "the second candidate answers");

    let bodies = h.mock.bodies();
    assert_eq!(
        calls_to(&bodies, "flaky-model"),
        1,
        "the flaky candidate was tried"
    );
    assert_eq!(
        calls_to(&bodies, "blind-model"),
        1,
        "and the failover answered once"
    );
    // Two images for the failed attempt, two for the failover.
    let descriptions = calls_to(&bodies, "eyes-model");
    assert_eq!(descriptions, 4, "one description per image per attempt");

    let vision = spent_today(&h.state.ledger(), BudgetScope::Provider { id: "v".into() });
    assert!(
        (vision - descriptions as f64 * ONE_DESCRIPTION_USD).abs() < 1e-12,
        "{descriptions} descriptions billed once each, ledger says {vision}"
    );

    h.shutdown().await;
}

/// The auxiliary spend belongs to the ledger the moment it is known, not to
/// the main request's settlement: when every candidate then fails, the money
/// that was already spent is still counted.
#[tokio::test]
async fn a_failed_main_request_keeps_the_spend_it_already_incurred() {
    let (addr, mock) = start_mock().await;
    let mut cfg = split_config(addr, "eyes-model");
    // Only the failing candidate is left in the tier.
    cfg.models
        .retain(|model| model.exposed_id() != "p-blind-model");
    cfg.models.push(
        ModelEntry::for_upstream("p", "flaky-model", Some(ModelTier::Standard)).with_priority(0),
    );
    let h = Harness::start(cfg, mock).await;

    let resp = h.post(image_request()).await;
    assert_ne!(resp.status(), 200, "no candidate can answer");

    let bodies = h.mock.bodies();
    assert_eq!(calls_to(&bodies, "eyes-model"), 1);
    let vision = spent_today(&h.state.ledger(), BudgetScope::Provider { id: "v".into() });
    assert!(
        (vision - ONE_DESCRIPTION_USD).abs() < 1e-12,
        "an already-incurred description is not lost: {vision}"
    );
    // The client failed, and the main provider is still not charged for the
    // auxiliary call.
    assert_eq!(
        spent_today(&h.state.ledger(), BudgetScope::Provider { id: "p".into() }),
        0.0
    );

    h.shutdown().await;
}
