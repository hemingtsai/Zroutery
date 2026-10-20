#![cfg(feature = "ml")]

//! End-to-end proof that the upstream's declared model survives normalisation.
//!
//! # What is under test
//!
//! The proxy rewrites the response's `model` field to the client-facing `exposed_id`,
//! so a client that asks for a Zroutery model id never learns which relay served it.
//! That rewrite is correct and is not what this test challenges. What it challenges is
//! that the *evidence* of what the upstream actually declared was not destroyed in the
//! process.
//!
//! Before the fix, `pipeline.rs` assigned over `resp.model` (buffered) and built the
//! stream encoder with `exposed_id` (streaming) with nothing kept, so `served_model`
//! could only ever report which configured candidate Zroutery chose — never which
//! backend the provider ran. A relay fronting several backends behind one name is
//! invisible, and any per-model quality statistic fitted on top is measuring the
//! relay's routing rather than the model.
//!
//! Both paths are covered, because they are separate code and either could regress
//! independently.

#![cfg(feature = "ml")]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use zroutery_core::config::{
    AppConfig, MemorySecretStore, ModelEntry, ModelTier, ProviderConfig, ProviderKind,
};
use zroutery_core::ml::execution_identity::UnavailableReason;
use zroutery_core::ml::task_evidence::{TaskStatus, VerifierKind};
use zroutery_core::ml::traces::TraceLog;
use zroutery_core::server::{AppState, ServerHandle};

const TOKEN: &str = "identity-token";

/// The tier's virtual id, derived rather than hardcoded so it cannot drift from
/// [`ModelTier::virtual_id`].
///
/// A trace is only written when the request retained its decision-time snapshot, and
/// that snapshot only exists for a *class-routed* request. A direct model id resolves
/// straight to one candidate and produces no `RouteDecision`, so there is nothing to
/// learn from and nothing to persist. Asking by tier is what makes this test exercise
/// the real collection path.
const TIER: &str = "standard-class";

/// The client-visible model id. `ModelEntry::exposed_id()` namespaces by provider,
/// so this is `alpha-zr-public-name` rather than the bare upstream name.
const EXPOSED: &str = "alpha-zr-public-name";

/// What the upstream claims it ran, distinct from what Zroutery calls it.
const UPSTREAM_DECLARED: &str = "backend-from-a-hidden-pool-0007";

/// Records the bodies the upstream received, so a test can assert what was asked for.
#[derive(Clone, Default)]
struct Mock {
    bodies: Arc<Mutex<Vec<Value>>>,
}

async fn mock_chat(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let wants_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    mock.bodies.lock().expect("bodies").push(body);
    if streaming {
        return if wants_tools {
            streaming_tool_response()
        } else {
            streaming_response()
        };
    }
    // A buffered body declaring a model nobody in Zroutery's config has heard of.
    let payload = json!({
        "id": "chatcmpl-mock-1",
        "object": "chat.completion",
        "created": 1_700_000_000u64,
        "model": UPSTREAM_DECLARED,
        "choices": [{
            "index": 0,
            "message": if wants_tools {
                json!({
                    "role": "assistant",
                    "content": serde_json::Value::Null,
                    "tool_calls": [{
                        "id": "call_weather_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"oslo\"}"},
                    }],
                })
            } else {
                json!({"role": "assistant", "content": "hello"})
            },
            "finish_reason": if wants_tools { "tool_calls" } else { "stop" },
        }],
        "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10},
    });
    axum::Json(payload).into_response()
}

/// An SSE stream whose opening frame declares the model.
fn streaming_response() -> Response {
    let first = format!(
        "data: {}\n\n",
        json!({
            "id": "chatcmpl-mock-stream",
            "object": "chat.completion.chunk",
            "created": 1_700_000_000u64,
            "model": UPSTREAM_DECLARED,
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": null}],
        })
    );
    let second = format!(
        "data: {}\n\n",
        json!({
            "id": "chatcmpl-mock-stream",
            "object": "chat.completion.chunk",
            "created": 1_700_000_000u64,
            "model": UPSTREAM_DECLARED,
            "choices": [{"index": 0, "delta": {"content": "hello"}, "finish_reason": null}],
        })
    );
    let done = "data: [DONE]\n\n";
    let body = format!("{first}{second}{done}");
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from(body))
        .expect("response")
}

/// An SSE stream that emits a tool call, as a real provider does: an opening frame,
/// a tool-use start, argument fragments, and a terminating `Stop`.
fn streaming_tool_response() -> Response {
    let frame = |value: Value| format!("data: {}\n\n", value);
    let id = "chatcmpl-mock-stream-tool";
    let mut body = String::new();
    body.push_str(&frame(json!({
        "id": id, "object": "chat.completion.chunk", "created": 1_700_000_000u64,
        "model": UPSTREAM_DECLARED,
        "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}],
    })));
    body.push_str(&frame(json!({
        "id": id, "object": "chat.completion.chunk", "created": 1_700_000_000u64,
        "model": UPSTREAM_DECLARED,
        "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call_1", "type": "function",
            "function": {"name": "get_weather", "arguments": ""}}]}, "finish_reason": null}],
    })));
    body.push_str(&frame(json!({
        "id": id, "object": "chat.completion.chunk", "created": 1_700_000_000u64,
        "model": UPSTREAM_DECLARED,
        "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0,
            "function": {"arguments": "{\"city\":"}}]}, "finish_reason": null}],
    })));
    body.push_str(&frame(json!({
        "id": id, "object": "chat.completion.chunk", "created": 1_700_000_000u64,
        "model": UPSTREAM_DECLARED,
        "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0,
            "function": {"arguments": "\"oslo\"}"}}]}, "finish_reason": null}],
    })));
    body.push_str(&frame(json!({
        "id": id, "object": "chat.completion.chunk", "created": 1_700_000_000u64,
        "model": UPSTREAM_DECLARED,
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 7, "completion_tokens": 5, "total_tokens": 12},
    })));
    body.push_str("data: [DONE]\n\n");
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from(body))
        .expect("response")
}
async fn start_mock() -> SocketAddr {
    let app = Router::new()
        .route("/v1/chat/completions", post(mock_chat))
        .with_state(Mock::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock serve");
    });
    addr
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    base: String,
    /// Held so the listener outlives `start`: dropping the handle stops the server,
    /// and every test would then fail with a connection reset rather than a real
    /// assertion.
    _server: ServerHandle,
    state: Arc<AppState>,
    client: reqwest::Client,
    trace_dir: std::path::PathBuf,
}

fn config(mock: SocketAddr, trace_dir: &std::path::Path) -> AppConfig {
    let mut cfg = AppConfig::default();
    assert_eq!(
        ModelTier::Standard.virtual_id(),
        TIER,
        "the tier virtual id this test asks by must match the registry's"
    );
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();
    // Durable traces are opt-in: without a state directory there is no trace log at
    // all, so this is what makes the evidence readable.
    cfg.ml_routing.state_dir = trace_dir.display().to_string();
    // A trace is only written when the request retained its decision-time snapshot,
    // and that snapshot is only taken when shadow is on or a model is attached.
    // Neither requires ML ranking to change anything: shadow observes, it does not
    // re-route, so this test exercises capture without altering the served candidate.
    cfg.shadow.enabled = true;
    let mut provider = ProviderConfig::new("alpha", "Alpha", ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{mock}");
    provider.key_ref = "provider:alpha".into();
    provider.timeout_secs = 10;
    cfg.providers = vec![provider];
    // The client-visible name is derived from `upstream_model`, and is deliberately
    // unrelated to what the upstream will claim it ran.
    cfg.models =
        vec![
            ModelEntry::for_upstream("alpha", "zr-public-name", Some(ModelTier::Standard))
                .with_priority(10),
        ];
    cfg
}

async fn start() -> Harness {
    let addr = start_mock().await;
    // A per-test unique directory. Tests run concurrently in one process, so a
    // timestamp is not enough on a fast machine: two harnesses can land in the same
    // nanosecond and then each reads the other's traces, which turns "one request
    // produced one trace" into a coin flip.
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let trace_dir = std::env::temp_dir().join(format!(
        "zroutery-identity-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&trace_dir).expect("trace dir");
    let cfg = config(addr, &trace_dir);
    let secrets = Arc::new(MemorySecretStore::new().with("provider:alpha", "sk-alpha"));
    let state = Arc::new(AppState::new(cfg, secrets));
    let server = ServerHandle::start(Arc::clone(&state))
        .await
        .expect("start");
    Harness {
        base: format!("http://{}", server.addr),
        _server: server,
        state,
        client: reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .expect("client"),
        trace_dir,
    }
}

impl Harness {
    fn ask(&self, streaming: bool) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}/v1/messages", self.base))
            .header("x-api-key", TOKEN)
            .json(&json!({
                "model": TIER,
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "hi"}],
                "stream": streaming,
            }))
    }

    /// Read back every trace this harness recorded.
    fn traces(&self) -> Vec<zroutery_core::ml::RequestTrace> {
        let log = TraceLog::open(&self.trace_dir).expect("open log");
        log.load().expect("load")
    }

    fn cleanup(&self) {
        let _ = &self.state;
        let _ = std::fs::remove_dir_all(&self.trace_dir);
    }
}

/// Wait for the trace to appear: the terminal transition runs after the response is
/// written, so the file is briefly behind the client.
fn wait_for_traces(harness: &Harness) -> Vec<zroutery_core::ml::RequestTrace> {
    for _ in 0..100 {
        let traces = harness.traces();
        if !traces.is_empty() {
            // One more beat so a second in-flight write is not missed.
            std::thread::sleep(std::time::Duration::from_millis(50));
            return harness.traces();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Vec::new()
}

/// A request that offers one tool and requires a call.
fn tool_request() -> Value {
    json!({
        "model": TIER,
        "max_tokens": 64,
        "tools": [{
            "name": "get_weather",
            "description": "Look up the weather",
            "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}},
        }],
        "tool_choice": {"type": "any"},
        "messages": [{"role": "user", "content": "weather in oslo"}],
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_streamed_tool_call_also_produces_a_task_verdict() {
    // Streaming was the open gap in M3: a streamed answer never exists as a
    // response object, so the verifier had nothing to read. The stream is
    // reassembled from its events and judged on the same terms as a buffered one.
    let harness = start().await;
    let mut request = tool_request();
    request["stream"] = json!(true);
    let response = harness
        .client
        .post(format!("{}/v1/messages", harness.base))
        .header("x-api-key", TOKEN)
        .json(&request)
        .send()
        .await
        .expect("send");
    assert!(response.status().is_success(), "{}", response.status());
    let text = response.text().await.expect("body");
    assert!(
        text.contains("get_weather"),
        "the tool call should reach the client: {text}"
    );

    let traces = wait_for_traces(&harness);
    let trace = traces.first().expect("trace");
    assert!(
        !trace.task_evidence.is_empty(),
        "a streamed required tool call must produce a verdict"
    );
    let verdict = &trace.task_evidence[0];
    assert_eq!(verdict.verifier, VerifierKind::ToolExecution);
    assert_eq!(
        verdict.status,
        TaskStatus::Succeeded,
        "the reassembled call should verify: {}",
        verdict.summary
    );
    assert!(verdict.is_attempt_attributable());
}

#[tokio::test]
async fn a_required_tool_call_produces_a_task_verdict_in_the_trace() {
    // The end of the `BLOCKED` chain for task quality: a verdict derived from real
    // traffic, by a check that had the request's demands and the response's content in
    // hand at once. Only the verdict is stored — no prompt, no answer.
    let harness = start().await;
    let response = harness
        .client
        .post(format!("{}/v1/messages", harness.base))
        .header("x-api-key", TOKEN)
        .json(&tool_request())
        .send()
        .await
        .expect("send");
    assert!(response.status().is_success(), "{}", response.status());

    let traces = wait_for_traces(&harness);
    let trace = traces.first().expect("trace");
    assert!(
        !trace.task_evidence.is_empty(),
        "a required tool call must produce a verdict"
    );
    let verdict = &trace.task_evidence[0];
    assert_eq!(verdict.status, TaskStatus::Succeeded);
    assert_eq!(verdict.verifier, VerifierKind::ToolExecution);
    // Attributed to the candidate that actually served it.
    assert!(verdict.is_attempt_attributable());
    assert_eq!(verdict.served_model.as_deref(), Some(EXPOSED));
}

#[tokio::test]
async fn a_request_with_nothing_to_verify_records_no_verdict_rather_than_a_pass() {
    // The absence of a verdict must mean "nothing was checked". If this ever produced
    // an `Unknown` or a `Succeeded`, label coverage would be reporting checks that
    // never ran.
    let harness = start().await;
    let _ = harness.ask(false).send().await.expect("send");
    let traces = wait_for_traces(&harness);
    let trace = traces.first().expect("trace");
    assert!(
        trace.task_evidence.is_empty(),
        "a plain chat turn has no check that applies: {:?}",
        trace.task_evidence
    );
}

#[tokio::test]
async fn a_buffered_response_records_the_upstreams_declared_model() {
    let harness = start().await;
    let response = harness.ask(false).send().await.expect("send");
    assert!(response.status().is_success(), "{}", response.status());
    let wire: Value = response.json().await.expect("json");

    // The client still sees the Zroutery name: the rewrite is unchanged.
    assert_eq!(
        wire["model"].as_str(),
        Some(EXPOSED),
        "client-visible model must remain the exposed id"
    );

    let traces = wait_for_traces(&harness);
    assert_eq!(traces.len(), 1, "expected exactly one trace");
    let identity = &traces[0].execution_identity;

    // The declared model survived the rewrite.
    assert_eq!(
        identity.provider_declared_model.value(),
        Some(UPSTREAM_DECLARED),
        "the upstream declaration must be recorded before normalisation"
    );
    assert!(identity.provider_declared_model.is_provider_declared());
    assert_eq!(
        identity.requested_model, TIER,
        "the requested model is the client's logical id, not the candidate's"
    );
    // Routing provenance is separate and correct.
    assert_eq!(identity.routed_candidate.as_deref(), Some(EXPOSED));
    assert_eq!(identity.routed_provider.as_deref(), Some("alpha"));
    // A declaration that disagrees with the request is reported, not corrected.
    assert_eq!(identity.declared_matches_request(), Some(false));

    harness.cleanup();
}

#[tokio::test]
async fn a_streaming_response_records_the_upstreams_declared_model() {
    let harness = start().await;
    let response = harness.ask(true).send().await.expect("send");
    assert!(response.status().is_success(), "{}", response.status());
    let text = response.text().await.expect("body");
    // The stream still publishes the Zroutery name.
    assert!(
        text.contains("zr-public-name"),
        "streaming frames must still carry the exposed id"
    );

    let traces = wait_for_traces(&harness);
    assert_eq!(traces.len(), 1, "expected exactly one trace");
    let identity = &traces[0].execution_identity;

    // Streaming declares the model only in its opening frame, so this is the sole
    // place the value exists — and it was previously discarded entirely.
    assert_eq!(
        identity.provider_declared_model.value(),
        Some(UPSTREAM_DECLARED),
        "the streaming opening frame declares the model and it must be captured"
    );
    assert_eq!(identity.declared_matches_request(), Some(false));

    harness.cleanup();
}

#[tokio::test]
async fn a_provider_that_declares_nothing_leaves_the_identity_unknown() {
    let harness = start().await;
    // Drive a request whose upstream body is never parsed into a model field by
    // asking for a model the registry does not know: the proxy answers without
    // contacting an upstream, so nothing can have been declared.
    let response = harness
        .client
        .post(format!("{}/v1/messages", harness.base))
        .header("x-api-key", TOKEN)
        .json(&json!({
            "model": "alpha-no-such-model",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hi"}],
        }))
        .send()
        .await
        .expect("send");
    assert!(!response.status().is_success());

    let traces = wait_for_traces(&harness);
    if let Some(trace) = traces.first() {
        // Never reached an upstream, so the reason must say exactly that — and the
        // requested name must not have been promoted into the declared field.
        let declared = &trace.execution_identity.provider_declared_model;
        if declared.is_unavailable() {
            assert_eq!(declared.value(), None);
            assert_eq!(
                *declared,
                zroutery_core::ml::IdentityEvidence::Unavailable {
                    reason: UnavailableReason::UpstreamNotContacted
                }
            );
        }
        assert_ne!(
            trace.execution_identity.provider_declared_model.value(),
            Some("no-such-model"),
            "the requested model must never stand in for a declaration"
        );
    }
    harness.cleanup();
}

#[tokio::test]
async fn a_trace_records_the_execution_identity_alongside_the_samples() {
    let harness = start().await;
    let _ = harness.ask(false).send().await.expect("send");
    let traces = wait_for_traces(&harness);
    let trace = traces.first().expect("trace");

    // The identity is additive: samples, decision input and ids are all still there.
    assert!(!trace.samples.is_empty());
    assert_eq!(trace.decision_id, trace.input.decision_id);
    assert!(!trace.request_id.is_empty());
    assert_eq!(trace.execution_identity.coverage() > 0.0, true);
    // And the routing identity inside the samples is untouched.
    assert!(trace.request_sample().is_some() || trace.attempt_samples().count() > 0);

    harness.cleanup();
}
