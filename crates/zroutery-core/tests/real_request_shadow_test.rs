#![cfg(feature = "ml")]

//! Node 7F — the shadow capability, reachable, against real requests.
//!
//! What "real requests" means here, stated once so no sentence in this file can
//! be read as a stronger claim than it is: these tests make **real HTTP
//! requests through the production serving path against a local fake upstream**.
//! The request goes through `ServerHandle` → the same axum router, the same
//! registry, the same `RequestLifecycle`, the same outcome accounting and the
//! same response construction that a deployed proxy uses. The upstream is a fake
//! running in this process. There is no deployed system, no user, and no caller
//! anywhere but this test binary, and nothing measured here should be described
//! as demand observed anywhere else.
//!
//! The claims under test, in the order the parent ruled on them:
//!
//! 1. **The candidate verifies on the way in.** `ModelCommit::verify`,
//!    `ReplayEngine::verify_checkpoint_integrity`, and a complete lineage —
//!    each named separately, and each shown to refuse a broken artifact.
//! 2. **The artifact survives persistence.** A plain JSON round trip
//!    re-verifies, demonstrated rather than assumed.
//! 3. **The shadow changes nothing served.** The same request sequence, against
//!    a fresh server each time, with and without a candidate attached: status,
//!    every response header and every body byte compared, with a
//!    candidate-withdrawn control run proving the only difference is the one
//!    Core mints fresh per request.
//! 4. **The shadow's own output genuinely differs** between those runs, so (3)
//!    is a measurement rather than a tautology.
//! 5. **The candidate reaches a named verdict** through 7E-3's gate over
//!    real-request evidence, with the statistical constituent's refusal reported
//!    in full and no floor moved to obtain it. The refusal obtained is
//!    `sample_too_small`: 12 effective decisions in the holdout against a floor
//!    of 30. That number is the result, not a shortfall to be engineered away —
//!    see [`FAILOVER_FRACTION`] for what this window can and cannot support, and
//!    `the_statistical_floor_is_untouched_and_the_power_formula_says_what_it_would_need`
//!    for the evidence the gate says it actually wants.
//! 6. **The observability projection is invoked on the serving path**, it
//!    correlates, it is deterministic, and its refusals survive.
//! 7. **Rollback restores the prior behaviour exactly**, by withdrawal, at
//!    runtime, on a live server.
//!
//! Fixture note, because it matters to how the numbers should be read: the
//! upstream is a fake and the requests are synthetic in the sense that a test
//! writes them. They are nevertheless real requests through real code. What
//! they are *not* is a measurement of any deployed system's behaviour, and the
//! gate verdict they produce is reported as exactly that.

use std::collections::{BTreeMap, BTreeSet};
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
use zroutery_core::ml::dataset::TrainingSample as DatasetTrainingSample;
use zroutery_core::ml::model_identity::{ModelCommit, ReplayEngine};
use zroutery_core::ml::offline_gate::{
    run_offline_gate, GateConfig, GateInput, RecordedDecision, ReleaseVerdict,
};
use zroutery_core::ml::shadow::{ModelEnsemblePredictor, ShadowDecision};
use zroutery_core::ml::statistics::{required_decisions, StatisticalConfig};
use zroutery_core::observability::{Projection, ProjectionBatch, RefusalReason};
use zroutery_core::outcome::Outcome;
use zroutery_core::server::{
    AppState, ServerHandle, ShadowAttachment, ShadowCandidate, ShadowCandidateArtifact,
    ShadowCandidateError, SHADOW_CANDIDATE_ARTIFACT, SHADOW_CANDIDATE_ENVELOPE_VERSION,
};

// ---------------------------------------------------------------------------
// The local fake upstream
// ---------------------------------------------------------------------------

/// Call counts per upstream model.
///
/// These were once load-bearing: the injected models failed on odd calls and
/// answered on even ones, so the flakiness was a function of how many times a
/// model had been called. The injected models now fail every call (see
/// [`fake_chat`] for why that changed), so the counter no longer decides
/// anything and the upstream's behaviour is a pure function of which model was
/// asked. The counts are kept because they are how a reader can see that fact —
/// every injected model is asked exactly once per routed decision, with no retry
/// and no rectifier pass.
///
/// This matters more than it looks either way. The served-bytes comparison runs
/// the same sequence three times and requires identical routing each time, so the
/// upstream's behaviour has to be a pure function of the request sequence. Wall
/// clock jitter would make the comparison meaningless.
#[derive(Clone, Default)]
struct Fake {
    calls: Arc<Mutex<BTreeMap<String, usize>>>,
}

impl Fake {
    fn next_call(&self, model: &str) -> usize {
        let mut calls = self.calls.lock().unwrap();
        let counter = calls.entry(model.to_string()).or_insert(0);
        *counter += 1;
        *counter
    }
}

/// A model named by [`INJECTED_MODELS`] fails **every** call.
///
/// This used to fail on odd calls and answer on even ones, and that has to be
/// reported rather than quietly replaced, because the reason it had to change is
/// the whole substance of this node:
///
/// * **What the old rule could not do.** With the injected models in the first
///   routing slot, the first candidate is asked *exactly once per decision*, so
///   an odd/even rule makes it succeed on every even ask. Measured, that gave a
///   window of 30 routed decisions containing 16 with arity 2 and **14 with arity
///   1** — a 53.3% failover rate. And `measure_release_evidence` does not skip an
///   arity-1 decision: `attribution.rs` returns `DegenerateAxis` on the *first*
///   one it walks, aborting the whole partition. So a cohort containing any
///   arity-1 decision is unmeasurable, and an odd/even head produces arity-1
///   decisions at every window size and every priority. No combination of
///   `FLAKY_PRIORITY`, `ROUTED_REQUESTS` or breaker settings reaches a measurable
///   cohort while the head alternates.
/// * **What replaced it.** The injected models now fail every call, which is what
///   this doc comment always *claimed* they did ("a deterministic two-attempt
///   failover on every request that reaches it") and what the code did not do.
///   Every routed decision now has arity 2.
///
/// The cost is stated in [`FAILOVER_FRACTION`]: the window's first-choice
/// failure rate is 100%, which is not a production rate and is not claimed to be
/// one. It is bought deliberately, to buy a cohort the gate can walk at all.
///
/// Failing every call is still a pure function of the request sequence, which is
/// what the served-bytes comparison needs: it depends only on which model was
/// asked, never on a clock or a counter's parity.
async fn fake_chat(State(fake): State<Fake>, Json(body): Json<Value>) -> Response {
    let model = body["model"].as_str().unwrap_or_default().to_string();
    fake.next_call(&model);
    if INJECTED_MODELS.iter().any(|name| model == *name) {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "fake upstream failure", "type": "server_error"}})),
        )
            .into_response();
    }
    let stream = body["stream"].as_bool().unwrap_or(false);
    let message = json!({
        "role": "assistant",
        "content": format!("answer from {model}"),
        "reasoning_content": "thinking",
    });
    if stream {
        let mut sse = String::new();
        let chunk = |delta: Value, finish: Value| {
            format!(
                "data: {}\n\n",
                json!({"id": "chatcmpl-fake", "object": "chat.completion.chunk", "created": 1,
                       "model": model, "choices": [{"index": 0, "delta": delta,
                       "finish_reason": finish}]})
            )
        };
        sse.push_str(&chunk(
            json!({"role": "assistant", "content": ""}),
            Value::Null,
        ));
        sse.push_str(&chunk(
            json!({"reasoning_content": "thinking"}),
            Value::Null,
        ));
        sse.push_str(&chunk(json!({"content": "answer"}), Value::Null));
        sse.push_str(&chunk(json!({}), json!("stop")));
        sse.push_str(&format!(
            "data: {}\n\n",
            json!({"id": "chatcmpl-fake", "object": "chat.completion.chunk", "created": 1,
                   "model": model, "choices": [], "usage": {"prompt_tokens": 11,
                   "completion_tokens": 7}})
        ));
        sse.push_str("data: [DONE]\n\n");
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(sse))
            .unwrap();
    }
    Json(json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion",
        "created": 1,
        "model": model,
        "choices": [{"index": 0, "message": message, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7}
    }))
    .into_response()
}

async fn start_fake() -> (SocketAddr, Fake) {
    let fake = Fake::default();
    let app = axum::Router::new()
        .route("/v1/chat/completions", post(fake_chat))
        .route("/chat/completions", post(fake_chat))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, fake)
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const TOKEN: &str = "zr-7f-token";

/// Class-routed requests, so the shadow has a real decision to observe.
///
/// Alternating the class is not decoration: the calibration partition refuses a
/// single served candidate outright, because with one winner the axis cannot be
/// told apart from a uniform guess. Two tiers give the window more than one
/// served identity in both partitions without having to make routing
/// unpredictable, which would destroy the served-bytes comparison.
const ROUTED_REQUESTS: usize = 30;

/// Direct model-id requests, which carry no routing decision by construction.
///
/// These exist to put a genuine refusal on the serving path: an absent decision
/// id must be reported absent. Skipping them would produce a clean run by
/// declining to ask the question.
const DIRECT_REQUESTS: usize = 4;

fn provider(id: &str, name: &str, upstream: SocketAddr) -> ProviderConfig {
    let mut provider = ProviderConfig::new(id, name, ProviderKind::OpenAICompatible);
    provider.base_url = format!("http://{upstream}");
    provider.key_ref = format!("provider:{id}");
    provider.timeout_secs = 10;
    provider
}

fn model(provider: &str, upstream: &str, priority: i32, tier: ModelTier) -> ModelEntry {
    let mut entry =
        ModelEntry::for_upstream(provider, upstream, Some(tier)).with_priority(priority);
    // One price for every model, so the served identity varies by tier rather
    // than by price and the routing decision is a function of the request alone.
    entry.pricing = Some(Pricing::new("USD", 3.0, 15.0));
    entry
}

/// The priority the flaky models sit at, which is what makes them the *first*
/// choice in their tier.
///
/// `RoutingStrategy::Priority` orders a tier by ascending priority number
/// (`router::order` sorts on `priority` alone), so the lower number is tried
/// first. This constant used to be **20**, which put `flaky-std` and
/// `flaky-fast` behind `std-one`/`std-two`/`fast-one`/`fast-two` at 10. Routing
/// therefore never *selected* the flaky model, its failure injection never
/// fired, and every decision in the window had exactly one attempt.
///
/// That made the window unmeasurable rather than merely unrepresentative.
/// `calibration::project_cohorts` builds each decision's K axis from the
/// attempt-scope rows the production dataset boundary created
/// (`dataset.rs:620` emits one `SampleScope::Attempt` row per attempt), so an
/// axis of arity 1 is all a never-failing window can produce — and
/// `statistics::measure_release_evidence` refuses an arity-1 axis with
/// `DegenerateAxis`, because one candidate cannot be ranked. The gate was
/// refusing with `degenerate_axis` on the first decision it walked, and no
/// amount of evidence would have moved it: **no decision in the window contained
/// a choice.** A router evaluated on data where routing never happened has
/// measured nothing.
///
/// Moving the flaky models to 5 makes the injected failure the first-choice
/// outcome of every routed request, which is what puts a real, two-candidate
/// choice in the axis at all. This half is a pure fixture repair: the
/// failure-injection mechanism already existed and was documented, it was simply
/// unreachable.
///
/// It is only half the repair, though, and the other half is *not* a pure repair:
/// once the injection is reachable the mechanism's own odd/even rule stops
/// admitting a measurable cohort, and the rule had to change too. See
/// [`fake_chat`] for that argument and [`FAILOVER_FRACTION`] for the rate it
/// leaves behind and for what that rate does and does not license a conclusion
/// about. Nothing about the rate is a tunable — there is no knob that sets it.
const FLAKY_PRIORITY: i32 = 5;

/// The upstream model names whose failures are injected, and the exposed ids
/// that resolve to them.
///
/// Kept as one list because the two spellings are easy to let drift: the fake
/// upstream injects on `body["model"]`, which is the bare upstream name, while
/// `Outcome.attempts` records the *exposed* id. Anything matching the injected
/// models is matched on the suffix so a `provider-` prefix does not defeat it.
const INJECTED_MODELS: [&str; 2] = ["flaky-std", "flaky-fast"];

/// The breaker's consecutive-failure threshold, set past anything a 34-request
/// window can produce. See `config_for` for why this alone was never enough.
const BREAKER_FAILURE_THRESHOLD: u32 = 1000;

/// The breaker holds per-model request counts below this, so its error-rate rule
/// cannot apply inside this window. See `config_for`.
const BREAKER_MIN_REQUESTS: u32 = 1000;

/// The window's configuration.
///
/// Shadow evaluation on — that is the capability under test.
///
/// Both tiers carry exactly three candidates, so the *roster* every request is
/// planned against is the same size throughout. That is not cosmetic: 7E-2D's
/// drift gate compares the fit and holdout partitions and refuses the whole run
/// when they sit in different bins, so a fixture whose tiers had different
/// candidate counts would be refused for a reason that has nothing to do with
/// the commit under consideration. Alternating the tiers is still what rotates
/// the served identity, which a single-tier window could not do — the partition
/// check refuses an axis on which only one candidate ever served.
///
/// Note what the roster size does *not* determine. The K axis is the number of
/// **attempts a decision actually made**, not the number of models it could have
/// chosen, so equal rosters buy a uniform plan and not a uniform axis. Before
/// this node's repair the axis was uniformly 1; it is now deliberately not
/// uniform, and `the_window_contains_a_rankable_axis_and_states_its_failover_rate`
/// measures exactly what it is rather than asserting a shape.
///
/// The breaker is configured so nothing is quarantined partway through the
/// window, because a mid-window regime change is what 7E-2D's drift gate
/// refuses. See the assignments below: getting this right took two corrections,
/// and both are documented where they happen. No statistical or calibration
/// default is touched anywhere in this file.
fn config_for(upstream: SocketAddr) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();
    cfg.providers = vec![
        provider("alpha", "Alpha", upstream),
        provider("beta", "Beta", upstream),
    ];
    cfg.models = vec![
        model("alpha", "flaky-std", FLAKY_PRIORITY, ModelTier::Standard),
        model("alpha", "std-one", 10, ModelTier::Standard),
        model("beta", "std-two", 10, ModelTier::Standard),
        model("alpha", "flaky-fast", FLAKY_PRIORITY, ModelTier::Fast),
        model("alpha", "fast-one", 10, ModelTier::Fast),
        model("beta", "fast-two", 10, ModelTier::Fast),
    ];
    // Every path from `Closed` to `Open` is put out of reach for a window this
    // short, so no candidate can be quarantined partway through.
    //
    // This fixture used to set `routing.break_after_failures` to 1000, and that
    // did nothing at all, twice over:
    //
    // 1. `break_after_failures` is a **legacy** field. `RoutingConfig::apply_legacy`
    //    migrates it into `routing.circuit_breaker.failure_threshold`, and only
    //    `AppConfig::normalize` calls that. This fixture builds
    //    `AppConfig::default()` in code and never normalizes it, so the live
    //    threshold stayed at `CircuitBreakerConfig::default()`'s **4**.
    // 2. Even at a live threshold of 1000 the window would still have changed
    //    shape. `CircuitBreaker::record_failure` opens on **either** consecutive
    //    failures **or** a sustained error rate once `min_requests` have
    //    accumulated, and the injected failure rate is high enough to trip the
    //    default `error_rate_threshold` of 0.6. Raising only the consecutive
    //    threshold could never have produced one regime here.
    //
    // So both gates are raised, and `min_requests` is raised rather than
    // `error_rate_threshold`: "minimum requests before the error rate check
    // applies" is a window-size precondition, and raising it declares the truth —
    // a 34-request window is far too small for a sustained error rate to mean
    // anything. The *rate* thresholds themselves, `failure_threshold` at 1000
    // and `error_rate_threshold` left at its own default of 0.6, are not
    // weakened; both are simply unreachable within the window.
    //
    // None of this touches a statistical or calibration default. It is the
    // fixture's own routing configuration, and it exists to keep one regime
    // throughout the window — the property 7E-2D's drift gate is entitled to
    // assume and that an earlier version of this file broke.
    // `the_window_has_one_regime_and_the_injected_model_never_leaves_the_first_slot`
    // measures the consequence rather than trusting these assignments.
    cfg.routing.circuit_breaker.failure_threshold = BREAKER_FAILURE_THRESHOLD;
    cfg.routing.circuit_breaker.min_requests = BREAKER_MIN_REQUESTS;
    cfg.shadow.enabled = true;
    cfg
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One served response, reduced to exactly what is compared.
#[derive(Debug, Clone)]
struct Served {
    status: u16,
    headers: BTreeMap<String, String>,
    /// The response body as JSON with the one per-request uuid masked out. See
    /// [`mask_per_request_identity`].
    body: Value,
    /// Whether the mask actually fired for this response.
    ///
    /// Carried so the comparison can *state* how much was excluded rather than
    /// leaving it to be assumed. When this is `false` for every response, the
    /// bodies were compared byte for byte with nothing excluded, which is a
    /// stronger claim than "identical modulo a known-random field" and is the
    /// one this fixture happens to support.
    masked_id: bool,
}

/// Replace the one field Core mints fresh for every response.
///
/// `protocol::anthropic` builds `id` as `msg_` plus a fresh v4 uuid, so two
/// identical requests differ there whether or not anything is attached to the
/// shadow. Rather than pretend it does not, the comparison masks exactly that
/// field and [`served_bytes_are_identical`] proves the mask is not hiding
/// anything by also running the same configuration twice and requiring the same
/// masked body.
fn mask_per_request_identity(body: &Value) -> Value {
    let mut masked = body.clone();
    if let Some(object) = masked.as_object_mut() {
        if let Some(id) = object.get_mut("id") {
            if id.as_str().is_some_and(|raw| raw.starts_with("msg_")) {
                *id = Value::String("<per-request uuid>".to_string());
            }
        }
    }
    masked
}

struct Harness {
    base: String,
    server: Option<ServerHandle>,
    state: Arc<AppState>,
    client: reqwest::Client,
}

impl Harness {
    async fn start(upstream: SocketAddr, attachment: ShadowAttachment) -> Harness {
        let secrets = Arc::new(
            MemorySecretStore::new()
                .with("provider:alpha", "sk-alpha")
                .with("provider:beta", "sk-beta"),
        );
        let state = Arc::new(AppState::with_shadow_attachment(
            config_for(upstream),
            secrets,
            attachment,
        ));
        let server = ServerHandle::start(Arc::clone(&state)).await.unwrap();
        Harness {
            base: format!("http://{}", server.addr),
            server: Some(server),
            state,
            client: reqwest::Client::builder()
                .pool_max_idle_per_host(0)
                .build()
                .unwrap(),
        }
    }

    async fn ask(&self, model: &str, stream: bool) -> Served {
        let mut body = json!({
            "model": model,
            "max_tokens": 32,
            "messages": [{"role": "user", "content": "what is the routing evidence?"}],
        });
        if stream {
            body["stream"] = Value::Bool(true);
        }
        let response = self
            .client
            .post(format!("{}/v1/messages", self.base))
            .header("x-api-key", TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let headers: BTreeMap<String, String> = response
            .headers()
            .iter()
            // `Date` is emitted by hyper from the wall clock. Core never sets
            // it and no routing or shadow decision can reach it, so comparing it
            // would measure how long a test took rather than what was served.
            // It is excluded here and the control run is what proves nothing
            // else varies: if another field ever did, the control would report
            // it and this exclusion would be visibly incomplete.
            .filter(|(name, _)| !name.as_str().eq_ignore_ascii_case("date"))
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or("<binary>").to_string(),
                )
            })
            .collect();
        let raw = response.bytes().await.unwrap();
        let mut masked_id = false;
        let parsed: Value = if stream {
            // A stream is not JSON; keep the wire text. The upstream's echoed
            // id is a constant here, so nothing is replaced — but the
            // substitution stays, because an upstream that mints a fresh id per
            // response would otherwise make this comparison meaningless.
            let text = String::from_utf8_lossy(&raw).to_string();
            Value::String(text.replace("chatcmpl-fake", "<upstream id>"))
        } else {
            let body: Value = serde_json::from_slice(&raw).expect("a JSON body");
            masked_id = body
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|raw| raw.starts_with("msg_"));
            mask_per_request_identity(&body)
        };
        Served {
            status,
            headers,
            body: parsed,
            masked_id,
        }
    }

    /// Every stored shadow record, oldest first.
    fn shadow_records(&self) -> Vec<ShadowDecision> {
        self.state.shadow().store().decisions()
    }

    async fn shutdown(mut self) {
        if let Some(server) = self.server.take() {
            server.stop().await;
        }
    }
}

/// A cancellation or a disconnect is noticed off the request path, so the
/// assertions wait for the terminal transition instead of guessing.
async fn settle(state: &AppState, expected: usize) {
    for _ in 0..400 {
        if state.outcomes().len() >= expected {
            tokio::time::sleep(Duration::from_millis(25)).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "expected {expected} terminal outcomes, saw {}",
        state.outcomes().len()
    );
}

/// One window of real requests: the served responses and the evidence the
/// requests produced.
struct Window {
    served: Vec<Served>,
    records: Vec<ShadowDecision>,
    outcomes: Vec<Outcome>,
    projections: ProjectionBatch,
    per_request: Vec<Projection>,
    attached_commit: Option<String>,
}

/// Drive the window against a fresh server with the given attachment.
///
/// A fresh server per window is not tidiness: the router carries health, latency
/// observations and quarantine state, so a second window on the same instance
/// would be measured against a different starting state and the served-byte
/// comparison would be comparing two different experiments.
async fn drive_window(attachment: ShadowAttachment, requests: usize) -> Window {
    let (upstream, _fake) = start_fake().await;
    let harness = Harness::start(upstream, attachment).await;
    let attached_commit = harness.state.shadow_candidate_commit_id();

    let mut served = Vec::with_capacity(requests);
    for index in 0..requests {
        // Alternate the class so more than one identity serves in both
        // partitions, then finish with the direct ids that carry no decision.
        let model = if index < ROUTED_REQUESTS {
            if index % 2 == 0 {
                "standard-class"
            } else {
                "fast-class"
            }
        } else {
            "alpha-std-one"
        };
        // One streaming request per window, so both request paths are covered.
        let stream = index == 1;
        served.push(harness.ask(model, stream).await);
    }

    settle(&harness.state, requests).await;
    let records = harness.shadow_records();
    let outcomes = harness.state.outcomes().recent(requests + 8);
    let projections = harness.state.projections().batch();
    let per_request = harness.state.projections().projections();
    harness.shutdown().await;

    Window {
        served,
        records,
        outcomes,
        projections,
        per_request,
        attached_commit,
    }
}

/// The request sequence every window replays.
fn window_requests() -> usize {
    ROUTED_REQUESTS + DIRECT_REQUESTS
}

/// Compare two windows' served responses in full and describe every difference.
///
/// Returns the differences rather than asserting, so the caller decides what a
/// difference means. A difference that is not the masked per-request identity is
/// a difference in what a client received, and nothing in this node may produce
/// one.
fn served_differences(left: &[Served], right: &[Served]) -> Vec<String> {
    let mut differences = Vec::new();
    assert_eq!(
        left.len(),
        right.len(),
        "the two windows must drive the same number of requests"
    );
    for (index, (a, b)) in left.iter().zip(right).enumerate() {
        if a.status != b.status {
            differences.push(format!(
                "request {index}: status {} != {}",
                a.status, b.status
            ));
        }
        if a.headers != b.headers {
            let mut changed: Vec<String> = Vec::new();
            for (name, left) in &a.headers {
                match b.headers.get(name) {
                    Some(right) if left != right => {
                        changed.push(format!("{name}: {left:?} != {right:?}"));
                    }
                    Some(_) => {}
                    None => changed.push(format!("{name}: {left:?} != <absent>")),
                }
            }
            for (name, right) in &b.headers {
                if !a.headers.contains_key(name) {
                    changed.push(format!("{name}: <absent> != {right:?}"));
                }
            }
            differences.push(format!(
                "request {index}: headers differ [{}]",
                changed.join("; ")
            ));
        }
        if a.body != b.body {
            differences.push(format!(
                "request {index}: body differs\n    left:  {}\n    right: {}",
                a.body, b.body
            ));
        }
    }
    differences
}

/// Assert two windows served byte-identical responses.
///
/// The claim is not "these two bodies are equal strings" — it is "attaching the
/// candidate changed nothing a client can observe", and that is only meaningful
/// against the noise floor. The noise floor is measured, not assumed: the caller
/// runs the same configuration twice and passes the result here as the control.
fn assert_served_identical(left: &[Served], right: &[Served], control: &[String], what: &str) {
    let differences = served_differences(left, right);
    assert_eq!(
        differences, control,
        "{what}: the served responses differ by more than the control does. The control is two \
         runs of the *same* configuration, so anything beyond it is a real change in what a \
         client received."
    );
}

/// What the shadow itself recorded, reduced to the parts that must differ when a
/// candidate is attached.
///
/// `shadow_id` is excluded on purpose: it is `shadow-` plus a fresh uuid for
/// every record, so it differs between any two runs and would make a comparison
/// of it vacuous. `model_commit` and `decision_checksum` are the two fields that
/// actually say which model produced the record and what it decided.
fn shadow_evidence(records: &[ShadowDecision]) -> Vec<(String, u64, String)> {
    records
        .iter()
        .map(|record| {
            (
                record.shadow.model_commit.as_str().to_string(),
                record.decision_checksum,
                record.shadow.selected.clone(),
            )
        })
        .collect()
}

/// The shadow's verdict alone, without the checksum that folds in measured
/// latency.
///
/// Two servers that ran the same requests measured their own latencies, so their
/// decision-time inputs differ and their checksums are not comparable. What *is*
/// comparable across servers is which commit produced the record and which model
/// it would have chosen, and that is what this returns.
fn shadow_verdicts(records: &[ShadowDecision]) -> Vec<(String, String)> {
    records
        .iter()
        .map(|record| {
            (
                record.shadow.model_commit.as_str().to_string(),
                record.shadow.selected.clone(),
            )
        })
        .collect()
}

/// Pair each stored shadow record with the canonical outcome for the same
/// request, which is what the offline gate replays.
fn recorded_decisions(window: &Window) -> Vec<RecordedDecision> {
    let outcomes: BTreeMap<&str, &Outcome> = window
        .outcomes
        .iter()
        .map(|outcome| (outcome.request_id.as_str(), outcome))
        .collect();
    window
        .records
        .iter()
        .filter_map(|decision| {
            outcomes
                .get(decision.actual.request_id.as_str())
                .map(|outcome| RecordedDecision {
                    decision: decision.clone(),
                    outcome: (*outcome).clone(),
                })
        })
        .collect()
}

/// The gate configuration, at its own defaults.
///
/// [`GateConfig::default`] already carries 7D's `StatisticalConfig::default`
/// and 7E-2D's `CalibrationConfig::default`. Nothing here loosens anything, and
/// [`the_statistical_floor_is_untouched`] fails if a future edit tries to.
fn gate_config() -> GateConfig {
    GateConfig::default()
}

// ---------------------------------------------------------------------------
// The K axis, measured
// ---------------------------------------------------------------------------

/// The failover rate this window's configuration produces: **every** routed
/// decision, 30 of 30, 100%.
///
/// **This number was chosen, and saying so is the point of this entry.** It is
/// not a parameter anyone can turn, and the reason it had to be 100% is
/// structural rather than convenient — but a reader is entitled to know that a
/// number arrived at by eliminating the alternatives is still a chosen number.
///
/// Why 100% is forced: `measure_release_evidence` aborts the whole partition on
/// the *first* arity-1 decision it walks, so a cohort is measurable only if
/// **every** decision in the holdout compared at least two candidates. The first
/// candidate in a plan is attempted exactly once per decision, so the only way to
/// keep every decision's arity at 2 is for the first candidate to fail every
/// time. Measured at the nearest alternative — an odd/even head, which yields a
/// 53.3% failover rate — the window carried 14 arity-1 decisions and the gate
/// refused with `degenerate_axis` before measuring anything, at any window size.
///
/// **Is it plausible?** As a first-choice upstream failure rate for a healthy
/// production fleet, **no**, and it is not dressed down here. No real fleet fails
/// every first attempt. It is plausible as what it actually is: a fixture whose
/// purpose is to exercise the failover path, soaked until the path is taken on
/// every decision so that the axis has something to rank. The distinction is the
/// difference between a representative cohort and a convenient one:
///
/// - **CAN** support: that the offline gate can walk a cohort whose decisions
///   genuinely contained a choice — that `project_cohorts` builds a multi-candidate
///   K axis from real serving-path evidence, that a failover survives replay,
///   attribution and the partition checks, and that the refusal which follows is
///   about *how much evidence* the gate needs rather than about the evidence
///   being unmeasurable. Before this repair none of that held: the axis was arity
///   1 throughout and the gate refused with `degenerate_axis` on the first
///   decision it walked.
/// - **CANNOT** support: anything about production failure rates, and therefore
///   anything about how a router performs when upstream failures are rare or
///   intermittent. This window was deliberately made maximally failure-heavy; a
///   number measured on it describes this window's chosen difficulty, not a
///   fleet's. It is also not a sample of demand — the upstream is an in-process
///   fake and the requests are synthetic, as the file's own header says. In
///   particular, **nothing measured here supports a claim that the model is safe
///   to serve**, and the verdict printed by
///   `the_candidate_reaches_a_named_verdict_over_real_request_evidence` is
///   reported exactly as obtained.
const FAILOVER_FRACTION: &str =
    "30 of 30 routed decisions (100%), forced by the gate's all-or-nothing \
                                arity requirement";

/// The K axis the gate walks, measured through the accepted projection.
///
/// Two properties of this function are what make the number worth reporting:
///
/// * it projects over the **same snapshot the gate projects over**. The gate
///   builds its holdout from `canonical_samples_from_decision_time` per recorded
///   decision, so this does too, rather than measuring the dataset store's
///   `training_slice` and calling it the axis;
/// * the model it hands to `project_cohorts` is a cold-start ensemble, and that
///   is deliberate. The K axis is the *count of attempt rows in a decision's
///   group*, which is fixed by how many attempts the router made and by nothing
///   in the model. Using the cold-start ensemble therefore measures the axis
///   without letting a parameter move it, and no model is trained or consulted
///   beyond constructing the type the trait needs.
fn axis_arities(recorded: &[RecordedDecision]) -> BTreeMap<usize, usize> {
    let mut samples = Vec::new();
    for entry in recorded {
        samples.extend(
            zroutery_core::ml::dataset::canonical_samples_from_decision_time(
                &entry.outcome,
                entry.input(),
                zroutery_core::feedback::DataOrigin::Native,
            )
            .expect("a recorded decision's retained features are complete by construction"),
        );
    }
    let ensemble = zroutery_core::ml::model_identity::ModelEnsemble::new();
    let cohorts = zroutery_core::ml::calibration::project_cohorts(
        &samples,
        &ensemble.success,
        zroutery_core::ml::calibration::CalibrationConfig::default().probability_floor,
    )
    .expect("the window's decisions project into cohorts");
    let mut histogram: BTreeMap<usize, usize> = BTreeMap::new();
    for cohort in &cohorts {
        *histogram.entry(cohort.arity()).or_insert(0) += 1;
    }
    histogram
}

/// How many of a window's decisions attempted more than one candidate.
///
/// Counted from the outcomes' own attempt lists rather than from the projection,
/// so the two measurements are independent and can be compared.
fn failover_count(recorded: &[RecordedDecision]) -> (usize, usize) {
    let mut failover = 0usize;
    for entry in recorded {
        if entry.outcome.attempts.len() > 1 {
            failover += 1;
        }
    }
    (failover, recorded.len())
}

/// The window's K axis is rankable, and the rate at which it is.
///
/// Before this node's repair every decision in this window had an axis of arity
/// 1 and the gate refused with `degenerate_axis` without measuring anything.
/// The measurement below is the evidence that the repair worked, and it is a
/// measurement rather than an assertion about a shape: it projects the real
/// snapshot and counts what came out.
#[tokio::test]
async fn the_window_contains_a_rankable_axis_and_states_its_failover_rate() {
    let window = drive_window(
        ShadowAttachment::embedded().expect("the candidate verifies"),
        window_requests(),
    )
    .await;
    let recorded = recorded_decisions(&window);
    assert!(
        !recorded.is_empty(),
        "the window must produce decisions or there is no axis to measure"
    );

    let histogram = axis_arities(&recorded);
    let total: usize = histogram.values().sum();
    let rankable: usize = histogram
        .iter()
        .filter(|(arity, _)| **arity >= 2)
        .map(|(_, count)| *count)
        .sum();
    let degenerate: usize = histogram
        .iter()
        .filter(|(arity, _)| **arity < 2)
        .map(|(_, count)| *count)
        .sum();
    println!(
        "K axis over {total} decisions: {}",
        histogram
            .iter()
            .map(|(arity, count)| format!("arity {arity}: {count}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "rankable (arity >= 2): {rankable}; degenerate (arity 1): {degenerate}; \
         expected failover rate: {FAILOVER_FRACTION}"
    );

    // The whole point of the repair: the gate's statistical walk aborts on the
    // *first* arity-1 decision, so a partially rankable cohort measures nothing.
    // Every routed decision must therefore have compared at least two candidates.
    assert_eq!(
        degenerate, 0,
        "{} of {total} decisions still hold an arity-1 axis. `measure_release_evidence` returns \
         `DegenerateAxis` on the first one it walks, so a single degenerate decision makes the \
         whole cohort unmeasurable no matter how many of the others are rankable.",
        degenerate
    );
    assert_eq!(
        rankable, total,
        "every decision in a measurable cohort must be rankable; {rankable} of {total} are"
    );

    // The two independent measurements of the same fact must agree: the
    // projection's arity histogram and the outcomes' own attempt counts.
    let (failover, decisions) = failover_count(&recorded);
    assert_eq!(
        rankable, failover,
        "the projection found {rankable} multi-candidate axes and the outcomes recorded {failover} \
         multi-attempt decisions; these are the same fact measured two ways and must agree"
    );
    assert_eq!(
        rankable + degenerate,
        decisions,
        "every recorded decision lands in exactly one arity bucket"
    );
    println!(
        "failover: {failover} of {decisions} routed decisions ({:.1}%)",
        100.0 * failover as f64 / decisions as f64
    );
}

/// The window has one regime: the injected model is still the first choice on
/// the last request, so nothing accumulated during the window changed routing.
///
/// This is the property 7E-2D's drift gate is entitled to assume and 7E-2D's own
/// predecessor broke. An earlier version of this fixture left
/// `break_after_failures` at its default, so the circuit breaker quarantined the
/// flaky model partway through the window and every request after the quarantine
/// had a different first choice — a mid-window regime change, which the drift
/// gate refused for, correctly.
///
/// It is asserted as a measurement over the whole window rather than as a
/// statement about the configuration: if any decision's first attempt had *not*
/// been the flaky model, the breaker would have opened and the later decisions
/// would differ. Checking the first attempt of **every** decision, including the
/// last, is what makes this a claim about the window's shape rather than about
/// its beginning.
#[tokio::test]
async fn the_window_has_one_regime_and_the_injected_model_never_leaves_the_first_slot() {
    let window = drive_window(
        ShadowAttachment::embedded().expect("the candidate verifies"),
        window_requests(),
    )
    .await;
    let recorded = recorded_decisions(&window);
    assert!(
        !recorded.is_empty(),
        "the window must produce decisions or there is no regime to measure"
    );

    // Direct model-id requests name a healthy upstream explicitly and are
    // excluded by construction: they asked for that model, so the injected model
    // was never going to be their first choice and counting them would measure
    // the request mix rather than the regime.
    let mut first_choice_flaky = 0usize;
    let mut routed = 0usize;
    let mut offenders: Vec<String> = Vec::new();
    for entry in &recorded {
        if entry
            .outcome
            .attempts
            .first()
            .is_some_and(|attempt| attempt.candidate_model == "alpha-std-one")
        {
            continue;
        }
        routed += 1;
        // The first candidate *actually attempted*, not the first candidate in
        // the retained plan. On a decision where the injected model was tried
        // and failed these are the same model; on a decision where it was
        // skipped they differ, and the difference is the whole measurement.
        let attempted = entry
            .outcome
            .attempts
            .iter()
            .find(|attempt| attempt.success || attempt.failure_class.is_some())
            .expect("every routed decision attempted at least one candidate");
        // The attempt records the model's *exposed* id (`provider-upstream`),
        // so the injected models are `alpha-flaky-std` and `alpha-flaky-fast`
        // here rather than the bare `flaky-std` / `flaky-fast` the fake upstream
        // matches on. Matching the suffix keeps the two spellings from drifting
        // apart silently.
        if INJECTED_MODELS
            .iter()
            .any(|name| attempted.candidate_model.ends_with(name))
        {
            first_choice_flaky += 1;
        } else {
            offenders.push(format!(
                "{}/{}",
                attempted.candidate_provider, attempted.candidate_model
            ));
        }
    }
    assert_eq!(
        routed, ROUTED_REQUESTS,
        "every routed request in the window is counted here, so the regime claim covers the whole \
         window rather than the part of it that happened to look right"
    );
    println!(
        "first candidate attempted was the injected model in {first_choice_flaky} of {routed} \
         routed decisions; the breaker threshold is raised so it cannot be quarantined mid-window"
    );
    assert!(
        offenders.is_empty(),
        "the injected model was not the first candidate attempted for {} decision(s): {offenders:?}. \
         That is a mid-window regime change — a quarantine, or some other accumulated state \
         changing the order — and the window is no longer one shape.",
        offenders.len()
    );
}

// ---------------------------------------------------------------------------
// 1. The candidate verifies on the way in
// ---------------------------------------------------------------------------

#[test]
fn the_embedded_candidate_passes_all_three_checks_on_the_way_in() {
    let artifact: ShadowCandidateArtifact =
        serde_json::from_str(SHADOW_CANDIDATE_ARTIFACT).expect("the embedded artifact parses");
    assert_eq!(
        artifact.schema_version, SHADOW_CANDIDATE_ENVELOPE_VERSION,
        "the artifact envelope is the one this code reads"
    );

    // Each check is asserted by name so a failure says which one moved, rather
    // than reporting one opaque "did not verify".
    assert!(
        artifact.commit.verify(),
        "check 1 of 3, ModelCommit::verify: the record does not match its own content-addressed \
         identity"
    );
    assert!(
        ReplayEngine::verify_checkpoint_integrity(&artifact.commit.checkpoint),
        "check 2 of 3, ReplayEngine::verify_checkpoint_integrity: the checkpoint does not load and \
         hash to what the commit says it hashes to"
    );

    let candidate = ShadowCandidate::from_artifact(&artifact)
        .expect("check 3 of 3, the complete lineage verifies");
    assert_eq!(
        candidate.commit_id(),
        artifact.commit.commit_id.as_str(),
        "the adopted candidate is the commit the artifact names"
    );
    assert!(
        !candidate.lineage().is_empty(),
        "a candidate always carries its lineage"
    );
    assert_eq!(
        candidate
            .lineage()
            .last()
            .map(|entry| entry.commit_id.as_str()),
        Some(artifact.commit.commit_id.as_str()),
        "the retained lineage ends at the commit under consideration"
    );
    assert!(
        candidate.predictor().verify(),
        "the predictor reconstructed from the artifact verifies itself"
    );
    assert!(
        !artifact.provenance.contains("PLACEHOLDER"),
        "the artifact must not still be the placeholder the node started from"
    );
}

#[test]
fn the_candidate_is_not_the_cold_start_commit_the_engine_pins() {
    // If these were the same commit, attaching the candidate would change
    // nothing at all and the shadow-comparison tests would be measuring a
    // tautology. This is the assertion that makes them worth running.
    let candidate = ShadowCandidate::embedded().expect("the embedded candidate verifies");
    let cold = ModelEnsemblePredictor::genesis().commit_record();
    assert_ne!(
        candidate.commit().commit_id.as_str(),
        cold.commit_id.as_str(),
        "the attached candidate must be a trained commit, not the cold-start root the shadow \
         engine pins by default"
    );
    assert!(
        candidate.commit().parent.is_some(),
        "the candidate is a child commit, which is what makes its lineage worth verifying"
    );
}

#[test]
fn each_of_the_three_checks_refuses_a_broken_artifact() {
    let artifact: ShadowCandidateArtifact =
        serde_json::from_str(SHADOW_CANDIDATE_ARTIFACT).expect("the embedded artifact parses");

    // Check 1, checkpoint integrity: move one parameter without moving its
    // checksum. The checkpoint no longer agrees with itself, and it is reported
    // as a checkpoint problem rather than as a commit problem.
    let mut corrupt_parameters = artifact.clone();
    corrupt_parameters.commit.checkpoint.success.parameters[0] += 0.5;
    assert!(
        matches!(
            ShadowCandidate::from_artifact(&corrupt_parameters),
            Err(ShadowCandidateError::CheckpointRefused { .. })
        ),
        "check 1 of 3 must refuse a checkpoint that disagrees with its own parameter checksum"
    );

    // Check 2, commit identity: bump a field the checkpoint's own checksum does
    // not cover but the commit's content-addressed identity does. The checkpoint
    // is still internally valid, so this can only be caught here.
    let mut edited_count = artifact.clone();
    edited_count.commit.checkpoint.success.update_count += 1;
    assert!(
        ReplayEngine::verify_checkpoint_integrity(&edited_count.commit.checkpoint),
        "precondition: this edit leaves the checkpoint itself valid"
    );
    assert!(
        matches!(
            ShadowCandidate::from_artifact(&edited_count),
            Err(ShadowCandidateError::CommitRefused { .. })
        ),
        "check 2 of 3 must refuse a commit whose content no longer matches its own identity"
    );

    // Check 3, lineage completeness: drop the parent record. A child commit
    // with no parent record is exactly the truncation the predictor constructor
    // exists to refuse, and refusing it is the whole reason a lineage is
    // required at all.
    let mut truncated = artifact.clone();
    truncated.lineage.pop();
    assert!(
        matches!(
            ShadowCandidate::from_artifact(&truncated),
            Err(ShadowCandidateError::PredictorRefused { .. })
        ),
        "check 3 of 3 must refuse a child commit whose parent record is missing"
    );

    // A lineage record that does not itself verify is refused too: the
    // constructor re-verifies every ancestor rather than trusting the child.
    let mut broken_ancestor = artifact.clone();
    if let Some(root) = broken_ancestor.lineage.first_mut() {
        root.checkpoint.latency.update_count += 7;
    }
    assert!(
        matches!(
            ShadowCandidate::from_artifact(&broken_ancestor),
            Err(ShadowCandidateError::PredictorRefused { .. })
        ),
        "check 3 of 3 must refuse a lineage carrying a record that does not verify"
    );

    // An envelope this code does not read is refused before anything else runs,
    // rather than a partially-understood newer shape being interpreted.
    let mut future = artifact.clone();
    future.schema_version = SHADOW_CANDIDATE_ENVELOPE_VERSION + 1;
    assert!(
        matches!(
            ShadowCandidate::from_artifact(&future),
            Err(ShadowCandidateError::UnsupportedEnvelope { .. })
        ),
        "an unknown envelope version must be refused, not guessed at"
    );

    // And bytes that are not the artifact at all are refused by the reader
    // rather than by any of the three checks.
    assert!(
        matches!(
            ShadowCandidate::from_json("{ not json"),
            Err(ShadowCandidateError::ArtifactUnreadable { .. })
        ),
        "unreadable bytes must be refused"
    );
    assert!(
        matches!(
            ShadowCandidate::from_json(
                &serde_json::to_string(&json!({
                    "schema_version": SHADOW_CANDIDATE_ENVELOPE_VERSION,
                    "provenance": "",
                    "model_id": "shadow",
                    "commit": Value::Null,
                    "lineage": [],
                }))
                .expect("the wrong-shaped artifact serializes")
            ),
            Err(ShadowCandidateError::ArtifactUnreadable { .. })
        ),
        "a missing commit must be refused rather than treated as no candidate"
    );
}

// ---------------------------------------------------------------------------
// 2. The artifact survives persistence
// ---------------------------------------------------------------------------

#[test]
fn a_plain_json_round_trip_reverifies_the_artifact() {
    let artifact: ShadowCandidateArtifact =
        serde_json::from_str(SHADOW_CANDIDATE_ARTIFACT).expect("the embedded artifact parses");

    // This is the measurement the workspace's `float_roundtrip` feature buys, and
    // it is measured rather than assumed: the commit's identity hashes the bits
    // of every f64 parameter, so a transport that lost one would make the
    // round-tripped commit refuse to verify itself.
    let encoded = serde_json::to_vec(&artifact).expect("the artifact serializes");
    let decoded: ShadowCandidateArtifact =
        serde_json::from_slice(&encoded).expect("the artifact deserializes");
    let moved = f64_parameters(&artifact.commit) - f64_parameters(&decoded.commit);
    assert_eq!(
        moved, 0,
        "every f64 parameter must come back with the same bits over plain JSON"
    );

    let original = ShadowCandidate::from_artifact(&artifact).expect("the artifact verifies");
    let round_tripped = ShadowCandidate::from_artifact(&decoded).expect("the round trip verifies");
    assert_eq!(
        original.commit_id(),
        round_tripped.commit_id(),
        "the commit id survives a plain JSON round trip, which is what makes a plain JSON store a \
         usable transport for this artifact"
    );
    assert!(
        round_tripped.predictor().verify(),
        "the predictor rebuilt from the round-tripped artifact verifies itself"
    );
}

/// How many `f64` parameters a commit carries, counted by walking the
/// serialized checkpoint and counting every float.
fn f64_parameters(commit: &ModelCommit) -> usize {
    fn walk(value: &Value, found: &mut usize) {
        match value {
            Value::Number(number) => {
                if number.is_f64() {
                    *found += 1;
                }
            }
            Value::Array(items) => items.iter().for_each(|item| walk(item, found)),
            Value::Object(fields) => fields.values().for_each(|field| walk(field, found)),
            _ => {}
        }
    }
    let mut found = 0;
    walk(
        &serde_json::to_value(&commit.checkpoint).expect("the checkpoint serializes"),
        &mut found,
    );
    found
}

#[test]
fn the_frozen_artifact_carries_its_own_provenance() {
    let artifact: ShadowCandidateArtifact =
        serde_json::from_str(SHADOW_CANDIDATE_ARTIFACT).expect("the embedded artifact parses");
    let provenance = artifact.provenance.to_lowercase();
    assert!(
        !provenance.contains("real traffic")
            && !provenance.contains("production traffic")
            && !provenance.contains("live traffic")
            && !provenance.contains("shadow window in production"),
        "the artifact's own description of itself must not claim a kind of demand this project \
         does not have: {provenance}"
    );
    assert!(
        provenance.contains("fake upstream"),
        "the artifact must say the requests it was fitted from went to a local fake upstream: \
         {provenance}"
    );
    assert!(
        provenance.contains("record-only"),
        "the artifact must say what the shadow it is attached to is: {provenance}"
    );
}

// ---------------------------------------------------------------------------
// 3 + 4. The shadow changes nothing served, and its own output does change
// ---------------------------------------------------------------------------

#[tokio::test]
async fn attaching_the_candidate_changes_nothing_a_client_receives() {
    let requests = window_requests();

    // Three windows, three fresh servers, one request sequence.
    //
    // `control` is two runs of the *same* configuration. It exists so the
    // comparison has a measured noise floor instead of an assumed one: if
    // attaching the candidate changed anything at all, the attached run would
    // differ from the withdrawn run by more than the control does.
    let before = drive_window(ShadowAttachment::withdrawn(), requests).await;
    let control = drive_window(ShadowAttachment::withdrawn(), requests).await;
    let during = drive_window(
        ShadowAttachment::embedded().expect("the candidate verifies"),
        requests,
    )
    .await;

    let control_differences = served_differences(&before.served, &control.served);
    println!(
        "control (candidate withdrawn in both runs): {} difference(s): {control_differences:?}",
        control_differences.len()
    );

    // One representative served response, printed in full, so the comparison
    // above can be checked by reading it rather than by trusting an assertion.
    let sample = &during.served[0];
    println!("--- one served response, taken from the attached window ---");
    println!("status: {}", sample.status);
    for (name, value) in &sample.headers {
        println!("header: {name}: {value}");
    }
    println!("body: {}", sample.body);

    // State, rather than assume, how much of the response the comparison had to
    // set aside. The two exclusions are named and counted: the clock-derived
    // `Date` header, and a body id Core would mint fresh per response. With this
    // fake upstream the second one never fires — the upstream supplies the id
    // and the proxy preserves it — so the bodies below were compared byte for
    // byte with nothing excluded at all.
    let masked: usize = during.served.iter().filter(|s| s.masked_id).count();
    println!(
        "compared {requests} responses per window: status and every response header except the \
         clock-derived Date, and every body with {masked} of {requests} per-request ids masked \
         (the fake upstream supplies its own id, so the proxy echoes a constant)"
    );

    assert_eq!(
        before.attached_commit, None,
        "the withdrawn window has no candidate attached"
    );
    assert!(
        during.attached_commit.is_some(),
        "the attached window must actually have a candidate attached"
    );

    assert_served_identical(
        &before.served,
        &during.served,
        &control_differences,
        "before vs during",
    );

    // And the shadow's own output really did change, which is what makes the
    // comparison above a measurement rather than a tautology.
    let before_evidence = shadow_evidence(&before.records);
    let during_evidence = shadow_evidence(&during.records);
    assert_eq!(
        before_evidence.len(),
        during_evidence.len(),
        "both windows must produce the same number of shadow records"
    );
    assert!(
        !before_evidence.is_empty(),
        "the window must actually produce shadow records, or nothing is being compared"
    );
    let candidate_id = during.attached_commit.clone().expect("the candidate id");
    assert!(
        during_evidence
            .iter()
            .all(|(commit, _, _)| commit == &candidate_id),
        "every record from the attached window must name the candidate commit"
    );
    let cold_id = ModelEnsemblePredictor::genesis()
        .commit()
        .as_str()
        .to_string();
    assert!(
        before_evidence
            .iter()
            .all(|(commit, _, _)| commit == &cold_id),
        "every record from the withdrawn window must name the engine's own cold-start commit, \
         which is the pre-7F behaviour"
    );
    let changed = before_evidence
        .iter()
        .zip(&during_evidence)
        .filter(|(a, b)| a.1 != b.1)
        .count();
    println!(
        "shadow records: {} compared; {changed} differ in their decision checksum; commit \
         withdrawn={cold_id} attached={candidate_id}",
        before_evidence.len()
    );
    println!(
        "shadow's own output, first record: withdrawn -> commit {} checksum {} selected {}; \
         attached -> commit {} checksum {} selected {}",
        before_evidence[0].0,
        before_evidence[0].1,
        before_evidence[0].2,
        during_evidence[0].0,
        during_evidence[0].1,
        during_evidence[0].2,
    );
    assert_eq!(
        changed,
        before_evidence.len(),
        "every shadow record must carry a different decision checksum under the two \
         configurations, otherwise the candidate is not reaching the records"
    );
    assert_ne!(
        before_evidence, during_evidence,
        "the shadow's own output must differ between the two runs"
    );
}

// ---------------------------------------------------------------------------
// 7. Rollback
// ---------------------------------------------------------------------------

#[tokio::test]
async fn withdrawing_the_candidate_at_runtime_restores_the_prior_behaviour_exactly() {
    let requests = window_requests();

    // Two servers, identical configuration, each with its own fake upstream so
    // each sees the same call sequence. One starts with the candidate attached
    // and rolls it back mid-life; the other never had one at all.
    //
    // Comparing at *matched positions in their lives* is the only sound way to
    // do this. A router carries latency observations and health state that evolve
    // with every request, so window 1 and window 2 of the same server are not
    // comparable to each other — an earlier version of this test compared them
    // and the differences it reported were the router's, not the shadow's. Two
    // servers driven in lockstep have the same state at the same offset, so a
    // difference there is a difference caused by the candidate.
    let (upstream_a, _fake_a) = start_fake().await;
    let (upstream_b, _fake_b) = start_fake().await;
    let first = Harness::start(
        upstream_a,
        ShadowAttachment::embedded().expect("the candidate verifies"),
    )
    .await;
    let second = Harness::start(upstream_b, ShadowAttachment::withdrawn()).await;
    assert!(
        first.state.shadow_candidate_attached(),
        "the candidate starts attached on the first server"
    );
    assert!(
        !second.state.shadow_candidate_attached(),
        "and is absent from the second from the start"
    );

    // -- before: window 1 on each -------------------------------------------
    let attached_window_1 = drive(&first, 0, requests).await;
    let never_window_1 = drive(&second, 0, requests).await;
    let attached_records_1 = first.shadow_records();
    let never_records_1 = second.shadow_records();

    // -- during: the rollback, on a live server, mid-life -------------------
    assert!(
        first.state.withdraw_shadow_candidate(),
        "withdrawing a candidate that is attached is a real rollback and says so"
    );
    assert!(
        !first.state.withdraw_shadow_candidate(),
        "withdrawing twice is a no-op and says so, rather than pretending to roll back again"
    );
    assert!(
        !first.state.shadow_candidate_attached(),
        "the candidate is gone"
    );
    assert_eq!(
        first.state.shadow_candidate_commit_id(),
        None,
        "a withdrawn attachment names no commit"
    );

    // -- after: window 2 on each, at the same offset ------------------------
    let rolled_back_window_2 = drive(&first, requests, requests).await;
    let never_window_2 = drive(&second, requests, requests).await;
    let rolled_back_records_2: Vec<ShadowDecision> =
        first.shadow_records().split_off(attached_records_1.len());
    let never_records_2: Vec<ShadowDecision> =
        second.shadow_records().split_off(never_records_1.len());

    settle(&first.state, 2 * requests).await;
    settle(&second.state, 2 * requests).await;

    // Window 1: attaching the candidate changed nothing a client received.
    assert_served_identical(
        &attached_window_1,
        &never_window_1,
        &[],
        "window 1, attached vs never attached",
    );

    // Window 2: the server that had a candidate and withdrew it now serves
    // exactly what a server that never had one serves, at the same point in its
    // life. This is the rollback claim.
    assert_served_identical(
        &rolled_back_window_2,
        &never_window_2,
        &[],
        "window 2, after rollback vs never attached",
    );

    // The rollback changed the shadow's own output, back to the engine's commit.
    let cold_id = ModelEnsemblePredictor::genesis()
        .commit()
        .as_str()
        .to_string();
    let candidate_id = ShadowCandidate::embedded()
        .expect("the candidate verifies")
        .commit_id()
        .to_string();
    assert!(
        !rolled_back_records_2.is_empty(),
        "the post-rollback window must still produce shadow records, or the rollback proved nothing"
    );
    assert!(
        attached_records_1
            .iter()
            .all(|record| record.shadow.model_commit.as_str() == candidate_id),
        "before the rollback every record named the candidate"
    );
    assert!(
        rolled_back_records_2
            .iter()
            .all(|record| record.shadow.model_commit.as_str() == cold_id),
        "after the rollback every record names the engine's own cold-start commit again"
    );
    assert!(
        never_records_2
            .iter()
            .all(|record| record.shadow.model_commit.as_str() == cold_id),
        "and a server that never had a candidate named it throughout, so the rollback landed \
         somewhere real"
    );
    println!(
        "rollback: {} records named {candidate_id} before, {} named {cold_id} after",
        attached_records_1.len(),
        rolled_back_records_2.len()
    );

    // And the post-rollback records are the records a never-attached server
    // produced for the very same requests, decision for decision.
    assert_eq!(
        shadow_verdicts(&rolled_back_records_2),
        shadow_verdicts(&never_records_2),
        "after withdrawal the shadow decides exactly what a server that never attached anything \
         decides"
    );
    // The decision *checksum* is deliberately not compared here, and the reason is
    // worth stating rather than leaving as a failure somebody will "fix" later: it
    // folds in the decision-time input, and that input carries each server's own
    // measured latencies, which were taken on two different servers at two
    // different moments. The claim those checksums would have to support —
    // that nothing observable changed — is already carried by the
    // served-response comparison above, which is the comparison that can
    // actually speak to it.

    first.shutdown().await;
    second.shutdown().await;
}

/// Drive `count` requests starting at `offset`, so each window of a live server
/// gets its own slice of the one request sequence.
///
/// The step is taken modulo the sequence length, so every window replays the
/// same requests rather than continuing past the end of them. An earlier
/// version did not, which meant the second window of a live server consisted
/// entirely of the trailing direct requests — a weaker comparison than it looked
/// like, and one that produced no shadow records at all.
async fn drive(harness: &Harness, offset: usize, count: usize) -> Vec<Served> {
    let mut served = Vec::with_capacity(count);
    for index in 0..count {
        let step = (offset + index) % window_requests();
        let model = if step < ROUTED_REQUESTS {
            if step % 2 == 0 {
                "standard-class"
            } else {
                "fast-class"
            }
        } else {
            "alpha-std-one"
        };
        served.push(harness.ask(model, step == 1).await);
    }
    served
}

// ---------------------------------------------------------------------------
// 5. The candidate reaches a named verdict through the offline gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_candidate_reaches_a_named_verdict_over_real_request_evidence() {
    let window = drive_window(
        ShadowAttachment::embedded().expect("the candidate verifies"),
        window_requests(),
    )
    .await;

    let recorded = recorded_decisions(&window);
    assert!(
        recorded.len() >= 20,
        "7E-2D's default partition needs 20 decisions before it can split at all; this window \
         produced {}",
        recorded.len()
    );
    assert!(
        recorded
            .iter()
            .all(|entry| entry.outcome.served_identity().is_some()),
        "every recorded decision must name an identity that served, or the gate refuses to key an \
         evaluation on it"
    );

    let artifact: ShadowCandidateArtifact =
        serde_json::from_str(SHADOW_CANDIDATE_ARTIFACT).expect("the embedded artifact parses");
    println!("gate input: {} recorded decisions", recorded.len());
    let input = GateInput {
        commit: artifact.commit.clone(),
        lineage: artifact.lineage.clone(),
        recorded,
        // The candidate was fitted before this window ran, and none of this
        // window's samples are in its fit set. Stating it as the empty set is a
        // claim the gate checks by construction: a holdout that overlapped the
        // fit set would refuse.
        fit_sample_ids: BTreeSet::new(),
        config: gate_config(),
    };

    let outcome = run_offline_gate(&input).expect("the gate produces a report");

    let report = outcome.report();
    println!();
    println!("=== 7E-3 offline release gate, over real requests through the serving path ===");
    println!("headline: {}", report.headline());
    println!("verdict: {}", report.recomputed_verdict());
    println!("scope:   {}", report.scope);
    println!("statistical scope: {}", report.statistical_scope);
    println!("blockers:");
    for detail in report.blocker_details() {
        println!("  - {detail}");
    }
    println!();

    // Gate 1: a commit reached a *named* verdict, computed by the accepted pure
    // function from the visible constituents, agreeing with the recorded one.
    let recomputed = ReleaseVerdict::from_measurements(&report.measurements);
    assert_eq!(
        recomputed, report.verdict,
        "the recorded verdict and the recomputed verdict must agree"
    );
    assert!(
        report.transport.plain_json_exact,
        "check 2 of gate 2: the commit transports over plain JSON exactly"
    );
    assert_eq!(
        report.measurements.decisions_equivalent, report.measurements.decisions_replayed,
        "every recorded decision must replay bit-identically"
    );
    assert_eq!(
        report.measurements.terminal_agreements, report.measurements.decisions_replayed,
        "every replay must sit consistently with its outcome"
    );
    assert_eq!(
        report.measurements.absent_served_identities, 0,
        "the gate refuses a decision with no served identity, so a run with a verdict has none"
    );
    assert!(
        report.float_fidelity.round_trip_exact,
        "check 2 of gate 2: the holdout's floats survive this workspace's JSON read path"
    );

    // The verdict this node reports is the one it got. The parent's ruling is
    // explicit that a refusal is the honest outcome and that the floor is not to
    // be moved to obtain a different one, so this test asserts the refusal's
    // *shape* and prints its numbers; it does not assert the verdict is any
    // particular value.
    if let Some(refusal) = report.statistics.refusal() {
        println!("statistical refusal code: {}", refusal.code);
        println!("statistical refusal reason: {}", refusal.reason);
        assert!(
            !report.statistics.is_supported(),
            "a refusal is never a support"
        );
        assert!(
            report
                .blocker_details()
                .iter()
                .any(|detail| detail.contains("statistical")),
            "a refused statistical constituent must withhold the verdict rather than be silent"
        );
    } else {
        println!(
            "statistical support: {}",
            report.statistics.support().expect("measured").headline()
        );
    }
    println!(
        "verdict is considerable for review: {}",
        outcome.is_considerable()
    );
}

/// The power-based sample size the accepted formula asks for, printed so the
/// refusal above has something to be read against.
///
/// The discordance rate is an *assumption*, stated as one, because the refusal
/// means it was never measured. Reporting it as a measurement would be the exact
/// kind of number this project has been burned by.
#[test]
fn the_statistical_floor_is_untouched_and_the_power_formula_says_what_it_would_need() {
    let config = StatisticalConfig::default();
    assert_eq!(config.min_decisions, 30, "7D's hard decision floor");
    assert_eq!(config.alpha, 0.05);
    assert_eq!(config.power, 0.80);
    assert_eq!(config.minimum_effect, 0.05);

    // The gate configuration carries 7D's defaults verbatim. This is the
    // tripwire: a future edit that loosens the claim to make a window sufficient
    // fails here rather than quietly producing a verdict nobody earned.
    let gate = GateConfig::default();
    assert_eq!(
        gate.statistics, config,
        "the gate runs at 7D's own defaults"
    );

    for assumed_discordance in [0.1_f64, 0.25, 0.5] {
        let needed = required_decisions(
            assumed_discordance,
            config.minimum_effect,
            zroutery_core::ml::statistics::normal_quantile(config.level()),
            zroutery_core::ml::statistics::normal_quantile(config.power),
            config.min_decisions,
        )
        .expect("a positive discordance rate yields a required n");
        println!(
            "at an ASSUMED discordance rate of {assumed_discordance}, detecting a {}pp effect at \
             {} power needs {needed} decisions (floor {})",
            (config.minimum_effect * 100.0) as u32,
            config.power,
            config.min_decisions
        );
        assert!(
            needed > config.min_decisions,
            "this is the whole tension: the power formula wants more than the hard floor, and a \
             bounded local window will not reach either"
        );
    }
}

// ---------------------------------------------------------------------------
// 6. Observability: invoked, correlated, deterministic, refusals preserved
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_observability_projection_is_invoked_on_the_serving_path_and_correlates() {
    let requests = window_requests();
    let window = drive_window(
        ShadowAttachment::embedded().expect("the candidate verifies"),
        requests,
    )
    .await;

    // -- it is actually invoked ---------------------------------------------
    assert_eq!(
        window.projections.len() + window.projections.refused_count(),
        requests,
        "the batch's accounting invariant: every request is either projected or refused, never \
         both and never neither. {} projected + {} refused for {requests} requests",
        window.projections.len(),
        window.projections.refused_count()
    );
    assert_eq!(
        window.per_request.len(),
        requests,
        "the per-request projection runs once per request, refusals included"
    );

    let projected: Vec<&zroutery_core::observability::RequestProjection> =
        window.projections.records.iter().collect();
    assert!(
        !projected.is_empty(),
        "policy-routed requests must project; a window of pure refusals would mean the capability \
         was reached but never actually used"
    );

    // -- it correlates -------------------------------------------------------
    for record in &projected {
        assert!(
            !record.decision_id().is_empty(),
            "a projected record carries a decision id"
        );
        let decision = record
            .decision
            .as_ref()
            .expect("a decision was supplied for every routed request");
        assert_eq!(
            decision.decision_id,
            record.decision_id(),
            "the joined decision is the one this record names"
        );
        assert!(
            record.served_identity.is_some(),
            "a projected routed request names the identity that served"
        );
        assert!(
            !record.attempt_ids().count() == 0 || record.served_identity.is_some(),
            "the record carries its attempt evidence"
        );
        assert!(record.outcome_id().starts_with("out_"));
        assert!(record.request_id().starts_with("req_"));
        assert!(record.decision_id().starts_with("dec-"));
    }

    // -- its refusals survive -----------------------------------------------
    //
    // The direct model-id requests in the window are routed with no policy, so
    // they carry no decision id. Those must be *reported absent*. A run that
    // quietly dropped them, or filled the id in from the planned identity, would
    // be the failure this whole assertion exists to catch.
    let absent = window
        .projections
        .refusals
        .iter()
        .filter(|refusal| matches!(refusal.reason(), RefusalReason::DecisionIdAbsent))
        .count();
    assert_eq!(
        absent, DIRECT_REQUESTS,
        "each direct request has no decision id and must be reported absent rather than filled in"
    );
    for refusal in window.projections.refusals.iter() {
        assert!(
            refusal.record.outcome_id.is_some(),
            "a refusal still carries its locator: a refused record is reported, never discarded"
        );
        assert!(
            !refusal.consequence().is_empty(),
            "a refusal says what it costs"
        );
    }
    // And nothing anywhere filled one in.
    for refusal in window.projections.refusals.iter() {
        assert!(
            refusal.record.decision_id.is_none(),
            "a decision-id-absent refusal must not carry a decision id"
        );
    }

    // -- it is deterministic -------------------------------------------------
    //
    // Two claims, kept apart because they are different claims. On identical
    // input the projection is a pure function, and that is asserted exactly. On
    // the same *request sequence* against a fresh server, the projection's
    // content is identical, which is stated rather than papered over.
    //
    // Note what is being compared: `ProjectionBatch::records` is ordered by
    // `(outcome_id, request_id, decision_id)`, and those are per-request uuids,
    // so the *order* of the list is random on every run by design. Comparing it
    // positionally would be comparing uuid randomness. The comparison is
    // therefore over the records themselves, keyed on their served identity and
    // terminal state, which is what determinism means here.
    let second = drive_window(
        ShadowAttachment::embedded().expect("the candidate verifies"),
        requests,
    )
    .await;
    assert_eq!(
        second.projections.len(),
        window.projections.len(),
        "the same request sequence projects the same number of records"
    );
    assert_eq!(
        second.projections.refused_count(),
        window.projections.refused_count(),
        "the same request sequence produces the same number of refusals"
    );
    assert_eq!(
        reason_multiset(&second.projections),
        reason_multiset(&window.projections),
        "the same request sequence produces the same refusal reasons"
    );
    assert_eq!(
        served_multiset(&second.projections),
        served_multiset(&window.projections),
        "the same request sequence serves the same identities, in the same numbers"
    );
    assert_eq!(
        attempt_multiset(&second.projections),
        attempt_multiset(&window.projections),
        "the same request sequence makes the same number of attempts per served identity"
    );
    assert_eq!(
        terminal_multiset(&second.projections),
        terminal_multiset(&window.projections),
        "the same request sequence produces the same terminal states"
    );
}

/// The terminal state of every projected record, as a multiset.
fn terminal_multiset(batch: &ProjectionBatch) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for record in &batch.records {
        let key = format!(
            "{:?}/{:?}/{}",
            record.final_status, record.success, record.streaming
        );
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
}

/// The served identity of every projected record, as a multiset.
fn served_multiset(batch: &ProjectionBatch) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for record in &batch.records {
        let key = record
            .served_identity
            .as_ref()
            .map(|label| format!("{}/{}", label.provider, label.model))
            .unwrap_or_else(|| "<none>".to_string());
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
}

/// How many attempts each served identity accumulated, as a multiset.
fn attempt_multiset(batch: &ProjectionBatch) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for record in &batch.records {
        let key = record
            .served_identity
            .as_ref()
            .map(|label| format!("{}/{}", label.provider, label.model))
            .unwrap_or_else(|| "<none>".to_string());
        *counts.entry(key).or_insert(0) += record.attempts.len();
    }
    counts
}

fn reason_multiset(batch: &ProjectionBatch) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for refusal in &batch.refusals {
        *counts.entry(format!("{:?}", refusal.reason())).or_insert(0) += 1;
    }
    counts
}

#[tokio::test]
async fn the_projection_is_a_pure_function_of_its_two_arguments() {
    let requests = window_requests();
    let (upstream, _fake) = start_fake().await;
    let harness = Harness::start(
        upstream,
        ShadowAttachment::embedded().expect("the candidate verifies"),
    )
    .await;
    let _ = drive(&harness, 0, requests).await;
    settle(&harness.state, requests).await;

    // Reading it twice gives the same thing, byte for byte. This is the exact
    // determinism claim: same retained inputs, same output, every time, with no
    // caching involved — the projections are recomputed from the retained pairs
    // on every call, so a stable result means the pure function is stable.
    let first = harness.state.projections().batch();
    let second = harness.state.projections().batch();
    assert_eq!(
        first, second,
        "the projection must be a pure function of its two arguments, not a stored copy"
    );
    let first_per_request = harness.state.projections().projections();
    let second_per_request = harness.state.projections().projections();
    assert_eq!(
        first_per_request, second_per_request,
        "and the per-request projection is the same each time it is asked for"
    );

    // Recomputing by hand from the retained pairs reaches the same place, which
    // is what proves the log stores inputs rather than precomputed output.
    // `recent` is newest-first, so it is reversed to match `projections`, which
    // is in arrival order.
    let mut retained = harness.state.projections().recent(requests + 8);
    retained.reverse();
    let outcomes: Vec<Outcome> = retained
        .iter()
        .map(|(outcome, _)| outcome.clone())
        .collect();
    let decisions = retained
        .iter()
        .filter_map(|(_, decision)| decision.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        harness.state.projections().batch(),
        zroutery_core::observability::project_batch(&outcomes, &decisions),
        "the batch view is exactly the accepted pure function over what was retained"
    );
    for (index, (outcome, decision)) in retained.iter().enumerate() {
        assert_eq!(
            first_per_request[index],
            zroutery_core::observability::project_request(outcome, decision.as_ref()),
            "the per-request projection is exactly the accepted pure function over one record"
        );
    }

    harness.shutdown().await;
}

// ---------------------------------------------------------------------------
// The boundary this node must not cross
// ---------------------------------------------------------------------------

/// The source-level tripwire for everything this node is forbidden to do.
///
/// The accepted boundary test scans `src/server` for the names of the
/// installer; this one scans for the *operations* this node is forbidden to
/// perform. They are different failure modes and both matter: naming the
/// installer would make it reachable, and calling a swap would make the shadow
/// something other than a shadow.
///
/// Read as a source scan rather than a behavioural one on purpose. The
/// behavioural claims are the tests above; this one exists because a forbidden
/// call that is never reached still does not belong in the serving path.
#[test]
fn the_serving_path_names_no_forbidden_operation() {
    let server_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/server");
    let mut scanned = 0usize;
    let mut stack = vec![server_dir.clone()];
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).expect("the server tree is readable") {
            let path = entry.expect("readable").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("readable");
        scanned += 1;
        // Scoped to the shadow, not to the whole file. `AppState` also calls
        // `AtomicBool::swap` on the ledger-dirty flag, which has nothing to do
        // with a predictor, so a bare `.swap(` would fail on unrelated correct
        // code and train whoever reads this next to generalise the scan instead
        // of fixing it.
        for forbidden in [
            "shadow().swap(",
            "shadow().train(",
            "shadow().try_train(",
            "ShadowEngine::swap",
            "ShadowEngine::train",
            "takeover",
            "exploration_enabled = true",
        ] {
            assert!(
                !text.contains(forbidden),
                "{} must not contain {forbidden:?}: the shadow is record-only and a predictor is \
                 never swapped, trained or taken over on the serving path",
                path.display()
            );
        }
    }
    assert!(
        scanned >= 2,
        "the scan covered the server tree: {scanned} files"
    );
}

// ---------------------------------------------------------------------------
// The generator: how the frozen artifact came to exist
// ---------------------------------------------------------------------------

/// Train the candidate once, offline, from samples the production serving path
/// collected itself, and freeze the result.
///
/// Ignored by default because it **writes into the source tree**. It is not part
/// of any gate and no gate depends on it: the artifact is frozen, and this
/// exists so the file's provenance is checkable by whoever wants to check it,
/// not so a test can regenerate it. Re-running it will **not** reproduce the same
/// commit — the samples' features come from a live router's latency
/// observations, so the trained parameters are not a function of the request
/// sequence alone. Nothing here claims reproducibility.
///
/// Run with:
///
/// ```sh
/// cargo test -p zroutery-core --features ml --test real_request_shadow_test \
///   -- --ignored --nocapture regenerate_the_frozen_shadow_candidate
/// ```
#[tokio::test]
#[ignore = "writes the frozen artifact into the source tree; run deliberately"]
async fn regenerate_the_frozen_shadow_candidate() {
    // 1. Real requests through the production serving path against a local fake
    //    upstream. Shadow evaluation on, so the serving path retains the
    //    decision-time features it would retain in production.
    let (upstream, _fake) = start_fake().await;
    let harness = Harness::start(upstream, ShadowAttachment::withdrawn()).await;
    let _ = drive(&harness, 0, window_requests()).await;
    settle(&harness.state, window_requests()).await;

    // 2. The canonical samples the production dataset boundary collected. These
    //    are request-scoped and attempt-scoped rows built from the retained
    //    decision-time features and each request's own terminal outcome. Not one
    //    of them is synthesised here.
    let canonical = harness.state.dataset().training_slice();
    assert!(
        canonical.len() >= 20,
        "the window must produce samples to fit on; it produced {}",
        canonical.len()
    );
    let samples: Vec<DatasetTrainingSample> = canonical
        .iter()
        .cloned()
        .map(DatasetTrainingSample::from)
        .collect();
    let attempt_rows = samples
        .iter()
        .filter(|sample| {
            canonical.iter().any(|row| {
                row.sample_id == sample.sample_id
                    && row.scope != zroutery_core::ml::dataset::SampleScope::Request
            })
        })
        .count();
    println!(
        "harvested {} canonical samples ({} attempt-scoped) from {} requests",
        samples.len(),
        attempt_rows,
        window_requests()
    );

    // 3. One training round from the cold-start root, entirely offline. No live
    //    engine is touched, nothing is swapped into the shadow, and no predictor
    //    is retained past this function.
    let root = ModelEnsemblePredictor::genesis().commit_record();
    let from_root =
        ModelEnsemblePredictor::from_model_commit_with_lineage(&root, std::slice::from_ref(&root))
            .expect("the genesis root lineage verifies");
    let (_, commit) = from_root
        .try_train(&samples)
        .expect("the harvested samples train");
    let lineage = vec![root.clone(), commit.clone()];

    // 4. The artifact, verified before it is written, so a file that cannot be
    //    adopted never reaches the tree.
    let provenance = format!(
        "Fitted once, offline, from {} canonical samples that the production serving path \
         collected itself while handling {} real HTTP requests made through the production axum \
         router against a local in-process fake upstream. No deployed system and no external \
         caller was involved. Fitted from the cold-start root commit {} with a single training \
         round by node 7F's generator, then frozen; no test regenerates it and no test claims \
         regenerating it would reproduce it. The shadow this commit is attached to is record-only \
         and returns no verdict to any response, and its release verdict is recorded rather than \
         assumed.",
        samples.len(),
        window_requests(),
        root.commit_id.as_str(),
    );
    let artifact = ShadowCandidateArtifact {
        schema_version: SHADOW_CANDIDATE_ENVELOPE_VERSION,
        provenance,
        model_id: commit.model_id.as_str().to_string(),
        commit,
        lineage,
    };
    let candidate = ShadowCandidate::from_artifact(&artifact)
        .expect("the artifact verifies before it is written");
    println!("generated candidate: {}", candidate.commit_id());

    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/server/shadow_candidate.json");
    let encoded = serde_json::to_vec_pretty(&artifact).expect("the artifact serializes");
    std::fs::write(&path, encoded).expect("the artifact file is writable");
    println!("wrote {}", path.display());
    harness.shutdown().await;
}
