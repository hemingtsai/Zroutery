#![cfg(feature = "ml")]

//! Production dataset ingestion gates.
//!
//! The pure boundary claims live in `dataset_ingestion_test.rs`. These tests
//! drive a real server in front of a mock provider and read what production
//! actually wrote, because the claims that matter here are about the wiring:
//!
//! - a served request produces exactly one bounded, validated ingestion, and the
//!   feature vectors in it are the ones the shadow record retained at decision
//!   time — the same values, for the same candidate, not a second extraction;
//! - a failed, abandoned or cancelled request is retained as the negative
//!   sample it was, with its failure class and terminal state intact;
//! - a request with no retained decision-time input ingests nothing, and the
//!   store says that is what happened rather than staying silent;
//! - collecting samples changes no response and can fail no request.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::billing::Pricing;
use zroutery_core::budget::{Budget, BudgetPeriod, BudgetScope};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::ml::dataset::{OutcomeTrainingSample, SampleScope};
use zroutery_core::ml::shadow::ShadowDecision;
use zroutery_core::outcome::{FinalStatus, Outcome};
use zroutery_core::server::{AppState, ServerHandle};

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
/// client that walks away lands *mid-stream*.
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
        if model.starts_with("hold") {
            return Response::builder()
                .header("content-type", "text/event-stream")
                .body(held_open_stream("chatcmpl-mock"))
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

const TOKEN: &str = "zr-dataset-token";

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
/// policy-routed request with a real decision — the only shape that carries a
/// decision-time input.
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

    /// Every terminal outcome recorded so far, newest first.
    fn all_outcomes(&self) -> Vec<Outcome> {
        self.state.outcomes().recent(100)
    }

    fn shadow_record(&self) -> ShadowDecision {
        let records = self.state.shadow().store().decisions();
        assert_eq!(records.len(), 1, "one decision-time record per request");
        records.into_iter().next().unwrap()
    }

    /// Every stored canonical sample, oldest first.
    fn samples(&self) -> Vec<OutcomeTrainingSample> {
        self.state.dataset().training_slice()
    }

    /// The samples production collected for one request.
    fn samples_for(&self, request_id: &str) -> Vec<OutcomeTrainingSample> {
        let samples: Vec<OutcomeTrainingSample> = self
            .samples()
            .into_iter()
            .filter(|sample| sample.request_id == request_id)
            .collect();
        assert!(
            !samples.is_empty(),
            "no sample was collected for '{request_id}'"
        );
        samples
    }

    /// The request-scope sample for one request.
    fn request_sample(&self, request_id: &str) -> OutcomeTrainingSample {
        self.samples_for(request_id)
            .into_iter()
            .find(|sample| matches!(sample.scope, SampleScope::Request))
            .expect("a request-scope sample")
    }

    /// The attempt-scope sample at one index.
    fn attempt_sample(&self, request_id: &str, index: usize) -> OutcomeTrainingSample {
        self.samples_for(request_id)
            .into_iter()
            .find(|sample| {
                matches!(sample.scope, SampleScope::Attempt { index: at, .. } if at == index)
            })
            .unwrap_or_else(|| panic!("an attempt-scope sample at {index}"))
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

/// Ingestion is the last step of a stream's terminal transition, so the same
/// polling discipline applies before reading what was collected.
async fn wait_for_samples(harness: &Harness, expected: usize) {
    for _ in 0..200 {
        if harness.state.dataset().len() >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "expected {expected} collected samples, saw {}",
        harness.state.dataset().len()
    );
}

// ------------------------------------------------------------------ gates

/// A served request produces exactly one bounded, validated ingestion, and the
/// vectors in it are the ones the record retained at decision time.
#[tokio::test]
async fn a_served_request_ingests_the_retained_decision_time_vectors() {
    let h = Harness::start_shadowed().await;

    let response = h.ask("standard-class").await;
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "hello from mock");

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Success);
    let served = outcome.served_identity().expect("an answer was delivered");
    let record = h.shadow_record();
    assert_eq!(record.actual.request_id, outcome.request_id);

    // One attempt plus the request itself.
    let samples = h.samples_for(&outcome.request_id);
    assert_eq!(samples.len(), 2);
    assert_eq!(h.state.shadow().fault_count(), 0, "nothing faulted");
    assert_eq!(h.state.dataset().counters().faults, 0, "no dataset fault");
    let counters = h.state.dataset().counters();
    assert_eq!(counters.ingested, 1, "one ingestion for one request");
    assert_eq!(counters.samples, 2);
    assert_eq!(counters.no_decision_time_input, 0);
    assert_eq!(counters.rejected, 0);

    // Every sample's features are byte-for-byte the vector this record retained
    // for that exact candidate. This is the whole provenance claim: the
    // ingested features are the decision-time ones, not a re-derivation.
    for sample in &samples {
        let retained = record
            .input()
            .candidates
            .iter()
            .find(|candidate| {
                candidate.candidate_id == sample.model_id
                    && candidate.provider_id == sample.provider_id
            })
            .unwrap_or_else(|| panic!("the record observed '{}'", sample.model_id));
        assert_eq!(
            sample.features.values, retained.features.values,
            "sample '{}' must carry the retained vector",
            sample.model_id
        );
        assert_eq!(
            sample.features.schema_version,
            retained.features.schema_version
        );
        assert_eq!(sample.request_id, outcome.request_id);
        assert_eq!(sample.outcome_id, outcome.outcome_id);
        assert!(sample.feedback.is_none(), "no user signal was supplied");
    }

    let request = h.request_sample(&outcome.request_id);
    assert!(request.success);
    assert_eq!(request.final_status, FinalStatus::Success);
    assert_eq!(request.model_id, served.model());
    assert_eq!(request.provider_id, served.provider());
    assert_eq!(
        request
            .identity
            .served
            .as_ref()
            .map(|identity| identity.model.as_str()),
        Some(served.model())
    );
    assert!(request.targets.latency_ms.is_some());
    assert!(request.targets.failure_class.is_none());

    h.shutdown().await;
}

/// The ingested features must be the retained ones and not merely *some*
/// features: a second extraction from the same request would have to be
/// distinguishable, and the only defensible distinction is that these vectors
/// are the record's.
#[tokio::test]
async fn the_ingested_vectors_are_the_record_s_own_candidates() {
    let h = Harness::start_shadowed().await;
    assert_eq!(h.ask("standard-class").await.status(), 200);

    let outcome = h.outcome();
    let record = h.shadow_record();
    let samples = h.samples_for(&outcome.request_id);
    let request = h.request_sample(&outcome.request_id);

    // The request sample's vector is the *served* candidate's, not the planned
    // one and not a default vector.
    let served_candidate = record
        .input()
        .candidates
        .iter()
        .find(|candidate| candidate.candidate_id == served_of(&outcome))
        .expect("the served candidate was observed");
    assert_eq!(request.features.values, served_candidate.features.values);
    let planned = outcome.planned_identity().expect("a planned identity");
    if planned.model() != served_of(&outcome) {
        let planned_candidate = record
            .input()
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_id == planned.model())
            .expect("the planned candidate was observed");
        assert_ne!(
            request.features.values, planned_candidate.features.values,
            "a request sample must not borrow the planned candidate's vector"
        );
    }
    assert_ne!(
        request.features.values,
        zroutery_core::ml::RoutingFeatures::default().values,
        "no sample carries a synthesized all-unknown vector"
    );
    assert_eq!(samples.len(), 2, "the ingestion is bounded by the evidence");

    h.shutdown().await;
}

fn served_of(outcome: &Outcome) -> String {
    outcome
        .served_identity()
        .expect("a served identity")
        .model()
        .to_string()
}

/// A failover keeps the failed attempt as its own negative sample, with the
/// failure class it really had.
#[tokio::test]
async fn a_failover_ingests_the_failed_attempt_as_a_negative_sample() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr, true);
    // The preferred candidate always fails, so the request fails over.
    cfg.models[0].upstream_model = "broken-model".into();
    cfg.models[0].priority = 0;
    let h = Harness::start(cfg, mock).await;

    assert_eq!(h.ask("standard-class").await.status(), 200);
    assert_eq!(h.mock.count(), 2, "the failing candidate was tried first");

    let outcome = h.outcome();
    assert_eq!(outcome.attempts.len(), 2);
    assert_ne!(
        outcome.planned_identity().unwrap(),
        outcome.served_identity().unwrap(),
        "this request failed over"
    );

    let samples = h.samples_for(&outcome.request_id);
    assert_eq!(samples.len(), 3, "two attempts plus the request");

    let failed = h.attempt_sample(&outcome.request_id, 0);
    assert!(!failed.success, "a failed attempt is a negative sample");
    assert!(!failed.targets.success);
    assert!(
        failed.targets.failure_class.is_some(),
        "the failure class is retained, not flattened away"
    );
    assert!(failed.targets.latency_ms.is_none());
    assert!(failed.targets.ttft_ms.is_none());
    assert_eq!(
        failed.final_status,
        FinalStatus::Success,
        "the request as a whole still succeeded"
    );

    let answered = h.attempt_sample(&outcome.request_id, 1);
    assert!(answered.success);
    assert!(answered.targets.latency_ms.is_some());
    assert!(answered.targets.failure_class.is_none());

    let request = h.request_sample(&outcome.request_id);
    assert!(request.success, "the request as a whole succeeded");
    assert_eq!(request.targets.fallback_count, 1);

    h.shutdown().await;
}

/// A request that never got an answer is retained as a negative sample with its
/// terminal state and failure class.
#[tokio::test]
async fn a_failed_request_is_ingested_as_a_negative_sample() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr, true);
    cfg.models.truncate(1);
    cfg.models[0].upstream_model = "broken-model".into();
    let h = Harness::start(cfg, mock).await;

    assert_eq!(h.ask("standard-class").await.status(), 500);

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert!(outcome.served_identity().is_none());

    let samples = h.samples_for(&outcome.request_id);
    assert_eq!(samples.len(), 2);
    let request = h.request_sample(&outcome.request_id);
    assert!(
        !request.success,
        "a failed request is never a success label"
    );
    assert_eq!(request.final_status, FinalStatus::Failed);
    let failure_class = outcome
        .failure_class()
        .expect("a failed request has a failure class");
    assert_eq!(
        request.targets.failure_class.as_deref(),
        Some(format!("{failure_class:?}").as_str()),
        "the sample keeps the classification the outcome recorded"
    );
    assert!(request.identity.served.is_none(), "nothing served");
    assert!(request.targets.latency_ms.is_none());
    assert!(request.targets.ttft_ms.is_none());
    assert!(request.terminal_error.is_some());
    assert_eq!(
        request.terminal_error.as_ref().map(|facts| facts.class),
        Some(failure_class)
    );

    let attempt = h.attempt_sample(&outcome.request_id, 0);
    assert!(!attempt.success);
    assert_eq!(
        attempt.targets.failure_class.as_deref(),
        Some(format!("{failure_class:?}").as_str())
    );
    assert_eq!(h.state.dataset().counters().rejected, 0);

    h.shutdown().await;
}

/// An abandoned answer is retained as what it was: interrupted, not served.
#[tokio::test]
async fn an_abandoned_stream_is_ingested_as_an_interrupted_sample() {
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
    wait_for_samples(&h, 2).await;

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Interrupted);
    assert!(outcome.served_identity().is_none());

    let request = h.request_sample(&outcome.request_id);
    assert!(!request.success, "an abandoned answer is not a success");
    assert_eq!(request.final_status, FinalStatus::Interrupted);
    assert_eq!(
        request.targets.failure_class.as_deref(),
        Some("Interrupted")
    );
    assert!(request.identity.served.is_none());
    assert!(request.targets.latency_ms.is_none());
    assert!(request.targets.ttft_ms.is_none());
    // The request was ingested, not skipped: it did have a decision-time input.
    let counters = h.state.dataset().counters();
    assert_eq!(counters.ingested, 1);
    assert_eq!(counters.no_decision_time_input, 0);
    assert_eq!(counters.rejected, 0);

    h.shutdown().await;
}

/// A request refused before any candidate was tried has no decision-time input,
/// so it has no sample — and the store says so instead of staying quiet.
#[tokio::test]
async fn a_budget_refusal_ingests_nothing_and_reports_no_decision_time_input() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr, true);
    // Any served candidate costs more than the whole budget allows.
    for entry in &mut cfg.models {
        entry.pricing = Some(Pricing::new("USD", 1000.0, 1000.0));
    }
    cfg.budgets = vec![Budget::new(
        BudgetScope::Global,
        BudgetPeriod::Day,
        "USD",
        0.01,
    )];
    let h = Harness::start(cfg, mock).await;

    // The first request fits and is ingested.
    assert_eq!(h.ask("standard-class").await.status(), 200);
    let first = h.outcome();
    assert_eq!(h.samples_for(&first.request_id).len(), 2);

    // The next is refused before any candidate was tried.
    let refused = h.ask("standard-class").await;
    assert_eq!(refused.status(), 402);
    assert_eq!(h.mock.count(), 1, "nothing reached the provider");

    let refused_outcome = h
        .all_outcomes()
        .into_iter()
        .find(|outcome| outcome.request_id != first.request_id)
        .expect("the refused request still has an outcome");
    assert!(
        refused_outcome.attempts.is_empty(),
        "no candidate was tried"
    );
    assert!(h
        .samples()
        .iter()
        .all(|sample| sample.request_id != refused_outcome.request_id));

    let counters = h.state.dataset().counters();
    assert_eq!(counters.ingested, 1);
    assert_eq!(
        counters.no_decision_time_input, 1,
        "the refusal is observable"
    );
    assert_eq!(counters.rejected, 0, "it is not a rejected sample");

    h.shutdown().await;
}

/// With shadow off there is no decision-time record, so there are no features
/// and no samples — and the store counts every such request instead of
/// pretending nothing happened.
#[tokio::test]
async fn without_a_retained_record_nothing_is_ingested_and_it_is_counted() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(config_for(addr, false), mock).await;
    assert!(
        !h.state.shadow().enabled(),
        "shadow is off for this harness"
    );

    assert_eq!(h.ask("standard-class").await.status(), 200);
    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert!(outcome.served_identity().is_some());

    assert!(h.state.shadow().store().decisions().is_empty());
    assert!(
        h.samples().is_empty(),
        "no record means no features and no sample"
    );
    assert_eq!(h.state.dataset().len(), 0);
    let counters = h.state.dataset().counters();
    assert_eq!(counters.no_decision_time_input, 1);
    assert_eq!(counters.ingested, 0);
    assert_eq!(counters.samples, 0);
    assert_eq!(counters.rejected, 0);
    assert_eq!(counters.faults, 0);

    h.shutdown().await;
}

/// Collecting samples must not influence anything the client sees: the same
/// request answered before and after ingestion is byte-identical, and no fault
/// anywhere in the dataset path can reach the response.
#[tokio::test]
async fn collecting_samples_influences_no_response() {
    let h = Harness::start_shadowed().await;

    let first = h.ask("standard-class").await;
    assert_eq!(first.status(), 200);
    let first_model = first.headers()["x-zroutery-model"].clone();
    let first_provider = first.headers()["x-zroutery-provider"].clone();
    let first_body: Value = first.json().await.unwrap();
    // One request, one ingestion, and the samples exist before the next ask.
    assert!(h.state.dataset().len() >= 2);

    let second = h.ask("standard-class").await;
    assert_eq!(second.status(), 200);
    let second_model = second.headers()["x-zroutery-model"].clone();
    let second_provider = second.headers()["x-zroutery-provider"].clone();
    let second_body: Value = second.json().await.unwrap();

    assert_eq!(first_model, second_model, "the same model served both");
    assert_eq!(
        first_provider, second_provider,
        "the same provider served both"
    );
    assert_eq!(first_body, second_body, "the answer is unchanged");
    assert_eq!(h.state.shadow().fault_count(), 0);
    assert_eq!(h.state.dataset().counters().faults, 0);
    assert_eq!(h.state.dataset().counters().rejected, 0);
    // Two requests, two ingestions: one per request, never two.
    let counters = h.state.dataset().counters();
    assert_eq!(counters.ingested, 2);
    assert_eq!(counters.samples, 4);
    assert_eq!(h.state.outcomes().len(), 2);

    h.shutdown().await;
}

/// The dataset is a collection, not a learner: nothing in production reads it
/// back, and the only thing that reaches it is the terminal transition.
#[tokio::test]
async fn the_dataset_is_never_read_back_by_production() {
    let source = include_str!("../src/server/pipeline.rs");
    // Reading the store for a decision would be a second `.dataset()` call.
    assert_eq!(source.matches(".dataset()").count(), 1);
    for reader in ["training_slice(", "legacy_training_slice(", "counters()"] {
        assert!(
            !source.contains(reader),
            "the pipeline must not read the dataset ({reader})"
        );
    }
    let state_source = include_str!("../src/server/mod.rs");
    for forbidden in ["train(", "try_train(", "update_all(", "ModelEnsemble"] {
        assert!(
            !state_source.contains(forbidden),
            "AppState must not reach a learning seam ({forbidden})"
        );
    }
    let routing = include_str!("../src/router.rs");
    assert!(
        !routing.contains("dataset"),
        "routing must not know the dataset exists"
    );
}

/// The store production uses is bounded by both count and age, and reports both.
#[tokio::test]
async fn the_production_store_is_bounded_and_reports_its_bounds() {
    let h = Harness::start_shadowed().await;
    let store = h.state.dataset();
    assert!(store.max_samples() > 0, "the count bound is set");
    assert!(store.max_age_secs() > 0, "the age bound is set");
    assert!(
        store.max_samples() <= 10_000,
        "the count bound is the documented production bound"
    );

    assert_eq!(h.ask("standard-class").await.status(), 200);
    assert_eq!(store.len(), 2);
    assert_eq!(store.training_slice().len(), 2);
    // The legacy shape is a projection, not a second storage path.
    let legacy = store.legacy_training_slice();
    assert_eq!(legacy.len(), 2);
    assert_eq!(legacy[0].outcome_id, store.training_slice()[0].outcome_id);
    assert_eq!(store.len(), 2, "projecting allocated nothing new");
    assert_eq!(store.evict_expired(), 0, "a fresh sample is not expired");

    h.shutdown().await;
}
