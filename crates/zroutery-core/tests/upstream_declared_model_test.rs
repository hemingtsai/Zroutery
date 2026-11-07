//! The three model identities a request touches stay separate facts.
//!
//! The client's entry (`requested_model`, the stable virtual/exposed id), the
//! configured plan (`ExecutionSnapshot::upstream_model`), and what the
//! upstream's *structured response* declared it ran are three different
//! answers, and collapsing any two of them is how an activity log starts
//! lying. These tests drive a real Zroutery server in front of a mock relay
//! and read what the lifecycle actually persisted: the activity record's
//! `upstream_declared`, the durable per-attempt facts, and the wire the client
//! received — which must keep carrying the stable exposed id exactly as
//! before.
//!
//! The declared value is a provider's claim about its own routing. Nothing
//! here treats it as a verified backend, and every "missing" case asserts the
//! honest absence instead of a back-fill from the requested model.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use serde_json::{json, Value};
use zroutery_core::accounting::AttemptFact;
use zroutery_core::billing::Pricing;
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::ir::{DeclaredIdentity, Dialect, UpstreamModelDeclaration};
use zroutery_core::runtime_store::RuntimeStore;
use zroutery_core::server::{AppState, ServerHandle};
use zroutery_core::stats::RequestRecord;

// ------------------------------------------------------------------ mock relay

#[derive(Clone, Default)]
struct Mock {
    /// Every upstream chat body the mock received, in order.
    bodies: Arc<Mutex<Vec<Value>>>,
}

fn openai_chunk(model: &str, delta: Value, finish: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"id": "chatcmpl-mock", "object": "chat.completion.chunk", "created": 1,
               "model": model, "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    )
}

async fn mock_chat(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    let model = body["model"].as_str().unwrap_or_default().to_string();
    mock.bodies.lock().unwrap().push(body.clone());

    if body["stream"].as_bool().unwrap_or(false) {
        if model == "contradicting-model" {
            // Two frames name two different models: the record must keep both
            // as a conflict, whichever arrives last.
            let sse = format!(
                "{}{}{}data: [DONE]\n\n",
                openai_chunk(
                    "relay-a",
                    json!({"role": "assistant", "content": ""}),
                    Value::Null
                ),
                openai_chunk("relay-b", json!({"content": "hi"}), Value::Null),
                openai_chunk("relay-b", json!({}), json!("stop")),
            );
            return sse_response(sse);
        }
        if model == "auto-stream-model" {
            // A relay that answers an automatic-selection request by echoing
            // the placeholder back. `openrouter/auto` is a routing policy,
            // not a model name.
            let sse = format!(
                "{}{}{}data: [DONE]\n\n",
                openai_chunk(
                    "openrouter/auto",
                    json!({"role": "assistant", "content": ""}),
                    Value::Null
                ),
                openai_chunk("openrouter/auto", json!({"content": "hi"}), Value::Null),
                openai_chunk("openrouter/auto", json!({}), json!("stop")),
            );
            return sse_response(sse);
        }
        if model == "silent-stream-model" {
            // No chunk names a model at all. Nothing may be back-filled.
            let sse = format!(
                "{}{}{}data: [DONE]\n\n",
                openai_chunk("", json!({"role": "assistant", "content": ""}), Value::Null),
                openai_chunk("", json!({"content": "hi"}), Value::Null),
                openai_chunk("", json!({}), json!("stop")),
            );
            return sse_response(sse);
        }
        if model == "stream-then-break-model" {
            // The handshake names a model, part of the answer streams, and
            // then the upstream reports an error frame. The failed attempt
            // keeps what its frames declared before it broke.
            let sse = format!(
                "{}{}data: {}\n\n",
                openai_chunk(
                    "relay-broken",
                    json!({"role": "assistant", "content": ""}),
                    Value::Null
                ),
                openai_chunk("relay-broken", json!({"content": "partial"}), Value::Null),
                json!({"error": {"message": "malformed output", "type": "server_error"}})
            );
            return sse_response(sse);
        }
        let sse = format!(
            "{}{}{}data: {}\n\ndata: [DONE]\n\n",
            openai_chunk(
                &model,
                json!({"role": "assistant", "content": ""}),
                Value::Null
            ),
            openai_chunk(&model, json!({"content": "hi"}), Value::Null),
            openai_chunk(&model, json!({}), json!("stop")),
            json!({"id": "chatcmpl-mock", "object": "chat.completion.chunk",
                   "model": model, "choices": [],
                   "usage": {"prompt_tokens": 5, "completion_tokens": 2}})
        );
        return sse_response(sse);
    }

    if model == "broken-model" {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "upstream exploded", "type": "server_error"}})),
        )
            .into_response();
    }
    if model == "billed-failure" {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "model":"failed-backend","modelVersion":"v7",
                "usage":{"prompt_tokens":12,"completion_tokens":4},
                "error":{"type":"server_error","message":"fixture refusal"}
            })),
        )
            .into_response();
    }
    if model == "nameless-model" {
        // A body that carries no `model` member at all: the honest record is
        // "declared nothing", never the configured model.
        return Json(json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "created": 1,
            "choices": [{"index": 0,
                         "message": {"role": "assistant", "content": "hello"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 7}
        }))
        .into_response();
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

fn sse_response(sse: String) -> Response {
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from(sse))
        .unwrap()
}

async fn start_mock() -> (SocketAddr, Mock) {
    let mock = Mock::default();
    let app = axum::Router::new()
        .route("/v1/chat/completions", post(mock_chat))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, mock)
}

// ------------------------------------------------------------------ harness

const TOKEN: &str = "zr-declared-token";

fn provider(mock: SocketAddr) -> ProviderConfig {
    let mut provider = ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{mock}");
    provider.key_ref = "provider:relay".into();
    provider.timeout_secs = 10;
    provider
}

fn model(upstream: &str, tier: ModelTier) -> ModelEntry {
    let mut entry = ModelEntry::for_upstream("relay", upstream, Some(tier));
    entry.pricing = Some(Pricing::new("USD", 3.0, 15.0));
    entry
}

struct Harness {
    base: String,
    server: Option<ServerHandle>,
    state: Arc<AppState>,
    store: Arc<RuntimeStore>,
    client: reqwest::Client,
    #[allow(dead_code)]
    mock: Mock,
    _directory: tempfile::TempDir,
}

impl Harness {
    /// One server with durable accounting attached, so the per-attempt
    /// declaration facts can be read back from the settlement store.
    async fn start(models: &[ModelEntry], mock: Mock, addr: SocketAddr) -> Harness {
        let mut cfg = AppConfig::default();
        cfg.routing.rectifier.codex_auto = false;
        cfg.server.host = "127.0.0.1".into();
        cfg.server.port = 0;
        cfg.server.auth_token = TOKEN.into();
        cfg.providers = vec![provider(addr)];
        cfg.models = models.to_vec();

        let state = Arc::new(AppState::new(
            cfg,
            Arc::new(MemorySecretStore::new().with("provider:relay", "sk-relay")),
        ));
        let directory = tempfile::tempdir().unwrap();
        let store =
            Arc::new(RuntimeStore::open(&directory.path().join("runtime.sqlite3")).unwrap());
        state
            .attach_accounting(
                Arc::clone(&store),
                &zroutery_core::budget::Ledger::new(),
                "test",
            )
            .unwrap();
        let server = ServerHandle::start(Arc::clone(&state)).await.unwrap();
        Harness {
            base: format!("http://{}", server.addr),
            server: Some(server),
            state,
            store,
            client: reqwest::Client::builder()
                .pool_max_idle_per_host(0)
                .build()
                .unwrap(),
            mock,
            _directory: directory,
        }
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}{path}", self.base))
            .header("x-api-key", TOKEN)
    }

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

    fn record(&self) -> RequestRecord {
        let records = self.state.stats().recent(10);
        assert_eq!(records.len(), 1, "expected exactly one activity record");
        records.into_iter().next().unwrap()
    }

    /// The durable settlement of the one recorded request.
    fn settlement(&self) -> zroutery_core::accounting::SettlementFact {
        let record = self.record();
        self.store
            .settlement(&record.id)
            .unwrap()
            .expect("the request settled durably")
    }

    async fn wait_for_outcomes(&self, expected: usize) {
        for _ in 0..200 {
            if self.state.outcomes().len() == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("expected {expected} terminal outcomes");
    }

    async fn shutdown(mut self) {
        if let Some(server) = self.server.take() {
            server.stop().await;
        }
    }
}

// ------------------------------------------------------------------ tests

/// A served buffered request keeps three separate identities: the client's
/// entry, the configured plan, and the upstream's own declaration — while the
/// wire keeps the stable exposed id exactly as before.
#[tokio::test]
async fn a_served_buffered_request_keeps_three_separate_identities() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(&[model("declared-good", ModelTier::Standard)], mock, addr).await;

    let response = h.ask("standard-class").await;
    assert_eq!(response.status(), 200);
    let wire: Value = response.json().await.unwrap();
    let record = h.record();

    assert_eq!(
        record.requested_model, "standard-class",
        "the client's entry stays the stable virtual id it named"
    );
    let resolved = record.resolved_model.as_deref().unwrap();
    assert!(
        resolved.starts_with("relay-") && resolved.contains("declared-good"),
        "the routing identity stays the stable exposed id: {resolved}"
    );
    assert_eq!(
        wire["model"].as_str(),
        Some(resolved),
        "the wire keeps the stable exposed id; the declaration never leaks to the client"
    );

    let declared = record.upstream_declared.as_ref().expect("captured");
    assert_eq!(
        declared.model,
        DeclaredIdentity::Declared("declared-good".into()),
        "the upstream's own structured claim, verbatim"
    );
    assert_eq!(declared.model_version, DeclaredIdentity::Missing);

    // And the per-attempt fact carries the same declaration durably.
    let settlement = h.settlement();
    assert_eq!(settlement.attempts.len(), 1);
    let fact_declared = settlement.attempts[0]
        .declared
        .as_ref()
        .expect("captured this build");
    assert_eq!(fact_declared.model, declared.model);
    assert_eq!(
        settlement.attempts[0].execution.upstream_model, "declared-good",
        "the configured plan model is the attempt's own fact"
    );
    assert_eq!(
        settlement.attempts[0]
            .execution
            .upstream_protocol
            .as_deref(),
        Some("openai_chat")
    );

    h.shutdown().await;
}

/// A stream declares in its frames; the record keeps the name the frames
/// carried, and the wire still reports the exposed id.
#[tokio::test]
async fn a_stream_records_what_its_frames_declared() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(&[model("stream-good", ModelTier::Fast)], mock, addr).await;

    let response = h.ask_streaming("fast-class").await;
    assert_eq!(response.status(), 200);
    let wire = response.text().await.unwrap();
    let record = h.record();

    let declared = record.upstream_declared.as_ref().expect("captured");
    assert_eq!(
        declared.model,
        DeclaredIdentity::Declared("stream-good".into())
    );
    assert!(
        wire.contains("fast-class") || wire.contains("relay-stream-good"),
        "the wire reports a Zroutery id, never a bare relay name: {wire}"
    );

    h.shutdown().await;
}

/// A stream whose frames disagree is a conflict with every value kept — the
/// last writer does not win, and neither does the first.
#[tokio::test]
async fn a_stream_that_contradicts_itself_is_a_conflict() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(&[model("contradicting-model", ModelTier::Fast)], mock, addr).await;

    let response = h.ask_streaming("fast-class").await;
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    h.wait_for_outcomes(1).await;

    let declared = h.record().upstream_declared.expect("captured");
    assert_eq!(
        declared.model,
        DeclaredIdentity::Conflicting(vec!["relay-a".into(), "relay-b".into()]),
        "both frames' claims survive, in first-seen order"
    );
    assert!(declared.model.is_uncertain());

    h.shutdown().await;
}

/// `openrouter/auto` — like `auto` itself — names a routing policy, not a
/// model. The value is kept verbatim and marked uncertain; it is never
/// promoted to a specific model name.
#[tokio::test]
async fn an_auto_placeholder_is_not_a_model_name() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(&[model("auto-stream-model", ModelTier::Fast)], mock, addr).await;

    let response = h.ask_streaming("fast-class").await;
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    h.wait_for_outcomes(1).await;

    let declared = h.record().upstream_declared.expect("captured");
    assert_eq!(
        declared.model,
        DeclaredIdentity::AutoOnly("openrouter/auto".into())
    );
    assert_eq!(declared.model.value(), None, "no specific name is claimed");
    assert!(declared.model.is_uncertain());

    h.shutdown().await;
}

/// A response without a model member stays missing. Nothing is back-filled
/// from the requested or configured model.
#[tokio::test]
async fn a_missing_declaration_stays_missing() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(&[model("nameless-model", ModelTier::Fast)], mock, addr).await;

    let response = h.ask("fast-class").await;
    assert_eq!(response.status(), 200);
    h.wait_for_outcomes(1).await;

    let declared = h.record().upstream_declared.expect("captured this build");
    assert_eq!(declared.model, DeclaredIdentity::Missing);
    assert_eq!(declared.model_version, DeclaredIdentity::Missing);

    let settlement = h.settlement();
    assert_eq!(
        settlement.attempts[0].declared.as_ref().unwrap().model,
        DeclaredIdentity::Missing,
        "the attempt fact records the same absence"
    );

    h.shutdown().await;
}

/// A fallback chain keeps each attempt's own declaration: the failed send
/// declared nothing, the served one declared its model, and the record carries
/// the terminal attempt's value.
#[tokio::test]
async fn a_fallback_chain_keeps_each_attempts_declaration() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(
        &[
            model("broken-model", ModelTier::Standard),
            model("declared-good", ModelTier::Standard),
        ],
        mock,
        addr,
    )
    .await;

    let response = h.ask("standard-class").await;
    assert_eq!(response.status(), 200);
    h.wait_for_outcomes(1).await;

    let settlement = h.settlement();
    assert_eq!(settlement.attempts.len(), 2, "one attempt per send");
    assert_eq!(
        settlement.attempts[0].declared.as_ref().unwrap().model,
        DeclaredIdentity::Missing,
        "a failed send produced no structured response to declare anything"
    );
    assert_eq!(
        settlement.attempts[1].declared.as_ref().unwrap().model,
        DeclaredIdentity::Declared("declared-good".into())
    );

    let record = h.record();
    assert_eq!(
        record.upstream_declared.as_ref().unwrap().model,
        DeclaredIdentity::Declared("declared-good".into()),
        "the record carries the attempt that settled the request"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn an_http_failure_keeps_its_structured_model_and_known_usage_before_fallback() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(
        &[
            model("billed-failure", ModelTier::Standard).with_priority(0),
            model("declared-good", ModelTier::Standard).with_priority(10),
        ],
        mock,
        addr,
    )
    .await;
    let response = h.ask("standard-class").await;
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    let fact = h.settlement();
    assert_eq!(fact.attempts.len(), 2);
    let failed = &fact.attempts[0];
    assert_eq!(
        failed.declared.as_ref().unwrap().observed_model(),
        Some("failed-backend")
    );
    assert_eq!(failed.usage.unwrap().input_tokens, 12);
    assert!(failed.cost.as_ref().unwrap().amount > 0.0);
    assert_eq!(
        fact.attempts[1].declared.as_ref().unwrap().observed_model(),
        Some("declared-good")
    );
    h.shutdown().await;
}

/// A stream that breaks after its handshake keeps what its frames declared:
/// the attempt failed, and the declaration it did receive stays on the failed
/// attempt instead of being discarded with it.
#[tokio::test]
async fn a_broken_stream_keeps_what_its_frames_declared() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start(
        &[model("stream-then-break-model", ModelTier::Fast)],
        mock,
        addr,
    )
    .await;

    let response = h.ask_streaming("fast-class").await;
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    h.wait_for_outcomes(1).await;

    let record = h.record();
    assert!(!record.ok, "the stream failed");
    let declared = record.upstream_declared.as_ref().expect("captured");
    assert_eq!(
        declared.model,
        DeclaredIdentity::Declared("relay-broken".into())
    );

    let settlement = h.settlement();
    assert_eq!(
        settlement.attempts[0].declared.as_ref().unwrap().model,
        DeclaredIdentity::Declared("relay-broken".into())
    );

    h.shutdown().await;
}

/// Records and attempt facts written before this capture existed load as
/// "not captured" (`None`) — which is a different fact from "the provider
/// declared nothing" — and a present declaration round-trips losslessly.
#[test]
fn legacy_records_load_without_the_declared_fields() {
    let mut builder = zroutery_core::stats::RecordBuilder::new(Dialect::OpenAI, "m", false);
    builder.resolved("relay-m", "Relay");
    let record = builder.finish(10);

    // A record that carries a declaration, serialized, then stripped of the
    // field the way an older build would have written it.
    let mut with_declaration = record.clone();
    with_declaration.upstream_declared = Some(UpstreamModelDeclaration::default());
    let mut legacy = serde_json::to_value(&with_declaration).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("upstream_declared")
        .expect("the field is part of the record when captured");
    let back: RequestRecord = serde_json::from_value(legacy).unwrap();
    assert!(
        back.upstream_declared.is_none(),
        "a legacy record is not captured, not missing"
    );

    // And a present declaration survives a round trip verbatim.
    let mut declaration = UpstreamModelDeclaration::default();
    declaration.observe_model(Some("relay-a"));
    declaration.observe_model(Some("relay-b"));
    let mut with = record.clone();
    with.upstream_declared = Some(declaration.clone());
    let json = serde_json::to_string(&with).unwrap();
    let back: RequestRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(back.upstream_declared, Some(declaration));

    // The attempt fact behaves the same way.
    let entry = ModelEntry::for_upstream("relay", "m", Some(ModelTier::Fast));
    let candidate = zroutery_core::router::Candidate {
        exposed_id: entry.exposed_id(),
        entry,
        provider: ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible),
        degraded: false,
        credential: None,
        rate_quote: None,
    };
    let mut fact = AttemptFact::new("att_1".into(), Some("req_1".into()), &candidate, true);
    fact.declared = Some(UpstreamModelDeclaration::default());
    let mut legacy = serde_json::to_value(&fact).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("declared")
        .expect("the field is part of the fact when captured");
    let back: AttemptFact = serde_json::from_value(legacy).unwrap();
    assert!(back.declared.is_none());
}

/// The protocol-level version axis: Gemini names its version in its own
/// `modelVersion` member, which is kept as a separate fact and never merged
/// into the model name.
#[test]
fn a_gemini_version_is_a_separate_axis() {
    let decoded = zroutery_core::protocol::decode_response(
        Dialect::Gemini,
        json!({
            "model": "gemini-2.5-flash",
            "modelVersion": "gemini-2.5-flash-002",
            "candidates": [{"content": {"parts": [{"text": "hi"}]}, "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 1}
        }),
    )
    .unwrap();
    let declared = UpstreamModelDeclaration::from_wire_fields(Some(decoded.model.as_str()), None);
    // The decoded response carries the classified declaration on its internal
    // key, where the lifecycle reads it.
    let from_response =
        zroutery_core::ir::declaration_from_passthrough(&decoded).expect("captured");
    assert_eq!(from_response.model.value(), Some("gemini-2.5-flash"));
    assert_eq!(
        from_response.model_version,
        DeclaredIdentity::Declared("gemini-2.5-flash-002".into())
    );
    assert_ne!(declared.model, from_response.model_version);
}

/// A body that carries both version spellings keeps their disagreement as a
/// conflict on the version axis instead of silently preferring one.
#[test]
fn conflicting_version_spellings_stay_a_conflict() {
    let decoded = zroutery_core::protocol::decode_response(
        Dialect::OpenAI,
        json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "model": "relay-a",
            "modelVersion": "2024-11-20",
            "model_version": "2025-01-01",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }),
    )
    .unwrap();
    let declared = zroutery_core::ir::declaration_from_passthrough(&decoded).expect("captured");
    assert_eq!(declared.model.value(), Some("relay-a"));
    assert_eq!(
        declared.model_version,
        DeclaredIdentity::Conflicting(vec!["2024-11-20".into(), "2025-01-01".into()])
    );
}
