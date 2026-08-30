//! Integration tests for Auto Mode classifier routing.
//!
//! A mock provider plays every role: the main pool's model and the classifier
//! pool's candidates. The scenarios follow what a real Claude Code session
//! produces — main requests and classifier side queries on the same endpoint,
//! often with the same model string — and pin the invariants that matter:
//!
//! * a main request never lands on a classifier candidate, and a classifier
//!   request never lands on the main pool;
//! * a candidate that fails, times out or answers without a verdict is
//!   failed over, never "interpreted";
//! * when nothing can produce a verdict, the request fails closed — an
//!   unparseable answer is never an approval.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::config::{
    AppConfig, ClassifierCandidate, ClassifierConfig, MemorySecretStore, ModelClass, ModelEntry,
    ProviderConfig, ProviderKind,
};
use zroutery_core::server::{AppState, ServerHandle};

// ------------------------------------------------------------------ mock upstream

#[derive(Default)]
struct MockInner {
    /// Every chat body the mock received, in order.
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

    fn count(&self) -> usize {
        self.inner.lock().unwrap().received.len()
    }
}

/// The mock reacts to the upstream model name:
/// * `broken*`  -> HTTP 500
/// * `refuse*`  -> HTTP 400 (non retryable protocol rejection)
/// * `garbage*`  -> HTTP 200 with no verdict in the text
/// * anything else -> a well-formed `<block>no</block>` answer
async fn mock_openai_chat(
    State(mock): State<Mock>,
    Json(body): Json<Value>,
) -> Response {
    mock.inner.lock().unwrap().received.push(body.clone());

    let model = body["model"].as_str().unwrap_or("").to_string();
    if model.starts_with("broken") {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "upstream exploded", "type": "server_error"}})),
        )
            .into_response();
    }
    if model.starts_with("refuse") {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "invalid stop parameter", "type": "invalid_request_error"}})),
        )
            .into_response();
    }

    let content = if model.starts_with("garbage") {
        // HTTP 200, model said something — just not a verdict. This is the
        // case that must never be read as approval.
        "This looks fine to me!"
    } else {
        "<block>no</block>"
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

/// A stage-1 shaped Auto Mode classifier request from "Claude Code".
fn classifier_body() -> Value {
    json!({
        "model": "claude-opus-4-8[1m]",
        "max_tokens": 64,
        "temperature": 0,
        "stop_sequences": ["</block>"],
        "system": [{"type": "text", "text":
            "You are a security monitor for autonomous AI coding agents."}],
        "messages": [{"role": "user", "content": "should this bash command run?"}]
    })
}

/// One provider serving the main conversation model plus every classifier
/// candidate, each registered so the pool actually resolves.
fn config_for(mock: SocketAddr, candidates: &[&str]) -> AppConfig {
    let mut provider = ProviderConfig::new("zai", "Z.ai", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{mock}");
    provider.key_ref = "provider:zai".into();
    provider.timeout_secs = 10;

    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();
    cfg.providers = vec![provider];

    // The main conversation: a class member.
    cfg.models = vec![ModelEntry::for_upstream(
        "zai",
        "main-model",
        Some(ModelClass::Sonnet),
    )];

    let mut classifier_candidates = Vec::new();
    for (i, name) in candidates.iter().enumerate() {
        cfg.models.push(ModelEntry::for_upstream("zai", *name, None));
        classifier_candidates.push(ClassifierCandidate {
            model: format!("zai-{name}"),
            priority: 10 * (i as i32 + 1),
            enabled: true,
        });
    }
    cfg.classifier = ClassifierConfig {
        enabled: true,
        candidates: classifier_candidates,
        max_attempts: 2,
        ..ClassifierConfig::default()
    };
    cfg
}

fn secrets() -> Arc<MemorySecretStore> {
    Arc::new(MemorySecretStore::new().with("provider:zai", "sk-zai"))
}

struct Harness {
    base: String,
    server: Option<ServerHandle>,
    state: Arc<AppState>,
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
            state,
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

/// The most important invariant (§39): with classifier routing on, a main
/// request still lands on the main pool and a classifier request lands on the
/// classifier pool — through the same endpoint, in the same session.
#[tokio::test]
async fn main_and_classifier_traffic_use_their_own_pools() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(config_for(addr, &["glm-5.3", "backup-glm"]), mock).await;

    // Main conversation request for the class the main model belongs to.
    let resp = h
        .post(json!({
            "model": "sonnet-class",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "refactor this module"}]
        }))
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "zai-main-model");
    // No classifier marker on main traffic.
    assert!(resp.headers().get("x-zroutery-classifier").is_none());

    // Classifier side query: different pool, different model.
    let resp = h.post(classifier_body()).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "zai-glm-5.3");
    assert_eq!(resp.headers()["x-zroutery-classifier"], "1");

    // Two upstream calls, in order: the main model first, then the classifier.
    let bodies = h.mock.bodies();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0]["model"], "main-model");
    assert_eq!(bodies[1]["model"], "glm-5.3");

    // And the stats split them by kind.
    let summary = h.state.stats().summary();
    let main = summary.per_kind.iter().find(|k| k.kind == "main").unwrap();
    let auto = summary.per_kind.iter().find(|k| k.kind == "auto_mode").unwrap();
    assert_eq!(main.requests, 1);
    assert_eq!(auto.requests, 1);

    h.shutdown().await;
}

/// A request that merely *looks* like the classifier (§40): small, cold and
/// deterministic. It must keep its main routing.
#[tokio::test]
async fn a_lookalike_request_keeps_main_routing() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(config_for(addr, &["glm-5.3"]), mock).await;

    let resp = h
        .post(json!({
            "model": "sonnet-class",
            "max_tokens": 64,
            "temperature": 0,
            "system": "Translate to German.",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "zai-main-model");
    assert!(resp.headers().get("x-zroutery-classifier").is_none());

    h.shutdown().await;
}

/// The classifier verdict travels back in the client's dialect, with the
/// candidate's identity — not the model string the client asked for.
#[tokio::test]
async fn the_verdict_comes_back_as_anthropic_shaped_with_candidate_identity() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(config_for(addr, &["glm-5.3"]), mock).await;

    let resp = h.post(classifier_body()).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "message");
    assert_eq!(body["model"], "zai-glm-5.3");
    assert_eq!(body["content"][0]["text"], "<block>no</block>");
    assert_eq!(body["stop_reason"], "end_turn");

    h.shutdown().await;
}

/// The stop sequence reaches the upstream exactly as an array (§17): the wire
/// shape that has broken real classifier traffic on compatible endpoints.
#[tokio::test]
async fn stop_sequences_reach_the_upstream_as_an_array() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(config_for(addr, &["glm-5.3"]), mock).await;

    let resp = h.post(classifier_body()).await;
    assert_eq!(resp.status(), 200);

    let sent = &h.mock.bodies()[0];
    assert_eq!(sent["model"], "glm-5.3");
    assert_eq!(
        sent["stop"],
        json!(["</block>"]),
        "stop must stay an array on the OpenAI dialect"
    );
    assert_eq!(sent["temperature"], 0.0);
    assert_eq!(sent["max_tokens"], 64);

    h.shutdown().await;
}

/// §42: candidate 1 returns 500 → candidate 2 answers, and the client sees
/// one clean response.
#[tokio::test]
async fn a_failing_candidate_fails_over_to_the_next() {
    let (addr, mock) = start_mock().await;
    // The first candidate's upstream model 500s; the second works.
    let h = Harness::start(config_for(addr, &["broken-glm", "backup-glm"]), mock).await;

    let resp = h.post(classifier_body()).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "zai-backup-glm");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "<block>no</block>");

    // Both candidates were actually consulted.
    let bodies = h.mock.bodies();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0]["model"], "broken-glm");
    assert_eq!(bodies[1]["model"], "backup-glm");

    h.shutdown().await;
}

/// HTTP 200 without a verdict is not a success (§20, §21): the answer is
/// retried on the next candidate, and the paraphrase is never forwarded as
/// though it were an approval.
#[tokio::test]
async fn an_answer_without_a_verdict_fails_over() {
    let (addr, mock) = start_mock().await;
    // First candidate says something natural-language and verdict-less.
    let h = Harness::start(config_for(addr, &["garbage-glm", "backup-glm"]), mock).await;

    let resp = h.post(classifier_body()).await;
    // The second candidate produced a real verdict, so the request succeeds.
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "zai-backup-glm");

    let bodies = h.mock.bodies();
    assert_eq!(bodies.len(), 2, "the verdict-less answer must be retried");
    assert_eq!(bodies[0]["model"], "garbage-glm");
    assert_eq!(bodies[1]["model"], "backup-glm");

    h.shutdown().await;
}

/// When every candidate answers without a verdict, the request fails closed
/// (§22): an error back to the client, never a 200 with improvised content,
/// and never silence.
#[tokio::test]
async fn all_answers_without_verdicts_fail_closed() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(config_for(addr, &["garbage-glm", "garbage-backup"]), mock).await;

    let resp = h.post(classifier_body()).await;
    assert_eq!(resp.status(), 502, "no verdict anywhere is an error, not a pass");
    let body: Value = resp.json().await.unwrap();
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("<block>"), "{message}");

    h.shutdown().await;
}

/// Every candidate broken: the failure surfaces to the client, which is what
/// makes Claude Code fall back to asking the user (§42).
#[tokio::test]
async fn all_candidates_failing_surfaces_an_error() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(config_for(addr, &["broken-glm", "broken-backup"]), mock).await;

    let resp = h.post(classifier_body()).await;
    assert_eq!(resp.status(), 500);
    assert_eq!(h.mock.count(), 2, "both candidates were tried");

    h.shutdown().await;
}

/// A non-retryable protocol rejection (400) fails the request rather than
/// being replayed at every candidate — the same semantics main traffic has,
/// and fail-closed either way.
#[tokio::test]
async fn a_non_retryable_rejection_fails_closed() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(config_for(addr, &["refuse-glm", "backup-glm"]), mock).await;

    let resp = h.post(classifier_body()).await;
    assert_eq!(resp.status(), 400);
    assert_eq!(h.mock.count(), 1, "a 400 is not retried at the next candidate");

    h.shutdown().await;
}

/// With classifier routing off, the same classifier-shaped request is routed
/// like any main request (here: to the class).
#[tokio::test]
async fn routing_disabled_means_business_as_usual() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr, &["glm-5.3"]);
    cfg.classifier.enabled = false;
    let h = Harness::start(cfg, mock).await;

    // A sonnet-shaped model string so the request resolves to the class the
    // main model belongs to; the [1m] modifier rides along, as it does in a
    // real session.
    let mut body = classifier_body();
    body["model"] = json!("claude-sonnet-4-5[1m]");
    let resp = h.post(body).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "zai-main-model");
    assert!(resp.headers().get("x-zroutery-classifier").is_none());

    h.shutdown().await;
}
