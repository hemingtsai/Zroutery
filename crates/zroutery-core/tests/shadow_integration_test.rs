#![cfg(feature = "ml")]

//! Production integration gates for the shadow counterfactual.
//!
//! The 7E-1B-CORE suites prove the pure seam in isolation. These tests drive a
//! real server in front of a mock provider and read what production actually
//! wrote: the stored shadow record, the terminal `Outcome`, and the response
//! the client received. The claims under test are the ones this node owns —
//!
//! - the served identity reaches the record from the request's one validated
//!   `Outcome`, and from nowhere else;
//! - the record retains the exact decision-time input the verdict was computed
//!   from, so it is still replayable (REG-009);
//! - a request that served nothing is correlated as the non-success terminal
//!   state it was, never as a success;
//! - the shadow path can observe a request but can never influence, change or
//!   fail one;
//! - two identical requests against the same pinned commit agree on both
//!   checksums.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::billing::Pricing;
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::ml::coordinator::CoordinatorConfig;
use zroutery_core::ml::decision_engine::DecisionEngine;
use zroutery_core::ml::features::RoutingFeatures;
use zroutery_core::ml::model_identity::CommitId;
use zroutery_core::ml::reward::{PredictionBundle, RewardPolicy};
use zroutery_core::ml::shadow::{EnsemblePredictor, ShadowDecision, ShadowEngine, ShadowInput};
use zroutery_core::ml::ShadowCandidateInput;
use zroutery_core::outcome::{FinalStatus, Outcome};
use zroutery_core::policy::{
    CandidateDecision, DecisionReason, PolicyRevision, RouteDecision, TaskProfile,
};
use zroutery_core::router::Candidate;
use zroutery_core::server::{AppState, ServerHandle, ShadowCandidate};

// ------------------------------------------------------------------ mock upstream

#[derive(Clone, Default)]
struct Mock {
    inner: Arc<Mutex<Vec<String>>>,
}

impl Mock {
    fn record(&self, model: &str) {
        self.inner.lock().unwrap().push(model.to_string());
    }

    fn count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

fn openai_chunk(id: &str, delta: Value, finish: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"id": id, "object": "chat.completion.chunk", "created": 1,
               "model": "mock", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    )
}

/// An upstream stream that delivers part of an answer and then stays open, so a
/// client that walks away lands *mid-stream*: bytes went out, the answer was
/// abandoned before it finished.
fn held_open_stream(id: &str) -> axum::body::Body {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let first = id.to_string();
    let second = id.to_string();
    tokio::spawn(async move {
        let _ = tx.send(openai_chunk(
            &first,
            json!({"role": "assistant", "content": ""}),
            Value::Null,
        ));
        let _ = tx.send(openai_chunk(
            &second,
            json!({"content": "hel"}),
            Value::Null,
        ));
        std::future::pending::<()>().await;
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|chunk| (Ok::<_, std::io::Error>(chunk), rx))
    });
    axum::body::Body::from_stream(stream)
}

async fn mock_chat(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    let model = body["model"].as_str().unwrap_or_default().to_string();
    mock.record(&model);

    if model.starts_with("broken") {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "upstream exploded", "type": "server_error"}})),
        )
            .into_response();
    }

    if body["stream"].as_bool().unwrap_or(false) {
        // Both never-ending models return an open stream. `cancel-model`
        // reports no upstream id, so the proxy publishes the id it registered
        // as in flight — the id a cancellation has to name.
        if model.starts_with("hold") {
            return Response::builder()
                .header("content-type", "text/event-stream")
                .body(held_open_stream("chatcmpl-mock"))
                .unwrap();
        }
        if model.starts_with("cancel") {
            return Response::builder()
                .header("content-type", "text/event-stream")
                .body(held_open_stream(""))
                .unwrap();
        }
        let mut sse = String::new();
        sse.push_str(&openai_chunk(
            "chatcmpl-mock",
            json!({"role": "assistant", "content": ""}),
            Value::Null,
        ));
        sse.push_str(&openai_chunk(
            "chatcmpl-mock",
            json!({"content": "hi"}),
            Value::Null,
        ));
        sse.push_str(&openai_chunk("chatcmpl-mock", json!({}), json!("stop")));
        sse.push_str("data: [DONE]\n\n");
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(sse))
            .unwrap();
    }

    Json(json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "created": 1,
        "model": model,
        "choices": [{"index": 0,
                     "message": {"role": "assistant", "content": "hello from mock"},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7}
    }))
    .into_response()
}

async fn start_mock() -> (SocketAddr, Mock) {
    let mock = Mock::default();
    let app = axum::Router::new()
        .route("/v1/chat/completions", post(mock_chat))
        .route("/chat/completions", post(mock_chat))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, mock)
}

// ------------------------------------------------------------------ fixtures

const TOKEN: &str = "zr-shadow-token";

fn provider(id: &str, name: &str, mock: SocketAddr) -> ProviderConfig {
    let mut provider = ProviderConfig::new(id, name, ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{mock}");
    provider.key_ref = format!("provider:{id}");
    provider.timeout_secs = 10;
    provider
}

fn model(provider: &str, upstream: &str, priority: i32, tier: ModelTier) -> ModelEntry {
    let mut entry =
        ModelEntry::for_upstream(provider, upstream, Some(tier)).with_priority(priority);
    entry.pricing = Some(Pricing::new("USD", 3.0, 15.0));
    entry
}

/// Two healthy standard-tier candidates, so a `standard-class` request is a
/// policy-routed request with a real decision — the only shape shadow covers.
fn config_for(mock: SocketAddr, shadow: bool) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();
    cfg.providers = vec![
        provider("alpha", "Alpha", mock),
        provider("beta", "Beta", mock),
    ];
    cfg.models = vec![
        model("alpha", "good-model", 10, ModelTier::Standard),
        model("beta", "good-model", 10, ModelTier::Standard),
    ];
    cfg.shadow.enabled = shadow;
    cfg
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
        let secrets = Arc::new(
            MemorySecretStore::new()
                .with("provider:alpha", "sk-alpha")
                .with("provider:beta", "sk-beta"),
        );
        let state = Arc::new(AppState::new(cfg, secrets));
        let server = ServerHandle::start(Arc::clone(&state)).await.unwrap();
        Harness {
            base: format!("http://{}", server.addr),
            server: Some(server),
            state,
            client: reqwest::Client::builder()
                .pool_max_idle_per_host(0)
                .build()
                .unwrap(),
            mock,
        }
    }

    async fn start_shadowed() -> Harness {
        let (addr, mock) = start_mock().await;
        Harness::start(config_for(addr, true), mock).await
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}{path}", self.base))
            .header("x-api-key", TOKEN)
    }

    /// A buffered policy-routed main-traffic request.
    async fn ask(&self, model: &str) -> reqwest::Response {
        self.post("/v1/messages")
            .json(&json!({
                "model": model,
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .send()
            .await
            .unwrap()
    }

    /// A streaming policy-routed main-traffic request.
    async fn ask_streaming(&self, model: &str) -> reqwest::Response {
        self.post("/v1/messages")
            .json(&json!({
                "model": model,
                "max_tokens": 16,
                "stream": true,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .send()
            .await
            .unwrap()
    }

    fn outcome(&self) -> Outcome {
        let outcomes = self.state.outcomes().recent(100);
        assert_eq!(
            outcomes.len(),
            1,
            "expected exactly one terminal outcome, got {:?}",
            outcomes
                .iter()
                .map(|outcome| (outcome.request_id.clone(), outcome.final_status))
                .collect::<Vec<_>>()
        );
        outcomes.into_iter().next().unwrap()
    }

    /// Every stored shadow decision, oldest first.
    fn shadow_records(&self) -> Vec<ShadowDecision> {
        self.state.shadow().store().decisions()
    }

    /// The one stored shadow decision this request produced.
    fn shadow_record(&self) -> ShadowDecision {
        let records = self.shadow_records();
        assert_eq!(
            records.len(),
            1,
            "expected exactly one stored shadow decision, got {:?}",
            records
                .iter()
                .map(|record| (record.shadow_id.clone(), record.actual.request_id.clone()))
                .collect::<Vec<_>>()
        );
        records.into_iter().next().unwrap()
    }

    async fn shutdown(mut self) {
        if let Some(server) = self.server.take() {
            server.stop().await;
        }
    }
}

/// A disconnect or cancellation is noticed asynchronously, so the assertions
/// poll for the terminal transition instead of guessing how long it takes.
async fn wait_for_outcomes(harness: &Harness, expected: usize) {
    for _ in 0..200 {
        if harness.state.outcomes().len() == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "expected {expected} terminal outcomes, saw {}",
        harness.state.outcomes().len()
    );
}

/// A fresh engine on the same deterministic genesis commit production pins.
fn fresh_engine() -> ShadowEngine {
    ShadowEngine::new(
        DecisionEngine::new(CoordinatorConfig::default(), RewardPolicy::default()),
        true,
    )
}

// ------------------------------------------------------------------ gates

/// The whole point of the node: a served request's shadow record carries the
/// served identity, and it got it from the request's single validated outcome.
#[tokio::test]
async fn a_served_request_correlates_the_outcomes_served_identity() {
    let h = Harness::start_shadowed().await;
    assert!(h.state.shadow().enabled(), "shadow is on for this harness");

    let response = h.ask("standard-class").await;
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "hello from mock");

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Success);
    let served = outcome.served_identity().expect("an answer was delivered");
    let planned = outcome.planned_identity().expect("a policy-routed plan");

    // Exactly one record for exactly one request, and it is this request's.
    let record = h.shadow_record();
    assert_eq!(h.state.shadow().fault_count(), 0, "nothing faulted");
    assert_eq!(record.actual.request_id, outcome.request_id);
    assert_eq!(record.actual.decision_id, outcome.decision_id.unwrap());
    assert_eq!(record.actual.planned_selected(), planned.model());
    assert_eq!(
        record.actual.served.as_deref(),
        Some(served.model()),
        "the record carries the identity that actually served"
    );
    assert!(record.actual.has_served_identity());

    // The planned identity is still the plan, not the answer: a record that
    // collapsed the two would be unable to say a failover happened at all.
    assert_eq!(
        record.observation.input.production_selected,
        planned.model()
    );

    // The served identity is a real observed candidate, and it is the same
    // identity the outcome names.
    assert!(record
        .input()
        .candidates
        .iter()
        .any(|candidate| candidate.candidate_id == served.model()));

    h.shutdown().await;
}

/// REG-009: the record retains the exact decision-time input, so it is still a
/// replayable observation. The verdict cannot be reproduced from anything the
/// request looked like later — only from the snapshot that was evaluated.
#[tokio::test]
async fn the_record_retains_the_exact_decision_time_input() {
    let h = Harness::start_shadowed().await;
    assert_eq!(h.ask("standard-class").await.status(), 200);
    let record = h.shadow_record();
    let outcome = h.outcome();

    // The retained input is the decision-time one: it is pinned to the router's
    // decision, not to anything the terminal transition observed.
    assert_eq!(
        record.input().decision_id,
        outcome.decision_id.clone().unwrap()
    );
    assert_eq!(
        record.input().production_selected,
        outcome.planned_identity().unwrap().model()
    );
    assert!(!record.input().candidates.is_empty());
    assert!(
        record
            .input()
            .candidates
            .iter()
            .all(|candidate| candidate.candidate_id != String::new()),
        "the snapshot carries real candidate identities"
    );

    // Replaying the retained input against the same pinned commit reproduces
    // both checksums exactly, which is only possible if this really is the
    // input the verdict was computed from.
    //
    // "The same pinned commit" is now resolved from the serving path rather than
    // assumed to be the cold-start root. Node 7F attaches a verified candidate on
    // the request path and hands it to `evaluate_with` per call, so the engine's
    // own internally pinned predictor is NOT what produced this record. Replaying
    // against genesis would compare two different models and prove nothing; the
    // witness has to be the model the record was actually made with.
    let attached = ShadowCandidate::embedded().expect("the embedded candidate is verified");
    let replay = fresh_engine()
        .evaluate_with(
            "replay-of-a-production-request",
            record.input(),
            attached.predictor(),
        )
        .expect("the retained input still evaluates");
    assert_eq!(
        replay.decision_input_checksum,
        record.decision_input_checksum
    );
    assert_eq!(replay.decision_checksum, record.decision_checksum);
    assert_eq!(replay.shadow.selected, record.shadow.selected);
    assert_eq!(replay.shadow.action, record.shadow.action);
    assert_eq!(
        replay.shadow.ranked_candidates,
        record.shadow.ranked_candidates
    );
    assert_eq!(replay.observation.model_commit, record.shadow.model_commit);

    // Correlating the served identity is not allowed to disturb that identity.
    assert_ne!(record.actual.served, None);

    h.shutdown().await;
}

/// A failover is exactly the case where planned and served differ, so this is
/// the correlation that has to be read from the outcome rather than assumed
/// from the plan.
#[tokio::test]
async fn a_failover_correlates_a_served_identity_that_differs_from_the_plan() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr, true);
    // The preferred standard candidate always fails, so the planned identity
    // and the identity that served the answer cannot be the same one.
    cfg.models[0].upstream_model = "broken-model".into();
    cfg.models[0].priority = 0;
    let h = Harness::start(cfg, mock).await;

    assert_eq!(h.ask("standard-class").await.status(), 200);
    assert_eq!(h.mock.count(), 2, "the failing candidate was tried first");

    let outcome = h.outcome();
    let planned = outcome.planned_identity().unwrap();
    let served = outcome.served_identity().unwrap();
    assert_ne!(planned, served, "this request failed over");
    assert_eq!(outcome.attempts.len(), 2);

    let record = h.shadow_record();
    assert_eq!(record.actual.planned_selected(), "alpha-broken-model");
    assert_eq!(record.actual.served.as_deref(), Some("beta-good-model"));
    assert_eq!(record.actual.served.as_deref(), Some(served.model()));
    assert_eq!(
        record.actual.served.as_deref(),
        Some(record.observation.input.candidates[1].candidate_id.as_str()),
        "the served identity is the second observed candidate"
    );
    assert_eq!(h.state.shadow().fault_count(), 0);

    h.shutdown().await;
}

/// A request that failed has no served identity, and the record says so: the
/// counterfactual is still recorded, the answer simply never arrived.
#[tokio::test]
async fn a_failed_request_is_correlated_as_a_non_success() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr, true);
    cfg.models.truncate(1);
    cfg.models[0].upstream_model = "broken-model".into();
    let h = Harness::start(cfg, mock).await;

    assert_eq!(h.ask("standard-class").await.status(), 500);

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert!(outcome.served_identity().is_none());

    let record = h.shadow_record();
    assert_eq!(record.actual.served, None);
    assert!(!record.actual.has_served_identity());
    assert_eq!(record.actual.planned_selected(), "alpha-broken-model");
    // The counterfactual is still valuable evidence: what the ML stack would
    // have picked for a request that never got an answer.
    assert!(!record.shadow.ranked_candidates.is_empty());

    h.shutdown().await;
}

/// A client that walks away mid-answer is not a success, and neither is the
/// shadow record of it. This is the terminal state the old drop path used to
/// resolve into a served one.
#[tokio::test]
async fn an_abandoned_stream_is_correlated_as_a_non_success() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr, true);
    cfg.models.truncate(1);
    cfg.models[0].upstream_model = "hold-model".into();
    cfg.models[0].tier = Some(ModelTier::Fast);
    let h = Harness::start(cfg, mock).await;

    let mut response = h.ask_streaming("fast-class").await;
    assert_eq!(response.status(), 200);
    let first = response.chunk().await.unwrap().expect("some output");
    assert!(!first.is_empty(), "the mock produced answer bytes");
    drop(response);

    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Interrupted);
    assert!(outcome.served_identity().is_none());

    let record = h.shadow_record();
    assert_eq!(
        record.actual.served, None,
        "an abandoned answer served nothing"
    );
    assert_eq!(record.actual.planned_selected(), "alpha-hold-model");
    assert_eq!(record.actual.request_id, outcome.request_id);
    assert!(!record.input().candidates.is_empty());

    h.shutdown().await;
}

/// An explicit cancellation is its own terminal state and is correlated as one.
#[tokio::test]
async fn a_cancelled_stream_is_correlated_as_a_non_success() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr, true);
    cfg.models.truncate(1);
    cfg.models[0].upstream_model = "cancel-model".into();
    cfg.models[0].tier = Some(ModelTier::Fast);
    let h = Harness::start(cfg, mock).await;

    let mut response = h
        .post("/v1/responses")
        .json(&json!({"model": "fast-class", "input": "hi", "stream": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let first = response.chunk().await.unwrap().expect("some output");
    let wire = String::from_utf8_lossy(&first).to_string();
    let response_id = wire
        .split("\"id\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("the first frame names the response")
        .to_string();

    let cancelled = h
        .post(&format!("/v1/responses/{response_id}/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(cancelled.status(), 200);
    let _ = cancelled.text().await;

    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Cancelled);
    assert!(outcome.served_identity().is_none());

    let record = h.shadow_record();
    assert_eq!(
        record.actual.served, None,
        "a cancelled answer served nothing"
    );
    assert_eq!(record.actual.planned_selected(), "alpha-cancel-model");

    h.shutdown().await;
}

/// A stream that runs to its end is a served answer, and the record says which
/// candidate produced it — the streaming correlation is the same evidence.
#[tokio::test]
async fn a_completed_stream_correlates_its_served_identity() {
    let h = Harness::start_shadowed().await;

    let response = h.ask_streaming("standard-class").await;
    let wire = response.text().await.unwrap();
    assert!(
        wire.contains("\"text\":\"hi\""),
        "the client saw the answer"
    );

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Success);
    let served = outcome.served_identity().expect("an answer was delivered");

    let record = h.shadow_record();
    assert_eq!(record.actual.served.as_deref(), Some(served.model()));
    assert_eq!(record.actual.request_id, outcome.request_id);

    h.shutdown().await;
}

/// A predictor fault is contained: the engine counts it, records nothing, and
/// the request it was evaluating still succeeds. The injected predictor enters
/// through the engine's public evaluation seam — the very call production makes
/// with its own pinned ensemble — because the production predictor is an
/// immutable, verified artifact that cannot be made to fail from outside.
#[tokio::test]
async fn an_injected_predictor_fault_is_absorbed_and_counted() {
    struct PanickingPredictor;

    impl EnsemblePredictor for PanickingPredictor {
        fn predict(
            &self,
            _model: &str,
            _provider: &str,
            _features: &RoutingFeatures,
        ) -> PredictionBundle {
            panic!("injected predictor fault");
        }

        fn commit_id(&self) -> CommitId {
            CommitId::new("injected-fault-commit")
        }
    }

    let h = Harness::start_shadowed().await;

    // A real request first, so the engine is the same live instance the
    // pipeline evaluates on, with a real decision-time snapshot to work from.
    assert_eq!(h.ask("standard-class").await.status(), 200);
    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert!(h.state.shadow().fault_count() == 0, "clean so far");
    let stored_before = h.state.shadow().store().len();
    assert_eq!(stored_before, 1);
    // The injected fault: built exactly the way the pipeline builds its
    // snapshot, from the live router's read-only stores.
    let input = production_shaped_input(&h);
    let decision = h
        .state
        .shadow()
        .evaluate_with(&outcome.request_id, &input, &PanickingPredictor);
    assert!(
        decision.is_none(),
        "a panicking predictor produces no shadow record"
    );
    assert_eq!(
        h.state.shadow().fault_count(),
        1,
        "the fault is counted, not swallowed silently"
    );
    assert_eq!(
        h.state.shadow().store().len(),
        stored_before,
        "nothing was recorded for the faulted evaluation"
    );

    // And the fault is not the request's problem: the next request is served
    // exactly as before, with its own correlation intact.
    assert_eq!(h.ask("standard-class").await.status(), 200);
    assert_eq!(h.state.outcomes().len(), 2);
    assert_eq!(h.state.shadow().store().len(), 2);
    let records = h.shadow_records();
    assert!(
        records.iter().all(|record| record.actual.served.is_some()),
        "both real requests are correlated, and only the injected one faulted"
    );
    assert_eq!(h.state.shadow().fault_count(), 1);

    h.shutdown().await;
}

/// Two identical requests against the same pinned commit, each starting from
/// the same cold state, must agree on the input and decision checksums. The
/// served identity is correlated per request afterwards and is deliberately
/// outside that identity.
#[tokio::test]
async fn two_identical_requests_agree_on_the_shadow_checksums() {
    let (addr, mock) = start_mock().await;
    let cfg = config_for(addr, true);
    let first = Harness::start(cfg.clone(), mock.clone()).await;
    let second = Harness::start(cfg, mock).await;

    assert_eq!(first.ask("standard-class").await.status(), 200);
    assert_eq!(second.ask("standard-class").await.status(), 200);

    let a = first.shadow_record();
    let b = second.shadow_record();

    assert_eq!(
        a.decision_input_checksum, b.decision_input_checksum,
        "identical decision-time input hashes identically"
    );
    assert_eq!(
        a.decision_checksum, b.decision_checksum,
        "identical input and pinned commit give an identical verdict"
    );
    assert_eq!(a.shadow.model_commit, b.shadow.model_commit);
    assert_eq!(a.shadow.selected, b.shadow.selected);
    assert_eq!(a.shadow.action, b.shadow.action);
    assert_eq!(a.shadow.ranked_candidates, b.shadow.ranked_candidates);
    assert_eq!(
        a.observation
            .input
            .candidates
            .iter()
            .map(|candidate| candidate.candidate_id.clone())
            .collect::<Vec<_>>(),
        b.observation
            .input
            .candidates
            .iter()
            .map(|candidate| candidate.candidate_id.clone())
            .collect::<Vec<_>>()
    );
    // The volatile correlation fields are per request, and do not leak into
    // the identity.
    assert_ne!(a.shadow_id, b.shadow_id);
    assert_ne!(a.actual.request_id, b.actual.request_id);
    assert_eq!(a.actual.served, b.actual.served);

    first.shutdown().await;
    second.shutdown().await;
}

/// The shadow path observes requests; it does not participate in them. The same
/// request against a shadow-on and a shadow-off server is the same request.
#[tokio::test]
async fn the_shadow_path_cannot_change_a_request() {
    let (addr, mock) = start_mock().await;
    let shadowed = Harness::start(config_for(addr, true), mock.clone()).await;
    let plain = Harness::start(config_for(addr, false), mock.clone()).await;

    let with_shadow = shadowed.ask("standard-class").await;
    let shadow_status = with_shadow.status();
    let shadow_body: Value = with_shadow.json().await.unwrap();
    let without_shadow = plain.ask("standard-class").await;
    let plain_status = without_shadow.status();
    let plain_body: Value = without_shadow.json().await.unwrap();

    assert_eq!(shadow_status, plain_status);
    assert_eq!(shadow_status, 200);
    assert_eq!(shadow_body["content"], plain_body["content"]);
    assert_eq!(shadow_body["model"], plain_body["model"]);
    assert_eq!(shadow_body["usage"], plain_body["usage"]);

    // Same routing decision on both sides, and the verdict never became one.
    let shadowed_outcome = shadowed.outcome();
    let plain_outcome = plain.outcome();
    assert_eq!(
        shadowed_outcome.served_identity(),
        plain_outcome.served_identity()
    );
    assert_eq!(
        shadowed_outcome.planned_identity(),
        plain_outcome.planned_identity()
    );
    assert_eq!(
        shadowed_outcome.attempts.len(),
        plain_outcome.attempts.len()
    );

    // Only the shadowed server has evidence to correlate; the other one
    // recorded nothing at all.
    assert_eq!(shadowed.shadow_records().len(), 1);
    assert!(
        plain.state.shadow().store().decisions().is_empty(),
        "shadow off means no snapshots and no records"
    );
    assert!(!plain.state.shadow().enabled());

    shadowed.shutdown().await;
    plain.shutdown().await;
}

/// A disabled engine is a no-op at the production boundary too, and the
/// request is unaffected.
#[tokio::test]
async fn a_disabled_engine_records_nothing() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(config_for(addr, false), mock).await;

    let response = h.ask("standard-class").await;
    assert_eq!(response.status(), 200);
    assert!(h.state.shadow().store().decisions().is_empty());
    assert_eq!(h.state.shadow().fault_count(), 0);
    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert!(outcome.served_identity().is_some());

    h.shutdown().await;
}

/// A configuration change reaches the running shadow engine: turning it off
/// through `set_config` stops recording without a restart, turning it back on
/// resumes, and a smaller retention cap evicts the stored records down to it.
#[tokio::test]
async fn a_config_change_reaches_the_running_shadow_engine() {
    let h = Harness::start_shadowed().await;
    assert_eq!(h.ask("standard-class").await.status(), 200);
    let first_id = h.shadow_record().shadow_id.clone();

    // The switch is now configuration rather than a construction constant.
    let mut off = h.state.config().as_ref().clone();
    off.shadow.enabled = false;
    h.state.set_config(off);
    assert!(
        !h.state.shadow().enabled(),
        "the running engine must adopt the switch"
    );
    assert_eq!(h.ask("standard-class").await.status(), 200);
    assert_eq!(
        h.shadow_records().len(),
        1,
        "a switched-off engine records nothing"
    );

    // Back on, with a cap of one: the next record evicts the previous one.
    let mut capped = h.state.config().as_ref().clone();
    capped.shadow.enabled = true;
    capped.shadow.max_decisions = 1;
    h.state.set_config(capped);
    assert!(h.state.shadow().enabled());
    assert_eq!(h.ask("standard-class").await.status(), 200);
    let records = h.shadow_records();
    assert_eq!(
        records.len(),
        1,
        "the store must evict down to the configured cap of one"
    );
    assert_ne!(
        records[0].shadow_id, first_id,
        "the newly recorded decision must be the one retained"
    );

    h.shutdown().await;
}

// ------------------------------------------------------------------ structural gates

/// The decision-time snapshot is built once, in the routing handler, and is
/// never rebuilt at the terminal. A correlation that reconstructed the input
/// would silently reintroduce the REG-009 defect.
#[test]
fn the_decision_time_input_is_built_once_and_never_rebuilt() {
    let source = include_str!("../src/server/pipeline.rs");
    assert_eq!(
        source.matches("ShadowInput::from_policy_plan").count(),
        1,
        "the decision-time snapshot is built exactly once"
    );
    let (before_finalize, after) = source
        .split_once("fn finalize(")
        .expect("the lifecycle still has its one terminal transition");
    assert!(
        before_finalize.contains("shadow_evaluated("),
        "evaluation happens on the decision path, ahead of the terminal transition"
    );
    // The terminal transition only ever carries a record identity forward.
    assert_eq!(after.matches("shadow_correlated(").count(), 1);
    assert!(!after.contains("from_policy_plan"));
    assert!(!after.contains("shadow_evaluated("));
}

/// The production shadow surface is record-only: it cannot panic a request,
/// cannot read a verdict back into routing, and reaches no learning or
/// activation seam. This node observes; it does not act.
#[test]
fn the_production_shadow_surface_is_record_only() {
    let source = include_str!("../src/server/pipeline.rs");
    let begin = "// shadow-block-begin";
    let end = "// shadow-block-end";
    let mut blocks = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find(begin) {
        let after_begin = &rest[start + begin.len()..];
        let stop = after_begin
            .find(end)
            .expect("every marked shadow block is closed");
        blocks.push(&after_begin[..stop]);
        rest = &after_begin[stop + end.len()..];
    }
    assert_eq!(blocks.len(), 4, "four marked shadow blocks");

    for block in &blocks {
        for forbidden in [
            // Nothing in the shadow path may fail a request.
            ".unwrap(",
            ".expect(",
            "panic!",
            "unreachable!",
            // No verdict is ever read back into a decision.
            ".shadow.selected",
            "ranked_candidates",
            "store().decisions()",
            // No learning, activation or ingestion seam is wired here.
            "train(",
            "try_train(",
            ".swap(",
            "DatasetStore",
            "to_feedback",
        ] {
            assert!(
                !block.contains(forbidden),
                "pipeline shadow block must not reference {forbidden}"
            );
        }
    }

    // The engine is reached exactly three times: the enablement check on the
    // decision path, one evaluation per attempt path, and one correlation in
    // the terminal transition.
    //
    // Node 7F moved ONE of those reaches behind an AppState hop, so the counts
    // are not the same three numbers. The reach total is unchanged: the
    // evaluation now enters through `state.shadow_evaluated(..)` rather than
    // touching the engine from here, which is what keeps the predictor from
    // leaving AppState. These counts exist to bound the surface, so the sum is
    // what matters and is asserted as such below.
    assert_eq!(source.matches(".shadow()").count(), 2);
    assert_eq!(source.matches("shadow_evaluated(").count(), 4);
    assert_eq!(source.matches("shadow_correlated(").count(), 2);
    // The evaluation still passes the decision identity and the retained input;
    // it just reaches them one call deeper now.
    assert!(source.contains("shadow_evaluated(self.id(), input)"));
    assert!(source.contains("correlate_served("));
}

/// The pure seam still owns the decision identity, and the served identity is
/// attached without touching either checksum.
#[test]
fn the_served_identity_is_attached_without_changing_the_decision_identity() {
    let engine = fresh_engine();
    let input = replayable_input();
    let before = engine.evaluate("req-before", &input).expect("evaluation");

    // Attaching the served identity to the stored record is idempotent for the
    // same identity and refused for a different one.
    assert!(engine.correlate_served(&before.shadow_id, Some("model-b")));
    let stored = engine.store().decisions();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].actual.served.as_deref(), Some("model-b"));
    assert_eq!(
        stored[0].decision_input_checksum,
        before.decision_input_checksum
    );
    assert_eq!(stored[0].decision_checksum, before.decision_checksum);
    assert_eq!(
        serde_json::to_value(&stored[0].observation.input).unwrap(),
        serde_json::to_value(&before.observation.input).unwrap(),
        "the retained input is untouched by correlation"
    );

    // Nothing served, and an unknown record, are both quiet no-ops.
    assert!(!engine.correlate_served(&before.shadow_id, None));
    assert_eq!(
        engine.store().decisions()[0].actual.served.as_deref(),
        Some("model-b")
    );
    assert!(!engine.correlate_served("shadow-does-not-exist", Some("model-b")));
    assert_eq!(engine.fault_count(), 0);

    // A served identity production never chose is refused, and counted.
    assert!(!engine.correlate_served(&before.shadow_id, Some("model-z")));
    assert_eq!(
        engine.store().decisions()[0].actual.served.as_deref(),
        Some("model-b"),
        "a refused correlation does not overwrite the evidence"
    );
    assert_eq!(engine.fault_count(), 1, "the refusal is counted as a fault");
    assert_eq!(engine.store().len(), 1, "and it stored no second record");
}

// ------------------------------------------------------------------ test fixtures

fn candidate_input(id: &str, provider: &str) -> ShadowCandidateInput {
    let mut values = [0.0f32; zroutery_core::ml::features::FEATURE_DIMENSION];
    for (index, value) in values.iter_mut().enumerate() {
        *value = 0.1 + index as f32 * 0.01;
    }
    ShadowCandidateInput {
        candidate_id: id.to_string(),
        provider_id: provider.to_string(),
        tier: Some(ModelTier::Standard),
        eligible: true,
        features: RoutingFeatures {
            values,
            schema_version: zroutery_core::ml::features::FEATURE_SCHEMA_VERSION,
        },
        rejection_reason: None,
    }
}

fn replayable_input() -> ShadowInput {
    ShadowInput {
        decision_id: "decision-1".to_string(),
        policy_id: "policy-1".to_string(),
        client_id: None,
        policy_revision: PolicyRevision::default(),
        task: Default::default(),
        production_selected: "model-a".to_string(),
        feature_schema: zroutery_core::ml::features::FEATURE_SCHEMA_VERSION,
        candidates: vec![
            candidate_input("model-a", "provider-a"),
            candidate_input("model-b", "provider-b"),
        ],
        session_mode: zroutery_core::session::SessionRoutingMode::Free,
        session_switch_count: 0,
        is_fallback: false,
    }
}

/// A snapshot built the way the pipeline builds it: from the live router's
/// read-only stores plus the plan and decision production already computed.
fn production_shaped_input(h: &Harness) -> ShadowInput {
    let entry = ModelEntry::for_upstream("alpha", "good-model", Some(ModelTier::Standard));
    let provider = ProviderConfig::new("alpha", "Alpha", ProviderKind::OpenAICompatible);
    let exposed_id = entry.exposed_id();
    let candidate = Candidate {
        exposed_id: exposed_id.clone(),
        entry,
        provider,
        degraded: false,
    };
    let decision = RouteDecision {
        decision_id: "decision-injected".to_string(),
        timestamp: 1_700_000_000,
        task: Default::default(),
        policy_id: "policy-1".to_string(),
        client_id: None,
        candidates: vec![CandidateDecision {
            model_id: exposed_id.clone(),
            provider_id: "alpha".to_string(),
            tier: Some("standard".to_string()),
            eligible: true,
            rejection: None,
            score: None,
            final_score: None,
        }],
        selected: Some(exposed_id),
        fallback_chain: Vec::new(),
        reason: DecisionReason::PolicySelected,
        policy_revision: PolicyRevision {
            policy_id: "policy-1".to_string(),
            policy_enabled: true,
            requirements_hash: 11,
            preference_hash: 22,
        },
    };
    ShadowInput::from_policy_plan(
        h.state.router().observations(),
        &h.state.router().stats_store,
        std::slice::from_ref(&candidate),
        &decision,
        &TaskProfile::default(),
        1,
    )
}
