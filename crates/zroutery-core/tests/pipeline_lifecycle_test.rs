//! Production lifecycle gates: one terminal transition per request.
//!
//! These tests drive a real Zroutery server in front of a mock provider and
//! read what the request lifecycle actually recorded: the activity record, the
//! terminal `Outcome`, the spend ledger and the router's health view. The claims
//! under test are the ones a request makes about itself — planned vs
//! last-attempted vs served identity, one classified failure path, exactly one
//! terminal transition, and never a success for an answer the client abandoned.

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
use zroutery_core::failure::FailureClass;
use zroutery_core::outcome::{FinalStatus, Outcome};
use zroutery_core::server::{AppState, ServerHandle};
use zroutery_core::stats::RequestRecord;

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

/// An upstream stream that delivers part of an answer and then stays open.
///
/// This is what a slow answer looks like to the proxy, and it is the only way
/// to make a client disconnect land *mid-stream*: bytes have gone to the client
/// and the client is gone before the answer is finished.
///
/// `id` is the chunk id the upstream reports. An empty one matters for the
/// Responses API: the proxy then substitutes the response id it registered as
/// in-flight, which is the id a cancellation has to name.
fn held_open_stream(id: &str) -> axum::body::Body {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let first = id.to_string();
    let second = id.to_string();
    let third = id.to_string();
    tokio::spawn(async move {
        let _ = tx.send(openai_chunk(
            &first,
            json!({"role": "assistant", "content": ""}),
            Value::Null,
        ));
        let _ = tx.send(openai_chunk(&second, json!({"content": "hel"}), Value::Null));
        let _ = tx.send(openai_chunk(&third, json!({"content": "lo"}), Value::Null));
        // The upstream never ends: the answer is still being written when the
        // client decides it has had enough.
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
    if model.starts_with("limited") {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": {"message": "rate limited", "type": "rate_limit_error"}})),
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
        if model.starts_with("cancel") {
            // An empty upstream id makes the proxy publish the response id it
            // registered as in-flight, which is what a cancel request names.
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
        sse.push_str(&openai_chunk("chatcmpl-mock", json!({"content": "hi"}), Value::Null));
        sse.push_str(&openai_chunk("chatcmpl-mock", json!({}), json!("stop")));
        sse.push_str(&format!(
            "data: {}\n\n",
            json!({"id": "chatcmpl-mock", "object": "chat.completion.chunk", "model": "mock",
                   "choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 2}})
        ));
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

const TOKEN: &str = "zr-lifecycle-token";

fn provider(id: &str, name: &str, mock: SocketAddr) -> ProviderConfig {
    let mut provider = ProviderConfig::new(id, name, ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{mock}");
    provider.key_ref = format!("provider:{id}");
    provider.timeout_secs = 10;
    provider
}

fn model(provider: &str, upstream: &str, priority: i32, tier: ModelTier) -> ModelEntry {
    let mut entry = ModelEntry::for_upstream(provider, upstream, Some(tier)).with_priority(priority);
    entry.pricing = Some(Pricing::new("USD", 3.0, 15.0));
    entry
}

/// Two healthy candidates in the standard tier, plus the awkward models the
/// lifecycle tests need: one that never finishes streaming and one that always
/// fails. Those live in the fast tier so they never join a standard-tier plan.
fn config_for(mock: SocketAddr) -> AppConfig {
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
        model("alpha", "hold-model", 10, ModelTier::Fast),
        model("beta", "hold-model", 10, ModelTier::Fast),
        model("beta", "broken-model", 10, ModelTier::Fast),
        model("alpha", "cancel-model", 10, ModelTier::Fast),
    ];
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
    async fn start_with_secrets(
        cfg: AppConfig,
        mock: Mock,
        secrets: Arc<MemorySecretStore>,
    ) -> Harness {
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

    async fn start(cfg: AppConfig, mock: Mock) -> Harness {
        Harness::start_with_secrets(
            cfg,
            mock,
            Arc::new(
                MemorySecretStore::new()
                    .with("provider:alpha", "sk-alpha")
                    .with("provider:beta", "sk-beta"),
            ),
        )
        .await
    }

    async fn new() -> Harness {
        let (addr, mock) = start_mock().await;
        Harness::start(config_for(addr), mock).await
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}{path}", self.base))
            .header("x-api-key", TOKEN)
    }

    /// A buffered main-traffic request.
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

    /// A streaming main-traffic request.
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

    fn records(&self) -> Vec<RequestRecord> {
        self.state.stats().recent(100)
    }

    fn outcomes(&self) -> Vec<Outcome> {
        self.state.outcomes().recent(100)
    }

    /// The one outcome a single request produced.
    fn outcome(&self) -> Outcome {
        let outcomes = self.outcomes();
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

    fn record(&self) -> RequestRecord {
        let records = self.records();
        assert_eq!(records.len(), 1, "expected exactly one activity record");
        records.into_iter().next().unwrap()
    }

    /// Total spend booked against the global budget scope today.
    fn spent_today(&self) -> f64 {
        self.state
            .ledger()
            .totals_for(&BudgetScope::Global, chrono::Local::now())
            .into_iter()
            .filter(|(period, _)| *period == BudgetPeriod::Day)
            .map(|(_, cost)| cost.amount)
            .sum()
    }

    async fn shutdown(mut self) {
        if let Some(server) = self.server.take() {
            server.stop().await;
        }
    }
}

/// Wait until the lifecycle has recorded a request's terminal transition.
///
/// A client disconnect is noticed by the server when the connection goes away,
/// which is asynchronous by nature, so the assertions poll instead of guessing
/// how long that takes.
async fn wait_for_outcomes(harness: &Harness, expected: usize) {
    for _ in 0..200 {
        if harness.state.outcomes().len() == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "expected {expected} terminal outcomes, saw {} (records: {})",
        harness.state.outcomes().len(),
        harness.state.stats().recent(10).len()
    );
}

// ------------------------------------------------------------------ gate 4

/// REG-007: a client that walks away mid-answer is not a success.
///
/// The abandoned answer used to be finalized with no error at all, so Activity,
/// the stats summary and the dashboard all reported a completed request. Here
/// the client reads part of the stream and disconnects, and the terminal state
/// has to say what actually happened.
#[tokio::test]
async fn a_client_disconnect_is_never_recorded_as_success() {
    let h = Harness::new().await;

    let mut response = h.ask_streaming("alpha-hold-model").await;
    assert_eq!(response.status(), 200);
    // Part of the answer arrives, so the drop lands mid-answer rather than
    // before the first byte.
    let first = response.chunk().await.unwrap().expect("some output");
    assert!(!first.is_empty(), "the mock produced answer bytes");
    drop(response);

    wait_for_outcomes(&h, 1).await;

    let record = h.record();
    assert!(!record.ok, "an abandoned answer is not a success");
    assert_eq!(record.status, 499, "the client closed the request");
    let error = record.error.as_deref().unwrap_or_default();
    assert!(
        error.contains("client disconnected"),
        "activity explains the terminal state: {error}"
    );

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Interrupted);
    assert!(!outcome.is_terminal_success());
    assert_eq!(outcome.failure_class(), Some(FailureClass::Interrupted));
    // The candidate that was streaming is the last attempt, and nothing served.
    let identity = outcome
        .last_attempted_identity()
        .expect("a stream that started has a last attempt");
    assert_eq!(identity.model(), "alpha-hold-model");
    assert_eq!(identity.provider(), "alpha");
    assert_eq!(outcome.planned_identity(), Some(identity.clone()));
    assert!(outcome.served_identity().is_none(), "no answer was delivered");
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    h.shutdown().await;
}

/// The same drop must not poison the provider: the upstream really did answer,
/// and a client leaving is not evidence that the model is unhealthy.
#[tokio::test]
async fn a_client_disconnect_does_not_count_against_provider_health() {
    let h = Harness::new().await;

    let mut response = h.ask_streaming("alpha-hold-model").await;
    let _ = response.chunk().await;
    drop(response);
    wait_for_outcomes(&h, 1).await;

    let health = h.state.router().health_snapshot();
    let alpha = health
        .iter()
        .find(|model| model.model_id == "alpha-hold-model")
        .expect("the handshake was reported");
    assert_eq!(alpha.total_success, 1, "one handshake, reported once");
    assert_eq!(alpha.total_failure, 0, "a client leaving is not a failure");
    assert!(!h.state.router().is_cooling("alpha-hold-model"));
    let observation = h
        .state
        .router()
        .observations()
        .get("alpha-hold-model", "alpha");
    assert_eq!(observation.health.total_failures, 0);
    assert_eq!(observation.health.total_requests, 1);
    // The terminal state is still recorded as a classified fact, once.
    let breakdown = h
        .state
        .router()
        .stats_store
        .get("alpha-hold-model", "alpha");
    assert_eq!(breakdown.failures.count(FailureClass::Interrupted), 1);

    h.shutdown().await;
}

/// An explicit cancellation is its own terminal state, and it is not a success.
///
/// The Responses API cancel endpoint signals the in-flight stream, which used
/// to finalize the request with no error at all. Here the client cancels a
/// stream that is still being written, and the record has to say the request was
/// cancelled rather than completed.
#[tokio::test]
async fn an_explicit_cancellation_is_never_recorded_as_success() {
    let h = Harness::new().await;

    let mut response = h
        .post("/v1/responses")
        .json(&json!({"model": "alpha-cancel-model", "input": "hi", "stream": true}))
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
    assert!(
        response_id.starts_with("resp-"),
        "the client learns the in-flight id: {response_id}"
    );

    let cancelled = h
        .post(&format!("/v1/responses/{response_id}/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(cancelled.status(), 200);
    let _ = cancelled.text().await;

    wait_for_outcomes(&h, 1).await;

    let record = h.record();
    assert!(!record.ok, "a cancelled request is not a success");
    assert_eq!(record.status, 499);
    let error = record.error.as_deref().unwrap_or_default();
    assert!(error.contains("cancelled"), "activity says why: {error}");

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Cancelled);
    assert_eq!(outcome.failure_class(), Some(FailureClass::ClientCancelled));
    assert!(outcome.served_identity().is_none());
    assert_eq!(outcome.response_id.as_deref(), Some(response_id.as_str()));
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());
    assert_eq!(
        h.state.outcomes().len(),
        1,
        "the cancelled request is recorded once"
    );

    // A cancellation is the client's doing, so provider health is untouched.
    let health = h.state.router().health_snapshot();
    let alpha = health
        .iter()
        .find(|model| model.model_id == "alpha-cancel-model")
        .expect("the handshake was reported");
    assert_eq!(alpha.total_success, 1);
    assert_eq!(alpha.total_failure, 0);

    h.shutdown().await;
}

// ------------------------------------------------------------------ gates 3, 5, 6

/// Exactly one terminal transition per request, across every kind of ending.
///
/// A served buffer, a served stream, a stream that fails at the handshake and a
/// stream the client abandons in one server: four requests, four activity
/// records, four outcomes correlated by request id, and the spend booked once
/// each.
#[tokio::test]
async fn every_request_reaches_exactly_one_terminal_transition() {
    let h = Harness::new().await;

    // 1. a served buffered request
    assert_eq!(h.ask("beta-good-model").await.status(), 200);
    // 2. a served stream, drained to its end
    let served = h.ask_streaming("beta-good-model").await;
    let _ = served.text().await.unwrap();
    // 3. a stream whose handshake fails
    assert_eq!(h.ask_streaming("beta-broken-model").await.status(), 500);
    // 4. a stream the client abandons
    let mut dropped = h.ask_streaming("beta-hold-model").await;
    let _ = dropped.chunk().await;
    drop(dropped);

    wait_for_outcomes(&h, 4).await;

    let records = h.records();
    let outcomes = h.outcomes();
    assert_eq!(records.len(), 4, "one activity record per request");
    assert_eq!(outcomes.len(), 4, "one outcome per request");

    // Records are newest first and so are outcomes, so the two line up index
    // for index: every recorded request has exactly one outcome.
    let record_ids: Vec<&str> = records.iter().map(|record| record.id.as_str()).collect();
    let outcome_ids: Vec<&str> = outcomes
        .iter()
        .map(|outcome| outcome.request_id.as_str())
        .collect();
    assert_eq!(record_ids, outcome_ids, "one outcome per recorded request");
    let mut unique = outcome_ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 4, "no request was recorded twice");

    for outcome in &outcomes {
        assert!(
            outcome.validate().is_ok(),
            "every constructed outcome satisfies the schema: {:?}",
            outcome.validate()
        );
    }

    // Two succeeded, one failed at the handshake, one was abandoned.
    let mut statuses: Vec<FinalStatus> = outcomes.iter().map(|o| o.final_status).collect();
    statuses.sort_by_key(|status| format!("{status:?}"));
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == FinalStatus::Success)
            .count(),
        2
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == FinalStatus::Failed)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == FinalStatus::Interrupted)
            .count(),
        1
    );
    assert!(outcomes
        .iter()
        .all(|outcome| !outcome.request_id.is_empty()));

    // The ledger and the records agree: spend was booked once per request.
    let expected: f64 = records
        .iter()
        .filter_map(|record| record.cost.as_ref())
        .map(|cost| cost.amount)
        .sum();
    assert!(
        (h.spent_today() - expected).abs() < 1e-12,
        "the ledger and the records agree: {} vs {expected}",
        h.spent_today()
    );

    h.shutdown().await;
}

/// The served identity is the identity that actually delivered the answer, and
/// the planned identity is the router's own choice — never the same field.
#[tokio::test]
async fn planned_last_attempted_and_served_are_correlated() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // The preferred standard candidate always fails, so the planned identity
    // and the identity that served the answer differ.
    cfg.models[0].upstream_model = "broken-model".into();
    cfg.models[0].priority = 0;
    let h = Harness::start(cfg, mock).await;

    assert_eq!(h.ask("standard-class").await.status(), 200);
    assert_eq!(h.mock.count(), 2, "the failing candidate was tried first");

    let outcome = h.outcome();
    let planned = outcome
        .planned_identity()
        .expect("a policy-routed request has a planned identity");
    let last = outcome
        .last_attempted_identity()
        .expect("the request made an attempt");
    let served = outcome
        .served_identity()
        .expect("the request was served");

    // Planned comes from the router's decision, not from what happened later.
    assert_eq!(planned.model(), "alpha-broken-model");
    assert_eq!(planned.provider(), "alpha");
    assert_eq!(last.model(), "beta-good-model");
    assert_eq!(served, last, "the last attempt is the one that served");
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert_eq!(outcome.attempts.len(), 2);
    assert_eq!(outcome.fallback_count, 1);
    assert_eq!(outcome.initial_model, planned.model());
    assert_eq!(outcome.final_model, served.model());
    assert!(outcome.is_terminal_success());
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    // The failed attempt carries the canonical class, not a re-derived one.
    let first = &outcome.attempts[0];
    assert!(!first.success);
    assert_eq!(first.failure_class, Some(FailureClass::Unknown));
    assert_eq!(first.http_status, Some(500));
    assert!(!first.rectified);
    assert!(outcome.attempts[1].is_terminal_success());

    // The outcome and the record correlate on the decision the router made.
    let record = h.record();
    let decision = record
        .routing_decision
        .as_ref()
        .expect("a policy-routed record carries its decision");
    assert_eq!(
        outcome.decision_id.as_deref(),
        Some(decision.decision_id.as_str())
    );
    assert_eq!(record.attempts, 2);

    h.shutdown().await;
}

/// A request that never reached a candidate is still a request: one record, one
/// outcome, and no provider health touched at all.
#[tokio::test]
async fn a_budget_denial_is_a_terminal_outcome_with_no_candidate() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.budgets = vec![
        Budget::new(BudgetScope::Global, BudgetPeriod::Day, "USD", 0.0).rejecting()
    ];
    let h = Harness::start(cfg, mock).await;

    let response = h.ask("alpha-good-model").await;
    assert_eq!(response.status(), 402, "the budget stopped it");

    let record = h.record();
    assert!(!record.ok);
    assert_eq!(record.status, 402);
    assert_eq!(record.attempts, 0);

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(outcome.failure_class(), Some(FailureClass::OverBudget));
    assert!(outcome.attempts.is_empty());
    assert!(outcome.planned_identity().is_none(), "nothing was planned");
    assert!(outcome.last_attempted_identity().is_none());
    assert!(outcome.served_identity().is_none());
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());
    assert_eq!(h.mock.count(), 0, "no provider was contacted");
    assert!(
        h.state.router().health_snapshot().is_empty(),
        "a local refusal cannot poison provider health"
    );

    h.shutdown().await;
}

// ------------------------------------------------------------------ gate 1 (last attempted)

/// A candidate the loop could not even prepare is still the final attempt it
/// made: last-attempted means the last thing the request tried, not the last
/// thing that reached a provider. A missing key is a local problem, so it is
/// recorded as a classified fact without becoming a health failure.
#[tokio::test]
async fn an_unusable_candidate_is_the_last_attempt_without_poisoning_health() {
    let (addr, mock) = start_mock().await;
    let cfg = config_for(addr);
    // `provider:alpha` is deliberately absent from the secret store.
    let h = Harness::start_with_secrets(
        cfg,
        mock,
        Arc::new(MemorySecretStore::new().with("provider:beta", "sk-beta")),
    )
    .await;

    // A direct resolution: the only candidate cannot be prepared, so the
    // request ends on that candidate.
    assert_eq!(h.ask("alpha-good-model").await.status(), 412);
    assert_eq!(h.mock.count(), 0, "nothing was sent");

    let record = h.record();
    assert!(!record.ok);
    assert_eq!(record.status, 412);
    assert_eq!(record.attempts, 1);

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(outcome.attempts.len(), 1);
    let last = outcome
        .last_attempted_identity()
        .expect("the loop tried a candidate");
    assert_eq!(last.model(), "alpha-good-model");
    assert_eq!(last.provider(), "alpha");
    assert_eq!(outcome.planned_identity(), Some(last.clone()));
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::MissingApiKey),
        "the unusable candidate is recorded with the canonical class"
    );
    assert_eq!(outcome.failure_class(), Some(FailureClass::MissingApiKey));
    assert!(outcome.served_identity().is_none());
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    assert!(
        h.state.router().health_snapshot().is_empty(),
        "a missing key is not provider health"
    );
    let breakdown = h
        .state
        .router()
        .stats_store
        .get("alpha-good-model", "alpha");
    assert_eq!(
        breakdown.failures.count(FailureClass::MissingApiKey),
        1,
        "the class is recorded exactly once"
    );

    h.shutdown().await;
}

// ------------------------------------------------------------------ gate 2

/// One classified failure path: the pipeline asks the canonical classifier and
/// the accepted router adapters, and never classifies a status or a message of
/// its own.
#[test]
fn the_pipeline_has_no_second_failure_classifier() {
    let source = include_str!("../src/server/pipeline.rs");
    for forbidden in [
        "from_status_with_body",
        "FailureClass::from_status",
        "ClassifiedFailure::from_status",
        "from_error_message",
        ".is_retryable()",
        "report_failure(",
    ] {
        assert!(
            !source.contains(forbidden),
            "pipeline.rs must not classify failures itself: {forbidden}"
        );
    }
    // The canonical entry point and the router adapters are what it does use.
    assert!(source.contains(".classified()"));
    assert!(source.contains("record_classified_attempt("));
    assert!(source.contains("should_fallback_failure("));
    assert!(
        !source.contains("Some(class)"),
        "the pipeline never assembles a class of its own to hand the router"
    );

    // Gate 6, structurally: the activity record and the charge are written in
    // exactly one place, so no terminal path can write either of them twice.
    assert_eq!(
        source.matches("stats.record(").count(),
        1,
        "the activity record is written once, in the terminal transition"
    );
    assert_eq!(
        source.matches(".charge(").count(),
        1,
        "the ledger is charged once, in the terminal transition"
    );
}

/// A rate limit is classified once, degrades observations, and must not open
/// the circuit: the impact table's answer, reached through one path.
#[tokio::test]
async fn a_rate_limit_is_classified_once_without_opening_the_circuit() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models = vec![model("alpha", "limited-model", 0, ModelTier::Standard)];
    let h = Harness::start(cfg, mock).await;

    assert_eq!(h.ask("alpha-limited-model").await.status(), 429);

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(outcome.failure_class(), Some(FailureClass::RateLimit));
    assert_eq!(outcome.attempts.len(), 1);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::RateLimit)
    );
    assert!(outcome.served_identity().is_none());
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    let breakdown = h
        .state
        .router()
        .stats_store
        .get("alpha-limited-model", "alpha");
    assert_eq!(breakdown.failures.count(FailureClass::RateLimit), 1);
    assert_eq!(
        breakdown.failures.count(FailureClass::ProviderUnavailable),
        0,
        "the class is not re-derived from the status"
    );
    let health = h.state.router().health_snapshot();
    assert_eq!(health[0].total_failure, 1, "counted once");
    assert!(
        !h.state.router().is_cooling("alpha-limited-model"),
        "a quota is not a dead provider"
    );

    h.shutdown().await;
}

// ------------------------------------------------------------------ gate 3 (streaming)

/// A stream that ends normally is one served outcome, charged once, with the
/// handshake's health report not repeated at the end.
#[tokio::test]
async fn a_completed_stream_is_one_served_outcome() {
    let h = Harness::new().await;

    let response = h.ask_streaming("alpha-good-model").await;
    let wire = response.text().await.unwrap();
    assert!(
        wire.contains("\"text\":\"hi\""),
        "the client saw the answer: {wire}"
    );

    let record = h.record();
    assert!(record.ok);
    assert_eq!(record.status, 200);
    assert!(record.stream);
    assert!(record.cost.is_some(), "a stream's spend is still recorded");

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert!(outcome.streaming);
    assert_eq!(outcome.dialect, "anthropic");
    let served = outcome
        .served_identity()
        .expect("an answer was delivered");
    assert_eq!(served.model(), "alpha-good-model");
    assert_eq!(outcome.last_attempted_identity(), Some(served.clone()));
    assert_eq!(outcome.planned_identity(), Some(served));
    assert_eq!(outcome.usage.map(|usage| usage.output_tokens), Some(2));
    assert_eq!(outcome.actual_cost, record.cost.as_ref().map(|cost| cost.amount));
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    let health = h.state.router().health_snapshot();
    let alpha = health
        .iter()
        .find(|model| model.model_id == "alpha-good-model")
        .unwrap();
    assert_eq!(
        alpha.total_success, 1,
        "the handshake is reported once, not again at the end"
    );
    assert_eq!(alpha.total_failure, 0);

    h.shutdown().await;
}

/// A stream that fails after the handshake is a failed request, not a served
/// one, and the failure is classified once through the same path.
#[tokio::test]
async fn a_failed_stream_handshake_is_a_failed_outcome() {
    let h = Harness::new().await;

    let response = h.ask_streaming("beta-broken-model").await;
    assert_eq!(response.status(), 500);
    let _ = response.text().await.unwrap();

    let record = h.record();
    assert!(!record.ok);
    assert_eq!(record.status, 500);

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert_eq!(outcome.attempts.len(), 1);
    assert_eq!(outcome.attempts[0].failure_class, Some(FailureClass::Unknown));
    assert!(outcome.served_identity().is_none());
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    let breakdown = h
        .state
        .router()
        .stats_store
        .get("beta-broken-model", "beta");
    assert_eq!(breakdown.failures.count(FailureClass::Unknown), 1);

    h.shutdown().await;
}

/// The drop path may not resolve to a success in the source either.
#[test]
fn dropping_a_stream_is_not_a_success_in_the_source_either() {
    let source = include_str!("../src/server/pipeline.rs");
    let (_, after) = source
        .split_once("impl Drop for SseState")
        .expect("the stream state still has a drop path");
    let (drop_impl, _) = after
        .split_once("\n}")
        .expect("the drop impl is closed");
    assert!(
        drop_impl.contains("ClientDisconnected"),
        "a dropped stream finalizes as a client disconnect"
    );
    assert!(
        !drop_impl.contains("TerminalKind::Served"),
        "a dropped stream is never finalized as served"
    );
    assert!(
        !drop_impl.contains("finalize(None)"),
        "a dropped stream is never finalized with no terminal state"
    );
}
