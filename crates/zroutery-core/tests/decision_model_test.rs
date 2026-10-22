//! A decision model answering the Auto Mode's question, end to end.
//!
//! # What this file is
//!
//! The Auto Mode asks a binary question — *may this action proceed?* — and a
//! language model answers it in prose that the proxy reads back out of a
//! `<block>yes</block>` tag. A decision model answers the same question in its
//! own protocol: a state, a typed question, and a calibrated probability
//! instead of a tag.
//!
//! So the proxy has to be the one that speaks both. It asks the decision
//! model in the decision protocol, and it answers the client in the client's
//! protocol — the client sees exactly what a language model's verdict would
//! have looked like, and nothing downstream knows which kind of model
//! answered.
//!
//! # What is driven here
//!
//! The whole path, against a real endpoint on a real socket: the classifier
//! request, the decision body that goes out, the answer that comes back, the
//! probability that becomes a verdict, and the text the client receives.
//!
//! The three answers are the three that matter, and they are the three the
//! threshold is about:
//!
//! * a confident *yes* blocks,
//! * a confident *no* allows,
//! * and an answer that is not confident **blocks**, because the Auto Mode is
//!   fail-closed: a decision that is not sure has not cleared the action.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

use zroutery_core::config::{
    AppConfig, ClassifierCandidate, ClassifierConfig, MemorySecretStore, ModelEntry,
    ProviderConfig, ProviderKind,
};
use zroutery_core::server::{AppState, ServerHandle};

const TOKEN: &str = "test-token";

/// What the decision endpoint answered, and what it was asked.
#[derive(Clone, Default)]
struct DecisionMock {
    inner: Arc<Mutex<DecisionInner>>,
}

#[derive(Default)]
struct DecisionInner {
    /// The probability every answer carries.
    noul: f64,
    /// Every body the endpoint received.
    bodies: Vec<Value>,
    /// Every `Authorization` header it saw.
    keys: Vec<String>,
}

impl DecisionMock {
    fn answering(noul: f64) -> Self {
        let mock = Self::default();
        mock.inner.lock().unwrap().noul = noul;
        mock
    }

    fn bodies(&self) -> Vec<Value> {
        self.inner.lock().unwrap().bodies.clone()
    }

    fn keys(&self) -> Vec<String> {
        self.inner.lock().unwrap().keys.clone()
    }
}

/// `POST /v1/systemone` — the decision protocol's own endpoint.
async fn decide(
    State(mock): State<DecisionMock>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let mut inner = mock.inner.lock().unwrap();
    inner.bodies.push(body);
    inner.keys.push(
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string(),
    );
    let noul = inner.noul;
    drop(inner);
    Json(json!({ "decision": { "type": "noul", "noul": noul } })).into_response()
}

/// A decision endpoint on a real port, answering with `noul`.
async fn start_decision_endpoint(noul: f64) -> (String, DecisionMock) {
    let mock = DecisionMock::answering(noul);
    let app = axum::Router::new()
        .route("/v1/systemone", post(decide))
        .with_state(mock.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{address}"), mock)
}

/// A configuration whose Auto Mode pool is one decision model.
fn config_for(base: &str, threshold: f64) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();

    let mut decision = ProviderConfig::new("jev", "Jev", ProviderKind::Decision);
    decision.base_url = base.into();
    decision.key_ref = "provider:jev".into();
    decision.timeout_secs = 10;
    decision.decision.path = "/v1/systemone".into();
    decision.decision.threshold = threshold;
    cfg.providers = vec![decision];

    // No tier: a decision model is not a candidate for main traffic, and a
    // tier it was filed under would put it in front of requests it cannot
    // answer.
    cfg.models = vec![ModelEntry::for_upstream("jev", "jev", None)];

    cfg.classifier = ClassifierConfig {
        enabled: true,
        candidates: vec![ClassifierCandidate {
            model: "jev-jev".into(),
            priority: 10,
            enabled: true,
        }],
        ..ClassifierConfig::default()
    };
    cfg
}

fn secrets() -> Arc<MemorySecretStore> {
    Arc::new(MemorySecretStore::new().with("provider:jev", "typesafe-key"))
}

struct Harness {
    base: String,
    server: Option<ServerHandle>,
}

impl Harness {
    async fn start(cfg: AppConfig) -> Harness {
        let state = Arc::new(AppState::new(cfg, secrets()));
        let server = ServerHandle::start(Arc::clone(&state)).await.unwrap();
        Harness {
            base: format!("http://{}", server.addr),
            server: Some(server),
        }
    }

    /// The Auto Mode's question, in the shape a client sends it.
    async fn ask(&self) -> (reqwest::StatusCode, String) {
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .unwrap();
        let resp = client
            .post(format!("{}/v1/messages", self.base))
            .header("x-api-key", TOKEN)
            .json(&json!({
                "model": "claude-opus-4-8[1m]",
                "max_tokens": 64,
                "temperature": 0,
                "stop_sequences": ["</block>"],
                "system": [{"type": "text", "text":
                    "You are a security monitor for autonomous AI coding agents."}],
                "messages": [{"role": "user", "content": "should this run?"}]
            }))
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let text = resp.text().await.unwrap();
        (status, text)
    }

    async fn shutdown(mut self) {
        if let Some(s) = self.server.take() {
            s.stop().await;
        }
    }
}

/// A confident *yes* blocks, and the client is told so in its own protocol.
#[tokio::test]
async fn a_confident_yes_blocks_the_action() {
    let (base, mock) = start_decision_endpoint(0.97).await;
    let h = Harness::start(config_for(&base, 0.85)).await;

    let (status, text) = h.ask().await;
    assert_eq!(status, 200, "the body was {text}");
    assert!(
        text.contains("<block>yes</block>"),
        "a confident yes must block, and the client must see the tag: {text}"
    );

    // The question went out in the decision protocol: the state is what the
    // client sent, and the question is the role the client gave the model.
    let bodies = mock.bodies();
    assert_eq!(bodies.len(), 1, "the endpoint was asked {bodies:?}");
    let body = &bodies[0];
    assert_eq!(body["state"], "should this run?");
    assert_eq!(
        body["questions"]["decision"]["type"], "noul",
        "the Auto Mode asks a binary question: {body}"
    );
    assert_eq!(
        body["questions"]["decision"]["instructions"],
        "You are a security monitor for autonomous AI coding agents."
    );

    // And the credential was presented the way the family presents it.
    assert_eq!(mock.keys(), vec!["Bearer typesafe-key".to_string()]);

    h.shutdown().await;
}

/// A confident *no* allows.
#[tokio::test]
async fn a_confident_no_allows_the_action() {
    let (base, _mock) = start_decision_endpoint(0.02).await;
    let h = Harness::start(config_for(&base, 0.85)).await;

    let (status, text) = h.ask().await;
    assert_eq!(status, 200, "the body was {text}");
    assert!(
        text.contains("<block>no</block>"),
        "a confident no must allow: {text}"
    );
    assert!(
        !text.contains("<block>yes</block>"),
        "a confident no must not block: {text}"
    );

    h.shutdown().await;
}

/// An answer that is not confident blocks, because the gate is fail-closed.
#[tokio::test]
async fn an_unconfident_answer_blocks_the_action() {
    let (base, _mock) = start_decision_endpoint(0.5).await;
    let h = Harness::start(config_for(&base, 0.85)).await;
    let (status, text) = h.ask().await;
    assert_eq!(status, 200, "the body was {text}");
    assert!(
        text.contains("<block>yes</block>"),
        "an answer that is not confident has not cleared the action: {text}"
    );
    h.shutdown().await;
}

/// A decision model is not a candidate for main traffic.
///
/// It answers typed questions and cannot answer a request that wants prose,
/// so it is not in the tier pools and it is not in the model listing: a
/// client that asked for it would be asking a model that cannot answer.
#[tokio::test]
async fn a_decision_model_is_not_offered_for_main_traffic() {
    let (base, _mock) = start_decision_endpoint(0.97).await;
    let mut cfg = config_for(&base, 0.85);
    // File it under a tier as well, so the rule is proved rather than assumed.
    cfg.models[0].tier = Some(zroutery_core::config::ModelTier::Standard);
    let state = AppState::new(cfg, secrets());

    let registry = state.registry();
    assert!(
        registry
            .tier_members(zroutery_core::config::ModelTier::Standard)
            .is_empty(),
        "a decision model is not a tier member"
    );
    assert!(
        !registry.list().iter().any(|m| m.id == "jev-jev"),
        "a decision model is not published to clients"
    );
}

/// A decision endpoint that answers with no probability is no answer at all.
#[tokio::test]
async fn an_endpoint_that_answers_with_no_probability_is_no_verdict() {
    use zroutery_core::classifier::{verdict_from_decision, ClassifierVerdict};

    // No answer at all.
    assert_eq!(
        verdict_from_decision(None, 0.85),
        ClassifierVerdict::Unparseable
    );
    // An answer, and the two confident bands.
    let confident_yes = zroutery_core::upstream::Decision { noul: 0.97 };
    let confident_no = zroutery_core::upstream::Decision { noul: 0.02 };
    let unsure = zroutery_core::upstream::Decision { noul: 0.5 };
    assert_eq!(
        verdict_from_decision(Some(confident_yes), 0.85),
        ClassifierVerdict::Block
    );
    assert_eq!(
        verdict_from_decision(Some(confident_no), 0.85),
        ClassifierVerdict::Allow
    );
    assert_eq!(
        verdict_from_decision(Some(unsure), 0.85),
        ClassifierVerdict::Unparseable
    );
}
